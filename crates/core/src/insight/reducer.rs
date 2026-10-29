//! [`InsightReducer`]: the events of a chat folded into an [`AgentInsight`].

use std::collections::{HashMap, HashSet, VecDeque};

use crate::agent::{
    AgentEvent, AgentEventKind, FileChange, FileChangeKind, MessageRole, TokenCounts, ToolStatus,
};
use crate::ids::{ItemId, SessionId, TurnId};

use super::report::{self, ContextReport};
use super::{
    AgentInsight, BreakdownKind, BreakdownRow, CostKind, Exactness, InsightBlock, InsightContext,
    InsightFile, InsightFiles, InsightModel, InsightOutcome, InsightSource, InsightSourceKind,
    InsightTime, InsightTools, InsightTotals, InsightTurn, MAX_FILES, MAX_PATH_BYTES,
    MAX_SNAPSHOT_BYTES, MAX_TOOL_NAMES, MAX_TURNS, OTHER_TOOLS, ToolCount, TurnUsage,
    harness_insight,
};

/// Tool calls whose end is still awaited; the oldest are forgotten past this many.
const MAX_PENDING_TOOLS: usize = 64;
/// Files counted apart within one turn.
const MAX_TURN_FILES: usize = 256;
/// The answer to `/context` is read up to this many bytes.
const MAX_REPORT_BYTES: usize = 32 * 1024;

/// What a turn in progress has collected.
#[derive(Debug, Clone)]
struct Running {
    turn_id: Option<TurnId>,
    started_ms: u64,
    tool_calls: u32,
    tool_failed: u32,
    files: HashSet<String>,
    context_after: Option<u64>,
    /// The turn is `/context`: not a turn of the conversation.
    measure: bool,
    /// What the assistant said in it.
    answer: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Sums {
    turns: u64,
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    reasoning: Option<u64>,
    total: Option<u64>,
    cost: Option<f64>,
    active_ms: Option<u64>,
}

fn add(sum: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *sum = Some(sum.unwrap_or(0).saturating_add(value));
    }
}

#[derive(Debug, Clone)]
struct PendingTool {
    item: ItemId,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRow {
    path: String,
    kind: FileChangeKind,
    added: Option<u64>,
    removed: Option<u64>,
    /// The item whose change this row last took: the same item reporting the file again (Codex
    /// says a patch twice, when it starts and when it ends) replaces its figures.
    last_item: Option<ItemId>,
    last: (Option<u64>, Option<u64>),
}

/// When each block last changed.
#[derive(Debug, Clone, Copy, Default)]
struct Stamps {
    context: Option<u64>,
    breakdown: Option<u64>,
    turns: Option<u64>,
    tools: Option<u64>,
    files: Option<u64>,
    time: Option<u64>,
}

/// A turn read back from the usage records: a turn older than the journal window.
#[derive(Debug, Clone, PartialEq)]
pub struct SeedTurn {
    pub turn_id: String,
    pub at_ms: u64,
    pub tokens: TokenCounts,
    pub cost_usd: Option<f64>,
}

/// Folds the events of one chat session into an [`AgentInsight`]. Feed it the events in `seq`
/// order ([`apply`](Self::apply)), the cost the harness reports ([`cost_reported`](Self::cost_reported)),
/// and ask for a [`snapshot`](Self::snapshot).
#[derive(Debug, Clone)]
pub struct InsightReducer {
    session_id: SessionId,
    harness: String,
    cwd: Option<String>,
    usage: TurnUsage,
    last_seq: u64,
    first_ts: Option<u64>,
    last_ts: u64,
    stamps: Stamps,

    context_used: Option<u64>,
    context_window: Option<u64>,
    context_estimated: bool,
    breakdown: Option<(Vec<BreakdownRow>, u64, Exactness)>,

    ended: VecDeque<InsightTurn>,
    running: Option<Running>,
    sums: Sums,

    tools: Vec<ToolCount>,
    tool_calls: u64,
    tool_failed: u64,
    tool_ms: Option<u64>,
    pending_tools: VecDeque<PendingTool>,

    files: Vec<FileRow>,
    files_more: bool,
    files_added: u64,
    files_removed: u64,
}

impl InsightReducer {
    /// A reducer for the session `session_id` of `harness` (`codex`, `omp`, `claude`,
    /// `direct`), whose folder is `cwd`. `fresh`: the conversation began with this daemon's
    /// turns, so its cost, when reported, started at zero ([`TurnUsage::new`]).
    pub fn new(session_id: SessionId, harness: &str, cwd: Option<&str>, fresh: bool) -> Self {
        Self {
            session_id,
            harness: harness.to_owned(),
            cwd: cwd.map(|cwd| cwd.trim_end_matches('/').to_owned()),
            usage: TurnUsage::new(fresh),
            last_seq: 0,
            first_ts: None,
            last_ts: 0,
            stamps: Stamps::default(),
            context_used: None,
            context_window: None,
            context_estimated: false,
            breakdown: None,
            ended: VecDeque::new(),
            running: None,
            sums: Sums::default(),
            tools: Vec::new(),
            tool_calls: 0,
            tool_failed: 0,
            tool_ms: None,
            pending_tools: VecDeque::new(),
            files: Vec::new(),
            files_more: false,
            files_added: 0,
            files_removed: 0,
        }
    }

    /// The `seq` of the newest event applied; `0` before any.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Whether `kind` can change the insight: the pump skips the rest without a lock.
    pub fn wants(kind: &AgentEventKind) -> bool {
        matches!(
            kind,
            AgentEventKind::TurnStarted
                | AgentEventKind::MessageEnd { .. }
                | AgentEventKind::ToolStart { .. }
                | AgentEventKind::ToolEnd { .. }
                | AgentEventKind::FileDiff { .. }
                | AgentEventKind::Usage { .. }
                | AgentEventKind::TurnCompleted { .. }
                | AgentEventKind::TurnFailed { .. }
                | AgentEventKind::TurnCancelled
        )
    }

    /// Folds one event. An event at or before the newest one applied is ignored, so replaying
    /// a journal over live events is harmless. Returns whether the insight may have changed.
    pub fn apply(&mut self, event: &AgentEvent) -> bool {
        if event.seq <= self.last_seq || event.kind.is_streaming() {
            return false;
        }
        self.last_seq = event.seq;
        if !Self::wants(&event.kind) {
            return false;
        }
        self.first_ts.get_or_insert(event.ts_ms);
        self.last_ts = self.last_ts.max(event.ts_ms);
        match &event.kind {
            AgentEventKind::TurnStarted => {
                if self.running.is_some() {
                    // The turn before never ended (the daemon went down in it).
                    self.end_turn(event.ts_ms, InsightOutcome::Cancelled, None);
                }
                self.usage.turn_started();
                self.running = Some(Running {
                    turn_id: event.turn_id,
                    started_ms: event.ts_ms,
                    tool_calls: 0,
                    tool_failed: 0,
                    files: HashSet::new(),
                    context_after: None,
                    measure: false,
                    answer: String::new(),
                });
            }
            AgentEventKind::MessageEnd { role, text, .. } => {
                let Some(running) = self.running.as_mut() else {
                    return false;
                };
                match role {
                    role if super::is_measure_message(*role, text) => running.measure = true,
                    MessageRole::Assistant if running.measure => {
                        if running.answer.len() < MAX_REPORT_BYTES {
                            running.answer.push_str(text);
                            running.answer.push('\n');
                        }
                    }
                    _ => return false,
                }
            }
            AgentEventKind::Usage {
                total,
                last,
                context_window,
            } => {
                self.usage.observe(*total, *last);
                if let Some(window) = context_window.filter(|window| *window > 0) {
                    self.context_window = Some(window);
                }
                // The context is the latest request's size when the harness also gives the
                // conversation's cumulative total (Codex, direct chats): the next request
                // resends the history. A harness that gives only a total (an ACP harness's
                // `usage_update`) says how full the context is with it. `last` alone is the
                // turn's spend, not the context.
                let context = match (total, last) {
                    (Some(total), Some(last)) => last.total_tokens.or(total.total_tokens),
                    (Some(total), None) => total.total_tokens,
                    _ => None,
                };
                if let Some(context) = context {
                    self.context_used = Some(context);
                    self.context_estimated = false;
                    if let Some(running) = self.running.as_mut() {
                        running.context_after = Some(context);
                    }
                }
                self.stamps.context = Some(event.ts_ms);
            }
            AgentEventKind::ToolStart { name, .. } => {
                self.tool_started(event.item_id, name, event.ts_ms);
            }
            AgentEventKind::ToolEnd {
                status,
                duration_ms,
                ..
            } => self.tool_ended(event.item_id, *status, *duration_ms, event.ts_ms),
            AgentEventKind::FileDiff { changes, .. } => {
                for change in changes {
                    self.file_changed(event.item_id, change, event.ts_ms);
                }
            }
            AgentEventKind::TurnCompleted { duration_ms } => {
                self.end_turn(event.ts_ms, InsightOutcome::Completed, *duration_ms);
            }
            AgentEventKind::TurnFailed { .. } => {
                self.end_turn(event.ts_ms, InsightOutcome::Failed, None);
            }
            AgentEventKind::TurnCancelled => {
                self.end_turn(event.ts_ms, InsightOutcome::Cancelled, None);
            }
            _ => return false,
        }
        true
    }

    /// The harness reported the cumulative cost of the session so far, in US dollars.
    pub fn cost_reported(&mut self, cost_usd: f64) {
        self.usage.cost_reported(cost_usd);
    }

    /// Fills what the journal cannot say from the usage records of the session's turns (oldest
    /// first): the cost of a turn, and, when `journal_cut` (the journal was read only as far
    /// back as its window), the turns that ended before it.
    pub fn seed(&mut self, rows: &[SeedTurn], journal_cut: bool) {
        let by_id: HashMap<String, usize> = self
            .ended
            .iter()
            .enumerate()
            .filter_map(|(at, turn)| Some((turn.turn_id?.to_string(), at)))
            .collect();
        let first = self.first_ts.unwrap_or(u64::MAX);
        let mut older = Vec::new();
        for row in rows {
            match by_id.get(&row.turn_id) {
                Some(&at) => {
                    if self.ended[at].cost_usd.is_none()
                        && let Some(cost) = row.cost_usd
                    {
                        self.ended[at].cost_usd = Some(cost);
                        self.sums.cost = Some(self.sums.cost.unwrap_or(0.0) + cost);
                    }
                }
                None if journal_cut && row.at_ms < first => older.push(row),
                None => {}
            }
        }
        for row in older.into_iter().rev().take(MAX_TURNS) {
            let turn = InsightTurn {
                n: 0,
                turn_id: row.turn_id.parse().ok(),
                started_ms: row.at_ms,
                duration_ms: None,
                outcome: InsightOutcome::Completed,
                input: row.tokens.input_tokens,
                output: row.tokens.output_tokens,
                cache_read: row.tokens.cached_input_tokens,
                cache_write: row.tokens.cache_write_tokens,
                reasoning: row.tokens.reasoning_output_tokens,
                cost_usd: row.cost_usd,
                context_after: None,
                tool_calls: 0,
                tool_failed: 0,
                files: 0,
                checkpoint: None,
            };
            self.sums.turns += 1;
            add(&mut self.sums.input, turn.input);
            add(&mut self.sums.output, turn.output);
            add(&mut self.sums.cache_read, turn.cache_read);
            add(&mut self.sums.cache_write, turn.cache_write);
            add(&mut self.sums.reasoning, turn.reasoning);
            add(&mut self.sums.total, row.tokens.total_tokens);
            if let Some(cost) = turn.cost_usd {
                self.sums.cost = Some(self.sums.cost.unwrap_or(0.0) + cost);
            }
            if self.ended.len() < MAX_TURNS {
                self.ended.push_front(turn);
            }
        }
    }

    /// When the breakdown was last measured.
    pub fn breakdown_at(&self) -> Option<u64> {
        self.breakdown.as_ref().map(|(_, at, _)| *at)
    }

    /// Roughly how many bytes the reducer holds (a test of the budget reads it).
    pub fn approx_bytes(&self) -> usize {
        use std::mem::size_of;
        size_of::<Self>()
            + self.ended.capacity() * size_of::<InsightTurn>()
            + self
                .tools
                .iter()
                .map(|tool| size_of::<ToolCount>() + tool.name.capacity())
                .sum::<usize>()
            + self
                .files
                .iter()
                .map(|file| size_of::<FileRow>() + file.path.capacity())
                .sum::<usize>()
            + self
                .pending_tools
                .iter()
                .map(|pending| size_of::<PendingTool>() + pending.name.capacity())
                .sum::<usize>()
            + self.running.as_ref().map_or(0, |running| {
                size_of::<Running>()
                    + running.answer.capacity()
                    + running
                        .files
                        .iter()
                        .map(|path| size_of::<String>() + path.capacity())
                        .sum::<usize>()
            })
            + self.breakdown.as_ref().map_or(0, |(rows, ..)| {
                rows.iter()
                    .map(|row| size_of::<BreakdownRow>() + row.id.capacity() + row.label.capacity())
                    .sum()
            })
    }

    // Tools -----------------------------------------------------------------------------------

    fn tool_row(&mut self, name: &str) -> &mut ToolCount {
        let name = name.trim();
        let at = self.tools.iter().position(|row| row.name == name);
        let at = match at {
            Some(at) => at,
            None => {
                let named = self
                    .tools
                    .iter()
                    .filter(|row| row.name != OTHER_TOOLS)
                    .count();
                let name = if named < MAX_TOOL_NAMES {
                    clip(name, 64)
                } else {
                    OTHER_TOOLS.to_owned()
                };
                match self.tools.iter().position(|row| row.name == name) {
                    Some(at) => at,
                    None => {
                        self.tools.push(ToolCount {
                            name,
                            calls: 0,
                            failed: 0,
                            ms: None,
                        });
                        self.tools.len() - 1
                    }
                }
            }
        };
        &mut self.tools[at]
    }

    fn tool_started(&mut self, item: Option<ItemId>, name: &str, ts_ms: u64) {
        if let Some(item) = item {
            if self
                .pending_tools
                .iter()
                .any(|pending| pending.item == item)
            {
                return;
            }
            if self.pending_tools.len() >= MAX_PENDING_TOOLS {
                self.pending_tools.pop_front();
            }
            self.pending_tools.push_back(PendingTool {
                item,
                name: name.to_owned(),
            });
        }
        self.tool_row(name).calls += 1;
        self.tool_calls += 1;
        if let Some(running) = self.running.as_mut() {
            running.tool_calls = running.tool_calls.saturating_add(1);
        }
        self.stamps.tools = Some(ts_ms);
    }

    fn tool_ended(
        &mut self,
        item: Option<ItemId>,
        status: ToolStatus,
        duration_ms: Option<u64>,
        ts_ms: u64,
    ) {
        let pending = item.and_then(|item| {
            let at = self.pending_tools.iter().position(|p| p.item == item)?;
            self.pending_tools.remove(at)
        });
        let known = pending.is_some();
        let name = pending.map_or_else(|| OTHER_TOOLS.to_owned(), |pending| pending.name);
        if !known {
            // The start was before this reducer's window: the call is counted now.
            self.tool_row(&name).calls += 1;
            self.tool_calls += 1;
            if let Some(running) = self.running.as_mut() {
                running.tool_calls = running.tool_calls.saturating_add(1);
            }
        }
        // Only a time the harness reported is a time; the gap between two events is not.
        let failed = status == ToolStatus::Failed;
        let row = self.tool_row(&name);
        if failed {
            row.failed += 1;
        }
        add(&mut row.ms, duration_ms);
        if failed {
            self.tool_failed += 1;
            if let Some(running) = self.running.as_mut() {
                running.tool_failed = running.tool_failed.saturating_add(1);
            }
        }
        add(&mut self.tool_ms, duration_ms);
        self.stamps.tools = Some(ts_ms);
    }

    // Files -----------------------------------------------------------------------------------

    fn relative(&self, path: &str) -> String {
        let inside = self
            .cwd
            .as_deref()
            .and_then(|cwd| path.strip_prefix(cwd))
            .and_then(|rest| rest.strip_prefix('/'));
        clip(inside.unwrap_or(path), MAX_PATH_BYTES)
    }

    fn file_changed(&mut self, item: Option<ItemId>, change: &FileChange, ts_ms: u64) {
        let path = self.relative(&change.path);
        let (added, removed) = line_counts(change);
        if let Some(running) = self.running.as_mut()
            && (running.files.len() < MAX_TURN_FILES || running.files.contains(&path))
        {
            running.files.insert(path.clone());
        }
        self.stamps.files = Some(ts_ms);
        let at = match self.files.iter().position(|row| row.path == path) {
            Some(at) => at,
            None if self.files.len() < MAX_FILES => {
                self.files.push(FileRow {
                    path,
                    kind: change.kind,
                    added: None,
                    removed: None,
                    last_item: None,
                    last: (None, None),
                });
                self.files.len() - 1
            }
            None => {
                self.files_more = true;
                self.files_added += added.unwrap_or(0);
                self.files_removed += removed.unwrap_or(0);
                return;
            }
        };
        let row = &mut self.files[at];
        // The same item saying it again replaces what it said before.
        if item.is_some() && row.last_item == item {
            self.files_added = self.files_added.saturating_sub(row.last.0.unwrap_or(0));
            self.files_removed = self.files_removed.saturating_sub(row.last.1.unwrap_or(0));
            row.added = row.added.map(|n| n.saturating_sub(row.last.0.unwrap_or(0)));
            row.removed = row
                .removed
                .map(|n| n.saturating_sub(row.last.1.unwrap_or(0)));
        }
        row.kind = match (row.kind, change.kind) {
            // A file added and then edited is still new; deleted wins over everything.
            (FileChangeKind::Add, FileChangeKind::Update) => FileChangeKind::Add,
            (_, kind) => kind,
        };
        add(&mut row.added, added);
        add(&mut row.removed, removed);
        row.last_item = item;
        row.last = (added, removed);
        self.files_added += added.unwrap_or(0);
        self.files_removed += removed.unwrap_or(0);
    }

    // Turns -----------------------------------------------------------------------------------

    fn end_turn(&mut self, ts_ms: u64, outcome: InsightOutcome, duration_ms: Option<u64>) {
        let Some(running) = self.running.take() else {
            return;
        };
        let (tokens, cost) = self.usage.turn_ended();
        if running.measure {
            // `/context` is not a turn of the conversation; its answer is the breakdown.
            if let Some(parsed) = report::parse(&running.answer) {
                self.set_report(&parsed, ts_ms);
            }
            return;
        }
        let duration_ms = duration_ms.or_else(|| Some(ts_ms.saturating_sub(running.started_ms)));
        let turn = InsightTurn {
            n: 0,
            turn_id: running.turn_id,
            started_ms: running.started_ms,
            duration_ms,
            outcome,
            input: tokens.input_tokens,
            output: tokens.output_tokens,
            cache_read: tokens.cached_input_tokens,
            cache_write: tokens.cache_write_tokens,
            reasoning: tokens.reasoning_output_tokens,
            cost_usd: cost,
            context_after: running.context_after,
            tool_calls: running.tool_calls,
            tool_failed: running.tool_failed,
            files: u32::try_from(running.files.len()).unwrap_or(u32::MAX),
            checkpoint: None,
        };
        self.sums.turns += 1;
        add(&mut self.sums.input, turn.input);
        add(&mut self.sums.output, turn.output);
        add(&mut self.sums.cache_read, turn.cache_read);
        add(&mut self.sums.cache_write, turn.cache_write);
        add(&mut self.sums.reasoning, turn.reasoning);
        add(&mut self.sums.total, tokens.total_tokens);
        add(&mut self.sums.active_ms, turn.duration_ms);
        if let Some(cost) = turn.cost_usd {
            self.sums.cost = Some(self.sums.cost.unwrap_or(0.0) + cost);
        }
        if self.ended.len() >= MAX_TURNS {
            self.ended.pop_front();
        }
        self.ended.push_back(turn);
        self.stamps.turns = Some(ts_ms);
        self.stamps.time = Some(ts_ms);
    }

    fn set_report(&mut self, parsed: &ContextReport, at_ms: u64) {
        let window = parsed.window.filter(|window| *window > 0);
        if self.context_window.is_none() {
            self.context_window = window;
        }
        if self.context_used.is_none()
            && let Some(used) = parsed.used
        {
            // No request has reported the context yet: the report's own figure stands in.
            self.context_used = Some(used);
            self.context_estimated = parsed.exactness == Exactness::Estimated;
            self.stamps.context = Some(at_ms);
        }
        self.breakdown = Some((parsed.rows.clone(), at_ms, parsed.exactness));
        self.stamps.breakdown = Some(at_ms);
    }

    // Snapshot --------------------------------------------------------------------------------

    /// The insight as of `as_of_ms`, numbered `seq`, for a model the daemon knows (the
    /// reducer reads none from the events). Cut to [`MAX_SNAPSHOT_BYTES`] as JSON.
    pub fn snapshot(&self, seq: u64, as_of_ms: u64, model: Option<InsightModel>) -> AgentInsight {
        let facts = harness_insight(&self.harness);
        let mut insight = AgentInsight::empty(self.session_id, seq, as_of_ms);
        insight.harness = Some(self.harness.clone());
        insight.model = model;
        insight.unsupported = facts.unsupported.to_vec();
        insight.can_measure = facts.can_measure;

        insight.context = self.context_used.map(|used| {
            let (breakdown, breakdown_as_of_ms, breakdown_exactness) = match &self.breakdown {
                Some((rows, at, exactness)) => (rows.clone(), Some(*at), Some(*exactness)),
                None => (Vec::new(), None, None),
            };
            let compact_at = self.context_window.and_then(|window| {
                breakdown
                    .iter()
                    .find(|row| row.kind == BreakdownKind::Buffer)
                    .map(|buffer| window.saturating_sub(buffer.tokens))
            });
            InsightContext {
                used,
                window: self.context_window,
                exactness: if self.context_estimated {
                    Exactness::Estimated
                } else {
                    Exactness::Reported
                },
                as_of_ms: self.stamps.context.unwrap_or(as_of_ms),
                breakdown,
                breakdown_as_of_ms,
                breakdown_exactness,
                compact_at,
            }
        });
        // A context nobody reported has no breakdown block either way; a breakdown without a
        // context cannot happen (`set_report` fills the context when it is empty).

        if self.sums.turns > 0 || self.running.is_some() {
            let sums = &self.sums;
            let cost = self.usage.cost_so_far().or(sums.cost);
            let cache_hit_ratio = match (sums.cache_read, sums.input, facts.cache_in_input) {
                (Some(read), Some(input), Some(in_input)) => {
                    let prompt = if in_input {
                        input
                    } else {
                        input
                            .saturating_add(read)
                            .saturating_add(sums.cache_write.unwrap_or(0))
                    };
                    (prompt > 0).then(|| read as f64 / prompt as f64)
                }
                _ => None,
            };
            insight.totals = Some(InsightTotals {
                turns: sums.turns,
                input: sums.input,
                output: sums.output,
                cache_read: sums.cache_read,
                cache_write: sums.cache_write,
                reasoning: sums.reasoning,
                total: sums.total,
                cost_usd: cost,
                cost_kind: cost.map(|_| CostKind::Estimate),
                cache_hit_ratio,
            });
        }

        let ended = self.ended.len() as u64;
        let mut turns: Vec<InsightTurn> = self.ended.iter().cloned().collect();
        for (at, turn) in turns.iter_mut().enumerate() {
            turn.n = self.sums.turns.saturating_sub(ended - 1 - at as u64);
        }
        if let Some(running) = &self.running
            && !running.measure
        {
            turns.push(InsightTurn {
                n: self.sums.turns + 1,
                turn_id: running.turn_id,
                started_ms: running.started_ms,
                duration_ms: None,
                outcome: InsightOutcome::Running,
                input: None,
                output: None,
                cache_read: None,
                cache_write: None,
                reasoning: None,
                cost_usd: None,
                context_after: running.context_after,
                tool_calls: running.tool_calls,
                tool_failed: running.tool_failed,
                files: u32::try_from(running.files.len()).unwrap_or(u32::MAX),
                checkpoint: None,
            });
        }
        insight.turns_truncated = self.sums.turns > ended;
        insight.turns = turns;

        if self.tool_calls > 0 {
            let mut by_name = self.tools.clone();
            by_name.sort_by(|a, b| {
                // "other" sums the tail: it goes last.
                (a.name == OTHER_TOOLS)
                    .cmp(&(b.name == OTHER_TOOLS))
                    .then(b.calls.cmp(&a.calls))
                    .then_with(|| a.name.cmp(&b.name))
            });
            insight.tools = Some(InsightTools {
                calls: self.tool_calls,
                failed: self.tool_failed,
                by_name,
            });
        }
        if !self.files.is_empty() {
            insight.files = Some(InsightFiles {
                count: self.files.len() as u64,
                added: self.files_added,
                removed: self.files_removed,
                items: self
                    .files
                    .iter()
                    .map(|row| InsightFile {
                        path: row.path.clone(),
                        kind: row.kind,
                        added: row.added,
                        removed: row.removed,
                    })
                    .collect(),
                truncated: self.files_more,
            });
        }
        if let Some(started_ms) = self.first_ts {
            insight.time = Some(InsightTime {
                started_ms,
                wall_ms: as_of_ms.saturating_sub(started_ms),
                active_ms: self.sums.active_ms,
                api_ms: None,
                tool_ms: self.tool_ms,
            });
        }

        let stamps = &self.stamps;
        for (block, at) in [
            (InsightBlock::Context, stamps.context),
            (InsightBlock::Breakdown, stamps.breakdown),
            (InsightBlock::Turns, stamps.turns),
            (InsightBlock::Tools, stamps.tools),
            (InsightBlock::Files, stamps.files),
            (InsightBlock::Time, stamps.time),
        ] {
            if let Some(as_of_ms) = at {
                insight.sources.push(InsightSource {
                    block,
                    source: InsightSourceKind::Chat,
                    as_of_ms,
                });
            }
        }
        fit(&mut insight);
        insight
    }
}

/// Cuts `insight` until its JSON is at most [`MAX_SNAPSHOT_BYTES`]: the oldest turns and the
/// last files go first.
fn fit(insight: &mut AgentInsight) {
    loop {
        let size = serde_json::to_vec(insight).map_or(0, |json| json.len());
        if size <= MAX_SNAPSHOT_BYTES {
            return;
        }
        let files = insight.files.as_ref().map_or(0, |files| files.items.len());
        if insight.turns.len() > 50 {
            let cut = (insight.turns.len() / 4).max(1);
            insight.turns.drain(..cut);
            insight.turns_truncated = true;
        } else if files > 25 {
            if let Some(list) = insight.files.as_mut() {
                list.items.truncate(files / 2);
                list.count = list.items.len() as u64;
                list.truncated = true;
            }
        } else if !insight.turns.is_empty() {
            insight.turns.remove(0);
            insight.turns_truncated = true;
        } else if files > 0 {
            if let Some(list) = insight.files.as_mut() {
                list.items.pop();
                list.count = list.items.len() as u64;
                list.truncated = true;
            }
        } else {
            return;
        }
    }
}

/// `text` cut to at most `max` bytes on a character boundary.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn lines(text: &str) -> u64 {
    text.lines().count() as u64
}

/// Lines added and removed by one change, where the harness gave enough to know. The adapters
/// keep the provider's text in `patch` whatever the kind (`FileChange`): an added file's
/// content, a deleted file's old content, an update's unified diff.
fn line_counts(change: &FileChange) -> (Option<u64>, Option<u64>) {
    match (change.kind, change.patch.as_deref()) {
        (FileChangeKind::Add, Some(content)) => (Some(lines(content)), Some(0)),
        (FileChangeKind::Delete, Some(content)) => (Some(0), Some(lines(content))),
        (FileChangeKind::Update, Some(patch)) => {
            let mut added = 0;
            let mut removed = 0;
            for line in patch.lines() {
                if line.starts_with('+') && !line.starts_with("+++") {
                    added += 1;
                } else if line.starts_with('-') && !line.starts_with("---") {
                    removed += 1;
                }
            }
            (Some(added), Some(removed))
        }
        (FileChangeKind::Add, None) => (change.after.as_deref().map(lines), Some(0)),
        (FileChangeKind::Delete, None) => (Some(0), change.before.as_deref().map(lines)),
        (FileChangeKind::Update, None) => (None, None),
    }
}

#[cfg(test)]
mod tests;
