//! Claude Code's log events (spike § 2, `fixtures/claude-code-2.1.316/logs.json`).
//!
//! `input_tokens` of an `api_request` **excludes** the cache counters (Anthropic's shape), which
//! is what the reducer expects of `claude`. `cost_usd` is the agent's own list-price estimate
//! per request; the mapper sums it into the conversation's cost. `tool_result`'s
//! `tool_input_size_bytes` and the like are never read, nor are `prompt`, `response` or any
//! text.

use hutan_core::insight::{InsightOutcome, TurnPhase};

use super::{AgentState, Builder, counts, key, model_of};
use crate::model::Attrs;

/// The event's name without the `claude_code.` prefix the body carries.
fn event(attrs: &Attrs) -> Option<&str> {
    let name = attrs.str("event.name")?;
    Some(name.strip_prefix("claude_code.").unwrap_or(name))
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
            // A prompt is counted once: `prompt.id` tells a retried batch from a new prompt.
            if let Some(id) = attrs.str("prompt.id")
                && !state.seen.insert(key("claude.prompt", id.as_bytes()))
            {
                return;
            }
            if state.open_turn.take().is_some() {
                builder.turn(TurnPhase::End, Some(InsightOutcome::Completed), None);
            }
            builder.turn(TurnPhase::Start, None, None);
            state.touch(now_ms);
        }
        Some("api_request") => {
            if let Some(id) = attrs.str("request_id")
                && !state.seen.insert(key("claude.request", id.as_bytes()))
            {
                return;
            }
            let Some(tokens) = counts(
                attrs.u64("input_tokens"),
                attrs.u64("cache_read_tokens"),
                attrs.u64("cache_creation_tokens"),
                attrs.u64("output_tokens"),
                None,
            ) else {
                return;
            };
            builder.tokens(tokens);
            if let Some(usd) = attrs.f64("cost_usd") {
                builder.cost(state.add_cost(usd));
            }
            if let Some(ms) = attrs.u64("duration_ms") {
                builder.api_ms(state.add_api_ms(ms));
            }
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
            if let Some(id) = attrs.str("tool_use_id")
                && !state.seen.insert(key("claude.tool", id.as_bytes()))
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
