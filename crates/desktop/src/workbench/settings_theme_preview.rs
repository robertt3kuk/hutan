//! The samples of the theme pickers' preview (TH-3): chrome, state cards, a diff and a terminal
//! ANSI row drawn from one color pack's own values, and a grid of icons drawn from one icon
//! pack. Every function takes the values to draw with; none reads `UiStyle`, the installed
//! icons or any other global, which is what keeps a preview from touching the rest of the app.

use std::collections::BTreeSet;

use gpui::{Div, FontWeight, Hsla, ParentElement, SharedString, Styled, div, prelude::*, px};
use hutan_settings::themes::TerminalTheme;
use hutan_settings::themes::icons::{IconId, ResolvedIcons};

use crate::theme::{Theme, Tokens, rgb_color};

const MONO: &str = "Menlo";

/// The window of the pack in miniature: sidebar with a selected and a hovered row, the center
/// with a card, a primary button, a quiet button and a link.
pub(super) fn chrome(theme: &Theme, tokens: &Tokens) -> Div {
    let nav = |label: &'static str, fill: Option<Hsla>, color: Hsla| {
        div()
            .px(px(tokens.spacing * 2.0))
            .h(px(tokens.sidebar_row - 6.0))
            .flex()
            .items_center()
            .rounded(px(tokens.radius))
            .text_size(px(tokens.font_size))
            .text_color(color)
            .when_some(fill, |s, fill| s.bg(fill))
            .child(label)
    };
    let sidebar = div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(92.0))
        .p(px(tokens.spacing * 2.0))
        .gap(px(2.0))
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.border)
        .child(nav("Home", Some(theme.sidebar_accent), theme.fg))
        .child(nav("Agents", Some(theme.sidebar_hover), theme.fg))
        .child(nav("Tasks", None, theme.muted));
    let button = |label: &'static str, fill: Hsla, color: Hsla| {
        div()
            .flex()
            .items_center()
            .h(px(tokens.control_height - 6.0))
            .px(px(tokens.spacing * 3.0))
            .rounded(px(tokens.radius))
            .bg(fill)
            .text_size(px(tokens.font_size))
            .text_color(color)
            .child(label)
    };
    let card = div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .gap(px(tokens.spacing))
        .p(px(tokens.spacing * 2.0))
        .rounded(px(tokens.card_radius))
        .bg(theme.surface)
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .text_size(px(tokens.font_size))
                .text_color(theme.fg)
                .child("Review changes"),
        )
        .child(
            div()
                .flex()
                .gap(px(tokens.spacing * 2.0))
                .text_size(px(tokens.secondary_font_size))
                .child(div().text_color(theme.muted).child("3 files"))
                .child(div().text_color(theme.faint).child("hint"))
                .child(div().text_color(theme.link).child("link")),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(tokens.spacing * 2.0))
                .child(button("Approve", theme.accent, theme.on_accent()))
                .child(button("Later", theme.surface_hover, theme.fg))
                .child(
                    div()
                        .px(px(tokens.spacing * 2.0))
                        .rounded(px(tokens.radius))
                        .bg(theme.accent_soft)
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.fg)
                        .child("selected"),
                ),
        );
    let center = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .p(px(tokens.spacing * 3.0))
        .bg(theme.bg)
        .child(card);
    div()
        .flex()
        .overflow_hidden()
        .rounded(px(tokens.card_radius))
        .border_1()
        .border_color(theme.border)
        .child(sidebar)
        .child(center)
}

/// One chip per session state, in the colors the app draws them with.
pub(super) fn state_cards(theme: &Theme, tokens: &Tokens) -> Div {
    let chip = |label: &'static str, color: Hsla| {
        div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 2.0))
            .h(px(tokens.control_height - 6.0))
            .rounded(px(tokens.radius))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.fg)
            .child(div().size(px(8.0)).rounded_full().bg(color))
            .child(label)
    };
    div()
        .flex()
        .flex_wrap()
        .gap(px(tokens.spacing * 2.0))
        .child(chip("Working", theme.accent))
        .child(chip("Requests", theme.warning))
        .child(chip("Failed", theme.danger))
        .child(chip("Done", theme.success))
        .child(chip("Idle", theme.muted))
}

/// A hunk header, an added and a removed line.
pub(super) fn diff(theme: &Theme, tokens: &Tokens) -> Div {
    let line = |text: &'static str, fill: Hsla, color: Hsla, marker: Hsla| {
        div()
            .flex()
            .gap(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 2.0))
            .bg(fill)
            .font_family(MONO)
            .text_size(px(tokens.secondary_font_size))
            .text_color(color)
            .child(
                div()
                    .w(px(8.0))
                    .text_color(marker)
                    .child(match text.as_bytes()[0] {
                        b'+' => "+",
                        b'-' => "-",
                        _ => "",
                    }),
            )
            .child(&text[usize::from(text.starts_with(['+', '-']))..])
    };
    div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(tokens.radius))
        .border_1()
        .border_color(theme.border)
        .bg(theme.bg)
        .child(line(
            "@@ -12,3 +12,4 @@ fn accent()",
            theme.diff_hunk_bg,
            theme.diff_hunk_fg,
            theme.diff_hunk_fg,
        ))
        .child(line(
            "+let accent = pack.accent;",
            theme.diff_added_bg,
            theme.fg,
            theme.diff_added,
        ))
        .child(line(
            "-let accent = DEFAULT;",
            theme.diff_removed_bg,
            theme.fg,
            theme.diff_removed,
        ))
}

/// A prompt with the cursor and a selection, and the 16 ANSI colors of the terminal palette.
pub(super) fn terminal(terminal: &TerminalTheme) -> Div {
    let bg = rgb_color(terminal.background);
    let fg = rgb_color(terminal.foreground);
    let selected_text = terminal.selection_foreground.map_or(fg, rgb_color);
    let cursor_text = terminal.cursor_foreground.map_or(bg, rgb_color);
    let swatches = |colors: &[hutan_settings::Rgb]| {
        div().flex().gap(px(3.0)).children(
            colors
                .iter()
                .map(|c| div().size(px(18.0)).rounded(px(3.0)).bg(rgb_color(*c))),
        )
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .p(px(8.0))
        .rounded(px(6.0))
        .bg(bg)
        .border_1()
        .border_color(rgb_color(terminal.ansi[8]))
        .font_family(MONO)
        .text_size(px(12.0))
        .text_color(fg)
        .child(
            div()
                .flex()
                .items_center()
                .child(
                    div()
                        .text_color(rgb_color(terminal.ansi[2]))
                        .child("~/hutan "),
                )
                .child("hutan theme ls ")
                .child(
                    div()
                        .bg(rgb_color(terminal.selection))
                        .text_color(selected_text)
                        .child("selected"),
                )
                .child(" ")
                .child(
                    div()
                        .bg(rgb_color(terminal.cursor))
                        .text_color(cursor_text)
                        .child("█"),
                ),
        )
        .child(swatches(&terminal.ansi[..8]))
        .child(swatches(&terminal.ansi[8..]))
}

/// Navigation, action, state and dock icons of `icons`, drawn in the colors of `theme` (the
/// window's own), each marked as the pack's or as the embedded Phosphor fallback; one fallback
/// is always called out.
pub(super) fn icon_grid(
    icons: &ResolvedIcons,
    own: &BTreeSet<IconId>,
    theme: &Theme,
    tokens: &Tokens,
) -> Div {
    const GROUPS: [(&str, &[IconId]); 4] = [
        (
            "Navigation",
            &[
                IconId::NavSearch,
                IconId::NavTasks,
                IconId::NavAgents,
                IconId::NavMawas,
                IconId::NavSettings,
            ],
        ),
        (
            "Actions",
            &[
                IconId::ActionClose,
                IconId::ActionAdd,
                IconId::ActionRefresh,
                IconId::ActionCopy,
                IconId::ActionMore,
            ],
        ),
        (
            "States",
            &[
                IconId::StateWorking,
                IconId::StateCompleted,
                IconId::StateFailed,
                IconId::StateBlocked,
                IconId::StateIdle,
            ],
        ),
        (
            "Dock",
            &[
                IconId::DockLeft,
                IconId::DockBottom,
                IconId::DockRight,
                IconId::DockHide,
            ],
        ),
    ];
    let cell = |id: IconId| {
        let mine = own.contains(&id);
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(2.0))
            .w(px(54.0))
            .child(crate::icons::icon_in(icons, id, 20.0, theme.fg))
            .child(
                div()
                    .text_size(px(tokens.secondary_font_size - 1.0))
                    .text_color(theme.muted)
                    .child(SharedString::from(
                        id.id().rsplit('.').next().unwrap_or("").to_owned(),
                    )),
            )
            .child(
                div()
                    .text_size(px(tokens.secondary_font_size - 2.0))
                    .text_color(if mine { theme.accent } else { theme.faint })
                    .child(if mine { "this pack" } else { "Phosphor" }),
            )
    };
    let mut grid = div().flex().flex_col().gap(px(tokens.spacing * 2.0));
    for (label, ids) in GROUPS {
        grid = grid.child(
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted)
                        .child(label),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(tokens.spacing))
                        .children(ids.iter().map(|id| cell(*id))),
                ),
        );
    }
    // The fallback: the first icon the pack does not draw itself.
    let fallback = IconId::ALL.iter().copied().find(|id| !own.contains(id));
    grid.child(
        div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 2.0))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(theme.border)
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .children(fallback.map(|id| crate::icons::icon_in(icons, id, 20.0, theme.warning)))
            .child(SharedString::from(match fallback {
                Some(id) => format!(
                    "Fallback: `{}` is not in this pack, so Phosphor's icon is used; {} of {} icons are its own.",
                    id.id(),
                    own.len(),
                    IconId::COUNT
                ),
                None => "A complete pack: no icon falls back to Phosphor.".to_owned(),
            })),
    )
}
