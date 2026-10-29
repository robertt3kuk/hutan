//! Agent insight (protocol 0.59, capability `agent.insight`; spike
//! `docs/spikes/2026-10-28-insight.md`): what a session's context holds, what each turn used,
//! which tools and files it touched and how long it took, in one record, with the source of
//! every block.
//!
//! [`InsightReducer`] folds the events of a chat (live or read back from the journal) into an
//! [`AgentInsight`]. It is pure: the daemon feeds it and decides who is told. A number nobody
//! reported is absent, never zero; [`AgentInsight::unsupported`] says which fields the harness
//! can never report, so a client words "not reported by Codex" instead of drawing zeros.
//!
//! Privacy: the record holds counts, tool names, file paths (relative to the session's folder
//! when inside it), times, and category names of the context. No message text, no tool output,
//! no diff.

mod reducer;
pub mod report;
mod tally;

use serde::{Deserialize, Serialize};

use crate::agent::FileChangeKind;
use crate::checkpoint::CheckpointState;
use crate::ids::{SessionId, TurnId};

pub use reducer::{InsightReducer, SeedTurn};
pub use tally::TurnUsage;

/// Turns kept in [`AgentInsight::turns`] (the newest).
pub const MAX_TURNS: usize = 200;
/// Distinct tool names counted apart; further names go into [`OTHER_TOOLS`].
pub const MAX_TOOL_NAMES: usize = 16;
/// Files listed in [`InsightFiles::items`].
pub const MAX_FILES: usize = 100;
/// Rows of [`InsightContext::breakdown`].
pub const MAX_BREAKDOWN: usize = 16;
/// A file path longer than this many bytes is cut.
pub const MAX_PATH_BYTES: usize = 260;
/// A snapshot larger than this many bytes (as JSON) is cut: the oldest turns and the last
/// files go first, and the block says it was truncated.
pub const MAX_SNAPSHOT_BYTES: usize = 32 * 1024;
/// The tool row that sums the names past [`MAX_TOOL_NAMES`].
pub const OTHER_TOOLS: &str = "other";
/// The slash command that makes OMP and Claude Code print their context by category. A turn
/// whose user message is this command is not a turn of the conversation: it is not counted,
/// and its answer is read as the breakdown ([`report::parse`]).
pub const MEASURE_COMMAND: &str = "/context";

/// Whether a message is the measurement command: a **user** message whose text is
/// [`MEASURE_COMMAND`] (surrounding white space ignored). The one rule for "this turn is a
/// measurement": the reducer does not count it, the chat view leaves it out, and a fork never
/// picks it as the latest turn.
pub fn is_measure_message(role: crate::agent::MessageRole, text: &str) -> bool {
    role == crate::agent::MessageRole::User && text.trim() == MEASURE_COMMAND
}

/// Everything known about one session's agent. Whole snapshots only: a client replaces what it
/// had when `seq` is larger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInsight {
    pub session_id: SessionId,
    /// The agent the record is about: the session's main agent. Absent when the answer is for
    /// the session as a whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Increases with every snapshot the daemon makes, across sessions and restarts of the
    /// daemon's cache.
    pub seq: u64,
    pub as_of_ms: u64,
    /// `codex`, `omp`, `claude` or `direct`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<InsightModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<InsightContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totals: Option<InsightTotals>,
    /// The newest turns, oldest first; a turn that still runs is last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<InsightTurn>,
    /// Turns older than the ones listed exist (the ring is full, or the snapshot was cut).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub turns_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<InsightTools>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<InsightFiles>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<InsightTime>,
    /// Where each block came from; empty when nothing reported yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<InsightSource>,
    /// Fields this harness never reports (as far as Hutan knows).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported: Vec<InsightField>,
    /// `agent.measure_context` works for this harness.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub can_measure: bool,
}

impl AgentInsight {
    /// A record with nothing in it.
    pub fn empty(session_id: SessionId, seq: u64, as_of_ms: u64) -> Self {
        Self {
            session_id,
            node_id: None,
            seq,
            as_of_ms,
            harness: None,
            model: None,
            context: None,
            totals: None,
            turns: Vec::new(),
            turns_truncated: false,
            tools: None,
            files: None,
            time: None,
            sources: Vec::new(),
            unsupported: Vec::new(),
            can_measure: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsightModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

/// How a figure came to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exactness {
    /// The harness said so.
    Reported,
    /// Rounded or derived by the harness itself (Claude Code's `/context` prints `1.9k`).
    Estimated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsightContext {
    /// Tokens in the context now: the latest request's (`last`) when the harness reports it
    /// with a cumulative total (Codex, direct chats), else the reported total (an ACP
    /// harness's `usage_update`).
    pub used: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    pub exactness: Exactness,
    pub as_of_ms: u64,
    /// By category, from the last `/context` ([`MEASURE_COMMAND`]); at most
    /// [`MAX_BREAKDOWN`] rows. Empty until one was read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub breakdown: Vec<BreakdownRow>,
    /// When the breakdown was measured; the context has grown since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakdown_as_of_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakdown_exactness: Option<Exactness>,
    /// The size at which the harness compacts by itself (the window less its buffer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_at: Option<u64>,
}

/// One category of the context.
///
/// `id` is a closed vocabulary the desktop knows (`system_prompt`, `tools`, `tools_deferred`,
/// `project_context`, `skills`, `messages`, `mcp`, `agents`, `autocompact_buffer`, `free`); an id
/// it does not know is shown under `label`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakdownRow {
    pub id: String,
    pub label: String,
    pub tokens: u64,
    pub kind: BreakdownKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakdownKind {
    /// Part of the context now.
    Used,
    /// Reserved by the harness for its own compaction.
    Buffer,
    /// What is left of the window.
    Free,
    /// Loaded only when needed (Claude Code's deferred tools).
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostKind {
    /// Billed, as the provider says.
    Reported,
    /// The harness's own figure from its price table: an estimate for a subscription.
    Estimate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsightTotals {
    /// Turns that ended (not counting `/context`).
    pub turns: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    /// As the harness counts it (`input` and `output`, and for some `cache_read`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_kind: Option<CostKind>,
    /// Derived: the share of the prompt that was read from the cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit_ratio: Option<f64>,
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightOutcome {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// One turn. Absent numbers were not reported for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsightTurn {
    /// The turn's position in the conversation, from 1.
    pub n: u64,
    /// The turn's id: `chat.fork {turn_id}` and `checkpoint.preview` take it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub outcome: InsightOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// The context after the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_after: Option<u64>,
    pub tool_calls: u32,
    pub tool_failed: u32,
    /// Distinct files the turn changed.
    pub files: u32,
    /// The turn's checkpoint, when the folder has them; what decides whether the turn can be
    /// rewound to (a client shows Fork only for `unsupported` and `late`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsightTools {
    pub calls: u64,
    pub failed: u64,
    /// Most used first; at most [`MAX_TOOL_NAMES`] names and [`OTHER_TOOLS`].
    pub by_name: Vec<ToolCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCount {
    pub name: String,
    pub calls: u64,
    pub failed: u64,
    /// Time the harness reported for these calls (Codex does; ACP harnesses do not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsightFiles {
    /// Files listed (at most [`MAX_FILES`]).
    pub count: u64,
    /// Lines added and removed over the listed files and the ones past the cap, where the
    /// harness gave a patch.
    pub added: u64,
    pub removed: u64,
    pub items: Vec<InsightFile>,
    /// More files were touched than are listed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsightFile {
    /// Relative to the session's folder when inside it.
    pub path: String,
    pub kind: FileChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsightTime {
    pub started_ms: u64,
    pub wall_ms: u64,
    /// Sum of the turns' durations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_ms: Option<u64>,
}

/// A block of [`AgentInsight`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightBlock {
    Context,
    Breakdown,
    Turns,
    Tools,
    Files,
    Time,
}

/// Where a block came from, best first: a chat sees the harness's own protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightSourceKind {
    Chat,
    Extension,
    Hooks,
    Statusline,
    Transcript,
    Osc7501,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsightSource {
    pub block: InsightBlock,
    pub source: InsightSourceKind,
    pub as_of_ms: u64,
}

/// A field a harness does not report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsightField {
    /// The context by category: no request or notification gives it (Codex) .
    Breakdown,
    /// A cost: Codex reports none per request, the gateway none at all.
    Cost,
    /// Tokens spent thinking.
    Reasoning,
}

/// What Hutan knows about a harness's insight: fixed facts per harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessInsight {
    pub unsupported: &'static [InsightField],
    /// `/context` makes the harness print its breakdown.
    pub can_measure: bool,
    /// Whether `input_tokens` already counts the cached ones (`Some(true)`: OpenAI's shape,
    /// Codex) or the cache counters come on top of it (`Some(false)`: Anthropic's shape). `None`:
    /// it depends on the provider (direct chats), and no cache ratio is derived.
    pub cache_in_input: Option<bool>,
}

/// The facts for `harness` (`codex`, `omp`, `claude`, `direct`); an unknown harness claims
/// nothing.
pub fn harness_insight(harness: &str) -> HarnessInsight {
    match harness {
        "omp" | "claude" => HarnessInsight {
            unsupported: &[InsightField::Reasoning],
            can_measure: true,
            cache_in_input: Some(false),
        },
        "codex" => HarnessInsight {
            unsupported: &[InsightField::Breakdown, InsightField::Cost],
            can_measure: false,
            cache_in_input: Some(true),
        },
        "direct" => HarnessInsight {
            unsupported: &[InsightField::Breakdown, InsightField::Cost],
            can_measure: false,
            cache_in_input: None,
        },
        _ => HarnessInsight {
            unsupported: &[],
            can_measure: false,
            cache_in_input: None,
        },
    }
}
