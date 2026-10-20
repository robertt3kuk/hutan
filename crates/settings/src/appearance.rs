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
