//! Session history search (protocol 0.8, capability `history.search`): the local FTS index over
//! Hutan terminal blocks and external agent transcripts (`docs/history-index.md`).

use hutan_core::{SessionId, TerminalId};
use serde::{Deserialize, Serialize};

/// Most hits one `history.search` returns, whatever `limit` asks for.
pub const SEARCH_MAX_LIMIT: u32 = 100;
/// `limit` when a request leaves it out.
pub const SEARCH_DEFAULT_LIMIT: u32 = 20;

/// Where an indexed session came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistorySource {
    /// Hutan's own terminal blocks and agent reports.
    Hutan,
    Claude,
    Codex,
    Omp,
}

impl HistorySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hutan => "hutan",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Omp => "omp",
        }
    }
}

/// Who or what wrote an indexed row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRole {
    User,
    Assistant,
    /// A tool call, a tool result, or an agent state report.
    Tool,
    /// A command typed in a Hutan terminal.
    Command,
    /// The output of a Hutan terminal command.
    Output,
}

impl HistoryRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Command => "command",
            Self::Output => "output",
        }
    }
}

fn default_limit() -> u32 {
    SEARCH_DEFAULT_LIMIT
}

/// `history.search`. `query` is words and `"quoted phrases"`; every one must match (quotes and
/// operators have no special meaning). Hits are grouped by session: the best row of each session
/// with the number of rows of that session that matched, best session first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySearch {
    pub query: String,
    /// Sessions returned; 1 to [`SEARCH_MAX_LIMIT`].
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Only these sources; empty = all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<HistorySource>,
    /// Only rows of these roles; empty = all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<HistoryRole>,
    /// Only sessions whose working directory is this directory or inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Only rows at or after this time (Unix milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// Only rows at or before this time (Unix milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<u64>,
}

/// The best matching row of one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryHit {
    pub source: HistorySource,
    /// The session's id in its source: the agent's session id, or the Hutan session id.
    pub session_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub role: HistoryRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<u64>,
    /// Matched text with the matching words wrapped in `«` and `»`.
    pub snippet: String,
    /// BM25 score with role weights; higher is better, comparable only within one response.
    pub score: f64,
    /// Rows of this session that match.
    pub matches: u32,
    /// Command that resumes the agent session (validated like `agent.report` resume commands);
    /// absent for sources that cannot be resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_argv: Option<Vec<String>>,
    /// Protocol 0.12: the Hutan session (a terminal or a chat) a `hutan` hit belongs to, so
    /// that a client can open it; absent for transcripts of other tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Protocol 0.15 (capability `chat.provenance`): the external transcript file (absolute)
    /// of a `claude`, `codex` or `omp` hit, so that a client can reveal it; absent for `hutan`
    /// hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Protocol 0.15: for a `hutan` hit on a terminal block, the terminal the block was in
    /// (`terminal_id`s are per daemon run: a client compares it with the session's current
    /// terminal before jumping) and the block's id there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<TerminalId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryHits {
    pub hits: Vec<HistoryHit>,
}

/// `history.set_enabled`: turns indexing on or off and records it in `daemon.toml`
/// (`[history] enabled`). Answered with the new [`HistoryStatusInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySetEnabled {
    pub enabled: bool,
}

/// Counters of one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySourceStatus {
    /// `hutan`, `claude`, `codex`, `omp`.
    pub name: String,
    /// Transcript files known (0 for `hutan`).
    pub files: u64,
    /// Rows in the index.
    pub rows: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_indexed_ms: Option<u64>,
    /// Lines that could not be read, summed over the files.
    pub errors: u64,
}

/// Result of `history.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryStatusInfo {
    pub enabled: bool,
    /// The sources that are switched on (empty while disabled).
    pub sources: Vec<HistorySourceStatus>,
    /// Size of the index database with its WAL; 0 when there is none.
    pub db_bytes: u64,
}

// ---- the session catalog (protocol 0.29, capability `history.sessions`) ----------------------

/// Most rows one `history.sessions` returns, whatever `limit` asks for.
pub const SESSIONS_MAX_LIMIT: u32 = 1000;
/// `limit` when a request leaves it out.
pub const SESSIONS_DEFAULT_LIMIT: u32 = 250;
/// Longest text of a [`HistoryTurn`], in characters (longer text is cut with `…`).
pub const TURN_TEXT_MAX_CHARS: usize = 400;
/// Longest [`HistorySessionRow::first_prompt`], in characters.
pub const FIRST_PROMPT_MAX_CHARS: usize = 2000;
/// Most [`HistorySessionRow::latest`] turns.
pub const LATEST_TURNS_MAX: usize = 3;

fn default_sessions_limit() -> u32 {
    SESSIONS_DEFAULT_LIMIT
}

/// Which sessions `history.sessions` lists, by where they ran.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryScope {
    /// Every session of every enabled source.
    #[default]
    All,
    /// Sessions whose working directory is inside a folder of `workspace_id` (a user
    /// workspace with its member projects, a project, or Home).
    Workspace,
    /// Sessions attributed to the project `workspace_id` (longest matching root wins).
    Project,
}

/// The order of `history.sessions`, newest first either way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistorySessionSort {
    /// By the time of the last message.
    #[default]
    Updated,
    /// By the time of the first message.
    Created,
}

/// `history.sessions`: the browsable catalog of agent sessions on this machine (Hutan's own
/// chats and terminals, and the transcripts of Claude Code, Codex and OMP), one row per
/// session. Needs history indexing to be on, like `history.search`; answered with
/// [`HistorySessions`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySessionsRequest {
    #[serde(default)]
    pub scope: HistoryScope,
    /// The workspace or project of `workspace` and `project` scopes; absent with `all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<hutan_core::WorkspaceId>,
    /// Words that must all occur (case-insensitive substrings) in the title, first prompt,
    /// latest turns, agent, model, branch, working directory or session id. `repo:<text>`
    /// matches the attributed project's name or folder, `path:<text>` the working directory
    /// or transcript path. Empty: no filter.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub filter: String,
    #[serde(default)]
    pub sort: HistorySessionSort,
    /// Only these sources; empty = every enabled source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<HistorySource>,
    /// Rows returned; 1 to [`SESSIONS_MAX_LIMIT`].
    #[serde(default = "default_sessions_limit")]
    pub limit: u32,
    /// Look at the transcript files again before answering instead of answering from the
    /// catalog as it is (the refresh button).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh: bool,
}

/// One turn of a session as the catalog shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryTurn {
    pub role: HistoryRole,
    /// At most [`TURN_TEXT_MAX_CHARS`] characters, on one line.
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<u64>,
}

/// One session of the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySessionRow {
    pub source: HistorySource,
    /// The session's id in its source (as in [`HistoryHit::session_key`]).
    pub session_key: String,
    /// The source's own title (a renamed or generated title), else the first prompt's first
    /// line; absent when the session has no prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The model of the last turn that names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// User and assistant messages (tool calls and results are not counted).
    pub messages: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_ms: Option<u64>,
    /// The first user prompt, at most [`FIRST_PROMPT_MAX_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    /// The last turns, oldest first, at most [`LATEST_TURNS_MAX`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub latest: Vec<HistoryTurn>,
    /// The Hutan project (or workspace) whose folder holds `cwd`, the longest matching root;
    /// absent when no workspace has that folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<hutan_core::WorkspaceId>,
    /// As [`HistoryHit::resume_argv`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_argv: Option<Vec<String>>,
    /// As [`HistoryHit::path`]: the transcript file of a `claude`, `codex` or `omp` session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// As [`HistoryHit::session_id`]: the Hutan session of a `hutan` row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Protocol 0.30 (capability `session.close`): the Hutan session of this row was closed;
    /// `session.reopen` brings it back (a client shows Reopen instead of Open).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
}

/// Transcript files seen by one source in the last scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySourceFiles {
    /// `hutan`, `claude`, `codex`, `omp`.
    pub name: String,
    pub files: u64,
}

/// The last (or running) look at the transcript files: "Scanned N files · just now".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryScanInfo {
    /// Transcript files found, all sources.
    pub files: u64,
    /// Files read so far in a running scan (equals `files` when it is done).
    pub done: u64,
    pub scanning: bool,
    /// When the last complete scan finished (Unix milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_source: Vec<HistorySourceFiles>,
}

/// Result of `history.sessions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySessions {
    pub sessions: Vec<HistorySessionRow>,
    /// Rows that match the scope, sources and filter (`sessions` holds the first `limit`).
    pub matched: u64,
    /// Rows in the catalog for the enabled sources, before scope and filter: "2 of 250".
    pub loaded: u64,
    pub scan: HistoryScanInfo,
}
