//! *Layout: Use New Default* (CL-CHROME, absorbing PR-CLEAN-DEFAULT): a new installation starts
//! with the clean default layout (Changes, Agents and History on the right rail, Requests and
//! Resources under its `⋯`), and a layout saved before it existed is never rewritten. Such a
//! layout gets one line under the title bar, once: *Use new default* takes the default preset's
//! panels and keeps the user's dock modes, sizes and center; *Keep mine* leaves it. Either answer
//! turns `layout.offer_new_default` off. View → Use New Default Layout and the palette's
//! *Layout: Use New Default* do the same at any time.

use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, div, prelude::*, px};

use super::model::LayoutPreset;
use super::render::probe;
use super::ui::{self, ButtonKind};
use super::view::{Region, Workbench};
use crate::icons::{Icon, glyph};
use crate::theme::DensityExt as _;

impl Workbench {
    /// Whether the one-time offer of the new default layout is on screen.
    pub fn layout_offer_shown(&self) -> bool {
        self.layout_offer
    }

    /// *Use new default*: the default preset's panels in every dock, the user's modes, sizes and
    /// center kept; the offer is answered.
    pub fn use_new_default_layout(&mut self, cx: &mut Context<Self>) {
        let preset = self
            .presets
            .iter()
            .find(|p| p.name == LayoutPreset::DEFAULT)
            .cloned()
            .unwrap_or_else(LayoutPreset::default_preset);
        self.mutate(cx, |s| s.use_new_default(&preset));
        self.answer_layout_offer(cx);
    }

    /// *Keep mine*: nothing changes and the offer is not made again.
    pub fn keep_layout(&mut self, cx: &mut Context<Self>) {
        self.answer_layout_offer(cx);
    }

    fn answer_layout_offer(&mut self, cx: &mut Context<Self>) {
        let was_offered = std::mem::replace(&mut self.layout_offer, false);
        if was_offered
            && let Err(error) = self
                .settings
                .write_user("layout.offer_new_default", toml::Value::Boolean(false))
        {
            tracing::warn!("cannot save layout.offer_new_default: {error}");
        }
        cx.notify();
    }

    /// The offer's line, under the title bar while it is open.
    pub(crate) fn render_layout_offer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.layout_offer {
            return None;
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        let button = |id: &'static str, label: &'static str, kind: ButtonKind| {
            ui::button(id, label, kind, true, &theme, &tokens)
                .relative()
                .child(probe(&self.regions, Region::Offer(id)))
        };
        Some(
            div()
                .id("layout-offer")
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 4.0))
                .py(px(tokens.spacing))
                .bg(theme.surface)
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(tokens.secondary_font_size))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.muted)
                        .child(glyph(Icon::StateInfo)),
                )
                .child(div().flex_1().min_w_0().text_color(theme.fg).child(
                    "A quieter default layout is available: Changes, Agents and History on the \
                     right rail, Requests and Resources under ⋯. Your layout stays as it is \
                     unless you choose it.",
                ))
                .child(
                    button("layout-use-new", "Use new default", ButtonKind::Secondary)
                        .on_click(cx.listener(|this, _, _, cx| this.use_new_default_layout(cx))),
                )
                .child(
                    button("layout-keep", "Keep mine", ButtonKind::Ghost)
                        .on_click(cx.listener(|this, _, _, cx| this.keep_layout(cx))),
                )
                .child(probe(&self.regions, Region::Offer("layout")))
                .into_any_element(),
        )
    }
}
