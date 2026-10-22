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

/// `ui::Button` and `ui::IconButton` fade their fill like [`Fader`] (UX-BUTTONS): the pointer
/// moves the transition, which stops asking for frames once it arrived.
struct ButtonFader;

impl Render for ButtonFader {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        use hutan_desktop::theme::DensityExt as _;
        let theme = hutan_desktop::theme::Theme::dark();
        let tokens = hutan_desktop::theme::Density::Compact.ui();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div().h(px(40.0)).child(
                    hutan_desktop::workbench::ui::Button::new(
                        "fade-button",
                        "Save",
                        &theme,
                        &tokens,
                    )
                    .on_click(|_, _, _| {}),
                ),
            )
            .child(
                div().h(px(40.0)).child(
                    hutan_desktop::workbench::ui::IconButton::new(
                        "fade-icon",
                        hutan_desktop::icons::Icon::ActionClose,
                        "Close",
                        &theme,
                        &tokens,
                    )
                    .on_click(|_, _, _| {}),
                ),
            )
    }
}

/// Moves the pointer off the buttons, lets the fade out finish, moves it onto the one at `y` and
/// checks the fade in: more frames than at rest while it runs, the frames of rest once it
/// arrived within 100 ms. The icon button's glyph holds one frame callback per paint (the
/// raster cache keeps its image until the next frame), so rest is measured, not assumed zero.
fn fade_cycle(cx: &mut TestAppContext, reduced: bool, y: f32) {
    set_reduce(cx, reduced);
    let (_view, cx) = cx.add_window_view(|_, _| ButtonFader);
    frame(cx);
    cx.simulate_mouse_move(
        gpui::point(px(500.0), px(300.0)),
        None,
        gpui::Modifiers::none(),
    );
    frame(cx);
    advance(cx, Duration::from_millis(150));
    frame(cx);
    frame(cx);
    frame(cx);
    let rest = frame(cx);
    assert_eq!(frame(cx), rest, "at rest the frames do not change");
    cx.simulate_mouse_move(gpui::point(px(10.0), px(y)), None, gpui::Modifiers::none());
    frame(cx);
    assert!(frame(cx) > rest, "the fill is fading in");
    advance(cx, Duration::from_millis(100));
    frame(cx);
    frame(cx);
    assert_eq!(frame(cx), rest, "arrived within 100 ms");
}

#[gpui::test]
fn a_button_fades_over_100_ms_and_then_rests(cx: &mut TestAppContext) {
    fade_cycle(cx, false, 20.0);
}

#[gpui::test]
fn an_icon_button_fades_over_100_ms_and_then_rests(cx: &mut TestAppContext) {
    fade_cycle(cx, false, 60.0);
}

#[gpui::test]
fn a_button_fade_is_still_a_fade_of_at_most_100_ms_under_reduce_motion(cx: &mut TestAppContext) {
    fade_cycle(cx, true, 20.0);
}

#[gpui::test]
fn an_icon_button_fade_is_still_a_fade_of_at_most_100_ms_under_reduce_motion(
    cx: &mut TestAppContext,
) {
    fade_cycle(cx, true, 60.0);
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

// ---- overlays that leave (UX-MOTION-2) ---------------------------------------------------------

/// An overlay whose state is dropped at its close, drawn as a real one is: the state goes into a
/// [`motion::Leaving`], the render lends it back while it leaves, and the surface is inert then.
struct Overlay {
    kind: Kind,
    open: Option<u32>,
    exit: motion::Leaving<u32>,
    seen: Rc<Seen>,
    clicks: Rc<Cell<usize>>,
}

impl Overlay {
    fn build(&mut self, wanted: bool) -> gpui::AnyElement {
        use gpui::{InteractiveElement as _, StatefulInteractiveElement as _};
        let (seen, clicks) = (Rc::clone(&self.seen), Rc::clone(&self.clicks));
        let body = div()
            .id("body")
            .w(px(100.0))
            .h(px(40.0))
            .on_click(move |_, _, _| clicks.set(clicks.get() + 1));
        let animate = move |element: gpui::Stateful<gpui::Div>, frame: motion::Frame| {
            seen.value.set(frame.value);
            seen.shift.set(frame.shift);
            seen.settled.set(frame.settled);
            seen.draws.set(seen.draws.get() + 1);
            frame.slide_fade(element)
        };
        motion::transition(
            "overlay",
            self.kind,
            if wanted { 1.0 } else { 0.0 },
            body,
            animate,
        )
        .from(0.0)
        .inert(!wanted)
        .into_any_element()
    }
}

impl Render for Overlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let element = if self.open.is_some() {
            self.exit.cancel();
            Some(self.build(true))
        } else {
            let now = cx.background_executor().now();
            motion::while_leaving(
                self,
                now,
                |this| &mut this.open,
                |this| &mut this.exit,
                |this| this.build(false),
            )
        };
        div().children(element)
    }
}

struct Opened<'a> {
    view: Entity<Overlay>,
    seen: Rc<Seen>,
    clicks: Rc<Cell<usize>>,
    cx: &'a mut VisualTestContext,
}

/// An overlay of `kind`, open and settled, with the last input a pointer press.
fn opened(cx: &mut TestAppContext, kind: Kind, reduced: bool) -> Opened<'_> {
    set_reduce(cx, reduced);
    motion::note_input(false);
    let seen = Rc::new(Seen::default());
    let clicks = Rc::new(Cell::new(0));
    let (view_seen, view_clicks) = (Rc::clone(&seen), Rc::clone(&clicks));
    let (view, cx) = cx.add_window_view(move |_, _| Overlay {
        kind,
        open: Some(1),
        exit: motion::Leaving::default(),
        seen: view_seen,
        clicks: view_clicks,
    });
    frame(cx);
    advance(cx, Duration::from_millis(300));
    frame(cx);
    Opened {
        view,
        seen,
        clicks,
        cx,
    }
}

impl Opened<'_> {
    fn close(&mut self) {
        self.view.update(self.cx, |overlay, cx| {
            let state = overlay.open.take().expect("open");
            overlay.exit.start(state, overlay.kind, cx);
            cx.notify();
        });
    }

    fn reopen(&mut self) {
        self.view.update(self.cx, |overlay, cx| {
            overlay.open = Some(2);
            cx.notify();
        });
    }

    /// Moves the clock by `by`, lets the exit's timer run and draws; returns the draws so far.
    fn step(&mut self, by: Duration) -> usize {
        advance(self.cx, by);
        self.cx.run_until_parked();
        frame(self.cx);
        self.seen.draws.get()
    }
}

#[gpui::test]
fn an_overlay_that_closed_is_drawn_for_its_exit_at_its_token_and_then_not(cx: &mut TestAppContext) {
    for (kind, exit_ms) in [(Kind::Popover, 100), (Kind::Sheet, 150)] {
        let mut o = opened(cx, kind, false);
        o.close();
        let at_close = o.step(Duration::ZERO);
        assert_eq!(
            o.seen.value.get(),
            1.0,
            "{kind:?}: leaves from where it was"
        );
        let half = o.step(Duration::from_millis(exit_ms / 2));
        assert!(half > at_close, "{kind:?}: drawn at half time");
        assert!(
            o.seen.value.get() < 0.5 && o.seen.value.get() > 0.0,
            "{kind:?}: ease-out is past half at half time: {}",
            o.seen.value.get()
        );
        let before_end = o.step(Duration::from_millis(exit_ms / 2 - 1));
        assert!(
            before_end > half,
            "{kind:?}: still drawn 1 ms before its token"
        );
        let end = o.step(Duration::from_millis(1));
        assert_eq!(
            end, before_end,
            "{kind:?}: not drawn once {exit_ms} ms passed"
        );
    }
}

#[gpui::test]
fn reopening_an_overlay_mid_exit_continues_from_the_value_on_screen(cx: &mut TestAppContext) {
    let mut o = opened(cx, Kind::Sheet, false);
    o.close();
    o.step(Duration::ZERO);
    o.step(Duration::from_millis(60));
    let leaving = o.seen.value.get();
    assert!(leaving < 1.0 && leaving > 0.0, "on its way out: {leaving}");
    o.reopen();
    o.step(Duration::ZERO);
    assert!(
        (o.seen.value.get() - leaving).abs() < 1e-4,
        "no jump when it comes back: {} vs {leaving}",
        o.seen.value.get()
    );
    o.step(Duration::from_millis(20));
    assert!(o.seen.value.get() > leaving, "heads back to shown");
    o.step(Duration::from_millis(300));
    assert_eq!(o.seen.value.get(), 1.0);
    assert!(o.seen.settled.get());
}

#[gpui::test]
fn a_leaving_overlay_takes_no_clicks_and_an_open_one_does(cx: &mut TestAppContext) {
    let mut o = opened(cx, Kind::Sheet, false);
    let spot = gpui::point(px(10.0), px(10.0));
    o.cx.simulate_click(spot, gpui::Modifiers::none());
    assert_eq!(o.clicks.get(), 1, "open: the click lands");
    o.close();
    o.step(Duration::from_millis(30));
    o.cx.simulate_click(spot, gpui::Modifiers::none());
    assert_eq!(
        o.clicks.get(),
        1,
        "leaving: the click goes to what is under it"
    );
}

#[gpui::test]
fn a_close_from_the_keyboard_leaves_nothing_behind(cx: &mut TestAppContext) {
    let mut o = opened(cx, Kind::Sheet, false);
    let drawn = o.seen.draws.get();
    motion::note_input(true);
    o.close();
    assert_eq!(o.step(Duration::ZERO), drawn, "gone at the very next frame");
    motion::note_input(false);
}

#[gpui::test]
fn an_overlay_leaves_without_moving_under_reduce_motion(cx: &mut TestAppContext) {
    let mut o = opened(cx, Kind::Sheet, true);
    o.close();
    let mut last = 1.0;
    for _ in 0..4 {
        o.step(Duration::from_millis(30));
        assert_eq!(o.seen.shift.get(), 0.0, "never moves");
        assert!(o.seen.value.get() <= last, "only fades");
        last = o.seen.value.get();
    }
    let before_end = o.seen.draws.get();
    o.step(Duration::from_millis(30));
    assert_eq!(
        o.seen.draws.get(),
        before_end,
        "a fade of at most 150 ms, then gone"
    );
}

#[gpui::test]
fn a_settled_exit_adds_no_frames(cx: &mut TestAppContext) {
    let mut o = opened(cx, Kind::Popover, false);
    let idle = frame(o.cx);
    o.close();
    o.step(Duration::ZERO);
    assert!(
        frame(o.cx) > idle,
        "leaving: a frame is requested per frame"
    );
    o.step(Duration::from_millis(110));
    frame(o.cx);
    assert_eq!(frame(o.cx), idle, "gone: back to the baseline");
}

mod in_the_workbench {
    use super::{advance, frame};
    use crate::common::{draw, offline_connection, open_with};
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use hutan_desktop::workbench::ui::motion;
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
        motion::note_input(false);
        advance(cx, Duration::from_millis(300));
        draw(cx);
        (view, cx)
    }

    fn leaving(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<&'static str> {
        let now = cx.executor().now();
        view.read_with(cx, |wb, _| wb.overlays_leaving(now))
    }

    fn step(cx: &mut VisualTestContext, by: Duration) {
        advance(cx, by);
        cx.run_until_parked();
        draw(cx);
    }

    fn focus_id(cx: &mut VisualTestContext) -> Option<gpui::FocusHandle> {
        cx.update(|window, cx| window.focused(cx))
    }

    #[gpui::test]
    fn the_palette_fades_out_in_100_ms_and_the_keyboard_is_back_at_once(cx: &mut TestAppContext) {
        let (view, cx) = open_workbench(cx, "off");
        view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
        step(cx, Duration::from_millis(300));
        let in_palette = focus_id(cx);
        assert!(leaving(&view, cx).is_empty());

        view.update_in(cx, |wb, window, cx| wb.close_palette(window, cx));
        draw(cx);
        assert!(!view.read_with(cx, |wb, _| wb.palette_open()));
        assert_eq!(leaving(&view, cx), ["palette"], "still on screen, fading");
        assert_ne!(
            focus_id(cx),
            in_palette,
            "the keyboard left the palette when it closed"
        );
        step(cx, Duration::from_millis(99));
        assert_eq!(
            leaving(&view, cx),
            ["palette"],
            "100 ms: not yet gone at 99"
        );
        step(cx, Duration::from_millis(1));
        assert!(leaving(&view, cx).is_empty(), "gone at 100 ms");
    }

    #[gpui::test]
    fn the_palette_closed_from_the_keyboard_is_gone_at_once(cx: &mut TestAppContext) {
        let (view, cx) = open_workbench(cx, "off");
        view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
        step(cx, Duration::from_millis(300));
        motion::note_input(true);
        view.update_in(cx, |wb, window, cx| wb.close_palette(window, cx));
        draw(cx);
        assert!(leaving(&view, cx).is_empty());
        motion::note_input(false);
    }

    #[gpui::test]
    fn the_jump_picker_leaves_like_the_palette(cx: &mut TestAppContext) {
        let (view, cx) = open_workbench(cx, "off");
        view.update_in(cx, |wb, window, cx| wb.open_jump_palette(window, cx));
        step(cx, Duration::from_millis(300));
        view.update_in(cx, |wb, window, cx| wb.close_picker(window, cx));
        draw(cx);
        assert_eq!(leaving(&view, cx), ["picker"]);
        step(cx, Duration::from_millis(100));
        assert!(leaving(&view, cx).is_empty());
    }

    fn footer_top(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Option<f32> {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Settings("sheet-footer"))
        })
        .map(|bounds| f32::from(bounds.origin.y))
    }

    #[gpui::test]
    fn the_settings_sheet_sinks_8_px_and_is_gone_in_150_ms(cx: &mut TestAppContext) {
        let (view, cx) = open_workbench(cx, "off");
        cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
        step(cx, Duration::from_millis(300));
        let rest = footer_top(&view, cx).expect("drawn");
        let in_sheet = focus_id(cx);

        view.update_in(cx, |wb, window, cx| wb.close_settings(window, cx));
        let first = footer_top(&view, cx).expect("still drawn while it leaves");
        assert_ne!(focus_id(cx), in_sheet, "the keyboard is back at the close");
        assert!(
            (first - rest).abs() < 0.5,
            "starts where it was: {first} vs {rest}"
        );
        step(cx, Duration::from_millis(75));
        let middle = footer_top(&view, cx).expect("drawn at 75 ms");
        assert!(middle > rest && middle < rest + 8.0, "sinking: {middle}");
        step(cx, Duration::from_millis(74));
        assert!(footer_top(&view, cx).is_some(), "drawn at 149 ms");
        step(cx, Duration::from_millis(1));
        assert_eq!(footer_top(&view, cx), None, "gone at 150 ms");
        assert!(leaving(&view, cx).is_empty());
    }

    #[gpui::test]
    fn reopening_settings_mid_exit_does_not_jump(cx: &mut TestAppContext) {
        let (view, cx) = open_workbench(cx, "off");
        cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
        step(cx, Duration::from_millis(300));
        view.update_in(cx, |wb, window, cx| wb.close_settings(window, cx));
        draw(cx);
        step(cx, Duration::from_millis(75));
        let leaving_at = footer_top(&view, cx).expect("drawn");
        cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
        let back = footer_top(&view, cx).expect("drawn");
        assert!(
            (back - leaving_at).abs() < 1.0,
            "continues from where it was: {back} vs {leaving_at}"
        );
        step(cx, Duration::from_millis(300));
        let rest = footer_top(&view, cx).expect("drawn");
        assert!(rest < back, "rises into place");
    }

    #[gpui::test]
    fn the_settings_sheet_does_not_move_while_it_leaves_under_reduce_motion(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = open_workbench(cx, "on");
        cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
        step(cx, Duration::from_millis(300));
        let rest = footer_top(&view, cx).expect("drawn");
        view.update_in(cx, |wb, window, cx| wb.close_settings(window, cx));
        for _ in 0..5 {
            if let Some(top) = footer_top(&view, cx) {
                assert_eq!(top, rest, "no movement at any frame");
            }
            step(cx, Duration::from_millis(30));
        }
        assert_eq!(
            footer_top(&view, cx),
            None,
            "a fade of at most 150 ms, then gone"
        );
    }

    #[gpui::test]
    fn a_workbench_whose_overlays_left_is_idle_again(cx: &mut TestAppContext) {
        let (view, cx) = open_workbench(cx, "off");
        frame(cx);
        frame(cx);
        let idle = frame(cx);
        view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
        step(cx, Duration::from_millis(300));
        view.update_in(cx, |wb, window, cx| wb.close_palette(window, cx));
        draw(cx);
        assert!(frame(cx) > idle, "leaving: frames are requested");
        step(cx, Duration::from_millis(110));
        frame(cx);
        frame(cx);
        assert_eq!(frame(cx), idle, "gone: back to the baseline");
    }
}

mod folds {
    use super::advance;
    use crate::common::{draw, offline_connection, open_with};
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use hutan_desktop::workbench::ui::motion;
    use hutan_desktop::workbench::{Region, Workbench, actions};
    use std::time::Duration;

    fn advanced_height(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> f32 {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Settings("nav-advanced"))
        })
        .map_or(0.0, |bounds| f32::from(bounds.size.height))
    }

    fn open_settings<'a>(
        cx: &'a mut TestAppContext,
        reduced: bool,
    ) -> (Entity<Workbench>, &'a mut VisualTestContext) {
        let (connection, settings, _) = offline_connection();
        settings
            .write_user(
                "appearance.reduce_motion",
                toml::Value::String(if reduced { "on" } else { "off" }.into()),
            )
            .unwrap();
        let (view, cx) = open_with(cx, settings, connection, None);
        motion::note_input(false);
        draw(cx);
        advance(cx, Duration::from_millis(300));
        cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
        draw(cx);
        advance(cx, Duration::from_millis(300));
        draw(cx);
        (view, cx)
    }

    #[gpui::test]
    fn the_advanced_group_unfolds_and_folds_in_150_ms(cx: &mut TestAppContext) {
        let (view, cx) = open_settings(cx, false);
        assert_eq!(advanced_height(&view, cx), 0.0, "folded");
        view.update(cx, |wb, cx| wb.sheet_toggle_advanced(cx));
        // The first frame measures the content; the clock starts the frame after.
        advanced_height(&view, cx);
        advance(cx, Duration::from_millis(75));
        let middle = advanced_height(&view, cx);
        advance(cx, Duration::from_millis(80));
        let open = advanced_height(&view, cx);
        assert!(
            middle > 0.0 && middle < open,
            "on its way at 75 ms: {middle} of {open}"
        );
        advance(cx, Duration::from_millis(300));
        assert_eq!(advanced_height(&view, cx), open, "and stays");

        view.update(cx, |wb, cx| wb.sheet_toggle_advanced(cx));
        advanced_height(&view, cx);
        advance(cx, Duration::from_millis(75));
        let closing = advanced_height(&view, cx);
        assert!(
            closing > 0.0 && closing < open,
            "folding at 75 ms: {closing}"
        );
        advance(cx, Duration::from_millis(80));
        assert_eq!(advanced_height(&view, cx), 0.0, "folded again at 150 ms");
    }

    #[gpui::test]
    fn the_advanced_group_does_not_animate_under_reduce_motion(cx: &mut TestAppContext) {
        let (view, cx) = open_settings(cx, true);
        assert_eq!(advanced_height(&view, cx), 0.0);
        view.update(cx, |wb, cx| wb.sheet_toggle_advanced(cx));
        let open = advanced_height(&view, cx);
        assert!(open > 0.0, "unfolded at once");
        view.update(cx, |wb, cx| wb.sheet_toggle_advanced(cx));
        assert_eq!(advanced_height(&view, cx), 0.0, "folded at once");
    }
}
