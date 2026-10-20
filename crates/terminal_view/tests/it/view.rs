//! GPUI tests: the view lays out against GPUI's deterministic test text system (every glyph is
//! 0.6 em wide, so a 13 px font gives 7.8 × 15.6 px cells), applies renders, and acks after a
//! frame.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Entity, Focusable as _, Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent, TestAppContext,
    TouchPhase, VisualTestContext, point, px, size,
};
use hutan_protocol::blocks::BlockPart;
use hutan_protocol::render::{
    CellFlags, Color, Cursor, CursorShape, Link, Modes, MouseMode, Palette, Row, Run,
    TerminalFrame, TerminalRender, TerminalSnapshot,
};
use hutan_terminal_view::{
    DefaultColors, TerminalBackend, TerminalSize, TerminalView, TerminalViewEvent,
    TerminalViewSettings, ViewStatus,
};

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Input(Vec<u8>),
    Resize(TerminalSize),
    Ack(u64),
    History(u32, u32),
    Colors(DefaultColors),
    BlockText(u64, BlockPart),
}

#[derive(Default)]
struct Recorder {
    calls: RefCell<Vec<Call>>,
    /// What `block_text` answers.
    texts: RefCell<Vec<(u64, BlockPart, String)>>,
}

impl Recorder {
    fn take(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.borrow_mut())
    }

    fn inputs(&self) -> Vec<Vec<u8>> {
        self.take()
            .into_iter()
            .filter_map(|c| match c {
                Call::Input(bytes) => Some(bytes),
                _ => None,
            })
            .collect()
    }
}

impl TerminalBackend for Recorder {
    fn input(&self, bytes: Vec<u8>) {
        self.calls.borrow_mut().push(Call::Input(bytes));
    }
    fn resize(&self, size: TerminalSize) {
        self.calls.borrow_mut().push(Call::Resize(size));
    }
    fn ack(&self, frame: u64) {
        self.calls.borrow_mut().push(Call::Ack(frame));
    }
    fn request_history(&self, from_line: u32, count: u32) {
        self.calls
            .borrow_mut()
            .push(Call::History(from_line, count));
    }
    fn set_default_colors(&self, colors: DefaultColors) {
        self.calls.borrow_mut().push(Call::Colors(colors));
    }
    fn block_text(&self, block_id: u64, part: BlockPart) -> gpui::Task<Option<String>> {
        self.calls
            .borrow_mut()
            .push(Call::BlockText(block_id, part));
        gpui::Task::ready(
            self.texts
                .borrow()
                .iter()
                .find(|(id, p, _)| *id == block_id && *p == part)
                .map(|(_, _, text)| text.clone()),
        )
    }
}

const CELL_W: f32 = 7.8;
const CELL_H: f32 = 15.6;

fn setup(
    cx: &mut TestAppContext,
    settings: TerminalViewSettings,
) -> (Entity<TerminalView>, &mut VisualTestContext, Rc<Recorder>) {
    let recorder = Rc::new(Recorder::default());
    let backend: Rc<dyn TerminalBackend> = recorder.clone();
    let (view, cx) =
        cx.add_window_view(|window, cx| TerminalView::new(backend, settings, window, cx));
    resize(cx, 800.0, 600.0);
    (view, cx, recorder)
}

fn resize(cx: &mut VisualTestContext, width: f32, height: f32) {
    cx.simulate_resize(size(px(width), px(height)));
    cx.run_until_parked();
    draw(cx);
}

/// One frame: draw, then the next-frame callbacks the platform would run before the following
/// draw.
fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.simulate_next_frame(cx);
    });
    cx.run_until_parked();
}

fn run(text: &str) -> Run {
    Run {
        text: text.to_owned(),
        fg: Color::Default,
        bg: Color::Default,
        flags: CellFlags::empty(),
        underline_color: None,
        link: None,
    }
}

fn row(text: &str) -> Row {
    Row {
        runs: if text.is_empty() {
            vec![]
        } else {
            vec![run(text)]
        },
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

fn snapshot(frame: u64, lines: &[&str], modes: Modes, history_len: u32) -> TerminalRender {
    TerminalRender::Snapshot(TerminalSnapshot {
        frame,
        cols: 100,
        rows: lines.len() as u16,
        cursor: cursor(),
        modes,
        palette: Palette::default(),
        title: None,
        lines: lines.iter().map(|l| row(l)).collect(),
        links: vec![],
        history_len,
        viewport_top: 0,
    })
}

fn frame(frame: u64, dirty: &[(u16, &str)], history_len: u32) -> TerminalRender {
    TerminalRender::Frame(TerminalFrame {
        frame,
        scrolled: 0,
        dirty: dirty.iter().map(|(i, t)| (*i, row(t))).collect(),
        cursor: cursor(),
        modes: None,
        title: None,
        links: vec![],
        history_len,
        viewport_top: 0,
    })
}

fn apply(view: &Entity<TerminalView>, cx: &mut VisualTestContext, render: TerminalRender) {
    view.update(cx, |view, cx| view.apply_render(render, cx));
}

fn at(col: f32, row: f32) -> gpui::Point<gpui::Pixels> {
    point(px(col * CELL_W + 1.0), px(row * CELL_H + 1.0))
}

#[gpui::test]
fn creation_reports_the_default_colors(cx: &mut TestAppContext) {
    let (_view, _cx, recorder) = setup(cx, TerminalViewSettings::default());
    let calls = recorder.take();
    let colors = calls
        .iter()
        .find_map(|c| match c {
            Call::Colors(colors) => Some(*colors),
            _ => None,
        })
        .expect("set_default_colors on creation");
    assert_eq!(
        colors.background,
        hutan_terminal_view::Rgb(0x26, 0x26, 0x24)
    );
    assert_eq!(
        colors.foreground,
        hutan_terminal_view::Rgb(0xea, 0xe8, 0xe1)
    );
    assert_eq!(colors.cursor, hutan_terminal_view::Rgb(0xea, 0x7a, 0x43));
}

#[gpui::test]
fn layout_computes_the_grid_from_bounds_and_font(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    // 800 / 7.8 = 102.5 columns, 600 / 15.6 = 38.4 rows.
    assert_eq!(view.read_with(cx, |v, _| v.grid_size()), Some((102, 38)));
    let cell = view.read_with(cx, |v, _| v.cell_size()).unwrap();
    assert!((f32::from(cell.width) - CELL_W).abs() < 0.01);
    assert!((f32::from(cell.height) - CELL_H).abs() < 0.01);

    // The window opened at the (maximized) test display size, which was reported at once; the
    // resize to 800 × 600 follows after the debounce.
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    let resizes: Vec<_> = recorder
        .take()
        .into_iter()
        .filter_map(|c| match c {
            Call::Resize(size) => Some(size),
            _ => None,
        })
        .collect();
    assert_eq!(resizes.len(), 2, "{resizes:?}");
    let last = resizes[1];
    assert_eq!((last.cols, last.rows), (102, 38));
    // Pixel size is the device size of the grid area: 800 × 600 logical pixels.
    assert!(
        last.pixel_width >= 800 && last.pixel_height >= 600,
        "{last:?}"
    );
}

#[gpui::test]
fn font_size_and_line_height_change_the_grid(cx: &mut TestAppContext) {
    let settings = TerminalViewSettings {
        font_size: 20.0,
        line_height: 1.5,
        ..TerminalViewSettings::default()
    };
    let (view, cx, _recorder) = setup(cx, settings);
    // Cell 12 × 30.
    assert_eq!(view.read_with(cx, |v, _| v.grid_size()), Some((66, 20)));
}

#[gpui::test]
fn resizes_are_debounced(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    recorder.take();
    resize(cx, 400.0, 300.0);
    assert_eq!(view.read_with(cx, |v, _| v.grid_size()), Some((51, 19)));
    assert!(recorder.take().is_empty(), "debounced: nothing sent yet");
    resize(cx, 390.0, 300.0);
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    let resizes: Vec<_> = recorder
        .take()
        .into_iter()
        .filter_map(|c| match c {
            Call::Resize(size) => Some((size.cols, size.rows)),
            _ => None,
        })
        .collect();
    assert_eq!(resizes, [(50, 19)], "only the last size is sent");
}

#[gpui::test]
fn frames_are_acked_when_they_are_painted(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    recorder.take();
    // The test platform paints a dirty view when the update that dirtied it ends. The ack goes
    // out with that paint, not a display frame later: the daemon's next frame is then already
    // here when the next tick comes (docs/perf.md, B4/B5).
    apply(
        &view,
        cx,
        snapshot(7, &["hello", "world"], Modes::default(), 0),
    );
    assert_eq!(recorder.take(), [Call::Ack(7)]);

    // Frames applied before one paint are acked once, by the paint that shows the newest.
    view.update(cx, |view, cx| {
        view.apply_render(frame(8, &[(0, "a")], 0), cx);
        view.apply_render(frame(9, &[(0, "b")], 0), cx);
    });
    assert_eq!(recorder.take(), [Call::Ack(9)]);
    draw(cx);
    assert!(recorder.take().is_empty(), "each frame is acked once");
}

#[gpui::test]
fn snapshot_content_reaches_the_model(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    let mut render = snapshot(1, &["$ ls", "file"], Modes::default(), 3);
    if let TerminalRender::Snapshot(s) = &mut render {
        s.title = Some("zsh".into());
    }
    let titles = Rc::new(RefCell::new(Vec::new()));
    let sink = titles.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &TerminalViewEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });
    apply(&view, cx, render);
    draw(cx);
    view.read_with(cx, |v, _| {
        assert_eq!(v.title().as_deref(), Some("zsh"));
        assert_eq!(v.model().history_len(), 3);
        assert_eq!(v.model().viewport().len(), 2);
    });
    assert_eq!(
        *titles.borrow(),
        [TerminalViewEvent::TitleChanged(Some("zsh".into()))]
    );
}

#[gpui::test]
fn typed_text_goes_through_the_input_method(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot(1, &["$"], Modes::default(), 0));
    draw(cx);
    recorder.take();
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    cx.simulate_input("héllo я");
    assert_eq!(
        recorder.inputs().concat(),
        "héllo я".as_bytes(),
        "text, including non-Latin text, is sent as UTF-8"
    );
}

#[gpui::test]
fn composition_is_marked_then_committed(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler as _;
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot(1, &["$"], Modes::default(), 0));
    draw(cx);
    recorder.take();
    view.update_in(cx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "にほ", Some(1..2), window, cx);
    });
    assert_eq!(
        view.read_with(cx, |v, _| v.marked_text().map(str::to_owned))
            .as_deref(),
        Some("にほ")
    );
    assert!(recorder.take().is_empty(), "composition is not sent");
    let range = view.update_in(cx, |v, window, cx| v.marked_text_range(window, cx));
    assert_eq!(range, Some(0..2));
    let selection = view.update_in(cx, |v, window, cx| v.selected_text_range(false, window, cx));
    assert_eq!(selection.map(|s| s.range), Some(1..2));
    draw(cx);

    view.update_in(cx, |v, window, cx| {
        v.replace_text_in_range(None, "日本", window, cx);
    });
    assert_eq!(recorder.inputs(), ["日本".as_bytes().to_vec()]);
    assert_eq!(
        view.read_with(cx, |v, _| v.marked_text().map(str::to_owned)),
        None
    );

    // Cancelling a composition sends nothing.
    view.update_in(cx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "x", None, window, cx);
        v.unmark_text(window, cx);
    });
    assert!(recorder.take().is_empty());
}

#[gpui::test]
fn ime_bounds_follow_the_cursor_and_marked_text(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler as _;
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    let mut render = snapshot(1, &["abc", "", ""], Modes::default(), 0);
    if let TerminalRender::Snapshot(s) = &mut render {
        s.cursor.col = 3;
        s.cursor.row = 1;
    }
    apply(&view, cx, render);
    draw(cx);
    view.update_in(cx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "日本", None, window, cx);
    });
    let element = gpui::Bounds::new(point(px(10.), px(20.)), size(px(800.), px(600.)));
    let first = view
        .update_in(cx, |v, window, cx| {
            v.bounds_for_range(0..1, element, window, cx)
        })
        .unwrap();
    let second = view
        .update_in(cx, |v, window, cx| {
            v.bounds_for_range(1..2, element, window, cx)
        })
        .unwrap();
    assert!((f32::from(first.origin.x) - (10.0 + 3.0 * CELL_W)).abs() < 0.01);
    assert!((f32::from(first.origin.y) - (20.0 + CELL_H)).abs() < 0.01);
    // Each wide character covers two columns.
    assert!((f32::from(second.origin.x - first.origin.x) - 2.0 * CELL_W).abs() < 0.01);
}

#[gpui::test]
fn keys_are_encoded_for_the_application(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    let modes = Modes {
        app_cursor: true,
        ..Modes::default()
    };
    apply(&view, cx, snapshot(1, &["$"], modes, 0));
    draw(cx);
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    recorder.take();
    for (keys, bytes) in [
        ("ctrl-c", &b"\x03"[..]),
        ("up", b"\x1bOA"),
        ("ctrl-up", b"\x1b[1;5A"),
        ("enter", b"\r"),
        ("tab", b"\t"),
        ("escape", b"\x1b"),
        ("backspace", b"\x7f"),
        ("f5", b"\x1b[15~"),
        ("alt-b", b"\x1bb"),
        ("shift-tab", b"\x1b[Z"),
    ] {
        cx.simulate_keystrokes(keys);
        assert_eq!(recorder.inputs().concat(), bytes, "{keys}");
    }
}

#[gpui::test]
fn kitty_flags_change_the_encoding(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    let modes = Modes {
        kitty_keyboard: 1,
        ..Modes::default()
    };
    apply(&view, cx, snapshot(1, &["$"], modes, 0));
    draw(cx);
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    recorder.take();
    cx.simulate_keystrokes("escape");
    assert_eq!(recorder.inputs().concat(), b"\x1b[27u");
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(recorder.inputs().concat(), b"\x1b[99;5u");
}

#[gpui::test]
fn ended_sessions_ignore_input(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot(1, &["$"], Modes::default(), 0));
    draw(cx);
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    view.update(cx, |v, cx| {
        v.set_status(ViewStatus::Ended("exited with code 0".into()), cx)
    });
    recorder.take();
    cx.simulate_keystrokes("ctrl-c");
    cx.simulate_input("x");
    assert!(recorder.take().is_empty());
    view.update(cx, |v, cx| v.set_status(ViewStatus::Live, cx));
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(recorder.inputs().concat(), b"\x03");
}

fn drag_select(cx: &mut VisualTestContext, from: (f32, f32), to: (f32, f32)) {
    cx.simulate_mouse_down(at(from.0, from.1), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        at(to.0, to.1),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    cx.simulate_mouse_up(at(to.0, to.1), MouseButton::Left, Modifiers::default());
}

#[gpui::test]
fn dragging_selects_and_cmd_c_copies_without_touching_the_pty(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    apply(
        &view,
        cx,
        snapshot(1, &["hello world", "second"], Modes::default(), 0),
    );
    draw(cx);
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    recorder.take();

    drag_select(cx, (0.0, 0.0), (4.0, 0.0));
    assert_eq!(
        view.read_with(cx, |v, _| v.selection_text()).as_deref(),
        Some("hello")
    );
    draw(cx);

    cx.simulate_keystrokes("cmd-c");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("hello")
    );
    assert!(recorder.inputs().is_empty(), "⌘C never reaches the PTY");

    // Dragging across rows joins them with a newline.
    drag_select(cx, (6.0, 0.0), (2.0, 1.0));
    assert_eq!(
        view.read_with(cx, |v, _| v.selection_text()).as_deref(),
        Some("world\nsec")
    );
}

#[gpui::test]
fn double_and_triple_click_select_words_and_lines(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    apply(
        &view,
        cx,
        snapshot(1, &["cd ~/src now", "next"], Modes::default(), 0),
    );
    draw(cx);
    cx.simulate_event(gpui::MouseDownEvent {
        button: MouseButton::Left,
        position: at(4.0, 0.0),
        modifiers: Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    assert_eq!(
        view.read_with(cx, |v, _| v.selection_text()).as_deref(),
        Some("~/src")
    );
    cx.simulate_event(gpui::MouseUpEvent {
        button: MouseButton::Left,
        position: at(4.0, 0.0),
        modifiers: Modifiers::default(),
        click_count: 2,
    });
    cx.simulate_event(gpui::MouseDownEvent {
        button: MouseButton::Left,
        position: at(4.0, 0.0),
        modifiers: Modifiers::default(),
        click_count: 3,
        first_mouse: false,
    });
    assert_eq!(
        view.read_with(cx, |v, _| v.selection_text()).as_deref(),
        Some("cd ~/src now")
    );
}

#[gpui::test]
fn copy_on_select_writes_the_clipboard(cx: &mut TestAppContext) {
    let settings = TerminalViewSettings {
        copy_on_select: true,
        ..TerminalViewSettings::default()
    };
    let (view, cx, _recorder) = setup(cx, settings);
    apply(
        &view,
        cx,
        snapshot(1, &["hello world"], Modes::default(), 0),
    );
    draw(cx);
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("old".into()));
    drag_select(cx, (6.0, 0.0), (10.0, 0.0));
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("world")
    );
}

#[gpui::test]
fn a_click_clears_the_selection(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    apply(
        &view,
        cx,
        snapshot(1, &["hello world"], Modes::default(), 0),
    );
    draw(cx);
    drag_select(cx, (0.0, 0.0), (4.0, 0.0));
    assert!(view.read_with(cx, |v, _| v.selection_text()).is_some());
    cx.simulate_click(at(8.0, 0.0), Modifiers::default());
    assert_eq!(view.read_with(cx, |v, _| v.selection_text()), None);
}

#[gpui::test]
fn paste_is_bracketed_and_sanitized(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    let modes = Modes {
        bracketed_paste: true,
        ..Modes::default()
    };
    apply(&view, cx, snapshot(1, &["$"], modes, 0));
    draw(cx);
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    recorder.take();
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("ls\x1b[201~rm".into()));
    cx.simulate_keystrokes("cmd-v");
    assert_eq!(recorder.inputs().concat(), b"\x1b[200~lsrm\x1b[201~");

    // Without the mode, line breaks become carriage returns and nothing is wrapped.
    apply(&view, cx, snapshot(2, &["$"], Modes::default(), 0));
    draw(cx);
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("a\nb".into()));
    cx.simulate_keystrokes("cmd-v");
    assert_eq!(recorder.inputs().concat(), b"a\rb");
}

#[gpui::test]
fn mouse_reporting_sends_sgr_press_drag_release_and_wheel(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    let modes = Modes {
        mouse: MouseMode::Drag,
        mouse_sgr: true,
        ..Modes::default()
    };
    apply(&view, cx, snapshot(1, &["x"; 30], modes, 0));
    draw(cx);
    recorder.take();

    cx.simulate_mouse_down(at(10.0, 5.0), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(at(12.0, 5.0), Some(MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(at(12.0, 5.0), MouseButton::Left, Modifiers::default());
    assert_eq!(
        recorder.inputs(),
        [
            b"\x1b[<0;11;6M".to_vec(),
            b"\x1b[<32;13;6M".to_vec(),
            b"\x1b[<0;13;6m".to_vec(),
        ]
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.selection_text()),
        None,
        "app owns the mouse"
    );

    cx.simulate_event(ScrollWheelEvent {
        position: at(1.0, 1.0),
        delta: ScrollDelta::Lines(point(0.0, 1.0)),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
    });
    assert_eq!(recorder.inputs(), [b"\x1b[<64;2;2M".to_vec()]);

    // Shift bypasses the application and selects.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    cx.simulate_mouse_down(at(0.0, 0.0), MouseButton::Left, shift);
    cx.simulate_mouse_move(at(0.0, 3.0), Some(MouseButton::Left), shift);
    cx.simulate_mouse_up(at(0.0, 3.0), MouseButton::Left, shift);
    assert!(recorder.inputs().is_empty());
    assert!(view.read_with(cx, |v, _| v.selection_text()).is_some());
}

#[gpui::test]
fn legacy_mouse_encoding_without_sgr(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    let modes = Modes {
        mouse: MouseMode::Click,
        ..Modes::default()
    };
    apply(&view, cx, snapshot(1, &["x"; 30], modes, 0));
    draw(cx);
    recorder.take();
    cx.simulate_mouse_down(at(0.0, 0.0), MouseButton::Right, Modifiers::default());
    assert_eq!(recorder.inputs(), [vec![0x1b, b'[', b'M', 34, 33, 33]]);
}

fn wheel(cx: &mut VisualTestContext, lines: f32) {
    cx.simulate_event(ScrollWheelEvent {
        position: at(5.0, 5.0),
        delta: ScrollDelta::Lines(point(0.0, lines)),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
    });
}

#[gpui::test]
fn wheel_scrolls_back_through_history_in_pages_of_200(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    apply(
        &view,
        cx,
        snapshot(1, &["live"; 30], Modes::default(), 1000),
    );
    draw(cx);
    recorder.take();

    wheel(cx, 3.0);
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 3);
    assert_eq!(recorder.take(), [Call::History(0, 200)]);

    // The answer arrives newest first; the visible lines are cached.
    view.update(cx, |v, cx| {
        v.apply_history(0, (0..200).map(|i| row(&format!("h{i}"))).collect(), cx)
    });
    draw(cx);
    wheel(cx, 3.0);
    assert!(recorder.take().is_empty(), "already cached");
    // Scrolling past the cached page asks for the next one (from the first missing line the
    // viewport shows).
    for _ in 0..4 {
        wheel(cx, 60.0);
    }
    let calls = recorder.take();
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, Call::History(from, 200) if *from >= 200)),
        "{calls:?}"
    );

    // Wheel down returns to the live screen.
    for _ in 0..5 {
        wheel(cx, -64.0);
    }
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 0);
}

#[gpui::test]
fn typing_returns_to_the_bottom_when_configured(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot(1, &["live"; 30], Modes::default(), 100));
    draw(cx);
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    wheel(cx, 5.0);
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 5);
    recorder.take();
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 0);
}

#[gpui::test]
fn scrolled_view_stays_on_the_same_lines_as_output_arrives(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot(1, &["live"; 30], Modes::default(), 100));
    draw(cx);
    wheel(cx, 5.0);
    apply(&view, cx, frame(2, &[(29, "new")], 104));
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 9);
}

/// The line at the top of the view, in the model's numbering.
fn top_line(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> i64 {
    view.read_with(cx, |v, _| {
        v.model().viewport_line(0) - i64::from(v.scroll_offset())
    })
}

fn snapshot_at(
    frame: u64,
    cols: u16,
    lines: &[&str],
    history_len: u32,
    viewport_top: u64,
) -> TerminalRender {
    let TerminalRender::Snapshot(mut s) = snapshot(frame, lines, Modes::default(), history_len)
    else {
        unreachable!()
    };
    s.cols = cols;
    s.viewport_top = viewport_top;
    TerminalRender::Snapshot(s)
}

#[gpui::test]
fn the_pill_counts_output_that_arrives_while_scrolled_back(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot(1, &["live"; 30], Modes::default(), 100));
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 0);
    wheel(cx, 5.0);
    let before = top_line(&view, cx);
    // Scrolled back but nothing arrived: no pill.
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 0);

    // 20 lines scroll into history: the rows on screen stay, the pill reads 20.
    apply(&view, cx, frame(2, &[(29, "new")], 120));
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 25);
    assert_eq!(top_line(&view, cx), before);
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 20);

    // More output adds up.
    apply(&view, cx, frame(3, &[(29, "newer")], 123));
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 23);

    // Scrolling towards the live screen counts the lines passed as seen, and scrolling back
    // does not bring them back.
    wheel(cx, -10.0);
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 18);
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 18);
    wheel(cx, 30.0);
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 18);

    // ⇧End returns to the live screen and clears the pill.
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    cx.simulate_keystrokes("shift-end");
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 0);
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 0);

    // Output at the live screen never shows the pill, and a new reading place starts at zero.
    apply(&view, cx, frame(4, &[(29, "x")], 124));
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 0);
    wheel(cx, 3.0);
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 0);
}

#[gpui::test]
fn clicking_the_pill_returns_to_the_live_screen(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot(1, &["live"; 30], Modes::default(), 100));
    draw(cx);
    wheel(cx, 5.0);
    apply(&view, cx, frame(2, &[(29, "new")], 112));
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 12);
    recorder.take();

    // The pill sits centered above the bottom edge of the 800 × 600 window.
    cx.simulate_click(
        point(px(400.0), px(600.0 - 8.0 - 12.0)),
        Modifiers::default(),
    );
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 0);
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 0);
    assert!(
        recorder.inputs().is_empty(),
        "the click is the view's, not the application's"
    );
}

#[gpui::test]
fn typing_clears_the_pill(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot(1, &["live"; 30], Modes::default(), 100));
    draw(cx);
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
    wheel(cx, 5.0);
    apply(&view, cx, frame(2, &[(29, "new")], 110));
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 10);
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 0);
}

#[gpui::test]
fn a_width_change_keeps_the_reading_place(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot_at(1, 100, &["live"; 30], 1000, 1000));
    draw(cx);
    wheel(cx, 40.0);
    // The daemon's line at the top of the view.
    let anchor = 1000 - 40;
    apply(&view, cx, snapshot_at(3, 60, &["live"; 30], 1000, 1000));
    // No line wrapped: the numbers did not move, the same line is still at the top.
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 40);
    assert_eq!(
        view.read_with(cx, |v, _| v.model().viewport_top()
            - u64::from(v.scroll_offset())),
        anchor
    );

    // Narrower still, lines wrapped: the daemon has more lines now (the viewport starts at
    // 1250) and the anchor keeps its number, so it is further back.
    apply(&view, cx, snapshot_at(4, 40, &["live"; 30], 1250, 1250));
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 290);

    // A reading place the daemon no longer retains goes to the oldest line it has.
    apply(&view, cx, snapshot_at(5, 30, &["live"; 30], 100, 2000));
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 100);

    // Widening so far that the place is on the live screen again returns to it.
    apply(&view, cx, snapshot_at(6, 200, &["live"; 30], 100, 1999));
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 99);
    apply(&view, cx, snapshot_at(7, 300, &["live"; 30], 50, 1900));
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 0);
}

#[gpui::test]
fn a_width_change_at_the_live_screen_stays_there(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot_at(1, 100, &["live"; 30], 1000, 1000));
    draw(cx);
    apply(&view, cx, snapshot_at(2, 60, &["live"; 30], 1100, 1100));
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 0);
    assert_eq!(view.read_with(cx, |v, _| v.new_lines_below()), 0);
}

#[gpui::test]
fn wheel_on_the_alternate_screen_sends_arrow_keys(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    let modes = Modes {
        alt_screen: true,
        alternate_scroll: true,
        ..Modes::default()
    };
    apply(&view, cx, snapshot(1, &["vim"; 30], modes, 0));
    draw(cx);
    recorder.take();
    wheel(cx, 2.0);
    assert_eq!(recorder.inputs().concat(), b"\x1b[A\x1b[A");
    wheel(cx, -1.0);
    assert_eq!(recorder.inputs().concat(), b"\x1b[B");
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 0);
}

#[gpui::test]
fn links_open_with_cmd_click_only_for_safe_schemes(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    let mut linked = run("click me");
    linked.link = Some(0);
    let mut risky = run("local");
    risky.link = Some(1);
    let render = TerminalRender::Snapshot(TerminalSnapshot {
        frame: 1,
        cols: 100,
        rows: 2,
        cursor: cursor(),
        modes: Modes::default(),
        palette: Palette::default(),
        title: None,
        lines: vec![
            Row {
                runs: vec![linked],
                wrapped: false,
            },
            Row {
                runs: vec![risky],
                wrapped: false,
            },
        ],
        links: vec![
            Link {
                id: 0,
                uri: "https://example.com/a".into(),
            },
            Link {
                id: 1,
                uri: "file:///Applications/Calculator.app".into(),
            },
        ],
        history_len: 0,
        viewport_top: 0,
    });
    apply(&view, cx, render);
    draw(cx);
    recorder.take();
    let cmd = Modifiers {
        platform: true,
        ..Modifiers::default()
    };
    cx.simulate_mouse_move(at(2.0, 0.0), None, cmd);
    draw(cx);
    cx.simulate_mouse_down(at(2.0, 0.0), MouseButton::Left, cmd);
    cx.simulate_mouse_up(at(2.0, 0.0), MouseButton::Left, cmd);
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/a"));
    cx.simulate_mouse_down(at(2.0, 1.0), MouseButton::Left, cmd);
    cx.simulate_mouse_up(at(2.0, 1.0), MouseButton::Left, cmd);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://example.com/a"),
        "file: targets are not opened"
    );
}

#[gpui::test]
fn settings_changes_report_new_default_colors(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    recorder.take();
    view.update(cx, |v, cx| {
        v.update_settings(
            TerminalViewSettings {
                palette: hutan_terminal_view::TerminalPalette::from(
                    &hutan_settings::themes::builtin_theme(
                        hutan_settings::themes::Builtin::HutanLight,
                    )
                    .terminal(),
                ),
                ..TerminalViewSettings::default()
            },
            cx,
        )
    });
    let calls = recorder.take();
    assert!(
        calls.iter().any(|c| matches!(c, Call::Colors(c) if c.background == hutan_terminal_view::Rgb(0xfa, 0xf9, 0xf5))),
        "{calls:?}"
    );
}

#[gpui::test]
fn every_run_style_paints_without_panicking(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    let styled = |text: &str, flags: CellFlags| Run {
        flags,
        fg: Color::Indexed(2),
        bg: Color::Rgb(10, 20, 30),
        underline_color: Some(Color::Rgb(255, 0, 0)),
        ..run(text)
    };
    let all = [
        CellFlags::BOLD,
        CellFlags::DIM,
        CellFlags::ITALIC,
        CellFlags::UNDERLINE,
        CellFlags::DOUBLE_UNDERLINE,
        CellFlags::UNDERCURL,
        CellFlags::DOTTED_UNDERLINE,
        CellFlags::DASHED_UNDERLINE,
        CellFlags::INVERSE,
        CellFlags::HIDDEN,
        CellFlags::STRIKETHROUGH,
        CellFlags::BOLD | CellFlags::ITALIC | CellFlags::UNDERCURL | CellFlags::STRIKETHROUGH,
    ];
    let mut runs: Vec<Run> = all.iter().map(|f| styled("ab", *f)).collect();
    runs.push(Run {
        flags: CellFlags::WIDE,
        ..run("日本")
    });
    runs.push(run("e\u{301}\u{7}"));
    for shape in [
        CursorShape::Block,
        CursorShape::Bar,
        CursorShape::Underline,
        CursorShape::HollowBlock,
    ] {
        let mut s = TerminalSnapshot {
            frame: 1,
            cols: 100,
            rows: 2,
            cursor: Cursor {
                col: 2,
                row: 0,
                shape,
                visible: true,
                blinking: false,
            },
            modes: Modes::default(),
            palette: Palette {
                indexed: vec![(2, (1, 2, 3))],
                foreground: Some((9, 9, 9)),
                background: Some((250, 250, 250)),
                cursor: Some((1, 1, 1)),
            },
            title: None,
            lines: vec![
                Row {
                    runs: runs.clone(),
                    wrapped: false,
                },
                row("second"),
            ],
            links: vec![],
            history_len: 0,
            viewport_top: 0,
        };
        s.frame = 1;
        apply(&view, cx, TerminalRender::Snapshot(s));
        draw(cx);
    }
}

#[gpui::test]
fn unfocused_cursor_is_hollow_and_blink_toggles(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot(1, &["$ "], Modes::default(), 0));
    draw(cx);
    // Blinking only runs while focused; unfocused the cursor stays visible.
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.cursor_blink_visible()));
    cx.update(|window, cx| {
        // Focus events only reach the view in an active window, like the blink itself.
        window.activate_window();
        window.focus(&view.focus_handle(cx), cx);
    });
    draw(cx);
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    let first = view.read_with(cx, |v, _| v.cursor_blink_visible());
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    let second = view.read_with(cx, |v, _| v.cursor_blink_visible());
    assert_ne!(first, second, "a focused blinking cursor toggles");
}

/// An idle terminal must not wake the process: the blink timer exists only while the cursor is
/// being used, ends in a solid visible cursor, and typing starts it again.
#[gpui::test]
fn the_cursor_stops_blinking_when_idle_and_typing_resumes_it(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    apply(&view, cx, snapshot(1, &["$ "], Modes::default(), 0));
    draw(cx);
    // Unfocused: nothing is scheduled at all.
    assert!(!view.read_with(cx, |v, _| v.blink_timer_active()));
    cx.update(|window, cx| {
        // Focus events only reach the view in an active window, like the blink itself.
        window.activate_window();
        window.focus(&view.focus_handle(cx), cx);
    });
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.blink_timer_active()));

    // Longer than the blink timeout with no activity.
    for _ in 0..14 {
        cx.executor().advance_clock(Duration::from_millis(540));
        cx.run_until_parked();
    }
    assert!(!view.read_with(cx, |v, _| v.blink_timer_active()));
    assert!(
        view.read_with(cx, |v, _| v.cursor_blink_visible()),
        "an idle cursor is solid"
    );

    cx.simulate_input("x");
    assert!(view.read_with(cx, |v, _| v.blink_timer_active()));
    cx.update(|window, cx| window.blur(cx));
    cx.run_until_parked();
    assert!(
        !view.read_with(cx, |v, _| v.blink_timer_active()),
        "losing focus ends the timer"
    );
}

/// A window root that shows an interactive view and a preview of one shared terminal.
struct Pair {
    main: Entity<TerminalView>,
    preview: Entity<TerminalView>,
}

impl gpui::Render for Pair {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{ParentElement as _, Styled as _, div};
        div()
            .flex()
            .size_full()
            .child(div().w(px(400.0)).h_full().child(self.main.clone()))
            .child(div().w(px(200.0)).h(px(100.0)).child(self.preview.clone()))
    }
}

#[gpui::test]
fn two_views_share_one_model_and_one_ack(cx: &mut TestAppContext) {
    use gpui::AppContext as _;
    use hutan_terminal_view::{SharedTerminal, ViewMode};
    let recorder = Rc::new(Recorder::default());
    let backend: Rc<dyn TerminalBackend> = recorder.clone();
    let (pair, cx) = cx.add_window_view(|window, cx| {
        let shared = cx.new(|_| SharedTerminal::new());
        let main = cx.new(|cx| {
            TerminalView::with_shared(
                shared.clone(),
                backend.clone(),
                TerminalViewSettings::default(),
                ViewMode::Interactive,
                window,
                cx,
            )
        });
        let preview = cx.new(|cx| {
            TerminalView::with_shared(
                shared,
                backend.clone(),
                TerminalViewSettings {
                    font_size: 6.0,
                    ..TerminalViewSettings::default()
                },
                ViewMode::Preview,
                window,
                cx,
            )
        });
        Pair { main, preview }
    });
    draw(cx);
    let (main, preview) = pair.read_with(cx, |p, _| (p.main.clone(), p.preview.clone()));
    let resizes = |calls: Vec<Call>| {
        calls
            .into_iter()
            .filter(|c| matches!(c, Call::Resize(_)))
            .count()
    };
    assert_eq!(
        resizes(recorder.take()),
        1,
        "only the interactive view sizes the terminal"
    );

    apply(
        &main,
        cx,
        snapshot(3, &["one", "two", "three"], Modes::default(), 0),
    );
    draw(cx);
    // Both views draw the one model.
    for view in [&main, &preview] {
        view.read_with(cx, |v, _| {
            assert_eq!(v.model().viewport().len(), 3);
            assert_eq!(v.model().frame(), 3);
        });
    }
    let acks: Vec<_> = recorder
        .take()
        .into_iter()
        .filter(|c| matches!(c, Call::Ack(_)))
        .collect();
    assert_eq!(
        acks,
        [Call::Ack(3)],
        "one ack, although two views painted the frame"
    );

    // The preview (6 px font, 3.6 × 7.2 px cells, 100 px high: 13 rows) shows all three rows
    // and never takes input.
    assert_eq!(
        preview.read_with(cx, |v, _| v.grid_size()).map(|g| g.1),
        Some(13)
    );
    cx.update(|window, cx| window.focus(&preview.focus_handle(cx), cx));
    cx.simulate_input("x");
    assert!(recorder.inputs().is_empty(), "previews ignore typing");
}

#[gpui::test]
fn padding_shrinks_the_grid_on_all_four_sides(cx: &mut TestAppContext) {
    let settings = TerminalViewSettings {
        padding: 10.0,
        ..TerminalViewSettings::default()
    };
    let (view, cx, recorder) = setup(cx, settings);
    // (800 - 20) / 7.8 = 100 columns, (600 - 20) / 15.6 = 37.2 rows.
    assert_eq!(view.read_with(cx, |v, _| v.grid_size()), Some((100, 37)));
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    let last = recorder
        .take()
        .into_iter()
        .filter_map(|c| match c {
            Call::Resize(size) => Some(size),
            _ => None,
        })
        .next_back()
        .unwrap();
    assert_eq!((last.cols, last.rows), (100, 37));
    // Device pixels of the content box (2x scale in the test display): 780 × 2.
    assert_eq!(
        (last.pixel_width, last.pixel_height),
        (1560, 1160),
        "{last:?}"
    );

    // Clicks in the padding clamp to the nearest cell instead of missing the grid.
    apply(&view, cx, snapshot(1, &["abc"], Modes::default(), 0));
    draw(cx);
    cx.simulate_mouse_down(
        point(px(2.0), px(2.0)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        point(px(2.0), px(2.0)),
        MouseButton::Left,
        Modifiers::default(),
    );
    assert_eq!(view.read_with(cx, |v, _| v.grid_size()), Some((100, 37)));
}
