//! When the scrollbars show and what they look like: one policy for the whole process, kept as a
//! GPUI global the app sets from `appearance.scrollbars`, macOS's "Show scroll bars" and the
//! color pack's `ui.scrollbar.*` tokens. A window that has no policy yet (a test window) shows
//! the bars always, so a thumb is there whenever there is more to scroll.

use std::time::Duration;

use gpui::{App, Global, Hsla, hsla};
use hutan_settings::ScrollbarsSetting;

/// How long an overlay thumb stays after the last scroll or the pointer leaving the edge.
pub const HOLD: Duration = Duration::from_millis(1000);
/// How long it takes to fade out (`docs/design.md` § Motion: the slow duration).
pub const FADE: Duration = Duration::from_millis(200);
/// The fade under Reduce Motion: opacity changes stay but are at most the base duration.
pub const FADE_REDUCED: Duration = Duration::from_millis(150);

/// When a thumb is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Thumb and track wherever there is more to scroll (macOS "Always").
    Always,
    /// An overlay thumb while the content scrolls and while the pointer is near the edge; it
    /// fades after [`HOLD`] (macOS "When scrolling" and "Automatically based on mouse or
    /// trackpad").
    Scrolling,
}

impl Visibility {
    /// What `setting` means now: `system` follows macOS (`system_always`: the preferred scroller
    /// style is the legacy one that keeps its bars).
    pub fn resolve(setting: ScrollbarsSetting, system_always: bool) -> Self {
        match setting {
            ScrollbarsSetting::Always => Self::Always,
            ScrollbarsSetting::Scrolling => Self::Scrolling,
            ScrollbarsSetting::System if system_always => Self::Always,
            ScrollbarsSetting::System => Self::Scrolling,
        }
    }
}

/// The colors of a scrollbar (`ui.scrollbar.thumb`, `.thumb.active`, `.track`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub thumb: Hsla,
    pub thumb_active: Hsla,
    pub track: Hsla,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            thumb: hsla(0.0, 0.0, 0.5, 0.4),
            thumb_active: hsla(0.0, 0.0, 0.5, 0.65),
            track: hsla(0.0, 0.0, 0.5, 0.1),
        }
    }
}

/// The process-wide scrollbar policy; [`Policy::of`] reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    /// `appearance.scrollbars`.
    pub setting: ScrollbarsSetting,
    /// macOS prefers bars that stay (`NSScroller.preferredScrollerStyle` is legacy).
    pub system_always: bool,
    pub style: Style,
}

impl Global for Policy {}

impl Default for Policy {
    fn default() -> Self {
        Self {
            setting: ScrollbarsSetting::Always,
            system_always: true,
            style: Style::default(),
        }
    }
}

impl Policy {
    /// The policy now.
    pub fn of(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }

    pub fn visibility(&self) -> Visibility {
        Visibility::resolve(self.setting, self.system_always)
    }

    fn change(cx: &mut App, f: impl FnOnce(&mut Self)) {
        let old = Self::of(cx);
        let mut new = old;
        f(&mut new);
        if new != old || cx.try_global::<Self>().is_none() {
            cx.set_global(new);
            cx.refresh_windows();
        }
    }
}

/// `appearance.scrollbars` changed (or was read for the first time).
pub fn set_setting(cx: &mut App, setting: ScrollbarsSetting) {
    Policy::change(cx, |policy| policy.setting = setting);
}

/// macOS's preferred scroller style changed (or was read for the first time).
pub fn set_system_always(cx: &mut App, system_always: bool) {
    Policy::change(cx, |policy| policy.system_always = system_always);
}

/// The color pack changed.
pub fn set_style(cx: &mut App, style: Style) {
    Policy::change(cx, |policy| policy.style = style);
}

/// How opaque an overlay thumb is `idle` after the last scroll or pointer: fully for [`HOLD`],
/// then out over the fade (an ease-out: quick at first), under Reduce Motion over the shorter
/// one.
pub fn fade_opacity(idle: Duration, reduced: bool) -> f32 {
    let Some(past) = idle.checked_sub(HOLD) else {
        return 1.0;
    };
    let fade = if reduced { FADE_REDUCED } else { FADE };
    let t = (past.as_secs_f32() / fade.as_secs_f32()).clamp(0.0, 1.0);
    (1.0 - t).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The system setting mapping: what each `appearance.scrollbars` value means for each state
    /// of macOS's "Show scroll bars".
    #[test]
    fn the_setting_resolves_against_the_system_style() {
        use ScrollbarsSetting::{Always, Scrolling, System};
        for (setting, system_always, expected) in [
            (System, true, Visibility::Always),
            (System, false, Visibility::Scrolling),
            (Always, false, Visibility::Always),
            (Always, true, Visibility::Always),
            (Scrolling, true, Visibility::Scrolling),
            (Scrolling, false, Visibility::Scrolling),
        ] {
            assert_eq!(
                Visibility::resolve(setting, system_always),
                expected,
                "{setting:?} with the system's bars always = {system_always}"
            );
        }
    }

    #[test]
    fn the_thumb_holds_for_a_second_then_fades_out_and_never_comes_back_by_itself() {
        let ms = Duration::from_millis;
        assert_eq!(fade_opacity(ms(0), false), 1.0);
        assert_eq!(
            fade_opacity(ms(1000), false),
            1.0,
            "held through the second"
        );
        let mid = fade_opacity(ms(1100), false);
        assert!(mid > 0.0 && mid < 1.0, "fading at 1.1 s: {mid}");
        assert_eq!(
            fade_opacity(ms(1200), false),
            0.0,
            "gone after the 200 ms fade"
        );
        assert_eq!(fade_opacity(ms(60_000), false), 0.0);
        let mut last = 1.0;
        for step in 0..=24 {
            let value = fade_opacity(ms(1000 + step * 10), false);
            assert!(value <= last, "monotonic at step {step}");
            last = value;
        }
    }

    #[test]
    fn reduced_motion_shortens_the_fade_to_150_ms() {
        assert_eq!(fade_opacity(Duration::from_millis(1150), true), 0.0);
        assert!(fade_opacity(Duration::from_millis(1150), false) > 0.0);
        assert!(FADE_REDUCED <= Duration::from_millis(150));
    }

    #[test]
    fn a_window_without_a_policy_shows_the_bars_always() {
        assert_eq!(Policy::default().visibility(), Visibility::Always);
    }
}
