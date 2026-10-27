//! The block overlay on GPUI's deterministic test text system (7.8 × 15.6 px cells): gutter bars,
//! chips, folds, prompt jumps, copy, rerun, hover actions and the animation, against a recording
//! backend. The shells and the daemon come in with the desktop's tests.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::{Entity, Focusable as _, Modifiers, TestAppContext, VisualTestContext, point, px, size};
use hutan_client::BlockList;
use hutan_protocol::blocks::{Block, BlockPart, BlockState, TerminalBlocksResult};
use hutan_protocol::render::{
    CellFlags, Color, Cursor, CursorShape, Modes, Palette, Row, Run, TerminalRender,
    TerminalSnapshot,
};
use hutan_terminal_view::blocks::Tone;
use hutan_terminal_view::{
    BlockSettings, DefaultColors, SharedEvent, TerminalBackend, TerminalSize, TerminalView,
    TerminalViewSettings, ViewMode,
};

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Input(Vec<u8>),
    History(u32),
    BlockText(u64, BlockPart),
}

#[derive(Default)]
struct Recorder {
    calls: RefCell<Vec<Call>>,
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
    fn resize(&self, _: TerminalSize) {}
    fn ack(&self, _: u64) {}
    fn request_history(&self, from_line: u32, _count: u32) {
        self.calls.borrow_mut().push(Call::History(from_line));
    }
    fn set_default_colors(&self, _: DefaultColors) {}
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

const CELL_H: f32 = 15.6;
const CELL_W: f32 = 7.8;
/// Rows of the test screen.
const ROWS: u16 = 10;
/// The absolute line of the first viewport row, equal to the scrollback length so that the
/// model's own line numbers and the daemon's absolute ones coincide.
const TOP: u64 = 20;

fn setup(
    cx: &mut TestAppContext,
    settings: TerminalViewSettings,
) -> (Entity<TerminalView>, &mut VisualTestContext, Rc<Recorder>) {
    let recorder = Rc::new(Recorder::default());
    let backend: Rc<dyn TerminalBackend> = recorder.clone();
    let (view, cx) =
        cx.add_window_view(|window, cx| TerminalView::new(backend, settings, window, cx));
    // The window is exactly as tall as the screen, as the daemon's terminal would be.
    cx.simulate_resize(size(px(800.0), px(f32::from(ROWS) * CELL_H + 2.0)));
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
        runs: if text.is_empty() {
            vec![]
        } else {
            vec![Run {
                text: text.to_owned(),
                fg: Color::Default,
                bg: Color::Default,
                flags: CellFlags::empty(),
                underline_color: None,
                link: None,
            }]
        },
        wrapped: false,
    }
}

fn snapshot(lines: &[&str], modes: Modes, cursor_row: u16, history_len: u32) -> TerminalRender {
    TerminalRender::Snapshot(TerminalSnapshot {
        frame: 1,
        cols: 100,
        rows: lines.len() as u16,
        cursor: Cursor {
            col: 2,
            row: cursor_row,
            shape: CursorShape::Block,
            visible: true,
            blinking: false,
        },
        modes,
        palette: Palette::default(),
        title: None,
        lines: lines.iter().map(|l| row(l)).collect(),
        links: vec![],
        history_len,
        viewport_top: u64::from(history_len),
    })
}

/// The scenario: 20 lines of scrollback, then the screen
///
/// ```text
/// 20 $ cmd-a            block 1: exit 0 in 1.2 s, output 21..24
/// 21 a1
/// 22 a2
/// 23 a3
/// 24 $ cmd-b            block 2: exit 2 in 3 m, output 25..27
/// 25 b1
/// 26 b2
/// 27 $                  block 3: the open prompt, cursor here
/// 28
/// 29
/// ```
const SCREEN: [&str; 10] = [
    "$ cmd-a", "a1", "a2", "a3", "$ cmd-b", "b1", "b2", "$", "", "",
];

fn block(id: u64, prompt: u64, state: BlockState) -> Block {
    Block {
        id,
        prompt_line: prompt,
        command_line: None,
        output_start: None,
        end_line: None,
        command: None,
        exit_code: None,
        started_at_ms: 10_000,
        finished_at_ms: None,
        cwd: Some(PathBuf::from("/repo")),
        state,
    }
}

fn scenario() -> Vec<Block> {
    vec![
        Block {
            command_line: Some(20),
            output_start: Some(21),
            end_line: Some(24),
            command: Some("cmd-a".into()),
            exit_code: Some(0),
            finished_at_ms: Some(11_234),
            ..block(1, 20, BlockState::Finished)
        },
        Block {
            command_line: Some(24),
            output_start: Some(25),
            end_line: Some(27),
            command: Some("cmd-b".into()),
            exit_code: Some(2),
            finished_at_ms: Some(10_000 + 180_000),
            ..block(2, 24, BlockState::Finished)
        },
        block(3, 27, BlockState::Prompt),
    ]
}

fn set_blocks(view: &Entity<TerminalView>, cx: &mut VisualTestContext, blocks: Vec<Block>) {
    let mut list = BlockList::new();
    list.replace(TerminalBlocksResult {
        blocks: blocks.into_iter().rev().collect(),
        viewport_top: TOP,
    });
    view.update(cx, |v, cx| {
        v.shared().update(cx, |s, cx| s.set_blocks(list, cx));
    });
    draw(cx);
}

fn show(view: &Entity<TerminalView>, cx: &mut VisualTestContext, blocks: Vec<Block>, modes: Modes) {
    view.update(cx, |v, cx| {
        v.apply_render(snapshot(&SCREEN, modes, 7, 20), cx)
    });
    draw(cx);
    set_blocks(view, cx, blocks);
}

fn focus(view: &Entity<TerminalView>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
}

fn text(view: &Entity<TerminalView>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, _| v.visible_text())
}

/// Scrollback rows (newest first, from the line right above the screen) for lines
/// `19, 18, …` named `h19, h18, …`.
fn give_history(view: &Entity<TerminalView>, cx: &mut VisualTestContext, from_line: u32, n: u32) {
    let rows: Vec<Row> = (0..n)
        .map(|i| row(&format!("h{}", 19 - from_line - i)))
        .collect();
    view.update(cx, |v, cx| v.apply_history(from_line, rows, cx));
    draw(cx);
}

fn raw(view: &Entity<TerminalView>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, _| {
        v.model()
            .viewport()
            .iter()
            .map(hutan_terminal_view::model::TerminalModel::row_text)
            .collect()
    })
}

fn at(col: f32, row: f32) -> gpui::Point<gpui::Pixels> {
    point(px(col * CELL_W + 1.0), px(row * CELL_H + 1.0))
}

#[gpui::test]
fn the_overlay_draws_bars_chips_and_separators_without_moving_the_text(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, vec![], Modes::default());
    let before = text(&view, cx);
    assert_eq!(before, SCREEN);

    set_blocks(&view, cx, scenario());
    assert_eq!(text(&view, cx), before, "the grid is never re-laid out");
    let report = view.read_with(cx, |v, _| v.painted_overlay().clone());
    // Block 1 on rows 0..4 (green), block 2 on rows 4..7 (red); the open prompt has no bar.
    assert_eq!(
        report.bars,
        vec![(0, 4, Tone::Success), (4, 7, Tone::Error)],
        "{report:?}"
    );
    // A separator above every prompt but the one on the first row.
    assert_eq!(report.separators, vec![4, 7]);
    assert_eq!(
        report.chips,
        vec![(0, "✓ 1.2 s".to_owned()), (4, "✗ 2 3 m".to_owned())]
    );
    assert!(report.highlighted.is_empty() && report.summaries.is_empty());
}

#[gpui::test]
fn a_running_block_has_a_neutral_bar_and_no_chip_until_it_finishes(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    let mut blocks = scenario();
    blocks[2] = Block {
        command_line: Some(27),
        output_start: Some(28),
        command: Some("sleep 9".into()),
        ..block(3, 27, BlockState::Running)
    };
    show(&view, cx, blocks, Modes::default());
    let report = view.read_with(cx, |v, _| v.painted_overlay().clone());
    assert_eq!(
        report.bars.last(),
        Some(&(7, 8, Tone::Neutral)),
        "{report:?}"
    );
    assert_eq!(report.chips.len(), 2, "no chip for the running command");
}

#[gpui::test]
fn a_chip_never_covers_what_its_prompt_line_shows(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    let long = "x".repeat(96);
    let mut lines = SCREEN.to_vec();
    lines[0] = &long;
    view.update(cx, |v, cx| {
        v.apply_render(snapshot(&lines, Modes::default(), 7, 20), cx)
    });
    draw(cx);
    set_blocks(&view, cx, scenario());
    let report = view.read_with(cx, |v, _| v.painted_overlay().clone());
    assert_eq!(report.chips, vec![(4, "✗ 2 3 m".to_owned())], "{report:?}");
}

#[gpui::test]
fn the_alternate_screen_hides_the_overlay_and_blocks_off_removes_it(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, scenario(), Modes::default());
    focus(&view, cx);
    assert!(view.read_with(cx, |v, _| v.blocks_visible()));
    assert!(!view.read_with(cx, |v, _| v.painted_overlay().bars.is_empty()));

    // vim: the overlay is gone and a prompt jump does nothing.
    view.update(cx, |v, cx| {
        v.apply_render(
            snapshot(
                &SCREEN,
                Modes {
                    alt_screen: true,
                    ..Modes::default()
                },
                0,
                0,
            ),
            cx,
        )
    });
    draw(cx);
    assert!(!view.read_with(cx, |v, _| v.blocks_visible()));
    assert_eq!(
        view.read_with(cx, |v, _| v.painted_overlay().clone()),
        hutan_terminal_view::blocks::OverlayReport::default()
    );
    recorder.take();
    cx.simulate_keystrokes("cmd-shift-up");
    assert_eq!(view.read_with(cx, |v, _| v.highlighted_block()), None);
    assert!(recorder.take().is_empty(), "keys pass through unchanged");

    // Back on the primary screen it returns; switching the setting off hot-removes it.
    show(&view, cx, scenario(), Modes::default());
    assert!(view.read_with(cx, |v, _| v.blocks_visible()));
    view.update(cx, |v, cx| {
        v.update_settings(
            TerminalViewSettings {
                blocks: BlockSettings {
                    enabled: false,
                    ..BlockSettings::default()
                },
                ..TerminalViewSettings::default()
            },
            cx,
        )
    });
    draw(cx);
    assert!(!view.read_with(cx, |v, _| v.blocks_visible()));
    assert_eq!(
        view.read_with(cx, |v, _| v.painted_overlay().clone()),
        hutan_terminal_view::blocks::OverlayReport::default()
    );
    cx.simulate_keystrokes("cmd-shift-up");
    assert_eq!(view.read_with(cx, |v, _| v.highlighted_block()), None);
}

#[gpui::test]
fn folding_replaces_the_output_with_one_summary_row_and_unfolding_restores_it(
    cx: &mut TestAppContext,
) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, scenario(), Modes::default());
    let collapsed_events = Rc::new(RefCell::new(0));
    let shared = view.read_with(cx, |v, _| v.shared().clone());
    let counter = Rc::clone(&collapsed_events);
    let _sub = cx.update(|_, cx| {
        cx.subscribe(&shared, move |_, event: &SharedEvent, _| {
            if *event == SharedEvent::Collapsed {
                *counter.borrow_mut() += 1;
            }
        })
    });
    let screen_before = raw(&view, cx);
    recorder.take();

    // Only finished blocks with output fold.
    assert!(!view.update(cx, |v, cx| v.toggle_block_collapsed(3, cx)));
    assert!(view.update(cx, |v, cx| v.toggle_block_collapsed(1, cx)));
    draw(cx);
    // The summary takes the place of three rows; older lines move in at the top, and the
    // view asks for the history it now needs.
    assert!(
        recorder.take().contains(&Call::History(0)),
        "the rows above the screen are needed"
    );
    give_history(&view, cx, 0, 2);
    assert_eq!(
        text(&view, cx),
        [
            "h18",
            "h19",
            "$ cmd-a",
            "… 3 lines, exit 0",
            "$ cmd-b",
            "b1",
            "b2",
            "$",
            "",
            ""
        ]
    );
    let report = view.read_with(cx, |v, _| v.painted_overlay().clone());
    assert_eq!(report.summaries, vec![3]);
    assert_eq!(
        report.bars,
        vec![(2, 4, Tone::Success), (4, 7, Tone::Error)],
        "the bar follows the block over the summary row"
    );
    assert_eq!(*collapsed_events.borrow(), 1);

    // View-only: the model's screen is the daemon's, and nothing was sent to the session.
    assert_eq!(raw(&view, cx), screen_before);
    assert!(recorder.inputs().is_empty());

    // The cursor stays on its own line while rows move around it.
    assert_eq!(view.read_with(cx, |v, _| v.model().cursor().row), 7);

    assert!(view.update(cx, |v, cx| v.toggle_block_collapsed(1, cx)));
    draw(cx);
    assert_eq!(text(&view, cx), SCREEN);
    assert_eq!(*collapsed_events.borrow(), 2);
    assert!(view.read_with(cx, |v, _| v.model().collapsed().is_empty()));
}

#[gpui::test]
fn a_fold_survives_new_output_and_is_dropped_with_its_block(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, scenario(), Modes::default());
    view.update(cx, |v, cx| v.toggle_block_collapsed(2, cx));
    // Another command finished: the fold of block 2 stays.
    let mut blocks = scenario();
    blocks[2] = Block {
        command_line: Some(27),
        output_start: Some(28),
        end_line: Some(29),
        command: Some("true".into()),
        exit_code: Some(0),
        finished_at_ms: Some(10_100),
        ..block(3, 27, BlockState::Finished)
    };
    set_blocks(&view, cx, blocks);
    assert!(view.read_with(cx, |v, _| v.model().is_collapsed(2)));
    // A fresh list without block 2 forgets the fold.
    set_blocks(&view, cx, vec![block(9, 27, BlockState::Prompt)]);
    assert!(view.read_with(cx, |v, _| v.model().collapsed().is_empty()));
}

#[gpui::test]
fn a_restored_fold_applies_once_the_blocks_arrive(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    view.update(cx, |v, cx| {
        v.apply_render(snapshot(&SCREEN, Modes::default(), 7, 20), cx)
    });
    draw(cx);
    let shared = view.read_with(cx, |v, _| v.shared().clone());
    shared.update(cx, |s, cx| s.set_collapsed([1u64].into(), cx));
    assert_eq!(text(&view, cx), SCREEN, "no blocks yet, nothing to fold");
    set_blocks(&view, cx, scenario());
    assert_eq!(text(&view, cx)[3], "… 3 lines, exit 0");
}

#[gpui::test]
fn prompt_jumps_move_the_highlight_and_scroll_to_the_block(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, scenario(), Modes::default());
    focus(&view, cx);
    let highlighted = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.highlighted_block());

    // From the live screen, ⌘⇧↑ goes to the command before the open prompt, then further up.
    cx.simulate_keystrokes("cmd-shift-up");
    assert_eq!(highlighted(cx), Some(2));
    assert_eq!(
        view.read_with(cx, |v, _| v.scroll_offset()),
        0,
        "already visible"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.painted_overlay().highlighted.clone()),
        vec![4, 5, 6]
    );
    cx.simulate_keystrokes("cmd-shift-up");
    assert_eq!(highlighted(cx), Some(1));
    cx.simulate_keystrokes("cmd-shift-up");
    assert_eq!(
        highlighted(cx),
        Some(1),
        "the first block has nothing above it"
    );
    cx.simulate_keystrokes("cmd-shift-down");
    assert_eq!(highlighted(cx), Some(2));
    cx.simulate_keystrokes("cmd-shift-down");
    assert_eq!(highlighted(cx), Some(3));
    // Past the newest block ⌘⇧↓ returns to the live screen.
    cx.simulate_keystrokes("cmd-shift-down");
    assert_eq!(highlighted(cx), None);
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 0);
}

#[gpui::test]
fn a_jump_scrolls_a_block_in_the_scrollback_into_view(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, vec![], Modes::default());
    // A command that ran 12 lines above the screen.
    let old = Block {
        command_line: Some(8),
        output_start: Some(9),
        end_line: Some(12),
        command: Some("old".into()),
        exit_code: Some(0),
        finished_at_ms: Some(10_100),
        ..block(1, 8, BlockState::Finished)
    };
    set_blocks(&view, cx, vec![old, block(2, 27, BlockState::Prompt)]);
    focus(&view, cx);
    recorder.take();

    cx.simulate_keystrokes("cmd-shift-up");
    assert_eq!(view.read_with(cx, |v, _| v.highlighted_block()), Some(1));
    // Line 8 at the top: 20 − 8 lines scrolled back, and the history it needs is requested.
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 12);
    assert!(
        recorder
            .take()
            .iter()
            .any(|c| matches!(c, Call::History(_)))
    );
    // Typing returns to the live screen and clears the highlight.
    cx.simulate_input("x");
    assert_eq!(view.read_with(cx, |v, _| v.highlighted_block()), None);
    assert_eq!(view.read_with(cx, |v, _| v.scroll_offset()), 0);
}

/// F-16: ⌘↑ and ⌘↓ in a terminal go to the top and the bottom of its scrollback (Terminal.app,
/// iTerm); the prompt jumps of the blocks are ⌘⇧↑ and ⌘⇧↓. Nothing reaches the shell.
#[gpui::test]
fn cmd_up_and_cmd_down_scroll_to_the_ends_of_the_scrollback(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, scenario(), Modes::default());
    focus(&view, cx);
    recorder.take();
    let offset = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.scroll_offset());
    assert_eq!(offset(cx), 0);

    cx.simulate_keystrokes("cmd-up");
    assert_eq!(
        offset(cx),
        20,
        "all 20 lines of scrollback above the screen"
    );
    assert_eq!(view.read_with(cx, |v, _| v.highlighted_block()), None);
    cx.simulate_keystrokes("cmd-down");
    assert_eq!(offset(cx), 0, "back on the live screen");
    cx.simulate_keystrokes("cmd-down");
    assert_eq!(offset(cx), 0);
    assert!(
        recorder
            .take()
            .iter()
            .all(|call| !matches!(call, Call::Input(_))),
        "the keys are the view's, the shell gets nothing"
    );

    // The block jump moved to ⌘⇧↑: it highlights a block and does not scroll to the top.
    cx.simulate_keystrokes("cmd-shift-up");
    assert_eq!(view.read_with(cx, |v, _| v.highlighted_block()), Some(2));
    assert_eq!(offset(cx), 0);
}

#[gpui::test]
fn scrolled_back_prompt_jumps_start_from_the_first_visible_line(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, scenario(), Modes::default());
    focus(&view, cx);
    // Scrolled 6 lines back the top row is line 14: above block 1's prompt.
    view.update(cx, |v, cx| v.scroll_lines(6, cx));
    cx.simulate_keystrokes("cmd-shift-down");
    assert_eq!(
        view.read_with(cx, |v, _| v.highlighted_block()),
        Some(1),
        "the first prompt below the top"
    );
}

#[gpui::test]
fn select_and_copy_act_on_the_current_blocks_output(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, scenario(), Modes::default());
    focus(&view, cx);
    recorder
        .texts
        .borrow_mut()
        .push((2, BlockPart::Output, "b1\nb2".to_owned()));

    // Without a highlight the current block is the newest command with output.
    cx.simulate_keystrokes("cmd-shift-a");
    assert_eq!(
        view.read_with(cx, |v, _| v.selection_text()).as_deref(),
        Some("b1\nb2")
    );
    assert_eq!(view.read_with(cx, |v, _| v.highlighted_block()), Some(2));

    recorder.take();
    cx.simulate_keystrokes("cmd-shift-c");
    assert_eq!(recorder.take(), vec![Call::BlockText(2, BlockPart::Output)]);
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("b1\nb2"),
        "the clipboard holds what terminal.block_text answered"
    );

    // A highlighted block is the current one.
    recorder
        .texts
        .borrow_mut()
        .push((1, BlockPart::Output, "a1\na2\na3".to_owned()));
    cx.simulate_keystrokes("cmd-shift-up");
    cx.simulate_keystrokes("cmd-shift-c");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("a1\na2\na3")
    );
    cx.simulate_keystrokes("cmd-shift-a");
    assert_eq!(
        view.read_with(cx, |v, _| v.selection_text()).as_deref(),
        Some("a1\na2\na3")
    );
}

#[gpui::test]
fn the_bound_actions_do_the_same_as_the_chords(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    cx.update(|_, cx| hutan_terminal_view::bind_default_keys(cx));
    show(&view, cx, scenario(), Modes::default());
    focus(&view, cx);
    recorder
        .texts
        .borrow_mut()
        .push((2, BlockPart::Output, "b1\nb2".to_owned()));
    cx.simulate_keystrokes("cmd-shift-up");
    assert_eq!(view.read_with(cx, |v, _| v.highlighted_block()), Some(2));
    cx.simulate_keystrokes("cmd-shift-c");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("b1\nb2")
    );
    cx.dispatch_action(hutan_terminal_view::ToggleBlockCollapse);
    assert!(view.read_with(cx, |v, _| v.model().is_collapsed(2)));
    cx.dispatch_action(hutan_terminal_view::ToggleBlockCollapse);
    assert!(view.read_with(cx, |v, _| v.model().collapsed().is_empty()));
    cx.dispatch_action(hutan_terminal_view::RerunBlock);
    assert_eq!(recorder.inputs(), vec![b"cmd-b\r".to_vec()]);
}

#[gpui::test]
fn rerun_types_the_command_and_enter_unless_something_runs(cx: &mut TestAppContext) {
    let (view, cx, recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, scenario(), Modes::default());
    recorder.take();
    assert!(view.update(cx, |v, cx| v.rerun_block(1, cx)));
    assert_eq!(recorder.inputs(), vec![b"cmd-a\r".to_vec()]);

    // A multi-line command is pasted (bracketed when the shell asked for it), then Enter.
    let mut blocks = scenario();
    blocks[1].command = Some("echo a \\\necho b".into());
    show(
        &view,
        cx,
        blocks,
        Modes {
            bracketed_paste: true,
            ..Modes::default()
        },
    );
    recorder.take();
    assert!(view.update(cx, |v, cx| v.rerun_block(2, cx)));
    assert_eq!(
        recorder.inputs(),
        vec![b"\x1b[200~echo a \\\necho b\x1b[201~\r".to_vec()]
    );

    // A running command owns the keyboard.
    let mut blocks = scenario();
    blocks[2] = Block {
        command_line: Some(27),
        output_start: Some(28),
        command: Some("sleep 9".into()),
        ..block(3, 27, BlockState::Running)
    };
    show(&view, cx, blocks, Modes::default());
    recorder.take();
    assert!(!view.update(cx, |v, cx| v.rerun_block(1, cx)));
    assert!(recorder.take().is_empty());
}

#[gpui::test]
fn send_to_agent_asks_the_owner_and_never_the_open_prompt(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, scenario(), Modes::default());
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(
            &view,
            move |_, event: &hutan_terminal_view::TerminalViewEvent, _| {
                sink.borrow_mut().push(event.clone());
            },
        )
        .detach();
    });
    assert!(view.update(cx, |v, cx| v.send_block_to_agent(2, cx)));
    // The open prompt (block 3) has nothing to send.
    assert!(!view.update(cx, |v, cx| v.send_block_to_agent(3, cx)));
    assert_eq!(
        *events.borrow(),
        [hutan_terminal_view::TerminalViewEvent::SendBlockToAgent { block: 2 }]
    );
}

#[gpui::test]
fn hovering_a_block_offers_its_actions(cx: &mut TestAppContext) {
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, scenario(), Modes::default());

    cx.simulate_mouse_move(at(3.0, 5.0), None, Modifiers::default());
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.hover_block()), Some(2));
    let actions = view
        .read_with(cx, |v, _| v.block_actions(2))
        .expect("actions for a finished block");
    assert!(actions.copy_command && actions.copy_output && actions.rerun && actions.send_to_agent);
    assert_eq!(actions.folded, Some(false));
    // The bar sits on the first row of the output (block 2's command is on row 4), so it
    // never covers the command or the exit badge.
    assert_eq!(view.read_with(cx, |v, _| v.block_actions_row(2)), Some(5));
    // The open prompt has nothing to offer.
    cx.simulate_mouse_move(at(3.0, 7.0), None, Modifiers::default());
    assert_eq!(view.read_with(cx, |v, _| v.hover_block()), Some(3));
    assert_eq!(view.read_with(cx, |v, _| v.block_actions(3)), None);
    // Blank space below the cursor belongs to no block.
    cx.simulate_mouse_move(at(3.0, 8.0), None, Modifiers::default());
    assert_eq!(view.read_with(cx, |v, _| v.hover_block()), None);
    // Leaving the grid hides it.
    cx.simulate_mouse_move(point(px(900.0), px(5.0)), None, Modifiers::default());
    assert_eq!(view.read_with(cx, |v, _| v.hover_block()), None);
}

#[gpui::test]
fn the_activity_indicator_runs_only_while_a_command_runs_and_motion_is_allowed(
    cx: &mut TestAppContext,
) {
    let running = || {
        let mut blocks = scenario();
        blocks[2] = Block {
            command_line: Some(27),
            output_start: Some(28),
            command: Some("sleep 9".into()),
            ..block(3, 27, BlockState::Running)
        };
        blocks
    };
    let (view, cx, _recorder) = setup(cx, TerminalViewSettings::default());
    show(&view, cx, scenario(), Modes::default());
    assert!(!view.read_with(cx, |v, _| v.block_animation_active()));
    set_blocks(&view, cx, running());
    assert!(view.read_with(cx, |v, _| v.block_animation_active()));
    cx.executor().advance_clock(Duration::from_millis(700));
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |v, _| v.animation_phase_for_tests()) > 0,
        "the indicator moved"
    );
    // The command finishes: the timer is gone.
    set_blocks(&view, cx, scenario());
    assert!(!view.read_with(cx, |v, _| v.block_animation_active()));

    // Reduce Motion: no timer at all.
    view.update(cx, |v, cx| {
        v.update_settings(
            TerminalViewSettings {
                blocks: BlockSettings {
                    animate: false,
                    ..BlockSettings::default()
                },
                ..TerminalViewSettings::default()
            },
            cx,
        )
    });
    set_blocks(&view, cx, running());
    assert!(!view.read_with(cx, |v, _| v.block_animation_active()));
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
            .child(div().w(px(300.0)).h(px(100.0)).child(self.preview.clone()))
    }
}

#[gpui::test]
fn previews_show_the_gutter_bars_only_and_never_fold(cx: &mut TestAppContext) {
    use gpui::AppContext as _;
    use hutan_terminal_view::SharedTerminal;

    let recorder = Rc::new(Recorder::default());
    let backend: Rc<dyn TerminalBackend> = recorder;
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
    cx.simulate_resize(size(px(800.0), px(f32::from(ROWS) * CELL_H + 2.0)));
    draw(cx);
    let (main, preview) = pair.read_with(cx, |p, _| (p.main.clone(), p.preview.clone()));
    show(&main, cx, scenario(), Modes::default());
    main.update(cx, |v, cx| v.toggle_block_collapsed(1, cx));
    draw(cx);

    let main_report = main.read_with(cx, |v, _| v.painted_overlay().clone());
    assert_eq!(main_report.summaries.len(), 1, "the pane folds");
    let report = preview.read_with(cx, |v, _| v.painted_overlay().clone());
    assert!(
        !report.bars.is_empty(),
        "gutter colors in the preview: {report:?}"
    );
    assert!(
        report
            .bars
            .iter()
            .any(|(_, _, tone)| *tone == Tone::Success)
    );
    assert!(report.bars.iter().any(|(_, _, tone)| *tone == Tone::Error));
    assert!(
        report.chips.is_empty()
            && report.separators.is_empty()
            && report.summaries.is_empty()
            && report.highlighted.is_empty(),
        "no chips, separators or folds in a preview: {report:?}"
    );
    assert!(preview.read_with(cx, |v, _| v.hover_block()).is_none());
    assert!(!preview.read_with(cx, |v, _| v.block_animation_active()));
}
