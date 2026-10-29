//! Render tracking, history and bounds of the headless engine.
#![allow(clippy::expect_used)] // tests assert by panicking

use hutan_protocol::messages::TerminalSize;
use hutan_protocol::render::{Color, CursorShape};
use hutan_terminal::{DefaultColors, Emulator, RenderTracker};

fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn emulator(cols: u16, rows: u16) -> Emulator {
    Emulator::new(size(cols, rows), 10_000).expect("emulator")
}

#[test]
fn fresh_tracker_needs_a_snapshot_and_frames_follow() {
    let mut em = emulator(10, 3);
    let mut tracker = RenderTracker::new();
    assert!(em.needs_snapshot(&tracker));
    assert!(
        em.frame(&mut tracker).is_none(),
        "a fresh tracker never gets a frame"
    );

    let snapshot = em.snapshot(&mut tracker);
    assert_eq!((snapshot.cols, snapshot.rows), (10, 3));
    assert_eq!(snapshot.lines.len(), 3);
    assert_eq!(snapshot.frame, 1);
    assert!(!em.needs_snapshot(&tracker));
    assert!(
        em.frame(&mut tracker).is_none(),
        "nothing changed since the snapshot"
    );

    em.feed(b"hi");
    let frame = em.frame(&mut tracker).expect("frame after output");
    assert_eq!(frame.frame, 2);
    assert_eq!(frame.scrolled, 0);
    assert_eq!(frame.dirty.len(), 1);
    assert_eq!(frame.dirty[0].0, 0);
    assert_eq!(frame.dirty[0].1.runs[0].text, "hi");
    assert_eq!(frame.cursor.col, 2);
    assert!(frame.modes.is_none() && frame.title.is_none() && frame.links.is_empty());
    assert!(em.frame(&mut tracker).is_none());
}

#[test]
fn cursor_only_change_produces_a_frame_without_dirty_rows() {
    let mut em = emulator(10, 3);
    let mut tracker = RenderTracker::new();
    em.snapshot(&mut tracker);
    em.feed(b"\x1b[2;4H");
    let frame = em.frame(&mut tracker).expect("cursor moved");
    assert!(frame.dirty.is_empty());
    assert_eq!((frame.cursor.col, frame.cursor.row), (3, 1));
    assert_eq!(frame.cursor.shape, CursorShape::Block);
}

#[test]
fn snapshots_and_frames_carry_the_absolute_line_of_the_first_row() {
    let mut em = emulator(10, 3);
    let mut tracker = RenderTracker::new();
    assert_eq!(em.snapshot(&mut tracker).viewport_top, 0);
    // Two line feeds fill the screen; each one after that scrolls a row into the history.
    em.feed(b"a\r\nb\r\nc\r\nd\r\ne");
    let frame = em.frame(&mut tracker).expect("output");
    assert_eq!(frame.viewport_top, em.viewport_top());
    assert_eq!(frame.viewport_top, 2);
    // With the history full, output that scrolls without changing a single visible row (the
    // same text again) moves only the number, so it must still produce a frame.
    let mut em = Emulator::new(size(10, 3), 1).expect("emulator");
    let mut tracker = RenderTracker::new();
    em.feed(b"x\r\nx\r\nx\r\nx\r\nx");
    let before = em.snapshot(&mut tracker);
    assert_eq!(before.history_len, 1, "the history is at its bound");
    em.feed(b"\r\nx");
    let frame = em.frame(&mut tracker).expect("the viewport scrolled");
    assert!(frame.dirty.is_empty(), "{:?}", frame.dirty);
    assert_eq!(frame.history_len, 1);
    assert_eq!(frame.viewport_top, before.viewport_top + 1);
    // The alternate screen keeps the number of the primary screen.
    em.feed(b"\x1b[?1049h\x1b[2J\x1b[Halt");
    let alt = em.snapshot(&mut tracker);
    assert!(alt.modes.alt_screen);
    assert_eq!(alt.viewport_top, frame.viewport_top);
}

#[test]
fn mode_and_title_changes_are_reported_once() {
    let mut em = emulator(10, 3);
    let mut tracker = RenderTracker::new();
    em.snapshot(&mut tracker);
    em.feed(b"\x1b[?2004h\x1b]2;hello\x07");
    let frame = em.frame(&mut tracker).expect("modes and title changed");
    assert!(frame.modes.is_some_and(|m| m.bracketed_paste));
    assert_eq!(frame.title, Some(Some("hello".to_owned())));
    em.feed(b"\x1b]2;\x07");
    let frame = em.frame(&mut tracker).expect("title cleared");
    assert_eq!(frame.title, Some(None));
    assert!(frame.modes.is_none());
}

#[test]
fn alt_screen_switch_redraws_changed_rows() {
    let mut em = emulator(10, 3);
    let mut tracker = RenderTracker::new();
    em.feed(b"main");
    em.snapshot(&mut tracker);
    em.feed(b"\x1b[?1049h\x1b[2J\x1b[Halt");
    let frame = em.frame(&mut tracker).expect("alt screen");
    assert!(frame.modes.is_some_and(|m| m.alt_screen));
    assert_eq!(frame.dirty.len(), 1);
    assert_eq!(frame.dirty[0].1.runs[0].text, "alt");
    em.feed(b"\x1b[?1049l");
    let frame = em.frame(&mut tracker).expect("back to primary");
    assert!(frame.modes.is_some_and(|m| !m.alt_screen));
    assert_eq!(frame.dirty[0].1.runs[0].text, "main");
}

#[test]
fn resize_and_palette_changes_require_a_snapshot() {
    let mut em = emulator(10, 3);
    let mut tracker = RenderTracker::new();
    em.snapshot(&mut tracker);
    em.resize(size(12, 4)).expect("resize");
    assert!(em.needs_snapshot(&tracker));
    assert!(em.frame(&mut tracker).is_none());
    let snapshot = em.snapshot(&mut tracker);
    assert_eq!(
        (snapshot.cols, snapshot.rows, snapshot.lines.len()),
        (12, 4, 4)
    );

    em.feed(b"\x1b]4;5;rgb:01/02/03\x07");
    assert!(
        em.needs_snapshot(&tracker),
        "frames cannot carry palette changes"
    );
    let snapshot = em.snapshot(&mut tracker);
    assert_eq!(snapshot.palette.indexed, vec![(5, (1, 2, 3))]);
    assert!(!em.needs_snapshot(&tracker));
}

#[test]
fn links_are_sent_once_in_the_frame_that_first_references_them() {
    let mut em = emulator(30, 3);
    let mut tracker = RenderTracker::new();
    em.snapshot(&mut tracker);
    em.feed(b"\x1b]8;;https://a.example\x07a\x1b]8;;\x07");
    let frame = em.frame(&mut tracker).expect("frame");
    assert_eq!(frame.links.len(), 1);
    assert_eq!(
        (frame.links[0].id, frame.links[0].uri.as_str()),
        (0, "https://a.example")
    );
    assert_eq!(frame.dirty[0].1.runs[0].link, Some(0));

    em.feed(b"\r\n\x1b]8;id=x;https://b.example\x07b\x1b]8;;\x07");
    let frame = em.frame(&mut tracker).expect("frame");
    assert_eq!(frame.links.len(), 1);
    assert_eq!(
        (frame.links[0].id, frame.links[0].uri.as_str()),
        (1, "https://b.example")
    );
}

#[test]
fn link_ids_are_stable_across_scrolling_and_history_queries() {
    let mut em = emulator(20, 2);
    em.feed(b"\x1b]8;id=k;https://keep.example\x07link\x1b]8;;\x07\r\n1\r\n2\r\n3");
    let mut tracker = RenderTracker::new();
    // The link line is in scrollback only: the viewport snapshot does not know it yet.
    assert!(em.snapshot(&mut tracker).links.is_empty());
    let linked = |rows: &[hutan_protocol::render::Row]| -> Vec<u32> {
        rows.iter()
            .flat_map(|row| &row.runs)
            .filter_map(|run| run.link)
            .collect()
    };
    assert_eq!(linked(&em.history(0, 10)), vec![0]);
    // The next frame announces links that were interned in the meantime.
    em.feed(b"\r\nx");
    let frame = em.frame(&mut tracker).expect("frame");
    assert_eq!(frame.links.len(), 1);
    assert_eq!(frame.links[0].uri, "https://keep.example");
    // Same id after further scrolling; the table does not grow.
    assert_eq!(linked(&em.history(0, 10)), vec![0]);
    assert_eq!(em.links().len(), 1);
}

#[test]
fn scrollback_is_bounded_and_history_is_newest_first() {
    let mut em = Emulator::new(size(10, 3), 100).expect("emulator");
    for line in 0..5000 {
        em.feed(format!("line{line}\r\n").as_bytes());
    }
    assert_eq!(em.history_len(), 100);
    let rows = em.history(0, 5);
    let text: Vec<&str> = rows.iter().map(|r| r.runs[0].text.as_str()).collect();
    // Viewport shows line4998 + blank (cursor row); history starts right above it.
    assert_eq!(
        text,
        ["line4997", "line4996", "line4995", "line4994", "line4993"]
    );
    assert_eq!(em.history(95, 50).len(), 5, "history is shorter at the top");
    assert!(em.history(100, 10).is_empty());
}

#[test]
fn synchronized_output_withholds_visibility_until_closed_or_forced() {
    let mut em = emulator(20, 3);
    assert!(em.feed(b"a"));
    let generation = em.generation();
    assert!(
        !em.feed(b"\x1b[?2026hbatched"),
        "bytes inside an open batch are not visible"
    );
    assert_eq!(em.generation(), generation);
    assert!(em.sync_deadline().is_some());
    assert_eq!(em.grid_text().lines().next(), Some("a"));
    em.stop_sync();
    assert!(em.generation() > generation);
    assert_eq!(em.grid_text().lines().next(), Some("abatched"));
    assert!(em.sync_deadline().is_none());
}

#[test]
fn degenerate_sizes_are_rejected_and_tiny_ones_work() {
    assert!(Emulator::new(size(0, 5), 10).is_err());
    assert!(Emulator::new(size(5, 0), 10).is_err());
    let mut em = emulator(1, 1);
    em.feed(b"abc\r\ndef\x1b[?1049h\x1b[?1049l");
    em.resize(size(3, 2)).expect("resize");
    assert!(em.resize(size(0, 2)).is_err());
    let mut tracker = RenderTracker::new();
    assert_eq!(em.snapshot(&mut tracker).lines.len(), 2);
}

#[test]
fn trailing_blank_cells_are_trimmed_but_styled_blanks_are_kept() {
    let mut em = emulator(10, 2);
    em.feed(b"ab   \r\n\x1b[41m  \x1b[0m");
    let mut tracker = RenderTracker::new();
    let snapshot = em.snapshot(&mut tracker);
    assert_eq!(snapshot.lines[0].runs.len(), 1);
    assert_eq!(snapshot.lines[0].runs[0].text, "ab");
    let styled = &snapshot.lines[1].runs[0];
    assert_eq!((styled.text.as_str(), styled.bg), ("  ", Color::Indexed(1)));
}

#[test]
fn configured_default_colors_answer_osc_queries_until_the_app_overrides_them() {
    let mut em = emulator(10, 3).with_default_colors(DefaultColors {
        foreground: (1, 2, 3),
        background: (4, 5, 6),
        cursor: (7, 8, 9),
    });
    em.feed(b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07");
    assert_eq!(
        String::from_utf8_lossy(&em.drain_replies()),
        "\x1b]10;rgb:0101/0202/0303\x07\x1b]11;rgb:0404/0505/0606\x07\x1b]12;rgb:0707/0808/0909\x07"
    );
    em.feed(b"\x1b]11;rgb:ff/ff/ff\x07\x1b]11;?\x07");
    assert_eq!(
        String::from_utf8_lossy(&em.drain_replies()),
        "\x1b]11;rgb:ffff/ffff/ffff\x07"
    );
}

#[test]
fn set_default_colors_changes_later_osc_replies_but_not_application_overrides() {
    let mut em = emulator(10, 3);
    em.set_default_colors(DefaultColors {
        foreground: (1, 2, 3),
        background: (0xab, 0xcd, 0xef),
        cursor: (7, 8, 9),
    });
    // Each reply ends with the terminator its query used.
    em.feed(b"\x1b]11;?\x07\x1b]11;?\x1b\\");
    assert_eq!(
        String::from_utf8_lossy(&em.drain_replies()),
        "\x1b]11;rgb:abab/cdcd/efef\x07\x1b]11;rgb:abab/cdcd/efef\x1b\\"
    );
    em.feed(b"\x1b]10;rgb:11/22/33\x07");
    em.set_default_colors(DefaultColors {
        foreground: (9, 9, 9),
        background: (0, 0, 0),
        cursor: (7, 8, 9),
    });
    em.feed(b"\x1b]10;?\x07\x1b]11;?\x07");
    assert_eq!(
        String::from_utf8_lossy(&em.drain_replies()),
        "\x1b]10;rgb:1111/2222/3333\x07\x1b]11;rgb:0000/0000/0000\x07"
    );
}

#[test]
fn a_flood_of_queries_is_answered_within_a_bounded_reply_buffer() {
    let mut em = emulator(10, 3);
    for _ in 0..4 {
        em.feed(&b"\x1b]4;1;?\x07\x1b[16t\x1b[14t".repeat(20_000));
    }
    let replies = em.drain_replies();
    assert!(
        !replies.is_empty() && replies.len() <= 64 * 1024,
        "{} bytes",
        replies.len()
    );
    // Still alive and answering after the flood.
    em.feed(b"\x1b[18t");
    assert_eq!(em.drain_replies(), b"\x1b[8;3;10t");
}

// ---- colour scheme (FX-UI-B, P3-14): a program that asks is told whether the terminal is light ----

fn light() -> DefaultColors {
    DefaultColors {
        foreground: (0x1f, 0x1e, 0x1d),
        background: (0xfa, 0xf9, 0xf5),
        cursor: (0x1f, 0x1e, 0x1d),
    }
}

fn reply(em: &mut Emulator) -> String {
    String::from_utf8_lossy(&em.drain_replies()).into_owned()
}

#[test]
fn the_scheme_query_is_answered_from_the_background_the_osc_11_query_gets() {
    let mut em = emulator(10, 3);
    // Dark by default (CSI ? 997 ; 1 n), the colour the program asks for the same way.
    em.feed(b"\x1b[?996n\x1b]11;?\x07");
    assert_eq!(reply(&mut em), "\x1b[?997;1n\x1b]11;rgb:1e1e/1e1e/1e1e\x07");
    em.set_default_colors(light());
    em.feed(b"\x1b[?996n\x1b]11;?\x07");
    assert_eq!(reply(&mut em), "\x1b[?997;2n\x1b]11;rgb:fafa/f9f9/f5f5\x07");
}

#[test]
fn a_program_that_turned_notifications_on_hears_of_a_change_and_one_that_did_not_does_not() {
    let mut em = emulator(10, 3);
    em.set_default_colors(light());
    assert_eq!(reply(&mut em), "", "nobody asked");
    // DECRQM for the mode: known, reset (2); the engine's own "not recognised" is not sent too.
    em.feed(b"\x1b[?2031$p");
    assert_eq!(reply(&mut em), "\x1b[?2031;2$y");
    em.feed(b"\x1b[?2031h\x1b[?2031$p");
    assert_eq!(reply(&mut em), "\x1b[?2031;1$y");
    // Light to dark and back: one notification each, none when the scheme stays.
    em.set_default_colors(DefaultColors::default());
    assert_eq!(reply(&mut em), "\x1b[?997;1n");
    em.set_default_colors(DefaultColors {
        foreground: (0xee, 0xee, 0xee),
        ..DefaultColors::default()
    });
    assert_eq!(reply(&mut em), "");
    em.set_default_colors(light());
    assert_eq!(reply(&mut em), "\x1b[?997;2n");
    em.feed(b"\x1b[?2031l");
    em.set_default_colors(DefaultColors::default());
    assert_eq!(reply(&mut em), "", "turned off");
}

#[test]
fn the_scheme_sequences_work_split_over_reads() {
    let mut em = emulator(10, 3);
    for byte in b"\x1b[?2031h\x1b[?996n\x1b[?2031$p" {
        em.feed(&[*byte]);
    }
    assert_eq!(reply(&mut em), "\x1b[?997;1n\x1b[?2031;1$y");
}
