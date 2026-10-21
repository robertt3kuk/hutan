//! Typed rows. Free-form payloads (`spec`, `resume_ref`) are opaque JSON strings so this crate
//! stays independent of the protocol and of agent integrations.

use std::path::PathBuf;

use hutan_core::{
    ApprovalId, Epoch, QuestionId, SessionId, SessionKind, SessionState, WorkspaceId,
};
use serde::{Deserialize, Serialize};

/// What a workspace row is. The database holds at most one `Home`; a `Project` is a repository
/// or folder, a `Workspace` the user's container of projects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Home,
    Project,
    Workspace,
}

/// Who created a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreatedBy {
    User,
    Cli,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub id: WorkspaceId,
    pub name: String,
    pub roots: Vec<PathBuf>,
    pub kind: WorkspaceKind,
    pub pinned: bool,
    pub created_by: CreatedBy,
    pub created_at_ms: u64,
    /// Latest session activity.
    pub last_active_ms: u64,
    /// Sort key for listing; unique ordering is the caller's business.
    pub position: i64,
    /// The git common directory of a project's repository; `None` for a plain folder and for
    /// rows that are no project.
    pub common_dir: Option<PathBuf>,
    /// The workspace a project is in.
    pub parent_id: Option<WorkspaceId>,
}

/// A project remembered after its workspace was closed (`recent_projects`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentProject {
    /// The project's primary root.
    pub path: PathBuf,
    pub name: String,
    pub last_opened_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub kind: SessionKind,
    pub title: String,
    /// The user named the session; terminal titles no longer rename it.
    pub title_locked: bool,
    /// What a restart spawns again (JSON written by the daemon).
    pub spec_json: String,
    pub cwd: Option<PathBuf>,
    /// The state at the last write. A live state here after the daemon stopped means the
    /// session was alive then.
    pub state: SessionState,
    pub created_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    /// Native resume reference of an agent session (JSON), used instead of `spec_json` on restore.
    pub resume_ref_json: Option<String>,
}

/// A session `session.close` removed from the lists: its row, kept for `session.reopen`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClosedSession {
    pub session: SessionRow,
    pub closed_at_ms: u64,
}

/// One daemon run recorded in `app_meta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochRecord {
    pub epoch: Epoch,
    pub started_at_ms: u64,
    /// How the run ended, as far as the run itself could say.
    #[serde(default)]
    pub end: EpochEnd,
}

/// How a daemon run ended. A run notes its clean stop itself, so a run that is over but never
/// did was killed or crashed; runs of builds that did not note it cannot be told either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EpochEnd {
    /// Recorded by a build that did not track how runs end.
    #[default]
    Untracked,
    /// Started by a build that tracks it; no stop was recorded (yet).
    Open,
    /// The daemon shut itself down at `at_ms`: its sessions were hung up on purpose.
    Clean { at_ms: u64 },
}

/// One journaled event of a chat session. The event itself is JSON (the core `AgentEvent`) so
/// this crate stays independent of the event model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatItemRow {
    pub session_id: SessionId,
    pub seq: u64,
    pub ts_ms: u64,
    pub turn_id: Option<String>,
    pub item_id: Option<String>,
    /// The event's `type`.
    pub kind: String,
    pub event_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatTurnStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTurnRow {
    pub session_id: SessionId,
    pub turn_id: String,
    pub provider_turn_id: Option<String>,
    pub status: ChatTurnStatus,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub error: Option<String>,
}

/// One message of a chat session's queue (`chat_queue`), in its place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedRow {
    pub session_id: SessionId,
    pub item_id: u64,
    pub text: String,
    pub queued_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Selected,
    Expired,
}

/// A permission request and, once it is over, how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRow {
    pub id: ApprovalId,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub kind: String,
    pub title: String,
    /// The options the provider offered (JSON array).
    pub options_json: String,
    pub requested_at_ms: u64,
    pub resolved_at_ms: Option<u64>,
    pub outcome: Option<ApprovalOutcome>,
    pub option_id: Option<String>,
    /// Which client answered (`cli`, `desktop`, ...); absent for expiries.
    pub decided_by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionOutcome {
    Answered,
    Declined,
    Cancelled,
    Expired,
}

/// A question of an agent and, once it is over, how it ended. Never the answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionRow {
    pub id: QuestionId,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub title: String,
    pub requested_at_ms: u64,
    pub resolved_at_ms: Option<u64>,
    pub outcome: Option<QuestionOutcome>,
    /// Which client decided (`cli`, `desktop`, ...); absent for expiries.
    pub decided_by: Option<String>,
}

/// A model provider's configuration. The credential is not here: it is in the Keychain, and
/// `keychain_ref` only names where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRow {
    /// A short slug, unique; also the Keychain account.
    pub id: String,
    /// `openai`, `anthropic`, `ollama` or `openai-compatible`.
    pub kind: String,
    pub name: String,
    pub base_url: String,
    pub default_model: Option<String>,
    pub keychain_ref: String,
    pub created_at_ms: u64,
}

/// A git worktree Hutan created (`worktree.create`). git itself is the source of truth for
/// which worktrees exist; this only remembers which of them are Hutan's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRow {
    /// The worktree's directory (canonical), unique.
    pub path: PathBuf,
    /// The workspace that asked for it.
    pub workspace_id: WorkspaceId,
    /// The repository's main worktree.
    pub repo_root: PathBuf,
    pub branch: String,
    /// The ref the branch was started from.
    pub base: String,
    pub created_at_ms: u64,
}

/// What the user decided about a folder's language servers (`lsp_trust`, protocol 0.39).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspTrustRow {
    /// Canonical, absolute.
    pub folder: PathBuf,
    /// `true`: trusted; `false`: denied.
    pub trusted: bool,
    pub decided_at_ms: u64,
    /// The client that decided (`desktop`, `cli`, ...).
    pub decided_by: String,
}

/// One grant of the control gate (`control_grants`, protocol 0.31): `tool` may run without an
/// approval for Mawas chats of `workspace_id`, or for the chat `chat_session_id` alone (exactly
/// one of the two is set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRow {
    pub id: u64,
    pub tool: String,
    pub workspace_id: Option<WorkspaceId>,
    pub chat_session_id: Option<SessionId>,
    pub created_ms: u64,
    /// The client that answered the approval (`desktop`, `cli`, ...).
    pub created_by: String,
}
