//! STYLE-TOKENS: the style kit's pieces measured in a bare window: the lead lane is exactly
//! `sidebar.lane` wide whatever its badge, a badge is `icon.badge.size` plus its ring, a group
//! label is tracked capitals with its count at the far end, a mono chip grows with its text.
//! No daemon.

use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, Window, div, px,
};
use hutan_desktop::icons::Icon;
use hutan_desktop::theme::{Density, DensityExt as _, Theme, metrics};
use hutan_desktop::workbench::ui::{self, BadgeState};

/// A state with the debug selectors of its lane and of its lone badge.
const STATES: [(&str, &str, &str, BadgeState); 6] = [
    (
        "working",
        "lane-working",
        "badge-working",
        BadgeState::Working,
    ),
    (
        "needs-you",
        "lane-needs-you",
        "badge-needs-you",
        BadgeState::NeedsYou,
    ),
    ("failed", "lane-failed", "badge-failed", BadgeState::Failed),
    ("done", "lane-done", "badge-done", BadgeState::DoneUnread),
    ("idle", "lane-idle", "badge-idle", BadgeState::Idle),
    ("ended", "lane-ended", "badge-ended", BadgeState::Ended),
];

struct Kit {
    theme: Theme,
}

impl Render for Kit {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let tokens = Density::Compact.ui();
        let theme = &self.theme;
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_start()
            .gap(px(8.0))
            .p(px(16.0))
            .child(
                div()
                    .flex()
                    .gap(px(16.0))
                    .children(STATES.map(|(name, lane, _, state)| {
                        div()
                            .debug_selector(move || lane.into())
                            .child(ui::row_lane(
                                format!("lane-{name}"),
                                Icon::HarnessOmp,
                                state,
                                theme.sidebar,
                                false,
                                theme,
                            ))
                    })),
            )
            .child(
                div()
                    .flex()
                    .gap(px(16.0))
                    .children(STATES.map(|(name, _, badge, state)| {
                        // The badge alone, as the tab strip draws it.
                        div()
                            .debug_selector(move || badge.into())
                            .children(ui::state_badge(
                                state,
                                format!("badge-{name}"),
                                theme.sidebar,
                                theme,
                            ))
                    })),
            )
            .child(
                div()
                    .w(px(200.0))
                    .debug_selector(|| "header".into())
                    .child(ui::group_header(
                        "Requests",
                        Some("2".into()),
                        theme,
                        &tokens,
                    )),
            )
            .child(
                div()
                    .debug_selector(|| "label".into())
                    .child(ui::group_label("Requests", theme, &tokens)),
            )
            .child(
                div()
                    .debug_selector(|| "plain".into())
                    .text_size(px(tokens.group_font_size))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("REQUESTS"),
            )
            .child(
                div()
                    .debug_selector(|| "chip-short".into())
                    .child(ui::chip_mono("gpt-6", theme, &tokens)),
            )
            .child(
                div()
                    .debug_selector(|| "chip-long".into())
                    .child(ui::chip_mono("claude-sonnet-5-5", theme, &tokens)),
            )
    }
}

fn open(cx: &mut TestAppContext, theme: Theme) -> &mut gpui::VisualTestContext {
    let (_view, cx) = cx.add_window_view(move |_, _| Kit { theme });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx
}

#[gpui::test]
fn the_lane_is_as_wide_as_the_token_whatever_its_badge(cx: &mut TestAppContext) {
    for theme in [Theme::dark(), Theme::light()] {
        let cx = open(cx, theme);
        let width = metrics().sidebar_lane;
        assert_eq!(width, 16.0);
        for (name, selector, _, _) in STATES {
            let bounds = cx.debug_bounds(selector).unwrap();
            assert_eq!(
                (bounds.size.width, bounds.size.height),
                (px(width), px(width)),
                "{name}: the badge floats over the glyph and takes no room"
            );
        }
    }
}

#[gpui::test]
fn a_badge_is_the_token_size_plus_its_ring_and_idle_has_none(cx: &mut TestAppContext) {
    let cx = open(cx, Theme::dark());
    let want = metrics().icon_badge_size + 2.0;
    assert_eq!(want, 10.0);
    for (name, _, badge, state) in STATES {
        let bounds = cx.debug_bounds(badge);
        if state == BadgeState::Idle {
            assert!(
                bounds.is_none_or(|b| b.size.width == px(0.0)),
                "idle: the quiet state has no badge"
            );
        } else {
            let bounds = bounds.unwrap();
            assert_eq!(
                (bounds.size.width, bounds.size.height),
                (px(want), px(want)),
                "{name}"
            );
        }
    }
}

#[gpui::test]
fn a_group_label_is_tracked_and_its_count_sits_at_the_far_end(cx: &mut TestAppContext) {
    let cx = open(cx, Theme::dark());
    let label = cx.debug_bounds("label").unwrap();
    let plain = cx.debug_bounds("plain").unwrap();
    // Eight letters, 0.075 em of a 10 px label between them: about 6 px wider than the same
    // capitals untracked (kerning pairs lost to the per-letter boxes eat a little).
    let extra = f32::from(label.size.width) - f32::from(plain.size.width);
    assert!(
        (3.0..=9.0).contains(&extra),
        "tracked {} vs plain {}",
        f32::from(label.size.width),
        f32::from(plain.size.width)
    );
    let header = cx.debug_bounds("header").unwrap();
    assert_eq!(header.size.width, px(200.0));
    assert!(
        label.size.height <= px(17.0),
        "a 10 px label is one line: {:?}",
        label.size.height
    );
}

#[gpui::test]
fn a_chip_grows_with_its_text_and_stays_a_small_line(cx: &mut TestAppContext) {
    let cx = open(cx, Theme::dark());
    let short = cx.debug_bounds("chip-short").unwrap();
    let long = cx.debug_bounds("chip-long").unwrap();
    assert!(long.size.width > short.size.width);
    assert!(
        short.size.height <= px(18.0),
        "a 9.5 px chip: {:?}",
        short.size.height
    );
    assert_eq!(short.size.height, long.size.height);
}
