//! What a daemon restart did to the sessions (protocol 0.26, capability `session.recovery`),
//! in words: the chip a session wears until it is acknowledged and the summary the desktop and
//! `hutan recovery` show once per restart. Only facts the daemon reported are said; nothing here
//! claims a program kept running through the restart.

use std::path::Path;

use hutan_core::SessionId;
use hutan_protocol::messages::SessionInfo;
use hutan_protocol::recovery::{DaemonStop, LastTurn, Recovery, RecoveryKind};

/// What a chip says, by how much attention it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipTone {
    /// Back as it was, as far as it can be: `respawned`, `resumed`.
    Info,
    /// Something was cut off or is not running: `interrupted`, `not restarted`.
    Warn,
}

/// The small label of a session row and header until the recovery is acknowledged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub label: &'static str,
    pub tone: ChipTone,
    /// One sentence for the terminal header and tooltips.
    pub detail: String,
}

/// A recovery that is worth telling the user about: a session that had ended before the restart
/// is only listed for history.
pub fn is_notable(recovery: &Recovery) -> bool {
    !matches!(recovery.kind, RecoveryKind::EndedBefore)
}

/// The chip of `info`, when its restart left something to say.
pub fn chip(info: &SessionInfo) -> Option<Chip> {
    let recovery = info.recovery.as_ref().filter(|r| is_notable(r))?;
    let (label, tone) = match &recovery.kind {
        RecoveryKind::ShellRespawned { .. } => ("respawned", ChipTone::Info),
        RecoveryKind::AgentResumed { .. } => ("resumed", ChipTone::Info),
        RecoveryKind::ChatResumed {
            last_turn: LastTurn::Interrupted,
            ..
        } => ("interrupted", ChipTone::Warn),
        RecoveryKind::ChatResumed { .. } => ("resumed", ChipTone::Info),
        RecoveryKind::NotRestarted { .. } => ("not restarted", ChipTone::Warn),
        RecoveryKind::EndedBefore => return None,
    };
    Some(Chip {
        label,
        tone,
        detail: detail(recovery),
    })
}

/// One sentence about what became of the session.
pub fn detail(recovery: &Recovery) -> String {
    match &recovery.kind {
        RecoveryKind::ShellRespawned { cwd, was_running } => {
            let mut text = match cwd {
                Some(cwd) => format!("A new shell started in {}.", short_path(cwd)),
                None => "A new shell started.".to_owned(),
            };
            match was_running {
                Some(command) => text.push_str(&format!(
                    " `{command}` was running when hutand ended; it is not running now."
                )),
                None => text.push_str(" Its screen and processes did not survive."),
            }
            text
        }
        RecoveryKind::AgentResumed { resume_argv } => format!(
            "The agent was started again with `{}`; its screen did not survive.",
            argv_text(resume_argv)
        ),
        RecoveryKind::ChatResumed {
            thread_id,
            last_turn,
        } => {
            let mut text = match thread_id {
                Some(_) => "The agent resumed its conversation.".to_owned(),
                None => "The agent started a new conversation; the transcript is kept.".to_owned(),
            };
            if *last_turn == LastTurn::Interrupted {
                text.push_str(" The last turn was interrupted before it finished.");
            }
            text
        }
        RecoveryKind::NotRestarted { reason } => format!("Not restarted: {reason}."),
        RecoveryKind::EndedBefore => "It had ended before hutand restarted.".to_owned(),
    }
}

/// `claude --resume abc…`: the first words of a resume command, cut short.
pub fn argv_text(argv: &[String]) -> String {
    const KEPT_WORDS: usize = 3;
    const MAX_CHARS: usize = 48;
    let mut text = argv
        .iter()
        .take(KEPT_WORDS)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let mut cut = argv.len() > KEPT_WORDS;
    if text.chars().count() > MAX_CHARS {
        text = text.chars().take(MAX_CHARS).collect();
        cut = true;
    }
    if cut {
        text.push('…');
    }
    text
}

/// A path as the user knows it: `~/…` under the home directory is not known here, so only the
/// last two components are kept for a long one.
fn short_path(path: &Path) -> String {
    let parts: Vec<_> = path.components().collect();
    if parts.len() <= 4 {
        return path.display().to_string();
    }
    let tail: std::path::PathBuf = parts[parts.len() - 2..].iter().collect();
    format!("…/{}", tail.display())
}

/// What the summary offers on a row besides opening the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    /// The chat's turn was cut off: send its last message again.
    Retry,
    /// The session did not come back: start it.
    Restart,
}

impl RowAction {
    pub fn label(self) -> &'static str {
        match self {
            RowAction::Retry => "Retry last message",
            RowAction::Restart => "Restart",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRow {
    pub session: SessionId,
    pub title: String,
    pub chip: Chip,
    pub action: Option<RowAction>,
}

/// The sessions one daemon start restored, for the sheet that follows a reconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// When the daemon that restored them started: identifies the restart.
    pub restarted_at_ms: u64,
    /// `hutand restarted 3 min ago (it was killed or crashed).`
    pub headline: String,
    /// `5 sessions: 2 shells respawned, 1 chat turn interrupted, 1 not restarted (omp not found)`.
    pub counts: String,
    /// The daemon of this restart ended with a clean stop (the user stopped it, or an upgrade).
    pub clean_stop: bool,
    /// The ones that need a decision first.
    pub rows: Vec<SummaryRow>,
}

/// The summary of the unacknowledged recoveries of `sessions` that belong to the newest restart;
/// `None` when there is nothing to say. `when` tells when the daemon restarted, as the words
/// after "restarted": `3 min ago`, or `at 2026-10-17 14:31 +0500, 3 min ago`.
pub fn summary(sessions: &[SessionInfo], when: impl Fn(u64) -> String) -> Option<Summary> {
    let notable: Vec<(&SessionInfo, &Recovery)> = sessions
        .iter()
        .filter_map(|s| {
            s.recovery
                .as_ref()
                .filter(|r| is_notable(r))
                .map(|r| (s, r))
        })
        .collect();
    let restarted_at_ms = notable.iter().map(|(_, r)| r.restarted_at_ms).max()?;
    let mut rows: Vec<SummaryRow> = notable
        .iter()
        .filter(|(_, r)| r.restarted_at_ms == restarted_at_ms)
        .filter_map(|(s, r)| {
            Some(SummaryRow {
                session: s.id,
                title: s.title.clone(),
                chip: chip(s)?,
                action: action_of(s, r),
            })
        })
        .collect();
    // Worst first; the daemon's order within.
    rows.sort_by_key(|row| match row.chip.label {
        "not restarted" => 0,
        "interrupted" => 1,
        _ => 2,
    });
    let newest = notable
        .iter()
        .find(|(_, r)| r.restarted_at_ms == restarted_at_ms)
        .map(|(_, r)| *r)?;
    Some(Summary {
        restarted_at_ms,
        headline: headline(newest, &when),
        counts: counts(&notable, restarted_at_ms),
        clean_stop: matches!(newest.daemon_stop, DaemonStop::Clean { .. }),
        rows,
    })
}

impl Summary {
    /// The restart followed a clean stop and nothing was cut off or left down: there is nothing
    /// to decide, and the user, who stopped the daemon, needs no card to tell them it came back.
    pub fn is_calm(&self) -> bool {
        self.clean_stop && self.rows.iter().all(|row| row.chip.tone != ChipTone::Warn)
    }

    /// The summary as plain lines: the headline, the counts, then one line per session
    /// (`  respawned      build  A new shell started in …`), with what can be done about it.
    pub fn text(&self) -> String {
        let mut lines = vec![self.headline.clone(), self.counts.clone()];
        let width = self
            .rows
            .iter()
            .map(|row| row.chip.label.len())
            .max()
            .unwrap_or(0);
        for row in &self.rows {
            let mut line = format!(
                "  {:<width$}  {}  {}",
                row.chip.label, row.title, row.chip.detail
            );
            if let Some(action) = row.action {
                line.push_str(&format!(
                    " [{}]",
                    match action {
                        RowAction::Retry => "hutan chat send to retry",
                        RowAction::Restart => "hutan restart",
                    }
                ));
            }
            lines.push(line);
        }
        lines.join("\n")
    }
}

fn action_of(session: &SessionInfo, recovery: &Recovery) -> Option<RowAction> {
    match &recovery.kind {
        RecoveryKind::ChatResumed {
            last_turn: LastTurn::Interrupted,
            ..
        } if session.state.is_live() => Some(RowAction::Retry),
        RecoveryKind::NotRestarted { .. } if !session.state.is_live() => Some(RowAction::Restart),
        _ => None,
    }
}

fn headline(recovery: &Recovery, when: &impl Fn(u64) -> String) -> String {
    let how = match recovery.daemon_stop {
        DaemonStop::Clean { .. } => " (it was stopped)",
        DaemonStop::Crashed => " (it was killed or crashed)",
        DaemonStop::Unknown => "",
    };
    format!("hutand restarted {}{how}.", when(recovery.restarted_at_ms))
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn counts(notable: &[(&SessionInfo, &Recovery)], restarted_at_ms: u64) -> String {
    let of_restart: Vec<&Recovery> = notable
        .iter()
        .map(|(_, r)| *r)
        .filter(|r| r.restarted_at_ms == restarted_at_ms)
        .collect();
    let mut shells = 0;
    let mut agents: Vec<&Vec<String>> = Vec::new();
    let mut chats = 0;
    let mut interrupted = 0;
    let mut not_restarted: Vec<&str> = Vec::new();
    for recovery in &of_restart {
        match &recovery.kind {
            RecoveryKind::ShellRespawned { .. } => shells += 1,
            RecoveryKind::AgentResumed { resume_argv } => agents.push(resume_argv),
            RecoveryKind::ChatResumed {
                last_turn: LastTurn::Interrupted,
                ..
            } => interrupted += 1,
            RecoveryKind::ChatResumed { .. } => chats += 1,
            RecoveryKind::NotRestarted { reason } => not_restarted.push(reason),
            RecoveryKind::EndedBefore => {}
        }
    }
    let mut parts = Vec::new();
    if shells > 0 {
        parts.push(format!("{} respawned", plural(shells, "shell", "shells")));
    }
    if !agents.is_empty() {
        let mut text = format!("{} resumed", plural(agents.len(), "agent", "agents"));
        if let [argv] = agents.as_slice() {
            text.push_str(&format!(" ({})", argv_text(argv)));
        }
        parts.push(text);
    }
    if chats > 0 {
        parts.push(format!("{} resumed", plural(chats, "chat", "chats")));
    }
    if interrupted > 0 {
        parts.push(format!(
            "{} interrupted",
            plural(interrupted, "chat turn", "chat turns")
        ));
    }
    if !not_restarted.is_empty() {
        let mut text = format!("{} not restarted", not_restarted.len());
        if let [reason] = not_restarted.as_slice() {
            text.push_str(&format!(" ({reason})"));
        }
        parts.push(text);
    }
    format!(
        "{}: {}",
        plural(of_restart.len(), "session", "sessions"),
        parts.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::{SessionKind, SessionState, WorkspaceId};
    use hutan_protocol::recovery::Lost;

    fn session(title: &str, state: SessionState, kind: Option<RecoveryKind>) -> SessionInfo {
        SessionInfo {
            forked_from: None,
            containment: None,
            terminal_agent: None,
            id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
            kind: SessionKind::Terminal,
            title: title.to_owned(),
            state,
            command: vec![],
            cwd: None,
            terminal_id: None,
            created_at_ms: 0,
            agent: None,
            restored: false,
            chat: None,
            placement: None,
            folder: None,
            recovery: kind.map(|kind| Recovery {
                kind,
                previous_epoch: None,
                daemon_stop: DaemonStop::Crashed,
                restarted_at_ms: 1_000,
                lost: Vec::<Lost>::new(),
            }),
            muted: false,
        }
    }

    fn clock(_: u64) -> String {
        "at 02:14".to_owned()
    }

    #[test]
    fn chips_say_what_became_of_the_session_and_ended_ones_stay_quiet() {
        let label = |kind| chip(&session("s", SessionState::Running, Some(kind))).map(|c| c.label);
        assert_eq!(
            label(RecoveryKind::ShellRespawned {
                cwd: None,
                was_running: None
            }),
            Some("respawned")
        );
        assert_eq!(
            label(RecoveryKind::AgentResumed {
                resume_argv: vec!["claude".into()]
            }),
            Some("resumed")
        );
        assert_eq!(
            label(RecoveryKind::ChatResumed {
                thread_id: Some("t".into()),
                last_turn: LastTurn::Interrupted
            }),
            Some("interrupted")
        );
        assert_eq!(
            label(RecoveryKind::NotRestarted { reason: "x".into() }),
            Some("not restarted")
        );
        assert_eq!(label(RecoveryKind::EndedBefore), None);
        assert_eq!(chip(&session("s", SessionState::Running, None)), None);
    }

    #[test]
    fn what_was_running_is_never_said_to_still_run() {
        let text = detail(
            session(
                "dev",
                SessionState::Running,
                Some(RecoveryKind::ShellRespawned {
                    cwd: Some("/work/app".into()),
                    was_running: Some("npm run dev".into()),
                }),
            )
            .recovery
            .as_ref()
            .unwrap(),
        );
        assert!(text.contains("/work/app"), "{text}");
        assert!(text.contains("`npm run dev` was running"), "{text}");
        assert!(text.contains("not running now"), "{text}");
    }

    #[test]
    fn the_summary_counts_the_newest_restart_and_offers_retry_and_restart() {
        let sessions = vec![
            session(
                "build",
                SessionState::Running,
                Some(RecoveryKind::ShellRespawned {
                    cwd: None,
                    was_running: None,
                }),
            ),
            session(
                "shell 2",
                SessionState::Running,
                Some(RecoveryKind::ShellRespawned {
                    cwd: None,
                    was_running: None,
                }),
            ),
            session(
                "claude",
                SessionState::Running,
                Some(RecoveryKind::AgentResumed {
                    resume_argv: vec!["claude".into(), "--resume".into(), "abc".into()],
                }),
            ),
            session(
                "chat",
                SessionState::Idle,
                Some(RecoveryKind::ChatResumed {
                    thread_id: Some("t".into()),
                    last_turn: LastTurn::Interrupted,
                }),
            ),
            session(
                "omp",
                SessionState::Stopped,
                Some(RecoveryKind::NotRestarted {
                    reason: "omp not found".into(),
                }),
            ),
            session(
                "old",
                SessionState::Stopped,
                Some(RecoveryKind::EndedBefore),
            ),
            session("plain", SessionState::Running, None),
        ];
        let summary = summary(&sessions, clock).expect("a summary");
        assert_eq!(summary.restarted_at_ms, 1_000);
        assert_eq!(
            summary.headline,
            "hutand restarted at 02:14 (it was killed or crashed)."
        );
        assert_eq!(
            summary.counts,
            "5 sessions: 2 shells respawned, 1 agent resumed (claude --resume abc), \
             1 chat turn interrupted, 1 not restarted (omp not found)"
        );
        let titles: Vec<_> = summary.rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["omp", "chat", "build", "shell 2", "claude"]);
        assert_eq!(summary.rows[0].action, Some(RowAction::Restart));
        assert_eq!(summary.rows[1].action, Some(RowAction::Retry));
        assert_eq!(summary.rows[2].action, None);
    }

    #[test]
    fn nothing_to_say_is_no_summary() {
        let sessions = vec![
            session("plain", SessionState::Running, None),
            session(
                "old",
                SessionState::Stopped,
                Some(RecoveryKind::EndedBefore),
            ),
        ];
        assert_eq!(summary(&sessions, clock), None);
    }

    #[test]
    fn a_resume_command_is_cut_short() {
        let argv: Vec<String> = ["claude", "--resume", "id", "--model", "x"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert_eq!(argv_text(&argv), "claude --resume id…");
        assert_eq!(argv_text(&argv[..2]), "claude --resume");
    }
}
