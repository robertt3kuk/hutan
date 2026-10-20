//! Attention (design.md § Attention model) for M2: terminal sessions only.
//!
//! `failed` (exit ≠ 0, a signal, a spawn failure) and `done` (exit 0, or a terminal
//! notification) are *unread* until the session's view is focused; everything else is idle.
//! A coding agent that reports through `agent.report` (M3.3) adds working / blocked / idle to
//! running sessions; an agent chat session (M4) says the same from its own state: a turn in
//! progress is working, a pending permission request is blocked (and needs the user).
//!
//! GPUI-free and unit tested: [`AttentionTracker`] watches successive session lists and
//! remembers what the user has not seen yet.

use std::collections::{BTreeMap, HashMap};

use hutan_core::{Epoch, SessionId, SessionState};
use hutan_protocol::messages::{AgentReport, AgentState, SessionInfo};

/// Why a session wants a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// Exited 0 or sent a terminal notification.
    Done,
    /// Exited non-zero, was killed by a signal, or failed to start.
    Failed,
}

/// Color role of a state chip (mapped to theme colors by the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Warning,
    Success,
    Accent,
    Muted,
    Error,
}

/// How a session shows up in lists, tabs and the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presentation {
    /// Short state word: `idle`, `done`, `failed`, `stopped`, …
    pub label: String,
    /// Subtitle detail such as `exit 2` or `signal 9`; empty when there is nothing to add.
    pub detail: String,
    pub tone: Tone,
    /// Filled dot (needs a look or is active) rather than hollow.
    pub filled: bool,
    pub needs_you: bool,
}

impl Presentation {
    /// The state word remains the accessible meaning; artwork is independently replaceable.
    pub fn icon(&self) -> crate::icons::Icon {
        use crate::icons::Icon;
        match self.label.as_str() {
            "working" | "starting" | "running" => Icon::StateWorking,
            "blocked" => Icon::StateBlocked,
            "approval" => Icon::StateApproval,
            "question" => Icon::StateQuestion,
            "done" | "completed" | "exited" => Icon::StateCompleted,
            "failed" => Icon::StateFailed,
            "stopped" => Icon::StateStopped,
            "interrupted" => Icon::StateInterrupted,
            "disconnected" => Icon::StateDisconnected,
            "review" => Icon::StateReview,
            "idle" => Icon::StateIdle,
            _ => Icon::StateUnknown,
        }
    }
}

/// Why a session counts in the menu bar item and the Dock badge (design.md § Attention
/// model → macOS surfaces). Finished-and-unread sessions are in the sidebar's *Needs you* but
/// not here: a badge is for what blocks someone or went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionKind {
    /// An agent in the session is blocked.
    Blocked,
    /// A permission request waits for an answer.
    Approval,
    /// An agent's question waits for an answer (protocol 0.24).
    Question,
    /// Exited non-zero, was killed, or failed to start, and the user has not looked.
    Failed,
}

impl AttentionKind {
    fn word(self) -> &'static str {
        match self {
            AttentionKind::Blocked => "blocked",
            AttentionKind::Approval => "approval",
            AttentionKind::Question => "question",
            AttentionKind::Failed => "failed",
        }
    }
}

/// One session that needs the user, as the macOS surfaces list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionEntry {
    pub session: SessionId,
    pub title: String,
    pub kind: AttentionKind,
    /// `exit 2`, `2 approvals`, the agent's message; empty when there is nothing to add.
    pub detail: String,
}

impl AttentionEntry {
    /// `claude — blocked: running tests`.
    pub fn text(&self) -> String {
        if self.detail.is_empty() {
            format!("{} — {}", self.title, self.kind.word())
        } else {
            format!("{} — {}: {}", self.title, self.kind.word(), self.detail)
        }
    }
}

/// How a session that `presentation` describes, with `pending_approvals` requests and
/// `pending_questions` questions waiting, counts for the macOS surfaces; `None` when it does not.
pub fn classify(
    presentation: &Presentation,
    pending_approvals: usize,
    pending_questions: usize,
) -> Option<AttentionKind> {
    if pending_approvals > 0 {
        return Some(AttentionKind::Approval);
    }
    if pending_questions > 0 {
        return Some(AttentionKind::Question);
    }
    if !presentation.needs_you {
        return None;
    }
    match presentation.tone {
        Tone::Warning => Some(AttentionKind::Blocked),
        Tone::Error => Some(AttentionKind::Failed),
        Tone::Success | Tone::Accent | Tone::Muted => None,
    }
}

/// What the unread flag of a state transition should become when a live session ends.
fn unread_for_end(state: &SessionState) -> Option<Unread> {
    match state {
        SessionState::Exited {
            code: Some(0),
            signal: None,
        } => Some(Unread::Done),
        SessionState::Exited { .. } | SessionState::Failed { .. } => Some(Unread::Failed),
        // The user asked for it; there is nothing to look at.
        SessionState::Stopped => None,
        _ => None,
    }
}

/// Presentation of `info` given its unread flag.
pub fn present(info: &SessionInfo, unread: Option<Unread>) -> Presentation {
    let exit_detail = |code: Option<i32>, signal: Option<i32>| match (code, signal) {
        (_, Some(sig)) => format!("signal {sig}"),
        (Some(code), None) => format!("exit {code}"),
        (None, None) => String::new(),
    };
    match &info.state {
        SessionState::Starting => Presentation {
            label: "starting".into(),
            detail: String::new(),
            tone: Tone::Accent,
            filled: false,
            needs_you: false,
        },
        // A terminal notification from a running session: it has something to show.
        SessionState::Running | SessionState::Idle if unread.is_some() => Presentation {
            label: "done".into(),
            detail: "notification".into(),
            tone: Tone::Success,
            filled: true,
            needs_you: true,
        },
        // An agent chat: the daemon runs the agent, so its state is exact.
        SessionState::Running | SessionState::Idle if info.chat.is_some() => {
            let busy = info.state == SessionState::Running;
            Presentation {
                label: if busy { "working" } else { "idle" }.into(),
                detail: chat_detail(info),
                tone: if busy { Tone::Accent } else { Tone::Muted },
                filled: busy,
                needs_you: false,
            }
        }
        SessionState::WaitingApproval if info.chat.is_some() => {
            let pending = info.chat.as_ref().map_or(0, |chat| chat.pending_approvals);
            let asked = info.chat.as_ref().map_or(0, |chat| chat.pending_questions);
            Presentation {
                label: "blocked".into(),
                detail: match (pending, asked) {
                    (0, 0) | (1, 0) => "approval".to_owned(),
                    (0, 1) => "question".to_owned(),
                    (0, n) => format!("{n} questions"),
                    (n, 0) => format!("{n} approvals"),
                    (a, q) => format!("{a} approvals, {q} questions"),
                },
                tone: Tone::Warning,
                filled: true,
                needs_you: true,
            }
        }
        // A coding agent that reports (`agent.report`) says more than a bare "idle".
        SessionState::Running | SessionState::Idle => match &info.agent {
            Some(report) => present_agent(report),
            None => Presentation {
                label: "idle".into(),
                detail: String::new(),
                tone: Tone::Muted,
                filled: false,
                needs_you: false,
            },
        },
        SessionState::WaitingApproval => Presentation {
            label: "approval".into(),
            detail: String::new(),
            tone: Tone::Warning,
            filled: true,
            needs_you: true,
        },
        SessionState::Exited { code, signal } => {
            let detail = exit_detail(*code, *signal);
            match unread {
                Some(Unread::Done) => Presentation {
                    label: "done".into(),
                    detail,
                    tone: Tone::Success,
                    filled: true,
                    needs_you: true,
                },
                Some(Unread::Failed) => Presentation {
                    label: "failed".into(),
                    detail,
                    tone: Tone::Error,
                    filled: true,
                    needs_you: true,
                },
                None => Presentation {
                    label: "exited".into(),
                    detail,
                    tone: if matches!((code, signal), (Some(0), None)) {
                        Tone::Muted
                    } else {
                        Tone::Error
                    },
                    filled: false,
                    needs_you: false,
                },
            }
        }
        // An assistant that could not start says so in the Assistant panel, which offers Retry
        // and another backend; it is not a session the user went away from.
        SessionState::Failed { reason } => {
            let looked_after =
                unread.is_some() && !info.chat.as_ref().is_some_and(|chat| chat.is_assistant());
            Presentation {
                label: "failed".into(),
                detail: reason.clone(),
                tone: Tone::Error,
                filled: looked_after,
                needs_you: looked_after,
            }
        }
        SessionState::Stopped => Presentation {
            label: "stopped".into(),
            detail: String::new(),
            tone: Tone::Muted,
            filled: false,
            needs_you: false,
        },
    }
}

/// `codex · gpt-6.1-sol`: which agent and model a chat session runs.
fn chat_detail(info: &SessionInfo) -> String {
    match &info.chat {
        Some(chat) => format!("{} · {}", chat.agent, chat.model),
        None => String::new(),
    }
}

/// The one-line status a terminal view shows when its session ended.
pub fn ended_text(state: &SessionState) -> Option<String> {
    match state {
        SessionState::Exited {
            code: Some(0),
            signal: None,
        } => Some("The process exited with code 0".to_owned()),
        SessionState::Exited {
            code,
            signal: Some(sig),
        } => Some(format!(
            "The process was killed by signal {sig}{}",
            code.map(|c| format!(" (code {c})")).unwrap_or_default()
        )),
        SessionState::Exited {
            code: Some(code), ..
        } => Some(format!("The process exited with code {code}")),
        SessionState::Exited { .. } => Some("The process exited".to_owned()),
        SessionState::Failed { reason } => Some(format!("The session failed: {reason}")),
        SessionState::Stopped => Some("The session was stopped".to_owned()),
        _ => None,
    }
}

/// Remembers which sessions the user has not looked at since they ended.
#[derive(Debug, Default)]
pub struct AttentionTracker {
    unread: BTreeMap<SessionId, Unread>,
    /// Whether each known session was live in the last list; `None` until the first synchronised
    /// list of the current daemon epoch has been taken as the baseline.
    baseline: Option<HashMap<SessionId, bool>>,
    epoch: Option<Epoch>,
}

impl AttentionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn unread(&self, id: SessionId) -> Option<Unread> {
        self.unread.get(&id).copied()
    }

    /// What is unread, for comparing before and after a change.
    pub fn unread_map(&self) -> &BTreeMap<SessionId, Unread> {
        &self.unread
    }

    /// Sessions flagged unread.
    pub fn unread_ids(&self) -> impl Iterator<Item = SessionId> + '_ {
        self.unread.keys().copied()
    }

    /// Looks at the latest synchronised session list. The first list of an epoch is the
    /// baseline (what was already over when the desktop arrived is not news); later lists flag
    /// sessions that were live and now ended, and clear the flag of sessions that restarted.
    pub fn observe(&mut self, epoch: Option<Epoch>, sessions: &[SessionInfo]) {
        if self.epoch != epoch {
            self.epoch = epoch;
            self.baseline = None;
            self.unread.clear();
        }
        let now: HashMap<SessionId, bool> =
            sessions.iter().map(|s| (s.id, s.state.is_live())).collect();
        if let Some(before) = &self.baseline {
            for session in sessions {
                let was_live = before.get(&session.id).copied();
                if session.state.is_live() {
                    // A restart (ended before, live now) starts a new life; a live session
                    // keeps a notification until the user looks.
                    if was_live == Some(false) {
                        self.unread.remove(&session.id);
                    }
                } else if was_live == Some(true)
                    && let Some(kind) = unread_for_end(&session.state)
                {
                    self.unread.insert(session.id, kind);
                }
            }
        }
        self.unread.retain(|id, _| now.contains_key(id));
        self.baseline = Some(now);
    }

    /// A terminal notification arrived for `session`: it wants a look unless something worse is
    /// already flagged.
    pub fn notified(&mut self, session: SessionId) {
        if self
            .baseline
            .as_ref()
            .is_some_and(|b| b.contains_key(&session))
        {
            self.unread.entry(session).or_insert(Unread::Done);
        }
    }

    /// The user looked at `session`.
    pub fn mark_read(&mut self, session: SessionId) -> bool {
        self.unread.remove(&session).is_some()
    }
}

/// Presentation of a live session whose agent has reported.
fn present_agent(report: &AgentReport) -> Presentation {
    let detail = match &report.message {
        Some(message) => format!("{}: {message}", report.agent),
        None => report.agent.clone(),
    };
    let (label, tone, filled, needs_you) = match report.state {
        AgentState::Idle => ("idle", Tone::Muted, false, false),
        AgentState::Working => ("working", Tone::Accent, true, false),
        AgentState::Blocked => ("blocked", Tone::Warning, true, true),
    };
    Presentation {
        label: label.into(),
        detail,
        tone,
        filled,
        needs_you,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::WorkspaceId;

    fn info(id: SessionId, state: SessionState) -> SessionInfo {
        SessionInfo {
            id,
            workspace_id: WorkspaceId::new(),
            kind: hutan_core::SessionKind::Terminal,
            title: "t".into(),
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
            recovery: None,
        }
    }

    fn exited(code: i32) -> SessionState {
        SessionState::Exited {
            code: Some(code),
            signal: None,
        }
    }

    fn presentation_of(state: SessionState, unread: Option<Unread>) -> Presentation {
        present(&info(SessionId::new(), state), unread)
    }

    #[test]
    fn only_blocked_approval_and_failed_sessions_count_for_the_macos_surfaces() {
        let failed = presentation_of(exited(2), Some(Unread::Failed));
        assert_eq!(classify(&failed, 0, 0), Some(AttentionKind::Failed));
        let done = presentation_of(exited(0), Some(Unread::Done));
        assert!(done.needs_you);
        assert_eq!(
            classify(&done, 0, 0),
            None,
            "a finished session is not a badge"
        );
        let seen = presentation_of(exited(2), None);
        assert_eq!(
            classify(&seen, 0, 0),
            None,
            "a failure the user looked at is over"
        );
        let idle = presentation_of(SessionState::Idle, None);
        assert_eq!(classify(&idle, 0, 0), None);
        let waiting = presentation_of(SessionState::WaitingApproval, None);
        assert_eq!(classify(&waiting, 0, 0), Some(AttentionKind::Blocked));
        assert_eq!(classify(&idle, 1, 0), Some(AttentionKind::Approval));
        assert_eq!(classify(&failed, 1, 0), Some(AttentionKind::Approval));
    }

    #[test]
    fn an_assistant_that_failed_to_start_is_not_in_needs_you_but_a_failed_chat_is() {
        let failed = || SessionState::Failed {
            reason: "agent request `session/new` failed".into(),
        };
        let chat_failed = present(&chat(failed(), false, 0), Some(Unread::Failed));
        assert!(chat_failed.needs_you);
        assert_eq!(classify(&chat_failed, 0, 0), Some(AttentionKind::Failed));

        let mut assistant = chat(failed(), false, 0);
        if let Some(c) = assistant.chat.as_mut() {
            c.assistant = true;
        }
        let shown = present(&assistant, Some(Unread::Failed));
        assert_eq!(shown.label, "failed", "the row still says what happened");
        assert!(!shown.needs_you);
        assert_eq!(classify(&shown, 0, 0), None, "no badge for it either");
        assert_eq!(
            classify(&shown, 1, 0),
            Some(AttentionKind::Approval),
            "a pending approval always counts"
        );
    }

    #[test]
    fn an_entry_reads_like_a_menu_line() {
        let mut entry = AttentionEntry {
            session: SessionId::new(),
            title: "claude".into(),
            kind: AttentionKind::Failed,
            detail: String::new(),
        };
        assert_eq!(entry.text(), "claude — failed");
        entry.detail = "exit 2".into();
        assert_eq!(entry.text(), "claude — failed: exit 2");
    }

    #[test]
    fn the_first_list_is_a_baseline_not_news() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let mut tracker = AttentionTracker::new();
        tracker.observe(
            Some(Epoch::new()),
            &[info(a, exited(1)), info(b, SessionState::Running)],
        );
        assert_eq!(tracker.unread(a), None);
        assert_eq!(tracker.unread(b), None);
    }

    #[test]
    fn live_to_ended_flags_done_or_failed_and_stop_flags_nothing() {
        let (ok, bad, sig, stopped, failed) = (
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
        );
        let epoch = Some(Epoch::new());
        let mut tracker = AttentionTracker::new();
        let all = |state: &dyn Fn(SessionId) -> SessionState| {
            [ok, bad, sig, stopped, failed]
                .into_iter()
                .map(|id| info(id, state(id)))
                .collect::<Vec<_>>()
        };
        tracker.observe(epoch, &all(&|_| SessionState::Running));
        tracker.observe(
            epoch,
            &all(&|id| {
                if id == ok {
                    exited(0)
                } else if id == bad {
                    exited(2)
                } else if id == sig {
                    SessionState::Exited {
                        code: None,
                        signal: Some(9),
                    }
                } else if id == stopped {
                    SessionState::Stopped
                } else {
                    SessionState::Failed {
                        reason: "spawn".into(),
                    }
                }
            }),
        );
        assert_eq!(tracker.unread(ok), Some(Unread::Done));
        assert_eq!(tracker.unread(bad), Some(Unread::Failed));
        assert_eq!(tracker.unread(sig), Some(Unread::Failed));
        assert_eq!(tracker.unread(failed), Some(Unread::Failed));
        assert_eq!(tracker.unread(stopped), None);
    }

    #[test]
    fn read_restart_and_removal_clear_the_flag() {
        let (a, b, c) = (SessionId::new(), SessionId::new(), SessionId::new());
        let epoch = Some(Epoch::new());
        let mut tracker = AttentionTracker::new();
        let live = |ids: &[SessionId]| {
            ids.iter()
                .map(|id| info(*id, SessionState::Running))
                .collect::<Vec<_>>()
        };
        tracker.observe(epoch, &live(&[a, b, c]));
        tracker.observe(
            epoch,
            &[info(a, exited(0)), info(b, exited(1)), info(c, exited(1))],
        );
        assert!(tracker.mark_read(a));
        assert!(!tracker.mark_read(a));
        tracker.observe(
            epoch,
            &[info(a, exited(0)), info(b, SessionState::Starting)],
        );
        assert_eq!(tracker.unread(a), None, "reading is sticky");
        assert_eq!(tracker.unread(b), None, "a restart clears it");
        assert_eq!(tracker.unread(c), None, "a removed session is forgotten");
    }

    #[test]
    fn notifications_flag_known_sessions_once() {
        let (a, ghost) = (SessionId::new(), SessionId::new());
        let epoch = Some(Epoch::new());
        let mut tracker = AttentionTracker::new();
        tracker.notified(a);
        assert_eq!(
            tracker.unread(a),
            None,
            "nothing is known before the baseline"
        );
        tracker.observe(epoch, &[info(a, SessionState::Running)]);
        tracker.notified(ghost);
        assert_eq!(tracker.unread(ghost), None);
        tracker.notified(a);
        assert_eq!(tracker.unread(a), Some(Unread::Done));
    }

    #[test]
    fn a_notification_survives_later_updates_of_a_live_session() {
        let a = SessionId::new();
        let epoch = Some(Epoch::new());
        let mut tracker = AttentionTracker::new();
        tracker.observe(epoch, &[info(a, SessionState::Running)]);
        tracker.notified(a);
        assert_eq!(tracker.unread(a), Some(Unread::Done));
        tracker.observe(epoch, &[info(a, SessionState::Running)]);
        assert_eq!(tracker.unread(a), Some(Unread::Done), "still unread");
        let live = present(&info(a, SessionState::Running), tracker.unread(a));
        assert_eq!((live.label.as_str(), live.needs_you), ("done", true));
    }

    #[test]
    fn a_new_epoch_starts_over() {
        let (a, b, c) = (SessionId::new(), SessionId::new(), SessionId::new());
        let mut tracker = AttentionTracker::new();
        let epoch = Some(Epoch::new());
        let running = [a, b, c].map(|id| info(id, SessionState::Running));
        tracker.observe(epoch, &running);
        tracker.observe(
            epoch,
            &[
                info(a, exited(1)),
                info(b, SessionState::Running),
                info(c, exited(0)),
            ],
        );
        tracker.observe(Some(Epoch::new()), &running);
        assert_eq!(tracker.unread_ids().count(), 0);
    }

    #[test]
    fn presentation_follows_the_unread_flag() {
        let id = SessionId::new();
        let done = info(id, exited(0));
        let unread = present(&done, Some(Unread::Done));
        assert_eq!((unread.label.as_str(), unread.needs_you), ("done", true));
        assert_eq!(unread.tone, Tone::Success);
        let read = present(&done, None);
        assert_eq!((read.label.as_str(), read.needs_you), ("exited", false));
        let failed = present(&info(id, exited(3)), Some(Unread::Failed));
        assert_eq!(
            (failed.label.as_str(), failed.detail.as_str()),
            ("failed", "exit 3")
        );
        assert_eq!(
            present(&info(id, SessionState::Running), None).label,
            "idle"
        );
        assert_eq!(
            ended_text(&exited(3)).as_deref(),
            Some("The process exited with code 3")
        );
    }

    fn chat(state: SessionState, turn_active: bool, pending: u32) -> SessionInfo {
        let mut session = info(SessionId::new(), state);
        session.kind = hutan_core::SessionKind::Harness;
        session.chat = Some(hutan_protocol::chat::ChatInfo {
            agent: "codex".into(),
            model: "gpt-6.1-sol".into(),
            approval_policy: hutan_core::ApprovalPolicy::OnRequest,
            sandbox: hutan_core::SandboxMode::WorkspaceWrite,
            turn_active,
            pending_approvals: pending,
            pending_questions: 0,
            provider_id: None,
            agent_session_id: None,
            assistant: false,
            images: None,
        });
        session
    }

    #[test]
    fn a_chat_session_is_working_during_a_turn_and_blocked_on_an_approval() {
        let idle = present(&chat(SessionState::Idle, false, 0), None);
        assert_eq!((idle.label.as_str(), idle.needs_you), ("idle", false));
        assert_eq!(idle.detail, "codex · gpt-6.1-sol");
        let working = present(&chat(SessionState::Running, true, 0), None);
        assert_eq!(
            (working.label.as_str(), working.tone, working.needs_you),
            ("working", Tone::Accent, false)
        );
        assert!(working.filled);
        let blocked = present(&chat(SessionState::WaitingApproval, true, 2), None);
        assert_eq!(
            (blocked.label.as_str(), blocked.tone, blocked.needs_you),
            ("blocked", Tone::Warning, true)
        );
        assert_eq!(blocked.detail, "2 approvals");
        let one = present(&chat(SessionState::WaitingApproval, true, 1), None);
        assert_eq!(one.detail, "approval");
    }

    #[test]
    fn a_reporting_agent_sets_the_label_and_a_blocked_one_needs_you() {
        let report = |state, message: Option<&str>| {
            let mut session = info(SessionId::new(), SessionState::Running);
            session.agent = Some(AgentReport {
                source: "hooks".into(),
                agent: "omp".into(),
                state,
                message: message.map(str::to_owned),
                agent_session_id: None,
                updated_at_ms: 1,
                blocked_sub: None,
            });
            present(&session, None)
        };
        let working = report(AgentState::Working, None);
        assert_eq!(
            (
                working.label.as_str(),
                working.detail.as_str(),
                working.tone
            ),
            ("working", "omp", Tone::Accent)
        );
        assert!(working.filled && !working.needs_you);
        let blocked = report(AgentState::Blocked, Some("approve rm"));
        assert_eq!(
            (
                blocked.label.as_str(),
                blocked.detail.as_str(),
                blocked.tone
            ),
            ("blocked", "omp: approve rm", Tone::Warning)
        );
        assert!(blocked.filled && blocked.needs_you);
        let idle = report(AgentState::Idle, None);
        assert_eq!(
            (idle.label.as_str(), idle.detail.as_str(), idle.tone),
            ("idle", "omp", Tone::Muted)
        );
        assert!(!idle.filled && !idle.needs_you);
    }
}
