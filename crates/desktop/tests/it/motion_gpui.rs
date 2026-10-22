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

/// A button that fades its fill: hover moves the transition, which stops asking for frames once
/// it arrived.
struct Fader;

impl Render for Fader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::InteractiveElement as _;
        let (rest, hover) = (
            gpui::hsla(0.0, 0.0, 0.1, 1.0),
            gpui::hsla(0.0, 0.0, 0.3, 1.0),
        );
        div().child(motion::hover_fade(
            "fader",
            div().id("fader-button").w(px(100.0)).h(px(40.0)),
            rest,
            hover,
            None,
            window,
            cx,
        ))
    }
}

#[gpui::test]
fn a_hover_fade_runs_for_100_ms_after_the_pointer_moves_and_then_rests(cx: &mut TestAppContext) {
    set_reduce(cx, false);
    let (_view, cx) = cx.add_window_view(|_, _| Fader);
    // The test window's pointer starts on the button: let the first fade out run.
    frame(cx);
    advance(cx, Duration::from_millis(150));
    frame(cx);
    frame(cx);
    assert_eq!(frame(cx), 0, "idle at rest");
    cx.simulate_mouse_move(
        gpui::point(px(500.0), px(300.0)),
        None,
        gpui::Modifiers::none(),
    );
    frame(cx);
    assert!(frame(cx) >= 1, "fading out after the pointer left");
    advance(cx, Duration::from_millis(100));
    frame(cx);
    frame(cx);
    assert_eq!(frame(cx), 0, "arrived: no more frames");
    cx.simulate_mouse_move(
        gpui::point(px(50.0), px(20.0)),
        None,
        gpui::Modifiers::none(),
    );
    frame(cx);
    assert!(frame(cx) >= 1, "fading in after the pointer arrived");
    advance(cx, Duration::from_millis(100));
    frame(cx);
    frame(cx);
    assert_eq!(frame(cx), 0, "arrived: no more frames");
}

// ---- the places that move, in a workbench -----------------------------------------------------

mod places {
    use super::{advance, frame};
    use crate::common::{draw, offline_connection, open_with};
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use hutan_desktop::workbench::{Region, Workbench, actions};
    use std::time::Duration;

    fn open_workbench<'a>(
        cx: &'a mut TestAppContext,
        reduce_motion: &str,
    ) -> (Entity<Workbench>, &'a mut VisualTestContext) {
        let (connection, settings, _) = offline_connection();
        settings
            .write_user(
                "appearance.reduce_motion",
                toml::Value::String(reduce_motion.into()),
            )
            .unwrap();
        let (view, cx) = open_with(cx, settings, connection, None);
        draw(cx);
        (view, cx)
    }

    fn footer_top(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> f32 {
        draw(cx);
        let bounds = view
            .read_with(cx, |wb, _| {
                wb.region_bounds(Region::Settings("sheet-footer"))
            })
            .expect("the sheet is drawn");
        f32::from(bounds.origin.y)
    }

    #[gpui::test]
    fn the_settings_sheet_rises_8_px_into_place_in_200_ms(cx: &mut TestAppContext) {
        let (view, cx) = open_workbench(cx, "off");
        advance(cx, Duration::from_millis(300));
        cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
        let start = footer_top(&view, cx);
        advance(cx, Duration::from_millis(100));
        let middle = footer_top(&view, cx);
        advance(cx, Duration::from_millis(150));
        let end = footer_top(&view, cx);
        assert!(
            (start - end - 8.0).abs() < 0.5,
            "starts 8 px low: {start} vs {end}"
        );
        assert!(
            middle < start && middle > end,
            "on its way at 100 ms: {middle}"
        );
        advance(cx, Duration::from_millis(300));
        assert_eq!(footer_top(&view, cx), end, "and stays");
    }

    #[gpui::test]
    fn the_settings_sheet_does_not_move_under_reduce_motion(cx: &mut TestAppContext) {
        let (view, cx) = open_workbench(cx, "on");
        cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
        let start = footer_top(&view, cx);
        advance(cx, Duration::from_millis(40));
        let later = footer_top(&view, cx);
        advance(cx, Duration::from_millis(300));
        let end = footer_top(&view, cx);
        assert_eq!((start, later), (end, end), "no movement at any frame");
    }

    #[gpui::test]
    fn motion_adds_frames_only_while_something_moves(cx: &mut TestAppContext) {
        let (view, cx) = open_workbench(cx, "off");
        advance(cx, Duration::from_millis(300));
        frame(cx);
        frame(cx);
        // What the idle workbench asks for on its own (a window of terminals and panels has
        // callbacks of its own): the baseline motion must not add to once it is over.
        let idle = frame(cx);
        assert_eq!(frame(cx), idle, "the baseline is steady");

        view.update_in(cx, |wb, window, cx| wb.show_daemon_status(window, cx));
        frame(cx);
        assert!(frame(cx) > idle, "entering: a frame is requested per frame");
        advance(cx, Duration::from_millis(160));
        frame(cx);
        frame(cx);
        assert_eq!(frame(cx), idle, "arrived: back to the baseline");

        view.update_in(cx, |wb, window, cx| wb.close_daemon_popover(window, cx));
        frame(cx);
        assert!(frame(cx) > idle, "leaving: still drawn and fading");
        advance(cx, Duration::from_millis(110));
        cx.run_until_parked();
        frame(cx);
        frame(cx);
        assert_eq!(frame(cx), idle, "gone: back to the baseline");
    }

    #[gpui::test]
    fn typing_never_waits_for_an_animation(cx: &mut TestAppContext) {
        let (view, cx) = open_workbench(cx, "off");
        cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
        draw(cx);
        // The sheet is at its first frame: 200 ms of motion are ahead, and no clock has moved.
        let before = footer_top(&view, cx);
        cx.simulate_input("dens");
        assert_eq!(
            view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().query().to_owned()),
            "dens",
            "every keystroke lands in the field at once"
        );
        // The keystrokes were a keyboard input: nothing slid while they landed.
        assert!(footer_top(&view, cx) <= before);
    }
}
