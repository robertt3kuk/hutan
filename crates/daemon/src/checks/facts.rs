//! What the detectors read: the recorded facts of a session, as plain data.
//!
//! Collecting them (the journal, the checkpoints, the audit tables, the state log, the insight
//! reducer) is `collect`'s job; the detectors are pure functions of these structs, so every rule
//! is tested with fixture events and no daemon.

use std::path::PathBuf;

use hutan_core::insight::AgentInsight;
use hutan_core::{SessionId, TurnId, WorkspaceId};
use hutan_protocol::review::ReviewCommand;
use hutan_storage::ChatTurnStatus;

use crate::signs::SeenFile;

/// Who a session is.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub harness: Option<String>,
    /// A chat has a journal; a terminal agent only what it reports.
    pub chat: bool,
    pub live: bool,
    pub created_ms: u64,
    /// The folders the session may change.
    pub roots: Vec<PathBuf>,
}

/// One changed file of a turn and, for a test file, what changed in it.
#[derive(Debug, Clone)]
pub struct TurnFile {
    pub seen: SeenFile,
    /// The unified patch of the file, read only for test files.
    pub patch: Option<String>,
}

/// Where the files of a turn come from.
#[derive(Debug, Clone)]
pub enum TurnFiles {
    /// The turn's checkpoint: exact files and lines.
    Checkpoint(Vec<TurnFile>),
    /// Only the files the agent's own file changes named; no line counts.
    Journal(Vec<TurnFile>),
    /// No checkpoint and no file change in the journal; why.
    Missing(String),
}

impl TurnFiles {
    pub fn files(&self) -> &[TurnFile] {
        match self {
            Self::Checkpoint(files) | Self::Journal(files) => files,
            Self::Missing(_) => &[],
        }
    }
}

/// A tool other than a command that failed, with the last line of what it said.
#[derive(Debug, Clone)]
pub struct ToolFailure {
    pub name: String,
    pub text: String,
}

/// One turn of a chat, reduced to what the detectors read.
#[derive(Debug, Clone)]
pub struct TurnFacts {
    pub turn_id: Option<TurnId>,
    /// From 1, in the order of the journal's turns.
    pub number: u32,
    pub status: ChatTurnStatus,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    pub error: Option<String>,
    pub final_message: Option<String>,
    /// Every command of the turn with its exit code when the harness gave one.
    pub commands: Vec<ReviewCommand>,
    pub tool_failures: Vec<ToolFailure>,
    pub files: TurnFiles,
}

impl TurnFacts {
    /// When the turn ended, or began while it still runs.
    pub fn at_ms(&self) -> u64 {
        self.ended_ms.unwrap_or(self.started_ms)
    }
}

/// Everything read from a chat's journal, checkpoints and insight: what changes only when the
/// journal does, so it can be kept by the journal's last sequence.
#[derive(Debug, Clone)]
pub struct JournalFacts {
    pub meta: SessionMeta,
    /// The newest turns analysed, oldest first.
    pub turns: Vec<TurnFacts>,
    /// How many turns the chat has.
    pub turns_total: usize,
    /// The insight of the chat, or why there is none.
    pub insight: Result<AgentInsight, String>,
}

/// A permission request or a question of an agent, as the audit tables hold it.
#[derive(Debug, Clone)]
pub struct RequestFact {
    pub id: String,
    /// `approval`, `question` or `plan`.
    pub kind: &'static str,
    pub title: String,
    pub asked_ms: u64,
    pub resolved_ms: Option<u64>,
}

/// A span in which a terminal agent said it was blocked.
#[derive(Debug, Clone)]
pub struct BlockedSpan {
    pub start_ms: u64,
    /// `None`: it has not left the state.
    pub end_ms: Option<u64>,
    pub kind: Option<String>,
}

/// A task of the session whose finished work waits for a reviewer.
#[derive(Debug, Clone)]
pub struct TaskFact {
    pub title: String,
    /// When the delivery moved to `awaiting_review`.
    pub awaiting_since_ms: u64,
}

/// Where a session works.
#[derive(Debug, Clone)]
pub struct PlacementFact {
    /// The checkout's top level.
    pub root: PathBuf,
    /// Other live sessions with an agent in this checkout.
    pub shared_with: Vec<SessionId>,
}

/// A file a session changed, by repository and path inside it, so that a worktree and the
/// main checkout of one repository name the same file the same way.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChangedFile {
    pub repo: String,
    pub path: String,
}

/// What changes with time or outside the journal: waits, the quiet since the last event, the
/// review of a task, where the session works. Read on every request.
#[derive(Debug, Clone)]
pub struct LiveFacts {
    pub meta: SessionMeta,
    pub now_ms: u64,
    /// The newest turn of the chat still runs.
    pub turn_running: bool,
    /// The newest event of the chat's journal.
    pub last_event_ms: Option<u64>,
    /// A terminal agent says it works, and when it said so last.
    pub working_since_ms: Option<u64>,
    /// An approval or a question waits for the user.
    pub pending_request: bool,
    pub requests: Vec<RequestFact>,
    pub blocked_spans: Vec<BlockedSpan>,
    /// The state log is on: a terminal agent's waits are recorded.
    pub state_log: bool,
    pub task: Option<TaskFact>,
    /// When the newest finished turn with file changes ended.
    pub changed_at_ms: Option<u64>,
    pub placement: Option<PlacementFact>,
    /// The files the session changed, when something reports them (a chat's insight).
    pub files: Option<Vec<ChangedFile>>,
    /// The newest run of Hutan's own tests of the session (`agents.verify`), if it ever ran one.
    pub verify: Option<VerifyFact>,
    /// When the agent last said it was done: the end of a chat's newest turn when it completed
    /// (and none runs now), or when a terminal agent last reported itself idle.
    pub done_since_ms: Option<u64>,
}

/// A run of Hutan's own tests, as `verify_runs` holds it.
#[derive(Debug, Clone)]
pub struct VerifyFact {
    pub run_id: u64,
    pub command: String,
    /// `running`, `passed`, `failed`, `timed_out`, `cancelled` or `error`.
    pub status: String,
    pub exit_code: Option<i32>,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// `user` or `mawas`.
    pub requested_by: String,
    /// The last line the run printed.
    pub last_line: Option<String>,
}
