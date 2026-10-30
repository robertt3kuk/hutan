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

impl ActivitySummary {
    /// What the user was away for, as one line, what needs them first: `2 requests wait for you
    /// · 1 turn failed · 3 turns finished · 4 files changed`. `None` when nothing that matters
    /// happened (a plain shell starting or ending is not news). The Catch-up chip of Mawas Home
    /// and the daemon's digest notification say exactly this, so they cannot disagree.
    pub fn line(&self) -> Option<String> {
        fn count(n: usize, one: &str, many: &str) -> String {
            format!("{n} {}", if n == 1 { one } else { many })
        }
        let mut parts = Vec::new();
        if self.requests_open > 0 {
            parts.push(count(
                self.requests_open,
                "request waits for you",
                "requests wait for you",
            ));
        }
        if self.turns_failed > 0 {
            parts.push(count(self.turns_failed, "turn failed", "turns failed"));
        }
        if self.tasks_failed > 0 {
            parts.push(count(self.tasks_failed, "task failed", "tasks failed"));
        }
        if self.tasks_review > 0 {
            parts.push(count(
                self.tasks_review,
                "task sent to review",
                "tasks sent to review",
            ));
        }
        if self.turns_finished > 0 {
            parts.push(count(
                self.turns_finished,
                "turn finished",
                "turns finished",
            ));
        }
        if self.tasks_done > 0 {
            parts.push(count(self.tasks_done, "task done", "tasks done"));
        }
        if let Some(files) = self.files_changed.filter(|files| *files > 0) {
            parts.push(count(files as usize, "file changed", "files changed"));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// Whether something in the window needs the user: a request that still waits, a turn or a
    /// task that failed, work sent to review. What a digest notification is for; finished work
    /// alone is not worth a ping.
    pub fn needs_you(&self) -> bool {
        self.requests_open > 0
            || self.turns_failed > 0
            || self.tasks_failed > 0
            || self.tasks_review > 0
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_says_what_needs_the_user_first_and_nothing_when_nothing_happened() {
        assert_eq!(ActivitySummary::default().line(), None);
        // A plain shell starting and ending is not news.
        let quiet = ActivitySummary {
            sessions_started: 3,
            sessions_ended: 2,
            ..ActivitySummary::default()
        };
        assert_eq!((quiet.line(), quiet.needs_you()), (None, false));
        let busy = ActivitySummary {
            turns_finished: 3,
            turns_failed: 1,
            requests_asked: 4,
            requests_open: 2,
            tasks_review: 1,
            files_changed: Some(4),
            ..ActivitySummary::default()
        };
        assert_eq!(
            busy.line().as_deref(),
            Some(
                "2 requests wait for you · 1 turn failed · 1 task sent to review · \
                 3 turns finished · 4 files changed"
            )
        );
        assert!(busy.needs_you());
    }

    #[test]
    fn finished_work_alone_is_a_line_but_not_a_reason_to_ping() {
        let done = ActivitySummary {
            turns_finished: 1,
            tasks_done: 1,
            files_changed: Some(1),
            ..ActivitySummary::default()
        };
        assert_eq!(
            done.line().as_deref(),
            Some("1 turn finished · 1 task done · 1 file changed")
        );
        assert!(!done.needs_you());
        // A count nobody recorded is absent, not zero.
        let none = ActivitySummary {
            files_changed: Some(0),
            ..ActivitySummary::default()
        };
        assert_eq!(none.line(), None);
    }
}
