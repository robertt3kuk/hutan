//! User hooks (protocol 0.49, capability `hooks`): the DTOs of `hooks.list`, `hooks.log` and
//! `hooks.test`, and the payload a hook receives (`docs/integrations.md` § User hooks).
//!
//! A hook is a `[[hooks]]` entry of the user's `daemon.toml`: a command the daemon runs when a
//! Hutan event happens. Hooks are the user's: nothing here can add or change one (only the file
//! can), and an agent-originated connection may not even read them, because a command line can
//! carry a token.

use hutan_core::{SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// The version of [`HookPayload`]; it grows only when a field changes meaning or goes away.
pub const PAYLOAD_SCHEMA: u32 = 1;
/// Longest [`HookPayload::session_title`], in characters.
pub const TITLE_MAX_CHARS: usize = 120;
/// Longest [`HookPayload::detail`], in characters.
pub const DETAIL_MAX_CHARS: usize = 240;
/// Longest captured output (stdout and stderr each) of a [`HookRun`], in bytes.
pub const OUTPUT_MAX_BYTES: usize = 4096;
/// Hooks that run at the same time, over all hooks.
pub const MAX_CONCURRENT: u32 = 4;
/// Runs waiting for each hook; a further event is dropped and counted in [`HookInfo::dropped`].
pub const QUEUE_CAPACITY: u32 = 8;
/// Runs the log keeps; the oldest goes first.
pub const LOG_CAPACITY: u32 = 200;
/// The default `timeout` of a hook, in seconds.
pub const DEFAULT_TIMEOUT_SECS: u32 = 30;
/// The largest `timeout` of a hook, in seconds.
pub const MAX_TIMEOUT_SECS: u32 = 300;

/// What a hook can be run on (`on = "…"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    /// An agent's turn ended normally: a chat turn, or the main agent of a terminal session
    /// going idle after a turn.
    #[serde(rename = "turn.finished")]
    TurnFinished,
    /// An agent's turn ended with an error.
    #[serde(rename = "turn.failed")]
    TurnFailed,
    /// An agent asks for a permission.
    #[serde(rename = "approval.requested")]
    ApprovalRequested,
    /// A session started or stopped needing the user (a blocked agent, a pending approval or
    /// question).
    #[serde(rename = "attention.changed")]
    AttentionChanged,
    /// A session's process ended.
    #[serde(rename = "session.exited")]
    SessionExited,
    /// A task finished successfully.
    #[serde(rename = "task.done")]
    TaskDone,
    /// The user's Finish of a checkout ended (commit, merge, push).
    #[serde(rename = "finish.done")]
    FinishDone,
}

impl HookEvent {
    pub const ALL: [Self; 7] = [
        Self::TurnFinished,
        Self::TurnFailed,
        Self::ApprovalRequested,
        Self::AttentionChanged,
        Self::SessionExited,
        Self::TaskDone,
        Self::FinishDone,
    ];

    /// The wire (and `daemon.toml`) name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TurnFinished => "turn.finished",
            Self::TurnFailed => "turn.failed",
            Self::ApprovalRequested => "approval.requested",
            Self::AttentionChanged => "attention.changed",
            Self::SessionExited => "session.exited",
            Self::TaskDone => "task.done",
            Self::FinishDone => "finish.done",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|event| event.as_str() == name)
    }
}

/// What a hook gets on its standard input as one JSON object, and as `HUTAN_EVENT_*`
/// environment variables (`HUTAN_EVENT` is [`event`](Self::event), every other scalar field
/// `HUTAN_EVENT_<FIELD>` in upper case).
///
/// Only identifiers, names, titles, states and counts: never transcript text, never a file
/// path, never a secret. Text is redacted, a single line and bounded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookPayload {
    /// [`PAYLOAD_SCHEMA`].
    pub schema: u32,
    pub event: HookEvent,
    /// When the daemon saw the event.
    pub time_ms: u64,
    /// `hooks.test` made this payload up; nothing happened.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// At most [`TITLE_MAX_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    /// The agent: `codex`, `omp`, `claude`, `direct`, … Absent for a plain shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// The project the session belongs to (a folder of the user's, usually a repository).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The workspace (a group of projects) the project is in, when it is in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// A word for what happened, per event: `completed` / `failed` (turns), `exited` /
    /// `signaled` (`session.exited`), `attention` / `clear` (`attention.changed`), `done`
    /// (`task.done`), `ok` / `failed` (`finish.done`), `pending` (`approval.requested`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// A short line: the approval's title, the task's outcome, the checkout's name. At most
    /// [`DETAIL_MAX_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// How long the turn or task took, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Files the turn changed (chats) or the Finish committed; absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_files: Option<u32>,
    /// `session.exited`: the exit code, absent when a signal ended the process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// `attention.changed`: whether the session needs the user now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_attention: Option<bool>,
    /// `approval.requested`: the approval, for `hutan approve`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    /// `task.done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_title: Option<String>,
    /// `finish.done`: the branch committed on, and the one it was merged into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_branch: Option<String>,
    /// `finish.done`: how each step went (`ok`, `skipped`, `failed`); absent steps did not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_step: Option<String>,
}

impl HookPayload {
    /// A payload with only what every event has.
    pub fn new(event: HookEvent, time_ms: u64) -> Self {
        Self {
            schema: PAYLOAD_SCHEMA,
            event,
            time_ms,
            test: false,
            session_id: None,
            session_title: None,
            harness: None,
            project_id: None,
            project: None,
            workspace_id: None,
            workspace: None,
            state: None,
            detail: None,
            duration_ms: None,
            changed_files: None,
            exit_code: None,
            needs_attention: None,
            approval_id: None,
            task_id: None,
            task_title: None,
            branch: None,
            target_branch: None,
            commit_step: None,
            merge_step: None,
            push_step: None,
        }
    }
}

/// How a run of a hook ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookOutcome {
    /// Exit code 0.
    Ok,
    /// A non-zero exit code, or a signal that was not ours.
    Failed,
    /// It ran past its `timeout` and was killed.
    TimedOut,
    /// It never started (no such program, no permission).
    SpawnFailed,
}

/// One run of a hook, as the log keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRun {
    /// Counts up from 1 while the daemon runs.
    pub id: u64,
    /// The hook: its position in `[[hooks]]`, from 0.
    pub hook: u32,
    pub event: HookEvent,
    /// Started by `hooks.test` with a sample payload.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub outcome: HookOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The signal that ended it, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// What it wrote to standard output: redacted, at most [`OUTPUT_MAX_BYTES`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    /// Output beyond [`OUTPUT_MAX_BYTES`] was dropped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub output_truncated: bool,
    /// Why it did not start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A configured hook and what became of it since the daemon started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookInfo {
    /// Its position in `[[hooks]]`, from 0.
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub on: HookEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The command: the program and its arguments, or with `shell` one command line.
    pub run: Vec<String>,
    pub shell: bool,
    pub timeout_secs: u32,
    /// `false`: skipped while a desktop window is focused.
    pub when_focused: bool,
    /// Runs finished since the daemon started (tests included).
    pub runs: u64,
    /// Of those, the ones that did not end `ok`.
    pub failures: u64,
    /// Events dropped because the hook's queue was full.
    pub dropped: u64,
    /// Events skipped because a window was focused (`when_focused = false`).
    pub skipped: u64,
    /// The latest finished run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<HookRun>,
}

/// Result of `hooks.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HooksList {
    pub hooks: Vec<HookInfo>,
    /// Hooks running right now.
    pub running: u32,
    /// [`MAX_CONCURRENT`].
    pub max_concurrent: u32,
    /// [`QUEUE_CAPACITY`].
    pub queue_capacity: u32,
}

/// `hooks.log`: the finished runs, newest first. Answered with [`HooksLog`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HooksLogRequest {
    /// Only this hook's runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook: Option<u32>,
    /// At most this many (default 50, at most [`LOG_CAPACITY`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Result of `hooks.log`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HooksLog {
    pub runs: Vec<HookRun>,
    /// [`LOG_CAPACITY`].
    pub capacity: u32,
}

/// `hooks.test`: runs the hook once with a made-up payload of its event (marked `test`), ignoring
/// its filters and `when_focused`, and waits for it. Answered with [`HooksTested`]; `not_found`
/// for a position that is not configured. User-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HooksTest {
    pub hook: u32,
}

/// Result of `hooks.test`: the run (it is in the log too) and the payload it was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HooksTested {
    pub run: HookRun,
    pub payload: HookPayload,
}
