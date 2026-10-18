//! The cold store must be invisible: random output streams through an emulator that spills
//! history (small hot margin) and a reference emulator that keeps the whole history in the
//! engine must give identical history, viewport and logical lines (ADR 0013).
#![allow(clippy::expect_used, clippy::unwrap_used)] // tests assert by panicking

use hutan_protocol::messages::TerminalSize;
use hutan_protocol::render::{CellFlags, Color, Link, Row};

use crate::emulator::{Emulator, LineScope, LogicalLine};
use crate::render::RenderTracker;

fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

const WORDS: &[&str] = &[
    "alpha",
    "beta",
    "gamma-delta",
    "x",
    "0123456789",
    "漢字",
    "日本語のテキスト",
    "e\u{301}clair",
    "n\u{303}",
    "😀",
    "a\u{200d}b",
    "tab\there",
    "  ",
    "https://example.com/path?q=1",
    "ünïcödé",
];

fn line(rng: &mut Rng, out: &mut Vec<u8>) {
    let words = 1 + rng.below(14);
    for index in 0..words {
        if index > 0 {
            out.push(b' ');
        }
        out.extend_from_slice(rng.pick(WORDS).as_bytes());
    }
    match rng.below(10) {
        0 => {}
        1 => out.extend_from_slice(b"\n"),
        _ => out.extend_from_slice(b"\r\n"),
    }
}

fn sgr(rng: &mut Rng, out: &mut Vec<u8>) {
    let params: &[&str] = &[
        "0",
        "1",
        "2",
        "3",
        "4",
        "4:3",
        "7",
        "8",
        "9",
        "21",
        "22",
        "31",
        "32;1",
        "39",
        "44",
        "49",
        "91",
        "100",
        "38;5;200",
        "48;5;17",
        "38;2;10;20;30",
        "48;2;200;100;50",
        "58;5;9",
        "58;2;1;2;3",
        "59",
    ];
    out.extend_from_slice(b"\x1b[");
    out.extend_from_slice(rng.pick(params).as_bytes());
    out.push(b'm');
}

fn link(rng: &mut Rng, out: &mut Vec<u8>) {
    match rng.below(4) {
        0 => out.extend_from_slice(b"\x1b]8;;\x07"),
        1 => out.extend_from_slice(b"\x1b]8;;\x1b\\"),
        2 => out.extend_from_slice(
            format!(
                "\x1b]8;id=k{};https://k.example/{}\x07",
                rng.below(3),
                rng.below(3)
            )
            .as_bytes(),
        ),
        _ => out.extend_from_slice(
            format!("\x1b]8;;https://e.example/{}\x1b\\", rng.below(50)).as_bytes(),
        ),
    }
}

fn op(rng: &mut Rng, out: &mut Vec<u8>, depth: u32, hostile: bool) {
    let mut pick = rng.below(100);
    // Output that wipes or hides history is rare in calm streams, so history can build up.
    if !hostile && matches!(pick, 59..=69 | 73..=74 | 79..=81) && rng.below(12) != 0 {
        pick = 0;
    }
    match pick {
        0..=44 => line(rng, out),
        45..=54 => sgr(rng, out),
        55..=58 => link(rng, out),
        59..=61 => out.extend_from_slice(b"\x1b[3J"),
        62 => out.extend_from_slice(b"\x1b[2J"),
        63 if rng.below(3) == 0 => out.extend_from_slice(b"\x1bc"),
        63 => out.extend_from_slice(b"\x1b[H"),
        64..=66 => out.extend_from_slice(if rng.below(2) == 0 {
            b"\x1b[?1049h"
        } else {
            b"\x1b[?1049l"
        }),
        67..=69 => out.extend_from_slice(
            format!("\x1b[{}{}", 1 + rng.below(30), rng.pick(&["S", "T"])).as_bytes(),
        ),
        70..=72 => out.extend_from_slice(
            format!("\x1b[{};{}H", 1 + rng.below(30), 1 + rng.below(90)).as_bytes(),
        ),
        73 => out.extend_from_slice(
            format!("\x1b[{};{}r", 1 + rng.below(4), 4 + rng.below(20)).as_bytes(),
        ),
        74 => out.extend_from_slice(b"\x1b[r"),
        75..=78 if depth == 0 => {
            out.extend_from_slice(b"\x1b[?2026h");
            for _ in 0..1 + rng.below(25) {
                op(rng, out, depth + 1, hostile);
            }
            if rng.below(6) != 0 {
                out.extend_from_slice(b"\x1b[?2026l");
            }
        }
        79..=81 => out.extend_from_slice(format!("\x1b[{}J", rng.below(2)).as_bytes()),
        82..=83 => out.extend_from_slice(format!("\x1b[{}K", rng.below(3)).as_bytes()),
        84 => out.extend_from_slice(rng.pick(&[b"\x1bD".as_slice(), b"\x1bM", b"\x1bE"])),
        _ => {
            for _ in 0..1 + rng.below(6) {
                out.extend_from_slice(b"\r\n");
            }
        }
    }
}

/// A random stream cut into chunks at arbitrary byte positions (escapes get split too).
fn stream(rng: &mut Rng, ops: u64, hostile: bool) -> Vec<Vec<u8>> {
    let mut bytes = Vec::new();
    for _ in 0..ops {
        op(rng, &mut bytes, 0, hostile);
    }
    let mut chunks = Vec::new();
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        let max = *rng.pick(&[3_u64, 40, 700, 6_000]);
        let take = (1 + rng.below(max) as usize).min(rest.len());
        chunks.push(rest[..take].to_vec());
        rest = &rest[take..];
    }
    chunks
}

type NormRun = (
    String,
    Color,
    Color,
    CellFlags,
    Option<Color>,
    Option<String>,
);

/// Rows with link ids replaced by URIs: ids depend on when rows are converted (spill time vs
/// read time), URIs do not.
fn norm(rows: &[Row], links: &[Link]) -> Vec<(Vec<NormRun>, bool)> {
    rows.iter()
        .map(|row| {
            let runs = row
                .runs
                .iter()
                .map(|run| {
                    let uri = run.link.map(|id| {
                        links
                            .iter()
                            .find(|link| link.id == id)
                            .map_or_else(|| format!("?{id}"), |link| link.uri.clone())
                    });
                    (
                        run.text.clone(),
                        run.fg,
                        run.bg,
                        run.flags,
                        run.underline_color,
                        uri,
                    )
                })
                .collect();
            (runs, row.wrapped)
        })
        .collect()
}

fn history_of(em: &mut Emulator, from: u32, count: u32) -> Vec<(Vec<NormRun>, bool)> {
    let rows = em.history(from, count);
    norm(&rows, &em.links())
}

fn lines_of(em: &mut Emulator, scope: LineScope) -> Vec<LogicalLine> {
    em.logical_lines(scope).collect()
}

fn assert_same(cold: &mut Emulator, reference: &mut Emulator, rng: &mut Rng, context: &str) {
    assert_eq!(
        cold.history_len(),
        reference.history_len(),
        "{context}: history_len"
    );
    let len = reference.history_len();
    assert_eq!(
        history_of(cold, 0, len + 5),
        history_of(reference, 0, len + 5),
        "{context}: whole history"
    );
    for _ in 0..4 {
        let from = rng.below(u64::from(len) + 3) as u32;
        let count = rng.below(70) as u32;
        assert_eq!(
            history_of(cold, from, count),
            history_of(reference, from, count),
            "{context}: history({from}, {count})"
        );
    }
    // history_while sees the same rows in the same order and stops where told to.
    let stop = rng.below(40) as usize;
    let mut seen_cold = 0;
    let rows_cold = cold.history_while(0, len, |_| {
        seen_cold += 1;
        seen_cold <= stop
    });
    let mut seen_ref = 0;
    let rows_ref = reference.history_while(0, len, |_| {
        seen_ref += 1;
        seen_ref <= stop
    });
    assert_eq!(
        norm(&rows_cold, &cold.links()),
        norm(&rows_ref, &reference.links()),
        "{context}: history_while"
    );

    let (mut a, mut b) = (RenderTracker::new(), RenderTracker::new());
    let (snap_cold, snap_ref) = (cold.snapshot(&mut a), reference.snapshot(&mut b));
    assert_eq!(
        norm(&snap_cold.lines, &cold.links()),
        norm(&snap_ref.lines, &reference.links()),
        "{context}: viewport"
    );
    assert_eq!(snap_cold.cursor, snap_ref.cursor, "{context}: cursor");
    assert_eq!(
        snap_cold.history_len, snap_ref.history_len,
        "{context}: snapshot history_len"
    );
    assert_eq!(snap_cold.modes, snap_ref.modes, "{context}: modes");

    for scope in [LineScope::History, LineScope::HistoryAndViewport] {
        assert_eq!(
            lines_of(cold, scope),
            lines_of(reference, scope),
            "{context}: logical lines {scope:?}"
        );
    }
}

/// What a case exercised, so the tests can prove they are not vacuous.
#[derive(Default, Debug)]
struct Coverage {
    max_cold: usize,
    /// Times the cold store went from non-empty to empty (cleared by `ESC[3J` / `ESC c`).
    cold_resets: u32,
    /// Feeds after which the history was exactly at the line bound.
    at_bound: u32,
}

fn run_case(
    seed: u64,
    cols: u16,
    rows: u16,
    lines: usize,
    margin: usize,
    ops: u64,
    hostile: bool,
) -> Coverage {
    let mut coverage = Coverage::default();
    let mut rng = Rng::new(seed);
    let mut cold = Emulator::with_hot_margin(size(cols, rows), lines, margin).expect("cold");
    let mut reference =
        Emulator::with_hot_margin(size(cols, rows), lines, usize::MAX).expect("reference");
    let mut last_cold = 0;
    for (index, chunk) in stream(&mut rng, ops, hostile).iter().enumerate() {
        cold.feed(chunk);
        reference.feed(chunk);
        let now = cold.cold_len();
        coverage.max_cold = coverage.max_cold.max(now);
        if now == 0 && last_cold > 0 {
            coverage.cold_resets += 1;
        }
        last_cold = now;
        if cold.history_len() as usize == lines {
            coverage.at_bound += 1;
        }
        assert!(
            u64::from(cold.history_len()) <= lines as u64,
            "bound exceeded (seed {seed})"
        );
        if index % 6 == 5 && !reference.sync_pending() {
            assert_same(
                &mut cold,
                &mut reference,
                &mut rng,
                &format!("seed {seed} chunk {index}"),
            );
        }
    }
    cold.stop_sync();
    reference.stop_sync();
    assert_same(
        &mut cold,
        &mut reference,
        &mut rng,
        &format!("seed {seed} end"),
    );
    coverage
}

#[test]
fn spilled_history_equals_full_history_for_random_streams() {
    let mut total = Coverage::default();
    for seed in 0..120 {
        let cols = [3_u16, 8, 20, 40, 80][seed as usize % 5];
        let rows = [1_u16, 3, 6, 24][seed as usize / 5 % 4];
        let lines = [10_usize, 60, 300, 1_000][seed as usize / 3 % 4];
        let margin = [1_usize, 4, 16, 256][seed as usize / 2 % 4];
        let coverage = run_case(seed, cols, rows, lines, margin, 350, seed % 3 == 0);
        total.max_cold = total.max_cold.max(coverage.max_cold);
        total.cold_resets += coverage.cold_resets;
        total.at_bound += coverage.at_bound;
    }
    // The streams really spilled, cleared the cold store and evicted at the bound.
    assert!(total.max_cold >= 100, "{total:?}");
    assert!(total.cold_resets >= 20, "{total:?}");
    assert!(total.at_bound >= 20, "{total:?}");
}

#[test]
fn spilled_history_equals_full_history_with_a_deep_cold_store() {
    // Enough output that most of the history is cold and the bound evicts for a long time.
    let mut total = Coverage::default();
    for seed in 100..104 {
        let coverage = run_case(seed, 30, 8, 500, 16, 1_500, false);
        total.max_cold = total.max_cold.max(coverage.max_cold);
        total.at_bound += coverage.at_bound;
    }
    assert!(total.max_cold >= 300, "{total:?}");
    assert!(total.at_bound >= 20, "{total:?}");
}

#[test]
fn default_margin_matches_reference_on_long_runs() {
    let mut max_cold = 0;
    for seed in 200..202 {
        max_cold = max_cold.max(run_case(seed, 40, 10, 700, 256, 3_000, false).max_cold);
    }
    assert!(max_cold >= 100, "max cold {max_cold}");
}

/// Numbered lines, some styled, wide and wrapped: what a build log or `cat` of a big file feeds.
fn flood(lines: usize, cols: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for n in 0..lines {
        match n % 7 {
            0 => out.extend_from_slice(format!("\x1b[3{}mline {n}\x1b[0m\r\n", n % 8).as_bytes()),
            1 => out.extend_from_slice(b"y\n"),
            2 => out.extend_from_slice("漢字 wide ünï\r\n".as_bytes()),
            3 => out.extend(std::iter::repeat_n(b'w', cols + 7)),
            _ => out.extend_from_slice(format!("plain line number {n}\r\n").as_bytes()),
        }
        if n % 7 == 3 {
            out.extend_from_slice(b"\r\n");
        }
    }
    out
}

/// The line bound, the absolute numbering and every retained row are the same whether a read of
/// sixty thousand lines arrives at once (settled when the history outgrows the hot window by
/// a slack, and at its end) or in reads of one slice (settled after each). Reads are cut at the engine's slice size: the
/// parser the engine uses drops the character after a multi-byte one that straddles two
/// `advance` calls, so other cut points would not give the same text.
#[test]
fn a_huge_read_leaves_the_same_scrollback_as_small_reads_and_the_full_history() {
    let bytes = flood(60_000, 80);
    for (lines, margin) in [(5_000_usize, 256_usize), (300, 16), (20_000, 256)] {
        let mut whole = Emulator::with_hot_margin(size(80, 24), lines, margin).expect("whole");
        let mut small = Emulator::with_hot_margin(size(80, 24), lines, margin).expect("small");
        let mut reference =
            Emulator::with_hot_margin(size(80, 24), lines, usize::MAX).expect("reference");
        whole.feed(&bytes);
        for chunk in bytes.chunks(2048) {
            small.feed(chunk);
        }
        for chunk in bytes.chunks(4096) {
            reference.feed(chunk);
        }
        let mut rng = Rng::new(7);
        assert_eq!(whole.history_len() as usize, lines, "bound ({lines})");
        assert_eq!(whole.viewport_top(), small.viewport_top(), "numbering");
        assert_eq!(whole.viewport_top(), reference.viewport_top(), "numbering");
        assert_same(&mut whole, &mut reference, &mut rng, "whole read");
        assert_same(&mut small, &mut reference, &mut rng, "small reads");
    }
}

/// Viewport rows are converted once per generation and size, and never served stale.
#[test]
fn the_viewport_row_cache_follows_the_grid() {
    let mut em = Emulator::new(size(20, 3), 100).expect("emulator");
    em.feed(b"one\r\ntwo");
    let first = em.grid_text();
    assert_eq!(first, "one\ntwo\n");
    let (mut a, mut b) = (RenderTracker::new(), RenderTracker::new());
    assert_eq!(em.snapshot(&mut a).lines, em.snapshot(&mut b).lines);
    em.feed(b"!");
    assert_eq!(em.grid_text(), "one\ntwo!\n");
    em.feed(b"\x1b[2J\x1b[Hnew");
    assert_eq!(em.grid_text(), "new\n\n");
    em.resize(size(10, 2)).expect("resize");
    assert_eq!(em.snapshot(&mut a).lines.len(), 2);
    // A frame for a tracker that saw the earlier state carries exactly the rows that differ.
    let mut c = RenderTracker::new();
    em.snapshot(&mut c);
    em.feed(b"\r\nx");
    let frame = em.frame(&mut c).expect("changed");
    assert!(!frame.dirty.is_empty());
}
