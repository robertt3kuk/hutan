//! Adapter + scanner behaviour on synthetic transcripts: incremental reads, rewrites,
//! malformed lines, partial lines, caps, and the stat pass.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use hutan_index::adapters::{ClaudeAdapter, MAX_TOOL_ROWS, OmpAdapter};
use hutan_index::scan::{self, FileOutcome, Metrics, ScanContext};
use hutan_index::search::search;
use hutan_index::store;
use hutan_index::{Adapter, ParseState, Parsed, Role, SearchQuery, Source};
use rusqlite::Connection;
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    conn: Connection,
    metrics: Metrics,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::open(&dir.path().join("index.sqlite")).unwrap();
        Self {
            dir,
            conn,
            metrics: Metrics::default(),
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join("src").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    fn run(&mut self, adapter: &dyn Adapter, path: &Path) -> FileOutcome {
        let ctx = ScanContext {
            cutoff_ms: 0,
            metrics: &self.metrics,
        };
        scan::process_file(&mut self.conn, adapter, path, ctx).unwrap()
    }

    fn rows(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
            .unwrap()
    }

    fn search(&self, text: &str) -> Vec<hutan_index::Hit> {
        search(
            &self.conn,
            &SearchQuery {
                text: text.to_owned(),
                limit: 20,
                ..SearchQuery::default()
            },
        )
        .unwrap()
    }
}

fn user_line(text: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"2026-09-28T05:01:22.983Z","cwd":"/work/app","sessionId":"s1","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

fn write(path: &Path, lines: &[String]) {
    let mut text = lines.join("\n");
    text.push('\n');
    fs::write(path, text).unwrap();
}

fn append(path: &Path, text: &str) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

#[test]
fn appended_lines_are_the_only_ones_read() {
    let mut f = Fixture::new();
    let path = f.file("proj/s1.jsonl");
    write(
        &path,
        &[
            user_line("first question about parsers"),
            user_line("second question"),
        ],
    );
    let first_size = fs::metadata(&path).unwrap().len();

    assert_eq!(
        f.run(&ClaudeAdapter, &path),
        FileOutcome::Indexed {
            rows: 2,
            errors: 0,
            reindexed: false
        }
    );
    assert_eq!(f.metrics.bytes_read(), first_size);
    assert_eq!(f.rows(), 2);

    // Nothing changed: not even opened.
    assert_eq!(f.run(&ClaudeAdapter, &path), FileOutcome::Unchanged);
    assert_eq!(f.metrics.files_opened(), 1);

    let extra = format!("{}\n", user_line("third question about lifetimes"));
    append(&path, &extra);
    assert_eq!(
        f.run(&ClaudeAdapter, &path),
        FileOutcome::Indexed {
            rows: 1,
            errors: 0,
            reindexed: false
        }
    );
    // Only the appended bytes were read the second time.
    assert_eq!(f.metrics.bytes_read(), first_size + extra.len() as u64);
    assert_eq!(f.metrics.reindexed(), 0);
    assert_eq!(f.rows(), 3);
    assert_eq!(f.search("lifetimes").len(), 1);

    let hit = &f.search("parsers")[0];
    assert_eq!(hit.source, Source::Claude);
    assert_eq!(hit.session_key, "s1");
    assert_eq!(hit.cwd.as_deref(), Some("/work/app"));
    assert_eq!(hit.title.as_deref(), Some("first question about parsers"));
    assert_eq!(hit.ts_ms, Some(1_790_571_682_983));
}

#[test]
fn a_truncated_file_is_indexed_again_from_the_start() {
    let mut f = Fixture::new();
    let path = f.file("proj/s1.jsonl");
    write(
        &path,
        &[
            user_line("alpha alpha alpha"),
            user_line("beta beta beta"),
            user_line("gamma gamma gamma"),
        ],
    );
    f.run(&ClaudeAdapter, &path);
    assert_eq!(f.rows(), 3);

    write(&path, &[user_line("delta delta delta")]);
    assert!(fs::metadata(&path).unwrap().len() < 400);
    assert_eq!(
        f.run(&ClaudeAdapter, &path),
        FileOutcome::Indexed {
            rows: 1,
            errors: 0,
            reindexed: true
        }
    );
    assert_eq!(f.metrics.reindexed(), 1);
    assert_eq!(f.rows(), 1);
    assert!(f.search("alpha").is_empty());
    assert_eq!(f.search("delta").len(), 1);
}

#[test]
fn a_rewritten_file_of_the_same_size_is_detected() {
    let mut f = Fixture::new();
    let path = f.file("proj/s1.jsonl");
    write(&path, &[user_line("aaaa one"), user_line("bbbb two")]);
    f.run(&ClaudeAdapter, &path);

    // Same length, different bytes in front of the stored offset; then one more line.
    write(&path, &[user_line("cccc one"), user_line("dddd two")]);
    append(&path, &format!("{}\n", user_line("eeee three")));
    assert_eq!(
        f.run(&ClaudeAdapter, &path),
        FileOutcome::Indexed {
            rows: 3,
            errors: 0,
            reindexed: true
        }
    );
    assert!(f.search("aaaa").is_empty());
    assert_eq!(f.search("cccc").len(), 1);
    assert_eq!(f.rows(), 3);
}

#[test]
fn malformed_lines_are_skipped_and_counted() {
    let mut f = Fixture::new();
    let path = f.file("proj/s1.jsonl");
    write(
        &path,
        &[
            user_line("good one"),
            "{this is not json".to_owned(),
            String::new(),
            r#"{"type":"user"}"#.to_owned(),
            r#"["array"]"#.to_owned(),
            user_line("good two"),
        ],
    );
    assert_eq!(
        f.run(&ClaudeAdapter, &path),
        FileOutcome::Indexed {
            rows: 2,
            errors: 3,
            reindexed: false
        }
    );
    let row = store::source_row(&f.conn, &path.to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(row.errors, 3);
    assert_eq!(row.rows, 2);
    // The counter accumulates over appends.
    append(&path, "garbage\n");
    f.run(&ClaudeAdapter, &path);
    let row = store::source_row(&f.conn, &path.to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(row.errors, 4);
}

#[test]
fn an_unfinished_last_line_waits_for_its_end() {
    let mut f = Fixture::new();
    let path = f.file("proj/s1.jsonl");
    let full = user_line("completed later");
    let (head, rest) = full.split_at(full.len() / 2);
    fs::write(&path, format!("{}\n{head}", user_line("whole line"))).unwrap();
    f.run(&ClaudeAdapter, &path);
    assert_eq!(f.rows(), 1);

    append(&path, &format!("{rest}\n"));
    assert_eq!(
        f.run(&ClaudeAdapter, &path),
        FileOutcome::Indexed {
            rows: 1,
            errors: 0,
            reindexed: false
        }
    );
    assert_eq!(f.search("later").len(), 1);
    assert_eq!(f.rows(), 2);
}

#[test]
fn a_complete_json_line_without_a_newline_is_taken() {
    let mut f = Fixture::new();
    let path = f.file("proj/s1.jsonl");
    fs::write(&path, user_line("no trailing newline")).unwrap();
    f.run(&ClaudeAdapter, &path);
    assert_eq!(f.rows(), 1);
    // The writer finishes the line later and goes on.
    append(&path, &format!("\n{}\n", user_line("next one")));
    f.run(&ClaudeAdapter, &path);
    assert_eq!(f.rows(), 2);
}

#[test]
fn oversized_lines_are_skipped_without_losing_the_next_one() {
    let mut f = Fixture::new();
    let path = f.file("proj/s1.jsonl");
    let huge = user_line(&"x".repeat(scan::MAX_LINE_BYTES + 10));
    write(
        &path,
        &[user_line("before"), huge, user_line("after the monster")],
    );
    assert_eq!(
        f.run(&ClaudeAdapter, &path),
        FileOutcome::Indexed {
            rows: 2,
            errors: 1,
            reindexed: false
        }
    );
    assert_eq!(f.search("monster").len(), 1);
}

#[test]
fn long_texts_are_chunked_and_tool_rows_are_capped() {
    let mut f = Fixture::new();
    let path = f.file("proj/s1.jsonl");
    let long = "log line with some words in it\\n".repeat(1000);
    let tool_result = format!(
        r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","content":"{long}"}}]}}}}"#
    );
    let user_long = user_line(&long);
    write(&path, &[user_long, tool_result.clone()]);
    f.run(&ClaudeAdapter, &path);
    let (user_rows, tool_rows): (i64, i64) = f
        .conn
        .query_row(
            "SELECT SUM(role = 'user'), SUM(role = 'tool') FROM entries",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!((7..=16).contains(&user_rows), "{user_rows}");
    assert_eq!(tool_rows, 4, "a tool result keeps its first 4 chunks");
    let max_body: i64 = f
        .conn
        .query_row("SELECT MAX(LENGTH(body)) FROM fts", [], |r| r.get(0))
        .unwrap();
    assert!(max_body <= 4096, "{max_body}");

    // The per-session cap on tool rows.
    let lines: Vec<String> = (0..MAX_TOOL_ROWS + 50)
        .map(|i| {
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","content":"output number {i}"}}]}}}}"#
            )
        })
        .collect();
    let capped = f.file("proj/s2.jsonl");
    write(&capped, &lines);
    f.run(&ClaudeAdapter, &capped);
    let count: i64 = f
        .conn
        .query_row(
            "SELECT COUNT(*) FROM entries e JOIN sessions s ON s.id = e.session_id WHERE s.key = 's2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, i64::from(MAX_TOOL_ROWS));
}

#[test]
fn secrets_never_reach_the_index() {
    let mut f = Fixture::new();
    let path = f.file("proj/s1.jsonl");
    write(
        &path,
        &[user_line(
            "my key is sk-proj-abcdefghijklmnop1234567890 and GITHUB_TOKEN=ghx_notreallyatoken12345 ok",
        )],
    );
    f.run(&ClaudeAdapter, &path);
    let all: String = f
        .conn
        .query_row("SELECT body || ' ' || idents FROM fts", [], |r| r.get(0))
        .unwrap();
    assert!(!all.contains("abcdefghijklmnop"), "{all}");
    assert!(!all.contains("notreallyatoken"), "{all}");
    assert!(all.contains("[REDACTED]"), "{all}");
    assert!(f.search("abcdefghijklmnop1234567890").is_empty());
}

#[test]
fn rows_older_than_the_range_are_not_indexed() {
    let mut f = Fixture::new();
    let path = f.file("proj/s1.jsonl");
    write(&path, &[user_line("an old message")]);
    let ctx = ScanContext {
        cutoff_ms: 1_800_000_000_000,
        metrics: &f.metrics,
    };
    // The file itself is older than the cutoff: not even opened.
    let outcome = scan::process_file(&mut f.conn, &ClaudeAdapter, &path, ctx).unwrap();
    assert_eq!(outcome, FileOutcome::Skipped);
    assert_eq!(f.metrics.files_opened(), 0);
    assert_eq!(f.rows(), 0);
}

#[derive(Debug)]
struct Versioned(u32);

impl Adapter for Versioned {
    fn source(&self) -> Source {
        Source::Claude
    }
    fn format_version(&self) -> u32 {
        self.0
    }
    fn key_from_path(&self, path: &Path) -> String {
        path.file_stem().unwrap().to_string_lossy().into_owned()
    }
    fn parse_line(&self, line: &str, _: &mut ParseState) -> Parsed {
        Parsed::Entries(vec![hutan_index::Entry::new(Role::User, None, line)])
    }
}

#[test]
fn a_new_format_version_reindexes_the_file() {
    let mut f = Fixture::new();
    let path = f.file("proj/v.jsonl");
    write(&path, &["one".to_owned(), "two".to_owned()]);
    f.run(&Versioned(1), &path);
    assert_eq!(f.run(&Versioned(1), &path), FileOutcome::Unchanged);
    assert_eq!(
        f.run(&Versioned(2), &path),
        FileOutcome::Indexed {
            rows: 2,
            errors: 0,
            reindexed: true
        }
    );
    assert_eq!(f.rows(), 2);
}

#[test]
fn the_stat_pass_lists_only_new_and_changed_files() {
    let mut f = Fixture::new();
    let a = f.file("proj/a.jsonl");
    let b = f.file("proj/b.jsonl");
    write(&a, &[user_line("alpha")]);
    write(&b, &[user_line("beta")]);
    fs::write(f.file("proj/notes.txt"), "not a transcript").unwrap();
    let root = f.dir.path().join("src");

    let mut changed = scan::changed_files(&f.conn, &ClaudeAdapter, &root, 0).unwrap();
    changed.sort();
    assert_eq!(changed, vec![a.clone(), b.clone()]);

    for path in &changed {
        f.run(&ClaudeAdapter, path);
    }
    let opened = f.metrics.files_opened();
    assert!(
        scan::changed_files(&f.conn, &ClaudeAdapter, &root, 0)
            .unwrap()
            .is_empty()
    );

    append(&b, &format!("{}\n", user_line("beta again")));
    assert_eq!(
        scan::changed_files(&f.conn, &ClaudeAdapter, &root, 0).unwrap(),
        vec![b]
    );
    assert_eq!(
        f.metrics.files_opened(),
        opened,
        "the stat pass opens nothing"
    );
}

#[test]
fn omp_title_line_is_rewritten_in_place() {
    let mut f = Fixture::new();
    let path = f.file("-work-app/2026-09-28T05-01-22-983Z_abc123.jsonl");
    let header = |title: &str| {
        let line = format!(
            r#"{{"type":"title","v":1,"title":"{title}","updatedAt":"2026-09-28T05:02:00.000Z","pad":""}}"#
        );
        // Fixed width, like the real file.
        let pad = 120_usize.saturating_sub(line.len());
        line.replace(r#""pad":"""#, &format!(r#""pad":"{}""#, " ".repeat(pad)))
    };
    let body = [
        r#"{"type":"session","version":3,"id":"abc123","timestamp":"2026-09-28T05:01:22.983Z","cwd":"/work/app"}"#.to_owned(),
        r#"{"type":"message","timestamp":"2026-09-28T05:01:23.000Z","message":{"role":"user","content":[{"type":"text","text":"explain the writer actor"}]}}"#.to_owned(),
    ];
    let mut lines = vec![header("")];
    lines.extend(body.clone());
    write(&path, &lines);
    f.run(&OmpAdapter, &path);
    let hit = &f.search("writer")[0];
    assert_eq!(hit.session_key, "abc123");
    assert_eq!(hit.cwd.as_deref(), Some("/work/app"));
    assert_eq!(hit.title.as_deref(), Some("explain the writer actor"));

    // OMP names the session: the first line changes in place, nothing is appended.
    let mut lines = vec![header("Writer actor deep dive")];
    lines.extend(body);
    write(&path, &lines);
    assert_eq!(
        f.run(&OmpAdapter, &path),
        FileOutcome::Indexed {
            rows: 0,
            errors: 0,
            reindexed: false
        }
    );
    assert_eq!(
        f.search("writer")[0].title.as_deref(),
        Some("Writer actor deep dive")
    );
    assert_eq!(f.rows(), 1);
}
