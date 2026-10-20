//! `Buffer` micro-benchmarks behind the "Editor" rows of `docs/perf.md` (E-1).
//!
//! Run with `cargo bench -p hutan-buffer --bench buffer` (release profile, plain `Instant`, median
//! of several runs). They time the text-model share of the editor budgets B11 (open), B12
//! (keystroke) and B15 (a command on a big buffer); the GPUI side is measured elsewhere.

use std::hint::black_box;
use std::time::{Duration, Instant};

use hutan_buffer::{Anchor, Buffer, SearchQuery};

fn corpus(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 128);
    let mut i = 0usize;
    while s.len() < bytes {
        if i % 7 == 3 {
            s.push_str(&format!("    // héllo 日本語 🦀 line {i}\n"));
        } else {
            s.push_str(&format!("    let value_{i} = compute(&state, {i})?;\n"));
        }
        i += 1;
    }
    s
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn time<F: FnMut() -> Duration>(runs: usize, mut f: F) -> Duration {
    median((0..runs).map(|_| f()).collect())
}

fn ms(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1e3)
}

fn us(d: Duration, ops: usize) -> String {
    format!("{:.2} µs/op", d.as_secs_f64() * 1e6 / ops as f64)
}

fn main() {
    println!("| scenario | 1 MB | 10 MB |");
    println!("|---|---|---|");
    let sizes = [1usize << 20, 10 << 20];
    let texts: Vec<String> = sizes.iter().map(|s| corpus(*s)).collect();
    let bytes: Vec<&[u8]> = texts.iter().map(|t| t.as_bytes()).collect();
    let runs = 7;

    let row = |name: &str, cells: Vec<String>| println!("| {name} | {} |", cells.join(" | "));

    // Open: validate UTF-8, detect/normalize line endings, build the rope.
    row(
        "open (`Buffer::from_bytes`)",
        bytes
            .iter()
            .map(|b| {
                ms(time(runs, || {
                    let t = Instant::now();
                    black_box(Buffer::from_bytes(black_box(b)).is_ok());
                    t.elapsed()
                }))
            })
            .collect(),
    );
    // Same for a CRLF file (an extra normalization pass).
    let crlf: Vec<String> = texts.iter().map(|t| t.replace('\n', "\r\n")).collect();
    row(
        "open CRLF file",
        crlf.iter()
            .map(|c| {
                ms(time(runs, || {
                    let t = Instant::now();
                    black_box(Buffer::from_bytes(black_box(c.as_bytes())).is_ok());
                    t.elapsed()
                }))
            })
            .collect(),
    );
    // Save: serialize back to the file bytes.
    row(
        "save (`to_bytes`, LF)",
        texts
            .iter()
            .map(|t| {
                let buffer = Buffer::from_content(t);
                ms(time(runs, || {
                    let s = Instant::now();
                    black_box(buffer.to_bytes());
                    s.elapsed()
                }))
            })
            .collect(),
    );

    // One keystroke: insert a character at the cursor through the whole path (validate, invert,
    // history, rope, 100 tracked anchors).
    const KEYS: usize = 20_000;
    row(
        "keystroke: `insert` 1 char at the cursor, 100 anchors tracked",
        texts
            .iter()
            .map(|t| {
                let d = time(runs, || {
                    let mut b = Buffer::from_content(t);
                    let mid = b.text().line_start(b.text().line_count() / 2).unwrap_or(0);
                    let lines = b.text().line_count();
                    for i in 0..100 {
                        // anchors at line starts (always character boundaries)
                        let at = b.text().line_start(i * 37 % lines).unwrap_or(0);
                        let _ = b.track_anchor(Anchor::left(at));
                    }
                    let s = Instant::now();
                    for i in 0..KEYS {
                        let _ = b.insert(mid + i, "x");
                    }
                    let d = s.elapsed();
                    black_box(b);
                    d
                });
                us(d, KEYS)
            })
            .collect(),
    );

    // A group of 1 000 keystrokes undone and redone as one step.
    row(
        "group of 1 000 typed chars: undo + redo",
        texts
            .iter()
            .map(|t| {
                let mut b = Buffer::from_content(t);
                let mid = b.text().line_start(b.text().line_count() / 2).unwrap_or(0);
                b.begin_group();
                for i in 0..1000 {
                    let _ = b.insert(mid + i, "x");
                }
                b.end_group();
                ms(time(runs, || {
                    let s = Instant::now();
                    black_box(b.undo());
                    black_box(b.redo());
                    s.elapsed()
                }))
            })
            .collect(),
    );

    // A snapshot for a background parse.
    row(
        "snapshot (`Text` clone)",
        texts
            .iter()
            .map(|t| {
                let b = Buffer::from_content(t);
                us(
                    time(runs, || {
                        let s = Instant::now();
                        for _ in 0..1000 {
                            black_box(b.snapshot());
                        }
                        s.elapsed()
                    }),
                    1000,
                )
            })
            .collect(),
    );

    // An agent rewrote one line / a hundred lines on disk.
    for (label, n) in [
        ("`apply_external`, 1 line changed", 1usize),
        ("`apply_external`, 100 lines changed", 100),
    ] {
        row(
            label,
            texts
                .iter()
                .map(|t| {
                    let lines: Vec<&str> = t.split_inclusive('\n').collect();
                    let mut changed = String::with_capacity(t.len() + 1000);
                    for (i, l) in lines.iter().enumerate() {
                        if n > 0
                            && i % (lines.len() / n).max(1) == 1
                            && i / (lines.len() / n).max(1) < n
                        {
                            changed.push_str("    let changed = 1; // edited by an agent\n");
                        } else {
                            changed.push_str(l);
                        }
                    }
                    ms(time(5, || {
                        let mut b = Buffer::from_content(t);
                        let s = Instant::now();
                        black_box(b.apply_external(black_box(&changed)).ok());
                        s.elapsed()
                    }))
                })
                .collect(),
        );
    }

    // Search: a rare literal (the whole text is scanned), a common one (first 1 000 matches), a
    // regex with a class and a case-insensitive Unicode literal (first 1 000 matches).
    for (label, query) in [
        ("search literal, absent", SearchQuery::literal("zzzqqq")),
        (
            "search literal, first 1 000 of many",
            SearchQuery::literal("compute"),
        ),
        (
            "search regex `value_\\d+7\\b`, first 1 000",
            SearchQuery::regex(r"value_\d+7\b"),
        ),
        (
            "search `HÉLLO` ignoring case, first 1 000",
            SearchQuery::literal("HÉLLO").case_sensitive(false),
        ),
    ] {
        row(
            label,
            texts
                .iter()
                .map(|t| {
                    let b = Buffer::from_content(t);
                    ms(time(runs, || {
                        let s = Instant::now();
                        black_box(b.text().search_all(black_box(&query), 1000).ok());
                        s.elapsed()
                    }))
                })
                .collect(),
        );
    }
}
