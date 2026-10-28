//! What the sidebar's agent card says (SIDEBAR-CARDS, spike 2026-10-28 §1.5): the badge on the
//! lane and the *news line* under the name. GPUI-free and unit tested; the panel only draws what
//! is decided here.
//!
//! The name is the first and strongest text of a row. A coding agent's row grows a second line
//! only when something is going on, by this precedence:
//!
//! 1. a request waits (the question, the permission, the plan) — `warning`;
//! 2. it failed (the error's message, or what the session ended with) — `danger`;
//! 3. the tool it runs right now — muted;
//! 4. the first line of the reply nobody has read yet (a chat) — muted;
//! 5. the agent's own name for a session the user renamed — muted.
//!
//! Plain shells never have one: a shell is one line whatever happens in it. A row with nothing
//! to say stays one line, and the first agent that no hook speaks for says once what installing
//! the hooks would add.

use hutan_protocol::messages::SessionInfo;

use super::presentation::ReasonKind;
use super::{AgentRecord, Presentation, Tone, Unread, agent_note, agent_subtitle};
use crate::workbench::ui::BadgeState;

/// Which fact a news line states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsKind {
    /// A question, a permission or a plan waits for the user.
    Request,
    /// The agent or the session failed.
    Failure,
    /// The tool the agent runs now.
    Tool,
    /// The first line of the last reply, unread.
    Reply,
    /// The agent's own title under the name the user gave the session.
    AgentTitle,
    /// What installing the agent's hooks adds (`Agent integrations…` is its link).
    HooksHint,
}

impl NewsKind {
    /// The colour role of the line: only the two that need the user are tinted.
    pub fn tone(self) -> Tone {
        match self {
            Self::Request => Tone::Warning,
            Self::Failure => Tone::Error,
            Self::Tool | Self::Reply | Self::AgentTitle | Self::HooksHint => Tone::Muted,
        }
    }
}

/// The second line of an agent card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct News {
    pub kind: NewsKind,
    /// One line of text, whitespace collapsed.
    pub text: String,
    /// The model, for the mono chip at the line's end; only where it is known.
    pub model: Option<String>,
}

/// What the line without hooks says; its link, *Agent integrations…*, opens where they are
/// installed. Short on purpose: the link shares the line.
pub const HOOKS_HINT: &str = "Hooks add model and tool";

/// The hint's tooltip: the whole sentence.
pub const HOOKS_HINT_TIP: &str =
    "Install the agent's hooks to see its model and the tool it runs on this card.";

/// The most characters of a reply the line keeps (the row cuts the rest with “…”).
const REPLY_CHARS: usize = 160;

/// Whether `info` is a coding agent: a chat, a terminal an agent reported from, or one it only
/// said it is. A plain shell is not.
pub fn is_agent(info: &SessionInfo, record: Option<&AgentRecord>) -> bool {
    info.chat.is_some() || info.terminal_agent.is_some() || info.agent.is_some() || record.is_some()
}

/// Whether the agent of a live terminal speaks through no hook: OSC 7501 or its program's name
/// is all the daemon has. A hook's report (`agent.report` from an installed integration) has
/// another source.
pub fn without_hooks(info: &SessionInfo) -> bool {
    info.chat.is_none()
        && info.state.is_live()
        && info.terminal_agent.is_some()
        && info
            .agent
            .as_ref()
            .is_none_or(|report| report.source == "osc7501")
}

/// The badge of `info`'s lane. Shape and colour both; an idle or read session has none.
///
/// Precedence: a request, a failure, work in progress, finished work nobody has seen, the end of
/// the session, nothing.
pub fn badge_state(
    info: &SessionInfo,
    presentation: &Presentation,
    unread: Option<Unread>,
) -> BadgeState {
    let live = info.state.is_live();
    if presentation.is_request() {
        BadgeState::Request
    } else if presentation.label == "failed" {
        BadgeState::Failed
    } else if matches!(presentation.label.as_str(), "working" | "starting") {
        BadgeState::Working
    } else if presentation.attention.is_some() || unread.is_some_and(Unread::is_unseen) {
        // Ready for review, a turn nobody has read, a bell, a mark: finished, unread.
        BadgeState::DoneUnread
    } else if !live {
        BadgeState::Ended
    } else {
        BadgeState::Idle
    }
}

/// The news line of an agent card, `None` for a one-line row.
pub fn news(
    info: &SessionInfo,
    presentation: &Presentation,
    record: Option<&AgentRecord>,
    unread: Option<Unread>,
    reply: Option<&str>,
) -> Option<News> {
    if !is_agent(info, record) {
        return None;
    }
    let model = record.and_then(|r| r.model.clone());
    let make = |kind, text: String| {
        let text = one_line(&text);
        (!text.is_empty()).then(|| News {
            kind,
            text,
            model: model.clone(),
        })
    };
    if presentation.is_request() {
        let asks = record
            .into_iter()
            .flat_map(|r| &r.reasons)
            .find(|reason| {
                matches!(
                    reason.kind,
                    ReasonKind::Approval
                        | ReasonKind::Question
                        | ReasonKind::Plan
                        | ReasonKind::Blocked
                )
            })
            .map(|reason| reason.text.clone());
        // A terminal says the question itself; a chat's pending object names it.
        let text = agent_note(info)
            .or(asks)
            .unwrap_or_else(|| waiting_text(presentation));
        return make(NewsKind::Request, text);
    }
    if presentation.label == "failed" {
        let text = agent_note(info)
            .or_else(|| (!presentation.detail.is_empty()).then(|| presentation.detail.clone()))
            .unwrap_or_else(|| "Failed".to_owned());
        return make(NewsKind::Failure, text);
    }
    if presentation.label == "working"
        && let Some(activity) = record.and_then(|r| r.activity.as_deref())
        && let Some(news) = make(NewsKind::Tool, activity.to_owned())
    {
        return Some(news);
    }
    if unread == Some(Unread::Turn)
        && let Some(reply) = reply
        && let Some(news) = make(NewsKind::Reply, reply.to_owned())
    {
        return Some(news);
    }
    agent_subtitle(info).and_then(|title| make(NewsKind::AgentTitle, title))
}

/// What a request says when nothing names it: the state's own word.
fn waiting_text(presentation: &Presentation) -> String {
    match presentation.label.as_str() {
        "question" => "Waiting for your answer",
        "plan" => "Plan waits for your approval",
        _ => "Waiting for approval",
    }
    .to_owned()
}

/// The first non-empty line of `text`, whitespace collapsed and cut to [`REPLY_CHARS`].
pub fn first_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let line = one_line(line);
    Some(if line.chars().count() > REPLY_CHARS {
        line.chars().take(REPLY_CHARS).collect()
    } else {
        line
    })
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::{SessionId, SessionKind, SessionState, WorkspaceId};
    use hutan_protocol::messages::{TerminalAgent, TerminalAgentSource, TerminalAgentState};

    fn session(title: &str) -> SessionInfo {
        SessionInfo {
            forked_from: None,
            extra_dirs: Vec::new(),
            containment: None,
            terminal_agent: None,
            id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
            kind: SessionKind::Terminal,
            title: title.to_owned(),
            state: SessionState::Running,
            command: vec!["sh".into()],
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
        }
    }

    fn agent(state: TerminalAgentState, message: Option<&str>) -> TerminalAgent {
        TerminalAgent {
            app: "omp".into(),
            state,
            kind: None,
            message: message.map(str::to_owned),
            since_ms: 1,
            title: None,
            source: TerminalAgentSource::Osc,
        }
    }

    fn agent_session(state: TerminalAgentState, message: Option<&str>) -> SessionInfo {
        SessionInfo {
            terminal_agent: Some(agent(state, message)),
            ..session("fix the build")
        }
    }

    fn presented(info: &SessionInfo, unread: Option<Unread>) -> Presentation {
        crate::sessions::present(info, unread, &Default::default())
    }

    #[test]
    fn a_plain_shell_is_one_line_whatever_happens_in_it() {
        let shell = session("zsh");
        for unread in [None, Some(Unread::Failed), Some(Unread::Done)] {
            let presentation = presented(&shell, unread);
            assert_eq!(news(&shell, &presentation, None, unread, Some("hi")), None);
        }
    }

    #[test]
    fn an_idle_read_agent_is_one_line() {
        let info = agent_session(TerminalAgentState::Idle, None);
        let presentation = presented(&info, None);
        assert_eq!(news(&info, &presentation, None, None, None), None);
        assert_eq!(
            badge_state(&info, &presentation, None),
            BadgeState::Idle,
            "an idle agent wears no badge"
        );
    }

    #[test]
    fn a_question_is_the_news_in_the_warning_tone_with_the_badge_of_a_request() {
        let info = agent_session(TerminalAgentState::Blocked, Some("Overwrite the lockfile?"));
        let presentation = presented(&info, None);
        let line = news(&info, &presentation, None, None, None).expect("news");
        assert_eq!(line.kind, NewsKind::Request);
        assert_eq!(line.text, "Overwrite the lockfile?");
        assert_eq!(line.kind.tone(), Tone::Warning);
        assert_eq!(badge_state(&info, &presentation, None), BadgeState::Request);
    }

    #[test]
    fn a_failure_says_its_message_in_the_danger_tone() {
        let info = agent_session(TerminalAgentState::Error, Some("  rate\nlimited  "));
        let presentation = presented(&info, Some(Unread::Failed));
        let line = news(&info, &presentation, None, Some(Unread::Failed), None).expect("news");
        assert_eq!(line.kind, NewsKind::Failure);
        assert_eq!(line.text, "rate limited", "one line, whitespace collapsed");
        assert_eq!(line.kind.tone(), Tone::Error);
        assert_eq!(
            badge_state(&info, &presentation, Some(Unread::Failed)),
            BadgeState::Failed
        );
    }

    #[test]
    fn the_tool_running_now_is_news_only_while_it_works() {
        let info = agent_session(TerminalAgentState::Working, None);
        let presentation = presented(&info, None);
        let record = record_with(|r| r.activity = Some("Bash: cargo test".into()));
        let line = news(&info, &presentation, Some(&record), None, None).expect("news");
        assert_eq!(
            (line.kind, line.text.as_str()),
            (NewsKind::Tool, "Bash: cargo test")
        );
        assert_eq!(badge_state(&info, &presentation, None), BadgeState::Working);
        // Working without a tool to name: nothing to add.
        assert_eq!(
            news(&info, &presentation, Some(&record_with(|_| {})), None, None),
            None
        );
    }

    #[test]
    fn an_unread_reply_is_news_until_it_is_read_and_the_badge_is_done_unread() {
        let mut chat = agent_session(TerminalAgentState::Idle, None);
        chat.terminal_agent = None;
        chat.chat = Some(tests_support::chat());
        chat.state = SessionState::Idle; // between turns
        let presentation = presented(&chat, Some(Unread::Turn));
        let line = news(
            &chat,
            &presentation,
            None,
            Some(Unread::Turn),
            Some("Fixed the race in the queue."),
        )
        .expect("news");
        assert_eq!(line.kind, NewsKind::Reply);
        assert_eq!(line.kind.tone(), Tone::Muted);
        assert_eq!(
            badge_state(&chat, &presentation, Some(Unread::Turn)),
            BadgeState::DoneUnread
        );
        // Read: one line again, and no badge.
        let read = presented(&chat, None);
        assert_eq!(news(&chat, &read, None, None, Some("Fixed it.")), None);
        assert_eq!(badge_state(&chat, &read, None), BadgeState::Idle);
    }

    #[test]
    fn a_request_beats_a_failure_and_the_tool_and_the_reply() {
        let info = agent_session(TerminalAgentState::Blocked, Some("Run rm -rf?"));
        let presentation = presented(&info, Some(Unread::Turn));
        let record = record_with(|r| r.activity = Some("Bash".into()));
        let line = news(
            &info,
            &presentation,
            Some(&record),
            Some(Unread::Turn),
            Some("done"),
        )
        .expect("news");
        assert_eq!(line.kind, NewsKind::Request);
    }

    #[test]
    fn the_model_rides_on_the_line_and_never_makes_one() {
        let info = agent_session(TerminalAgentState::Idle, None);
        let presentation = presented(&info, None);
        let record = record_with(|r| r.model = Some("Sonnet 5.5".into()));
        assert_eq!(news(&info, &presentation, Some(&record), None, None), None);
        let blocked = agent_session(TerminalAgentState::Blocked, Some("Go on?"));
        let presentation = presented(&blocked, None);
        let line = news(&blocked, &presentation, Some(&record), None, None).expect("news");
        assert_eq!(line.model.as_deref(), Some("Sonnet 5.5"));
    }

    #[test]
    fn the_agents_own_title_shows_under_a_name_the_user_gave() {
        let mut info = agent_session(TerminalAgentState::Idle, None);
        info.title = "My name".into();
        info.terminal_agent.as_mut().expect("agent").title = Some("Fix the build".into());
        let presentation = presented(&info, None);
        let line = news(&info, &presentation, None, None, None).expect("news");
        assert_eq!(
            (line.kind, line.text.as_str()),
            (NewsKind::AgentTitle, "Fix the build")
        );
    }

    #[test]
    fn hooks_speak_for_an_agent_only_through_a_report_of_their_own() {
        let osc = agent_session(TerminalAgentState::Idle, None);
        assert!(without_hooks(&osc));
        let mut hooked = osc.clone();
        hooked.agent = Some(tests_support::report("claude-hooks"));
        assert!(!without_hooks(&hooked));
        let mut folded = osc.clone();
        folded.agent = Some(tests_support::report("osc7501"));
        assert!(
            without_hooks(&folded),
            "an OSC report folded into the tree is no hook"
        );
        assert!(
            !without_hooks(&session("zsh")),
            "a shell has no agent to hook"
        );
    }

    #[test]
    fn a_reply_keeps_its_first_line_cut_to_length() {
        assert_eq!(
            first_line("\n\n  Hello   world \nsecond"),
            Some("Hello world".into())
        );
        assert_eq!(first_line("   \n"), None);
        let long = "x".repeat(400);
        assert_eq!(
            first_line(&long).map(|l| l.chars().count()),
            Some(REPLY_CHARS)
        );
    }

    fn record_with(edit: impl FnOnce(&mut AgentRecord)) -> AgentRecord {
        let mut record = tests_support::record();
        edit(&mut record);
        record
    }

    pub(super) mod tests_support {
        use super::*;
        use hutan_protocol::chat::ChatInfo;
        use hutan_protocol::messages::{AgentReport, AgentState};

        pub fn chat() -> ChatInfo {
            serde_json::from_value(serde_json::json!({
                "agent": "omp",
                "model": "anthropic/claude-haiku-5-5",
                "approval_policy": "on-request",
                "sandbox": "workspace-write",
            }))
            .expect("chat info")
        }

        pub fn report(source: &str) -> AgentReport {
            AgentReport {
                source: source.to_owned(),
                agent: "omp".into(),
                state: AgentState::Idle,
                message: None,
                updated_at_ms: 1,
                blocked_sub: None,
                agent_session_id: None,
            }
        }

        pub fn record() -> AgentRecord {
            AgentRecord {
                session: SessionId::new(),
                title: "t".into(),
                role: crate::sessions::Role::Agent,
                state: crate::sessions::State::Working,
                group: crate::sessions::Group::Working,
                reasons: Vec::new(),
                requests: Vec::new(),
                submitting: 0,
                questions: Vec::new(),
                agent: "omp".into(),
                harness: "OMP".into(),
                model: None,
                model_id: None,
                workspace: WorkspaceId::new(),
                container: None,
                project: "p".into(),
                container_name: None,
                checkout: None,
                folder: None,
                activity: None,
                detail: String::new(),
                updated_at_ms: None,
                reported_at_ms: None,
                started_at_ms: None,
                ended_at_ms: None,
                usage: None,
                transcript: None,
                provenance: Vec::new(),
                children: Vec::new(),
                subagents_live: 0,
                subagents_finished: 0,
                subagents_blocked: 0,
                live: true,
                managed: false,
                turn_active: false,
                changes_to_review: false,
                report_source: None,
            }
        }
    }
}
