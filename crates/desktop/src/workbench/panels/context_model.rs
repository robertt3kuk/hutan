//! What the Context panel says about an [`AgentInsight`], as plain data: the meter, the
//! breakdown by category, the turn rows, the totals, the tools and files, and the honest notes
//! of a harness that does not report something. No GPUI here; the panel draws what this
//! returns, and the tests read it.
//!
//! A number nobody reported is absent: this module never turns a missing figure into a zero.
//! A figure the harness rounded or derived (`exactness: estimated`) is written with `≈`.

use hutan_core::TurnId;
use hutan_core::agent::FileChangeKind;
use hutan_core::insight::{
    AgentInsight, BreakdownKind, BreakdownRow, CostKind, Exactness, InsightBlock, InsightContext,
    InsightField, InsightOutcome, InsightSourceKind, InsightTurn,
};

use crate::workbench::chat::widgets::{compact_tokens, duration};

/// The word for a harness id the insight carries (`omp`, `claude`, `codex`, `direct`).
pub fn harness_label(harness: &str) -> String {
    match harness {
        "direct" => "Direct chat".to_owned(),
        other => crate::workbench::harnesses::harness_name(other),
    }
}

/// `41 %`, `<1 %` for a context that holds something but less than one percent.
pub fn percent_text(used: u64, window: u64) -> String {
    if window == 0 {
        return String::new();
    }
    let percent = used.saturating_mul(100) / window;
    if percent == 0 && used > 0 {
        "<1 %".to_owned()
    } else {
        format!("{percent} %")
    }
}

/// `≈ ` before a figure the harness estimated, nothing before one it reported.
fn approx(exactness: Exactness) -> &'static str {
    match exactness {
        Exactness::Reported => "",
        Exactness::Estimated => "≈ ",
    }
}

/// The meter's words: `82k / 200k · 41 %`; `82k tokens` when the window is unknown (a share of
/// nothing is not known); with `≈` when the harness estimated the figure.
pub fn meter_text(context: &InsightContext) -> String {
    let mark = approx(context.exactness);
    match context.window.filter(|window| *window > 0) {
        Some(window) => format!(
            "{mark}{} / {} · {}",
            compact_tokens(context.used),
            compact_tokens(window),
            percent_text(context.used, window)
        ),
        None => format!("{mark}{} tokens", compact_tokens(context.used)),
    }
}

/// How full the window is, 0 to 1; `None` without a window.
pub fn fill(context: &InsightContext) -> Option<f32> {
    let window = context.window.filter(|window| *window > 0)?;
    Some((context.used as f64 / window as f64).clamp(0.0, 1.0) as f32)
}

/// Where the harness compacts by itself, as a share of the window.
pub fn compact_marker(context: &InsightContext) -> Option<f32> {
    let window = context.window.filter(|window| *window > 0)?;
    let at = context.compact_at.filter(|at| *at > 0 && *at < window)?;
    Some((at as f64 / window as f64) as f32)
}

/// What colours a category: a closed set of looks, so an unknown id from a newer harness gets
/// [`Swatch::Other`] and its own label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swatch {
    System,
    Tools,
    Project,
    Skills,
    Messages,
    Mcp,
    Agents,
    /// OMP's non-message part, which it does not split.
    Fixed,
    Other,
    Buffer,
    Free,
}

pub fn swatch(row: &BreakdownRow) -> Swatch {
    match row.kind {
        BreakdownKind::Buffer => return Swatch::Buffer,
        BreakdownKind::Free => return Swatch::Free,
        BreakdownKind::Used | BreakdownKind::Deferred => {}
    }
    match row.id.as_str() {
        "system_prompt" => Swatch::System,
        "tools" | "tools_deferred" => Swatch::Tools,
        "project_context" => Swatch::Project,
        "skills" => Swatch::Skills,
        "messages" => Swatch::Messages,
        "mcp" | "mcp_deferred" => Swatch::Mcp,
        "agents" => Swatch::Agents,
        "fixed" => Swatch::Fixed,
        "autocompact_buffer" => Swatch::Buffer,
        "free" => Swatch::Free,
        _ => Swatch::Other,
    }
}

/// One line of the breakdown list.
#[derive(Debug, Clone, PartialEq)]
pub struct BreakdownLine {
    pub label: String,
    pub tokens: u64,
    pub tokens_text: String,
    /// Share of the window (or of the rows when the window is unknown), as `4 %`.
    pub share_text: String,
    pub share: f32,
    pub swatch: Swatch,
    pub kind: BreakdownKind,
}

/// The breakdown of the context: what `/context` printed, as lines and the shares the stacked
/// bar draws. `None` until a measurement was read.
#[derive(Debug, Clone, PartialEq)]
pub struct Breakdown {
    pub lines: Vec<BreakdownLine>,
    /// `as of /context at 2026-10-28 14:02 +0500`, written for a tooltip; `as_of` is the age.
    pub as_of_ms: Option<u64>,
    pub estimated: bool,
}

pub fn breakdown(context: &InsightContext) -> Option<Breakdown> {
    if context.breakdown.is_empty() {
        return None;
    }
    // The bar is the window when it is known (the buffer and the free space are in the rows);
    // else the rows' own sum.
    let whole = context
        .window
        .filter(|window| *window > 0)
        .unwrap_or_else(|| {
            context
                .breakdown
                .iter()
                .filter(|row| row.kind != BreakdownKind::Deferred)
                .map(|row| row.tokens)
                .sum::<u64>()
                .max(1)
        });
    let lines = context
        .breakdown
        .iter()
        .filter(|row| row.tokens > 0)
        .map(|row| {
            let share = (row.tokens as f64 / whole as f64).clamp(0.0, 1.0) as f32;
            BreakdownLine {
                label: row.label.clone(),
                tokens: row.tokens,
                tokens_text: compact_tokens(row.tokens),
                share_text: percent_text(row.tokens, whole),
                share,
                swatch: swatch(row),
                kind: row.kind,
            }
        })
        .collect();
    Some(Breakdown {
        lines,
        as_of_ms: context.breakdown_as_of_ms,
        estimated: context.breakdown_exactness == Some(Exactness::Estimated),
    })
}

/// How the two rows of a breakdown that are not in the context are worded in the list.
pub fn kind_note(kind: BreakdownKind) -> Option<&'static str> {
    match kind {
        BreakdownKind::Used => None,
        BreakdownKind::Buffer => Some("reserved for compaction"),
        BreakdownKind::Free => Some("free"),
        BreakdownKind::Deferred => Some("loaded when needed"),
    }
}

// ---- turns ------------------------------------------------------------------------------------

/// What a turn's row offers for rewinding, before the checkpoint store says which button it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// One row of the turn list.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnLine {
    pub n: u64,
    pub turn_id: Option<TurnId>,
    /// `T6`.
    pub label: String,
    pub state: TurnState,
    /// The first line of the turn's prompt, when the window has it.
    pub prompt: Option<String>,
    /// What the turn added, in words: `+9.8k context`, `8.2k in · 52 out`.
    pub tokens: Option<String>,
    pub tools: Option<String>,
    pub files: Option<String>,
    pub duration: Option<String>,
    pub cost: Option<String>,
    pub started_ms: u64,
    /// Every figure of the turn, for the row's tooltip.
    pub detail: String,
    /// The insight's view of the turn's checkpoint; the store has the id.
    pub checkpoint: Option<hutan_core::CheckpointState>,
}

/// `$0.21`, `<$0.01`, `$12`.
pub fn money(cost: f64) -> String {
    if cost > 0.0 && cost < 0.01 {
        "<$0.01".to_owned()
    } else if cost < 100.0 {
        format!("${cost:.2}")
    } else {
        format!("${cost:.0}")
    }
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The rows of the turn list, newest first. `prompt_of` gives the first line of a turn's prompt
/// from the transcript the window holds (the insight itself carries no message text).
pub fn turn_lines(
    insight: &AgentInsight,
    prompt_of: &dyn Fn(TurnId) -> Option<String>,
) -> Vec<TurnLine> {
    let mut lines: Vec<TurnLine> = Vec::with_capacity(insight.turns.len());
    let mut previous: Option<u64> = None;
    for turn in &insight.turns {
        lines.push(turn_line(turn, previous, prompt_of));
        if turn.context_after.is_some() {
            previous = turn.context_after;
        }
    }
    lines.reverse();
    lines
}

fn turn_line(
    turn: &InsightTurn,
    previous_context: Option<u64>,
    prompt_of: &dyn Fn(TurnId) -> Option<String>,
) -> TurnLine {
    let state = match turn.outcome {
        InsightOutcome::Running => TurnState::Running,
        InsightOutcome::Completed => TurnState::Done,
        InsightOutcome::Failed => TurnState::Failed,
        InsightOutcome::Cancelled => TurnState::Cancelled,
    };
    // The growth of the context is what the turn added to it; without a context figure before
    // and after (the first turn, a harness that reports none) the turn's own input and output.
    let growth = match (previous_context, turn.context_after) {
        (Some(before), Some(after)) if after > before => {
            Some(format!("+{} context", compact_tokens(after - before)))
        }
        _ => None,
    };
    let spent = match (turn.input, turn.output) {
        (None, None) => None,
        (input, output) => Some(
            [
                input.map(|n| format!("{} in", compact_tokens(n))),
                output.map(|n| format!("{} out", compact_tokens(n))),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · "),
        ),
    };
    let tokens = growth.or(spent);
    let tools = (turn.tool_calls > 0).then(|| {
        let calls = plural(u64::from(turn.tool_calls), "tool call", "tool calls");
        if turn.tool_failed > 0 {
            format!("{calls}, {} failed", turn.tool_failed)
        } else {
            calls
        }
    });
    let files =
        (turn.files > 0).then(|| plural(u64::from(turn.files), "file changed", "files changed"));
    let mut detail = Vec::new();
    for (label, value) in [
        ("input", turn.input),
        ("output", turn.output),
        ("cache read", turn.cache_read),
        ("cache write", turn.cache_write),
        ("reasoning", turn.reasoning),
        ("context after", turn.context_after),
    ] {
        if let Some(value) = value {
            detail.push(format!("{label} {}", thousands(value)));
        }
    }
    TurnLine {
        n: turn.n,
        turn_id: turn.turn_id,
        label: format!("T{}", turn.n),
        state,
        prompt: turn.turn_id.and_then(prompt_of),
        tokens,
        tools,
        files,
        duration: turn.duration_ms.map(duration),
        cost: turn.cost_usd.map(money),
        started_ms: turn.started_ms,
        detail: detail.join(" · "),
        checkpoint: turn.checkpoint,
    }
}

/// `12 345` (thin grouping for exact figures in tooltips).
pub fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// The bars of *Fill by turn*: the context after each of the newest `limit` turns that
/// reported one, oldest first, as shares of the largest figure (or of the window when it is
/// larger), and the peak.
#[derive(Debug, Clone, PartialEq)]
pub struct FillChart {
    pub bars: Vec<f32>,
    pub peak: u64,
}

pub fn fill_chart(insight: &AgentInsight, limit: usize) -> Option<FillChart> {
    let values: Vec<u64> = insight
        .turns
        .iter()
        .filter_map(|turn| turn.context_after)
        .collect();
    if values.len() < 2 {
        return None;
    }
    let values = &values[values.len().saturating_sub(limit)..];
    let peak = values.iter().copied().max()?;
    let scale = insight
        .context
        .as_ref()
        .and_then(|context| context.window)
        .filter(|window| *window >= peak)
        .unwrap_or(peak)
        .max(1);
    Some(FillChart {
        bars: values
            .iter()
            .map(|value| (*value as f64 / scale as f64).clamp(0.0, 1.0) as f32)
            .collect(),
        peak,
    })
}

// ---- totals, tools, files, time -----------------------------------------------------------------

/// A label with its value, one line of a block.
pub type Pair = (String, String);

/// What the harness never reports, named: `Codex does not report cost.`
pub fn unsupported_note(harness: &str, unsupported: &[InsightField]) -> Option<String> {
    if unsupported.is_empty() {
        return None;
    }
    let words: Vec<&str> = unsupported
        .iter()
        .map(|field| match field {
            InsightField::Breakdown => "the context by category",
            InsightField::Cost => "cost",
            InsightField::Reasoning => "reasoning tokens",
        })
        .collect();
    let list = match words.as_slice() {
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
        [] => return None,
    };
    Some(format!(
        "{} does not report {list}.",
        harness_label(harness)
    ))
}

/// The cost line: the harness's own figure, labelled; or why there is none.
pub fn cost_pair(insight: &AgentInsight) -> Option<Pair> {
    let harness = insight.harness.as_deref().unwrap_or_default();
    let cost = insight
        .totals
        .as_ref()
        .and_then(|totals| totals.cost_usd.map(|cost| (cost, totals.cost_kind)));
    match cost {
        Some((cost, kind)) => {
            let value = match kind {
                Some(CostKind::Reported) => money(cost),
                Some(CostKind::Estimate) | None => format!("≈ {} est.", money(cost)),
            };
            Some(("Cost".to_owned(), value))
        }
        None if insight.unsupported.contains(&InsightField::Cost) => Some((
            "Cost".to_owned(),
            format!("not reported by {}", harness_label(harness)),
        )),
        None => None,
    }
}

/// Tokens, cache and cost, in the order the panel lists them. Absent figures are absent.
pub fn totals_pairs(insight: &AgentInsight) -> Vec<Pair> {
    let Some(totals) = insight.totals.as_ref() else {
        return Vec::new();
    };
    let mut rows: Vec<Pair> = Vec::new();
    let mut push = |label: &str, value: Option<u64>| {
        if let Some(value) = value {
            rows.push((label.to_owned(), compact_tokens(value)));
        }
    };
    push("Input", totals.input);
    push("Output", totals.output);
    push("Cache read", totals.cache_read);
    push("Cache write", totals.cache_write);
    push("Reasoning", totals.reasoning);
    if let Some(ratio) = totals.cache_hit_ratio {
        rows.push((
            "Cache hit".to_owned(),
            format!("{} %", (ratio * 100.0).round() as u64),
        ));
    }
    rows.extend(cost_pair(insight));
    rows
}

/// Wall time and the parts of it the harness reported.
pub fn time_pairs(insight: &AgentInsight) -> Vec<Pair> {
    let Some(time) = insight.time.as_ref() else {
        return Vec::new();
    };
    let mut rows = vec![("Wall".to_owned(), duration_long(time.wall_ms))];
    for (label, value) in [
        ("Working", time.active_ms),
        ("Model API", time.api_ms),
        ("Tools", time.tool_ms),
    ] {
        if let Some(value) = value {
            rows.push((label.to_owned(), duration_long(value)));
        }
    }
    rows
}

/// `14 min`, `2 h 5 min`, `38 s`: a span in the coarse units a session's age needs.
pub fn duration_long(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0 => "<1 s".to_owned(),
        1..=59 => format!("{seconds} s"),
        60..=3599 => format!("{} min", seconds / 60),
        _ => {
            let (hours, minutes) = (seconds / 3600, (seconds % 3600) / 60);
            if minutes == 0 {
                format!("{hours} h")
            } else {
                format!("{hours} h {minutes} min")
            }
        }
    }
}

/// One tool row: `read  14`, with the failures and the time when the harness reported them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLine {
    pub name: String,
    pub calls: u64,
    pub detail: Option<String>,
}

pub fn tool_lines(insight: &AgentInsight) -> Vec<ToolLine> {
    let Some(tools) = insight.tools.as_ref() else {
        return Vec::new();
    };
    tools
        .by_name
        .iter()
        .map(|tool| {
            let mut parts = Vec::new();
            if tool.failed > 0 {
                parts.push(format!("{} failed", tool.failed));
            }
            if let Some(ms) = tool.ms {
                parts.push(duration(ms));
            }
            ToolLine {
                name: tool.name.clone(),
                calls: tool.calls,
                detail: (!parts.is_empty()).then(|| parts.join(" · ")),
            }
        })
        .collect()
}

/// `31 calls · 2 failed`.
pub fn tools_summary(insight: &AgentInsight) -> Option<String> {
    let tools = insight.tools.as_ref()?;
    Some(if tools.failed > 0 {
        format!(
            "{} · {} failed",
            plural(tools.calls, "call", "calls"),
            tools.failed
        )
    } else {
        plural(tools.calls, "call", "calls")
    })
}

/// One file row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLine {
    pub path: String,
    /// `edited`, `created`, `deleted`, `renamed`.
    pub kind: &'static str,
    pub stat: Option<String>,
}

pub fn file_kind_word(kind: FileChangeKind) -> &'static str {
    match kind {
        FileChangeKind::Add => "created",
        FileChangeKind::Delete => "deleted",
        FileChangeKind::Update => "edited",
    }
}

pub fn file_lines(insight: &AgentInsight) -> Vec<FileLine> {
    let Some(files) = insight.files.as_ref() else {
        return Vec::new();
    };
    files
        .items
        .iter()
        .map(|file| FileLine {
            path: file.path.clone(),
            kind: file_kind_word(file.kind),
            stat: match (file.added, file.removed) {
                (None, None) => None,
                (added, removed) => Some(format!(
                    "+{} −{}",
                    added.unwrap_or_default(),
                    removed.unwrap_or_default()
                )),
            },
        })
        .collect()
}

/// `8 touched`, `8 touched · +120 −34`.
pub fn files_summary(insight: &AgentInsight) -> Option<String> {
    let files = insight.files.as_ref()?;
    let mut text = format!("{} touched", files.count);
    if files.added > 0 || files.removed > 0 {
        text.push_str(&format!(" · +{} −{}", files.added, files.removed));
    }
    Some(text)
}

/// The words for where a block came from.
pub fn source_word(source: InsightSourceKind) -> &'static str {
    match source {
        InsightSourceKind::Chat => "the chat",
        InsightSourceKind::Extension => "the OMP extension",
        InsightSourceKind::Hooks => "hooks",
        InsightSourceKind::Statusline => "the status line",
        InsightSourceKind::Transcript => "the transcript",
        InsightSourceKind::Osc7501 => "the terminal's status",
    }
}

pub fn block_word(block: InsightBlock) -> &'static str {
    match block {
        InsightBlock::Context => "context",
        InsightBlock::Breakdown => "breakdown",
        InsightBlock::Turns => "turns",
        InsightBlock::Tools => "tools",
        InsightBlock::Files => "files",
        InsightBlock::Time => "time",
    }
}

/// `context, turns, tools, files, time from the chat · breakdown from /context`: one line per
/// source, the blocks it gave.
pub fn source_lines(insight: &AgentInsight) -> Vec<String> {
    let mut by_source: Vec<(InsightSourceKind, Vec<&'static str>)> = Vec::new();
    for source in &insight.sources {
        let word = if source.block == InsightBlock::Breakdown {
            // The breakdown is always what the harness printed for `/context`.
            "breakdown (from /context)"
        } else {
            block_word(source.block)
        };
        match by_source
            .iter_mut()
            .find(|(kind, _)| *kind == source.source)
        {
            Some((_, blocks)) => blocks.push(word),
            None => by_source.push((source.source, vec![word])),
        }
    }
    by_source
        .into_iter()
        .map(|(source, blocks)| format!("{} — {}", source_word(source), blocks.join(", ")))
        .collect()
}

#[cfg(test)]
mod tests;
