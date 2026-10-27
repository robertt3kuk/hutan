//! The placement badge of a session (protocol 0.14, `docs/design.md` § session row): `main` for
//! the repository's main checkout, `wt <branch>` for a linked worktree, tinted as a warning
//! while another live agent works in the same checkout.

use hutan_protocol::messages::{AgentState, SessionInfo};
use hutan_protocol::worktree::PlacementKind;

/// What a session's row and tab say about where it works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementBadge {
    /// `main`, `wt <branch>`, or `wt (detached)`; while another live agent works in the same
    /// checkout the words say that instead: `shared checkout` (`wt <branch> · shared`).
    pub text: String,
    /// Another live agent works in the same checkout: the badge is drawn as a warning.
    pub shared: bool,
    /// The checkout's folder, its branch, and the warning, one per line.
    pub tooltip: String,
}

/// The badge of `session`; `None` outside a git checkout.
pub fn placement_badge(session: &SessionInfo) -> Option<PlacementBadge> {
    let placement = session.placement.as_ref()?;
    let text = match (placement.kind, placement.branch.as_deref()) {
        (PlacementKind::Main, _) => "main".to_owned(),
        (PlacementKind::Worktree, Some(branch)) => format!("wt {branch}"),
        (PlacementKind::Worktree, None) => "wt (detached)".to_owned(),
    };
    let mut lines = vec![
        match placement.kind {
            PlacementKind::Main => "Main checkout".to_owned(),
            PlacementKind::Worktree => "Worktree".to_owned(),
        },
        placement.root.display().to_string(),
    ];
    if let Some(branch) = &placement.branch {
        lines.push(format!("Branch {branch}"));
    }
    let others = placement.shared_with.len();
    if others > 0 {
        lines.push(match others {
            1 => "⚠ Another live agent works in this checkout: your edits and its can overwrite each other".to_owned(),
            n => format!(
                "⚠ {n} other live agents work in this checkout: your edits and theirs can overwrite each other"
            ),
        });
    }
    let text = match (others > 0, placement.kind) {
        (false, _) => text,
        (true, PlacementKind::Main) => "shared checkout".to_owned(),
        (true, PlacementKind::Worktree) => format!("{text} · shared"),
    };
    Some(PlacementBadge {
        text,
        shared: others > 0,
        tooltip: lines.join("\n"),
    })
}

/// Whether `session` has an agent that runs a turn or waits for the user: what "another live
/// agent works in this checkout" counts (the Start sheet's warning). The same rule as the
/// daemon's, which decides the badge above and the warning a task start returns
/// (`hutan_daemon::placement::works_in_checkout`): an idle terminal, a chat between turns (an
/// empty Mawas session included) and a session whose chat edits no files (Mawas, a direct model
/// chat) do not count.
pub fn works_in_checkout(session: &SessionInfo) -> bool {
    if !session.state.is_live() {
        return false;
    }
    let terminal_agent = session
        .agent
        .as_ref()
        .is_some_and(|agent| matches!(agent.state, AgentState::Working | AgentState::Blocked));
    let chat = session.chat.as_ref().is_some_and(|chat| {
        !chat.is_assistant()
            && chat.provider_id.is_none()
            && chat.agent != "direct"
            && (chat.turn_active || chat.pending_approvals > 0 || chat.pending_questions > 0)
    });
    terminal_agent || chat
}

/// What a quiet sidebar row says about where its session works, on a second line: the badge of
/// [`placement_badge`] only when the session is in a linked worktree (not the main checkout every
/// row would repeat). A checkout another live agent works in is not a line of its own: the row
/// wears a warning glyph in its trailing slot and keeps its one-line height.
pub fn row_placement_badge(session: &SessionInfo) -> Option<PlacementBadge> {
    let placement = session.placement.as_ref()?;
    (placement.kind == PlacementKind::Worktree)
        .then(|| placement_badge(session))
        .flatten()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hutan_core::{SessionId, SessionKind, SessionState, WorkspaceId};
    use hutan_protocol::worktree::Placement;

    use super::*;

    fn session(placement: Option<Placement>) -> SessionInfo {
        SessionInfo {
            containment: None,
            id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
            kind: SessionKind::Terminal,
            title: "t".into(),
            state: SessionState::Running,
            command: vec![],
            cwd: None,
            terminal_id: None,
            created_at_ms: 0,
            agent: None,
            restored: false,
            chat: None,
            placement,
            folder: None,
            recovery: None,
            muted: false,
        }
    }

    fn placement(kind: PlacementKind, branch: Option<&str>, shared: usize) -> Placement {
        Placement {
            kind,
            root: PathBuf::from("/work/repo"),
            branch: branch.map(str::to_owned),
            shared_with: (0..shared).map(|_| SessionId::new()).collect(),
        }
    }

    #[test]
    fn no_placement_no_badge() {
        assert_eq!(placement_badge(&session(None)), None);
    }

    #[test]
    fn the_text_is_main_or_wt_with_the_branch() {
        let badge = |p| placement_badge(&session(Some(p))).unwrap();
        assert_eq!(
            badge(placement(PlacementKind::Main, Some("master"), 0)).text,
            "main"
        );
        assert_eq!(
            badge(placement(PlacementKind::Worktree, Some("feature/x"), 0)).text,
            "wt feature/x"
        );
        assert_eq!(
            badge(placement(PlacementKind::Worktree, None, 0)).text,
            "wt (detached)"
        );
    }

    #[test]
    fn a_row_only_shows_a_placement_that_is_not_the_quiet_default() {
        let badge = |p| row_placement_badge(&session(Some(p))).map(|b| (b.text, b.shared));
        assert_eq!(
            badge(placement(PlacementKind::Main, Some("master"), 0)),
            None
        );
        assert_eq!(
            badge(placement(PlacementKind::Worktree, Some("x"), 0)),
            Some(("wt x".to_owned(), false))
        );
        assert_eq!(
            badge(placement(PlacementKind::Main, Some("master"), 1)),
            None,
            "a shared main checkout is a glyph in the row, not a second line"
        );
        assert_eq!(
            badge(placement(PlacementKind::Worktree, Some("x"), 1)),
            Some(("wt x · shared".to_owned(), true))
        );
        assert_eq!(row_placement_badge(&session(None)), None);
    }

    #[test]
    fn sharing_warns_and_the_tooltip_says_who_and_where() {
        let alone = placement_badge(&session(Some(placement(
            PlacementKind::Main,
            Some("master"),
            0,
        ))))
        .unwrap();
        assert!(!alone.shared);
        assert_eq!(alone.tooltip, "Main checkout\n/work/repo\nBranch master");

        let one = placement_badge(&session(Some(placement(
            PlacementKind::Worktree,
            Some("x"),
            1,
        ))))
        .unwrap();
        assert!(one.shared);
        assert!(
            one.tooltip
                .contains("⚠ Another live agent works in this checkout")
        );
        assert_eq!(one.text, "wt x · shared");
        let two = placement_badge(&session(Some(placement(PlacementKind::Main, None, 2)))).unwrap();
        assert!(
            two.tooltip
                .contains("⚠ 2 other live agents work in this checkout")
        );
        assert_eq!(two.text, "shared checkout");
    }

    fn chat(agent: &str, turn_active: bool) -> hutan_protocol::chat::ChatInfo {
        hutan_protocol::chat::ChatInfo {
            agent: agent.to_owned(),
            model: String::new(),
            approval_policy: hutan_core::ApprovalPolicy::default(),
            sandbox: hutan_core::SandboxMode::default(),
            turn_active,
            pending_approvals: 0,
            pending_questions: 0,
            provider_id: None,
            agent_session_id: None,
            assistant: false,
            images: None,
            mode: None,
            checkpoints: None,
            account: None,
            safety: None,
        }
    }

    fn report(state: AgentState) -> hutan_protocol::messages::AgentReport {
        hutan_protocol::messages::AgentReport {
            source: "test".into(),
            agent: "omp".into(),
            state,
            message: None,
            agent_session_id: None,
            updated_at_ms: 0,
            blocked_sub: None,
        }
    }

    /// F-12: an idle terminal and an empty Mawas session are no "other live agent"; a running or
    /// waiting turn is.
    #[test]
    fn only_a_running_or_waiting_agent_turn_works_in_a_checkout() {
        assert!(!works_in_checkout(&session(None)), "an idle terminal");

        let mut agent = session(None);
        agent.agent = Some(report(AgentState::Idle));
        assert!(!works_in_checkout(&agent), "a terminal agent between turns");
        agent.agent = Some(report(AgentState::Working));
        assert!(works_in_checkout(&agent));
        agent.agent = Some(report(AgentState::Blocked));
        assert!(works_in_checkout(&agent), "waiting for a decision counts");
        agent.state = SessionState::Stopped;
        assert!(!works_in_checkout(&agent), "a stopped session never does");

        let mut empty = session(None);
        empty.chat = Some(chat("omp", false));
        assert!(!works_in_checkout(&empty), "a chat between turns");
        empty.chat = Some(chat("omp", true));
        assert!(works_in_checkout(&empty), "a chat that runs a turn");
        let mut asking = session(None);
        let mut waiting = chat("codex", false);
        waiting.pending_approvals = 1;
        asking.chat = Some(waiting);
        assert!(
            works_in_checkout(&asking),
            "a chat that waits for an answer"
        );

        let mut mawas = session(None);
        let mut assistant = chat("omp", true);
        assistant.assistant = true;
        mawas.chat = Some(assistant);
        assert!(!works_in_checkout(&mawas), "Mawas edits no files");
        let mut direct = session(None);
        direct.chat = Some(chat("direct", true));
        assert!(
            !works_in_checkout(&direct),
            "a direct model chat edits no files"
        );
    }
}
