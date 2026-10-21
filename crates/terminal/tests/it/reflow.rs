//! Resize and reflow (UX-TERM-RESIZE phases A and B): what the daemon's grid does on its own when
//! the width changes, without a shell repainting; scripted shells that repaint by moving up the
//! rows they believe their prompt took (one-line, two-line, with a right prompt); and where the
//! blocks sit afterwards. The recorded shells (fish, zsh) are in the corpus, see
//! `docs/spikes/2026-10-21-term-resize.md`.
#![allow(clippy::expect_used)] // tests assert by panicking

use hutan_protocol::messages::TerminalSize;
use hutan_protocol::render::Row;
use hutan_terminal::{Emulator, RenderTracker};

fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

const PROMPT: &str = "developer@dev-workstation /p/v/f/9/1/T/t/h/u/a/l/p/proj-demo> ";

/// Two commands with their output, each behind the 62-column prompt, the way a shell leaves a
/// screen; the cursor ends after the last prompt.
fn session() -> Vec<u8> {
    let mut out = Vec::new();
    for (command, output) in [
        (
            "ls -la",
            &[
                "total 24",
                "-rw-r--r--@ 1 dev  staff  1 Oct 21 03:50 Cargo.toml",
            ][..],
        ),
        ("echo hello", &["hello"][..]),
    ] {
        out.extend_from_slice(format!("{PROMPT}{command}\r\n").as_bytes());
        for line in output {
            out.extend_from_slice(format!("{line}\r\n").as_bytes());
        }
    }
    out.extend_from_slice(PROMPT.as_bytes());
    out
}

fn text(row: &Row) -> String {
    row.runs.iter().map(|run| run.text.as_str()).collect()
}

/// Scrollback then screen, soft-wrapped rows joined into logical lines, trailing blank lines
/// dropped: what the user would copy, independent of where the rows break.
fn logical_lines(emulator: &mut Emulator) -> Vec<String> {
    let snapshot = emulator.snapshot(&mut RenderTracker::new());
    let mut rows: Vec<Row> = emulator
        .history(0, snapshot.history_len)
        .into_iter()
        .rev()
        .collect();
    rows.extend(snapshot.lines);
    let mut lines = Vec::new();
    let mut current = String::new();
    for row in &rows {
        if row.wrapped {
            current.push_str(&text(row));
        } else {
            current.push_str(text(row).trim_end());
            lines.push(std::mem::take(&mut current));
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

fn fresh(cols: u16, rows: u16) -> Emulator {
    let mut emulator = Emulator::new(size(cols, rows), 1000).expect("emulator");
    emulator.feed(&session());
    emulator
}

/// The audit's nine width changes (dock and split toggles), with the heights that went with
/// them.
const NINE: [(u16, u16); 9] = [
    (60, 24),
    (44, 24),
    (44, 12),
    (70, 12),
    (100, 24),
    (55, 24),
    (40, 24),
    (90, 24),
    (60, 24),
];

#[test]
fn a_wrapped_prompt_after_nine_width_changes_equals_a_fresh_render_at_the_final_width() {
    let mut emulator = fresh(100, 24);
    for (cols, rows) in NINE {
        emulator.resize(size(cols, rows)).expect("resize");
    }
    let (cols, rows) = NINE[NINE.len() - 1];
    let mut want = fresh(cols, rows);
    assert_eq!(logical_lines(&mut emulator), logical_lines(&mut want));

    // The screen is the same rows, and the cursor is on the same text after the last prompt:
    // narrowing does not push the top rows of a short screen into scrollback while blank rows
    // remain below the cursor.
    assert_eq!(emulator.grid_text(), want.grid_text());
    assert_eq!(emulator.history_len(), want.history_len());
    let have = emulator.snapshot(&mut RenderTracker::new());
    let want = want.snapshot(&mut RenderTracker::new());
    assert_eq!(have.cursor, want.cursor);
}

#[test]
fn every_intermediate_width_also_equals_a_fresh_render() {
    let mut emulator = fresh(100, 24);
    for (cols, rows) in NINE {
        emulator.resize(size(cols, rows)).expect("resize");
        let mut want = fresh(cols, rows);
        assert_eq!(
            logical_lines(&mut emulator),
            logical_lines(&mut want),
            "after the resize to {cols}x{rows}"
        );
        // The screen too, row by row: narrowing does not push the top rows into scrollback
        // while blank rows remain below the cursor.
        assert_eq!(
            emulator.grid_text(),
            want.grid_text(),
            "screen after the resize to {cols}x{rows}"
        );
        assert_eq!(
            emulator.history_len(),
            want.history_len(),
            "history at {cols}x{rows}"
        );
    }
}

/// A shell at its prompt, as fish and zsh behave when the terminal width changes: it repaints
/// the prompt for the new width after SIGWINCH by moving up the rows it believes the prompt
/// occupied at the old width, erasing downwards and drawing again. It cannot know how the
/// terminal broke the old prompt into rows.
struct Shell {
    marks: bool,
    cols: u16,
    command: &'static str,
}

impl Shell {
    fn prompt_rows(&self) -> usize {
        (PROMPT.len() + self.command.len()).div_ceil(usize::from(self.cols))
    }

    fn draw(&self) -> Vec<u8> {
        let (a, b) = if self.marks {
            ("\x1b]133;A\x1b\\", "\x1b]133;B\x1b\\")
        } else {
            ("", "")
        };
        format!("{a}{PROMPT}{b}{}", self.command).into_bytes()
    }

    /// What the shell writes after a resize to `cols`.
    fn repaint(&mut self, cols: u16) -> Vec<u8> {
        let up = self.prompt_rows() - 1;
        self.cols = cols;
        let mut out = b"\r".to_vec();
        if up > 0 {
            out.extend_from_slice(format!("\x1b[{up}A").as_bytes());
        }
        out.extend_from_slice(b"\x1b[J");
        out.extend(self.draw());
        out
    }
}

fn at_a_prompt(marks: bool, cols: u16, rows: u16) -> (Emulator, Shell) {
    let mut emulator = Emulator::new(size(cols, rows), 1000).expect("emulator");
    let mut shell = Shell {
        marks,
        cols,
        command: "",
    };
    for (command, output) in [("ls -la", "total 24"), ("echo hello", "hello")] {
        shell.command = command;
        emulator.feed(&shell.draw());
        emulator.feed(format!("\r\n{output}\r\n").as_bytes());
    }
    shell.command = "git sta";
    emulator.feed(&shell.draw());
    (emulator, shell)
}

/// What a fresh render at `cols` shows: the same screen, the shell having drawn its prompt
/// there from the start.
fn at_a_prompt_fresh(marks: bool, cols: u16, rows: u16) -> Emulator {
    at_a_prompt(marks, cols, rows).0
}

fn resize_with_repaint(emulator: &mut Emulator, shell: &mut Shell, cols: u16, rows: u16) {
    emulator.resize(size(cols, rows)).expect("resize");
    emulator.feed(&shell.repaint(cols));
}

#[test]
fn a_prompt_the_shell_repaints_stays_clean_through_width_changes() {
    let (mut emulator, mut shell) = at_a_prompt(true, 100, 24);
    for (cols, rows) in NINE {
        resize_with_repaint(&mut emulator, &mut shell, cols, rows);
        let mut want = at_a_prompt_fresh(true, cols, rows);
        let have = emulator.snapshot(&mut RenderTracker::new());
        let want_snapshot = want.snapshot(&mut RenderTracker::new());
        // The rows at and below the prompt are exactly a fresh render's: one prompt, the typed
        // text after it, nothing stacked above or below.
        let rows_of = |snapshot: &hutan_protocol::render::TerminalSnapshot| {
            let at = usize::from(snapshot.cursor.row);
            let prompt_rows = (PROMPT.len() + "git sta".len()).div_ceil(usize::from(cols));
            // Nothing below the cursor.
            assert!(
                snapshot.lines[at + 1..]
                    .iter()
                    .all(|row| row.runs.is_empty())
            );
            snapshot.lines[at + 1 - prompt_rows..=at].to_vec()
        };
        assert_eq!(
            rows_of(&have),
            rows_of(&want_snapshot),
            "after the resize to {cols}x{rows}"
        );
        assert_eq!(have.cursor.col, want_snapshot.cursor.col);
        // Everything above the prompt is the earlier output, reflowed and unharmed.
        let lines = logical_lines(&mut emulator);
        let mut want = at_a_prompt_fresh(true, cols, rows);
        assert_eq!(lines, logical_lines(&mut want), "after {cols}x{rows}");
    }
}

#[test]
fn without_prompt_marks_the_reflowed_prompt_and_the_repaint_stack() {
    // A shell that sends no OSC 133 marks (no integration, an old shell): nothing says the
    // prompt is a prompt, so the rows are reflowed as output and the repaint, which moves up
    // by the rows the *old* width gave, lands inside them. Pinned as what happens today.
    let (mut emulator, mut shell) = at_a_prompt(false, 100, 24);
    for (cols, rows) in NINE {
        resize_with_repaint(&mut emulator, &mut shell, cols, rows);
    }
    let (cols, rows) = NINE[NINE.len() - 1];
    let mut want = at_a_prompt_fresh(false, cols, rows);
    assert_ne!(logical_lines(&mut emulator), logical_lines(&mut want));
}

// ---- blocks across reflow (UX-TERM-RESIZE phase B) ----------------------------------------

/// A session with every mark (`A`, `B`, `C`, `D`): `count` commands whose output has a long line
/// that wraps when the terminal is narrow, then an open prompt. Returns the bytes and, per
/// command, its output's first line.
fn marked_session(count: usize) -> (Vec<u8>, Vec<String>) {
    let mut out = Vec::new();
    let mut first_lines = Vec::new();
    for i in 0..count {
        let first = format!("out{i}-first {}", "x".repeat(50));
        out.extend_from_slice(
            format!(
                "\x1b]133;A\x1b\\{PROMPT}\x1b]133;B\x1b\\cmd{i}\r\n\x1b]133;C\x1b\\{first}\r\nshort{i}\r\n\x1b]133;D;0\x1b\\"
            )
            .as_bytes(),
        );
        first_lines.push(first);
    }
    out.extend_from_slice(format!("\x1b]133;A\x1b\\{PROMPT}\x1b]133;B\x1b\\").as_bytes());
    (out, first_lines)
}

/// The row of absolute line `line`, and whether the row before it soft-wraps into it.
fn row_of_line(emulator: &mut Emulator, line: u64) -> (String, bool) {
    let snapshot = emulator.snapshot(&mut RenderTracker::new());
    let top = snapshot.viewport_top;
    let at = |rows: &[Row], index: usize| rows.get(index).map(|row| (text(row), false));
    if line >= top {
        let index = usize::try_from(line - top).expect("index");
        let wrapped_before = index > 0 && snapshot.lines[index - 1].wrapped;
        let (text, _) = at(&snapshot.lines, index).expect("a viewport row");
        return (text, wrapped_before);
    }
    let history = emulator.history(0, snapshot.history_len);
    // History rows are newest first, starting at the line above the viewport.
    let index = usize::try_from(top - 1 - line).expect("index");
    let wrapped_before = history.get(index + 1).is_some_and(|row| row.wrapped);
    (text(&history[index]), wrapped_before)
}

fn assert_blocks_on_their_rows(emulator: &mut Emulator, first_lines: &[String], context: &str) {
    let blocks = emulator.blocks(None, 100);
    let hot: Vec<_> = blocks.into_iter().rev().collect();
    assert!(!hot.is_empty(), "{context}: blocks");
    for block in &hot {
        let Some(command) = block.command.as_deref() else {
            continue;
        };
        let i: usize = command.trim_start_matches("cmd").parse().expect("cmdN");
        let (prompt_row, wrapped) = row_of_line(emulator, block.prompt_line);
        assert!(
            prompt_row.starts_with("developer@") && !wrapped,
            "{context}: block {i} starts on a prompt row, not a wrap row: {prompt_row:?} wrapped={wrapped}"
        );
        let start = block.output_start.expect("output start");
        let (output_row, wrapped) = row_of_line(emulator, start);
        assert!(
            output_row.starts_with(&first_lines[i][..first_lines[i].len().min(output_row.len())])
                && !wrapped,
            "{context}: block {i} output starts at its first line: {output_row:?} wrapped={wrapped}"
        );
    }
}

#[test]
fn blocks_stay_on_their_rows_through_width_changes() {
    let (bytes, first_lines) = marked_session(40);
    let mut emulator = Emulator::new(size(100, 24), 1000).expect("emulator");
    emulator.feed(&bytes);
    assert_blocks_on_their_rows(&mut emulator, &first_lines, "before any resize");
    let mut shell = Shell {
        marks: true,
        cols: 100,
        command: "",
    };
    for (cols, rows) in NINE {
        resize_with_repaint(&mut emulator, &mut shell, cols, rows);
        assert_blocks_on_their_rows(&mut emulator, &first_lines, &format!("after {cols}x{rows}"));
    }
}

/// A recorded stream with its size and its resizes (`OFFSET COLSxROWS`), see the corpus README.
fn recorded(name: &str) -> (Vec<u8>, TerminalSize, Vec<(usize, TerminalSize)>) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/recorded");
    let dims = |spec: &str| {
        let (cols, rows) = spec.trim().split_once('x').expect("COLSxROWS");
        size(cols.parse().expect("cols"), rows.parse().expect("rows"))
    };
    let raw = std::fs::read(dir.join(format!("{name}.raw"))).expect("recorded stream");
    let start = dims(&std::fs::read_to_string(dir.join(format!("{name}.size"))).expect("size"));
    let resizes = std::fs::read_to_string(dir.join(format!("{name}.resizes")))
        .expect("resizes")
        .lines()
        .map(|line| {
            let (offset, spec) = line.split_once(' ').expect("OFFSET COLSxROWS");
            (offset.parse().expect("offset"), dims(spec))
        })
        .collect();
    (raw, start, resizes)
}

/// Replays `name`; after the bytes before every resize and after the last byte, `check` looks
/// at the emulator (`step` 0 is before the first resize).
fn replay_checking(name: &str, mut check: impl FnMut(&mut Emulator, usize, TerminalSize)) {
    let (raw, start, resizes) = recorded(name);
    let mut emulator = Emulator::new(start, 10_000).expect("emulator");
    let (mut fed, mut at) = (0, start);
    for (step, (offset, size)) in resizes.iter().enumerate() {
        emulator.feed(&raw[fed..*offset]);
        fed = *offset;
        check(&mut emulator, step, at);
        emulator.resize(*size).expect("resize");
        at = *size;
    }
    emulator.feed(&raw[fed..]);
    check(&mut emulator, resizes.len(), at);
}

/// Every block starts on the first row of its line (never on a wrap row), its command row is
/// where the prompt ends, and its output starts on the row of the output's first line.
fn assert_recorded_blocks_on_their_rows(emulator: &mut Emulator, prompt: &str, context: &str) {
    let blocks: Vec<_> = emulator.blocks(None, 100).into_iter().rev().collect();
    assert!(!blocks.is_empty(), "{context}: no blocks");
    for block in &blocks {
        let (first, wrapped) = row_of_line(emulator, block.prompt_line);
        // fish cuts a first line wider than the terminal at its start and shows `…`.
        assert!(
            (first.starts_with(prompt) || first.starts_with('…')) && !wrapped,
            "{context}: block {} starts on {first:?} (wrap row: {wrapped})",
            block.id
        );
        if let Some(start) = block.output_start {
            let output = emulator
                .block_text(block.id, hutan_protocol::blocks::BlockPart::Output, 4096)
                .expect("output text")
                .text;
            let first_line = output.lines().next().unwrap_or_default();
            let (row, wrapped) = row_of_line(emulator, start);
            let row = row.trim_end();
            assert!(
                !wrapped && (first_line.starts_with(row) || row.starts_with(first_line)),
                "{context}: block {} output starts on {row:?}, its first line is {first_line:?} (wrap row: {wrapped})",
                block.id
            );
        }
    }
}

#[test]
fn recorded_zsh_and_fish_blocks_stay_on_their_rows_through_resizes() {
    // zsh with Hutan's injected marks, fish with its own; both with a two-line prompt and a
    // right prompt (`RPROMPT`, `fish_right_prompt`), five width changes while it sits at a
    // prompt and after commands whose output wraps.
    for name in ["zsh_two_line_resize", "fish_two_line_resize"] {
        replay_checking(name, |emulator, step, size| {
            assert_recorded_blocks_on_their_rows(
                emulator,
                "dev@devbox",
                &format!("{name}, step {step} at {}x{}", size.cols, size.rows),
            );
        });
    }
}

/// A two-line prompt with a right prompt, as zsh draws it: the first line is longer than a
/// narrow terminal, the second ends where the command is typed and has the right prompt
/// (`ESC[K`, move right, text, move back) at the far edge.
struct TwoLineShell {
    cols: u16,
    command: &'static str,
}

const FIRST_LINE: &str = "dev@devbox /t/h/a/b/c/d/e/a-long-project-folder-name-demo on main";
const RIGHT: &str = "[12:00:00]";

impl TwoLineShell {
    fn rows(&self) -> usize {
        // The first line wraps; the second line (`❯ ` and the command) may too.
        FIRST_LINE.len().div_ceil(usize::from(self.cols))
            + (2 + self.command.len()).div_ceil(usize::from(self.cols))
    }

    fn draw(&self) -> Vec<u8> {
        let cols = usize::from(self.cols);
        let mut out = format!("\x1b]133;A\x1b\\{FIRST_LINE}\r\n❯ \x1b]133;B\x1b\\").into_bytes();
        // The right prompt only fits when the second line leaves room for it.
        // One column short of the edge, as zsh does, so the cursor never wraps.
        let at = cols.saturating_sub(RIGHT.len() + 1);
        if 2 + self.command.len() + 1 < at {
            // From the end of `❯ ` (column 2): erase, jump right, draw, come back.
            out.extend_from_slice(
                format!(
                    "\x1b[K\x1b[{}C{RIGHT}\x1b[{}D",
                    at - 2,
                    at + RIGHT.len() - 2
                )
                .as_bytes(),
            );
        }
        out.extend_from_slice(self.command.as_bytes());
        out
    }

    fn repaint(&mut self, cols: u16) -> Vec<u8> {
        let up = self.rows() - 1;
        self.cols = cols;
        let mut out = b"\r".to_vec();
        if up > 0 {
            out.extend_from_slice(format!("\x1b[{up}A").as_bytes());
        }
        out.extend_from_slice(b"\x1b[J");
        out.extend(self.draw());
        out
    }
}

fn two_line_at_a_prompt(cols: u16, rows: u16) -> (Emulator, TwoLineShell) {
    let mut emulator = Emulator::new(size(cols, rows), 1000).expect("emulator");
    let mut shell = TwoLineShell { cols, command: "" };
    for (command, output) in [("ls -la", "total 24"), ("echo hello", "hello")] {
        shell.command = command;
        emulator.feed(&shell.draw());
        emulator.feed(format!("\r\n\x1b]133;C\x1b\\{output}\r\n\x1b]133;D;0\x1b\\").as_bytes());
    }
    shell.command = "git sta";
    emulator.feed(&shell.draw());
    (emulator, shell)
}

#[test]
fn a_two_line_prompt_with_a_right_prompt_equals_a_fresh_render_after_each_width_change() {
    let (mut emulator, mut shell) = two_line_at_a_prompt(100, 24);
    for (cols, rows) in NINE {
        emulator.resize(size(cols, rows)).expect("resize");
        emulator.feed(&shell.repaint(cols));
        let (mut want, _) = two_line_at_a_prompt(cols, rows);
        let have = emulator.snapshot(&mut RenderTracker::new());
        let want_snapshot = want.snapshot(&mut RenderTracker::new());
        // The prompt's rows (both lines, the right prompt, the typed text) and nothing below.
        let prompt_rows = |snapshot: &hutan_protocol::render::TerminalSnapshot| {
            let at = usize::from(snapshot.cursor.row);
            let count = TwoLineShell {
                cols,
                command: "git sta",
            }
            .rows();
            assert!(
                snapshot.lines[at + 1..]
                    .iter()
                    .all(|row| row.runs.is_empty()),
                "nothing below the cursor at {cols}x{rows}"
            );
            snapshot.lines[at + 1 - count..=at]
                .iter()
                .map(text)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            prompt_rows(&have),
            prompt_rows(&want_snapshot),
            "at {cols}x{rows}"
        );
        assert_eq!(
            have.cursor.col, want_snapshot.cursor.col,
            "cursor at {cols}x{rows}"
        );
        // And everything above: the earlier prompts and output, reflowed, no fragments. (The
        // right prompts of finished prompts keep the column they were drawn at, so only the
        // text is compared.)
        let words = |emulator: &mut Emulator| {
            logical_lines(emulator)
                .iter()
                .map(|line| {
                    line.replace(RIGHT, "")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            words(&mut emulator),
            words(&mut want),
            "text at {cols}x{rows}"
        );
    }
}
