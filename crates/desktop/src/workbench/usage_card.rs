//! The hover card of a usage ring (STATUS-METERS): what one harness's limits are — every running
//! window with its account, its share used and when it resets, then what the turns Hutan ran
//! used today and this week. A tooltip view like [`TextTooltip`](super::style::TextTooltip): it
//! draws nothing once the wheel moved or an overlay opened since its first look.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, Hsla, IntoElement, ParentElement, Render, SharedString, Styled, Window, div, px,
    relative,
};

use super::style::{TooltipBirth, UiStyle};
use super::ui::{self, StyledExt as _};
use crate::sessions::usage::{CardRow, CardView, Severity};
use crate::theme::{DensityExt as _, Theme};

/// What a click on the ring does, said once at the foot of the card.
pub const CARD_HINT: &str = "Click for the Agents page";

/// The colour of a share used: calm is the accent (the Agents page's bars), 80 % amber, 95 % red.
pub fn severity_color(theme: &Theme, severity: Severity) -> Hsla {
    match severity {
        Severity::Calm => theme.accent,
        Severity::Warning => theme.warning,
        Severity::Critical => theme.danger,
    }
}

/// The hover card of one meter.
#[derive(Debug)]
pub struct UsageCard {
    card: CardView,
    born: TooltipBirth,
}

impl UsageCard {
    pub fn new(card: CardView) -> Self {
        Self {
            card,
            born: TooltipBirth::default(),
        }
    }

    /// Every line of text the card draws, top to bottom: the harness, one line per window
    /// (`5-hour · Work  82%  resets in 3h`), the turns' totals and the click's hint.
    pub fn lines(card: &CardView) -> Vec<String> {
        let mut lines = vec![card.title.clone()];
        for row in &card.rows {
            let mut line = format!("{}  {}", row.name, row.limit.percent);
            if let Some(resets) = &row.limit.resets {
                line.push_str(&format!("  {resets}"));
            }
            lines.push(line);
        }
        lines.extend(card.today.clone());
        lines.extend(card.week.clone());
        lines.push(CARD_HINT.to_owned());
        lines
    }

    /// Whether the card would draw nothing now.
    pub fn stale(&self, cx: &App) -> bool {
        self.born.stale(cx)
    }
}

fn limit_row(row: &CardRow, theme: &Theme, size: f32) -> gpui::Div {
    let color = severity_color(theme, row.limit.severity);
    let percent_color = match row.limit.severity {
        Severity::Calm => theme.fg,
        _ => color,
    };
    div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .child(
            div()
                .flex()
                .items_baseline()
                .justify_between()
                .gap(px(12.0))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.fg)
                        .child(SharedString::from(row.name.clone())),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(percent_color)
                        .child(SharedString::from(row.limit.percent.clone())),
                ),
        )
        .child(
            div()
                .h(px(4.0))
                .w_full()
                .rounded_full()
                .bg(theme.border)
                .child(
                    div()
                        .h_full()
                        .rounded_full()
                        .w(relative(row.limit.fraction))
                        .bg(color),
                ),
        )
        .children(row.limit.resets.clone().map(|resets| {
            div()
                .text_size(px(size - 1.0))
                .text_color(theme.muted)
                .child(SharedString::from(resets))
        }))
}

impl Render for UsageCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.stale(cx) {
            return div();
        }
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let size = tokens.secondary_font_size;
        let rows = self
            .card
            .rows
            .iter()
            .map(|row| limit_row(row, &theme, size));
        let totals: Vec<SharedString> = self
            .card
            .today
            .iter()
            .chain(&self.card.week)
            .map(|total| SharedString::from(total.clone()))
            .collect();
        div()
            .w(px(260.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .popover(&theme, &tokens)
            .light_edge(&theme)
            .p(px(10.0))
            .text_size(px(size))
            .text_color(theme.fg)
            .child(ui::group_label(&self.card.title, &theme, &tokens))
            .children(rows)
            .when(!totals.is_empty(), |card| {
                card.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .pt(px(6.0))
                        .border_t_1()
                        .border_color(theme.border)
                        .text_color(theme.muted)
                        .children(totals),
                )
            })
            .child(div().text_color(theme.muted).child(CARD_HINT))
    }
}
