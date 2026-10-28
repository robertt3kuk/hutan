#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! FX-FLAKY-6: what GPUI does with a click on a button that moves. The staging tests used to miss
//! clicks on a button that had just been redrawn (the file's *Stage file* after a hunk changed
//! sides, the *Undo* bar after a discard); the question was whether GPUI hit-tests against the
//! frame before the one on screen, which would be a bug a user could hit. It is not:
//!
//! - a press and a release are hit-tested against the frame that is on screen *when they
//!   arrive*, so a click aimed with that frame's bounds always reaches the button, however
//!   often the page re-flowed before;
//! - a press on a button that then moves from under the pointer before the release is not a
//!   click (GPUI clears the pending press when the release is over something else, zed issue
//!   24600): a button that jumps away right after being pressed does not fire.
//!
//! The flake was in the harness: `simulate_click` runs every ready task between the press and the
//! release, and under load the daemon's answers re-flow the page in that gap. `common::click_at`
//! dispatches both in one go. These tests pin the GPUI behaviour that the harness relies on.

use crate::common;

use gpui::{
    Context, IntoElement, Modifiers, MouseButton, Render, TestAppContext, Window, div, point,
    prelude::*, px,
};

/// A row with a spacer in front of an 80 × 24 button: `shifted` pushes the button 200 px right.
#[derive(Default)]
struct Mover {
    shifted: bool,
    clicks: u32,
}

impl Mover {
    /// The centre of the button in the current layout.
    fn centre(&self) -> gpui::Point<gpui::Pixels> {
        let left = if self.shifted { 200.0 } else { 0.0 };
        point(px(left + 40.0), px(12.0))
    }
}

impl Render for Mover {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .child(
                div()
                    .w(px(if self.shifted { 200.0 } else { 0.0 }))
                    .h(px(24.0)),
            )
            .child(
                div()
                    .id("button")
                    .w(px(80.0))
                    .h(px(24.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.clicks += 1;
                        cx.notify();
                    })),
            )
    }
}

#[gpui::test]
fn a_click_is_hit_tested_against_the_frame_on_screen(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| Mover::default());
    cx.update(|window, _| window.activate_window());
    common::draw(cx);
    let clicks = |cx: &gpui::VisualTestContext| view.read_with(cx, |m, _| m.clicks);

    // The button is where the first frame has it.
    let before = view.read_with(cx, |m, _| m.centre());
    common::click_at(cx, before);
    assert_eq!(clicks(cx), 1);

    // The page re-flows (the test platform draws as soon as the update is done): the button is
    // gone from where it was, a click there reaches nothing — not the frame before.
    view.update(cx, |m, cx| {
        m.shifted = true;
        cx.notify();
    });
    common::click_at(cx, before);
    assert_eq!(
        clicks(cx),
        1,
        "the old place is empty in the frame on screen"
    );
    // Where the frame on screen has it, the click lands.
    let after = view.read_with(cx, |m, _| m.centre());
    common::click_at(cx, after);
    assert_eq!(clicks(cx), 2);

    // Pressed, then the button jumps away before the release: no click (zed issue 24600) …
    cx.simulate_mouse_down(after, MouseButton::Left, Modifiers::none());
    view.update(cx, |m, cx| {
        m.shifted = false;
        cx.notify();
    });
    cx.simulate_mouse_up(after, MouseButton::Left, Modifiers::none());
    assert_eq!(
        clicks(cx),
        2,
        "a button that moved after the press is not clicked"
    );
    // … and it is not left half-pressed: the next whole click works.
    let back = view.read_with(cx, |m, _| m.centre());
    common::click_at(cx, back);
    assert_eq!(clicks(cx), 3);
}
