//! The block index on synthetic streams: exact blocks and line ranges for marks split across
//! reads, synchronized batches, the alternate screen, `ESC[3J`, eviction past the scrollback
//! bound, reflow and unbalanced marks.
#![allow(clippy::expect_used, clippy::unwrap_used)] // tests assert by panicking

use std::path::PathBuf;

use hutan_protocol::blocks::{Block, BlockPart, BlockState};
use hutan_protocol::messages::TerminalSize;

use crate::blocks::{BlockTextError, MAX_BLOCKS};
use crate::emulator::Emulator;
use crate::sniffer::TerminalMeta;

fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn emulator(cols: u16, rows: u16, lines: usize) -> Emulator {
    // A tiny hot window so a few hundred lines exercise the cold store.
    Emulator::with_hot_margin(size(cols, rows), lines, 8).expect("emulator")
}

const PROMPT: &str = "\x1b]133;A\x07$ \x1b]133;B\x07";

fn command(cmd: &str, output: &str, code: i32) -> String {
    format!("{cmd}\r\n\x1b]133;C\x07{output}\x1b]133;D;{code}\x07")
}

/// Everything about a block except its times.
#[derive(Debug, PartialEq, Eq)]
struct Shape {
    id: u64,
    prompt: u64,
    input: Option<u64>,
    output: Option<u64>,
    end: Option<u64>,
    command: Option<String>,
    exit: Option<i32>,
    state: BlockState,
}

fn shape(block: &Block) -> Shape {
    Shape {
        id: block.id,
        prompt: block.prompt_line,
        input: block.command_line,
        output: block.output_start,
        end: block.end_line,
        command: block.command.clone(),
        exit: block.exit_code,
        state: block.state,
    }
}

fn all(em: &Emulator) -> Vec<Shape> {
    let mut blocks = em.blocks(None, 10_000);
    blocks.reverse();
    blocks.iter().map(shape).collect()
}

#[allow(clippy::too_many_arguments)]
fn finished(id: u64, prompt: u64, output: u64, end: u64, command: &str, exit: i32) -> Shape {
    Shape {
        id,
        prompt,
        input: Some(prompt),
        output: Some(output),
        end: Some(end),
        command: Some(command.to_owned()),
        exit: Some(exit),
        state: BlockState::Finished,
    }
}

fn text(em: &mut Emulator, id: u64, part: BlockPart) -> String {
    em.block_text(id, part, 1 << 20).expect("text").text
}

fn session() -> String {
    let mut stream = String::new();
    stream.push_str(PROMPT);
    stream.push_str(&command("true", "", 0));
    stream.push_str(PROMPT);
    stream.push_str(&command("printf 'a\\nb\\n'", "a\r\nb\r\n", 0));
    stream.push_str(PROMPT);
    stream.push_str(&command("false", "", 1));
    stream.push_str(PROMPT);
    stream
}

#[test]
fn three_commands_give_exact_blocks_and_texts() {
    let mut em = emulator(40, 24, 1000);
    em.feed(session().as_bytes());
    // Rows: 0 `$ true`, 1 `$ printf`, 2 a, 3 b, 4 `$ false`, 5 prompt.
    let mut third = finished(3, 4, 5, 5, "false", 1);
    third.state = BlockState::Finished;
    let fourth = Shape {
        id: 4,
        prompt: 5,
        input: Some(5),
        output: None,
        end: None,
        command: None,
        exit: None,
        state: BlockState::Prompt,
    };
    assert_eq!(
        all(&em),
        [
            finished(1, 0, 1, 1, "true", 0),
            finished(2, 1, 2, 4, "printf 'a\\nb\\n'", 0),
            third,
            fourth,
        ]
    );
    assert_eq!(text(&mut em, 2, BlockPart::Output), "a\nb");
    assert_eq!(text(&mut em, 2, BlockPart::Command), "printf 'a\\nb\\n'");
    assert_eq!(text(&mut em, 1, BlockPart::Output), "");
    assert_eq!(em.viewport_top(), 0);
    let times = em.blocks(None, 1);
    assert!(times[0].started_at_ms > 0 && times[0].finished_at_ms.is_none());
    let first = &em.blocks(None, 10)[3];
    assert!(first.finished_at_ms.unwrap() >= first.started_at_ms);
}

#[test]
fn every_split_of_the_stream_gives_the_same_blocks() {
    let stream = session();
    let mut whole = emulator(40, 24, 1000);
    whole.feed(stream.as_bytes());
    for piece in [1, 2, 3, 5, 7, 11] {
        let mut em = emulator(40, 24, 1000);
        for chunk in stream.as_bytes().chunks(piece) {
            em.feed(chunk);
        }
        assert_eq!(all(&em), all(&whole), "pieces of {piece}");
        assert_eq!(text(&mut em, 2, BlockPart::Output), "a\nb");
    }
}

#[test]
fn marks_are_tied_to_the_cursor_at_the_mark_not_at_the_end_of_the_read() {
    // The whole stream arrives in one read and scrolls 30 rows after the last mark.
    let mut em = emulator(20, 5, 1000);
    let mut stream = String::new();
    stream.push_str(PROMPT);
    stream.push_str(&command("seq", "1\r\n2\r\n3\r\n", 0));
    stream.push_str(PROMPT);
    for index in 0..30 {
        stream.push_str(&format!("tail{index}\r\n"));
    }
    em.feed(stream.as_bytes());
    let blocks = all(&em);
    assert_eq!(blocks[0], finished(1, 0, 1, 4, "seq", 0));
    assert_eq!(blocks[1].prompt, 4);
    assert_eq!(em.viewport_top(), 4 + 30 + 1 - 5 + 1 - 1);
    assert_eq!(text(&mut em, 1, BlockPart::Output), "1\n2\n3");
}

#[test]
fn events_follow_the_state_changes_in_order() {
    let mut em = emulator(40, 24, 1000);
    em.feed(session().as_bytes());
    let events: Vec<Block> = em
        .drain_meta()
        .into_iter()
        .filter_map(|meta| match meta {
            TerminalMeta::Block(block) => Some(block),
            _ => None,
        })
        .collect();
    use BlockState::{Finished, Prompt, Running};
    // A prompt is announced when it starts (`A`) and again once its input line is known (`B`).
    let sequence: Vec<(u64, BlockState, Option<u64>)> = events
        .iter()
        .map(|b| (b.id, b.state, b.command_line))
        .collect();
    assert_eq!(
        sequence,
        [
            (1, Prompt, None),
            (1, Prompt, Some(0)),
            (1, Running, Some(0)),
            (1, Finished, Some(0)),
            (2, Prompt, None),
            (2, Prompt, Some(1)),
            (2, Running, Some(1)),
            (2, Finished, Some(1)),
            (3, Prompt, None),
            (3, Prompt, Some(4)),
            (3, Running, Some(4)),
            (3, Finished, Some(4)),
            (4, Prompt, None),
            (4, Prompt, Some(5)),
        ]
    );
    // What the events built is what the index holds.
    let mut latest: std::collections::BTreeMap<u64, &Block> = std::collections::BTreeMap::new();
    for block in &events {
        latest.insert(block.id, block);
    }
    let built: Vec<Shape> = latest.values().map(|b| shape(b)).collect();
    assert_eq!(built, all(&em));
}

#[test]
fn a_running_block_reads_its_output_so_far() {
    let mut em = emulator(40, 24, 1000);
    em.feed(PROMPT.as_bytes());
    em.feed(b"sleep\r\n\x1b]133;C\x07one\r\ntwo");
    let running = em.blocks(None, 1)[0].clone();
    assert_eq!(running.state, BlockState::Running);
    assert_eq!(running.end_line, None);
    assert_eq!(text(&mut em, running.id, BlockPart::Output), "one\ntwo");
    // Output that ends without a newline still counts its last line.
    em.feed(b"\x1b]133;D;0\x07");
    let done = em.blocks(None, 1)[0].clone();
    assert_eq!((done.output_start, done.end_line), (Some(1), Some(3)));
    assert_eq!(text(&mut em, done.id, BlockPart::Output), "one\ntwo");
}

#[test]
fn output_text_spans_hot_and_cold_rows_and_the_byte_limit_cuts_it() {
    let mut em = emulator(20, 5, 2000);
    em.feed(PROMPT.as_bytes());
    let mut output = String::new();
    for index in 0..600 {
        output.push_str(&format!("line{index}\r\n"));
    }
    em.feed(command("many", &output, 0).as_bytes());
    assert!(em.cold_len() > 400);
    let block = em.blocks(None, 1)[0].clone();
    assert_eq!(block.output_start, Some(1));
    assert_eq!(block.end_line, Some(601));
    let full = em.block_text(block.id, BlockPart::Output, 1 << 20).unwrap();
    assert!(!full.truncated);
    let expected: Vec<String> = (0..600).map(|index| format!("line{index}")).collect();
    assert_eq!(full.text, expected.join("\n"));
    let cut = em.block_text(block.id, BlockPart::Output, 100).unwrap();
    assert!(cut.truncated);
    assert_eq!(cut.text.len(), 100);
    assert!(full.text.starts_with(&cut.text));
    assert_eq!(
        em.block_text(block.id, BlockPart::Command, 3).unwrap().text,
        "man"
    );
    assert_eq!(
        em.block_text(99, BlockPart::Output, 10).unwrap_err(),
        BlockTextError::NotFound
    );
}

#[test]
fn soft_wrapped_output_is_joined_into_one_line() {
    let mut em = emulator(10, 6, 100);
    em.feed(PROMPT.as_bytes());
    em.feed(command("long", "0123456789abcdefghij\r\nshort\r\n", 0).as_bytes());
    let id = em.blocks(None, 1)[0].id;
    assert_eq!(
        text(&mut em, id, BlockPart::Output),
        "0123456789abcdefghij\nshort"
    );
}

#[test]
fn a_long_command_wraps_and_is_read_back_whole() {
    let mut em = emulator(12, 6, 100);
    em.feed(PROMPT.as_bytes());
    em.feed(command("echo aaaaaaaaaaaaaaaaaaaa", "", 0).as_bytes());
    let block = em.blocks(None, 1)[0].clone();
    assert_eq!(block.command.as_deref(), Some("echo aaaaaaaaaaaaaaaaaaaa"));
    assert_eq!((block.output_start, block.end_line), (Some(3), Some(3)));
}

#[test]
fn marks_inside_the_alternate_screen_are_ignored_and_the_output_ends_where_it_left() {
    let mut em = emulator(20, 5, 100);
    em.feed(PROMPT.as_bytes());
    em.feed(b"vim\r\n\x1b]133;C\x07before\r\n");
    em.feed(b"\x1b[?1049h\x1b[2J\x1b[Hfull screen");
    // A nested shell's marks and a D inside the program change nothing.
    em.feed(b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;9\x07");
    assert_eq!(em.blocks(None, 10).len(), 1);
    assert_eq!(em.blocks(None, 1)[0].state, BlockState::Running);
    for _ in 0..30 {
        em.feed(b"\r\nscrolling inside the alternate screen");
    }
    assert_eq!(em.viewport_top(), 0);
    em.feed(b"\x1b[?1049l\x1b]133;D;0\x07");
    let block = em.blocks(None, 1)[0].clone();
    assert_eq!(block.state, BlockState::Finished);
    assert_eq!((block.output_start, block.end_line), (Some(1), Some(2)));
    assert_eq!(text(&mut em, block.id, BlockPart::Output), "before");
}

#[test]
fn output_scrolled_before_entering_the_alternate_screen_is_still_counted() {
    let mut em = emulator(20, 5, 100);
    em.feed(PROMPT.as_bytes());
    let mut chunk = String::from("run\r\n\x1b]133;C\x07");
    for index in 0..20 {
        chunk.push_str(&format!("o{index}\r\n"));
    }
    chunk.push_str("\x1b[?1049hTUI\x1b[?1049l");
    em.feed(chunk.as_bytes());
    // 1 command row + 20 output rows, the cursor on the 22nd row of a 5-row screen.
    assert_eq!(em.viewport_top(), 22 - 5);
    em.feed(b"\x1b]133;D;0\x07");
    let block = em.blocks(None, 1)[0].clone();
    assert_eq!((block.output_start, block.end_line), (Some(1), Some(21)));
    let out = text(&mut em, block.id, BlockPart::Output);
    assert_eq!(out.lines().count(), 20);
    assert!(out.starts_with("o0\no1\n") && out.ends_with("o19"));
    // While the alternate screen shows, the rows are out of reach.
    em.feed(b"\x1b[?1049h");
    assert_eq!(
        em.block_text(block.id, BlockPart::Output, 100).unwrap_err(),
        BlockTextError::Unavailable
    );
    assert_eq!(text(&mut em, block.id, BlockPart::Command), "run");
}

#[test]
fn erase_scrollback_truncates_the_blocks_it_erased() {
    // `clear` prints: home, erase screen (pushes the screen into the history), erase saved lines.
    let mut em = emulator(20, 4, 100);
    for index in 0..6 {
        em.feed(PROMPT.as_bytes());
        em.feed(command(&format!("echo {index}"), &format!("{index}\r\n"), 0).as_bytes());
    }
    em.feed(PROMPT.as_bytes());
    em.feed(b"clear\r\n\x1b]133;C\x07\x1b[H\x1b[2J\x1b[3J\x1b]133;D;0\x07");
    em.feed(PROMPT.as_bytes());
    let blocks = em.blocks(None, 100);
    assert_eq!(blocks.len(), 8);
    // Newest first: the new prompt is on the first row and intact; everything before was
    // scrolled away and erased.
    assert_eq!(blocks[0].state, BlockState::Prompt);
    assert_eq!(blocks[0].prompt_line, em.viewport_top());
    for block in &blocks[1..] {
        assert_eq!(block.state, BlockState::Truncated, "{block:?}");
    }
    // `clear` itself ended with its exit code even though it erased its own prompt.
    assert_eq!(blocks[1].exit_code, Some(0));
    assert_eq!(blocks[1].command.as_deref(), Some("clear"));
    assert!(blocks[1].finished_at_ms.is_some());
    assert_eq!(em.history_len(), 0);
}

#[test]
fn rows_scrolled_by_the_erase_command_are_counted_exactly() {
    // The numbering must not depend on how the stream is cut into reads. 4-row screen: `ls`
    // scrolls 3 rows, `junk` 1 more, `ESC[2J` pushes the 3 used rows of the screen.
    let stream = format!(
        "{PROMPT}{}{PROMPT}junk\r\n\x1b[H\x1b[2J\x1b[3J{PROMPT}{}",
        command("ls", "a\r\nb\r\nc\r\nd\r\ne\r\n", 0),
        command("pwd", "/\r\n", 0)
    );
    let mut whole = emulator(20, 4, 100);
    whole.feed(stream.as_bytes());
    let mut parts = emulator(20, 4, 100);
    for piece in stream.as_bytes().chunks(3) {
        parts.feed(piece);
    }
    let mut cut = emulator(20, 4, 100);
    let at = stream.find("\x1b[H").unwrap();
    cut.feed(&stream.as_bytes()[..at]);
    cut.feed(b"\x1b[H\x1b[2J");
    cut.feed(&stream.as_bytes()[at + 7..]);
    for em in [&mut whole, &mut parts, &mut cut] {
        assert_eq!(
            em.viewport_top(),
            7,
            "3 + 1 rows scrolled, then the 3 used rows pushed"
        );
    }
    assert_eq!(all(&whole), all(&parts));
    assert_eq!(all(&whole), all(&cut));
    let blocks = all(&whole);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].state, BlockState::Truncated);
    // The prompt after the erase reused the open prompt block, now on the first row.
    assert_eq!(blocks[1].prompt, 7);
    assert_eq!(blocks[1].state, BlockState::Finished);
    assert_eq!(text(&mut whole, 2, BlockPart::Output), "/");
}

#[test]
fn full_reset_truncates_everything() {
    let mut em = emulator(20, 6, 100);
    em.feed(PROMPT.as_bytes());
    em.feed(command("echo 1", "1\r\n", 0).as_bytes());
    em.feed(PROMPT.as_bytes());
    em.feed(b"reset\r\n\x1b]133;C\x07\x1bc\x1b]133;D;0\x07");
    em.feed(PROMPT.as_bytes());
    let blocks = em.blocks(None, 10);
    assert_eq!(blocks[0].state, BlockState::Prompt);
    assert_eq!(blocks[1].state, BlockState::Truncated);
    assert_eq!(blocks[2].state, BlockState::Truncated);
    assert_eq!(blocks[0].prompt_line, em.viewport_top());
}

#[test]
fn evicted_blocks_are_marked_truncated_exactly_when_their_prompt_line_is_gone() {
    let mut em = emulator(20, 5, 60);
    for index in 0..40 {
        em.feed(PROMPT.as_bytes());
        let output: String = (0..7).map(|row| format!("c{index}r{row}\r\n")).collect();
        em.feed(command(&format!("cmd{index}"), &output, 0).as_bytes());
    }
    em.feed(PROMPT.as_bytes());
    assert_eq!(em.history_len(), 60);
    let first_retained = em.viewport_top() - 60;
    let blocks = em.blocks(None, 1000);
    assert_eq!(blocks.len(), 41);
    let mut truncated = 0;
    for block in &blocks {
        let gone = block.prompt_line < first_retained;
        assert_eq!(block.state == BlockState::Truncated, gone, "{block:?}");
        truncated += usize::from(gone);
    }
    assert!(truncated > 25 && truncated < 40);
    // A block that is still retained reads back whole; its predecessor is cut.
    let newest = &blocks[1];
    assert_eq!(newest.command.as_deref(), Some("cmd39"));
    let expected: Vec<String> = (0..7).map(|row| format!("c39r{row}")).collect();
    assert_eq!(
        text(&mut em, newest.id, BlockPart::Output),
        expected.join("\n")
    );
    // Lines of a truncated block that are gone are reported as lost.
    let oldest = blocks.last().unwrap();
    let lost = em.block_text(oldest.id, BlockPart::Output, 1000).unwrap();
    assert!(lost.truncated && lost.text.is_empty());
    assert_eq!(oldest.command.as_deref(), Some("cmd0"));
}

#[test]
fn a_block_whose_output_is_partly_evicted_returns_the_rest_and_says_so() {
    let mut em = emulator(20, 5, 30);
    em.feed(PROMPT.as_bytes());
    let output: String = (0..100).map(|row| format!("r{row}\r\n")).collect();
    em.feed(command("flood", &output, 0).as_bytes());
    let block = em.blocks(None, 1)[0].clone();
    assert_eq!(block.state, BlockState::Truncated);
    let got = em.block_text(block.id, BlockPart::Output, 10_000).unwrap();
    assert!(got.truncated);
    assert!(got.text.ends_with("r99"));
    // 30 history rows and the viewport rows up to the end of the output.
    assert_eq!(got.text.lines().count(), 34);
}

#[test]
fn at_most_two_thousand_blocks_are_kept_and_paged_newest_first() {
    let mut em = emulator(20, 4, 100);
    for _ in 0..MAX_BLOCKS + 500 {
        em.feed(PROMPT.as_bytes());
        em.feed(command("x", "", 0).as_bytes());
    }
    let newest = em.blocks(None, 3);
    assert_eq!(
        newest.iter().map(|b| b.id).collect::<Vec<_>>(),
        [2500, 2499, 2498]
    );
    let mut ids = Vec::new();
    let mut before = None;
    loop {
        let page = em.blocks(before, 500);
        let Some(last) = page.last() else { break };
        before = Some(last.id);
        ids.extend(page.iter().map(|block| block.id));
    }
    assert_eq!(ids.len(), MAX_BLOCKS);
    assert_eq!(ids.first(), Some(&2500));
    assert_eq!(ids.last(), Some(&501));
    assert!(ids.windows(2).all(|pair| pair[0] == pair[1] + 1));
    assert_eq!(
        em.block_text(1, BlockPart::Command, 10).unwrap_err(),
        BlockTextError::NotFound
    );
}

#[test]
fn unbalanced_marks_are_handled_leniently() {
    let mut em = emulator(30, 10, 100);
    // D with nothing running; a redrawn prompt; an empty Enter.
    em.feed(b"\x1b]133;D;3\x07");
    assert!(em.blocks(None, 10).is_empty());
    em.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    em.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    em.feed(b"\r\n\x1b]133;A\x07$ \x1b]133;B\x07");
    let blocks = em.blocks(None, 10);
    assert_eq!(blocks.len(), 1, "one open prompt block is reused");
    assert_eq!(blocks[0].prompt_line, 1);
    // Output without a B; a second C while running.
    em.feed(b"cmd\r\n\x1b]133;C\x07\x1b]133;C\x07out\r\n");
    // The next prompt without a D finishes it.
    em.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    let blocks = em.blocks(None, 10);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[1].state, BlockState::Finished);
    assert_eq!(blocks[1].exit_code, None);
    assert_eq!(
        (blocks[1].output_start, blocks[1].end_line),
        (Some(2), Some(3))
    );
    assert_eq!(blocks[0].state, BlockState::Prompt);
    // C and D alone (no A, no B) still make a block.
    let mut bare = emulator(30, 10, 100);
    bare.feed(b"out\r\n\x1b]133;C\x07x\r\n\x1b]133;D;2\x07");
    let only = bare.blocks(None, 10);
    assert_eq!(only.len(), 1);
    assert_eq!(
        (only[0].state, only[0].exit_code),
        (BlockState::Finished, Some(2))
    );
}

#[test]
fn the_working_directory_comes_from_osc_7_before_the_prompt() {
    let mut em = emulator(30, 10, 100);
    em.feed(b"\x1b]7;file://host/tmp/one\x07");
    em.feed(PROMPT.as_bytes());
    em.feed(command("cd /tmp/two", "", 0).as_bytes());
    em.feed(b"\x1b]7;file://host/tmp/two\x07");
    em.feed(PROMPT.as_bytes());
    let blocks = em.blocks(None, 10);
    assert_eq!(blocks[1].cwd, Some(PathBuf::from("/tmp/one")));
    assert_eq!(blocks[0].cwd, Some(PathBuf::from("/tmp/two")));
    // fish prints OSC 7 after the prompt mark: the open prompt takes it.
    em.feed(b"\x1b]7;file://host/tmp/three\x07");
    assert_eq!(em.blocks(None, 1)[0].cwd, Some(PathBuf::from("/tmp/three")));
}

#[test]
fn marks_inside_a_synchronized_batch_land_where_the_batch_left_the_cursor() {
    let mut em = emulator(20, 10, 100);
    em.feed(PROMPT.as_bytes());
    em.feed(b"cmd\r\n\x1b]133;C\x07");
    // The batch is open when the mark arrives: it waits for the batch to be executed.
    em.feed(b"\x1b[?2026hout\r\n\x1b]133;D;0\x07");
    assert_eq!(em.blocks(None, 1)[0].state, BlockState::Running);
    em.feed(b"\x1b[?2026l");
    let block = em.blocks(None, 1)[0].clone();
    assert_eq!(block.state, BlockState::Finished);
    assert_eq!((block.output_start, block.end_line), (Some(1), Some(2)));
    assert_eq!(text(&mut em, block.id, BlockPart::Output), "out");
    // A batch that never closes is committed by the sync timeout.
    em.feed(b"\x1b[?2026h\x1b]133;A\x07");
    assert_eq!(em.blocks(None, 10).len(), 1);
    em.stop_sync();
    assert_eq!(em.blocks(None, 10).len(), 2);
}

#[test]
fn reflow_on_resize_keeps_blocks_on_their_lines() {
    let mut em = emulator(20, 6, 100);
    em.feed(PROMPT.as_bytes());
    em.feed(command("one", "aaaaaaaaaabbbbbbbbbbcccc\r\nshort\r\n", 0).as_bytes());
    em.feed(PROMPT.as_bytes());
    em.feed(command("two", "tail\r\n", 0).as_bytes());
    em.feed(PROMPT.as_bytes());
    let before = all(&em);
    assert_eq!(
        text(&mut em, 1, BlockPart::Output),
        "aaaaaaaaaabbbbbbbbbbcccc\nshort"
    );
    for width in [10u16, 40, 7, 20] {
        em.resize(size(width, 6)).unwrap();
        assert_eq!(
            text(&mut em, 1, BlockPart::Output),
            "aaaaaaaaaabbbbbbbbbbcccc\nshort",
            "width {width}"
        );
        assert_eq!(text(&mut em, 2, BlockPart::Output), "tail", "width {width}");
        assert_eq!(em.blocks(None, 1)[0].state, BlockState::Prompt);
    }
    let after = all(&em);
    assert_eq!(
        after
            .iter()
            .map(|s| (s.id, s.state, s.exit))
            .collect::<Vec<_>>(),
        before
            .iter()
            .map(|s| (s.id, s.state, s.exit))
            .collect::<Vec<_>>()
    );
    // Rows only: lines stay where they are, history is pulled in and pushed out.
    for rows in [3u16, 12, 5] {
        em.resize(size(20, rows)).unwrap();
        assert_eq!(text(&mut em, 2, BlockPart::Output), "tail", "rows {rows}");
        assert_eq!(
            text(&mut em, 1, BlockPart::Output),
            "aaaaaaaaaabbbbbbbbbbcccc\nshort"
        );
    }
}

#[test]
fn blocks_survive_scrolling_with_a_small_scrollback_through_resizes() {
    let mut em = emulator(20, 5, 40);
    for index in 0..30 {
        em.feed(PROMPT.as_bytes());
        em.feed(command(&format!("n{index}"), &format!("out{index}\r\n"), 0).as_bytes());
        if index % 7 == 3 {
            em.resize(size(12 + (index as u16 % 5) * 4, 5 + (index as u16 % 3)))
                .unwrap();
        }
    }
    let blocks = em.blocks(None, 5);
    for block in blocks.iter().filter(|b| b.state == BlockState::Finished) {
        let n = block.command.clone().unwrap();
        assert_eq!(
            text(&mut em, block.id, BlockPart::Output),
            n.replace('n', "out"),
            "{block:?}"
        );
    }
}

#[test]
fn the_viewport_top_counts_every_scrolled_row() {
    let mut em = emulator(10, 4, 50);
    for _ in 0..200 {
        em.feed(b"x\r\n");
    }
    // 200 line feeds on a 4-row screen: the cursor is on row 3 after 3, then each scrolls one.
    assert_eq!(em.viewport_top(), 200 - 3);
    em.feed(b"\x1b[3J");
    assert_eq!(em.viewport_top(), 197);
    em.feed(b"y\r\n");
    assert_eq!(em.viewport_top(), 198);
}

/// What a client knows of the blocks: the last event of each id, as the desktop keeps them.
#[derive(Default)]
struct Mirror(std::collections::BTreeMap<u64, Block>);

impl Mirror {
    fn catch_up(&mut self, em: &mut Emulator) {
        for meta in em.drain_meta() {
            if let TerminalMeta::Block(block) = meta {
                self.0.insert(block.id, block);
            }
        }
    }

    fn shapes(&self) -> Vec<Shape> {
        self.0.values().map(shape).collect()
    }
}

/// The text of the row at absolute line `line`, in the viewport or the history.
fn row_at(em: &mut Emulator, line: u64) -> String {
    let top = em.viewport_top();
    let rows = u64::from(em.size().rows);
    match line.checked_sub(top) {
        Some(row) if row < rows => {
            let grid = em.grid_text();
            grid.lines()
                .nth(row as usize)
                .unwrap_or("")
                .trim_end()
                .to_owned()
        }
        _ => {
            let row = &em.history((top - line - 1) as u32, 1)[0];
            let text: String = row.runs.iter().map(|run| run.text.as_str()).collect();
            text.trim_end().to_owned()
        }
    }
}

#[test]
fn the_events_follow_the_blocks_through_every_reflow() {
    // A shell session as the user recorded it: wrapped output in the middle, a failing command
    // after it, one more behind that. Once on a tall screen and once on a short one (the blocks
    // reach into the scrollback).
    let long = "bench *args   # Measure the performance budgets B1-B6, B3p, B7-B10 (docs/perf.md); not part of the gate.\r\n";
    let list = format!("Available recipes:\r\n{long}    default\r\n    deny\r\n    fmt\r\n");
    for rows in [30u16, 7] {
        let mut em = emulator(60, rows, 500);
        let mut mirror = Mirror::default();
        em.feed(PROMPT.as_bytes());
        em.feed(command("git status -sb", "## master...origin/master\r\n", 0).as_bytes());
        em.feed(PROMPT.as_bytes());
        em.feed(command("just --list | head -8", &list, 0).as_bytes());
        em.feed(PROMPT.as_bytes());
        let failing = command(
            "git switch release/0.4",
            "fatal: invalid reference: release/0.4\r\n",
            128,
        );
        em.feed(failing.as_bytes());
        em.feed(PROMPT.as_bytes());
        em.feed(command("ls crates | wc -l", "22\r\n", 0).as_bytes());
        em.feed(PROMPT.as_bytes());
        mirror.catch_up(&mut em);
        assert_eq!(mirror.shapes(), all(&em), "rows {rows}: before");
        for width in [30u16, 18, 44, 60, 25, 80] {
            em.resize(size(width, rows)).unwrap();
            mirror.catch_up(&mut em);
            let truth = all(&em);
            assert_eq!(mirror.shapes(), truth, "rows {rows}, width {width}");
            // The failing command keeps its exit code, and its rows are its own.
            let failed = truth.iter().find(|b| b.exit == Some(128)).expect("failed");
            assert_eq!(failed.command.as_deref(), Some("git switch release/0.4"));
            if failed.state == BlockState::Finished {
                assert_eq!(
                    text(&mut em, failed.id, BlockPart::Output),
                    "fatal: invalid reference: release/0.4"
                );
                let prompt = row_at(&mut em, failed.prompt);
                assert!(
                    prompt.starts_with("$ git switch"),
                    "rows {rows}, width {width}: {prompt:?}"
                );
            }
        }
    }
}
