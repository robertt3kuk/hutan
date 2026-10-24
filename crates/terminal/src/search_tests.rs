//! `Emulator::scan_lines`: logical lines with absolute rows and cell columns across soft wraps,
//! wide characters, the cold store, the alternate screen and blocks.
#![allow(clippy::expect_used, clippy::unwrap_used)] // tests assert by panicking

use hutan_protocol::messages::TerminalSize;

use crate::blocks::BlockTextError;
use crate::emulator::{Emulator, ScanFlow, ScanProgress};

fn emulator(cols: u16, rows: u16, lines: usize) -> Emulator {
    // A tiny hot window so a few hundred lines exercise the cold store.
    Emulator::with_hot_margin(
        TerminalSize {
            cols,
            rows,
            pixel_width: 0,
            pixel_height: 0,
        },
        lines,
        8,
    )
    .expect("emulator")
}

/// `(row, rows, text, block)` of one line.
type Seen = (u64, u32, String, Option<u64>);

/// Every line from `from`.
fn lines(em: &mut Emulator, from: u64, block: Option<u64>) -> (Vec<Seen>, ScanProgress) {
    let mut seen = Vec::new();
    let progress = em
        .scan_lines(from, usize::MAX, block, |line| {
            seen.push((line.row, line.rows, line.text.to_owned(), line.block_id()));
            ScanFlow::Continue
        })
        .expect("scan");
    (seen, progress)
}

#[test]
fn lines_have_absolute_rows_and_the_scope_covers_history_and_screen() {
    let mut em = emulator(10, 3, 100);
    for index in 0..10 {
        em.feed(format!("line{index}\r\n").as_bytes());
    }
    let (seen, progress) = lines(&mut em, 0, None);
    // Ten lines were fed on three rows: the screen shows line9 and two blank rows.
    let top = em.viewport_top();
    assert_eq!(progress.first_row, 0);
    assert_eq!(progress.viewport_top, top);
    assert_eq!(progress.end, top + 3);
    assert!(progress.done());
    assert_eq!(seen[0], (0, 1, "line0".into(), None));
    assert_eq!(seen[9], (9, 1, "line9".into(), None));
    assert_eq!(seen.len() as u64, top + 3);
    // Starting later skips the older lines.
    let (later, _) = lines(&mut em, 7, None);
    assert_eq!(later[0].0, 7);
}

#[test]
fn eviction_moves_the_first_row_but_rows_keep_their_numbers() {
    let mut em = emulator(10, 3, 50);
    for index in 0..500 {
        em.feed(format!("n{index}\r\n").as_bytes());
    }
    let (seen, progress) = lines(&mut em, 0, None);
    assert_eq!(progress.first_row, em.viewport_top() - 50);
    assert_eq!(seen[0].0, progress.first_row);
    // Row r holds `n{r}` as long as it is retained.
    for (row, _, text, _) in seen.iter().filter(|line| !line.2.is_empty()) {
        assert_eq!(*text, format!("n{row}"));
    }
}

#[test]
fn a_wrapped_line_is_one_line_and_positions_cross_the_wrap() {
    let mut em = emulator(10, 4, 100);
    em.feed(b"abcdefghijKLMNOP tail\r\nnext\r\n");
    let mut found = Vec::new();
    em.scan_lines(0, usize::MAX, None, |line| {
        if let Some(at) = line.text.find("ijKLM") {
            found.push((
                line.row,
                line.rows,
                line.position(at),
                line.end_position(at + 5),
            ));
        }
        ScanFlow::Continue
    })
    .expect("scan");
    // "abcdefghij" fills row 0, "KLMNOP tai" row 1, "l" row 2.
    assert_eq!(found, vec![(0, 3, (0, 8), (1, 3))]);
    let (seen, _) = lines(&mut em, 0, None);
    assert_eq!(seen[0].2, "abcdefghijKLMNOP tail");
    assert_eq!(seen[0].1, 3);
    assert_eq!(seen[1].2, "next");
    assert_eq!(seen[1].0, 3);
}

#[test]
fn a_match_that_ends_at_the_end_of_a_row_stays_on_that_row() {
    let mut em = emulator(10, 3, 100);
    em.feed(b"abcdefghijkl\r\n");
    let mut found = None;
    em.scan_lines(0, usize::MAX, None, |line| {
        if line.text.starts_with("abc") {
            found = Some((line.position(5), line.end_position(10)));
        }
        ScanFlow::Continue
    })
    .expect("scan");
    assert_eq!(found, Some(((0, 5), (0, 10))));
}

#[test]
fn wide_characters_take_two_cells_and_combining_marks_none() {
    let mut em = emulator(20, 3, 100);
    // Two wide characters, then "ab", an "e" with a combining accent, then "z".
    em.feed("日本ab\u{65}\u{301}z\r\n".as_bytes());
    let mut found = Vec::new();
    em.scan_lines(0, usize::MAX, None, |line| {
        if line.text.contains("ab") {
            let a = line.text.find('a').unwrap();
            let z = line.text.find('z').unwrap();
            found.push((
                line.position(a),
                line.end_position(a + 2),
                line.position(z),
                line.end_position(z + 1),
            ));
        }
        ScanFlow::Continue
    })
    .expect("scan");
    assert_eq!(found, vec![((0, 4), (0, 6), (0, 7), (0, 8))]);
}

#[test]
fn the_row_budget_stops_after_a_whole_line_and_paging_continues() {
    let mut em = emulator(10, 3, 200);
    for index in 0..40 {
        em.feed(format!("w{index}\r\n").as_bytes());
    }
    let mut all = Vec::new();
    let mut from = 0;
    let mut calls = 0;
    loop {
        calls += 1;
        let progress = em
            .scan_lines(from, 8, None, |line| {
                all.push(line.row);
                ScanFlow::Continue
            })
            .expect("scan");
        if progress.done() {
            break;
        }
        assert!(progress.next > from, "every call makes progress");
        from = progress.next;
    }
    assert!(calls >= 5, "{calls} calls");
    let (whole, _) = lines(&mut em, 0, None);
    assert_eq!(all, whole.iter().map(|line| line.0).collect::<Vec<_>>());
}

#[test]
fn stop_before_and_stop_after_say_where_to_resume() {
    let mut em = emulator(10, 3, 100);
    for index in 0..10 {
        em.feed(format!("s{index}\r\n").as_bytes());
    }
    let before = em
        .scan_lines(0, usize::MAX, None, |line| {
            if line.row == 4 {
                ScanFlow::StopBefore
            } else {
                ScanFlow::Continue
            }
        })
        .expect("scan");
    assert_eq!(before.next, 4);
    assert!(!before.done());
    let after = em
        .scan_lines(0, usize::MAX, None, |line| {
            if line.row == 4 {
                ScanFlow::StopAfter
            } else {
                ScanFlow::Continue
            }
        })
        .expect("scan");
    assert_eq!(after.next, 5);
}

#[test]
fn the_alternate_screen_shows_only_itself() {
    let mut em = emulator(10, 3, 100);
    for index in 0..30 {
        em.feed(format!("old{index}\r\n").as_bytes());
    }
    em.feed(b"\x1b[?1049h\x1b[Halt text");
    let (seen, progress) = lines(&mut em, 0, None);
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].2, "alt text");
    assert_eq!(seen[0].0, em.viewport_top());
    assert_eq!(progress.first_row, em.viewport_top());
    em.feed(b"\x1b[?1049l");
    let (back, _) = lines(&mut em, 0, None);
    assert!(back.iter().any(|line| line.2 == "old0"));
}

const PROMPT: &str = "\x1b]133;A\x07$ \x1b]133;B\x07";

fn command(cmd: &str, output: &str) -> String {
    format!("{PROMPT}{cmd}\r\n\x1b]133;C\x07{output}\x1b]133;D;0\x07")
}

#[test]
fn matches_know_their_block_and_a_block_can_be_the_only_scope() {
    let mut em = emulator(40, 5, 200);
    em.feed(command("echo one", "one-out\r\nshared\r\n").as_bytes());
    em.feed(command("echo two", "two-out\r\nshared\r\n").as_bytes());
    em.feed(PROMPT.as_bytes());
    let (seen, _) = lines(&mut em, 0, None);
    let block_of = |text: &str| {
        seen.iter()
            .find(|line| line.2 == text)
            .unwrap_or_else(|| panic!("no line {text}"))
            .3
    };
    assert_eq!(block_of("one-out"), Some(1));
    assert_eq!(block_of("two-out"), Some(2));
    assert_eq!(block_of("$ echo one"), Some(1));

    let (only_two, progress) = lines(&mut em, 0, Some(2));
    assert_eq!(
        only_two
            .iter()
            .map(|line| line.2.as_str())
            .collect::<Vec<_>>(),
        ["two-out", "shared"]
    );
    assert!(progress.done());
    // `from` inside the scope narrows it further; a `from` past it ends the scan.
    let (rest, _) = lines(&mut em, only_two[1].0, Some(2));
    assert_eq!(rest.len(), 1);
    let (none, progress) = lines(&mut em, only_two[1].0 + 1, Some(2));
    assert!(none.is_empty());
    assert!(progress.done());
}

#[test]
fn a_running_block_reaches_the_cursor_and_unknown_blocks_are_refused() {
    let mut em = emulator(40, 5, 200);
    em.feed(format!("{PROMPT}long\r\n\x1b]133;C\x07first\r\nsecond").as_bytes());
    let (seen, progress) = lines(&mut em, 0, Some(1));
    assert_eq!(
        seen.iter().map(|line| line.2.as_str()).collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(progress.done());
    assert_eq!(
        em.scan_lines(0, 10, Some(99), |_| ScanFlow::Continue),
        Err(BlockTextError::NotFound)
    );
    em.feed(b"\x1b[?1049h");
    assert_eq!(
        em.scan_lines(0, 10, Some(1), |_| ScanFlow::Continue),
        Err(BlockTextError::Unavailable)
    );
}
