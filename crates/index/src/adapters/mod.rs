//! Transcript adapters: each knows one agent's JSONL shape and turns a line into index
//! [`Entry`]s. Adapters are pure: no I/O, no clock. The scanner owns offsets and files.
//!
//! Every adapter has a `format_version`; bumping it re-indexes the files of that source from
//! scratch the next time they are looked at (`docs/history-index.md`).

mod claude;
mod codex;
mod omp;

use std::fmt::Debug;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::summary::Summary;
use crate::types::{Entry, Role, Source};

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use omp::OmpAdapter;

/// What survives between two reads of one file (persisted in the `sources` table).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseState {
    /// The agent's own session id.
    pub key: String,
    pub cwd: Option<String>,
    pub title: Option<String>,
    /// Tool-role rows indexed so far; capped per session ([`MAX_TOOL_ROWS`]).
    pub tool_rows: u32,
    /// What the session catalog shows of the session, folded along with the rows.
    #[serde(default)]
    pub summary: Summary,
}

/// Tool-role rows indexed per session; everything after is dropped.
pub const MAX_TOOL_ROWS: u32 = 2_000;
/// Chunks indexed per tool call or result.
pub const MAX_TOOL_CHUNKS: usize = 4;
/// Chunks indexed per user or assistant message.
pub const MAX_MESSAGE_CHUNKS: usize = 16;

/// The result of reading one line.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Nothing to index (bookkeeping lines, thinking, duplicates).
    Skip,
    Entries(Vec<Entry>),
    /// The line is not valid JSON (or not the expected shape); counted, then skipped.
    Malformed,
}

pub trait Adapter: Debug + Send + Sync {
    fn source(&self) -> Source;

    /// Bumped whenever this adapter would index a file differently.
    fn format_version(&self) -> u32;

    /// Whether `path` (below the source root) is a transcript this adapter reads.
    fn accepts(&self, path: &Path) -> bool {
        path.extension().is_some_and(|ext| ext == "jsonl")
    }

    /// Whether the transcript at `path` belongs to a sub-agent of another session (the catalog
    /// lists the parent, not its helpers).
    fn is_subagent(&self, _path: &Path) -> bool {
        false
    }

    /// Session id when the file does not say (and until it does).
    fn key_from_path(&self, path: &Path) -> String;

    /// Reads one line; may update `state` (cwd, title, session id).
    fn parse_line(&self, line: &str, state: &mut ParseState) -> Parsed;

    /// The first line of a file changes in place (OMP's fixed-width title line): look at it
    /// again whenever the file changes.
    fn rereads_head(&self) -> bool {
        false
    }
}

/// The adapter for `source`; `None` for [`Source::Hutan`], which has no files.
pub fn adapter_for(source: Source) -> Option<Box<dyn Adapter>> {
    match source {
        Source::Hutan => None,
        Source::Claude => Some(Box::new(ClaudeAdapter)),
        Source::Codex => Some(Box::new(CodexAdapter)),
        Source::Omp => Some(Box::new(OmpAdapter)),
    }
}

// ---- helpers shared by the adapters ------------------------------------------------------------

/// Folds the conversation messages among `entries` (one line of a transcript) into the
/// session summary: the text parts of one role are one message. Tool rows are not messages.
/// `ts` also moves the session's first and last time.
/// Entries for which `skip` is true are not part of the conversation (injected context).
pub(crate) fn fold_messages(
    state: &mut ParseState,
    ts: Option<u64>,
    entries: &[Entry],
    skip: impl Fn(&Entry) -> bool,
) {
    state.summary.note_time(ts);
    for role in [Role::User, Role::Assistant] {
        let mut text = String::new();
        for entry in entries.iter().filter(|e| e.role == role) {
            if entry.text.trim().is_empty() || skip(entry) {
                continue;
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&entry.text);
        }
        if !text.is_empty() {
            state.summary.note_message(role, ts, &text);
        }
    }
}

/// Claude Code and its forks inject machinery into the conversation as user turns (task
/// notifications, slash-command envelopes, local-command output, interruption and compaction
/// notices). They are not something the user said: the catalog neither counts nor shows them.
/// Only tags seen from harnesses count; a prompt that happens to start with another `<tag>` is
/// a real prompt.
pub(crate) fn is_harness_injected(text: &str) -> bool {
    const TAGS: &[&str] = &[
        "agent-message",
        "bash-input",
        "bash-stderr",
        "bash-stdout",
        "command-args",
        "command-message",
        "command-name",
        "cross-session-message",
        "fork-boilerplate",
        "local-command-caveat",
        "local-command-stderr",
        "local-command-stdout",
        "mcp-polling-update",
        "mcp-resource-update",
        "system-reminder",
        "task-notification",
        "teammate-message",
        "user-memory-input",
        "user-prompt-submit-hook",
    ];
    const PREFIXES: &[&str] = &[
        "<channel source=",
        "[request interrupted",
        "a message arrived from ",
        "another claude session sent a message",
        "no response requested.",
        "caveat: the messages below were generated by the user while running local commands",
        "this session is being continued from a previous conversation",
    ];
    let head: String = text
        .trim_start()
        .chars()
        .take(256)
        .collect::<String>()
        .to_lowercase();
    if PREFIXES.iter().any(|prefix| head.starts_with(prefix)) {
        return true;
    }
    let Some(rest) = head.strip_prefix('<') else {
        return false;
    };
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    let after = &rest[name.len()..];
    TAGS.contains(&name.as_str()) && (after.is_empty() || after.starts_with(['>', ' ', '\t', '\n']))
}

/// Text of a message `content`: a string, or the `text` of every part that has one.
pub(crate) fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.as_str()),
                _ => part.get("text").and_then(Value::as_str),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `8-4-4-4-12` hexadecimal, as in session ids and Codex/Claude file names.
pub(crate) fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Keys of a tool call's arguments worth indexing, in display order. Everything else (file
/// contents, patches, long edit bodies) is left out.
const ARGUMENT_KEYS: &[&str] = &[
    "command",
    "cmd",
    "file_path",
    "path",
    "pattern",
    "query",
    "url",
    "description",
    "intent",
    "prompt",
    "i",
    "subagent_type",
];

/// One line per interesting argument, `key: value`, or compact JSON (bounded) when the
/// arguments have none of the known keys. `arguments` may be a JSON object or a string that
/// holds one.
pub(crate) fn summarize_arguments(arguments: &Value) -> String {
    let parsed;
    let object = match arguments {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                parsed = value;
                &parsed
            }
            Err(_) => return truncate(text, 1024).to_owned(),
        },
        other => other,
    };
    let Some(map) = object.as_object() else {
        return truncate(&object.to_string(), 1024).to_owned();
    };
    let mut lines = Vec::new();
    for key in ARGUMENT_KEYS {
        let Some(value) = map.get(*key) else { continue };
        let text = match value {
            Value::String(text) => text.clone(),
            Value::Array(items) => items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            _ => continue,
        };
        if !text.trim().is_empty() {
            lines.push(format!("{key}: {}", truncate(&text, 2048)));
        }
    }
    if lines.is_empty() {
        return truncate(&object.to_string(), 1024).to_owned();
    }
    lines.join("\n")
}

pub(crate) fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The first non-empty line of `text`, at most 80 characters: a session title when the agent
/// gave none.
pub(crate) fn title_from(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(line.chars().take(80).collect())
}

/// Unix milliseconds of an RFC 3339 timestamp (`2026-09-28T05:01:22.983Z`, offsets allowed).
pub(crate) fn parse_rfc3339_ms(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't' | b' ')
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { text.get(from..to)?.parse().ok() };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut index = 19;
    let mut millis = 0_i64;
    if bytes.get(index) == Some(&b'.') {
        let digits = text[index + 1..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let fraction = text.get(index + 1..index + 1 + digits.min(3))?;
        millis =
            fraction.parse::<i64>().ok()? * 10_i64.pow(3 - u32::try_from(fraction.len()).ok()?);
        index += 1 + digits;
    }
    let offset_minutes = match bytes.get(index)? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let hours = num(index + 1, index + 3)?;
            let minutes = num(index + 4, index + 6).unwrap_or(0);
            let total = hours * 60 + minutes;
            if *sign == b'+' { total } else { -total }
        }
        _ => return None,
    };
    // Days since 1970-01-01 (Howard Hinnant's civil-from-days, inverted).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    u64::try_from(seconds * 1000 + millis).ok()
}

pub(crate) fn timestamp(value: &Value) -> Option<u64> {
    match value {
        Value::String(text) => parse_rfc3339_ms(text),
        Value::Number(number) => number.as_u64(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_timestamps_parse() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2026-09-28T05:01:22.983Z"),
            Some(1_790_571_682_983)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-09-28T07:01:22+02:00"),
            Some(1_790_571_682_000)
        );
        assert_eq!(
            parse_rfc3339_ms("2024-02-29T12:00:00.5Z"),
            Some(1_709_208_000_500)
        );
        assert_eq!(parse_rfc3339_ms("yesterday"), None);
        assert_eq!(parse_rfc3339_ms("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn arguments_keep_only_the_interesting_keys() {
        let args =
            serde_json::json!({"command": "cargo test -p daemon", "content": "x".repeat(10_000)});
        assert_eq!(summarize_arguments(&args), "command: cargo test -p daemon");
        let as_string = Value::String(r#"{"cmd":["git","status"],"workdir":"/x"}"#.into());
        assert_eq!(summarize_arguments(&as_string), "cmd: git status");
        let other = serde_json::json!({"weird": 1});
        assert_eq!(summarize_arguments(&other), r#"{"weird":1}"#);
    }

    #[test]
    fn only_observed_harness_machinery_counts_as_injected() {
        for injected in [
            "<local-command-stdout>ok</local-command-stdout>",
            "  <system-reminder>\nremember</system-reminder>",
            "<command-name>/clear</command-name>",
            "<task-notification>done</task-notification>",
            "[Request interrupted by user]",
            "This session is being continued from a previous conversation that ran out",
            "<channel source=\"x\">hi</channel>",
        ] {
            assert!(is_harness_injected(injected), "{injected}");
        }
        for prompt in [
            "fix the build",
            "<my-element>real code</my-element> why does this fail",
            "<system-reminders>not a tag we know</system-reminders>",
            "a message from the user: hello",
            "",
        ] {
            assert!(!is_harness_injected(prompt), "{prompt}");
        }
    }

    #[test]
    fn the_text_parts_of_one_role_are_one_message_and_tool_rows_are_none() {
        let mut state = ParseState::default();
        let entries = vec![
            Entry::new(Role::Assistant, Some(10), "first part"),
            Entry::new(Role::Tool, Some(10), "Bash\nls"),
            Entry::new(Role::Assistant, Some(10), "second part"),
            Entry::new(Role::User, Some(10), "   "),
        ];
        fold_messages(&mut state, Some(10), &entries, |_| false);
        assert_eq!(state.summary.messages, 1);
        assert_eq!(state.summary.latest[0].text, "first part second part");
        assert_eq!(
            (state.summary.created_ms, state.summary.updated_ms),
            (Some(10), Some(10))
        );

        let injected = vec![Entry::new(
            Role::User,
            None,
            "<system-reminder>x</system-reminder>",
        )];
        fold_messages(&mut state, None, &injected, |e| {
            is_harness_injected(&e.text)
        });
        assert_eq!(
            state.summary.messages, 1,
            "skipped entries are not messages"
        );
    }

    #[test]
    fn titles_come_from_the_first_line() {
        assert_eq!(
            title_from("\n  fix the build  \nmore").as_deref(),
            Some("fix the build")
        );
        assert_eq!(title_from(" \n"), None);
        assert_eq!(title_from(&"x".repeat(200)).map(|t| t.len()), Some(80));
    }
}
