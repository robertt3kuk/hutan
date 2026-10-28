//! `UiStyle`: theme + density as a GPUI global so panels render without a back-pointer to the
//! workbench. The workbench is the only writer.

use crate::theme::{Density, DensityExt as _, Theme};
use gpui::{App, Global};

#[derive(Clone, Copy, Debug)]
pub struct UiStyle {
    pub theme: Theme,
    pub density: Density,
}

impl Global for UiStyle {}

impl UiStyle {
    /// Current style; falls back to dark/compact before the workbench installs one.
    pub fn get(cx: &App) -> UiStyle {
        cx.try_global::<UiStyle>().copied().unwrap_or(UiStyle {
            theme: Theme::dark(),
            density: Density::default(),
        })
    }
}

/// The terminal view configuration currently in effect (`terminal` settings with the theme
/// resolved); the workbench writes it, overview previews read it.
#[derive(Clone, Debug)]
pub struct TerminalStyle(pub hutan_terminal_view::TerminalViewSettings);

impl Global for TerminalStyle {}

impl TerminalStyle {
    pub fn get(cx: &App) -> hutan_terminal_view::TerminalViewSettings {
        cx.try_global::<TerminalStyle>()
            .map(|s| s.0.clone())
            .unwrap_or_default()
    }
}

/// Which tooltips may still be drawn (FX-WORK-2, N-07). A tooltip is a view GPUI keeps while the
/// pointer rests on its element, also after the wheel moved other content under the pointer and
/// after a sheet, menu or palette opened over it. Each tooltip remembers the epoch it was born in
/// and draws nothing once the epoch moved on: the workbench bumps it on every wheel notch and
/// when an overlay opens, so a tooltip born inside the overlay (its own buttons') is the only one
/// that can show over it.
#[derive(Debug, Default)]
pub struct TooltipGate {
    epoch: u64,
}

impl gpui::Global for TooltipGate {}

impl TooltipGate {
    /// Everything shown so far is stale.
    pub fn bump(cx: &mut gpui::App) {
        cx.default_global::<Self>().epoch += 1;
    }

    pub fn epoch(cx: &gpui::App) -> u64 {
        cx.try_global::<Self>().map_or(0, |gate| gate.epoch)
    }
}

/// An invisible element whose paint registers a window-level wheel observer for the frame: any
/// wheel notch makes the tooltips shown so far stale ([`TooltipGate`]). The workbench root
/// carries one.
pub fn tooltip_scroll_observer() -> impl gpui::IntoElement {
    use gpui::Styled as _;
    gpui::canvas(
        |_, _, _| {},
        |_, _, window, _| {
            window.on_mouse_event(|_: &gpui::ScrollWheelEvent, phase, _, cx| {
                if phase == gpui::DispatchPhase::Capture {
                    TooltipGate::bump(cx);
                }
            });
        },
    )
    .absolute()
    .size_0()
}

/// A tooltip of text: one line, or wrapped at 360 px when the text is longer than that.
#[derive(Debug)]
pub struct TextTooltip {
    text: gpui::SharedString,
    /// The [`TooltipGate`] epoch of its first frame.
    born: std::cell::Cell<Option<u64>>,
}

impl TextTooltip {
    pub fn new(text: gpui::SharedString) -> Self {
        Self {
            text,
            born: std::cell::Cell::new(None),
        }
    }
}

impl TextTooltip {
    /// Whether the wheel moved or an overlay opened since this tooltip's first look (the first
    /// call is that look): it draws nothing then.
    pub fn stale(&self, cx: &gpui::App) -> bool {
        let now = TooltipGate::epoch(cx);
        self.born.get().unwrap_or_else(|| {
            self.born.set(Some(now));
            now
        }) != now
    }
}

impl gpui::Render for TextTooltip {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{ParentElement as _, Styled as _, div, px};
        if self.stale(cx) {
            return div();
        }
        let style = UiStyle::get(cx);
        div()
            .max_w(px(360.0))
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(style.density.ui().radius - 2.0))
            .bg(style.theme.surface)
            .border_1()
            .border_color(style.theme.border)
            .text_color(style.theme.fg)
            .text_size(px(style.density.ui().font_size - 1.0))
            .child(self.text.clone())
    }
}
