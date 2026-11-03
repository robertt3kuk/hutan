//! Codex's rollout as insight (`CodexAdapter::usage_line`).
//!
//! Lines are `{timestamp, type, payload}`. Read (`docs/integrations.md` § Insight lists which
//! shapes were recorded and which come from Codex's public source):
//!
//! - `session_meta` (`id`, `cwd`) and `turn_context` (`cwd`, `model`): who the conversation is.
//! - `event_msg` of type `task_started` / `turn_started` and `task_complete` / `turn_complete` /
//!   `turn_aborted`: the turn's edges; an older Codex without them has `user_message` begin one.
//! - `token_count {info {total_token_usage, last_token_usage, model_context_window}}`: what a
//!   model call used is the growth of the cumulative `total_token_usage` (a repeated event adds
//!   nothing), the context is the latest request's `last_token_usage.total_tokens` — the thread's
//!   total is not the context — and the window is the model's.
//! - `exec_command_end {exit_code, duration}`, `patch_apply_end {success, changes}`,
//!   `mcp_tool_call_end {invocation, result, duration}`, `web_search_end`: tools with their
//!   times and exit codes, and the files a patch changed.
//!
//! No command, output, diff or message text is read.

use serde_json::{Map, Value};

use super::timestamp;
use super::usage::{
    Counts, FileKind, MAX_FILES_PER_LINE, MAX_NAME_BYTES, Outcome, UsageEvent, UsageState, clip,
    number, plain_path,
};

pub(super) fn usage_line(line: &str, state: &mut UsageState, out: &mut Vec<UsageEvent>) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    let ts = object.get("timestamp").and_then(timestamp);
    let payload = object.get("payload").and_then(Value::as_object);
    match object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "session_meta" => {
            if let Some(payload) = payload {
                state.identity(
                    payload.get("id").and_then(Value::as_str),
                    payload.get("cwd").and_then(Value::as_str),
                    None,
                    out,
                );
            }
        }
        "turn_context" => {
            if let Some(payload) = payload {
                state.identity(
                    None,
                    payload.get("cwd").and_then(Value::as_str),
                    payload.get("model").and_then(Value::as_str),
                    out,
                );
            }
        }
        "event_msg" => {
            if let Some(payload) = payload {
                event(payload, ts, state, out);
            }
        }
        _ => {}
    }
    state.note_ts(ts);
    true
}

fn event(
    payload: &Map<String, Value>,
    ts: Option<u64>,
    state: &mut UsageState,
    out: &mut Vec<UsageEvent>,
) {
    match payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "task_started" | "turn_started" => {
            state.explicit_turns = true;
            if let Some(window) = number(payload.get("model_context_window")).filter(|w| *w > 0) {
                state.window = Some(window);
            }
            state.start_turn(ts, out);
        }
        "user_message" => {
            // A Codex that says where its turns begin has said it already.
            if !state.explicit_turns {
                state.start_turn(ts, out);
            }
        }
        "task_complete" | "turn_complete" => {
            let duration = number(payload.get("duration_ms"));
            state.end_turn(ts, Outcome::Completed, duration, out);
        }
        "turn_aborted" => state.end_turn(ts, Outcome::Cancelled, None, out),
        "token_count" => token_count(payload, ts, state, out),
        "exec_command_end" => {
            let failed = match number(payload.get("exit_code")) {
                Some(code) => code != 0,
                // A negative code is a failure the number reader does not take.
                None => {
                    payload.get("exit_code").is_some_and(|code| !code.is_null())
                        || payload
                            .get("status")
                            .and_then(Value::as_str)
                            .is_some_and(|status| status != "completed")
                }
            };
            tool(
                "Bash".to_owned(),
                failed,
                duration_ms(payload.get("duration")),
                ts,
                out,
            );
        }
        "patch_apply_end" => {
            let success = payload.get("success").and_then(Value::as_bool) != Some(false);
            tool("apply_patch".to_owned(), !success, None, ts, out);
            if success && let Some(changes) = payload.get("changes").and_then(Value::as_object) {
                for (path, change) in changes.iter().take(MAX_FILES_PER_LINE) {
                    let kind = match change.get("type").and_then(Value::as_str) {
                        Some("add") => FileKind::Add,
                        Some("delete") => FileKind::Delete,
                        _ => FileKind::Update,
                    };
                    if let Some(path) = plain_path(path) {
                        out.push(UsageEvent::File {
                            ts_ms: ts,
                            path,
                            kind,
                        });
                    }
                }
            }
        }
        "mcp_tool_call_end" => {
            let invocation = payload.get("invocation");
            let part = |key: &str| {
                invocation
                    .and_then(|i| i.get(key))
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
            };
            let name = clip(
                &format!("mcp__{}__{}", part("server"), part("tool")),
                MAX_NAME_BYTES,
            );
            // `result` is `{"Ok": …}` or `{"Err": …}`; an `Ok` that says `isError` failed too.
            let failed = payload.get("result").is_some_and(|result| {
                result.get("Err").is_some()
                    || result
                        .get("Ok")
                        .and_then(|ok| ok.get("isError"))
                        .and_then(Value::as_bool)
                        == Some(true)
            });
            tool(name, failed, duration_ms(payload.get("duration")), ts, out);
        }
        "web_search_end" => tool("web_search".to_owned(), false, None, ts, out),
        _ => {}
    }
}

fn tool(name: String, failed: bool, ms: Option<u64>, ts: Option<u64>, out: &mut Vec<UsageEvent>) {
    out.push(UsageEvent::Tool {
        ts_ms: ts,
        name,
        failed,
        ms,
    });
}

/// `{"secs": 1, "nanos": 500000000}` (Rust's `Duration`), or a number of milliseconds.
fn duration_ms(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if let Some(secs) = value.get("secs").and_then(Value::as_u64) {
        let nanos = value.get("nanos").and_then(Value::as_u64).unwrap_or(0);
        return Some(secs.saturating_mul(1000) + nanos / 1_000_000);
    }
    number(Some(value))
}

fn counts(usage: &Value) -> Counts {
    Counts {
        input: number(usage.get("input_tokens")),
        cache_read: number(usage.get("cached_input_tokens")),
        cache_write: None,
        output: number(usage.get("output_tokens")),
        reasoning: number(usage.get("reasoning_output_tokens")),
        total: number(usage.get("total_tokens")),
    }
}

fn token_count(
    payload: &Map<String, Value>,
    ts: Option<u64>,
    state: &mut UsageState,
    out: &mut Vec<UsageEvent>,
) {
    // `info` is null in an event that only carries the rate limits.
    let Some(info) = payload.get("info").filter(|info| info.is_object()) else {
        return;
    };
    if let Some(window) = number(info.get("model_context_window")).filter(|w| *w > 0) {
        state.window = Some(window);
    }
    let total = info.get("total_token_usage").map(counts);
    let last = info.get("last_token_usage").map(counts);
    if !state.turn_open {
        state.start_turn(ts, out);
    }
    let delta = match (total, state.previous_total, last) {
        (Some(total), Some(before), _) => Some(total.grown_from(&before)),
        // The first event of what was read: its request is what it says (a rollout starts at the
        // first call, so `last` and `total` agree; a resumed read has only `last` to go by).
        (Some(_), None, Some(last)) | (None, _, Some(last)) => Some(last),
        (Some(total), None, None) => Some(total),
        (None, _, None) => None,
    };
    if let Some(total) = total {
        state.previous_total = Some(total);
    }
    if let Some(delta) = delta.filter(Counts::any) {
        out.push(UsageEvent::Tokens {
            ts_ms: ts,
            counts: delta,
        });
    }
    if let Some(used) = last
        .and_then(|last| last.total)
        .or_else(|| total.and_then(|total| total.total))
    {
        out.push(UsageEvent::Context {
            ts_ms: ts,
            used,
            window: state.window,
            estimated: false,
        });
    }
}
