//! Codex's log events (spike § 2, `fixtures/codex-0.166.0/logs-turn.json`).
//!
//! Only the log exporter is read (Codex's traces and metrics are a firehose of internal spans
//! and nobody maps them). `input_token_count` **includes** the cached tokens (OpenAI's shape),
//! which is what the reducer expects of `codex`: the counts go through as they are, with the
//! cache beside them. Codex reports no cost. `tool_result` also carries the tool's `arguments`
//! and `output`; they are not on the allowlist and never reach the mapper.

use hutan_core::insight::{InsightOutcome, TurnPhase};

use super::{AgentState, Builder, counts, key, model_of};
use crate::model::Attrs;

/// The event's name without the `codex.` prefix.
fn event(attrs: &Attrs) -> Option<&str> {
    let name = attrs.str("event.name")?;
    Some(name.strip_prefix("codex.").unwrap_or(name))
}

pub(super) fn log(
    state: &mut AgentState,
    builder: &mut Builder,
    attrs: &Attrs,
    model: &mut Option<String>,
    now_ms: u64,
) {
    match event(attrs) {
        Some("user_prompt") => {
            // Codex gives no id: the timestamp tells a retried batch from a new prompt.
            if let Some(at) = attrs.str("event.timestamp")
                && !state.seen.insert(key("codex.prompt", at.as_bytes()))
            {
                return;
            }
            if state.open_turn.take().is_some() {
                builder.turn(TurnPhase::End, Some(InsightOutcome::Completed), None);
            }
            builder.turn(TurnPhase::Start, None, None);
            state.touch(now_ms);
        }
        Some("sse_event") => {
            // Only the completion that carries the counts: Codex logs `response.completed`
            // twice, once bare.
            if attrs.str("event.kind") != Some("response.completed") {
                return;
            }
            let Some(tokens) = counts(
                attrs.u64("input_token_count"),
                attrs.u64("cached_token_count"),
                attrs.u64("cache_write_token_count"),
                attrs.u64("output_token_count"),
                attrs.u64("reasoning_token_count"),
            ) else {
                return;
            };
            // Codex gives its records no id: the timestamp and the counts tell a retried batch
            // from a new call.
            let id = format!(
                "{}|{:?}|{:?}",
                attrs.str("event.timestamp").unwrap_or_default(),
                tokens.input_tokens,
                tokens.output_tokens
            );
            if attrs.str("event.timestamp").is_some()
                && !state.seen.insert(key("codex.completed", id.as_bytes()))
            {
                return;
            }
            builder.tokens(tokens);
            if let Some(name) = model_of(attrs, &["model"]) {
                *model = Some(name);
            }
            state.touch(now_ms);
        }
        Some("tool_result") => {
            let Some(name) = attrs
                .str("tool_name")
                .map(str::trim)
                .filter(|n| !n.is_empty())
            else {
                return;
            };
            if let Some(id) = attrs.str("call_id")
                && !state.seen.insert(key("codex.tool", id.as_bytes()))
            {
                return;
            }
            let failed = attrs.bool("success") == Some(false);
            builder.tool(name, failed, attrs.u64("duration_ms"));
            state.touch(now_ms);
        }
        _ => {}
    }
}
