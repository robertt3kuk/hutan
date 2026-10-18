//! Titles an agent keeps outside its transcripts. Codex names a thread lazily in
//! `<codex home>/session_index.jsonl` (`{"id": …, "thread_name": …}`, appended when the thread
//! is named or renamed); the title outranks the thread's first prompt in the session catalog.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use serde_json::Value;

use crate::summary::clean_title;

/// The file next to Codex's `sessions` directory.
pub const CODEX_TITLES_FILE: &str = "session_index.jsonl";
/// Most bytes read from the file; a longer file keeps its first part.
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// A longer line is skipped.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// `(thread id, title)` for every named thread; a later line for the same id wins. `None` when
/// the file cannot be read.
pub fn read_codex_titles(path: &Path) -> Option<Vec<(String, String)>> {
    let file = File::open(path).ok()?;
    let mut titles: HashMap<String, String> = HashMap::new();
    for line in BufReader::new(file.take(MAX_FILE_BYTES)).lines() {
        let Ok(line) = line else { break };
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        let Ok(Value::Object(record)) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (Some(id), Some(name)) = (
            record.get("id").and_then(Value::as_str),
            record.get("thread_name").and_then(Value::as_str),
        ) else {
            continue;
        };
        if let Some(title) = clean_title(name) {
            titles.insert(id.to_owned(), title);
        }
    }
    Some(titles.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_name_of_a_thread_wins_and_junk_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join(CODEX_TITLES_FILE);
        std::fs::write(
            &file,
            [
                r#"{"id":"a","thread_name":"First name","updated_at":"2026-09-01T00:00:00Z"}"#,
                "not json",
                r#"{"id":"b","thread_name":"  Other\nthread "}"#,
                r#"{"id":"a","thread_name":"Renamed"}"#,
                r#"{"id":"c","thread_name":"   "}"#,
                r#"{"id":"d"}"#,
            ]
            .join("\n"),
        )
        .expect("write");
        let mut titles = read_codex_titles(&file).expect("titles");
        titles.sort();
        assert_eq!(
            titles,
            vec![
                ("a".to_owned(), "Renamed".to_owned()),
                ("b".to_owned(), "Other thread".to_owned()),
            ]
        );
        assert_eq!(read_codex_titles(&dir.path().join("missing.jsonl")), None);
    }
}
