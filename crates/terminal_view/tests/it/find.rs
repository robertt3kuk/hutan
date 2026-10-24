#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns
//! What the view draws of a find: the cells of the matches on screen (every one tinted, the
//! current one stronger), matches that stay with their text while output scrolls, matches in the
//! scrollback that the view scrolls to and loads, and ⌘F, which never reaches the shell. GPUI's
//! test text system gives 7.8 × 15.6 px cells.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext, px, size};
use hutan_protocol::blocks::BlockPart;
use hutan_protocol::render::{
    CellFlags, Color, Cursor, CursorShape, Modes, Palette, Row, Run, TerminalFrame, TerminalRender,
    TerminalSnapshot,
};
use hutan_terminal_view::{
    DefaultColors, FindHighlights, FindSpan, TerminalBackend, TerminalSize, TerminalView,
    TerminalViewSettings,
};

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Input(Vec<u8>),
    History(u32, u32),
}

#[derive(Default)]
struct Recorder(RefCell<Vec<Call>>);

impl Recorder {
    fn take(&self) -> Vec<Call> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

impl TerminalBackend for Recorder {
    fn input(&self, bytes: Vec<u8>) {
        self.0.borrow_mut().push(Call::Input(bytes));
    }
    fn resize(&self, _: TerminalSize) {}
    fn ack(&self, _: u64) {}
    fn request_history(&self, from_line: u32, count: u32) {
        self.0.borrow_mut().push(Call::History(from_line, count));
    }
    fn set_default_colors(&self, _: DefaultColors) {}
    fn block_text(&self, _: u64, _: BlockPart) -> gpui::Task<Option<String>> {
        gpui::Task::ready(None)
    }
}

/// Rows of the grid: 600 / 15.6.
const ROWS: u16 = 38;

fn setup(cx: &mut TestAppContext) -> (Entity<TerminalView>, &mut VisualTestContext, Rc<Recorder>) {
    let recorder = Rc::new(Recorder::default());
    let backend: Rc<dyn TerminalBackend> = recorder.clone();
    let (view, cx) = cx.add_window_view(|window, cx| {
        TerminalView::new(backend, TerminalViewSettings::default(), window, cx)
    });
    cx.simulate_resize(size(px(800.0), px(600.0)));
    cx.run_until_parked();
    draw(cx);
    (view, cx, recorder)
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.simulate_next_frame(cx);
    });
    cx.run_until_parked();
}

fn row(text: &str) -> Row {
    Row {
        runs: vec![Run {
            text: text.to_owned(),
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            underline_color: None,
            link: None,
        }],
        wrapped: false,
    }
}

fn cursor() -> Cursor {
    Cursor {
        col: 0,
        row: 0,
        shape: CursorShape::Block,
        visible: true,
        blinking: false,
    }
}

/// `ROWS` screen rows `s0, s1, …` over `history` scrolled-away rows; absolute row `n` is the
/// first row of the screen when `n == history`.
fn snapshot(frame: u64, history: u32) -> TerminalRender {
    TerminalRender::Snapshot(TerminalSnapshot {
        frame,
        cols: 100,
        rows: ROWS,
        cursor: cursor(),
        modes: Modes::default(),
        palette: Palette::default(),
        title: None,
        lines: (0..ROWS).map(|i| row(&format!("s{i}"))).collect(),
        links: vec![],
        history_len: history,
        viewport_top: u64::from(history),
    })
}

/// The screen after `scrolled` more rows went into the scrollback.
fn scrolled(frame: u64, history: u32, scrolled: u32) -> TerminalRender {
    let history = history + scrolled;
    TerminalRender::Frame(TerminalFrame {
        frame,
        scrolled: 0,
        dirty: (0..ROWS)
            .map(|i| (i, row(&format!("s{}", u32::from(i) + scrolled))))
            .collect(),
        cursor: cursor(),
        modes: None,
        title: None,
        links: vec![],
        history_len: history,
        viewport_top: u64::from(history),
    })
}

fn apply(view: &Entity<TerminalView>, cx: &mut VisualTestContext, render: TerminalRender) {
    view.update(cx, |view, cx| view.apply_render(render, cx));
    draw(cx);
}

fn span(row: u64, col: u16, end_row: u64, end_col: u16) -> FindSpan {
    FindSpan {
        row,
        col,
        end_row,
        end_col,
    }
}

fn highlight(
    view: &Entity<TerminalView>,
    cx: &mut VisualTestContext,
    spans: Vec<FindSpan>,
    current: Option<usize>,
) {
    view.update(cx, |view, cx| {
        view.set_find_highlights(Some(FindHighlights::new(spans, current)), cx);
    });
    draw(cx);
}

fn painted(view: &Entity<TerminalView>, cx: &VisualTestContext) -> Vec<(u16, u16, u16, bool)> {
    view.read_with(cx, |v, _| v.painted_find().to_vec())
}

#[gpui::test]
fn matches_on_screen_are_tinted_and_the_current_one_is_marked(cx: &mut TestAppContext) {
    let (view, cx, _) = setup(cx);
    apply(&view, cx, snapshot(1, 1000));
    assert!(painted(&view, cx).is_empty());
    // Screen row 3 is absolute row 1003; the second match is on row 5, across a wrap into 6.
    highlight(
        &view,
        cx,
        vec![span(1003, 2, 1003, 4), span(1005, 98, 1006, 3)],
        Some(1),
    );
    assert_eq!(
        painted(&view, cx),
        [(3, 2, 4, false), (5, 98, 100, true), (6, 0, 3, true)]
    );
    view.update(cx, |v, cx| v.set_find_highlights(None, cx));
    draw(cx);
    assert!(painted(&view, cx).is_empty());
}

#[gpui::test]
fn matches_stay_with_their_text_while_output_scrolls(cx: &mut TestAppContext) {
    let (view, cx, _) = setup(cx);
    apply(&view, cx, snapshot(1, 1000));
    highlight(&view, cx, vec![span(1010, 0, 1010, 3)], Some(0));
    assert_eq!(painted(&view, cx), [(10, 0, 3, true)]);
    // Four rows scroll away: the same text is four rows higher; ten more and it is gone.
    apply(&view, cx, scrolled(2, 1000, 4));
    assert_eq!(painted(&view, cx), [(6, 0, 3, true)]);
    apply(&view, cx, scrolled(3, 1004, 10));
    assert!(painted(&view, cx).is_empty());
}

#[gpui::test]
fn a_match_in_the_scrollback_is_scrolled_to_and_loaded(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx);
    apply(&view, cx, snapshot(1, 1000));
    recorder.take();
    // Absolute row 400 is 600 rows above the screen: nothing of it is loaded.
    let old = span(400, 1, 400, 5);
    highlight(&view, cx, vec![old], Some(0));
    assert!(painted(&view, cx).is_empty());
    view.update(cx, |v, cx| v.reveal_find_match(old, cx));
    draw(cx);
    let offset = view.read_with(cx, |v, _| v.scroll_offset());
    // The match is in the middle of the view: 600 rows back, and the lower half of the view
    // shows rows newer than it.
    assert_eq!(offset, 600 + u32::from(ROWS) - 1 - u32::from(ROWS / 2));
    // The view asks the daemon for the first rows of the view that are not loaded.
    let asked = recorder.take();
    let from_line = asked
        .iter()
        .find_map(|c| match c {
            Call::History(from, 200) => Some(*from),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no history request: {asked:?}"));
    assert!(from_line >= 500, "{from_line}");
    // The page arrives (newest first) and the match is drawn in the middle of the view.
    view.update(cx, |v, cx| {
        v.apply_history(
            from_line,
            (0..200).map(|i| row(&format!("h{i}"))).collect(),
            cx,
        )
    });
    draw(cx);
    let cells = painted(&view, cx);
    assert_eq!(cells.len(), 1, "{cells:?}");
    let (grid_row, from, to, current) = cells[0];
    assert_eq!((from, to, current), (1, 5, true));
    assert!(
        (i32::from(grid_row) - i32::from(ROWS / 2)).abs() <= 1,
        "row {grid_row}"
    );
}

#[gpui::test]
fn revealing_a_visible_match_does_not_scroll(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx);
    apply(&view, cx, snapshot(1, 1000));
    recorder.take();
    let visible = span(1020, 0, 1020, 2);
    view.update(cx, |v, cx| v.reveal_find_match(visible, cx));
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 0);
    assert!(recorder.take().is_empty());
}

#[gpui::test]
fn cmd_f_is_not_sent_to_the_shell(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx);
    apply(&view, cx, snapshot(1, 0));
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    recorder.take();
    cx.simulate_keystrokes("cmd-f cmd-g cmd-shift-g");
    assert!(recorder.take().is_empty(), "no bytes for ⌘F, ⌘G, ⌘⇧G");
    // A plain key still goes through.
    cx.simulate_input("f");
    assert!(
        recorder
            .take()
            .iter()
            .any(|c| matches!(c, Call::Input(bytes) if bytes == b"f"))
    );
}
