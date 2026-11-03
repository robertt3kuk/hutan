//! The attributes Hutan reads, by name (ADR 0026 decision 4: an allowlist, not a denylist).
//!
//! Every mapper reads only names listed here; the decoders drop every other attribute before
//! its value is read (the protobuf decoder keeps the value as an undecoded slice, the JSON
//! decoder skips it token by token). Prompts, tool arguments and outputs, system instructions,
//! command lines, file contents and e-mail addresses are on no list, so a payload that carries
//! them (the spike's findings F8: Codex's `tool_result`, Gemini's `process.command_args`,
//! OpenCode's `ai.prompt`, Qwen's `function_args`) has them discarded at decode time.
//!
//! A name is added here only together with the mapper that reads it and a test that says what
//! it is for.

/// Whether an attribute named `key` may be read at all.
pub fn attribute(key: &str) -> bool {
    matches!(
        key,
        // Resource: which agent, which version, how it counts input.
        "service.name"
            | "service.version"
            | "hutan.usage.input_includes_cache"
            // Log records of Claude Code and Codex: the event, the model, the counts, the tool's
            // name, outcome and duration, and the ids that tell a retried record from a new one.
            | "event.name"
            | "event.kind"
            | "event.timestamp"
            | "prompt.id"
            | "model"
            | "input_tokens"
            | "output_tokens"
            | "cache_read_tokens"
            | "cache_creation_tokens"
            | "cost_usd"
            | "request_id"
            | "tool_name"
            | "tool_use_id"
            | "success"
            | "duration_ms"
            | "input_token_count"
            | "output_token_count"
            | "cached_token_count"
            | "cache_write_token_count"
            | "reasoning_token_count"
            | "call_id"
            // Spans following the OpenTelemetry GenAI semantic conventions.
            | "gen_ai.operation.name"
            | "gen_ai.request.model"
            | "gen_ai.response.model"
            | "gen_ai.tool.name"
            | "gen_ai.usage.input_tokens"
            | "gen_ai.usage.output_tokens"
            | "gen_ai.usage.cache_read.input_tokens"
            | "gen_ai.usage.cache_creation.input_tokens"
            | "gen_ai.usage.reasoning.output_tokens"
            | "error.type"
            // The optional extension of the public contract (`docs/integrations.md`).
            | "hutan.cost.usd"
            | "hutan.context.used_tokens"
            | "hutan.context.window_tokens"
            | "hutan.files.changed"
    )
}

#[cfg(test)]
mod tests {
    use super::attribute;

    #[test]
    fn content_bearing_names_are_on_no_list() {
        for name in [
            "prompt",
            "prompt_text",
            "response",
            "arguments",
            "output",
            "function_args",
            "tool_input",
            "tool_parameters",
            "ai.prompt",
            "ai.prompt.messages",
            "ai.response.text",
            "ai.response.toolCalls",
            "gen_ai.input.messages",
            "gen_ai.output.messages",
            "gen_ai.system_instructions",
            "gen_ai.tool.call.arguments",
            "gen_ai.tool.call.result",
            "process.command_args",
            "process.command_line",
            "user.email",
            "user.id",
            "organization.id",
            "session.id",
            "conversation.id",
            "gen_ai.conversation.id",
            "hutan.session",
            "code.file.path",
            "db.statement",
            "url.full",
        ] {
            assert!(!attribute(name), "{name} must not be readable");
        }
    }

    #[test]
    fn the_names_the_mappers_read_are_listed() {
        for name in ["service.name", "input_tokens", "gen_ai.usage.input_tokens"] {
            assert!(attribute(name), "{name}");
        }
    }
}
