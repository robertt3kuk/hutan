//! The one-time hint at the Files icon of the right rail (FX-WORK-2, N-01).
//!
//! A fresh layout opens the right side on Files; in a window too narrow for that (or after the
//! user collapsed the panel) the right side is only its rail of glyphs, and a new user cannot
//! tell which one is the file tree. While `layout.files_hint` is on and the rail is all there
//! is of the right side, a small label sits beside the Files icon, over the center's edge: a
//! click on it opens Files (a peek where the center has no room), its × — or the first time
//! Files is shown any other way — turns the setting off.

use gpui::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Styled, div, px};

use super::model::{DockMode, DockPosition, PanelId};
use super::render::probe;
use super::ui::{Button, ButtonKind, IconButton};
use super::view::{Region, Workbench};
use crate::icons::Icon;
use crate::settings::LayoutSettings;
use crate::theme::DensityExt as _;

const FILES: &str = "files";

impl Workbench {
    /// The right dock lists Files and shows its body (beside the center or as the peek).
    fn files_body_shown(&self) -> bool {
        let dock = self.state.docks.get(DockPosition::Right);
        dock.active.as_ref().is_some_and(|a| a.as_str() == FILES)
            && self.dock_body_shown(DockPosition::Right)
    }

    /// Whether the hint is drawn: the setting is on, the right dock has a Files panel and shows
    /// only its rail.
    pub fn rail_hint_visible(&self) -> bool {
        self.settings.get::<LayoutSettings>().files_hint
            && self.state.docks.right.contains(&PanelId::new(FILES))
            && self.dock_mode(DockPosition::Right) == DockMode::Compact
            && !self.files_body_shown()
    }

    /// Files is shown after the hint was drawn (a body, a peek, a key): the hint has done its
    /// work. A window that opens on Files at once never drew it and writes nothing.
    pub(crate) fn retire_rail_hint(&mut self) {
        if self.rail_hint_seen.get() && self.files_body_shown() {
            self.end_rail_hint();
        }
    }

    /// Ends the hint for good (`layout.files_hint` off).
    pub(crate) fn end_rail_hint(&self) {
        self.rail_hint_seen.set(false);
        if self.settings.get::<LayoutSettings>().files_hint
            && let Err(error) = self
                .settings
                .write_user("layout.files_hint", toml::Value::Boolean(false))
        {
            tracing::warn!("cannot save layout.files_hint: {error}");
        }
    }

    /// The label beside the rail's Files icon, `top` points down the rail; `None` when it is not
    /// to be shown. Drawing it is what makes it count as seen.
    pub(crate) fn render_rail_hint(&self, top: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.rail_hint_visible() {
            return None;
        }
        self.rail_hint_seen.set(true);
        let theme = self.theme;
        let tokens = self.density.ui();
        Some(
            div()
                .id("rail-hint")
                .absolute()
                .top(px(top))
                .right(px(DockPosition::Right.rail_width() + tokens.spacing))
                .flex()
                .items_center()
                .gap(px(tokens.spacing))
                .pl(px(tokens.spacing * 3.0))
                .pr(px(tokens.spacing))
                .py(px(tokens.spacing))
                .rounded(px(tokens.radius + 2.0))
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border)
                .shadow_md()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.fg)
                .child(div().flex_none().child("Your project's files"))
                .child(
                    Button::new("rail-hint-open", "Show Files", &theme, &tokens)
                        .kind(ButtonKind::Secondary)
                        .compact()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.end_rail_hint();
                            this.click_rail_icon(
                                DockPosition::Right,
                                &PanelId::new(FILES),
                                window,
                                cx,
                            );
                        }))
                        .child(probe(&self.regions, Region::Offer("rail-hint-open"))),
                )
                .child(
                    IconButton::new(
                        "rail-hint-dismiss",
                        Icon::ActionClose,
                        "Dismiss",
                        &theme,
                        &tokens,
                    )
                    .compact()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.end_rail_hint();
                        cx.notify();
                    }))
                    .child(probe(&self.regions, Region::Offer("rail-hint-dismiss"))),
                )
                .child(probe(&self.regions, Region::Offer("rail-hint")))
                .into_any_element(),
        )
    }
}
