//! Notification intents (protocol 0.13, capability `notify.intents`): DTOs of `notify.*`,
//! `client.focus` and the `notify.intent` / `ui.open` events (`docs/protocol.md` § Notification
//! intents).
//!
//! The daemon decides *that* something deserves the user's attention and says so with an intent;
//! whoever can actually show a notification (a Notifier, the desktop, `hutan notify watch`)
//! renders it and sends the user's choice back with `notify.act`.
//!
//! Protocol 0.34 (capability `notify.mute`): `notify.mute` silences one session, which
//! `SessionInfo.muted` then says; a session some client has focused raises no intent of any kind,
//! and focusing a session withdraws the intents it has pending.

use hutan_core::{SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// Longest [`NotificationIntent::body`], in characters.
pub const BODY_MAX_CHARS: usize = 240;
/// Longest [`NotificationIntent::title`], in characters.
pub const TITLE_MAX_CHARS: usize = 120;
/// The one action of every intent that is not an approval; `notify.act` with it sends `ui.open`.
pub const ACTION_OPEN: &str = "open";

/// Why the user is being told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    /// An agent asks for permission (`approval.required`). Actions are the approval's options.
    ApprovalRequired,
    /// A node of a session's agent tree became blocked.
    AgentBlocked,
    /// The main agent of a session finished a turn that took at least `notify.min_turn_secs`.
    AgentFinished,
    /// A session's process exited with a non-zero code.
    SessionExitedWithError,
    /// A chat turn finished while no client had the session focused.
    ChatTurnFinished,
    /// A task finished successfully (protocol 0.16). `session_id` is the session that ran it.
    TaskDone,
    /// A task failed (protocol 0.16): a non-zero exit, a failed turn, a lost session.
    TaskFailed,
    /// An agent asks the user a question (protocol 0.24, `question.required`). The one action
    /// is [`ACTION_OPEN`]: a question is answered in the chat, never from a notification.
    QuestionRequired,
    /// The daemon's digest (protocol 0.68, capability `notify.digest`): the user has been away
    /// from every Hutan window for hours and something needs them. One per away period, never
    /// one per agent; `session_id` is the Global Mawas chat, `body` the one summary line
    /// (`ActivitySummary::line`). The one action is [`ACTION_OPEN`], which publishes `ui.open`
    /// with [`UiView::MawasRecent`].
    Digest,
}

impl IntentKind {
    pub const ALL: [Self; 9] = [
        Self::ApprovalRequired,
        Self::AgentBlocked,
        Self::AgentFinished,
        Self::SessionExitedWithError,
        Self::ChatTurnFinished,
        Self::TaskDone,
        Self::TaskFailed,
        Self::QuestionRequired,
        Self::Digest,
    ];

    /// The wire (and `daemon.toml`) name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalRequired => "approval_required",
            Self::AgentBlocked => "agent_blocked",
            Self::AgentFinished => "agent_finished",
            Self::SessionExitedWithError => "session_exited_with_error",
            Self::ChatTurnFinished => "chat_turn_finished",
            Self::TaskDone => "task_done",
            Self::TaskFailed => "task_failed",
            Self::QuestionRequired => "question_required",
            Self::Digest => "digest",
        }
    }
}

/// One thing the user can do about an intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentAction {
    /// For an approval the `id` of one of its options (what `approval.respond` takes),
    /// otherwise [`ACTION_OPEN`].
    pub id: String,
    pub label: String,
}

/// A request for the user's attention. `title` and `body` are redacted (secrets masked) and
/// bounded; `body` has at most [`BODY_MAX_CHARS`] characters.
///
/// An intent stays pending until it is acknowledged (`notify.ack`, or `notify.act`), its
/// approval is resolved, or it expires; `expires_at_ms` is absent for an approval, which
/// lives exactly as long as the approval does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationIntent {
    /// Opaque, unique within the daemon's lifetime.
    pub id: String,
    pub kind: IntentKind,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    pub title: String,
    pub body: String,
    pub actions: Vec<IntentAction>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
}

/// Result of `notify.list`: the pending intents, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyList {
    pub intents: Vec<NotificationIntent>,
}

/// `notify.ack`: the user has seen the intent; it is no longer pending. Answered with `Empty`;
/// `not_found` for an unknown, already acknowledged or expired intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyAck {
    pub id: String,
}

/// `notify.act`: the user chose `action_id` of the intent. An approval's action is routed to
/// `approval.respond` with it as `option_id`; [`ACTION_OPEN`] makes the daemon publish a
/// `ui.open` event for the clients. The intent is acknowledged by it. Answered with
/// [`NotifyActed`].
///
/// Errors: `not_found` (unknown, acknowledged or expired intent), `option_not_offered` (the
/// intent does not offer `action_id`), and whatever `approval.respond` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyAct {
    pub id: String,
    pub action_id: String,
}

/// Where a chosen action went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActRoute {
    /// Answered the approval.
    Approval,
    /// Published `ui.open`.
    UiOpen,
}

/// Result of `notify.act`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyActed {
    pub route: ActRoute,
    /// For [`ActRoute::UiOpen`]: how many subscribed desktop clients the event reached; `0`
    /// means nobody can show the session, and the caller may start the desktop.
    pub desktop_clients: u32,
}

/// `client.focus`: the session this connection's user is looking at, or none. Per connection:
/// replaced by the next report and dropped when the connection closes. A chat turn that
/// finishes in a session some client has focused raises no `chat_turn_finished` intent.
/// Answered with `Empty`. Unknown ids are accepted; they match nothing.
///
/// Protocol 0.34 (capability `notify.mute`): a session some client has focused raises no intent
/// of any kind (its card is on screen), and reporting a session withdraws every intent it has
/// pending (the user is looking at it), approvals and questions included.
///
/// Protocol 0.68 (capability `notify.digest`): `window_active` says whether the window of this
/// connection is the active one. A desktop sends it with every change of its window's activation
/// (also while the session it shows does not change); the daemon's digest counts a window the
/// user used within the last N hours as *the user is here*. Absent (an older client): a
/// connection that reports a session is taken as active, one that reports none as not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClientFocus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_active: Option<bool>,
}

/// What a `ui.open` also asks the window to show (protocol 0.68, capability `notify.digest`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiView {
    /// Mawas Home with the rail on its Recent tab (a digest notification's *Open*).
    MawasRecent,
}

/// `notify.mute` (protocol 0.34, capability `notify.mute`): `muted: true` silences the session:
/// it raises no intent of any kind (so no banner and no sound) and its pending intents are
/// withdrawn; `false` lets it notify again. What the session needs from the user is unchanged:
/// approvals still wait, and the desktop's attention list and Dock count still show it. The
/// choice survives a daemon restart and is reported as `SessionInfo.muted`. Answered with
/// `SessionResult`; `not_found` for an unknown session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyMute {
    pub session_id: SessionId,
    pub muted: bool,
}

/// `digest.status` (protocol 0.69, capability `digest.model`, MAWAS-DIGEST-B): where the
/// scheduled model digest stands, for Settings → Mawas → Catching up. Read-only; the schedule
/// itself is the daemon setting `notify.digest_model`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestStatus {
    /// `notify.digest_model` as the daemon reads it: `off`, `daily 08:30` or `every 6h`.
    pub schedule: String,
    /// The harness the digest would run on: the one of Global Mawas (`omp`, `claude`, `codex`,
    /// `provider`), absent when none is set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Whether a run could start now; `false` comes with `reason`.
    pub runnable: bool,
    /// Why not, in words for the user (a harness that is not proven offline, OMP missing, an
    /// OMP version the recipe was not verified on), or what the last check said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The OMP version the recipe was verified on.
    pub verified_omp: String,
    /// The OMP version installed, when the daemon could ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omp_version: Option<String>,
    /// When the next run is due (ms since the epoch); absent while the schedule is `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_ms: Option<u64>,
    /// The newest run, whatever became of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<DigestRunInfo>,
    /// How many runs the daemon remembers (the newest 50).
    pub runs: u32,
    /// The average tokens of the runs that answered and reported usage: what the next one will
    /// cost, about. Absent before the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_tokens: Option<u64>,
}

/// One run of the scheduled model digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestRunInfo {
    pub started_ms: u64,
    pub ended_ms: u64,
    /// `done`, `skipped` (the check refused the launch), `failed` or `timeout`.
    pub outcome: String,
    /// What a run that was not `done` says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The digest chat the answer (or the reason) is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Tokens of the turn, when the harness reported them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}
