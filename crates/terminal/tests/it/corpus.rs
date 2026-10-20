//! Conformance corpus (roadmap M0.4): hand-written feature streams with expected outcomes and
//! recorded real-program streams, replayed through a fresh [`Emulator`].
#![allow(clippy::expect_used)] // tests assert by panicking

use std::fs;
use std::path::{Path, PathBuf};

use hutan_protocol::messages::TerminalSize;
use hutan_terminal::{Emulator, RenderTracker, TerminalMeta};
use serde_json::Value;

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn files(dir: &str, extension: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(corpus_dir().join(dir))
        .unwrap_or_else(|e| panic!("read {dir}: {e}"))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        .collect();
    found.sort();
    found
}

/// Decodes the `.vt` text format described in `tests/corpus/feature/README.md`.
fn decode_vt(source: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in source.split(|&b| b == b'\n') {
        if line.trim_ascii_start().starts_with(b"//") {
            continue;
        }
        let mut i = 0;
        while i < line.len() {
            if line[i] != b'\\' {
                out.push(line[i]);
                i += 1;
                continue;
            }
            let escape = line.get(i + 1).copied().expect("dangling backslash in .vt");
            i += 2;
            match escape {
                b'e' => out.push(0x1b),
                b'a' => out.push(0x07),
                b'r' => out.push(b'\r'),
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'\\' => out.push(b'\\'),
                b'x' => {
                    let hex = std::str::from_utf8(&line[i..i + 2]).expect("hex digits");
                    out.push(u8::from_str_radix(hex, 16).expect("hex digits"));
                    i += 2;
                }
                other => panic!("unknown escape \\{}", other as char),
            }
        }
    }
    out
}

struct Outcome {
    emulator: Emulator,
    replies: Vec<u8>,
    meta: Vec<String>,
}

fn replay(stream: &[u8], size: TerminalSize, chunk: usize) -> Outcome {
    replay_resizing(stream, size, &[], chunk)
}

/// [`replay`] that resizes the emulator to `resizes` (`(offset in the stream, size)`, in order)
/// when the replay reaches the offset.
fn replay_resizing(
    stream: &[u8],
    size: TerminalSize,
    resizes: &[(usize, TerminalSize)],
    chunk: usize,
) -> Outcome {
    let mut emulator = Emulator::new(size, 10_000).expect("emulator");
    let mut replies = Vec::new();
    let mut meta = Vec::new();
    let mut feed = |emulator: &mut Emulator, bytes: &[u8]| {
        for piece in bytes.chunks(chunk.max(1)) {
            emulator.feed(piece);
            replies.extend(emulator.drain_replies());
            // The block index derives its events from the marks; the corpus pins the marks.
            meta.extend(
                emulator
                    .drain_meta()
                    .into_iter()
                    .filter(|m| !matches!(m, TerminalMeta::Block(_)))
                    .map(|m| format!("{m:?}")),
            );
        }
    };
    let mut fed = 0;
    for (offset, size) in resizes {
        feed(&mut emulator, &stream[fed..*offset]);
        fed = *offset;
        emulator.resize(*size).expect("resize");
    }
    feed(&mut emulator, &stream[fed..]);
    Outcome {
        emulator,
        replies,
        meta,
    }
}

/// Resizes a recording made with `--resize`: `NAME.resizes` next to the `.raw`, one
/// `OFFSET COLSxROWS` line per resize.
fn recorded_resizes(raw: &Path) -> Vec<(usize, TerminalSize)> {
    let Ok(text) = fs::read_to_string(raw.with_extension("resizes")) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| {
            let (offset, size) = line.split_once(' ').expect("OFFSET COLSxROWS");
            let (cols, rows) = size.split_once('x').expect("COLSxROWS");
            (
                offset.parse().expect("offset"),
                TerminalSize {
                    cols: cols.parse().expect("cols"),
                    rows: rows.parse().expect("rows"),
                    pixel_width: 0,
                    pixel_height: 0,
                },
            )
        })
        .collect()
}

/// `[cols, rows]` or `[cols, rows, pixel_width, pixel_height]`; default 80×24, no pixel size.
fn size_from(spec: Option<&Value>) -> TerminalSize {
    let Some(Value::Array(dims)) = spec else {
        return TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        };
    };
    let dim = |index: usize| {
        dims.get(index)
            .map_or(0, |value| value.as_u64().expect("size entry") as u16)
    };
    TerminalSize {
        cols: dim(0),
        rows: dim(1),
        pixel_width: dim(2),
        pixel_height: dim(3),
    }
}

/// Size a recording was made at: `NAME.size` (`COLSxROWS`) next to the `.raw`, else 80×24.
fn recorded_size(raw: &Path) -> TerminalSize {
    let (cols, rows) = match fs::read_to_string(raw.with_extension("size")) {
        Ok(text) => {
            let (cols, rows) = text.trim().split_once('x').expect("COLSxROWS");
            (cols.parse().expect("cols"), rows.parse().expect("rows"))
        }
        Err(_) => (80, 24),
    };
    TerminalSize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Every key of `expected` (recursively for objects) must equal the same key of `actual`.
fn check_subset(expected: &Value, actual: &Value, path: &str) {
    match (expected, actual) {
        (Value::Object(want), Value::Object(have)) => {
            for (key, want) in want {
                let have = have
                    .get(key)
                    .unwrap_or_else(|| panic!("{path}: no key {key:?} in {actual}"));
                check_subset(want, have, &format!("{path}.{key}"));
            }
        }
        _ => assert_eq!(expected, actual, "{path}"),
    }
}

fn check(name: &str, expect: &Value, outcome: &mut Outcome, chunk: usize) {
    let at = |what: &str| format!("{name} (chunk {chunk}) {what}");
    let mut tracker = RenderTracker::new();
    let snapshot = outcome.emulator.snapshot(&mut tracker);
    let rows = snapshot.lines.len();

    if let Some(grid) = expect.get("grid") {
        let want: Vec<&str> = grid
            .as_array()
            .expect("grid array")
            .iter()
            .map(|v| v.as_str().expect("row"))
            .collect();
        let text = outcome.emulator.grid_text();
        let have: Vec<&str> = text.split('\n').collect();
        assert_eq!(have.len(), rows, "{}", at("row count"));
        for (index, have) in have.iter().enumerate() {
            let want = want.get(index).copied().unwrap_or("");
            assert_eq!(*have, want, "{}", at(&format!("grid row {index}")));
        }
    }
    if let Some(runs) = expect.get("runs") {
        for (row, want) in runs.as_object().expect("runs object") {
            let index: usize = row.parse().expect("row index");
            let have = serde_json::to_value(&snapshot.lines[index].runs).expect("serialize runs");
            let have = have.as_array().expect("runs array");
            let want = want.as_array().expect("run list");
            assert_eq!(
                have.len(),
                want.len(),
                "{}",
                at(&format!("row {index} run count: {have:?}"))
            );
            for (run, (want, have)) in want.iter().zip(have).enumerate() {
                check_subset(want, have, &at(&format!("row {index} run {run}")));
            }
        }
    }
    if let Some(wrapped) = expect.get("wrapped") {
        let want: Vec<bool> = wrapped
            .as_array()
            .expect("wrapped")
            .iter()
            .map(|v| v.as_bool().expect("bool"))
            .collect();
        let have: Vec<bool> = snapshot.lines.iter().map(|row| row.wrapped).collect();
        assert_eq!(have, want, "{}", at("wrapped"));
    }
    for (key, actual) in [
        (
            "cursor",
            serde_json::to_value(snapshot.cursor).expect("cursor"),
        ),
        (
            "modes",
            serde_json::to_value(snapshot.modes).expect("modes"),
        ),
        (
            "palette",
            serde_json::to_value(&snapshot.palette).expect("palette"),
        ),
    ] {
        if let Some(want) = expect.get(key) {
            check_subset(want, &actual, &at(key));
        }
    }
    if let Some(links) = expect.get("links") {
        let want: Vec<&str> = links
            .as_array()
            .expect("links")
            .iter()
            .map(|v| v.as_str().expect("uri"))
            .collect();
        let have: Vec<String> = snapshot.links.iter().map(|link| link.uri.clone()).collect();
        assert_eq!(have, want, "{}", at("links"));
        for (index, link) in snapshot.links.iter().enumerate() {
            assert_eq!(
                link.id as usize,
                index,
                "{}",
                at("link ids are table indexes")
            );
        }
    }
    if let Some(replies) = expect.get("replies") {
        let want = replies.as_str().expect("replies string").as_bytes();
        assert_eq!(
            String::from_utf8_lossy(&outcome.replies)
                .escape_debug()
                .to_string(),
            String::from_utf8_lossy(want).escape_debug().to_string(),
            "{}",
            at("replies")
        );
    }
    if let Some(meta) = expect.get("meta") {
        let want: Vec<&str> = meta
            .as_array()
            .expect("meta")
            .iter()
            .map(|v| v.as_str().expect("event"))
            .collect();
        assert_eq!(outcome.meta, want, "{}", at("meta"));
    }
    if let Some(pending) = expect.get("sync_pending") {
        assert_eq!(
            outcome.emulator.sync_pending(),
            pending.as_bool().expect("bool"),
            "{}",
            at("sync_pending")
        );
    }
    if let Some(len) = expect.get("history_len") {
        assert_eq!(
            u64::from(snapshot.history_len),
            len.as_u64().expect("number"),
            "{}",
            at("history_len")
        );
    }
    if let Some(title) = expect.get("title") {
        assert_eq!(snapshot.title.as_deref(), title.as_str(), "{}", at("title"));
    }
    if let Some(history) = expect.get("history_text") {
        let want: Vec<&str> = history
            .as_array()
            .expect("history_text")
            .iter()
            .map(|v| v.as_str().expect("row"))
            .collect();
        let have: Vec<String> = outcome
            .emulator
            .history(0, 1000)
            .iter()
            .map(|row| {
                row.runs
                    .iter()
                    .map(|run| run.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect();
        assert_eq!(have, want, "{}", at("history_text"));
    }
}

#[test]
fn feature_corpus_matches_expectations_under_every_chunking() {
    let streams = files("feature", "vt");
    assert!(
        streams.len() >= 20,
        "corpus unexpectedly small: {}",
        streams.len()
    );
    for path in streams {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("file name")
            .to_owned();
        let expect_path = path.with_extension("expect.json");
        let expect: Value = serde_json::from_str(
            &fs::read_to_string(&expect_path)
                .unwrap_or_else(|e| panic!("{}: {e}", expect_path.display())),
        )
        .unwrap_or_else(|e| panic!("{}: {e}", expect_path.display()));
        let stream = decode_vt(&fs::read(&path).expect("read .vt"));
        let size = size_from(expect.get("size"));
        for chunk in [usize::MAX, 1, 7] {
            let mut outcome = replay(&stream, size, chunk);
            check(&name, &expect, &mut outcome, chunk);
        }
    }
}

#[test]
fn every_expectation_has_a_stream() {
    for path in files("feature", "json") {
        let stream = path.with_file_name(
            path.file_name()
                .and_then(|n| n.to_str())
                .expect("name")
                .replace(".expect.json", ".vt"),
        );
        assert!(stream.exists(), "{} has no .vt", path.display());
    }
}

#[test]
fn recorded_streams_replay_to_their_recorded_grid() {
    let recordings = files("recorded", "raw");
    assert!(!recordings.is_empty(), "no recorded streams committed");
    for path in recordings {
        let raw = fs::read(&path).expect("read .raw");
        let want = fs::read_to_string(path.with_extension("grid")).expect("read .grid");
        let size = recorded_size(&path);
        let resizes = recorded_resizes(&path);
        for chunk in [usize::MAX, 4096, 1] {
            let mut outcome = replay_resizing(&raw, size, &resizes, chunk);
            let have = outcome.emulator.grid_text();
            assert_eq!(
                have.trim_end(),
                want.trim_end(),
                "{} (chunk {chunk})",
                path.display()
            );
        }
    }
}

#[test]
fn recorded_agent_startups_get_their_color_queries_answered() {
    // Codex and omp ask for the foreground/background colours at startup; unanswered, such
    // TUIs stall or pick the wrong theme.
    let recorded = corpus_dir().join("recorded");
    for (name, queries) in [
        (
            "codex_startup",
            &["]10;rgb:d4d4/d4d4/d4d4", "]11;rgb:1e1e/1e1e/1e1e"][..],
        ),
        ("omp_startup", &["]11;rgb:1e1e/1e1e/1e1e"][..]),
    ] {
        let raw = fs::read(recorded.join(format!("{name}.raw"))).expect("read .raw");
        for chunk in [usize::MAX, 4096, 1] {
            let outcome = replay(
                &raw,
                recorded_size(&recorded.join(format!("{name}.raw"))),
                chunk,
            );
            let replies = String::from_utf8_lossy(&outcome.replies).into_owned();
            for query in queries {
                assert!(
                    replies.contains(query),
                    "{name} (chunk {chunk}): no {query:?} in {replies:?}"
                );
            }
        }
    }
}
