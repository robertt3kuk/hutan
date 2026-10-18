//! Claude Code: `~/.claude/projects/<cwd-slug>/<session-id>.jsonl`.
//!
//! One JSON object per line. `user` and `assistant` lines carry `message.content` (a string or
//! parts: `text`, `tool_use`, `tool_result`, `thinking`), plus `sessionId`, `cwd`, `timestamp`.
//! `ai-title` lines carry the generated title. Resume: `claude --resume <session id>`; the
//! session id is the file name.
//!
//! Format version 1.

use std::path::Path;

use serde_json::Value;

use super::{
    Adapter, ParseState, Parsed, content_text, fold_messages, is_harness_injected, is_uuid,
    summarize_arguments, timestamp, title_from,
};
use crate::types::{Entry, Role, Source};

#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeAdapter;

impl Adapter for ClaudeAdapter {
    fn source(&self) -> Source {
        Source::Claude
    }

    fn format_version(&self) -> u32 {
        1
    }

    /// A transcript below a session directory (`<session>/subagents/agent-x.jsonl`).
    fn is_subagent(&self, path: &Path) -> bool {
        session_dir(path).is_some()
    }

    /// The file name; for a sub-agent transcript below a session directory
    /// (`<session>/subagents/agent-x.jsonl`) the id of that session, so that resuming the hit
    /// resumes the conversation it belongs to.
    fn key_from_path(&self, path: &Path) -> String {
        match session_dir(path) {
            Some(id) => id,
            None => path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    fn parse_line(&self, line: &str, state: &mut ParseState) -> Parsed {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return Parsed::Malformed;
        };
        let Some(object) = value.as_object() else {
            return Parsed::Malformed;
        };
        if let Some(cwd) = object.get("cwd").and_then(Value::as_str) {
            state.cwd = Some(cwd.to_owned());
        }
        if let Some(branch) = object.get("gitBranch").and_then(Value::as_str) {
            state.summary.set_branch(branch);
        }
        let ts = object.get("timestamp").and_then(timestamp);
        state.summary.note_time(ts);
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            // A title the user gave beats the generated one, which beats the first prompt.
            "custom-title" => {
                if let Some(title) = object.get("customTitle").and_then(Value::as_str) {
                    state.summary.set_custom_title(title);
                }
                Parsed::Skip
            }
            "ai-title" => {
                if let Some(title) = object.get("aiTitle").and_then(Value::as_str)
                    && !title.trim().is_empty()
                {
                    state.title = Some(title.trim().to_owned());
                    state.summary.set_generated_title(title);
                }
                Parsed::Skip
            }
            "user" | "assistant" => {
                if object.get("isMeta").and_then(Value::as_bool) == Some(true) {
                    return Parsed::Skip;
                }
                let Some(message) = object.get("message") else {
                    return Parsed::Malformed;
                };
                let role = if kind == "user" {
                    Role::User
                } else {
                    Role::Assistant
                };
                if role == Role::Assistant
                    && let Some(model) = message.get("model").and_then(Value::as_str)
                    && !model.starts_with('<')
                {
                    state.summary.set_model(model);
                }
                let entries = message_entries(role, message.get("content"), ts, state);
                fold_messages(state, ts, &entries, |entry| {
                    entry.role == Role::User && is_harness_injected(&entry.text)
                });
                Parsed::Entries(entries)
            }
            _ => Parsed::Skip,
        }
    }
}

/// The session directory (a UUID) a sub-agent transcript lives below, if it does.
fn session_dir(path: &Path) -> Option<String> {
    path.parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .filter_map(|dir| dir.file_name())
        .map(|name| name.to_string_lossy())
        .find(|name| is_uuid(name))
        .map(|name| name.into_owned())
}

fn message_entries(
    role: Role,
    content: Option<&Value>,
    ts: Option<u64>,
    state: &mut ParseState,
) -> Vec<Entry> {
    let mut entries = Vec::new();
    let text = |role: Role, text: String, state: &mut ParseState, entries: &mut Vec<Entry>| {
        if role == Role::User && state.title.is_none() {
            state.title = title_from(&text);
        }
        entries.push(Entry::new(role, ts, text));
    };
    match content {
        Some(Value::String(body)) => text(role, body.clone(), state, &mut entries),
        Some(Value::Array(parts)) => {
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(body) = part.get("text").and_then(Value::as_str) {
                            text(role, body.to_owned(), state, &mut entries);
                        }
                    }
                    Some("tool_use") => {
                        let name = part.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let input = part
                            .get("input")
                            .map(summarize_arguments)
                            .unwrap_or_default();
                        entries.push(Entry::new(Role::Tool, ts, format!("{name}\n{input}")));
                    }
                    Some("tool_result") => {
                        let body = part.get("content").map(content_text).unwrap_or_default();
                        entries.push(Entry::new(Role::Tool, ts, body));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> (Parsed, ParseState) {
        let mut state = ParseState::default();
        let parsed = ClaudeAdapter.parse_line(line, &mut state);
        (parsed, state)
    }

    #[test]
    fn user_text_parts_and_string_content() {
        let (parsed, state) = parse(
            r#"{"type":"user","timestamp":"2026-09-28T05:01:22.983Z","cwd":"/work/app","sessionId":"s1","message":{"role":"user","content":[{"type":"text","text":"fix the flaky test"}]}}"#,
        );
        assert_eq!(
            parsed,
            Parsed::Entries(vec![Entry::new(
                Role::User,
                Some(1_790_571_682_983),
                "fix the flaky test"
            )])
        );
        assert_eq!(state.cwd.as_deref(), Some("/work/app"));
        assert_eq!(state.title.as_deref(), Some("fix the flaky test"));

        let (parsed, _) = parse(r#"{"type":"user","message":{"role":"user","content":"plain"}}"#);
        assert_eq!(
            parsed,
            Parsed::Entries(vec![Entry::new(Role::User, None, "plain")])
        );
    }

    #[test]
    fn assistant_tool_use_and_results_are_tool_rows() {
        let (parsed, _) = parse(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"Running it."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test -p daemon","description":"run tests"}}]}}"#,
        );
        let Parsed::Entries(entries) = parsed else {
            panic!("{parsed:?}")
        };
        assert_eq!(entries.len(), 2, "thinking is not indexed");
        assert_eq!(entries[0].role, Role::Assistant);
        assert_eq!(entries[1].role, Role::Tool);
        assert_eq!(
            entries[1].text,
            "Bash\ncommand: cargo test -p daemon\ndescription: run tests"
        );

        let (parsed, _) = parse(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"test result: ok. 12 passed"}]}}"#,
        );
        assert_eq!(
            parsed,
            Parsed::Entries(vec![Entry::new(
                Role::Tool,
                None,
                "test result: ok. 12 passed"
            )])
        );
        let (parsed, _) = parse(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}]}}"#,
        );
        assert_eq!(
            parsed,
            Parsed::Entries(vec![Entry::new(Role::Tool, None, "a\nb")])
        );
    }

    #[test]
    fn titles_bookkeeping_and_garbage() {
        let (parsed, state) =
            parse(r#"{"type":"ai-title","aiTitle":"Fix flaky test","sessionId":"s1"}"#);
        assert_eq!(parsed, Parsed::Skip);
        assert_eq!(state.title.as_deref(), Some("Fix flaky test"));
        assert_eq!(
            parse(r#"{"type":"queue-operation","operation":"enqueue"}"#).0,
            Parsed::Skip
        );
        assert_eq!(
            parse(
                r#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>"}}"#
            )
            .0,
            Parsed::Skip
        );
        assert_eq!(parse("{not json").0, Parsed::Malformed);
        assert_eq!(parse("[1,2]").0, Parsed::Malformed);
        assert_eq!(parse(r#"{"type":"user"}"#).0, Parsed::Malformed);
    }

    #[test]
    fn the_session_key_is_the_file_name_or_the_parent_session_of_a_subagent() {
        assert_eq!(
            ClaudeAdapter.key_from_path(Path::new("/r/-Users-me-app/5f3e1c1a-8d51.jsonl")),
            "5f3e1c1a-8d51"
        );
        assert_eq!(
            ClaudeAdapter.key_from_path(Path::new(
                "/r/-Users-me-app/5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60/subagents/agent-a1b2.jsonl"
            )),
            "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60"
        );
    }
}
