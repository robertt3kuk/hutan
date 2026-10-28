//! The lead lane of a row and the state badge on it (STYLE-TOKENS, spike 2026-10-28 §1.5): one
//! harness glyph in a `sidebar.lane` box, and in the glyph's top-right corner a badge of
//! `icon.badge.size` that says what the thing is doing. Shape and colour both, never colour
//! alone: an arc turns while it works, a disc holds `!` when a request waits, `×` when it failed and
//! `✓` when it finished unread, a hollow ring marks an ended session, and an idle or read one has
//! no badge at all (the quiet state).

use gpui::{
    AnyElement, ElementId, Hsla, IntoElement, ParentElement as _, PathBuilder, Rgba, Styled as _,
    canvas, div, point, prelude::FluentBuilder as _, px, rgb, white,
};

use super::spinner_at;
use crate::icons::Icon;
use crate::theme::{Theme, metrics};

/// What a badge says. [`Idle`](Self::Idle) has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BadgeState {
    /// An agent turn or a command is running: an accent arc that turns (still under Reduce
    /// Motion).
    Working,
    /// A request waits for the user (a question, an approval, a plan): a `warning` disc with
    /// `!`. The word is the group's: *Request*.
    Request,
    /// It failed: a `danger` disc with `×`.
    Failed,
    /// A turn finished and nobody has looked: a `success` disc with `✓`.
    DoneUnread,
    /// Nothing to say: no badge.
    Idle,
    /// The session ended: a hollow ring, its name `faint`.
    Ended,
}

impl BadgeState {
    /// The state in words, for a tooltip (`None` for idle: nothing to say).
    pub fn word(self) -> Option<&'static str> {
        match self {
            Self::Working => Some("Working"),
            Self::Request => Some("Request"),
            Self::Failed => Some("Failed"),
            Self::DoneUnread => Some("Done, unread"),
            Self::Idle => None,
            Self::Ended => Some("Ended"),
        }
    }

    /// Whether the badge draws anything.
    pub fn is_shown(self) -> bool {
        self != Self::Idle
    }

    /// The fill of the disc, for the states that are a disc.
    fn disc(self, theme: &Theme) -> Option<Hsla> {
        match self {
            Self::Request => Some(theme.warning),
            Self::Failed => Some(theme.danger),
            Self::DoneUnread => Some(theme.success),
            Self::Working | Self::Idle | Self::Ended => None,
        }
    }
}

/// The relative luminance of `color` (WCAG), alpha ignored.
fn luminance(color: Hsla) -> f32 {
    let Rgba { r, g, b, .. } = Rgba::from(color);
    let linear = |c: f32| {
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn contrast(a: Hsla, b: Hsla) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The ink of a mark on a `fill`: the warm near-black of the built-in packs or white, whichever
/// reads better, so a user pack's `state.*` colours keep a legible mark.
pub fn ink_on(fill: Hsla) -> Hsla {
    let dark: Hsla = rgb(0x1f1e1d).into();
    let light = white();
    if contrast(fill, dark) >= contrast(fill, light) {
        dark
    } else {
        light
    }
}

type Strokes = &'static [&'static [(f32, f32)]];

/// A mark in a square of `size` px: every polyline of `strokes` (unit coordinates), 1.2 px wide.
fn stroke_mark(size: f32, strokes: Strokes, ink: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            for points in strokes {
                let mut builder = PathBuilder::stroke(px(1.2));
                for (i, &(x, y)) in points.iter().enumerate() {
                    let at = point(
                        bounds.origin.x + px(size * x),
                        bounds.origin.y + px(size * y),
                    );
                    if i == 0 {
                        builder.move_to(at);
                    } else {
                        builder.line_to(at);
                    }
                }
                if let Ok(path) = builder.build() {
                    window.paint_path(path, ink);
                }
            }
        },
    )
    .size(px(size))
}

/// The `×` of a failed badge.
const CROSS: Strokes = &[&[(0.32, 0.32), (0.68, 0.68)], &[(0.68, 0.32), (0.32, 0.68)]];
/// The `✓` of a done badge.
const TICK: Strokes = &[&[(0.27, 0.52), (0.44, 0.68), (0.74, 0.34)]];

/// The `!` of a request badge: a bar over a dot, drawn with boxes so it stays crisp at 8 px.
fn bang(ink: Hsla) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(0.7))
        .child(div().w(px(1.2)).h(px(2.5)).rounded(px(0.6)).bg(ink))
        .child(div().size(px(1.3)).rounded_full().bg(ink))
}

/// The badge of `state`, ringed in `ground` (the colour of what it sits on: the sidebar, or the
/// selected row's fill) so it cuts the glyph under it; `None` for [`BadgeState::Idle`]. `id`
/// keys the spinner's animation.
pub fn state_badge(
    state: BadgeState,
    id: impl Into<ElementId>,
    ground: Hsla,
    theme: &Theme,
) -> Option<AnyElement> {
    if !state.is_shown() {
        return None;
    }
    let size = metrics().icon_badge_size;
    // The ring: 1 px of `ground` all round.
    let outer = div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(size + 2.0))
        .rounded_full()
        .bg(ground);
    let badge = match state {
        BadgeState::Working => outer.child(spinner_at(id, size, theme.accent, 15.0)),
        BadgeState::Ended => outer.child(
            div()
                .size(px(size - 1.0))
                .rounded_full()
                .border_1()
                .border_color(theme.faint),
        ),
        BadgeState::Request | BadgeState::Failed | BadgeState::DoneUnread => {
            let fill = state.disc(theme).unwrap_or(theme.muted);
            let ink = ink_on(fill);
            outer.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(size))
                    .rounded_full()
                    .bg(fill)
                    .child(match state {
                        BadgeState::Request => bang(ink).into_any_element(),
                        BadgeState::Failed => stroke_mark(size, CROSS, ink).into_any_element(),
                        _ => stroke_mark(size, TICK, ink).into_any_element(),
                    }),
            )
        }
        BadgeState::Idle => return None,
    };
    Some(badge.into_any_element())
}

/// The lead lane of a row: a `sidebar.lane` wide box holding the harness `glyph` at
/// `icon.row.size` (so the pack's `[small]` artwork draws) in `muted` (`fg` on the selected
/// row, `faint` once ended) and, in its top-right corner, the [`state_badge`] of `state`. The
/// box is as wide as the lane whatever the badge: titles align to it.
pub fn row_lane(
    id: impl Into<ElementId>,
    glyph: Icon,
    state: BadgeState,
    ground: Hsla,
    selected: bool,
    theme: &Theme,
) -> gpui::Div {
    let m = metrics();
    let color = if state == BadgeState::Ended {
        theme.faint
    } else if selected {
        theme.fg
    } else {
        theme.muted
    };
    let badge = state_badge(state, id, ground, theme);
    div()
        .flex_none()
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .size(px(m.sidebar_lane))
        .child(crate::icons::icon(glyph, m.icon_row_size, color))
        .when_some(badge, |lane, badge| {
            lane.child(
                div()
                    .absolute()
                    .top(px(-m.icon_badge_size * 0.375))
                    .right(px(-m.icon_badge_size * 0.25))
                    .child(badge),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_quiet_state_has_no_badge() {
        for state in [
            BadgeState::Working,
            BadgeState::Request,
            BadgeState::Failed,
            BadgeState::DoneUnread,
            BadgeState::Ended,
        ] {
            assert!(state.is_shown() && state.word().is_some(), "{state:?}");
        }
        assert!(!BadgeState::Idle.is_shown() && BadgeState::Idle.word().is_none());
    }

    #[test]
    fn a_mark_is_legible_on_every_disc_of_both_built_in_packs() {
        for theme in [Theme::dark(), Theme::light()] {
            for state in [
                BadgeState::Request,
                BadgeState::Failed,
                BadgeState::DoneUnread,
            ] {
                let fill = state.disc(&theme).expect("a disc state");
                let ink = ink_on(fill);
                assert!(
                    contrast(fill, ink) >= 3.0,
                    "{state:?} on {}: {:.2}:1",
                    if theme.dark { "dark" } else { "light" },
                    contrast(fill, ink)
                );
            }
        }
    }

    #[test]
    fn ink_follows_the_fill() {
        assert_eq!(ink_on(white()), rgb(0x1f1e1d).into());
        assert_eq!(ink_on(rgb(0x101010).into()), white());
    }
}
