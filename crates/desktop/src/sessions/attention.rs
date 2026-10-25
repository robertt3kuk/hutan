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

use super::models::ModelNames;

/// Why a session wants a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// Exited 0 or sent a terminal notification: finished work not seen yet.
    Done,
    /// Exited non-zero, was killed by a signal, or failed to start.
    Failed,
    /// A chat turn ended and the user has not looked yet. Only a mark on the row (the dot and the
    /// title's weight), never a group: whether the turn left work for review is the task's
    /// business (FX-REVIEW: it changed files), and that is *Ready for review* by its own rule.
    Turn,
    /// A failure the user has looked at: it stays in *Failed* (without a badge) until a new run
    /// starts or the user marks it read.
    FailedSeen,
    /// The user marked it unread (*Mark as unread*): it stays until the session is looked at.
    Marked,
}

impl Unread {
    /// Whether the user has not looked at what this says yet (a seen failure is only listed).
    pub fn is_unseen(self) -> bool {
        self != Unread::FailedSeen
    }
}

/// The three groups of what wants the user, most urgent first. *Requests* is the only one that
/// blocks an agent; a finished turn alone is never a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attention {
    /// An approval, a question or a plan waits for an answer.
    Request,
    /// A turn or a session failed.
    Failed,
    /// Finished work the user has not seen yet (or marked unread).
    Review,
}

impl Attention {
    pub const ALL: [Attention; 3] = [Attention::Request, Attention::Failed, Attention::Review];

    /// The group's name on every surface.
    pub const fn label(self) -> &'static str {
        match self {
            Attention::Request => "Requests",
            Attention::Failed => "Failed",
            Attention::Review => "Ready for review",
        }
    }
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
    /// The group of attention the session is in, if any.
    pub attention: Option<Attention>,
}

impl Presentation {
    /// Whether a request (an approval, a question, a plan) waits: the only blocking attention.
    pub fn is_request(&self) -> bool {
        self.attention == Some(Attention::Request)
    }

    /// The state word of a row that is in a group of attention (`Approval`, `Question`,
    /// `Plan to approve`, `Failed`, `Ready for review`); `None` for every other state, which
    /// says its own label.
    pub fn state_word(&self) -> Option<&'static str> {
        self.attention?;
        Some(match self.label.as_str() {
            "approval" => "Approval",
            "question" => "Question",
            "plan" => "Plan to approve",
            "failed" => "Failed",
            "review" => "Ready for review",
            _ => return None,
        })
    }

    /// The state word remains the accessible meaning; artwork is independently replaceable.
    pub fn icon(&self) -> crate::icons::Icon {
        use crate::icons::Icon;
        match self.label.as_str() {
            "working" | "starting" | "running" => Icon::StateWorking,
            "blocked" => Icon::StateBlocked,
            "approval" => Icon::StateApproval,
            "question" => Icon::StateQuestion,
            "plan" => Icon::StatePlan,
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
/// model → macOS surfaces). Finished work is in the sidebar's *Ready for review* but not here:
/// a badge is for what blocks someone (*Requests*) or went wrong (*Failed*, not yet seen).
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
    match presentation.attention? {
        Attention::Request => Some(AttentionKind::Blocked),
        // A failure the user has looked at stays in the group, without a badge.
        Attention::Failed if presentation.filled => Some(AttentionKind::Failed),
        Attention::Failed | Attention::Review => None,
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

/// Presentation of `info` given its unread flag; a chat names its model as `names` know it. A
/// session the user *marked unread* is a quiet *Ready for review* item whatever it does (an
/// accent state, never a badge: [`classify`] leaves it alone) until it is read again.
pub fn present(info: &SessionInfo, unread: Option<Unread>, names: &ModelNames) -> Presentation {
    if unread != Some(Unread::Marked) {
        return present_state(info, unread, names);
    }
    let base = present_state(info, None, names);
    if base.attention.is_some() {
        return base;
    }
    Presentation {
        detail: if base.detail.is_empty() {
            "marked unread".to_owned()
        } else {
            format!("{} · marked unread", base.detail)
        },
        tone: Tone::Accent,
        filled: true,
        attention: Some(Attention::Review),
        ..base
    }
}

fn present_state(info: &SessionInfo, unread: Option<Unread>, names: &ModelNames) -> Presentation {
    let exit_detail = |code: Option<i32>, signal: Option<i32>| match (code, signal) {
        (_, Some(sig)) => format!("signal {sig}"),
        (Some(code), None) => format!("exit {code}"),
        (None, None) => String::new(),
    };
    let quiet = |label: &str, detail: String, tone: Tone, filled: bool| Presentation {
        label: label.into(),
        detail,
        tone,
        filled,
        attention: None,
    };
    match &info.state {
        SessionState::Starting => quiet("starting", String::new(), Tone::Accent, false),
        // Finished work of a running session the user has not seen: a terminal notification.
        // Never a request.
        SessionState::Running | SessionState::Idle if unread == Some(Unread::Done) => {
            Presentation {
                label: "review".into(),
                detail: "notification".into(),
                tone: Tone::Accent,
                filled: true,
                attention: Some(Attention::Review),
            }
        }
        // An agent chat: the daemon runs the agent, so its state is exact.
        SessionState::Running | SessionState::Idle if info.chat.is_some() => {
            let busy = info.state == SessionState::Running;
            quiet(
                if busy { "working" } else { "idle" },
                chat_detail(info, names),
                if busy { Tone::Accent } else { Tone::Muted },
                busy,
            )
        }
        SessionState::WaitingApproval if info.chat.is_some() => {
            let pending = info.chat.as_ref().map_or(0, |chat| chat.pending_approvals);
            let asked = info.chat.as_ref().map_or(0, |chat| chat.pending_questions);
            Presentation {
                // The word says what waits; the detail only counts when there is more than one.
                label: if pending == 0 && asked > 0 {
                    "question"
                } else {
                    "approval"
                }
                .into(),
                detail: match (pending, asked) {
                    (0 | 1, 0) | (0, 1) => String::new(),
                    (0, n) => format!("{n} questions"),
                    (n, 0) => format!("{n} approvals"),
                    (a, q) => format!("{a} approvals, {q} questions"),
                },
                tone: Tone::Warning,
                filled: true,
                attention: Some(Attention::Request),
            }
        }
        // A coding agent that reports (`agent.report`) says more than a bare "idle".
        SessionState::Running | SessionState::Idle => match &info.agent {
            Some(report) => present_agent(report),
            None => quiet("idle", String::new(), Tone::Muted, false),
        },
        SessionState::WaitingApproval => Presentation {
            label: "approval".into(),
            detail: String::new(),
            tone: Tone::Warning,
            filled: true,
            attention: Some(Attention::Request),
        },
        SessionState::Exited { code, signal } => {
            let detail = exit_detail(*code, *signal);
            match unread {
                Some(Unread::Done) => Presentation {
                    label: "review".into(),
                    detail,
                    tone: Tone::Accent,
                    filled: true,
                    attention: Some(Attention::Review),
                },
                Some(Unread::Failed | Unread::FailedSeen) => Presentation {
                    label: "failed".into(),
                    detail,
                    tone: Tone::Error,
                    filled: unread == Some(Unread::Failed),
                    attention: Some(Attention::Failed),
                },
                // A mark is applied over this in `present`, which never passes it down.
                None | Some(Unread::Marked | Unread::Turn) => quiet(
                    "exited",
                    detail,
                    if matches!((code, signal), (Some(0), None)) {
                        Tone::Muted
                    } else {
                        Tone::Error
                    },
                    false,
                ),
            }
        }
        // An assistant that could not start says so in the Assistant panel, which offers Retry
        // and another backend; it is not a session the user went away from.
        SessionState::Failed { reason } => {
            let looked_after = !matches!(unread, None | Some(Unread::Turn))
                && !info.chat.as_ref().is_some_and(|chat| chat.is_assistant());
            Presentation {
                label: "failed".into(),
                detail: reason.clone(),
                tone: Tone::Error,
                filled: looked_after && unread != Some(Unread::FailedSeen),
                attention: looked_after.then_some(Attention::Failed),
            }
        }
        SessionState::Stopped => quiet("stopped", String::new(), Tone::Muted, false),
    }
}

/// `codex · gpt-6.1-sol`: which agent and model a chat session runs (the model by name when
/// `names` know it).
fn chat_detail(info: &SessionInfo, names: &ModelNames) -> String {
    match &info.chat {
        Some(chat) => format!("{} · {}", chat.agent, names.label(&chat.agent, &chat.model)),
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

/// Remembers which sessions the user has not looked at since they ended or finished a turn.
#[derive(Debug, Default)]
pub struct AttentionTracker {
    unread: BTreeMap<SessionId, Unread>,
    /// Sessions the user snoozed, with the attention they had (`Self::snooze`).
    snoozed: BTreeMap<SessionId, String>,
    /// Whether each known session was live in the last list; `None` until the first synchronised
    /// list of the current daemon epoch has been taken as the baseline.
    baseline: Option<HashMap<SessionId, bool>>,
    /// Whether a chat turn ran in each known session in the last list (same baseline).
    turns: HashMap<SessionId, bool>,
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
    /// sessions that were live and now ended, and chat sessions whose turn ended, and clear the
    /// flag of sessions that restarted or started a new turn.
    pub fn observe(&mut self, epoch: Option<Epoch>, sessions: &[SessionInfo]) {
        if self.epoch != epoch {
            self.epoch = epoch;
            self.baseline = None;
            // What the daemon flagged belongs to its epoch; what the user marked does not.
            self.unread.retain(|_, kind| *kind == Unread::Marked);
        }
        let now: HashMap<SessionId, bool> =
            sessions.iter().map(|s| (s.id, s.state.is_live())).collect();
        let turns: HashMap<SessionId, bool> = sessions
            .iter()
            .map(|s| (s.id, s.chat.as_ref().is_some_and(|c| c.turn_active)))
            .collect();
        if let Some(before) = &self.baseline {
            for session in sessions {
                let was_live = before.get(&session.id).copied();
                if session.state.is_live() {
                    // A restart (ended before, live now) starts a new life; a live session
                    // keeps a notification until the user looks.
                    if was_live == Some(false) {
                        self.unread.remove(&session.id);
                    }
                    observe_turn(
                        &mut self.unread,
                        session,
                        self.turns.get(&session.id).copied(),
                        turns.get(&session.id).copied().unwrap_or(false),
                    );
                } else if was_live == Some(true)
                    && let Some(kind) = unread_for_end(&session.state)
                {
                    self.unread.insert(session.id, kind);
                }
            }
        }
        self.unread.retain(|id, _| now.contains_key(id));
        self.snoozed.retain(|id, _| now.contains_key(id));
        self.baseline = Some(now);
        self.turns = turns;
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

    /// The user looked at `session`: finished work and marks are seen; a failure stays listed
    /// in *Failed* (no longer a badge) until a new run starts or [`Self::mark_read`]. Whether
    /// anything changed.
    pub fn look_at(&mut self, session: SessionId) -> bool {
        match self.unread.get(&session).copied() {
            Some(Unread::Failed) => {
                self.unread.insert(session, Unread::FailedSeen);
                true
            }
            Some(Unread::FailedSeen) | None => false,
            Some(Unread::Done | Unread::Turn | Unread::Marked) => {
                self.unread.remove(&session);
                true
            }
        }
    }

    /// *Mark as read*: whatever is flagged about `session`, a failure too, is dismissed.
    pub fn mark_read(&mut self, session: SessionId) -> bool {
        self.unread.remove(&session).is_some()
    }

    /// *Mark as unread*: `session` needs a look again until it is looked at. What is already
    /// flagged (a finished session, a failure) stays as it is. False when nothing changed.
    pub fn mark_unread(&mut self, session: SessionId) -> bool {
        if self.unread.contains_key(&session) {
            return false;
        }
        self.unread.insert(session, Unread::Marked);
        true
    }

    /// Whether `session` is marked unread by the user.
    pub fn is_marked(&self, session: SessionId) -> bool {
        self.unread.get(&session) == Some(&Unread::Marked)
    }

    /// *Snooze until it changes*: `session` is left out of what needs the user while its
    /// attention stays what `key` says (see `SessionsStore::attention_key`).
    pub fn snooze(&mut self, session: SessionId, key: String) -> bool {
        self.snoozed.insert(session, key.clone()).as_deref() != Some(key.as_str())
    }

    /// Ends a snooze. False when there was none.
    pub fn unsnooze(&mut self, session: SessionId) -> bool {
        self.snoozed.remove(&session).is_some()
    }

    /// The attention `session` was snoozed at, while it is snoozed.
    pub fn snoozed_key(&self, session: SessionId) -> Option<&str> {
        self.snoozed.get(&session).map(String::as_str)
    }

    /// Every snooze, for comparing before and after a change.
    pub fn snoozed_map(&self) -> &BTreeMap<SessionId, String> {
        &self.snoozed
    }

    /// What the desktop keeps across restarts: the sessions marked unread and the snoozes.
    pub fn marks(&self) -> Marks {
        Marks {
            unread: self
                .unread
                .iter()
                .filter(|(_, kind)| **kind == Unread::Marked)
                .map(|(id, _)| id.to_string())
                .collect(),
            snoozed: self
                .snoozed
                .iter()
                .map(|(id, key)| (id.to_string(), key.clone()))
                .collect(),
        }
    }

    /// Takes the marks of an earlier run back (before the first list is observed; sessions that
    /// no longer exist are dropped by it).
    pub fn restore(&mut self, marks: &Marks) {
        for id in marks.unread.iter().filter_map(|id| id.parse().ok()) {
            self.unread.entry(id).or_insert(Unread::Marked);
        }
        for (id, key) in &marks.snoozed {
            if let Ok(id) = id.parse() {
                self.snoozed.insert(id, key.clone());
            }
        }
    }
}

/// A chat turn that ended while nobody was asked anything is an unseen mark on its row, no group
/// (a turn that changed files is *Ready for review* by the task's own rule); a new turn is the
/// work going on again. `was` is
/// whether a turn ran in the previous list, `running` whether one runs now.
fn observe_turn(
    unread: &mut BTreeMap<SessionId, Unread>,
    session: &SessionInfo,
    was: Option<bool>,
    running: bool,
) {
    let Some(chat) = &session.chat else {
        return;
    };
    if running && was == Some(false) {
        if unread.get(&session.id) == Some(&Unread::Turn) {
            unread.remove(&session.id);
        }
    } else if !running
        && was == Some(true)
        && !chat.is_assistant()
        && chat.pending_approvals == 0
        && chat.pending_questions == 0
    {
        unread.entry(session.id).or_insert(Unread::Turn);
    }
}
/// What the user did to the queue, kept in `desktop-state.json` (`attention`): client read
/// state the daemon does not know (`docs/spikes/2026-10-20-chrome-v3.md` § 2.7).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Marks {
    /// Session ids marked unread.
    pub unread: Vec<String>,
    /// Session id → the attention it was snoozed at.
    pub snoozed: BTreeMap<String, String>,
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        self.unread.is_empty() && self.snoozed.is_empty()
    }
}

/// Presentation of a live session whose agent has reported.
fn present_agent(report: &AgentReport) -> Presentation {
    let detail = match &report.message {
        Some(message) => format!("{}: {message}", report.agent),
        None => report.agent.clone(),
    };
    let (label, tone, filled, attention) = match report.state {
        AgentState::Idle => ("idle", Tone::Muted, false, None),
        AgentState::Working => ("working", Tone::Accent, true, None),
        // A subagent that waits while the main agent works (`blocked_sub`) is no request: the
        // main agent is still at work. The main agent's own wait is a permission request.
        AgentState::Blocked if report.blocked_sub.is_some() => {
            ("working", Tone::Accent, true, None)
        }
        AgentState::Blocked => ("approval", Tone::Warning, true, Some(Attention::Request)),
    };
    Presentation {
        label: label.into(),
        detail,
        tone,
        filled,
        attention,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::WorkspaceId;

    fn info(id: SessionId, state: SessionState) -> SessionInfo {
        SessionInfo {
            containment: None,
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
            muted: false,
        }
    }

    fn exited(code: i32) -> SessionState {
        SessionState::Exited {
            code: Some(code),
            signal: None,
        }
    }

    fn presentation_of(state: SessionState, unread: Option<Unread>) -> Presentation {
        present(
            &info(SessionId::new(), state),
            unread,
            &ModelNames::default(),
        )
    }

    #[test]
    fn only_blocked_approval_and_failed_sessions_count_for_the_macos_surfaces() {
        let failed = presentation_of(exited(2), Some(Unread::Failed));
        assert_eq!(classify(&failed, 0, 0), Some(AttentionKind::Failed));
        let done = presentation_of(exited(0), Some(Unread::Done));
        assert!(done.attention.is_some());
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
    fn an_assistant_that_failed_to_start_is_not_in_failed_but_a_failed_chat_is() {
        let failed = || SessionState::Failed {
            reason: "agent request `session/new` failed".into(),
        };
        let chat_failed = present(
            &chat(failed(), false, 0),
            Some(Unread::Failed),
            &ModelNames::default(),
        );
        assert!(chat_failed.attention.is_some());
        assert_eq!(classify(&chat_failed, 0, 0), Some(AttentionKind::Failed));

        let mut assistant = chat(failed(), false, 0);
        if let Some(c) = assistant.chat.as_mut() {
            c.assistant = true;
        }
        let shown = present(&assistant, Some(Unread::Failed), &ModelNames::default());
        assert_eq!(shown.label, "failed", "the row still says what happened");
        assert!(!shown.attention.is_some());
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
        let live = present(
            &info(a, SessionState::Running),
            tracker.unread(a),
            &ModelNames::default(),
        );
        assert_eq!(
            (live.label.as_str(), live.attention),
            ("review", Some(Attention::Review))
        );
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
        let unread = present(&done, Some(Unread::Done), &ModelNames::default());
        assert_eq!(
            (unread.label.as_str(), unread.attention),
            ("review", Some(Attention::Review))
        );
        assert_eq!(unread.state_word(), Some("Ready for review"));
        assert_eq!(unread.tone, Tone::Accent);
        let read = present(&done, None, &ModelNames::default());
        assert_eq!(
            (read.label.as_str(), read.attention.is_some()),
            ("exited", false)
        );
        let failed = present(
            &info(id, exited(3)),
            Some(Unread::Failed),
            &ModelNames::default(),
        );
        assert_eq!(
            (failed.label.as_str(), failed.detail.as_str()),
            ("failed", "exit 3")
        );
        assert_eq!(
            present(
                &info(id, SessionState::Running),
                None,
                &ModelNames::default()
            )
            .label,
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
            mode: None,
            checkpoints: None,
            account: None,
            safety: None,
        });
        session
    }

    #[test]
    fn a_chat_session_is_working_during_a_turn_and_blocked_on_an_approval() {
        let idle = present(
            &chat(SessionState::Idle, false, 0),
            None,
            &ModelNames::default(),
        );
        assert_eq!(
            (idle.label.as_str(), idle.attention.is_some()),
            ("idle", false)
        );
        assert_eq!(idle.detail, "codex · gpt-6.1-sol");
        let working = present(
            &chat(SessionState::Running, true, 0),
            None,
            &ModelNames::default(),
        );
        assert_eq!(
            (
                working.label.as_str(),
                working.tone,
                working.attention.is_some()
            ),
            ("working", Tone::Accent, false)
        );
        assert!(working.filled);
        let blocked = present(
            &chat(SessionState::WaitingApproval, true, 2),
            None,
            &ModelNames::default(),
        );
        assert_eq!(
            (
                blocked.label.as_str(),
                blocked.tone,
                blocked.attention.is_some()
            ),
            ("approval", Tone::Warning, true)
        );
        assert_eq!(blocked.detail, "2 approvals");
        assert_eq!(blocked.state_word(), Some("Approval"));
        let one = present(
            &chat(SessionState::WaitingApproval, true, 1),
            None,
            &ModelNames::default(),
        );
        assert_eq!(one.detail, "", "the word says what waits");
        let asked = {
            let mut session = chat(SessionState::WaitingApproval, true, 0);
            if let Some(c) = session.chat.as_mut() {
                c.pending_questions = 1;
            }
            present(&session, None, &ModelNames::default())
        };
        assert_eq!(
            (asked.label.as_str(), asked.state_word()),
            ("question", Some("Question"))
        );
    }

    #[test]
    fn a_reporting_agent_sets_the_label_and_a_blocked_one_is_a_request() {
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
            present(&session, None, &ModelNames::default())
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
        assert!(working.filled && working.attention.is_none());
        let blocked = report(AgentState::Blocked, Some("approve rm"));
        assert_eq!(
            (
                blocked.label.as_str(),
                blocked.detail.as_str(),
                blocked.tone
            ),
            ("approval", "omp: approve rm", Tone::Warning)
        );
        assert!(blocked.filled && blocked.is_request());
        let idle = report(AgentState::Idle, None);
        assert_eq!(
            (idle.label.as_str(), idle.detail.as_str(), idle.tone),
            ("idle", "omp", Tone::Muted)
        );
        assert!(!idle.filled && idle.attention.is_none());
    }

    #[test]
    fn a_session_marked_unread_stays_until_it_is_read_and_survives_a_new_epoch_and_a_restart() {
        let (a, gone) = (SessionId::new(), SessionId::new());
        let mut tracker = AttentionTracker::new();
        let epoch = Some(Epoch::new());
        tracker.observe(epoch, &[info(a, SessionState::Running)]);
        assert!(tracker.mark_unread(a));
        assert!(!tracker.mark_unread(a), "already marked");
        assert!(tracker.is_marked(a));

        // What is marked is not what the daemon flagged: a new epoch keeps it.
        tracker.observe(Some(Epoch::new()), &[info(a, SessionState::Running)]);
        assert!(tracker.is_marked(a));

        // It travels in the marks, and a restart (a fresh tracker) takes it back; a session
        // that no longer exists is dropped by the first list.
        let mut marks = tracker.marks();
        marks.unread.push(gone.to_string());
        let mut again = AttentionTracker::new();
        again.restore(&marks);
        again.observe(Some(Epoch::new()), &[info(a, SessionState::Running)]);
        assert!(again.is_marked(a));
        assert!(!again.is_marked(gone));

        // Reading ends it.
        assert!(again.mark_read(a));
        assert!(again.marks().is_empty());
    }

    #[test]
    fn marking_leaves_what_the_daemon_flagged_alone_and_shows_as_a_quiet_need() {
        let a = SessionId::new();
        let mut tracker = AttentionTracker::new();
        let epoch = Some(Epoch::new());
        tracker.observe(epoch, &[info(a, SessionState::Running)]);
        tracker.observe(epoch, &[info(a, exited(1))]);
        assert_eq!(tracker.unread(a), Some(Unread::Failed));
        assert!(!tracker.mark_unread(a), "a failure is stronger");

        let idle = info(SessionId::new(), SessionState::Idle);
        let marked = present(&idle, Some(Unread::Marked), &ModelNames::default());
        assert_eq!(marked.attention, Some(Attention::Review));
        assert!(marked.filled);
        assert_eq!(marked.tone, Tone::Accent, "no badge for a mark");
        assert!(marked.detail.ends_with("marked unread"));
        assert_eq!(classify(&marked, 0, 0), None);
        // A session that needs the user anyway keeps saying why.
        let blocked = present(
            &chat(SessionState::WaitingApproval, false, 1),
            Some(Unread::Marked),
            &ModelNames::default(),
        );
        assert_eq!(blocked.label, "approval");
    }

    #[test]
    fn a_failure_is_seen_on_view_but_stays_until_it_is_dismissed_or_a_new_run_starts() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let epoch = Some(Epoch::new());
        let mut tracker = AttentionTracker::new();
        tracker.observe(
            epoch,
            &[
                info(a, SessionState::Running),
                info(b, SessionState::Running),
            ],
        );
        tracker.observe(epoch, &[info(a, exited(2)), info(b, exited(0))]);
        assert_eq!(tracker.unread(a), Some(Unread::Failed));
        assert_eq!(tracker.unread(b), Some(Unread::Done));

        // Looking: finished work is seen and gone; the failure stays, seen.
        assert!(tracker.look_at(a));
        assert!(tracker.look_at(b));
        assert!(!tracker.look_at(a), "nothing new to see");
        assert_eq!(tracker.unread(a), Some(Unread::FailedSeen));
        assert_eq!(tracker.unread(b), None);
        let seen = present(
            &info(a, exited(2)),
            tracker.unread(a),
            &ModelNames::default(),
        );
        assert_eq!(seen.attention, Some(Attention::Failed), "still listed");
        assert!(!seen.filled, "no longer unseen");
        assert_eq!(classify(&seen, 0, 0), None, "and no longer a badge");

        // Dismissing ends it …
        assert!(tracker.mark_read(a));
        assert_eq!(tracker.unread(a), None);
        // … and so does a new run.
        tracker.observe(epoch, &[info(a, exited(2)), info(b, exited(0))]);
        tracker.observe(epoch, &[info(a, SessionState::Running), info(b, exited(0))]);
        tracker.observe(epoch, &[info(a, exited(4)), info(b, exited(0))]);
        assert_eq!(tracker.unread(a), Some(Unread::Failed));
        assert!(tracker.look_at(a));
        tracker.observe(epoch, &[info(a, SessionState::Running), info(b, exited(0))]);
        assert_eq!(tracker.unread(a), None, "a new run clears a seen failure");
    }

    #[test]
    fn a_chat_turn_that_ends_is_an_unseen_mark_never_a_group_and_a_new_turn_clears_it() {
        let epoch = Some(Epoch::new());
        let mut tracker = AttentionTracker::new();
        let mut session = chat(SessionState::Running, true, 0);
        let id = session.id;
        let with_turn = |session: &SessionInfo, active: bool| {
            let mut next = session.clone();
            if let Some(c) = next.chat.as_mut() {
                c.turn_active = active;
            }
            next
        };
        // The first list is the baseline: a turn that runs then is not news when it ends later.
        tracker.observe(epoch, std::slice::from_ref(&session));
        assert_eq!(tracker.unread(id), None);
        session = with_turn(&session, false);
        tracker.observe(epoch, std::slice::from_ref(&session));
        assert_eq!(
            tracker.unread(id),
            Some(Unread::Turn),
            "a finished turn is an unseen mark on its row"
        );
        assert!(tracker.unread(id).unwrap().is_unseen());
        let shown = present(&session, tracker.unread(id), &ModelNames::default());
        assert_eq!(
            shown.attention, None,
            "no group: whether it changed files is the task's rule"
        );
        assert_eq!(classify(&shown, 0, 0), None, "never a request or a badge");

        // Looking at it clears the mark.
        assert!(tracker.look_at(id));
        assert_eq!(tracker.unread(id), None);
        session = with_turn(&session, true);
        tracker.observe(epoch, std::slice::from_ref(&session));
        session = with_turn(&session, false);
        tracker.observe(epoch, std::slice::from_ref(&session));
        assert_eq!(tracker.unread(id), Some(Unread::Turn));

        // A new turn is the work going on again.
        session = with_turn(&session, true);
        tracker.observe(epoch, std::slice::from_ref(&session));
        assert_eq!(tracker.unread(id), None);

        // A turn that stops on a question is waiting for the user, not finished work.
        let mut asking = with_turn(&session, false);
        if let Some(c) = asking.chat.as_mut() {
            c.pending_questions = 1;
        }
        tracker.observe(epoch, std::slice::from_ref(&asking));
        assert_eq!(tracker.unread(id), None);

        // Mawas's own turns are conversation, not work to review.
        let mut mawas = chat(SessionState::Running, true, 0);
        if let Some(c) = mawas.chat.as_mut() {
            c.assistant = true;
        }
        tracker.observe(epoch, std::slice::from_ref(&mawas));
        let mut done = mawas.clone();
        if let Some(c) = done.chat.as_mut() {
            c.turn_active = false;
        }
        tracker.observe(epoch, std::slice::from_ref(&done));
        assert_eq!(tracker.unread(mawas.id), None);
    }

    #[test]
    fn a_snooze_is_remembered_with_its_attention_and_dropped_with_its_session() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let mut tracker = AttentionTracker::new();
        let epoch = Some(Epoch::new());
        tracker.observe(
            epoch,
            &[
                info(a, SessionState::Running),
                info(b, SessionState::Running),
            ],
        );
        assert!(tracker.snooze(a, "blocked|approval".into()));
        assert!(
            !tracker.snooze(a, "blocked|approval".into()),
            "the same snooze"
        );
        assert!(tracker.snooze(b, "x".into()));
        assert_eq!(tracker.snoozed_key(a), Some("blocked|approval"));

        tracker.observe(epoch, &[info(a, SessionState::Running)]);
        assert_eq!(tracker.snoozed_key(b), None, "its session is gone");

        let mut again = AttentionTracker::new();
        again.restore(&tracker.marks());
        assert_eq!(again.snoozed_key(a), Some("blocked|approval"));
        assert!(again.unsnooze(a));
        assert!(!again.unsnooze(a));
    }
}
