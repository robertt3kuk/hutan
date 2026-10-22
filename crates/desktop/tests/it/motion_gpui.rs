#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Motion (UX-MOTION): every transition reaches its end state at the token duration on the test
//! clock, a reversal continues from where it is, Reduce Motion removes movement and caps fades,
//! and a settled window requests no frames.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, Styled, TestAppContext, VisualTestContext,
    Window, div, px,
};
use hutan_desktop::workbench::ui::motion::{self, Kind, Presence, ReduceMotion};
use hutan_settings::ReduceMotionSetting;

/// What the last frame of a probe drew.
#[derive(Default)]
struct Seen {
    value: Cell<f32>,
    shift: Cell<f32>,
    settled: Cell<bool>,
    draws: Cell<usize>,
}

/// A view with one popover-like surface whose `shown` flag the test flips.
struct Probe {
    kind: Kind,
    shown: bool,
    from_start: bool,
    seen: Rc<Seen>,
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let seen = Rc::clone(&self.seen);
        let surface = div().w(px(100.0)).h(px(40.0));
        let animate = move |element: gpui::Div, frame: motion::Frame| {
            seen.value.set(frame.value);
            seen.shift.set(frame.shift);
            seen.settled.set(frame.settled);
            seen.draws.set(seen.draws.get() + 1);
            frame.slide_fade(element)
        };
        let target = if self.shown { 1.0 } else { 0.0 };
        let transition = motion::transition("probe", self.kind, target, surface, animate);
        div().child(if self.from_start {
            transition.from(0.0)
        } else {
            transition
        })
    }
}

fn open(
    cx: &mut TestAppContext,
    kind: Kind,
    shown: bool,
    from_start: bool,
    reduced: bool,
) -> (Entity<Probe>, Rc<Seen>, &mut VisualTestContext) {
    set_reduce(cx, reduced);
    let seen = Rc::new(Seen::default());
    let probe_seen = Rc::clone(&seen);
    let (view, cx) = cx.add_window_view(move |_, _| Probe {
        kind,
        shown,
        from_start,
        seen: probe_seen,
    });
    (view, seen, cx)
}

/// Draws a frame; returns how many frames the draw asked for (animations ask for the next).
fn frame(cx: &mut VisualTestContext) -> usize {
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.simulate_next_frame(cx)
    })
}

fn advance(cx: &mut VisualTestContext, by: Duration) {
    cx.executor().advance_clock(by);
}

fn set_reduce(cx: &mut TestAppContext, on: bool) {
    cx.update(|cx| {
        motion::apply(
            cx,
            ReduceMotion {
                system: false,
                setting: if on {
                    ReduceMotionSetting::On
                } else {
                    ReduceMotionSetting::Off
                },
            },
        );
    });
}

#[gpui::test]
fn a_popover_fades_and_slides_in_over_150_ms_and_ends_in_place(cx: &mut TestAppContext) {
    let (_view, seen, cx) = open(cx, Kind::Popover, true, true, false);
    frame(cx);
    assert_eq!(seen.value.get(), 0.0, "starts hidden");
    assert_eq!(seen.shift.get(), 4.0, "starts 4 px away");
    assert!(!seen.settled.get());

    advance(cx, Duration::from_millis(75));
    frame(cx);
    let middle = seen.value.get();
    assert!(
        middle > 0.5 && middle < 1.0,
        "ease-out is past half at half time: {middle}"
    );
    assert!(seen.shift.get() > 0.0 && seen.shift.get() < 2.0);

    advance(cx, Duration::from_millis(75));
    frame(cx);
    assert_eq!(seen.value.get(), 1.0);
    assert_eq!(seen.shift.get(), 0.0);
    assert!(seen.settled.get());
}

#[gpui::test]
fn a_sheet_takes_200_ms_in_and_a_toast_the_same(cx: &mut TestAppContext) {
    for kind in [Kind::Sheet, Kind::Toast] {
        let (_view, seen, cx) = open(cx, kind, true, true, false);
        frame(cx);
        advance(cx, Duration::from_millis(190));
        frame(cx);
        assert!(!seen.settled.get(), "{kind:?} is still moving at 190 ms");
        advance(cx, Duration::from_millis(10));
        frame(cx);
        assert!(seen.settled.get(), "{kind:?} arrives at 200 ms");
        assert_eq!(seen.value.get(), 1.0);
    }
}

#[gpui::test]
fn a_settled_transition_asks_for_no_frames(cx: &mut TestAppContext) {
    let (_view, seen, cx) = open(cx, Kind::Popover, true, true, false);
    frame(cx);
    assert!(frame(cx) >= 1, "moving: the next frame is requested");
    advance(cx, Duration::from_millis(150));
    frame(cx);
    assert!(seen.settled.get());
    for _ in 0..5 {
        assert_eq!(frame(cx), 0, "an idle window requests nothing");
    }
}

#[gpui::test]
fn a_reversal_continues_from_the_value_on_screen(cx: &mut TestAppContext) {
    let (view, seen, cx) = open(cx, Kind::Popover, true, true, false);
    frame(cx);
    advance(cx, Duration::from_millis(40));
    frame(cx);
    let on_screen = seen.value.get();
    assert!(on_screen > 0.0 && on_screen < 1.0);

    view.update(cx, |probe, cx| {
        probe.shown = false;
        cx.notify();
    });
    frame(cx);
    assert!(
        (seen.value.get() - on_screen).abs() < 1e-3,
        "no jump: {} vs {on_screen}",
        seen.value.get()
    );
    // The exit is 100 ms scaled by the distance left: done by then, not before.
    advance(cx, Duration::from_millis(20));
    frame(cx);
    assert!(seen.value.get() < on_screen);
    advance(cx, Duration::from_millis(80));
    frame(cx);
    assert_eq!(seen.value.get(), 0.0);
    assert!(seen.settled.get());
}

#[gpui::test]
fn an_exit_is_shorter_than_the_enter(cx: &mut TestAppContext) {
    let (view, seen, cx) = open(cx, Kind::Popover, true, false, false);
    frame(cx);
    assert_eq!(seen.value.get(), 1.0, "mounted shown: no entrance");
    view.update(cx, |probe, cx| {
        probe.shown = false;
        cx.notify();
    });
    frame(cx);
    advance(cx, Duration::from_millis(100));
    frame(cx);
    assert!(
        seen.settled.get() && seen.value.get() == 0.0,
        "gone at 100 ms"
    );
}

#[gpui::test]
fn reduce_motion_removes_movement_and_caps_the_fade(cx: &mut TestAppContext) {
    let (_view, seen, cx) = open(cx, Kind::Sheet, true, true, true);
    frame(cx);
    assert_eq!(seen.shift.get(), 0.0, "nothing moves");
    assert!(!seen.settled.get() || seen.value.get() == 1.0);
    advance(cx, Duration::from_millis(150));
    frame(cx);
    assert!(
        seen.settled.get(),
        "a sheet's 200 ms fade is capped at 150 ms"
    );
    assert_eq!(seen.value.get(), 1.0);
    assert_eq!(seen.shift.get(), 0.0);
}

#[gpui::test]
fn reduce_motion_follows_the_system_live_and_the_setting_overrides_it(cx: &mut TestAppContext) {
    let (_view, _seen, cx) = open(cx, Kind::Popover, true, false, false);
    cx.update(|_, cx| {
        motion::apply(
            cx,
            ReduceMotion {
                system: false,
                setting: ReduceMotionSetting::System,
            },
        );
        assert!(!cx.reduce_motion() && !motion::reduce_motion());
        motion::set_system(cx, true);
        assert!(
            cx.reduce_motion() && motion::reduce_motion(),
            "the system flag arrived live"
        );
        motion::set_setting(cx, ReduceMotionSetting::Off);
        assert!(!cx.reduce_motion(), "off overrides the system");
        motion::set_setting(cx, ReduceMotionSetting::System);
        assert!(cx.reduce_motion());
        motion::set_system(cx, false);
        assert!(!cx.reduce_motion());
        motion::set_setting(cx, ReduceMotionSetting::On);
        assert!(
            cx.reduce_motion() && motion::reduce_motion(),
            "on overrides the system"
        );
        motion::set_setting(cx, ReduceMotionSetting::System);
    });
}

/// A group that expands: the height runs 0 to the content's own height in 150 ms.
struct Group {
    open: bool,
    height: Rc<Cell<Option<f32>>>,
}

impl Render for Group {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let content = div().w(px(100.0)).h(px(60.0));
        let height = Rc::clone(&self.height);
        div()
            .w(px(100.0))
            .child(motion::expand("group-body", self.open, content))
            .on_children_prepainted(move |bounds, _, _| {
                height.set(bounds.first().map(|b| f32::from(b.size.height)));
            })
    }
}

fn open_group(
    cx: &mut TestAppContext,
    open: bool,
    reduced: bool,
) -> (Entity<Group>, Rc<Cell<Option<f32>>>, &mut VisualTestContext) {
    set_reduce(cx, reduced);
    let height = Rc::new(Cell::new(None));
    let seen = Rc::clone(&height);
    let (view, cx) = cx.add_window_view(move |_, _| Group { open, height: seen });
    (view, height, cx)
}

#[gpui::test]
fn a_group_expands_to_its_height_in_150_ms_and_collapses_back(cx: &mut TestAppContext) {
    let (view, height, cx) = open_group(cx, false, false);
    frame(cx);
    assert_eq!(height.get(), Some(0.0), "closed and drawn: nothing");

    view.update(cx, |group, cx| {
        group.open = true;
        cx.notify();
    });
    // The first frame measures the content; the clock starts on the next.
    frame(cx);
    frame(cx);
    advance(cx, Duration::from_millis(75));
    frame(cx);
    let middle = height.get().unwrap();
    assert!(middle > 20.0 && middle < 60.0, "mid-flight height {middle}");
    advance(cx, Duration::from_millis(75));
    frame(cx);
    assert_eq!(height.get(), Some(60.0));
    assert_eq!(frame(cx), 0, "arrived: no more frames");

    view.update(cx, |group, cx| {
        group.open = false;
        cx.notify();
    });
    frame(cx);
    advance(cx, Duration::from_millis(150));
    frame(cx);
    assert_eq!(height.get(), Some(0.0));
    assert_eq!(frame(cx), 0);
}

#[gpui::test]
fn a_group_is_instant_under_reduce_motion(cx: &mut TestAppContext) {
    let (view, height, cx) = open_group(cx, false, true);
    frame(cx);
    view.update(cx, |group, cx| {
        group.open = true;
        cx.notify();
    });
    frame(cx);
    frame(cx);
    assert_eq!(height.get(), Some(60.0), "no height animation");
    assert_eq!(frame(cx), 0);
    view.update(cx, |group, cx| {
        group.open = false;
        cx.notify();
    });
    frame(cx);
    assert_eq!(height.get(), Some(0.0));
}

/// Something that leaves: the owner keeps drawing it for the exit time.
struct Leaver {
    presence: Presence,
    drawn: Rc<RefCell<Vec<bool>>>,
}

impl Render for Leaver {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = cx.background_executor().now();
        self.drawn.borrow_mut().push(self.presence.draws(now));
        div()
    }
}

#[gpui::test]
fn a_thing_that_leaves_is_drawn_for_its_exit_and_then_not(cx: &mut TestAppContext) {
    let drawn = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&drawn);
    set_reduce(cx, false);
    let (view, cx) = cx.add_window_view(move |_, _| Leaver {
        presence: Presence::new(true),
        drawn: seen,
    });
    frame(cx);
    view.update(cx, |leaver, cx| {
        leaver.presence.set(false, Kind::Popover, cx);
        cx.notify();
    });
    frame(cx);
    assert_eq!(drawn.borrow().last(), Some(&true), "still leaving");
    advance(cx, Duration::from_millis(99));
    cx.run_until_parked();
    frame(cx);
    assert_eq!(
        drawn.borrow().last(),
        Some(&true),
        "100 ms exit: not yet gone at 99 ms"
    );
    advance(cx, Duration::from_millis(1));
    cx.run_until_parked();
    frame(cx);
    assert_eq!(
        drawn.borrow().last(),
        Some(&false),
        "gone once the exit is over"
    );
}

#[gpui::test]
fn a_thing_that_leaves_is_gone_at_once_when_exits_are_instant(cx: &mut TestAppContext) {
    let drawn = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&drawn);
    set_reduce(cx, true);
    let (view, cx) = cx.add_window_view(move |_, _| Leaver {
        presence: Presence::new(true),
        drawn: seen,
    });
    view.update(cx, |leaver, cx| {
        // A collapse is instant under Reduce Motion.
        leaver.presence.set(false, Kind::Collapse, cx);
        cx.notify();
    });
    frame(cx);
    assert_eq!(drawn.borrow().last(), Some(&false));
}

#[gpui::test]
fn the_setting_reaches_the_workbench_live(cx: &mut TestAppContext) {
    use crate::common::{draw, offline_connection, open_with};
    let (connection, settings, _) = offline_connection();
    let (_view, cx) = open_with(cx, settings.clone(), connection, None);
    draw(cx);
    assert!(!cx.update(|_, cx| cx.reduce_motion()), "system: off here");
    settings
        .write_user("appearance.reduce_motion", toml::Value::String("on".into()))
        .unwrap();
    for _ in 0..20 {
        draw(cx);
        cx.run_until_parked();
    }
    assert!(
        cx.update(|_, cx| cx.reduce_motion()),
        "appearance.reduce_motion = on"
    );
    assert!(motion::reduce_motion());
    settings
        .write_user(
            "appearance.reduce_motion",
            toml::Value::String("off".into()),
        )
        .unwrap();
    for _ in 0..20 {
        draw(cx);
        cx.run_until_parked();
    }
    assert!(!cx.update(|_, cx| cx.reduce_motion()));
}
