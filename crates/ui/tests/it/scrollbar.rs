//! The scrollbar over a real scrolled element: the thumb reflects the content (top, middle, end),
//! dragging it reaches the last row, a click on the track pages, the overlay style appears with
//! scrolling or the pointer and fades on the test clock, `always` keeps it, and a bar that is
//! under an occluding overlay is not reached.

use std::time::Duration;

use gpui::{
    Context, Entity, InteractiveElement, IntoElement, Modifiers, MouseButton, ParentElement,
    Render, ScrollHandle, StatefulInteractiveElement, Styled, TestAppContext, VisualTestContext,
    Window, div, point, px,
};
use hutan_settings::ScrollbarsSetting;
use hutan_ui::scrollbar::{self, Axis, Scrollbar};

const ROW: f32 = 20.0;
const HEIGHT: f32 = 200.0;
const WIDTH: f32 = 300.0;

struct Probe {
    handle: ScrollHandle,
    bar: Scrollbar,
    rows: usize,
    occluded: bool,
    renders: std::cell::Cell<usize>,
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        div()
            .id("probe")
            .relative()
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .child(
                div()
                    .id("list")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.handle)
                    .children((0..self.rows).map(|n| div().h(px(ROW)).child(format!("row {n}")))),
            )
            .child(self.bar.element())
            .when_occluded(self.occluded)
    }
}

trait WhenOccluded: ParentElement + Sized {
    fn when_occluded(self, occluded: bool) -> Self;
}

impl WhenOccluded for gpui::Stateful<gpui::Div> {
    fn when_occluded(self, occluded: bool) -> Self {
        if occluded {
            self.child(div().absolute().top_0().left_0().size_full().occlude())
        } else {
            self
        }
    }
}

fn open(
    cx: &mut TestAppContext,
    rows: usize,
    occluded: bool,
) -> (Entity<Probe>, &mut VisualTestContext) {
    let (view, cx) = cx.add_window_view(move |_, _| {
        let handle = ScrollHandle::new();
        Probe {
            bar: Scrollbar::new(handle.clone(), Axis::Vertical),
            handle,
            rows,
            occluded,
            renders: std::cell::Cell::default(),
        }
    });
    draw(cx);
    draw(cx);
    (view, cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn setting(cx: &mut TestAppContext, setting: ScrollbarsSetting) {
    cx.update(|cx| {
        scrollbar::set_system_always(cx, false);
        scrollbar::set_setting(cx, setting);
    });
}

fn scroll_to(view: &Entity<Probe>, cx: &mut VisualTestContext, y: f32) {
    view.read_with(cx, |p, _| p.handle.set_offset(point(px(0.0), px(-y))));
    draw(cx);
}

fn offset(view: &Entity<Probe>, cx: &VisualTestContext) -> f32 {
    view.read_with(cx, |p, _| -f32::from(p.handle.offset().y))
}

fn thumb(view: &Entity<Probe>, cx: &VisualTestContext) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |p, _| p.bar.thumb())
}

fn opacity(view: &Entity<Probe>, cx: &VisualTestContext) -> f32 {
    view.read_with(cx, |p, _| p.bar.opacity())
}

fn advance(cx: &mut VisualTestContext, by: Duration) {
    cx.executor().advance_clock(by);
    draw(cx);
}

fn edge_x() -> gpui::Pixels {
    // The thumb is 7 px wide, 2 px from the right edge.
    px(WIDTH - 2.0 - 3.5)
}

fn mouse_move(cx: &mut VisualTestContext, x: gpui::Pixels, y: f32, pressed: bool) {
    cx.simulate_mouse_move(
        point(x, px(y)),
        pressed.then_some(MouseButton::Left),
        Modifiers::none(),
    );
}

#[gpui::test]
fn the_thumb_reflects_the_content_at_the_top_in_the_middle_and_at_the_end(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, 100, false);
    let max = 100.0 * ROW - HEIGHT;
    let track_len = HEIGHT - 4.0;
    let top = thumb(&view, cx).expect("100 rows do not fit");
    // 200 of 2 000 in view is 20 px of a 196 px track: the 28 px minimum holds it grabbable.
    assert_eq!(f32::from(top.size.height), 28.0);
    assert_eq!(
        f32::from(top.origin.y),
        2.0,
        "at the top: at the track's start"
    );
    assert_eq!(
        f32::from(top.right()),
        WIDTH - 2.0,
        "against the right edge"
    );

    scroll_to(&view, cx, max / 2.0);
    let middle = thumb(&view, cx).expect("scrolled");
    let travel = track_len - 28.0;
    assert!(
        (f32::from(middle.origin.y) - (2.0 + travel / 2.0)).abs() < 0.5,
        "halfway: in the middle of the travel, got {:?}",
        middle.origin.y
    );

    scroll_to(&view, cx, max);
    let end = thumb(&view, cx).expect("scrolled");
    assert_eq!(
        f32::from(end.bottom()),
        2.0 + track_len,
        "at the end: the track's end"
    );
}

#[gpui::test]
fn a_thumb_with_proportions_is_the_share_of_the_content_in_view(cx: &mut TestAppContext) {
    // 20 rows: 400 px, half in view: half of the 196 px track.
    let (view, cx) = open(cx, 20, false);
    let thumb = thumb(&view, cx).expect("20 rows do not fit");
    assert_eq!(f32::from(thumb.size.height), 98.0);
}

#[gpui::test]
fn content_that_fits_has_no_thumb(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, 5, false);
    assert!(thumb(&view, cx).is_none());
    assert_eq!(opacity(&view, cx), 0.0);
}

#[gpui::test]
fn dragging_the_thumb_to_the_end_reaches_the_last_row(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, 100, false);
    let max = 100.0 * ROW - HEIGHT;
    let grab = thumb(&view, cx).expect("scrollable");
    let y = f32::from(grab.origin.y) + 10.0;
    cx.simulate_mouse_down(point(edge_x(), px(y)), MouseButton::Left, Modifiers::none());
    assert!(view.read_with(cx, |p, _| p.bar.held()));
    // Past the end of the track: the offset stops at the end.
    mouse_move(cx, edge_x(), 900.0, true);
    draw(cx);
    assert_eq!(offset(&view, cx), max, "the last row is the last in view");
    cx.simulate_mouse_up(
        point(edge_x(), px(900.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    assert!(!view.read_with(cx, |p, _| p.bar.held()));
    // And back to the start.
    let end = thumb(&view, cx).expect("scrollable");
    let y = f32::from(end.origin.y) + 10.0;
    cx.simulate_mouse_down(point(edge_x(), px(y)), MouseButton::Left, Modifiers::none());
    mouse_move(cx, edge_x(), -300.0, true);
    assert_eq!(offset(&view, cx), 0.0);
    cx.simulate_mouse_up(
        point(edge_x(), px(-300.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
}

#[gpui::test]
fn a_drag_moves_the_content_in_proportion_to_the_thumb(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, 100, false);
    let max = 100.0 * ROW - HEIGHT;
    let travel = (HEIGHT - 4.0) - 28.0;
    let grab = thumb(&view, cx).expect("scrollable");
    let y = f32::from(grab.origin.y) + 10.0;
    cx.simulate_mouse_down(point(edge_x(), px(y)), MouseButton::Left, Modifiers::none());
    mouse_move(cx, edge_x(), y + travel / 2.0, true);
    let moved = offset(&view, cx);
    assert!(
        (moved - max / 2.0).abs() < 1.0,
        "half the travel is half the content: {moved} of {max}"
    );
    cx.simulate_mouse_up(point(edge_x(), px(y)), MouseButton::Left, Modifiers::none());
}

#[gpui::test]
fn a_click_on_the_track_pages_towards_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, 100, false);
    let max = 100.0 * ROW - HEIGHT;
    // Below the thumb: one viewport down.
    cx.simulate_click(point(edge_x(), px(150.0)), Modifiers::none());
    assert_eq!(offset(&view, cx), HEIGHT);
    assert!(
        !view.read_with(cx, |p, _| p.bar.held()),
        "a click on the track grabs nothing"
    );
    draw(cx);
    cx.simulate_click(point(edge_x(), px(190.0)), Modifiers::none());
    assert_eq!(offset(&view, cx), 2.0 * HEIGHT);
    // Above the thumb at the end: one viewport up, never past the start.
    scroll_to(&view, cx, max);
    cx.simulate_click(point(edge_x(), px(10.0)), Modifiers::none());
    assert_eq!(offset(&view, cx), max - HEIGHT);
    scroll_to(&view, cx, 50.0);
    cx.simulate_click(point(edge_x(), px(190.0)), Modifiers::none());
    draw(cx);
    cx.simulate_click(point(edge_x(), px(2.0)), Modifiers::none());
    assert!(offset(&view, cx) >= 0.0);
}

#[gpui::test]
fn the_overlay_appears_with_scrolling_holds_a_second_and_fades_out(cx: &mut TestAppContext) {
    setting(cx, ScrollbarsSetting::Scrolling);
    let (view, cx) = open(cx, 100, false);
    assert!(
        thumb(&view, cx).is_none(),
        "nothing scrolled, nothing shown"
    );
    assert_eq!(opacity(&view, cx), 0.0);

    scroll_to(&view, cx, 300.0);
    assert_eq!(opacity(&view, cx), 1.0, "scrolling shows it");
    assert!(thumb(&view, cx).is_some());

    advance(cx, Duration::from_millis(900));
    assert_eq!(opacity(&view, cx), 1.0, "held for a second");
    advance(cx, Duration::from_millis(200));
    let fading = opacity(&view, cx);
    assert!(fading > 0.0 && fading < 1.0, "fading at 1.1 s: {fading}");
    advance(cx, Duration::from_millis(200));
    assert_eq!(opacity(&view, cx), 0.0, "gone");
    assert!(thumb(&view, cx).is_none());

    // Scrolling again brings it back.
    scroll_to(&view, cx, 600.0);
    assert_eq!(opacity(&view, cx), 1.0);
}

#[gpui::test]
fn the_end_of_the_hold_wakes_the_view_by_itself(cx: &mut TestAppContext) {
    setting(cx, ScrollbarsSetting::Scrolling);
    let (view, cx) = open(cx, 100, false);
    scroll_to(&view, cx, 300.0);
    // No frame is asked for by hand: the timer the bar set notifies the view.
    let before = view.read_with(cx, |p, _| p.renders.get());
    cx.executor().advance_clock(Duration::from_millis(1000));
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |p, _| p.renders.get()) > before,
        "the hold's end asked for a frame"
    );
}

#[gpui::test]
fn under_reduced_motion_the_fade_is_at_most_150_ms(cx: &mut TestAppContext) {
    setting(cx, ScrollbarsSetting::Scrolling);
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, 100, false);
    scroll_to(&view, cx, 300.0);
    advance(cx, Duration::from_millis(1160));
    assert_eq!(opacity(&view, cx), 0.0);
}

#[gpui::test]
fn the_pointer_near_the_edge_shows_the_bar_and_keeps_it_while_it_stays(cx: &mut TestAppContext) {
    setting(cx, ScrollbarsSetting::Scrolling);
    let (view, cx) = open(cx, 100, false);
    assert_eq!(opacity(&view, cx), 0.0);
    mouse_move(cx, px(WIDTH - 6.0), 100.0, false);
    draw(cx);
    assert_eq!(opacity(&view, cx), 1.0, "hover near the edge shows it");
    advance(cx, Duration::from_secs(10));
    assert_eq!(
        opacity(&view, cx),
        1.0,
        "and keeps it while the pointer stays"
    );

    // The pointer goes away to the middle of the list: the hold starts there.
    mouse_move(cx, px(100.0), 100.0, false);
    draw(cx);
    assert_eq!(opacity(&view, cx), 1.0);
    advance(cx, Duration::from_millis(1300));
    assert_eq!(opacity(&view, cx), 0.0);
}

#[gpui::test]
fn the_always_style_keeps_a_thumb_without_scrolling(cx: &mut TestAppContext) {
    setting(cx, ScrollbarsSetting::Always);
    let (view, cx) = open(cx, 100, false);
    assert!(thumb(&view, cx).is_some());
    advance(cx, Duration::from_secs(30));
    assert_eq!(opacity(&view, cx), 1.0);
}

#[gpui::test]
fn the_system_setting_follows_the_scroller_style_until_overridden(cx: &mut TestAppContext) {
    cx.update(|cx| {
        scrollbar::set_setting(cx, ScrollbarsSetting::System);
        scrollbar::set_system_always(cx, true);
    });
    let (view, cx) = open(cx, 100, false);
    assert_eq!(
        opacity(&view, cx),
        1.0,
        "macOS shows its scroll bars always"
    );
    cx.update(|_, cx| scrollbar::set_system_always(cx, false));
    draw(cx);
    assert_eq!(
        opacity(&view, cx),
        0.0,
        "macOS switched to overlay bars: live"
    );
    cx.update(|_, cx| scrollbar::set_setting(cx, ScrollbarsSetting::Always));
    draw(cx);
    assert_eq!(opacity(&view, cx), 1.0, "the override wins");
}

#[gpui::test]
fn a_bar_under_an_occluding_overlay_is_not_reached(cx: &mut TestAppContext) {
    setting(cx, ScrollbarsSetting::Always);
    let (view, cx) = open(cx, 100, true);
    cx.simulate_click(point(edge_x(), px(150.0)), Modifiers::none());
    assert_eq!(
        offset(&view, cx),
        0.0,
        "the click did not page the list under the overlay"
    );
    let grab = thumb(&view, cx).expect("scrollable");
    cx.simulate_mouse_down(
        point(edge_x(), grab.origin.y + px(5.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    assert!(!view.read_with(cx, |p, _| p.bar.held()));
}
