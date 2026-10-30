//! `[appearance]`: density, the light/dark mode and which color pack each mode uses. The sizes
//! that change with density live in the packs (`docs/themes.md`), not here.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Setting;
use crate::themes::{DARK, LIGHT};

/// UI density. Compact is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    #[default]
    Compact,
    Comfortable,
}

/// The `appearance` setting: whether the window follows macOS light/dark or is pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AppearanceMode {
    #[default]
    System,
    Light,
    Dark,
}

/// `appearance.reduce_motion`: whether the interface moves. `system` follows macOS (System
/// Settings ▸ Accessibility ▸ Display ▸ Reduce motion) live; `on` and `off` override it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReduceMotionSetting {
    #[default]
    System,
    On,
    Off,
}

/// `appearance.scrollbars`: when the scrollbars show. `system` follows macOS (System Settings ▸
/// Appearance ▸ Show scroll bars: "Automatically based on mouse or trackpad" and "When scrolling"
/// are overlay bars, "Always" is bars that stay) live; `always` and `scrolling` override it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ScrollbarsSetting {
    #[default]
    System,
    Always,
    Scrolling,
}

/// The user's overrides of one color pack: `[appearance.theme_overrides."<id>".colors]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ThemeOverrides {
    /// Color tokens (`"ui.accent" = "#d99a58"`) laid over the pack; see `docs/themes.md` for the
    /// list. Unknown tokens are ignored with a warning; a bad value rejects the whole table.
    pub colors: BTreeMap<String, String>,
}

/// The `appearance` namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AppearanceSettings {
    /// `compact` (default) or `comfortable`; see the density token table in `docs/design.md`.
    pub density: Density,
    /// `system` (follow macOS light/dark live, default), `light` or `dark`. Files written before
    /// v2 call this key `chrome`.
    #[serde(alias = "chrome")]
    pub appearance: AppearanceMode,
    /// Id of the color pack used while the window is light (`hutan-light`, or a light pack from
    /// `<data dir>/themes`). An unknown or dark pack falls back to `hutan-light` and applies
    /// again once installed.
    pub light_theme: String,
    /// Id of the color pack used while the window is dark (`hutan-dark`, or a dark pack from
    /// `<data dir>/themes`).
    pub dark_theme: String,
    /// Independent monochrome icon pack from `<data dir>/icon-themes`.
    pub icon_theme: String,
    /// Font family of the interface (chrome, chat, empty states). Empty (default): the color
    /// pack's `[fonts] ui`. Chat and prose follow it unless the pack names a prose family of its
    /// own. A family that is not installed is ignored (the pack's font is drawn) with a note in
    /// Settings.
    pub ui_font_family: String,
    /// `system` (follow macOS Reduce Motion live, default), `on` or `off`. Reduced, nothing
    /// slides or changes height and fades are at most 150 ms; see `docs/design.md` § Motion.
    pub reduce_motion: ReduceMotionSetting,
    /// `system` (follow macOS "Show scroll bars" live, default), `always` (a thumb on a track
    /// wherever there is more to scroll) or `scrolling` (an overlay thumb that appears while the
    /// content scrolls and when the pointer is near the edge, and fades after about a second);
    /// see `docs/design.md` § Scrolling.
    pub scrollbars: ScrollbarsSetting,
    /// Color overrides per pack id, applied on top of the pack.
    pub theme_overrides: BTreeMap<String, ThemeOverrides>,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            density: Density::default(),
            appearance: AppearanceMode::default(),
            light_theme: LIGHT.to_owned(),
            dark_theme: DARK.to_owned(),
            icon_theme: "phosphor".to_owned(),
            ui_font_family: String::new(),
            reduce_motion: ReduceMotionSetting::default(),
            scrollbars: ScrollbarsSetting::default(),
            theme_overrides: BTreeMap::new(),
        }
    }
}

impl Setting for AppearanceSettings {
    const NAMESPACE: &'static str = "appearance";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_compact_and_system() {
        let a = AppearanceSettings::default();
        assert_eq!(a.density, Density::Compact);
        assert_eq!(a.appearance, AppearanceMode::System);
        assert_eq!(
            (a.light_theme.as_str(), a.dark_theme.as_str()),
            ("hutan-light", "hutan-dark")
        );
        assert!(a.theme_overrides.is_empty());
    }

    #[test]
    fn reduce_motion_reads_system_on_off_and_defaults_to_system() {
        assert_eq!(
            AppearanceSettings::default().reduce_motion,
            ReduceMotionSetting::System
        );
        for (text, expected) in [
            ("system", ReduceMotionSetting::System),
            ("on", ReduceMotionSetting::On),
            ("off", ReduceMotionSetting::Off),
        ] {
            let a: AppearanceSettings =
                toml::from_str(&format!("reduce_motion = \"{text}\"\n")).unwrap();
            assert_eq!(a.reduce_motion, expected);
        }
        assert!(toml::from_str::<AppearanceSettings>("reduce_motion = \"maybe\"\n").is_err());
    }

    #[test]
    fn scrollbars_read_system_always_scrolling_and_default_to_system() {
        assert_eq!(
            AppearanceSettings::default().scrollbars,
            ScrollbarsSetting::System
        );
        for (text, expected) in [
            ("system", ScrollbarsSetting::System),
            ("always", ScrollbarsSetting::Always),
            ("scrolling", ScrollbarsSetting::Scrolling),
        ] {
            let a: AppearanceSettings =
                toml::from_str(&format!("scrollbars = \"{text}\"\n")).unwrap();
            assert_eq!(a.scrollbars, expected);
        }
        assert!(toml::from_str::<AppearanceSettings>("scrollbars = \"never\"\n").is_err());
    }

    #[test]
    fn the_theme_selectors_and_overrides_read_from_toml() {
        let a: AppearanceSettings = toml::from_str(
            "dark_theme = \"canopy-night\"\n[theme_overrides.\"canopy-night\".colors]\n\"ui.accent\" = \"#d99a58\"\n",
        )
        .unwrap();
        assert_eq!(a.dark_theme, "canopy-night");
        assert_eq!(a.light_theme, "hutan-light");
        assert_eq!(
            a.theme_overrides["canopy-night"].colors["ui.accent"],
            "#d99a58"
        );
    }

    #[test]
    fn deserializes_lowercase_names_with_defaults() {
        let a: AppearanceSettings =
            toml::from_str("density = \"comfortable\"\nappearance = \"dark\"\n").unwrap();
        assert_eq!(a.density, Density::Comfortable);
        assert_eq!(a.appearance, AppearanceMode::Dark);
        let a: AppearanceSettings = toml::from_str("appearance = \"light\"\n").unwrap();
        assert_eq!(a.density, Density::Compact);
        assert!(toml::from_str::<AppearanceSettings>("density = \"roomy\"\n").is_err());
    }
}

#[cfg(test)]
mod alias_tests {
    use super::*;

    #[test]
    fn the_pre_v2_chrome_key_still_reads() {
        let a: AppearanceSettings = toml::from_str("chrome = \"dark\"\n").unwrap();
        assert_eq!(a.appearance, AppearanceMode::Dark);
    }
}
