//! Fork a chat from a turn as the desktop says it (CHAT-FORK-B; protocol 0.56 `chat.fork`,
//! 0.58 `chat.fork.worktree`, ADR 0023; `docs/agents.md` § Fork): which cuts a harness can make
//! (never substituted: a cut it cannot make is offered as a *choice*), the words for each, the
//! reading of a refusal, the sidebar's lineage (a fork's row under its parent), and the command
//! a terminal agent forks itself with (`omp --fork`, `claude --resume --fork-session`,
//! `codex fork`).
//!
//! Pure data and text: the daemon decides and does; nothing here talks to it.

use std::collections::{HashMap, HashSet};

use hutan_client::ClientError;
use hutan_core::{ForkMode, ForkSupport, SessionId};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::SessionInfo;

/// What the popover offers for the cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// The harness cuts at the turn: nothing to choose.
    Exact,
    /// The harness cannot cut here: the user chooses one of these (in this order); nothing is
    /// done until they do.
    Choose(Vec<ForkMode>),
    /// The chat cannot be forked at all.
    Never,
}

/// The cuts to offer for a chat whose harness `support`s forks, for the turn that is (or is not)
/// the chat's newest. `None`: the agent has not said yet (`ChatInfo.fork` is absent until it
/// started once in this daemon run): ask for the exact cut and let the daemon's answer say.
pub fn offer(support: Option<ForkSupport>, at_head: bool) -> Offer {
    let Some(support) = support else {
        return Offer::Exact;
    };
    if support.allows(ForkMode::Exact, at_head) {
        return Offer::Exact;
    }
    let choices: Vec<ForkMode> = [ForkMode::Head, ForkMode::Summary]
        .into_iter()
        .filter(|mode| support.allows(*mode, at_head))
        .collect();
    if choices.is_empty() {
        Offer::Never
    } else {
        Offer::Choose(choices)
    }
}

/// `turns 1–4`, or `the turns so far` while the turn's place is not known (the transcript is
/// not loaded from its start).
pub fn turns_text(ordinal: Option<u32>) -> String {
    match ordinal {
        Some(1) => "turn 1".to_owned(),
        Some(n) => format!("turns 1–{n}"),
        None => "the turns so far".to_owned(),
    }
}

/// The title and the sentence of a cut: what the new chat will know.
pub fn cut_words(mode: ForkMode, ordinal: Option<u32>) -> (String, String) {
    let upto = turns_text(ordinal);
    match mode {
        ForkMode::Exact => (
            format!("Exact: {upto}"),
            "The conversation as it was after that turn, cut by the agent itself.".to_owned(),
        ),
        ForkMode::Head => (
            "Fork at the latest turn".to_owned(),
            "The new chat keeps the whole conversation as it is now, not only up to the turn you \
             picked."
                .to_owned(),
        ),
        ForkMode::Summary => (
            format!("Fork with a summary of {upto}"),
            "The agent starts a new conversation and is given a digest of those turns, written \
             by Hutan, not by a model. The history stays visible in the new chat."
                .to_owned(),
        ),
    }
}

/// What the journal's footer of the popover says about an exact fork, in a line.
pub fn exact_line(agent: &str, ordinal: Option<u32>) -> String {
    let upto = turns_text(ordinal);
    format!(
        "Exact fork · {} keeps {upto}; the original is not changed",
        agent_label(agent)
    )
}

fn agent_label(agent: &str) -> &str {
    match agent {
        "codex" => "Codex",
        "omp" => "OMP",
        "claude" => "Claude Code",
        "direct" => "this chat",
        other => other,
    }
}

/// A refusal of `chat.fork` the user can answer by choosing another cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The daemon's sentence.
    pub message: String,
    /// The cuts that would work, in the daemon's order (`data.alternatives`).
    pub alternatives: Vec<ForkMode>,
    /// What the harness can do (`data.support`).
    pub support: Option<ForkSupport>,
}

/// Why a fork did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkFailure {
    /// What to say, in the daemon's own words (or the transport's).
    pub message: String,
    /// `unsupported` with the cuts that would work: the popover turns into a choice.
    pub refused: Option<Refused>,
    /// `conflict`: the branch exists.
    pub branch_taken: bool,
}

impl ForkFailure {
    pub fn of(error: &ClientError) -> Self {
        let ClientError::Remote(body) = error else {
            return Self {
                message: error.to_string(),
                refused: None,
                branch_taken: false,
            };
        };
        let refused = (body.code == ErrorCode::Unsupported)
            .then(|| {
                let data = body.data.as_ref()?;
                let alternatives = data
                    .get("alternatives")?
                    .as_array()?
                    .iter()
                    .filter_map(|mode| match mode.as_str()? {
                        "head" => Some(ForkMode::Head),
                        "summary" => Some(ForkMode::Summary),
                        "exact" => Some(ForkMode::Exact),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let support = match data.get("support").and_then(|s| s.as_str()) {
                    Some("exact") => Some(ForkSupport::Exact),
                    Some("head") => Some(ForkSupport::Head),
                    Some("summary") => Some(ForkSupport::Summary),
                    Some("none") => Some(ForkSupport::None),
                    _ => None,
                };
                (!alternatives.is_empty()).then(|| Refused {
                    message: body.message.clone(),
                    alternatives,
                    support,
                })
            })
            .flatten();
        Self {
            message: body.message.clone(),
            refused,
            branch_taken: body.code == ErrorCode::Conflict,
        }
    }
}

// ---- lineage -----------------------------------------------------------------------------------

/// `sessions` in order with every fork placed right under its parent (and its own forks under
/// it), each with how many levels it hangs below the first row. A fork whose parent is not in
/// the list (another checkout, another group, deleted) keeps its own place: the chat header's
/// chip is what links it to the parent then.
pub fn nest<'a>(sessions: &[&'a SessionInfo]) -> Vec<(&'a SessionInfo, usize)> {
    let present: HashSet<SessionId> = sessions.iter().map(|s| s.id).collect();
    let mut children: HashMap<SessionId, Vec<&'a SessionInfo>> = HashMap::new();
    let mut roots: Vec<&'a SessionInfo> = Vec::new();
    for session in sessions {
        match session
            .forked_from
            .as_ref()
            .map(|origin| origin.session_id)
            .filter(|parent| *parent != session.id && present.contains(parent))
        {
            Some(parent) => children.entry(parent).or_default().push(session),
            None => roots.push(session),
        }
    }
    let mut out = Vec::with_capacity(sessions.len());
    let mut seen = HashSet::new();
    fn place<'a>(
        session: &'a SessionInfo,
        depth: usize,
        children: &HashMap<SessionId, Vec<&'a SessionInfo>>,
        seen: &mut HashSet<SessionId>,
        out: &mut Vec<(&'a SessionInfo, usize)>,
    ) {
        if !seen.insert(session.id) {
            return;
        }
        out.push((session, depth));
        for child in children.get(&session.id).into_iter().flatten() {
            place(child, depth + 1, children, seen, out);
        }
    }
    for root in roots {
        place(root, 0, &children, &mut seen, &mut out);
    }
    // A cycle of forks (impossible from the daemon) would otherwise drop its rows.
    for session in sessions {
        place(session, 0, &children, &mut seen, &mut out);
    }
    out
}

/// The text of the header chip: `Forked from “Title” · turn 4`.
pub fn chip_text(parent: Option<&str>, ordinal: u32) -> String {
    let parent = match parent {
        Some(title) => format!("“{title}”"),
        None => "a chat that is gone".to_owned(),
    };
    if ordinal == 0 {
        format!("Forked from {parent}")
    } else {
        format!("Forked from {parent} · turn {ordinal}")
    }
}

// ---- names ------------------------------------------------------------------------------------

/// What the daemon appends to the parent's title for a fork nobody named.
const DEFAULT_SUFFIX: &str = " (fork)";

/// The longest a worktree branch is written in a fork's mark.
const BRANCH_MARK: usize = 12;

/// A session's name as the window draws it: the `head` is what gives way when there is no room
/// (cut with "…"), the `tail` never does. Only a fork nobody renamed has a tail: its mark says
/// where it parted from the parent (`⑂ 2`: after the parent's turn 2; `⑂ reply-…dy-t1`: in
/// the worktree of that branch), so the parent, a fork of it and a fork into a worktree never
/// read the same in a row or a tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleParts {
    pub head: String,
    pub tail: Option<String>,
}

impl TitleParts {
    /// The name on one line, as a screen reader or a tooltip says it.
    pub fn text(&self) -> String {
        match &self.tail {
            Some(tail) => format!("{} {tail}", self.head),
            None => self.head.clone(),
        }
    }
}

/// `text` cut to `max` characters by dropping its middle (`…`).
fn middle(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max || max < 3 {
        return text.to_owned();
    }
    let keep = max - 1;
    let (front, back) = (keep.div_ceil(2), keep / 2);
    let start: String = text.chars().take(front).collect();
    let end: String = text.chars().skip(count - back).collect();
    format!("{start}…{end}")
}

/// The name of `info`; `find` looks up a session by id (the parent of a fork).
pub fn title_parts<'a>(
    info: &'a SessionInfo,
    find: &dyn Fn(SessionId) -> Option<&'a SessionInfo>,
) -> TitleParts {
    parts_at(info, find, 0)
}

/// Whether the session's name is the one its coding agent gave it (OMP names its sessions from
/// the task, often behind an emoji): the user's own name is theirs, whatever it begins with.
fn named_by_agent(info: &SessionInfo) -> bool {
    info.terminal_agent
        .as_ref()
        .and_then(|agent| agent.title.as_deref())
        .is_some_and(|title| title == info.title)
}

fn parts_at<'a>(
    info: &'a SessionInfo,
    find: &dyn Fn(SessionId) -> Option<&'a SessionInfo>,
    depth: usize,
) -> TitleParts {
    let plain = || TitleParts {
        head: if named_by_agent(info) {
            super::card::strip_pictographs(&info.title).to_owned()
        } else {
            info.title.clone()
        },
        tail: None,
    };
    let Some(origin) = info.forked_from.as_ref() else {
        return plain();
    };
    // A name the user gave is theirs.
    let Some(mut stem) = info.title.strip_suffix(DEFAULT_SUFFIX) else {
        return plain();
    };
    let parent = find(origin.session_id).filter(|parent| parent.id != info.id);
    let own_worktree = info
        .placement
        .as_ref()
        .filter(|placement| placement.kind == hutan_protocol::worktree::PlacementKind::Worktree)
        .filter(|placement| {
            parent
                .and_then(|parent| parent.placement.as_ref())
                .is_none_or(|theirs| theirs.root != placement.root)
        })
        .and_then(|placement| placement.branch.as_deref());
    let mark = match (own_worktree, origin.ordinal) {
        (Some(branch), _) => format!(
            "⑂ {}",
            middle(branch.strip_prefix("fork/").unwrap_or(branch), BRANCH_MARK)
        ),
        (None, 0) => "⑂".to_owned(),
        (None, turn) => format!("⑂ {turn}"),
    };
    // A fork of a fork carries its parent's mark in front of its own.
    if let Some(parent) = parent
        && depth < 8
        && parent.forked_from.is_some()
        && parent.title.ends_with(DEFAULT_SUFFIX)
    {
        let above = parts_at(parent, find, depth + 1);
        return TitleParts {
            tail: Some(match above.tail {
                Some(above) => format!("{above} {mark}"),
                None => mark,
            }),
            head: above.head,
        };
    }
    while let Some(rest) = stem.strip_suffix(DEFAULT_SUFFIX) {
        stem = rest;
    }
    TitleParts {
        head: stem.to_owned(),
        tail: Some(mark),
    }
}

// ---- terminal agents ---------------------------------------------------------------------------

/// What *Fork session* does for a terminal session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalFork {
    /// No coding agent is known to run here: no entry.
    Nothing,
    /// An agent runs but has not told its session id (`hutan agent report --agent-session-id`):
    /// the entry is greyed and says so.
    NoSessionId { app: String },
    /// The agent's own command that forks its conversation, run in a new terminal.
    Ready { app: String, argv: Vec<String> },
}

/// The command a harness forks its own session with, at its head (spike §2.2, TERM-FORK);
/// `None` for a program that has no fork command (or is not one).
pub fn terminal_fork_argv(app: &str, session_id: &str) -> Option<Vec<String>> {
    let id = session_id.to_owned();
    match app {
        "omp" => Some(vec!["omp".to_owned(), "--fork".to_owned(), id]),
        "claude" => Some(vec![
            "claude".to_owned(),
            "--resume".to_owned(),
            id,
            "--fork-session".to_owned(),
        ]),
        "codex" => Some(vec!["codex".to_owned(), "fork".to_owned(), id]),
        _ => None,
    }
}

/// What *Fork session* means for `session` (a terminal): the harness that runs in it, from the
/// agent's own report (`hutan agent report`) or else the daemon's recognition of the program,
/// and the session id the agent reported.
pub fn terminal_fork(session: &SessionInfo) -> TerminalFork {
    if session.chat.is_some() {
        return TerminalFork::Nothing;
    }
    let app = session
        .agent
        .as_ref()
        .map(|report| report.agent.as_str())
        .or_else(|| {
            session
                .terminal_agent
                .as_ref()
                .map(|agent| agent.app.as_str())
        });
    let Some(app) = app else {
        return TerminalFork::Nothing;
    };
    // The hooks name their agent as they like (`Claude Code`): the harness is what they mean.
    let app = app.to_ascii_lowercase();
    let Some(app) = ["omp", "claude", "codex"]
        .into_iter()
        .find(|harness| app.starts_with(harness))
        .map(str::to_owned)
    else {
        return TerminalFork::Nothing;
    };
    let id = session
        .agent
        .as_ref()
        .and_then(|report| report.agent_session_id.as_deref())
        .map(str::trim)
        .filter(|id| !id.is_empty());
    match id.and_then(|id| terminal_fork_argv(&app, id)) {
        Some(argv) => TerminalFork::Ready { app, argv },
        None => TerminalFork::NoSessionId { app },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::{ForkOrigin, SessionKind, SessionState, TurnId, WorkspaceId};

    fn session(title: &str, parent: Option<SessionId>) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
            kind: SessionKind::Harness,
            title: title.to_owned(),
            state: SessionState::Idle,
            command: Vec::new(),
            cwd: None,
            terminal_id: None,
            created_at_ms: 0,
            agent: None,
            restored: false,
            chat: None,
            placement: None,
            folder: None,
            recovery: None,
            muted: false,
            containment: None,
            terminal_agent: None,
            forked_from: parent.map(|session_id| ForkOrigin {
                session_id,
                turn_id: TurnId::new(),
                ordinal: 2,
                mode: ForkMode::Exact,
                at_ms: 0,
            }),
            extra_dirs: Vec::new(),
        }
    }

    #[test]
    fn what_to_offer_follows_what_the_harness_can_do() {
        use ForkSupport::{Exact, Head, None as Never, Summary};
        assert_eq!(offer(Option::None, false), Offer::Exact);
        assert_eq!(offer(Some(Exact), false), Offer::Exact);
        assert_eq!(offer(Some(Head), true), Offer::Exact);
        assert_eq!(
            offer(Some(Head), false),
            Offer::Choose(vec![ForkMode::Head, ForkMode::Summary])
        );
        assert_eq!(
            offer(Some(Summary), true),
            Offer::Choose(vec![ForkMode::Summary])
        );
        assert_eq!(offer(Some(Never), true), Offer::Never);
    }

    #[test]
    fn the_words_name_the_turns() {
        assert_eq!(turns_text(Some(1)), "turn 1");
        assert_eq!(turns_text(Some(4)), "turns 1–4");
        assert_eq!(turns_text(None), "the turns so far");
        assert_eq!(
            cut_words(ForkMode::Summary, Some(4)).0,
            "Fork with a summary of turns 1–4"
        );
        assert_eq!(
            cut_words(ForkMode::Head, Some(4)).0,
            "Fork at the latest turn"
        );
        assert_eq!(chip_text(Some("Auth"), 4), "Forked from “Auth” · turn 4");
        assert_eq!(chip_text(None, 0), "Forked from a chat that is gone");
    }

    #[test]
    fn a_refusal_names_the_cuts_that_would_work() {
        let mut body = hutan_protocol::ErrorBody::new(ErrorCode::Unsupported, "only at the end");
        body.data =
            Some(serde_json::json!({"support": "head", "alternatives": ["head", "summary"]}));
        let failure = ForkFailure::of(&ClientError::Remote(body));
        let refused = failure.refused.expect("a choice");
        assert_eq!(refused.alternatives, [ForkMode::Head, ForkMode::Summary]);
        assert_eq!(refused.support, Some(ForkSupport::Head));
        let plain = ForkFailure::of(&ClientError::Remote(hutan_protocol::ErrorBody::new(
            ErrorCode::Conflict,
            "branch exists",
        )));
        assert!(plain.refused.is_none() && plain.branch_taken);
    }

    #[test]
    fn a_fork_hangs_under_its_parent_and_the_rest_keeps_its_order() {
        let a = session("a", None);
        let b = session("b", None);
        let a_fork = session("a fork", Some(a.id));
        let a_fork_fork = session("a fork fork", Some(a_fork.id));
        let orphan = session("orphan", Some(SessionId::new()));
        let list = [&a, &b, &a_fork_fork, &orphan, &a_fork];
        let nested: Vec<(&str, usize)> = nest(&list)
            .into_iter()
            .map(|(s, depth)| (s.title.as_str(), depth))
            .collect();
        assert_eq!(
            nested,
            [
                ("a", 0),
                ("a fork", 1),
                ("a fork fork", 2),
                ("b", 0),
                ("orphan", 0),
            ]
        );
    }

    #[test]
    fn a_terminal_forks_through_its_own_cli_only_with_its_session_id() {
        assert_eq!(
            terminal_fork_argv("omp", "abc"),
            Some(vec!["omp".into(), "--fork".into(), "abc".into()])
        );
        assert_eq!(
            terminal_fork_argv("claude", "abc"),
            Some(vec![
                "claude".into(),
                "--resume".into(),
                "abc".into(),
                "--fork-session".into()
            ])
        );
        assert_eq!(
            terminal_fork_argv("codex", "abc"),
            Some(vec!["codex".into(), "fork".into(), "abc".into()])
        );
        assert_eq!(terminal_fork_argv("zsh", "abc"), None);
        assert_eq!(
            terminal_fork(&session("shell", None)),
            TerminalFork::Nothing
        );
    }

    fn worktree(info: &mut SessionInfo, root: &str, branch: &str) {
        info.placement = Some(hutan_protocol::worktree::Placement {
            kind: hutan_protocol::worktree::PlacementKind::Worktree,
            root: root.into(),
            branch: Some(branch.to_owned()),
            shared_with: Vec::new(),
        });
    }

    #[test]
    fn a_default_named_fork_keeps_its_parents_name_and_wears_the_turn_it_parted_at() {
        let parent = session("Reply with the single word ready", None);
        let fork = session("Reply with the single word ready (fork)", Some(parent.id));
        let all = [parent.clone(), fork.clone()];
        let find = |id: SessionId| all.iter().find(|s| s.id == id);
        let parts = title_parts(&fork, &find);
        assert_eq!(parts.head, "Reply with the single word ready");
        assert_eq!(parts.tail.as_deref(), Some("⑂ 2"));
        assert_eq!(parts.text(), "Reply with the single word ready ⑂ 2");
        // The parent has no mark: they never read the same.
        assert_eq!(title_parts(&parent, &find).tail, None);
    }

    #[test]
    fn a_fork_into_a_worktree_wears_the_branch_and_a_fork_of_a_fork_both_marks() {
        let parent = session("Plan", None);
        let mut wt = session("Plan (fork)", Some(parent.id));
        worktree(
            &mut wt,
            "/repo-wt/fork",
            "fork/reply-with-the-single-word-ready-t1",
        );
        let mut deep = session("Plan (fork) (fork)", Some(wt.id));
        deep.forked_from.as_mut().unwrap().ordinal = 3;
        worktree(&mut deep, "/repo-wt/fork", "fork/other");
        let all = [parent.clone(), wt.clone(), deep.clone()];
        let find = |id: SessionId| all.iter().find(|s| s.id == id);
        let tail = title_parts(&wt, &find).tail.unwrap();
        assert!(
            tail.starts_with("⑂ reply-") && tail.ends_with("dy-t1"),
            "{tail}"
        );
        assert!(tail.chars().count() <= 2 + BRANCH_MARK);
        // The fork of the fork stays in the worktree of its parent: a turn, after the parent's mark.
        let nested = title_parts(&deep, &find);
        assert_eq!(nested.head, "Plan");
        assert_eq!(nested.tail, Some(format!("{tail} ⑂ 3")));
    }

    #[test]
    fn a_name_the_user_gave_and_a_session_that_is_no_fork_are_left_alone() {
        let parent = session("Plan", None);
        let renamed = session("Plan B", Some(parent.id));
        let all = [parent.clone(), renamed.clone()];
        let find = |id: SessionId| all.iter().find(|s| s.id == id);
        assert_eq!(title_parts(&renamed, &find).text(), "Plan B");
        let lookalike = session("Notes (fork)", None);
        assert_eq!(title_parts(&lookalike, &find).text(), "Notes (fork)");
        // A parent that is gone: the name without the marker of the daemon, still marked.
        let orphan = session("Gone (fork)", Some(SessionId::new()));
        assert_eq!(title_parts(&orphan, &|_| None).text(), "Gone ⑂ 2");
    }

    #[test]
    fn the_middle_of_a_long_branch_goes() {
        assert_eq!(middle("short", 16), "short");
        assert_eq!(middle("abcdefghijklmnopqrstuvwxyz", 9), "abcd…wxyz");
    }

    fn agent_titled(own: &str, agent: Option<&str>) -> SessionInfo {
        let mut info = session(own, None);
        info.forked_from = None;
        info.terminal_agent = agent.map(|title| hutan_protocol::messages::TerminalAgent {
            app: "omp".into(),
            state: hutan_protocol::messages::TerminalAgentState::Idle,
            kind: None,
            message: None,
            since_ms: 1,
            title: Some(title.to_owned()),
            source: hutan_protocol::messages::TerminalAgentSource::Osc,
        });
        info
    }

    #[test]
    fn an_agents_emoji_prefix_goes_from_its_title_and_the_users_own_name_is_theirs() {
        use super::super::card::strip_pictographs;
        assert_eq!(
            strip_pictographs("➖ SUB: Add sub function"),
            "SUB: Add sub function"
        );
        assert_eq!(strip_pictographs("👷 WORKER: worker"), "WORKER: worker");
        assert_eq!(strip_pictographs("⚡️ 🚀 Ship it"), "Ship it");
        assert_eq!(strip_pictographs("Fix the build"), "Fix the build");
        assert_eq!(strip_pictographs("1️⃣ first"), "1️⃣ first");
        // Nothing but a pictograph: it stays rather than leaving an empty name.
        assert_eq!(strip_pictographs("🚀"), "🚀");

        // The agent's own name loses its prefix; a name the user gave keeps theirs.
        let agents = agent_titled("➖ SUB: Add sub", Some("➖ SUB: Add sub"));
        assert_eq!(title_parts(&agents, &|_| None).text(), "SUB: Add sub");
        let users = agent_titled("🚀 My launch", Some("➖ SUB: Add sub"));
        assert_eq!(title_parts(&users, &|_| None).text(), "🚀 My launch");
        let shell = agent_titled("🚀 My launch", None);
        assert_eq!(title_parts(&shell, &|_| None).text(), "🚀 My launch");
    }
}
