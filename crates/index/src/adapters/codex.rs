//! Codex: `~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl`.
//!
//! Lines are `{timestamp, type, payload}`. `session_meta` carries the session `id` and `cwd`;
//! `turn_context` the current `cwd`; `response_item` payloads are `message` (role user,
//! assistant, developer), `function_call` / `function_call_output`, and
//! `custom_tool_call` / `custom_tool_call_output`. `event_msg` repeats messages as events and
//! is ignored. Resume: `codex resume <id>`. `~/.codex/*.sqlite` is not read.
//!
//! Format version 1.

use std::path::Path;

use serde_json::Value;

use super::{
    Adapter, ParseState, Parsed, content_text, fold_messages, is_uuid, summarize_arguments,
    timestamp, title_from,
};
use crate::types::{Entry, Role, Source};

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexAdapter;

impl Adapter for CodexAdapter {
    fn source(&self) -> Source {
        Source::Codex
    }

    fn format_version(&self) -> u32 {
        1
    }

    /// `rollout-2026-04-16T10-11-12-<uuid>.jsonl`: the id is the last five dash-separated
    /// groups of a UUID.
    fn key_from_path(&self, path: &Path) -> String {
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let groups: Vec<&str> = stem.split('-').collect();
        if groups.len() >= 5 {
            let tail = groups[groups.len() - 5..].join("-");
            if is_uuid(&tail) {
                return tail;
            }
        }
        stem
    }

    fn parse_line(&self, line: &str, state: &mut ParseState) -> Parsed {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return Parsed::Malformed;
        };
        let Some(object) = value.as_object() else {
            return Parsed::Malformed;
        };
        let ts = object.get("timestamp").and_then(timestamp);
        let payload = object.get("payload");
        let field = |name: &str| payload.and_then(|p| p.get(name));
        state.summary.note_time(ts);
        match object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "session_meta" => {
                if let Some(id) = field("id").and_then(Value::as_str) {
                    state.key = id.to_owned();
                }
                if let Some(cwd) = field("cwd").and_then(Value::as_str) {
                    state.cwd = Some(cwd.to_owned());
                }
                if let Some(branch) = field("git")
                    .and_then(|git| git.get("branch"))
                    .and_then(Value::as_str)
                {
                    state.summary.set_branch(branch);
                }
                if let Some(name) = field("thread_name").and_then(Value::as_str) {
                    state.summary.set_generated_title(name);
                }
                // A thread started by another thread (review, compaction worker, …) is a
                // helper, not a session of its own.
                if field("source").is_some_and(|source| source.get("subagent").is_some()) {
                    state.summary.subagent = true;
                }
                Parsed::Skip
            }
            "turn_context" => {
                if let Some(cwd) = field("cwd").and_then(Value::as_str) {
                    state.cwd = Some(cwd.to_owned());
                }
                if let Some(model) = field("model").and_then(Value::as_str) {
                    state.summary.set_model(model);
                }
                Parsed::Skip
            }
            "response_item" => {
                let Some(payload) = payload else {
                    return Parsed::Malformed;
                };
                let parsed = response_item(payload, ts, state);
                if let Parsed::Entries(entries) = &parsed {
                    fold_messages(state, ts, entries, |_| false);
                }
                parsed
            }
            _ => Parsed::Skip,
        }
    }
}

fn response_item(payload: &Value, ts: Option<u64>, state: &mut ParseState) -> Parsed {
    let field = |name: &str| payload.get(name);
    let entry = |role, text: String| Parsed::Entries(vec![Entry::new(role, ts, text)]);
    match field("type").and_then(Value::as_str).unwrap_or_default() {
        "message" => {
            let role = match field("role").and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => return Parsed::Skip,
            };
            let text = field("content").map(content_text).unwrap_or_default();
            if role == Role::User {
                if is_injected_context(&text) {
                    return Parsed::Skip;
                }
                if state.title.is_none() {
                    state.title = title_from(&text);
                }
            }
            entry(role, text)
        }
        "function_call" => {
            let name = field("name").and_then(Value::as_str).unwrap_or("tool");
            let arguments = field("arguments")
                .map(summarize_arguments)
                .unwrap_or_default();
            entry(Role::Tool, format!("{name}\n{arguments}"))
        }
        "custom_tool_call" => {
            let name = field("name").and_then(Value::as_str).unwrap_or("tool");
            let input = field("input").map(summarize_arguments).unwrap_or_default();
            entry(Role::Tool, format!("{name}\n{input}"))
        }
        "function_call_output" | "custom_tool_call_output" => {
            let output = match field("output") {
                Some(Value::String(text)) => unwrap_output(text),
                Some(other) => content_text(other),
                None => String::new(),
            };
            entry(Role::Tool, output)
        }
        _ => Parsed::Skip,
    }
}

/// Tool outputs are sometimes a JSON object with an `output` string; use that string.
fn unwrap_output(text: &str) -> String {
    if text.starts_with('{')
        && let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text)
        && let Some(Value::String(output)) = map.get("output")
    {
        return output.clone();
    }
    text.to_owned()
}

/// Codex sends the environment and instructions as user messages wrapped in a tag
/// (`<environment_context>…</environment_context>`); they are not something the user said.
fn is_injected_context(text: &str) -> bool {
    let text = text.trim();
    let Some(rest) = text.strip_prefix('<') else {
        return false;
    };
    let Some((tag, _)) = rest.split_once('>') else {
        return false;
    };
    !tag.is_empty()
        && tag.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        && text.ends_with(&format!("</{tag}>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str, state: &mut ParseState) -> Parsed {
        CodexAdapter.parse_line(line, state)
    }

    #[test]
    fn session_meta_sets_id_and_cwd() {
        let mut state = ParseState::default();
        let parsed = parse(
            r#"{"timestamp":"2026-04-16T10:00:00.000Z","type":"session_meta","payload":{"id":"0198aaaa-bbbb-cccc-dddd-eeeeffff0000","cwd":"/work/app"}}"#,
            &mut state,
        );
        assert_eq!(parsed, Parsed::Skip);
        assert_eq!(state.key, "0198aaaa-bbbb-cccc-dddd-eeeeffff0000");
        assert_eq!(state.cwd.as_deref(), Some("/work/app"));
        parse(
            r#"{"type":"turn_context","payload":{"cwd":"/work/app/sub"}}"#,
            &mut state,
        );
        assert_eq!(state.cwd.as_deref(), Some("/work/app/sub"));
    }

    #[test]
    fn messages_and_tool_calls() {
        let mut state = ParseState::default();
        let user = parse(
            r#"{"timestamp":"2026-04-16T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add a retry to the client"}]}}"#,
            &mut state,
        );
        assert_eq!(
            user,
            Parsed::Entries(vec![Entry::new(
                Role::User,
                Some(1_776_333_601_000),
                "add a retry to the client"
            )])
        );
        assert_eq!(state.title.as_deref(), Some("add a retry to the client"));

        let call = parse(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"cargo test\"],\"workdir\":\"/w\"}","call_id":"c1"}}"#,
            &mut state,
        );
        assert_eq!(
            call,
            Parsed::Entries(vec![Entry::new(
                Role::Tool,
                None,
                "shell\ncommand: bash -lc cargo test"
            )])
        );
        let output = parse(
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"{\"output\":\"ok\",\"metadata\":{}}"}}"#,
            &mut state,
        );
        assert_eq!(
            output,
            Parsed::Entries(vec![Entry::new(Role::Tool, None, "ok")])
        );
        let assistant = parse(
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}"#,
            &mut state,
        );
        assert_eq!(
            assistant,
            Parsed::Entries(vec![Entry::new(Role::Assistant, None, "Done.")])
        );
    }

    #[test]
    fn injected_context_developer_messages_and_events_are_skipped() {
        let mut state = ParseState::default();
        for line in [
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/w</cwd>\n</environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"be nice"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#,
            r#"{"type":"token_usage_record"}"#,
        ] {
            assert_eq!(parse(line, &mut state), Parsed::Skip, "{line}");
        }
        assert_eq!(state.title, None);
        assert_eq!(parse("nope", &mut state), Parsed::Malformed);
    }

    #[test]
    fn the_key_is_the_uuid_at_the_end_of_the_file_name() {
        assert_eq!(
            CodexAdapter.key_from_path(Path::new(
                "/r/2026/04/16/rollout-2026-04-16T10-11-12-0198aaaa-bbbb-cccc-dddd-eeeeffff0000.jsonl"
            )),
            "0198aaaa-bbbb-cccc-dddd-eeeeffff0000"
        );
        assert_eq!(CodexAdapter.key_from_path(Path::new("/r/odd.jsonl")), "odd");
    }
}
