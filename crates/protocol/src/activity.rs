//! What happened across agents while the user was away (protocol 0.65, capability
//! `agents.activity`, ADR 0025): DTOs of `agents.activity`, one bounded, read-only, redacted
//! catch-up built from what the daemon already records (`docs/protocol.md` § Agents activity).
//!
//! It reads the turn table, the approval and question audit tables, the checkpoints, the task
//! store and the session rows. It says what it does not have ([`AgentsActivity::not_recorded`]):
//! a terminal agent has no turns and no history of states, and a number nobody recorded is
//! absent, never zero. "Since when" is the caller's: the daemon keeps no user presence, and a
//! window remembers when it last looked.

use hutan_core::{SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// The most events one answer lists.
pub const ACTIVITY_MAX_EVENTS: usize = 200;
/// How many events an answer lists when the request names no `limit`.
pub const ACTIVITY_DEFAULT_EVENTS: u32 = 100;
/// The most bytes of JSON one answer takes: events are listed (newest first) until the next
/// would not fit.
pub const ACTIVITY_MAX_BYTES: usize = 24 * 1024;
/// The window when the request names no `since_ms`: the last day.
pub const ACTIVITY_DEFAULT_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
/// The most characters of any text of the answer; longer text ends in `…`.
pub const ACTIVITY_TEXT_CHARS: usize = 200;

/// `agents.activity`: what happened in a window. Read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentsActivityRequest {
    /// The start of the window, ms since the epoch. Absent: the last 24 hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// The end of the window. Absent: now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<u64>,
    /// Only this workspace, with the projects in it; a project; or Home. `not_found` for an
    /// unknown id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// Only this session. `not_found` for an unknown id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// The most events to list, 1 to [`ACTIVITY_MAX_EVENTS`]. Absent: [`ACTIVITY_DEFAULT_EVENTS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// The catch-up. `summary` covers **every** event of the window; `events` lists the newest ones
/// that fit (see `omitted`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsActivity {
    pub from_ms: u64,
    pub to_ms: u64,
    /// The oldest time the daemon's records reach back to (its oldest session): a window that
    /// begins before it is partly not recorded. Absent while there is nothing recorded at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_from_ms: Option<u64>,
    pub summary: ActivitySummary,
    /// Newest first.
    pub events: Vec<ActivityEvent>,
    /// Events of the window that were not listed (the `limit`, [`ACTIVITY_MAX_EVENTS`] or
    /// [`ACTIVITY_MAX_BYTES`]): the oldest ones.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: usize,
    /// What this answer cannot say, in words (always the same for the same daemon).
    pub not_recorded: Vec<String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Counts over the whole window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ActivitySummary {
    /// Chat turns that ended in the window, by outcome.
    pub turns_finished: usize,
    pub turns_failed: usize,
    pub turns_cancelled: usize,
    /// Chat turns that started in the window and had not ended by its end.
    pub turns_running: usize,
    /// Approvals and questions asked in the window, and those answered (or ended) in it.
    pub requests_asked: usize,
    pub requests_resolved: usize,
    /// Asked in the window and still waiting.
    pub requests_open: usize,
    /// How long the requests resolved in the window waited: the sum and the longest. Absent
    /// when none was resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_total_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_longest_ms: Option<u64>,
    /// Checkpoints kept in the window and what they changed. Absent when there is none.
    pub checkpoints: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<u64>,
    /// Tasks that ended in the window, by state, and tasks whose work was sent to review.
    pub tasks_done: usize,
    pub tasks_failed: usize,
    pub tasks_review: usize,
    pub sessions_started: usize,
    pub sessions_ended: usize,
}

/// One thing that happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub at_ms: u64,
    /// The session it happened in; absent for a task that ran in none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// The session's title, or the task's.
    pub title: String,
    /// The project the session or task is in.
    pub project: String,
    #[serde(flatten)]
    pub detail: ActivityDetail,
}

/// What kind of thing, with what is recorded of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActivityDetail {
    /// A chat turn ended.
    TurnEnded {
        outcome: TurnOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// The error of a failed turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// An approval or a question was asked, and how it ended when it did. The event is at the
    /// time it was resolved, or at the time it was asked while it waits.
    Request {
        request: RequestKind,
        /// What was asked.
        text: String,
        asked_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_ms: Option<u64>,
        /// `selected`, `expired` for an approval; `answered`, `declined`, `cancelled`,
        /// `expired` for a question. Never the answer itself.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
        /// Who decided (`desktop`, `cli`, `grant:7`, `policy`, …); absent for an expiry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decided_by: Option<String>,
    },
    /// A task ended (`done`, `failed`, `cancelled`) or its work moved in review (`delivery`:
    /// `awaiting_review`, `accepted`, `changes_requested`, `merged`, …).
    Task {
        state: String,
        /// `true`: `state` is the task's own state; `false`: its delivery state.
        run: bool,
    },
    /// A session was created.
    SessionStarted { chat: bool },
    /// A session's process ended; `state` says how (`exited 0`, `exited 2`, `stopped`, …).
    SessionEnded { state: String },
    /// A checkpoint of a chat turn: what the turn changed.
    Checkpoint {
        files: u64,
        added: u64,
        removed: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Approval,
    Question,
}
