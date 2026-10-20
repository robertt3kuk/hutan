//! Targeted behaviour of the hot/cold split (ADR 0013): the bound, clearing, the alternate
//! screen, resize and logical lines, with a tiny hot window so a few hundred lines are enough.
#![allow(clippy::expect_used, clippy::unwrap_used)] // tests assert by panicking

use hutan_protocol::messages::TerminalSize;
use hutan_protocol::render::Row;

use crate::emulator::{Emulator, LineScope, LogicalLine};

const MARGIN: usize = 8;

fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn emulator(cols: u16, rows: u16, lines: usize) -> Emulator {
    Emulator::with_hot_margin(size(cols, rows), lines, MARGIN).expect("emulator")
}

fn reference(cols: u16, rows: u16, lines: usize) -> Emulator {
    Emulator::with_hot_margin(size(cols, rows), lines, usize::MAX).expect("reference")
}

fn text(row: &Row) -> String {
    row.runs.iter().map(|run| run.text.as_str()).collect()
}

fn texts(em: &mut Emulator, from: u32, count: u32) -> Vec<String> {
    em.history(from, count).iter().map(text).collect()
}

fn feed_lines(em: &mut Emulator, prefix: &str, count: usize) {
    for index in 0..count {
        em.feed(format!("{prefix}{index}\r\n").as_bytes());
    }
}

#[test]
fn the_line_bound_is_exact_and_the_newest_rows_are_kept() {
    let mut em = emulator(10, 3, 100);
    feed_lines(&mut em, "line", 5_000);
    assert_eq!(em.history_len(), 100);
    assert_eq!(em.cold_len(), 100 - MARGIN);
    // Viewport shows line4999 and two blank rows, so history ends at line4998... as the plain
    // engine does (see the bound test in tests/it/emulator.rs).
    let rows = texts(&mut em, 0, 100);
    let newest = 4_997;
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(*row, format!("line{}", newest - index));
    }
    assert!(em.history(100, 10).is_empty());
    assert_eq!(texts(&mut em, 95, 50).len(), 5);
}

#[test]
fn one_huge_chunk_keeps_the_same_rows_as_many_small_ones() {
    let mut big = String::new();
    for index in 0..3_000 {
        big.push_str(&format!("row{index}\r\n"));
    }
    let mut once = emulator(12, 5, 250);
    once.feed(big.as_bytes());
    let mut piecewise = emulator(12, 5, 250);
    for piece in big.as_bytes().chunks(7) {
        piecewise.feed(piece);
    }
    assert_eq!(once.history_len(), 250);
    assert_eq!(texts(&mut once, 0, 300), texts(&mut piecewise, 0, 300));
    assert_eq!(texts(&mut once, 249, 1), vec!["row2746".to_owned()]);
}

#[test]
fn a_small_bound_never_needs_the_cold_store() {
    let mut em = emulator(10, 3, MARGIN);
    feed_lines(&mut em, "x", 200);
    assert_eq!(em.history_len(), MARGIN as u32);
    assert_eq!(em.cold_len(), 0);
}

#[test]
fn erase_scrollback_clears_cold_and_hot_rows() {
    for clear in [
        "\x1b[3J",
        "\x1bc",
        "\x1b[?2026h\x1b[3J\x1b[?2026l",
        "\x1b[?2026hxx\x1b[3J",
    ] {
        let mut em = emulator(10, 4, 500);
        feed_lines(&mut em, "old", 300);
        assert!(em.cold_len() > 200, "{clear:?}");
        em.feed(clear.as_bytes());
        em.stop_sync();
        assert_eq!(em.history_len(), 0, "{clear:?}");
        assert_eq!(em.cold_len(), 0, "{clear:?}");
        assert!(em.history(0, 10).is_empty(), "{clear:?}");
        feed_lines(&mut em, "new", 40);
        let rows = texts(&mut em, 0, 500);
        // Only the viewport rows (not erased by ED 3) can still show an old line.
        assert!(
            rows.iter()
                .all(|row| row.contains("new")
                    || ["old297", "old298", "old299"].contains(&row.as_str())),
            "{clear:?}: {rows:?}"
        );
        assert!(rows.len() >= 36, "{clear:?}");
    }
}

#[test]
fn erase_scrollback_is_noticed_even_when_the_history_regrows_in_the_same_feed() {
    let mut em = emulator(10, 4, 500);
    feed_lines(&mut em, "old", 300);
    let mut chunk = b"\x1b[3J".to_vec();
    for index in 0..400 {
        chunk.extend_from_slice(format!("new{index}\r\n").as_bytes());
    }
    em.feed(&chunk);
    let rows = texts(&mut em, 0, 500);
    // The viewport (old297..old299 and the cursor row) is not erased, so it scrolls into history.
    assert_eq!(rows.len(), 400);
    assert!(
        rows[..397].iter().all(|row| row.starts_with("new")),
        "{rows:?}"
    );
    assert_eq!(rows[397..], ["old299", "old298", "old297"]);
}

#[test]
fn erase_scrollback_split_across_feeds_inside_a_sync_batch() {
    let mut em = emulator(10, 4, 500);
    feed_lines(&mut em, "old", 300);
    for piece in [
        &b"\x1b[?20"[..],
        b"26h",
        b"\x1b[",
        b"3",
        b"J",
        b"\x1b[?2026",
        b"l",
    ] {
        em.feed(piece);
    }
    assert_eq!(em.history_len(), 0);
}

#[test]
fn the_alternate_screen_hides_history_and_its_own_erase_leaves_it_alone() {
    let mut em = emulator(10, 4, 500);
    feed_lines(&mut em, "keep", 300);
    let before = texts(&mut em, 0, 500);
    let len = em.history_len();
    assert_eq!(before.len(), len as usize);
    assert!(em.cold_len() > 200);

    em.feed(b"\x1b[?1049h");
    assert_eq!(em.history_len(), 0);
    assert!(em.history(0, 10).is_empty());
    assert_eq!(em.logical_lines(LineScope::History).count(), 0);
    // Scrolling and erasing on the alternate screen never touches the primary history.
    feed_lines(&mut em, "alt", 200);
    em.feed(b"\x1b[3J");
    assert_eq!(em.history_len(), 0);
    em.feed(b"\x1b[?1049l");
    assert_eq!(em.history_len(), len);
    assert_eq!(texts(&mut em, 0, 500), before);
}

#[test]
fn a_reset_inside_the_alternate_screen_clears_the_history() {
    let mut em = emulator(10, 4, 500);
    feed_lines(&mut em, "keep", 300);
    em.feed(b"\x1b[?1049h");
    em.feed(b"\x1bc");
    assert_eq!(em.history_len(), 0);
    assert_eq!(em.cold_len(), 0);
}

#[test]
fn resize_reflows_the_hot_window_and_leaves_cold_rows_alone() {
    let lines = 2_000;
    let margin = 40;
    let mut cold = Emulator::with_hot_margin(size(40, 10), lines, margin).expect("emulator");
    let mut full = reference(40, 10, lines);
    for index in 0..3_000 {
        let row = format!("n{index} {}\r\n", "x".repeat(index % 70));
        cold.feed(row.as_bytes());
        full.feed(row.as_bytes());
    }
    assert_eq!(cold.history_len() as usize, lines);
    let cold_before = cold.cold_len();
    assert_eq!(cold_before, lines - margin);
    let cold_rows_before = cold.history(margin as u32, 5_000);
    assert_eq!(cold_rows_before.len(), cold_before);

    // Narrower: the hot window reflows into more rows, the surplus spills (at the new width)
    // and the bound still holds exactly, the store giving up its oldest rows.
    cold.resize(size(25, 10)).expect("resize");
    full.resize(size(25, 10)).expect("resize");
    assert_eq!(cold.history_len() as usize, lines);
    assert_eq!(cold.history_len() as usize, margin + cold.cold_len());
    let cold_rows_after = cold.history(margin as u32, 5_000);
    assert_eq!(cold_rows_after.len(), cold.cold_len());
    // The rows that were cold before the resize are still there, unchanged, behind `spilled`
    // freshly spilled (reflowed) ones.
    let spilled = (0..=cold_rows_after.len())
        .find(|&at| cold_rows_after[at..] == cold_rows_before[..cold_rows_after.len() - at])
        .expect("old cold rows survive unchanged");
    assert!(
        spilled >= 8,
        "the narrower hot window spilled {spilled} rows"
    );
    // The newest rows equal what a full reflow gives (the oldest hot logical line may be cut).
    let compared = (margin + spilled - 12) as u32;
    assert_eq!(cold.history(0, compared), full.history(0, compared));
    assert_eq!(cold.grid_text(), full.grid_text());

    // Wider again: fewer hot rows, cold rows still byte-identical, counts consistent.
    let cold_len = cold.cold_len();
    cold.resize(size(80, 10)).expect("resize");
    assert_eq!(cold.cold_len(), cold_len);
    let hot = cold.history_len() as usize - cold.cold_len();
    assert!(hot >= 1);
    assert_eq!(cold.history(hot as u32, 5_000), cold_rows_after);
    assert!(cold.history_len() as usize <= lines);
}

#[test]
fn changing_the_row_count_by_less_than_the_hot_window_matches_the_reference() {
    let mut cold = emulator(10, 4, 500);
    let mut full = reference(10, 4, 500);
    for em in [&mut cold, &mut full] {
        feed_lines(em, "r", 300);
    }
    for rows in [8, 3, 6] {
        cold.resize(size(10, rows)).expect("resize");
        full.resize(size(10, rows)).expect("resize");
        assert_eq!(cold.history_len(), full.history_len(), "rows {rows}");
        let len = full.history_len();
        assert_eq!(cold.history(0, len), full.history(0, len), "rows {rows}");
        assert_eq!(cold.grid_text(), full.grid_text(), "rows {rows}");
    }
}

#[test]
fn enlarging_the_screen_past_the_hot_window_drops_the_cold_store() {
    let mut em = emulator(10, 4, 500);
    feed_lines(&mut em, "r", 300);
    em.resize(size(10, 60)).expect("resize");
    assert_eq!(em.history_len(), 0);
    assert_eq!(em.cold_len(), 0);
    feed_lines(&mut em, "again", 100);
    assert!(em.history_len() > 0);
}

#[test]
fn resizing_inside_the_alternate_screen_keeps_the_history() {
    let mut em = emulator(10, 4, 500);
    feed_lines(&mut em, "keep", 300);
    let cold_len = em.cold_len();
    let cold_rows = em.history(MARGIN as u32, 500);
    em.feed(b"\x1b[?1049h");
    em.resize(size(14, 4)).expect("resize");
    feed_lines(&mut em, "alt", 20);
    em.feed(b"\x1b[?1049l");
    assert_eq!(em.cold_len(), cold_len);
    let hot = em.history_len() as usize - em.cold_len();
    assert!(hot >= 1);
    assert_eq!(em.history(hot as u32, 500), cold_rows);
    // The history is still guarded: the marker was re-applied on return.
    em.feed(b"\x1b[3J");
    assert_eq!(em.history_len(), 0);
    assert_eq!(em.cold_len(), 0);
}

fn logical(em: &mut Emulator, scope: LineScope) -> Vec<LogicalLine> {
    em.logical_lines(scope).collect()
}

#[test]
fn logical_lines_join_wrapped_rows_across_cold_and_hot() {
    let mut em = emulator(10, 3, 1_000);
    em.feed(b"aaaaaaaa  bbb\r\n");
    // A wrapped line whose rows straddle the cold/hot boundary for some offset below.
    for offset in 0..14 {
        for _ in 0..offset {
            em.feed(b"s\r\n");
        }
        em.feed(b"0123456789abcdefghijKLMNO\r\n");
    }
    feed_lines(&mut em, "z", 20);
    assert!(em.cold_len() > 50);
    let lines = logical(&mut em, LineScope::History);
    assert_eq!(
        lines[0],
        LogicalLine {
            start: 0,
            rows: 2,
            text: "aaaaaaaa  bbb".to_owned()
        }
    );
    let wrapped = lines
        .iter()
        .filter(|line| line.text == "0123456789abcdefghijKLMNO")
        .count();
    assert_eq!(wrapped, 14);
    assert!(lines.iter().filter(|l| l.rows == 3).count() == 14);
    // Row indices are contiguous and cover every history row exactly once.
    let mut next = 0;
    for line in &lines {
        assert_eq!(line.start, next);
        next += line.rows;
    }
    assert_eq!(next, em.history_len());

    let mut full = reference(10, 3, 1_000);
    full.feed(b"aaaaaaaa  bbb\r\n");
    for offset in 0..14 {
        for _ in 0..offset {
            full.feed(b"s\r\n");
        }
        full.feed(b"0123456789abcdefghijKLMNO\r\n");
    }
    feed_lines(&mut full, "z", 20);
    assert_eq!(lines, logical(&mut full, LineScope::History));
}

#[test]
fn logical_lines_can_include_the_viewport() {
    let mut em = emulator(10, 3, 100);
    feed_lines(&mut em, "q", 30);
    let history = em.history_len();
    let all = logical(&mut em, LineScope::HistoryAndViewport);
    let only_history = logical(&mut em, LineScope::History);
    assert_eq!(all.len(), only_history.len() + 3);
    assert_eq!(all[..only_history.len()], only_history[..]);
    assert_eq!(all[only_history.len()].start, history);
    assert_eq!(all[only_history.len()].text, "q28");
    assert_eq!(all.last().map(|line| line.text.as_str()), Some(""));
    em.feed(b"\x1b[?1049h\x1b[Hhello");
    let alt = logical(&mut em, LineScope::HistoryAndViewport);
    assert_eq!(alt.len(), 3);
    assert_eq!(alt[0].text, "hello");
    assert_eq!(alt[0].start, 0);
}

#[test]
fn rows_beyond_the_margin_are_cold_as_soon_as_a_feed_returns() {
    let mut em = emulator(10, 4, 500);
    for index in 0..300 {
        em.feed(format!("c{index}\r\n").as_bytes());
    }
    let len = em.history_len() as usize;
    assert_eq!(em.cold_len(), len - MARGIN);
    // New output extends the same history seamlessly.
    em.feed(b"tail\r\n");
    assert_eq!(texts(&mut em, 0, 1)[0], "c297");
    assert_eq!(em.history_len() as usize, len + 1);
}

#[test]
fn a_flood_past_the_bound_keeps_exactly_the_newest_rows() {
    let mut em = emulator(10, 4, 500);
    feed_lines(&mut em, "old", 300);
    // More lines than the bound in one feed.
    let mut flood = String::new();
    for index in 0..2_000 {
        flood.push_str(&format!("flood{index}\r\n"));
    }
    em.feed(flood.as_bytes());
    assert_eq!(em.history_len(), 500);
    assert_eq!(em.cold_len(), 500 - MARGIN);
    let rows = texts(&mut em, 0, 500);
    assert_eq!(rows[0], "flood1996");
    assert_eq!(rows[499], "flood1497");
}
