//! Flow statistics across agents (protocol 0.66, capability `agents.stats`, ADR 0025): DTOs of
//! `agents.stats`, one bounded, read-only, redacted answer to "where does the time go, what do
//! agents wait on, what fails again and again" (`docs/protocol.md` § Agents stats).
//!
//! It only says what the daemon recorded, and every number says how well: chats are journaled
//! (exact), tasks have a start and an end (exact), a terminal agent's waits exist only in the
//! state log (`agent_state_log`, 14 days, and only while it is on), retries are inferred, the
//! clustering of failures is a heuristic, and cost is an estimate that is absent where nobody
//! reported one. A number nobody recorded is absent, never zero.

use hutan_core::{SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// The most groups one answer lists.
pub const STATS_MAX_GROUPS: usize = 50;
/// The groups listed when the request names no `limit`.
pub const STATS_DEFAULT_GROUPS: u32 = 20;
/// The most failure clusters one answer lists.
pub const STATS_MAX_FAILURES: usize = 10;
/// The most stuck flags one answer lists.
pub const STATS_MAX_FLAGS: usize = 20;
/// The most bytes of JSON one answer takes: groups are listed until the next would not fit.
pub const STATS_MAX_BYTES: usize = 24 * 1024;
/// The window when the request names no `since_ms`: the last 7 days.
pub const STATS_DEFAULT_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// The most characters of any text of the answer; longer text ends in `…`.
pub const STATS_TEXT_CHARS: usize = 200;

/// What the groups of an answer are made by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatsBy {
    /// The project (or Home) the session is in.
    #[default]
    Project,
    /// The harness: `omp`, `claude`, `codex`, `direct`; `unknown` for a session the daemon no
    /// longer lists.
    Harness,
    /// The calendar day (UTC) a turn ended, a wait began, a task ended.
    Day,
    Session,
    /// The task whose run the session was (`no task` for the rest).
    Task,
}

/// `agents.stats`: the statistics of a window. Read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentsStatsRequest {
    /// The start of the window, ms since the epoch. Absent: the last 7 days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// The end of the window. Absent: now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<u64>,
    #[serde(default)]
    pub group_by: StatsBy,
    /// Only this workspace, with its projects; a project; or Home. `not_found` when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// The most groups to list, 1 to [`STATS_MAX_GROUPS`]. Absent: [`STATS_DEFAULT_GROUPS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentsStats {
    pub from_ms: u64,
    pub to_ms: u64,
    pub group_by: StatsBy,
    /// What the whole answer rests on.
    pub coverage: StatsCoverage,
    /// Everything in scope, as one group (`key` is `all`).
    pub total: StatsGroup,
    /// The groups, the heaviest first (a day: the newest first).
    pub groups: Vec<StatsGroup>,
    /// Groups that were not listed (the `limit` or [`STATS_MAX_BYTES`]): the lightest ones.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_groups: usize,
    /// Failures that came again and again, with how often. The clustering is a heuristic
    /// (digits, paths and ids are stripped and what is equal is counted); the counts are exact.
    pub failures: Vec<FailureCluster>,
    /// Agents that look stuck, and what is not known about others.
    pub flags: Vec<StatsFlag>,
    /// What this answer cannot say, in words.
    pub not_recorded: Vec<String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// What the numbers of the answer rest on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsCoverage {
    /// The state log is on: terminal agents' waits are recorded (from the time it began).
    pub state_log: bool,
    /// The oldest row of the state log, when it holds any: a window that begins before it has
    /// no record of terminal agents' waits before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_log_from_ms: Option<u64>,
    /// Token counts are kept 35 days: a window that begins before this has none before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_from_ms: Option<u64>,
    /// The sessions of the answer, by how they are seen: a chat has a journal; a terminal agent
    /// only has what it reports.
    pub chats: usize,
    pub terminal_agents: usize,
}

/// The numbers of one group. A part that nothing recorded is absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatsGroup {
    /// What the group is: a project id, a harness, `2026-10-30`, a session id, a task id.
    pub key: String,
    /// What a person reads: the project's name, the session's title, the task's.
    pub label: String,
    /// How the sessions of this group are seen (§ coverage of the spike).
    pub coverage: GroupCoverage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<TurnStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<TaskStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waits: Option<WaitStats>,
    /// Retries of chats: inferred, never exact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<RetryStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenStats>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupCoverage {
    /// Chat sessions in the group: their turns, requests and tokens are exact.
    pub chats: usize,
    /// Terminal agents in the group: only their waits, and only from the state log.
    pub terminal_agents: usize,
}

/// Chat turns that ended in the window (exact; terminal agents have no turns).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStats {
    pub count: usize,
    pub failed: usize,
    pub cancelled: usize,
    /// Wall time from start to end: the sum, the longest and the mean.
    pub wall_ms_total: u64,
    pub wall_ms_longest: u64,
    pub wall_ms_mean: u64,
    /// Active time (the harness's own figure) is not recorded per turn: absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_ms_total: Option<u64>,
}

/// Tasks that ended in the window (exact: a task has a start and an end).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStats {
    pub done: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub wall_ms_total: u64,
    pub wall_ms_longest: u64,
}

/// What agents waited for the user, in the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitStats {
    pub count: usize,
    pub total_ms: u64,
    pub longest_ms: u64,
    /// Of those, the ones of chats (exact: the audit tables) and of terminal agents (from the
    /// state log: [`WaitSource::StateLog`]).
    pub by_source: Vec<WaitSide>,
    /// By what was waited for: `command`, `file_change`, `question`, `plan`, `permission`, …
    pub by_kind: Vec<WaitKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitSource {
    /// The approval and question tables of a chat: exact.
    Audit,
    /// The state log of a terminal agent: exact as far as it is on and as far as it goes back;
    /// a wait that has not ended counts up to the end of the window.
    StateLog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitSide {
    pub source: WaitSource,
    pub count: usize,
    pub total_ms: u64,
    pub longest_ms: u64,
    /// Waits that had not ended at the end of the window.
    pub open: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitKind {
    pub kind: String,
    pub count: usize,
    pub total_ms: u64,
}

/// Turns that were tried again after a failure. Never exact: a retry is a click, not an event,
/// so a turn of the same chat that began within [`RETRY_WITHIN_MS`] of a failed one is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryStats {
    pub count: usize,
    /// Always `true`: the number is inferred.
    pub inferred: bool,
}

/// How soon a turn after a failed one is taken for its retry.
pub const RETRY_WITHIN_MS: u64 = 10 * 60 * 1000;

/// Tokens of the turns that reported them (exact), and their cost where the harness gave one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TokenStats {
    /// Turns that reported usage: fewer than `turns.count` when some did not.
    pub turns_reported: usize,
    pub input: u64,
    pub cached_input: u64,
    pub output: u64,
    pub total: u64,
    /// The harnesses' own estimate, summed over the turns that gave one; absent when none did
    /// (Codex and direct chats report none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd_estimate: Option<f64>,
}

/// One failure that came again, as it reads once digits, paths and ids are stripped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureCluster {
    pub pattern: String,
    pub count: usize,
    /// In how many sessions it came.
    pub sessions: usize,
    pub last_ms: u64,
    /// Where it was recorded: the error of a failed chat turn, or a terminal agent's error
    /// state (state log).
    pub source: FailureSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureSource {
    TurnError,
    TerminalError,
}

/// An agent worth a look, with how sure the daemon is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsFlag {
    pub session_id: SessionId,
    pub title: String,
    pub kind: FlagKind,
    pub since_ms: u64,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagKind {
    /// A chat's turn has run a long time: exact (`turn_active`, its start).
    LongTurn,
    /// A terminal agent says it works and has said nothing for hours. Its state is
    /// *unconfirmed*, not "stuck": nothing proves the agent is not simply working.
    UnconfirmedWorking,
}
