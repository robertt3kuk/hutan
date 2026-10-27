//! Scrolling past the end (EDITOR-POLISH, workability F-19): the view stops with the last line
//! at the bottom plus `editor.scroll_beyond_last_line` of its height (half a screen by
//! default), never on a blank view, and a view that comes back to a file that got shorter, or to
//! a window that got taller, is brought inside the new extent at its next frame.

use gpui::{TestAppContext, VisualTestContext, px, size};
use hutan_code_view::{CodeView, DEFAULT_SCROLL_BEYOND};

use crate::editing::{draw, open, run};
use hutan_code_view::actions as a;

fn numbered(lines: usize) -> String {
    (1..=lines).map(|n| format!("line {n}\n")).collect()
}

/// (line height, view height, content height of `rows` rows).
fn metrics(view: &gpui::Entity<CodeView>, rows: usize, cx: &VisualTestContext) -> (f32, f32, f32) {
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    let line_h = f32::from(layout.line_height);
    (
        line_h,
        f32::from(layout.bounds.size.height),
        rows as f32 * line_h,
    )
}

fn scroll_to_the_end(view: &gpui::Entity<CodeView>, cx: &mut VisualTestContext) -> f32 {
    view.update(cx, |v, cx| v.scroll_to(1.0e9, cx));
    draw(cx);
    view.read_with(cx, |v, _| v.scroll_y())
}

#[gpui::test]
fn scrolling_stops_half_a_screen_past_the_last_line_never_on_a_blank_view(cx: &mut TestAppContext) {
    // 100 lines and the empty line after the last break.
    let (view, _buffer, cx) = open(cx, "a.txt", &numbered(100), false);
    assert!((DEFAULT_SCROLL_BEYOND - 0.5).abs() < f32::EPSILON);
    let (line_h, view_h, content) = metrics(&view, 101, cx);
    let end = scroll_to_the_end(&view, cx);
    assert_eq!(
        end,
        content - view_h * 0.5,
        "the last row, half a screen above the bottom"
    );
    // The last line is on screen: its top is inside the view.
    let last_top = 100.0 * line_h - end;
    assert!(
        (0.0..view_h - line_h).contains(&last_top),
        "the last line is at {last_top} of {view_h}"
    );
    // One more step down does not move.
    view.update(cx, |v, cx| v.scroll_lines(5.0, cx));
    assert_eq!(view.read_with(cx, |v, _| v.scroll_y()), end);
    // Nor does the wheel of the keyboard: the caret at the end and a page down.
    run(cx, a::MoveToEnd);
    run(cx, a::PageDown);
    assert!(view.read_with(cx, |v, _| v.scroll_y()) <= end);
}

#[gpui::test]
fn the_setting_chooses_how_much_room_there_is(cx: &mut TestAppContext) {
    let (view, _buffer, cx) = open(cx, "a.txt", &numbered(100), false);
    let (_, view_h, content) = metrics(&view, 101, cx);
    view.update(cx, |v, cx| v.set_scroll_beyond_last_line(0.0, cx));
    assert_eq!(
        scroll_to_the_end(&view, cx),
        content - view_h,
        "0: the last line stops at the bottom"
    );
    view.update(cx, |v, cx| v.set_scroll_beyond_last_line(1.0, cx));
    assert_eq!(
        scroll_to_the_end(&view, cx),
        content,
        "1: a whole screen of room"
    );
    // Out of range or not a number: kept inside.
    view.update(cx, |v, cx| v.set_scroll_beyond_last_line(7.0, cx));
    assert_eq!(view.read_with(cx, |v, _| v.scroll_beyond_last_line()), 1.0);
    view.update(cx, |v, cx| v.set_scroll_beyond_last_line(f32::NAN, cx));
    assert_eq!(
        view.read_with(cx, |v, _| v.scroll_beyond_last_line()),
        DEFAULT_SCROLL_BEYOND
    );
    // A smaller room takes the view back at once, not at the next scroll.
    view.update(cx, |v, cx| v.set_scroll_beyond_last_line(1.0, cx));
    scroll_to_the_end(&view, cx);
    view.update(cx, |v, cx| v.set_scroll_beyond_last_line(0.0, cx));
    assert_eq!(view.read_with(cx, |v, _| v.scroll_y()), content - view_h);
}

#[gpui::test]
fn a_file_that_fits_the_window_does_not_scroll(cx: &mut TestAppContext) {
    let (view, _buffer, cx) = open(cx, "a.txt", &numbered(5), false);
    assert_eq!(scroll_to_the_end(&view, cx), 0.0);
}

#[gpui::test]
fn a_view_that_comes_back_to_a_shorter_file_or_a_taller_window_is_inside_the_extent(
    cx: &mut TestAppContext,
) {
    let (view, buffer, cx) = open(cx, "a.txt", &numbered(100), false);
    let end = scroll_to_the_end(&view, cx);
    assert!(end > 0.0);
    // While the tab is behind another the file gets shorter (an agent, a reload).
    buffer.update(cx, |b, cx| b.set_loaded(&numbered(60), false, cx));
    draw(cx);
    let (_, view_h, content) = metrics(&view, 61, cx);
    let now = view.read_with(cx, |v, _| v.scroll_y());
    assert!(
        now <= content - view_h * 0.5,
        "{now} is inside the new extent"
    );
    assert!(now > 0.0, "and the view did not jump to the top");
    // A taller window: the extent shrinks and the offset follows.
    scroll_to_the_end(&view, cx);
    cx.simulate_resize(size(px(900.0), px(900.0)));
    draw(cx);
    let (_, view_h, content) = metrics(&view, 61, cx);
    let now = view.read_with(cx, |v, _| v.scroll_y());
    assert!(
        now <= content - view_h * 0.5 + 0.01,
        "{now} fits the taller window"
    );
}
