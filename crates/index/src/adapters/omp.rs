//! OMP: `~/.omp/agent/sessions/<cwd-slug>/<timestamp>_<id>.jsonl`.
//!
//! The first line is `{"type":"title","title":…,"pad":…}`, a fixed-width record rewritten in
//! place when the title changes (so the file changes without growing at the end: the scanner
//! reads that line again, see [`Adapter::rereads_head`]). The second is `session` with the `id`
//! and `cwd`. `message` lines carry `message.role` (`user`, `assistant`, `toolResult`,
//! `developer`) and `content` parts: `text`, `thinking`, and `toolCall` (`name`,
//! `arguments`). `title_change` lines update the title. Resume: `omp --resume=<id>`.
//!
//! Format version 1.

use std::path::Path;

use serde_json::Value;

use super::{
    Adapter, ParseState, Parsed, content_text, fold_messages, summarize_arguments, timestamp,
    title_from,
};
use crate::types::{Entry, Role, Source};

#[derive(Debug, Clone, Copy, Default)]
pub struct OmpAdapter;

impl Adapter for OmpAdapter {
    fn source(&self) -> Source {
        Source::Omp
    }

    fn format_version(&self) -> u32 {
        1
    }

    /// A transcript in a directory named like its parent's file (`<timestamp>_<id>/<Name>.jsonl`).
    fn is_subagent(&self, path: &Path) -> bool {
        parent_session(path).is_some()
    }

    /// `2026-09-28T05-01-22-983Z_<id>.jsonl`: everything after the first underscore. A sub-agent
    /// transcript lives in a directory named like its parent's file
    /// (`<timestamp>_<id>/<Name>.jsonl`) and takes the parent's id, so that resuming the hit
    /// resumes the conversation it belongs to.
    fn key_from_path(&self, path: &Path) -> String {
        if let Some(id) = parent_session(path) {
            return id;
        }
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        id_of(&stem).unwrap_or(stem)
    }

    fn rereads_head(&self) -> bool {
        true
    }

    fn parse_line(&self, line: &str, state: &mut ParseState) -> Parsed {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return Parsed::Malformed;
        };
        let Some(object) = value.as_object() else {
            return Parsed::Malformed;
        };
        let ts = object.get("timestamp").and_then(timestamp);
        state.summary.note_time(ts);
        match object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "title" | "title_change" => {
                if let Some(title) = object.get("title").and_then(Value::as_str)
                    && !title.trim().is_empty()
                {
                    state.title = Some(title.trim().to_owned());
                    state.summary.set_generated_title(title);
                }
                Parsed::Skip
            }
            "model_change" => {
                // Pi writes `modelId`, OMP `model`.
                let model = object
                    .get("modelId")
                    .and_then(Value::as_str)
                    .or_else(|| object.get("model").and_then(Value::as_str));
                if let Some(model) = model {
                    state.summary.set_model(model);
                }
                Parsed::Skip
            }
            "session" => {
                // A sub-agent has an id of its own; its hits resume the parent (the key comes
                // from the path).
                if object.get("parentSession").is_some() {
                    state.summary.subagent = true;
                } else if let Some(id) = object.get("id").and_then(Value::as_str) {
                    state.key = id.to_owned();
                }
                if let Some(cwd) = object.get("cwd").and_then(Value::as_str) {
                    state.cwd = Some(cwd.to_owned());
                }
                Parsed::Skip
            }
            "message" => {
                let Some(message) = object.get("message") else {
                    return Parsed::Malformed;
                };
                let parsed = message_entries(message, ts, state);
                if let Parsed::Entries(entries) = &parsed {
                    fold_messages(state, ts, entries, |_| false);
                }
                parsed
            }
            _ => Parsed::Skip,
        }
    }
}

/// The id in a name like `2026-09-28T05-01-22-983Z_<id>`.
fn id_of(name: &str) -> Option<String> {
    let (stamp, id) = name.split_once('_')?;
    let looks_like_time = stamp.len() >= 10 && stamp.as_bytes()[4] == b'-' && stamp.contains('T');
    (looks_like_time && !id.is_empty()).then(|| id.to_owned())
}

/// The id of the top-level session whose directory holds the transcript at `path`: the
/// outermost directory named like a session file.
fn parent_session(path: &Path) -> Option<String> {
    path.parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .filter_map(|dir| dir.file_name())
        .filter_map(|name| id_of(&name.to_string_lossy()))
        .last()
}

fn message_entries(message: &Value, ts: Option<u64>, state: &mut ParseState) -> Parsed {
    let content = message.get("content");
    match message.get("role").and_then(Value::as_str) {
        Some("user") => {
            let text = content.map(content_text).unwrap_or_default();
            if state.title.is_none() {
                state.title = title_from(&text);
            }
            Parsed::Entries(vec![Entry::new(Role::User, ts, text)])
        }
        Some("assistant") => {
            if let Some(model) = message.get("model").and_then(Value::as_str) {
                state.summary.set_model(model);
            }
            let mut entries = Vec::new();
            match content {
                Some(Value::Array(parts)) => {
                    for part in parts {
                        match part.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(text) = part.get("text").and_then(Value::as_str) {
                                    entries.push(Entry::new(Role::Assistant, ts, text));
                                }
                            }
                            Some("toolCall") => {
                                let name =
                                    part.get("name").and_then(Value::as_str).unwrap_or("tool");
                                let arguments = part
                                    .get("arguments")
                                    .map(summarize_arguments)
                                    .unwrap_or_default();
                                entries.push(Entry::new(
                                    Role::Tool,
                                    ts,
                                    format!("{name}\n{arguments}"),
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                Some(Value::String(text)) => {
                    entries.push(Entry::new(Role::Assistant, ts, text.clone()))
                }
                _ => {}
            }
            Parsed::Entries(entries)
        }
        Some("toolResult") => {
            let name = message
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let body = content.map(content_text).unwrap_or_default();
            Parsed::Entries(vec![Entry::new(Role::Tool, ts, format!("{name}\n{body}"))])
        }
        _ => Parsed::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_session_and_titles() {
        let mut state = ParseState::default();
        let title = OmpAdapter.parse_line(
            r#"{"type":"title","v":1,"title":"Refactor storage","updatedAt":"2026-09-28T05:02:00.000Z","pad":"          "}"#,
            &mut state,
        );
        assert_eq!(title, Parsed::Skip);
        assert_eq!(state.title.as_deref(), Some("Refactor storage"));
        OmpAdapter.parse_line(
            r#"{"type":"session","version":3,"id":"0f4c7d52","timestamp":"2026-09-28T05:01:22.983Z","cwd":"/work/meroving"}"#,
            &mut state,
        );
        assert_eq!(
            (state.key.as_str(), state.cwd.as_deref()),
            ("0f4c7d52", Some("/work/meroving"))
        );
        OmpAdapter.parse_line(
            r#"{"type":"title_change","title":"New name","source":"user"}"#,
            &mut state,
        );
        assert_eq!(state.title.as_deref(), Some("New name"));
        // An empty title (not generated yet) does not erase anything.
        OmpAdapter.parse_line(r#"{"type":"title","v":1,"title":"","pad":""}"#, &mut state);
        assert_eq!(state.title.as_deref(), Some("New name"));
    }

    #[test]
    fn messages_tool_calls_and_results() {
        let mut state = ParseState::default();
        let user = OmpAdapter.parse_line(
            r#"{"type":"message","timestamp":"2026-09-28T05:01:23.000Z","message":{"role":"user","content":[{"type":"text","text":"list the tests"}]}}"#,
            &mut state,
        );
        assert_eq!(
            user,
            Parsed::Entries(vec![Entry::new(
                Role::User,
                Some(1_790_571_683_000),
                "list the tests"
            )])
        );
        let assistant = OmpAdapter.parse_line(
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"Looking."},{"type":"toolCall","id":"c","name":"bash","arguments":{"command":"cargo test","i":"run"}}]}}"#,
            &mut state,
        );
        assert_eq!(
            assistant,
            Parsed::Entries(vec![
                Entry::new(Role::Assistant, None, "Looking."),
                Entry::new(Role::Tool, None, "bash\ncommand: cargo test\ni: run"),
            ])
        );
        let result = OmpAdapter.parse_line(
            r#"{"type":"message","message":{"role":"toolResult","toolName":"bash","content":[{"type":"text","text":"3 passed"}]}}"#,
            &mut state,
        );
        assert_eq!(
            result,
            Parsed::Entries(vec![Entry::new(Role::Tool, None, "bash\n3 passed")])
        );
        for skipped in [
            r#"{"type":"message","message":{"role":"developer","content":[{"type":"text","text":"x"}]}}"#,
            r#"{"type":"custom","customType":"tool-start"}"#,
            r#"{"type":"model_change","model":"m"}"#,
        ] {
            assert_eq!(OmpAdapter.parse_line(skipped, &mut state), Parsed::Skip);
        }
        assert_eq!(OmpAdapter.parse_line("{", &mut state), Parsed::Malformed);
    }

    #[test]
    fn the_key_follows_the_underscore() {
        assert_eq!(
            OmpAdapter.key_from_path(Path::new(
                "/r/-work-x/2026-09-28T05-01-22-983Z_0f4c7d52.jsonl"
            )),
            "0f4c7d52"
        );
    }
}
