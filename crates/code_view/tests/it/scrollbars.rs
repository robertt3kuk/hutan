//! The editor's scrollbars (SCROLLBARS item 2), in a GPUI test window: the vertical thumb
//! follows the scroll position and counts display rows (wrapped rows included), dragging it
//! reaches the last line, the horizontal thumb exists while a line is wider than the text area
//! and not while the view wraps, a click on the track pages and never moves the caret, the
//! overlay style shows the thumb with scrolling, and the ticks on the track (diagnostics, git
//! changes, find matches) are computed again only when one of them changes.

use std::rc::Rc;

use gpui::{
    Modifiers, MouseButton, ScrollWheelEvent, TestAppContext, VisualTestContext, point, px,
};
use hutan_buffer::Utf16Point;
use hutan_code_view::{
    CodeView, DiagnosticInput, FindQuery, GitBase, GitBaseKind, Severity, WrapMode, rgba,
};
use hutan_settings::{Rgba, ScrollbarsSetting};
use hutan_ui::Mark;

use crate::editing::{draw, open};

fn numbered(lines: usize) -> String {
    (1..=lines).map(|n| format!("line {n}\n")).collect()
}

fn thumb(
    view: &gpui::Entity<CodeView>,
    cx: &VisualTestContext,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |v, _| v.vertical_scrollbar().thumb())
}

fn x_of_bar(view: &gpui::Entity<CodeView>, cx: &VisualTestContext) -> gpui::Pixels {
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    layout.bounds.right() - px(2.0 + 3.5)
}

fn max_y(view: &gpui::Entity<CodeView>, cx: &VisualTestContext) -> f32 {
    view.read_with(cx, |v, cx| v.max_scroll_y(cx))
}

#[gpui::test]
fn the_thumb_reflects_the_scroll_at_the_top_in_the_middle_and_at_the_end(cx: &mut TestAppContext) {
    let (view, _buffer, cx) = open(cx, "a.txt", &numbered(300), false);
    let top = thumb(&view, cx).expect("300 lines do not fit");
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    assert_eq!(
        f32::from(top.origin.y - layout.bounds.origin.y),
        2.0,
        "at the top: at the track's start"
    );
    assert_eq!(
        top.right(),
        layout.bounds.right() - px(2.0),
        "against the right edge"
    );
    let max = max_y(&view, cx);

    view.update(cx, |v, cx| v.scroll_to(max / 2.0, cx));
    draw(cx);
    let middle = thumb(&view, cx).expect("scrolled");
    assert!(middle.origin.y > top.origin.y);
    assert!(
        middle.bottom() < layout.bounds.bottom() - px(20.0),
        "halfway is not at the bottom"
    );

    view.update(cx, |v, cx| v.scroll_to(max, cx));
    draw(cx);
    let end = thumb(&view, cx).expect("scrolled");
    // The track stops short of the corner the horizontal bar would take.
    assert_eq!(
        end.bottom(),
        layout.bounds.bottom() - px(2.0 + 7.0 + 2.0),
        "at the end: the track's end"
    );
}

#[gpui::test]
fn a_file_that_fits_has_no_thumb(cx: &mut TestAppContext) {
    let (view, _buffer, cx) = open(cx, "a.txt", &numbered(5), false);
    assert!(thumb(&view, cx).is_none());
}

#[gpui::test]
fn dragging_the_thumb_to_the_end_reaches_the_last_line(cx: &mut TestAppContext) {
    let (view, _buffer, cx) = open(cx, "a.txt", &numbered(300), false);
    let grab = thumb(&view, cx).expect("scrollable");
    let (x, y) = (x_of_bar(&view, cx), grab.origin.y + px(6.0));
    cx.simulate_mouse_down(point(x, y), MouseButton::Left, Modifiers::none());
    assert!(view.read_with(cx, |v, _| v.vertical_scrollbar().held()));
    cx.simulate_mouse_move(
        point(x, px(5_000.0)),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    draw(cx);
    let max = max_y(&view, cx);
    assert_eq!(view.read_with(cx, |v, _| v.scroll_y()), max, "at the end");
    cx.simulate_mouse_up(point(x, px(5_000.0)), MouseButton::Left, Modifiers::none());
    // The last line is on screen.
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    let last_top = 300.0 * f32::from(layout.line_height) - max;
    assert!(
        last_top < f32::from(layout.bounds.size.height),
        "line 300 is in view: top {last_top}"
    );
    // Back to the top.
    let end = thumb(&view, cx).expect("scrollable");
    cx.simulate_mouse_down(
        point(x, end.origin.y + px(6.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_move(
        point(x, px(-500.0)),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    assert_eq!(view.read_with(cx, |v, _| v.scroll_y()), 0.0);
    cx.simulate_mouse_up(point(x, px(-500.0)), MouseButton::Left, Modifiers::none());
}

#[gpui::test]
fn a_click_on_the_track_pages_and_leaves_the_caret_alone(cx: &mut TestAppContext) {
    let (view, _buffer, cx) = open(cx, "a.txt", &numbered(300), false);
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    let caret = view.read_with(cx, |v, _| v.selections().primary().head);
    let x = x_of_bar(&view, cx);
    cx.simulate_click(
        point(x, layout.bounds.origin.y + px(300.0)),
        Modifiers::none(),
    );
    draw(cx);
    assert_eq!(
        view.read_with(cx, |v, _| v.scroll_y()),
        f32::from(layout.bounds.size.height),
        "one page down"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.selections().primary().head),
        caret,
        "the click did not reach the text"
    );
}

#[gpui::test]
fn the_thumb_counts_display_rows_so_a_wrapped_file_has_a_shorter_one(cx: &mut TestAppContext) {
    let line = (0..40)
        .map(|n| format!("w{n:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    let text = (0..30).map(|_| format!("{line}\n")).collect::<String>();
    let (view, _buffer, cx) = open(cx, "a.md", &text, false);
    let plain = thumb(&view, cx).expect("30 lines scroll");
    view.update(cx, |v, cx| v.set_soft_wrap(WrapMode::Viewport, cx));
    draw(cx);
    let wrapped = thumb(&view, cx).expect("the wrapped rows scroll");
    assert!(
        wrapped.size.height < plain.size.height,
        "more rows in the same pane: a shorter thumb ({:?} vs {:?})",
        wrapped.size.height,
        plain.size.height
    );
    // And it reaches the last wrapped row.
    view.update(cx, |v, cx| v.scroll_to(1.0e9, cx));
    draw(cx);
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    let rows = view.read_with(cx, |v, cx| v.display_rows(cx));
    let max = max_y(&view, cx);
    let last_row_top = (rows - 1) as f32 * f32::from(layout.line_height) - max;
    assert!(last_row_top < f32::from(layout.bounds.size.height));
}

#[gpui::test]
fn the_horizontal_thumb_exists_for_a_line_wider_than_the_text_and_not_while_wrapping(
    cx: &mut TestAppContext,
) {
    let wide = "x".repeat(400);
    let (view, _buffer, cx) = open(cx, "a.txt", &format!("short\n{wide}\n"), false);
    let bar = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.horizontal_scrollbar().thumb());
    let thumb_x = bar(cx).expect("400 columns do not fit 900 px");
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    assert!(
        thumb_x.origin.x >= layout.text_left,
        "along the text area, not the gutter"
    );
    assert_eq!(
        thumb_x.bottom(),
        layout.bounds.bottom() - px(2.0),
        "at the bottom edge"
    );

    // Drag it to the end.
    let (y, start) = (thumb_x.origin.y + px(3.0), thumb_x.origin.x + px(8.0));
    cx.simulate_mouse_down(point(start, y), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        point(px(5_000.0), y),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    draw(cx);
    let max_x = view.read_with(cx, |v, cx| v.max_scroll_x(cx));
    assert!(max_x > 0.0);
    assert_eq!(view.read_with(cx, |v, _| v.scroll_x()), max_x);
    cx.simulate_mouse_up(point(px(5_000.0), y), MouseButton::Left, Modifiers::none());

    view.update(cx, |v, cx| v.set_soft_wrap(WrapMode::Viewport, cx));
    draw(cx);
    assert!(bar(cx).is_none(), "a wrapped view never scrolls sideways");
}

#[gpui::test]
fn a_file_whose_lines_fit_has_no_horizontal_thumb(cx: &mut TestAppContext) {
    let (view, _buffer, cx) = open(cx, "a.txt", &numbered(300), false);
    assert!(
        view.read_with(cx, |v, _| v.horizontal_scrollbar().thumb())
            .is_none()
    );
}

#[gpui::test]
fn the_overlay_thumb_appears_with_scrolling_and_fades(cx: &mut TestAppContext) {
    cx.update(|cx| {
        hutan_ui::scrollbar::set_system_always(cx, false);
        hutan_ui::scrollbar::set_setting(cx, ScrollbarsSetting::Scrolling);
    });
    let (view, _buffer, cx) = open(cx, "a.txt", &numbered(300), false);
    assert!(
        thumb(&view, cx).is_none(),
        "an overlay bar waits for scrolling"
    );
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    cx.simulate_event(ScrollWheelEvent {
        position: layout.bounds.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.scroll_y()) > 0.0);
    assert!(thumb(&view, cx).is_some(), "the wheel shows it");
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(1300));
    draw(cx);
    assert!(thumb(&view, cx).is_none(), "and it fades after a second");
}

fn marks(view: &gpui::Entity<CodeView>, cx: &mut VisualTestContext) -> Rc<[Mark]> {
    view.update(cx, |v, cx| v.scroll_marks(cx))
}

fn diagnostic(line: usize, severity: Severity) -> DiagnosticInput {
    DiagnosticInput {
        start: Utf16Point { line, character: 0 },
        end: Utf16Point { line, character: 4 },
        severity,
        message: "problem".into(),
        source: None,
        code: None,
    }
}

#[gpui::test]
fn diagnostics_find_matches_and_git_changes_are_ticks_on_the_track(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", &numbered(200), false);
    assert!(marks(&view, cx).is_empty(), "nothing to mark yet");

    buffer.update(cx, |b, cx| {
        b.set_diagnostics(
            vec![
                diagnostic(100, Severity::Error),
                diagnostic(100, Severity::Hint),
            ],
            cx,
        );
    });
    draw(cx);
    let ticks = marks(&view, cx);
    assert_eq!(ticks.len(), 1, "one tick per kind and bucket: {ticks:?}");
    assert!(
        (ticks[0].at - 0.5).abs() < 0.01,
        "line 101 of 201 rows is halfway: {}",
        ticks[0].at
    );
    let error = view.read_with(cx, |v, _| v.style().diagnostic_color(Severity::Error));
    assert_eq!(ticks[0].color, error, "the worse severity is the one drawn");

    view.update(cx, |v, cx| {
        v.set_find_query(FindQuery::literal("line 20"), cx)
    });
    cx.run_until_parked();
    draw(cx);
    let with_find = marks(&view, cx);
    assert!(
        with_find.len() > 1,
        "matches of \"line 20\", \"line 200\" … add ticks: {with_find:?}"
    );

    // A line that differs from the git base is a tick of the modified color near its place.
    view.update(cx, |v, cx| v.set_find_query(FindQuery::default(), cx));
    cx.run_until_parked();
    let base = numbered(200).replace("line 150\n", "other\n");
    buffer.update(cx, |b, cx| {
        b.set_git_base(Some(GitBase::new(&base, GitBaseKind::Index)), cx);
    });
    draw(cx);
    let modified = view.read_with(cx, |v, _| {
        let theme = &v.style().theme;
        rgba(Rgba {
            a: 1.0,
            ..theme.diff_modified
        })
    });
    let ticks = marks(&view, cx);
    let git: Vec<&Mark> = ticks.iter().filter(|m| m.color == modified).collect();
    assert_eq!(git.len(), 1, "one changed line, one tick: {ticks:?}");
    assert!(
        (git[0].at - 149.0 / 201.0).abs() < 0.01,
        "line 150 of 201 rows: {}",
        git[0].at
    );
}

#[gpui::test]
fn the_ticks_are_computed_again_only_when_something_changed(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", &numbered(200), false);
    buffer.update(cx, |b, cx| {
        b.set_diagnostics(vec![diagnostic(10, Severity::Warning)], cx)
    });
    draw(cx);
    let first = marks(&view, cx);
    draw(cx);
    draw(cx);
    assert!(
        Rc::ptr_eq(&first, &marks(&view, cx)),
        "frames without a change reuse the answer"
    );
    buffer.update(cx, |b, cx| {
        b.set_diagnostics(vec![diagnostic(20, Severity::Warning)], cx)
    });
    draw(cx);
    assert!(
        !Rc::ptr_eq(&first, &marks(&view, cx)),
        "a new publish recomputes"
    );
}
