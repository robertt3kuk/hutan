//! Spans that follow the OpenTelemetry GenAI semantic conventions: what Copilot CLI, Goose, OMP
//! and Gemini's traces send, and what any third-party agent can send (`docs/integrations.md`
//! § Report your agent's stats to Hutan, level L3).
//!
//! - `chat` (also `text_completion`, `generate_content`) spans carry one model call's usage;
//! - `execute_tool` spans are tool calls (the arguments and results are never read);
//! - an `invoke_agent` span is one turn: it ends the turn;
//! - the optional `hutan.*` attributes of a `chat` or `invoke_agent` span give what the
//!   conventions have no name for: the conversation's cost, the context, files changed.
//!
//! `gen_ai.usage.input_tokens` counts cached tokens by the conventions, and the report is in the
//! shape the reducer expects of an agent it does not know: input **without** the cache counters.
//! An agent that counts the other way says so with the resource attribute
//! `hutan.usage.input_includes_cache = false`; Claude Code, which does, has its own mapper.

use hutan_core::insight::{InsightOutcome, ReportedFile, TurnPhase};

use super::{Agent, AgentState, Builder, counts, file_kind, key, model_of};
use crate::MAX_ARRAY;
use crate::model::{Attrs, Span};

/// Files of the `hutan.files.changed` attribute that are read.
const MAX_FILES: usize = 32;

/// Whether the agent's `input_tokens` already include the cached ones.
pub(super) fn input_includes_cache(agent: Agent, resource: &Attrs) -> bool {
    resource
        .bool("hutan.usage.input_includes_cache")
        .unwrap_or(agent != Agent::Claude)
}

pub(super) fn span(
    state: &mut AgentState,
    builder: &mut Builder,
    span: &Span,
    includes_cache: bool,
    model: &mut Option<String>,
) {
    let attrs = &span.attrs;
    let Some(
        operation @ ("chat" | "text_completion" | "generate_content" | "execute_tool"
        | "invoke_agent"),
    ) = attrs.str("gen_ai.operation.name")
    else {
        return;
    };
    // An exporter that retries sends a span twice.
    if !span.span_id.is_empty() && !state.seen.insert(key("span", &span.span_id)) {
        return;
    }
    match operation {
        "execute_tool" => {
            let Some(name) = attrs
                .str("gen_ai.tool.name")
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                return;
            };
            let failed = span.error || attrs.str("error.type").is_some();
            builder.tool(name, failed, span.duration_ms());
        }
        "invoke_agent" => {
            extension(builder, attrs);
            let outcome = if span.error {
                InsightOutcome::Failed
            } else {
                InsightOutcome::Completed
            };
            builder.turn(TurnPhase::End, Some(outcome), span.duration_ms());
        }
        _ => {
            extension(builder, attrs);
            chat(state, builder, span, includes_cache, model);
        }
    }
}

fn chat(
    state: &mut AgentState,
    builder: &mut Builder,
    span: &Span,
    includes_cache: bool,
    model: &mut Option<String>,
) {
    let attrs = &span.attrs;
    if let Some(name) = model_of(attrs, &["gen_ai.response.model", "gen_ai.request.model"]) {
        *model = Some(name);
    }
    // A span that wraps one already counted adds nothing.
    if !span.span_id.is_empty() && state.parents.contains(key("span", &span.span_id)) {
        return;
    }
    let read = attrs.u64("gen_ai.usage.cache_read.input_tokens");
    let write = attrs.u64("gen_ai.usage.cache_creation.input_tokens");
    let mut input = attrs.u64("gen_ai.usage.input_tokens");
    if includes_cache {
        input = input.map(|input| {
            input.saturating_sub(read.unwrap_or(0).saturating_add(write.unwrap_or(0)))
        });
    }
    let Some(tokens) = counts(
        input,
        read,
        write,
        attrs.u64("gen_ai.usage.output_tokens"),
        attrs.u64("gen_ai.usage.reasoning.output_tokens"),
    ) else {
        return;
    };
    builder.tokens(tokens);
    if !span.parent_span_id.is_empty() {
        state.parents.insert(key("span", &span.parent_span_id));
    }
}

/// The `hutan.*` attributes.
fn extension(builder: &mut Builder, attrs: &Attrs) {
    if let Some(usd) = attrs.f64("hutan.cost.usd") {
        builder.cost(usd);
    }
    if let Some(used) = attrs.u64("hutan.context.used_tokens") {
        builder.context(used, attrs.u64("hutan.context.window_tokens"));
    }
    if let Some(changed) = attrs.strs("hutan.files.changed") {
        builder.files(
            changed
                .iter()
                .take(MAX_ARRAY)
                .filter_map(|entry| {
                    let (kind, path) = entry.split_once(':')?;
                    let path = path.trim();
                    if path.is_empty() {
                        return None;
                    }
                    Some(ReportedFile {
                        path: path.to_owned(),
                        kind: file_kind(kind)?,
                    })
                })
                .take(MAX_FILES),
        );
    }
}
