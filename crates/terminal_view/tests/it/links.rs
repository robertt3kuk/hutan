#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns
//! Links in terminal text: ⌘-hover underlines a URL or an existing `path:line:col`, ⌘-click asks
//! the owner to open it (`TerminalViewEvent::OpenLink`). GPUI's test text system gives 7.8 × 15.6
//! px cells.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px, size};
use hutan_client::BlockList;
use hutan_protocol::blocks::{Block, BlockPart, BlockState, TerminalBlocksResult};
use hutan_protocol::render::{
    CellFlags, Color, Cursor, CursorShape, Link, Modes, Palette, Row, Run, TerminalRender,
    TerminalSnapshot,
};
use hutan_terminal_view::{
    DefaultColors, LinkTarget, TerminalBackend, TerminalSize, TerminalView, TerminalViewEvent,
    TerminalViewSettings,
};

const CELL_W: f32 = 7.8;
const CELL_H: f32 = 15.6;
const COLS: usize = 100;

/// A backend that only knows the shell's folder.
#[derive(Default)]
struct Folder(RefCell<Option<PathBuf>>);

impl TerminalBackend for Folder {
    fn input(&self, _: Vec<u8>) {}
    fn resize(&self, _: TerminalSize) {}
    fn ack(&self, _: u64) {}
    fn request_history(&self, _: u32, _: u32) {}
    fn set_default_colors(&self, _: DefaultColors) {}
    fn block_text(&self, _: u64, _: BlockPart) -> gpui::Task<Option<String>> {
        gpui::Task::ready(None)
    }
    fn working_dir(&self) -> Option<PathBuf> {
        self.0.borrow().clone()
    }
}

struct Harness {
    view: Entity<TerminalView>,
    folder: Rc<Folder>,
    opened: Rc<RefCell<Vec<LinkTarget>>>,
}

fn setup(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
    let folder = Rc::new(Folder::default());
    let backend: Rc<dyn TerminalBackend> = folder.clone();
    let (view, cx) = cx.add_window_view(|window, cx| {
        TerminalView::new(backend, TerminalViewSettings::default(), window, cx)
    });
    cx.simulate_resize(size(px(800.0), px(600.0)));
    cx.run_until_parked();
    draw(cx);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = opened.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &TerminalViewEvent, _| {
            if let TerminalViewEvent::OpenLink(target) = event {
                sink.borrow_mut().push(target.clone());
            }
        })
        .detach();
    });
    (
        Harness {
            view,
            folder,
            opened,
        },
        cx,
    )
}

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

fn row(text: &str, wrapped: bool) -> Row {
    Row {
        runs: if text.is_empty() {
            vec![]
        } else {
            vec![run(text)]
        },
        wrapped,
    }
}

fn show(h: &Harness, cx: &mut VisualTestContext, rows: Vec<Row>) {
    let render = TerminalRender::Snapshot(TerminalSnapshot {
        frame: 1,
        cols: COLS as u16,
        rows: rows.len() as u16,
        cursor: Cursor {
            col: 0,
            row: 0,
            shape: CursorShape::Block,
            visible: true,
            blinking: false,
        },
        modes: Modes::default(),
        palette: Palette::default(),
        title: None,
        lines: rows,
        links: vec![],
        history_len: 0,
        viewport_top: 0,
    });
    h.view.update(cx, |v, cx| v.apply_render(render, cx));
    draw(cx);
}

fn plain(lines: &[&str]) -> Vec<Row> {
    lines.iter().map(|l| row(l, false)).collect()
}

fn at(col: f32, row: usize) -> gpui::Point<gpui::Pixels> {
    point(px(col * CELL_W + 1.0), px(row as f32 * CELL_H + 1.0))
}

const CMD: Modifiers = Modifiers {
    platform: true,
    control: false,
    alt: false,
    shift: false,
    function: false,
};

fn hover(h: &Harness, cx: &mut VisualTestContext, col: f32, row: usize) -> Option<LinkTarget> {
    cx.simulate_mouse_move(at(col, row), None, CMD);
    draw(cx);
    h.view.read_with(cx, |v, _| {
        v.hovered_link().map(|(target, _)| target.clone())
    })
}

fn cmd_click(cx: &mut VisualTestContext, col: f32, row: usize) {
    cx.simulate_mouse_move(at(col, row), None, CMD);
    cx.simulate_mouse_down(at(col, row), MouseButton::Left, CMD);
    cx.simulate_mouse_up(at(col, row), MouseButton::Left, CMD);
}

fn file(path: &Path, line: Option<u32>, column: Option<u32>) -> LinkTarget {
    LinkTarget::File {
        path: path.to_path_buf(),
        line,
        column,
    }
}

/// A folder holding `src/lib.rs`.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("src")).expect("mkdir");
    std::fs::write(dir.path().join("src/lib.rs"), "fn main() {}\n").expect("write");
    dir
}

#[gpui::test]
fn cmd_hover_underlines_an_existing_path_and_cmd_click_opens_it(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx);
    let dir = project();
    *h.folder.0.borrow_mut() = Some(dir.path().to_path_buf());
    show(
        &h,
        cx,
        plain(&["error at src/lib.rs:42:7: boom", "plain words"]),
    );

    // Without ⌘ nothing is found.
    cx.simulate_mouse_move(at(12.0, 0), None, Modifiers::default());
    draw(cx);
    assert!(h.view.read_with(cx, |v, _| v.hovered_link().is_none()));

    let want = file(&dir.path().join("src/lib.rs"), Some(42), Some(7));
    assert_eq!(hover(&h, cx, 12.0, 0), Some(want.clone()));
    // The underline covers `src/lib.rs:42:7`: columns 9..24 of the first line.
    let segments = h
        .view
        .read_with(cx, |v, _| v.hovered_link().map(|(_, s)| s.to_vec()));
    assert_eq!(segments, Some(vec![(0, 9, 24)]));

    // Holding ⌘ and leaving the text drops it; so does releasing ⌘ over it.
    assert_eq!(hover(&h, cx, 40.0, 0), None);
    assert_eq!(hover(&h, cx, 12.0, 0), Some(want.clone()));
    cx.simulate_mouse_move(at(12.0, 0), None, Modifiers::default());
    draw(cx);
    assert!(h.view.read_with(cx, |v, _| v.hovered_link().is_none()));

    cmd_click(cx, 12.0, 0);
    assert_eq!(*h.opened.borrow(), [want]);
}

#[gpui::test]
fn a_click_without_cmd_opens_nothing_and_selects_as_before(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx);
    let dir = project();
    *h.folder.0.borrow_mut() = Some(dir.path().to_path_buf());
    show(&h, cx, plain(&["src/lib.rs:1"]));
    cx.simulate_mouse_down(at(3.0, 0), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(at(3.0, 0), MouseButton::Left, Modifiers::default());
    assert!(h.opened.borrow().is_empty());
}

#[gpui::test]
fn text_that_is_not_a_file_is_not_a_link(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx);
    let dir = project();
    *h.folder.0.borrow_mut() = Some(dir.path().to_path_buf());
    show(
        &h,
        cx,
        plain(&["e.g. 1.5 and missing/file.rs:3 and src/ and localhost:3000"]),
    );
    for col in [1.0, 8.0, 20.0, 50.0] {
        assert_eq!(hover(&h, cx, col, 0), None, "col {col}");
    }
    cmd_click(cx, 20.0, 0);
    assert!(h.opened.borrow().is_empty());
}

#[gpui::test]
fn a_relative_path_needs_a_folder_but_an_absolute_one_does_not(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx);
    let dir = project();
    let absolute = dir.path().join("src/lib.rs");
    let line = format!("src/lib.rs:1 and {}:9", absolute.display());
    show(&h, cx, plain(&[&line]));
    // No folder known: the relative one is no link.
    assert_eq!(hover(&h, cx, 3.0, 0), None);
    // The absolute one is.
    let col = (line
        .find(&absolute.display().to_string())
        .expect("absolute")
        + 3) as f32;
    assert_eq!(hover(&h, cx, col, 0), Some(file(&absolute, Some(9), None)));
}

#[gpui::test]
fn a_blocks_folder_wins_over_the_shells_current_folder(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx);
    let old = project();
    let now = tempfile::tempdir().expect("tempdir");
    *h.folder.0.borrow_mut() = Some(now.path().to_path_buf());
    let mut rows = plain(&["$ cargo build", "src/lib.rs:5", "$", ""]);
    rows.truncate(4);
    h.view.update(cx, |v, cx| {
        v.apply_render(
            TerminalRender::Snapshot(TerminalSnapshot {
                frame: 1,
                cols: COLS as u16,
                rows: 4,
                cursor: Cursor {
                    col: 2,
                    row: 2,
                    shape: CursorShape::Block,
                    visible: true,
                    blinking: false,
                },
                modes: Modes::default(),
                palette: Palette::default(),
                title: None,
                lines: rows,
                links: vec![],
                history_len: 0,
                viewport_top: 0,
            }),
            cx,
        );
    });
    draw(cx);
    // Without blocks the shell's folder (which has no such file) decides.
    assert_eq!(hover(&h, cx, 3.0, 1), None);

    let block = |id: u64, prompt: u64, state: BlockState, end: Option<u64>| Block {
        id,
        prompt_line: prompt,
        command_line: Some(prompt),
        output_start: Some(prompt + 1),
        end_line: end,
        command: Some("cargo build".into()),
        exit_code: Some(1),
        started_at_ms: 1,
        finished_at_ms: Some(2),
        cwd: Some(old.path().to_path_buf()),
        state,
    };
    let mut list = BlockList::new();
    list.replace(TerminalBlocksResult {
        blocks: vec![
            Block {
                cwd: None,
                command: None,
                ..block(2, 2, BlockState::Prompt, None)
            },
            block(1, 0, BlockState::Finished, Some(2)),
        ],
        viewport_top: 0,
    });
    h.view.update(cx, |v, cx| {
        v.shared().update(cx, |s, cx| s.set_blocks(list, cx))
    });
    draw(cx);
    // The pointer moves on and back: the link is looked for again.
    assert_eq!(hover(&h, cx, 90.0, 3), None);
    assert_eq!(
        hover(&h, cx, 3.0, 1),
        Some(file(&old.path().join("src/lib.rs"), Some(5), None))
    );
}

#[gpui::test]
fn urls_are_found_without_trailing_punctuation(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx);
    show(
        &h,
        cx,
        plain(&[
            "see (https://example.com/a/b?q=1), then https://example.org/x.",
            "none of https://",
        ]),
    );
    let url = |s: &str| Some(LinkTarget::Url(s.to_owned()));
    assert_eq!(hover(&h, cx, 12.0, 0), url("https://example.com/a/b?q=1"));
    assert_eq!(hover(&h, cx, 50.0, 0), url("https://example.org/x"));
    assert_eq!(hover(&h, cx, 12.0, 1), None);
    cmd_click(cx, 50.0, 0);
    assert_eq!(
        *h.opened.borrow(),
        [LinkTarget::Url("https://example.org/x".into())]
    );
}

/// A row exactly as wide as the terminal that ends in `tail` after `head`, padded with `fill`.
fn full_row(head: &str, fill: char, tail: &str) -> String {
    let pad = COLS - head.len() - tail.len();
    format!("{head}{}{tail}", fill.to_string().repeat(pad))
}

#[gpui::test]
fn a_url_that_wrapped_is_one_link_and_a_hard_break_is_not(cx: &mut TestAppContext) {
    let (h, cx) = setup(cx);
    let first = full_row("go https://example.com/", 'p', "");
    let wrapped = vec![row(&first, true), row("tail.html end", false)];
    show(&h, cx, wrapped);
    let whole = format!("{}tail.html", &first[3..]);
    assert_eq!(hover(&h, cx, 5.0, 0), Some(LinkTarget::Url(whole.clone())));
    // Hovering the continuation finds the same link; both rows are underlined.
    assert_eq!(hover(&h, cx, 3.0, 1), Some(LinkTarget::Url(whole)));
    let segments = h
        .view
        .read_with(cx, |v, _| v.hovered_link().map(|(_, s)| s.to_vec()));
    assert_eq!(segments, Some(vec![(0, 3, COLS as u16), (1, 0, 9)]));

    // The same text with a hard line break: the first row's URL ends at the break.
    show(
        &h,
        cx,
        vec![row(&first, false), row("tail.html end", false)],
    );
    assert_eq!(
        hover(&h, cx, 5.0, 0),
        Some(LinkTarget::Url(first[3..].to_owned()))
    );
    assert_eq!(hover(&h, cx, 3.0, 1), None);
}

#[gpui::test]
fn an_osc8_hyperlink_is_opened_through_the_event_and_only_for_safe_schemes(
    cx: &mut TestAppContext,
) {
    let (h, cx) = setup(cx);
    let mut linked = run("click me");
    linked.link = Some(0);
    let mut risky = run("local");
    risky.link = Some(1);
    let render = TerminalRender::Snapshot(TerminalSnapshot {
        frame: 1,
        cols: 100,
        rows: 2,
        cursor: Cursor {
            col: 0,
            row: 0,
            shape: CursorShape::Block,
            visible: true,
            blinking: false,
        },
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
    h.view.update(cx, |v, cx| v.apply_render(render, cx));
    draw(cx);
    cmd_click(cx, 2.0, 0);
    assert_eq!(
        *h.opened.borrow(),
        [LinkTarget::Url("https://example.com/a".into())]
    );
    cmd_click(cx, 2.0, 1);
    assert_eq!(h.opened.borrow().len(), 1, "file: targets are not opened");
}
