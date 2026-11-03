//! Claude Code's transcript as insight (`ClaudeAdapter::usage_line`).
//!
//! Shapes read (`docs/integrations.md` § Insight lists which were recorded and which are not):
//!
//! - `assistant` lines: `message.model`, `message.id`, `message.usage {input_tokens,
//!   cache_read_input_tokens, cache_creation_input_tokens, output_tokens}`, `message.stop_reason`
//!   and `tool_use` parts (`id`, `name`, `input`). Claude Code writes one line per content block
//!   of a message, each with the message's usage: a message is counted once, by its id, and what
//!   a later line adds to it is the delta.
//! - `user` lines: a prompt (a string, or `text` parts that are not the harness's own notices) opens
//!   a turn; `tool_result` parts (`tool_use_id`, `is_error`) end the tool call and carry its
//!   time (`toolUseResult.durationMs`, else the two lines' times).
//! - `system` lines: `subtype: "turn_duration"` ends a turn; `contextUsage` is what the user's
//!   `/context` printed (recorded from Claude Code 2.1.312's `-p /context`).
//! - `cost-state` lines: `totalCostUSD`, `totalAPIDuration` (recorded as zeros; where the keys
//!   sit in a real session is not known: at the top or under `costState`).
//!
//! Lines of a sidechain (a subagent's, in older versions) are not the main agent's and are
//! skipped.

use serde_json::{Map, Value};

use super::usage::{
    BreakdownLine, Counts, FileKind, MAX_NAME_BYTES, Outcome, PendingTool, UsageEvent, UsageState,
    clip, number, plain_path,
};
use super::{is_harness_injected, timestamp};

pub(super) fn usage_line(line: &str, state: &mut UsageState, out: &mut Vec<UsageEvent>) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    let ts = object.get("timestamp").and_then(timestamp);
    state.identity(
        object.get("sessionId").and_then(Value::as_str),
        object.get("cwd").and_then(Value::as_str),
        None,
        out,
    );
    match object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "assistant" => assistant(object, ts, state, out),
        "user" => user(object, ts, state, out),
        "system" => system(object, ts, state, out),
        "cost-state" | "cost_state" => {
            let node = ["costState", "cost_state", "state"]
                .iter()
                .find_map(|key| object.get(*key).and_then(Value::as_object))
                .unwrap_or(object);
            cost(node, ts, out);
        }
        _ => {}
    }
    state.note_ts(ts);
    true
}

fn assistant(
    object: &Map<String, Value>,
    ts: Option<u64>,
    state: &mut UsageState,
    out: &mut Vec<UsageEvent>,
) {
    let Some(message) = object.get("message") else {
        return;
    };
    let named = message.get("model").and_then(Value::as_str);
    // `<synthetic>`: a message Claude Code wrote itself (an API error, a notice): no call.
    let synthetic = named.is_some_and(|model| model.starts_with('<'));
    state.identity(
        None,
        None,
        named.filter(|model| !model.starts_with('<')),
        out,
    );
    if !synthetic && let Some(usage) = message.get("usage").and_then(Value::as_object) {
        let id = message
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        tokens(usage, id, ts, state, out);
    }
    if let Some(parts) = message.get("content").and_then(Value::as_array) {
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let (Some(id), Some(name)) = (
                part.get("id").and_then(Value::as_str),
                part.get("name").and_then(Value::as_str),
            ) else {
                continue;
            };
            state.push_pending(PendingTool {
                id: id.to_owned(),
                name: clip(name, MAX_NAME_BYTES),
                started_ms: ts,
                files: files_of(name, part.get("input")),
            });
        }
    }
    if matches!(
        message.get("stop_reason").and_then(Value::as_str),
        Some("end_turn" | "max_tokens" | "stop_sequence" | "refusal")
    ) {
        state.end_turn(ts, Outcome::Completed, None, out);
    }
}

/// The tokens of the message `id` as `usage` says them now: the part of them not counted yet
/// as a [`UsageEvent::Tokens`], and the context a request of that size makes.
fn tokens(
    usage: &Map<String, Value>,
    id: &str,
    ts: Option<u64>,
    state: &mut UsageState,
    out: &mut Vec<UsageEvent>,
) {
    let mut counts = Counts {
        input: number(usage.get("input_tokens")),
        cache_read: number(usage.get("cache_read_input_tokens")),
        cache_write: number(usage.get("cache_creation_input_tokens")),
        output: number(usage.get("output_tokens")),
        reasoning: None,
        total: None,
    };
    if [counts.input, counts.cache_read, counts.output]
        .iter()
        .any(Option::is_some)
    {
        counts.total = Some(
            counts.input.unwrap_or(0) + counts.cache_read.unwrap_or(0) + counts.output.unwrap_or(0),
        );
    }
    let delta = match state
        .messages
        .iter_mut()
        .find(|(known, _)| !id.is_empty() && known == id)
    {
        Some((_, before)) => {
            let delta = counts.grown_from(before);
            *before = counts;
            delta
        }
        None => {
            if state.messages.len() >= super::usage::MAX_REMEMBERED_MESSAGES {
                state.messages.pop_front();
            }
            state.messages.push_back((id.to_owned(), counts));
            counts
        }
    };
    // What the request read: the context Claude Code's own percentage counts (input, cache
    // creation and cache read).
    let prompt = counts.input.unwrap_or(0)
        + counts.cache_read.unwrap_or(0)
        + counts.cache_write.unwrap_or(0);
    if !delta.any() && prompt == 0 {
        return;
    }
    if !state.turn_open {
        // The reader began in the middle of a turn (or after a compaction): the call belongs to
        // one that started before what was read.
        state.start_turn(ts, out);
    }
    if delta.any() {
        out.push(UsageEvent::Tokens {
            ts_ms: ts,
            counts: delta,
        });
    }
    if prompt > 0 {
        out.push(UsageEvent::Context {
            ts_ms: ts,
            used: prompt,
            window: state.window,
            estimated: false,
        });
    }
}

/// The files a tool call will change when it succeeds, from its input. Reads are not changes.
fn files_of(tool: &str, input: Option<&Value>) -> Vec<(String, FileKind)> {
    let key = match tool {
        "Edit" | "MultiEdit" | "Write" => "file_path",
        "NotebookEdit" => "notebook_path",
        _ => return Vec::new(),
    };
    input
        .and_then(|input| input.get(key))
        .and_then(Value::as_str)
        .and_then(plain_path)
        .map(|path| vec![(path, FileKind::Update)])
        .unwrap_or_default()
}

fn user(
    object: &Map<String, Value>,
    ts: Option<u64>,
    state: &mut UsageState,
    out: &mut Vec<UsageEvent>,
) {
    if object.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return;
    }
    let Some(content) = object.get("message").and_then(|m| m.get("content")) else {
        return;
    };
    let mut prompts: Vec<&str> = Vec::new();
    match content {
        Value::String(text) => prompts.push(text),
        Value::Array(parts) => {
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("tool_result") => tool_result(part, object, ts, state, out),
                    Some("text") => prompts.extend(part.get("text").and_then(Value::as_str)),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    for text in prompts {
        if text.trim().is_empty() {
            continue;
        }
        if text
            .trim_start()
            .to_lowercase()
            .starts_with("[request interrupted")
        {
            state.end_turn(ts, Outcome::Cancelled, None, out);
        } else if !is_harness_injected(text) {
            state.start_turn(ts, out);
            return;
        }
    }
}

fn tool_result(
    part: &Value,
    line: &Map<String, Value>,
    ts: Option<u64>,
    state: &mut UsageState,
    out: &mut Vec<UsageEvent>,
) {
    let Some(pending) = part
        .get("tool_use_id")
        .and_then(Value::as_str)
        .and_then(|id| state.take_pending(id))
    else {
        return;
    };
    let failed = part.get("is_error").and_then(Value::as_bool) == Some(true);
    let result = line.get("toolUseResult").filter(|r| r.is_object());
    let measured = result.and_then(|result| {
        ["durationMs", "duration_ms", "totalDurationMs"]
            .iter()
            .find_map(|key| number(result.get(*key)))
    });
    let ms = measured.or_else(|| match (pending.started_ms, ts) {
        (Some(start), Some(end)) => end.checked_sub(start),
        _ => None,
    });
    out.push(UsageEvent::Tool {
        ts_ms: ts,
        name: pending.name.clone(),
        failed,
        ms,
    });
    if failed {
        return;
    }
    for (path, kind) in pending.files {
        // `Write` creates or overwrites: the result says which.
        let kind = if pending.name == "Write"
            && result.and_then(|r| r.get("type")).and_then(Value::as_str) == Some("create")
        {
            FileKind::Add
        } else {
            kind
        };
        out.push(UsageEvent::File {
            ts_ms: ts,
            path,
            kind,
        });
    }
}

fn system(
    object: &Map<String, Value>,
    ts: Option<u64>,
    state: &mut UsageState,
    out: &mut Vec<UsageEvent>,
) {
    if object.get("subtype").and_then(Value::as_str) == Some("turn_duration") {
        state.end_turn(
            ts,
            Outcome::Completed,
            number(object.get("durationMs")),
            out,
        );
    }
    let Some(usage) = object
        .get("contextUsage")
        .or_else(|| object.get("context_usage"))
        .filter(|usage| usage.is_object())
    else {
        return;
    };
    state.identity(None, None, usage.get("model").and_then(Value::as_str), out);
    let window = ["raw_max_tokens", "rawMaxTokens", "max_tokens", "maxTokens"]
        .iter()
        .find_map(|key| number(usage.get(*key)))
        .filter(|window| *window > 0);
    if window.is_some() {
        state.window = window;
    }
    if let Some(used) = ["total_tokens", "totalTokens"]
        .iter()
        .find_map(|key| number(usage.get(*key)))
    {
        out.push(UsageEvent::Context {
            ts_ms: ts,
            used,
            window: state.window,
            estimated: false,
        });
    }
    let rows: Vec<BreakdownLine> = usage
        .get("categories")
        .and_then(Value::as_array)
        .map(|categories| {
            categories
                .iter()
                .filter_map(|category| {
                    Some(BreakdownLine {
                        name: clip(category.get("name")?.as_str()?, 64),
                        tokens: number(category.get("tokens"))?,
                        kind: category
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    })
                })
                .take(32)
                .collect()
        })
        .unwrap_or_default();
    if !rows.is_empty() {
        out.push(UsageEvent::Breakdown { ts_ms: ts, rows });
    }
}

fn cost(node: &Map<String, Value>, ts: Option<u64>, out: &mut Vec<UsageEvent>) {
    // No call made yet: the record says zero, which is no cost, not a free session.
    if let Some(usd) = ["totalCostUSD", "total_cost_usd"]
        .iter()
        .find_map(|key| node.get(*key).and_then(Value::as_f64))
        .filter(|usd| usd.is_finite() && *usd > 0.0)
    {
        out.push(UsageEvent::Cost { ts_ms: ts, usd });
    }
    if let Some(ms) = ["totalAPIDuration", "total_api_duration_ms"]
        .iter()
        .find_map(|key| number(node.get(*key)))
        .filter(|ms| *ms > 0)
    {
        out.push(UsageEvent::ApiTime { ts_ms: ts, ms });
    }
}
