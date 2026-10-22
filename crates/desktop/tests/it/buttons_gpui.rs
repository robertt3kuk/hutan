//! The button system (UX-BUTTONS): a disabled or loading button is not clickable, a loading one
//! keeps its width, Return and Space click a focused one, an icon button clicks like a button.
//! No daemon: a bare window with the buttons.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Context, FocusHandle, InteractiveElement as _, IntoElement, Modifiers, ParentElement as _,
    Render, Styled as _, TestAppContext, Window, div, px,
};
use hutan_desktop::icons::Icon;
use hutan_desktop::theme::{Density, DensityExt as _, Theme};
use hutan_desktop::workbench::ui::{Button, ButtonKind, IconButton};

/// Rows are 40 px tall, so the button of row `i` is centered at `y = 40 i + 20`.
const ROW: f32 = 40.0;

struct Probe {
    clicks: Rc<[Cell<u32>; 6]>,
    loading: bool,
    focus: FocusHandle,
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let tokens = Density::Compact.ui();
        let counter = |i: usize| {
            let clicks = self.clicks.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, _: &mut gpui::App| {
                clicks[i].set(clicks[i].get() + 1);
            }
        };
        let row = || div().h(px(ROW)).w(px(300.0)).flex().items_center();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                row().child(
                    Button::new("enabled", "Save changes", &theme, &tokens)
                        .primary()
                        .on_click(counter(0)),
                ),
            )
            .child(
                row().child(
                    Button::new("disabled", "Save changes", &theme, &tokens)
                        .disabled(true)
                        .on_click(counter(1)),
                ),
            )
            .child(
                row().child(
                    div().debug_selector(|| "loading".into()).child(
                        Button::new("loading", "Save changes", &theme, &tokens)
                            .loading(self.loading)
                            .on_click(counter(2)),
                    ),
                ),
            )
            .child(
                row().child(
                    IconButton::new("icon", Icon::ActionForward, "Send", &theme, &tokens)
                        .on_click(counter(3)),
                ),
            )
            .child(
                row().child(
                    IconButton::new(
                        "icon-disabled",
                        Icon::ActionForward,
                        "Send",
                        &theme,
                        &tokens,
                    )
                    .disabled(true)
                    .on_click(counter(4)),
                ),
            )
            .child(
                row().child(
                    Button::new("focusable", "Open", &theme, &tokens)
                        .kind(ButtonKind::Ghost)
                        .focus_handle(&self.focus)
                        .on_click(counter(5)),
                ),
            )
            .child(
                row().child(
                    div().debug_selector(|| "loading-icon".into()).child(
                        Button::new("loading-icon", "Save changes", &theme, &tokens)
                            .icon(Icon::ActionForward)
                            .loading(self.loading),
                    ),
                ),
            )
    }
}

fn open(
    cx: &mut TestAppContext,
    loading: bool,
) -> (
    gpui::Entity<Probe>,
    Rc<[Cell<u32>; 6]>,
    &mut gpui::VisualTestContext,
) {
    let clicks: Rc<[Cell<u32>; 6]> = Rc::new(Default::default());
    let shared = clicks.clone();
    let (view, cx) = cx.add_window_view(move |_, cx| Probe {
        clicks: shared,
        loading,
        focus: cx.focus_handle(),
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    (view, clicks, cx)
}

fn click_row(cx: &mut gpui::VisualTestContext, row: usize) {
    cx.simulate_click(
        gpui::point(px(20.0), px(ROW * row as f32 + ROW / 2.0)),
        Modifiers::none(),
    );
}

#[gpui::test]
fn an_enabled_button_clicks_and_a_disabled_or_loading_one_does_not(cx: &mut TestAppContext) {
    let (_view, clicks, cx) = open(cx, true);
    click_row(cx, 0);
    click_row(cx, 1);
    click_row(cx, 2);
    click_row(cx, 3);
    click_row(cx, 4);
    let got: Vec<u32> = clicks.iter().map(Cell::get).collect();
    assert_eq!(
        got,
        [1, 0, 0, 1, 0, 0],
        "enabled clicks; disabled and loading do not"
    );
}

#[gpui::test]
fn the_loading_button_clicks_again_once_it_stops_loading(cx: &mut TestAppContext) {
    let (view, clicks, cx) = open(cx, true);
    click_row(cx, 2);
    assert_eq!(clicks[2].get(), 0);
    view.update(cx, |probe, cx| {
        probe.loading = false;
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    click_row(cx, 2);
    assert_eq!(clicks[2].get(), 1);
}

#[gpui::test]
fn return_and_space_click_the_focused_button(cx: &mut TestAppContext) {
    let (view, clicks, cx) = open(cx, false);
    let focus = view.read_with(cx, |probe, _| probe.focus.clone());
    cx.update(|window, cx| {
        window.focus(&focus, cx);
        window.draw(cx).clear(cx);
    });
    // The test platform delivers only the key down; GPUI clicks on the matching key up.
    let press = |cx: &mut gpui::VisualTestContext, key: &str| {
        cx.simulate_keystrokes(key);
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
        });
    };
    press(cx, "enter");
    assert_eq!(clicks[5].get(), 1, "Return clicks the focused button");
    press(cx, "space");
    assert_eq!(clicks[5].get(), 2, "Space clicks it too");
}

#[gpui::test]
fn a_loading_button_keeps_its_width(cx: &mut TestAppContext) {
    let (view, _clicks, cx) = open(cx, false);
    let widths = |cx: &mut gpui::VisualTestContext| {
        ["loading", "loading-icon"].map(|name| cx.debug_bounds(name).unwrap().size.width)
    };
    let idle = widths(cx);
    view.update(cx, |probe, cx| {
        probe.loading = true;
        cx.notify();
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(
        widths(cx),
        idle,
        "the spinner takes the icon's place, or the padding"
    );
}
