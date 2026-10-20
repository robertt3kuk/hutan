//! Resize and reflow (UX-TERM-RESIZE phase A): what the daemon's grid does on its own when the
//! width changes, without a shell repainting. The shell half of the story (a shell that
//! repaints its prompt after SIGWINCH on top of the reflowed rows) is the recorded stream
//! `fish_prompt_resize` in the corpus; see `docs/spikes/2026-10-21-term-resize.md`.
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

    // The cursor is on the same text after the last prompt. (The rows themselves are not
    // compared: narrowing pushes the top rows of a short screen into scrollback although
    // blank rows remain below the cursor, so a reflowed screen starts later than a fresh one.)
    let have = emulator.snapshot(&mut RenderTracker::new());
    let want = want.snapshot(&mut RenderTracker::new());
    assert_eq!(have.cursor.col, want.cursor.col);
    assert_eq!(
        text(&have.lines[usize::from(have.cursor.row)]),
        text(&want.lines[usize::from(want.cursor.row)])
    );
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
