//! Mappers: decoded OTLP records → [`InsightReport`]s with `source: otlp`.
//!
//! One [`Mapper`] belongs to one session (the receiver keeps a registry of them). It holds what a
//! report needs to be right across requests, all bounded: the cumulative cost, the ids of the
//! requests, tools and spans already counted (an exporter that retries sends a batch twice), and
//! whether a turn is open.
//!
//! What each mapper reads (and nothing else — [`crate::allow`]):
//!
//! | Agent | Signal | Reads |
//! |---|---|---|
//! | Claude Code | logs | `user_prompt` (a turn begins), `api_request` (tokens, cost, API time, model), `tool_result` (tool, outcome, time) |
//! | Codex | logs | `user_prompt`, `sse_event` of `response.completed` (tokens), `tool_result` |
//! | any other (the GenAI semantic conventions) | traces | `chat` spans (tokens, model), `execute_tool` (tool, outcome, time), `invoke_agent` (the turn ends), the `hutan.*` extension (cost, context, files) |
//!
//! Metrics are never read: the logs and spans carry the same facts, and counting both would add
//! them twice.

mod claude;
mod codex;
mod genai;

use std::collections::{HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};

use hutan_core::agent::{FileChangeKind, TokenCounts};
use hutan_core::insight::{
    InsightOutcome, InsightReport, InsightSourceKind, ReportedContext, ReportedFile, ReportedTool,
    ReportedTurn, TurnPhase,
};

use crate::model::{Attrs, Decoded};

/// The agents Hutan knows by name. `Other` is the generic channel: any program that follows the
/// GenAI semantic conventions (`docs/integrations.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Agent {
    Claude,
    Codex,
    Omp,
    Gemini,
    Qwen,
    Copilot,
    Goose,
    OpenCode,
    Other,
}

impl Agent {
    pub const ALL: [Self; 9] = [
        Self::Claude,
        Self::Codex,
        Self::Omp,
        Self::Gemini,
        Self::Qwen,
        Self::Copilot,
        Self::Goose,
        Self::OpenCode,
        Self::Other,
    ];

    /// The name in `[telemetry.agents]` and in `hutan telemetry enable <agent>`; also the
    /// agent's program as the insight reducer knows it (`claude`, `codex`).
    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Omp => "omp",
            Self::Gemini => "gemini",
            Self::Qwen => "qwen",
            Self::Copilot => "copilot",
            Self::Goose => "goose",
            Self::OpenCode => "opencode",
            Self::Other => "other",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|agent| agent.id() == id)
    }

    /// What the agent is called to a person.
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Omp => "OMP",
            Self::Gemini => "Gemini CLI",
            Self::Qwen => "Qwen Code",
            Self::Copilot => "Copilot CLI",
            Self::Goose => "Goose",
            Self::OpenCode => "OpenCode",
            Self::Other => "other agents (OpenTelemetry GenAI)",
        }
    }

    /// The agent a resource's `service.name` names (the names the spike observed); an unknown
    /// name is [`Agent::Other`].
    pub fn from_service(name: &str) -> Self {
        let name = name.trim().to_ascii_lowercase();
        match name.as_str() {
            "claude-code" | "claude_code" | "claude" => Self::Claude,
            "oh-my-pi" | "omp" => Self::Omp,
            "gemini-cli" | "gemini" => Self::Gemini,
            "github-copilot" | "copilot" | "copilot-cli" => Self::Copilot,
            "goose" => Self::Goose,
            "opencode" => Self::OpenCode,
            _ if name.starts_with("codex") => Self::Codex,
            _ if name.starts_with("qwen") => Self::Qwen,
            _ => Self::Other,
        }
    }
}

/// What one request gave for one agent.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapped {
    pub agent: Agent,
    /// `service.version` of the agent, for display.
    pub service_version: Option<String>,
    /// The model the records name (the last one).
    pub model: Option<String>,
    /// In order: apply them one after the other. Every report has `source: otlp`.
    pub reports: Vec<InsightReport>,
}

/// Request ids and the like already counted, oldest forgotten first.
#[derive(Debug, Default)]
struct Recent {
    order: VecDeque<u64>,
    set: HashSet<u64>,
}

impl Recent {
    const CAPACITY: usize = 512;

    /// Remembers `key`; `false` when it was already there.
    fn insert(&mut self, key: u64) -> bool {
        if !self.set.insert(key) {
            return false;
        }
        self.order.push_back(key);
        if self.order.len() > Self::CAPACITY
            && let Some(old) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        true
    }

    fn contains(&self, key: u64) -> bool {
        self.set.contains(&key)
    }
}

/// A stable hash of a kind of record and its id.
fn key(kind: &str, id: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    kind.hash(&mut hasher);
    id.hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug, Default)]
struct AgentState {
    /// The conversation's cost so far, as the records summed it (cumulative).
    cost_usd: Option<f64>,
    /// Milliseconds the agent waited for its model so far.
    api_ms: Option<u64>,
    seen: Recent,
    /// Parents of the chat spans counted: a span that is such a parent wraps one already
    /// counted (Goose exports a `chat` span around a `chat` span).
    parents: Recent,
    /// A turn is open: the time of the last record that belongs to it.
    open_turn: Option<u64>,
}

/// The mapper of one session.
#[derive(Debug, Default)]
pub struct Mapper {
    agents: Vec<(Agent, AgentState)>,
}

impl Mapper {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&mut self, agent: Agent) -> &mut AgentState {
        let at = match self.agents.iter().position(|(held, _)| *held == agent) {
            Some(at) => at,
            None => {
                self.agents.push((agent, AgentState::default()));
                self.agents.len() - 1
            }
        };
        &mut self.agents[at].1
    }

    /// Maps what one request carried. `allow` says which agents the user enabled; records of
    /// another agent are left alone (nothing is counted, nothing is remembered). `now_ms` is
    /// when the request arrived.
    pub fn map(
        &mut self,
        decoded: &Decoded,
        allow: &dyn Fn(Agent) -> bool,
        now_ms: u64,
    ) -> Vec<Mapped> {
        let mut out: Vec<Mapped> = Vec::new();
        for resource in &decoded.resources {
            let agent = resource
                .attrs
                .str("service.name")
                .map_or(Agent::Other, Agent::from_service);
            if !allow(agent) {
                continue;
            }
            let state = self.state(agent);
            let mut builder = Builder::new();
            let mut model = None;
            match agent {
                // Claude Code and Codex are read from their log events only: their spans (the
                // beta traces of Claude Code, Codex's internal ones) tell the same facts again.
                Agent::Claude => {
                    for record in &resource.logs {
                        claude::log(state, &mut builder, &record.attrs, &mut model, now_ms);
                    }
                }
                Agent::Codex => {
                    for record in &resource.logs {
                        codex::log(state, &mut builder, &record.attrs, &mut model, now_ms);
                    }
                }
                // Every other agent's spans follow the GenAI semantic conventions.
                _ => {
                    let includes_cache = genai::input_includes_cache(agent, &resource.attrs);
                    for span in &resource.spans {
                        genai::span(state, &mut builder, span, includes_cache, &mut model);
                    }
                }
            }
            let reports = builder.finish();
            if reports.is_empty() {
                continue;
            }
            out.push(Mapped {
                agent,
                service_version: resource.attrs.str("service.version").map(str::to_owned),
                model,
                reports,
            });
        }
        out
    }

    /// Ends the turns that have been quiet for `idle_ms` since their last record, as completed:
    /// the logs of Claude Code and Codex say when a turn begins and never say that it ended.
    pub fn close_idle(&mut self, now_ms: u64, idle_ms: u64) -> Vec<Mapped> {
        let mut out = Vec::new();
        for (agent, state) in &mut self.agents {
            let Some(last) = state.open_turn else {
                continue;
            };
            if now_ms.saturating_sub(last) < idle_ms {
                continue;
            }
            state.open_turn = None;
            let mut builder = Builder::new();
            builder.turn(TurnPhase::End, Some(InsightOutcome::Completed), None);
            out.push(Mapped {
                agent: *agent,
                service_version: None,
                model: None,
                reports: builder.finish(),
            });
        }
        out
    }

    /// When the first open turn has been quiet for `idle_ms`, for a timer; `None` when no turn
    /// is open.
    pub fn next_idle_at(&self, idle_ms: u64) -> Option<u64> {
        self.agents
            .iter()
            .filter_map(|(_, state)| state.open_turn)
            .map(|last| last.saturating_add(idle_ms))
            .min()
    }
}

impl AgentState {
    /// The turn is alive at `now_ms`.
    fn touch(&mut self, now_ms: u64) {
        self.open_turn = Some(now_ms);
    }

    /// Adds `usd` to the conversation's cost; the new cumulative figure.
    fn add_cost(&mut self, usd: f64) -> f64 {
        let total = self.cost_usd.unwrap_or(0.0) + usd;
        self.cost_usd = Some(total);
        total
    }

    fn add_api_ms(&mut self, ms: u64) -> u64 {
        let total = self.api_ms.unwrap_or(0).saturating_add(ms);
        self.api_ms = Some(total);
        total
    }
}

/// Collects the reports of one resource in order. A turn's edge is a report of its own (the
/// reducer folds a report's turn edge first or last), and the content between two edges merges
/// into one report.
pub(crate) struct Builder {
    done: Vec<InsightReport>,
    cur: InsightReport,
}

fn fresh() -> InsightReport {
    InsightReport {
        source: InsightSourceKind::Otlp,
        ..InsightReport::default()
    }
}

fn add(sum: Option<u64>, value: Option<u64>) -> Option<u64> {
    match (sum, value) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (a, b) => a.or(b),
    }
}

impl Builder {
    fn new() -> Self {
        Self {
            done: Vec::new(),
            cur: fresh(),
        }
    }

    fn flush(&mut self) {
        if !self.cur.is_empty() {
            self.done.push(std::mem::replace(&mut self.cur, fresh()));
        }
    }

    pub(crate) fn turn(
        &mut self,
        phase: TurnPhase,
        outcome: Option<InsightOutcome>,
        duration_ms: Option<u64>,
    ) {
        self.flush();
        self.done.push(InsightReport {
            turn: Some(ReportedTurn {
                phase,
                outcome,
                duration_ms,
            }),
            ..fresh()
        });
    }

    /// One model call's counts (a delta); counts of calls in the same stretch add up.
    pub(crate) fn tokens(&mut self, tokens: TokenCounts) {
        let sum = self.cur.tokens.get_or_insert_with(TokenCounts::default);
        sum.input_tokens = add(sum.input_tokens, tokens.input_tokens);
        sum.cached_input_tokens = add(sum.cached_input_tokens, tokens.cached_input_tokens);
        sum.cache_write_tokens = add(sum.cache_write_tokens, tokens.cache_write_tokens);
        sum.output_tokens = add(sum.output_tokens, tokens.output_tokens);
        sum.reasoning_output_tokens =
            add(sum.reasoning_output_tokens, tokens.reasoning_output_tokens);
        sum.total_tokens = add(sum.total_tokens, tokens.total_tokens);
    }

    pub(crate) fn tool(&mut self, name: &str, failed: bool, ms: Option<u64>) {
        let failed = u32::from(failed);
        match self.cur.tools.iter_mut().find(|tool| tool.name == name) {
            Some(tool) => {
                tool.calls = tool.calls.saturating_add(1);
                tool.failed = tool.failed.saturating_add(failed);
                tool.ms = add(tool.ms, ms);
            }
            None => self.cur.tools.push(ReportedTool {
                name: name.to_owned(),
                calls: 1,
                failed,
                ms,
            }),
        }
    }

    pub(crate) fn cost(&mut self, cumulative_usd: f64) {
        self.cur.cost_usd = Some(cumulative_usd);
    }

    pub(crate) fn api_ms(&mut self, cumulative_ms: u64) {
        self.cur.api_ms = Some(cumulative_ms);
    }

    pub(crate) fn context(&mut self, used: u64, window: Option<u64>) {
        self.cur.context = Some(ReportedContext {
            used,
            window: window.filter(|window| *window > 0),
            fixed: None,
            estimated: false,
        });
    }

    pub(crate) fn files(&mut self, files: impl IntoIterator<Item = ReportedFile>) {
        self.cur.files.extend(files);
    }

    fn finish(mut self) -> Vec<InsightReport> {
        self.flush();
        self.done
    }
}

/// The counts of a call, as the agent counts them: `input` as the agent says, the cache counters
/// beside it.
pub(crate) fn counts(
    input: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
) -> Option<TokenCounts> {
    if input.is_none()
        && cache_read.is_none()
        && cache_write.is_none()
        && output.is_none()
        && reasoning.is_none()
    {
        return None;
    }
    Some(TokenCounts {
        input_tokens: input,
        cached_input_tokens: cache_read,
        cache_write_tokens: cache_write,
        output_tokens: output,
        reasoning_output_tokens: reasoning,
        total_tokens: None,
    })
}

/// A `FileChangeKind` named as the contract writes it (`add`, `update`, `delete`).
pub(crate) fn file_kind(text: &str) -> Option<FileChangeKind> {
    match text.trim().to_ascii_lowercase().as_str() {
        "add" | "added" | "create" | "created" => Some(FileChangeKind::Add),
        "update" | "updated" | "modify" | "modified" | "edit" | "edited" => {
            Some(FileChangeKind::Update)
        }
        "delete" | "deleted" | "remove" | "removed" => Some(FileChangeKind::Delete),
        _ => None,
    }
}

/// The attribute `name` of `attrs` as a model name worth keeping.
pub(crate) fn model_of(attrs: &Attrs, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| attrs.str(name))
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
}
