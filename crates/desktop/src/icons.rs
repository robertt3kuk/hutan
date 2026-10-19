//! The icon set: a handful of Lucide SVGs (ISC, `assets/icons/LICENSE-lucide.txt`) embedded at
//! compile time and served to GPUI through an [`AssetSource`], so the executable carries them
//! and the app bundle needs no resource files. [`icon`] draws one tinted by a theme colour.

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, SharedString, Styled as _, Svg, px, svg};

macro_rules! icons {
    ($($variant:ident => $file:literal),+ $(,)?) => {
        /// An icon of the set. The asset path is `icons/<file>.svg`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Icon {
            $($variant),+
        }

        impl Icon {
            /// Every icon, for iteration (the asset source, the lookup test).
            pub const ALL: &'static [Icon] = &[$(Icon::$variant),+];

            /// The path GPUI loads the icon under.
            pub const fn path(self) -> &'static str {
                match self {
                    $(Icon::$variant => concat!("icons/", $file, ".svg")),+
                }
            }

            const fn bytes(self) -> &'static [u8] {
                match self {
                    $(Icon::$variant => include_bytes!(concat!(
                        "../../../assets/icons/", $file, ".svg"
                    ))),+
                }
            }
        }
    };
}

icons! {
    Search => "search",
    ListChecks => "list-checks",
    LayoutDashboard => "layout-dashboard",
    Sparkles => "sparkles",
    History => "history",
    GitCompare => "git-compare",
    Info => "info",
    GitBranch => "git-branch",
    Gauge => "gauge",
    Rows3 => "rows-3",
    ListFilter => "list-filter",
    RefreshCw => "refresh-cw",
    Settings => "settings",
    ChevronDown => "chevron-down",
    ChevronUp => "chevron-up",
    ChevronLeft => "chevron-left",
    Ellipsis => "ellipsis",
    Plus => "plus",
    ArrowUpDown => "arrow-up-down",
    PanelLeft => "panel-left",
    PanelRight => "panel-right",
    Folder => "folder",
}

impl Icon {
    /// The icon of a built-in panel (its tab in the right sidebar's strip, its nav row), when the
    /// set has one; other panels keep their glyph.
    pub fn for_panel(panel: &str) -> Option<Icon> {
        Some(match panel {
            "history" => Icon::History,
            "changes" => Icon::GitCompare,
            "inspector" => Icon::Info,
            "agents" => Icon::GitBranch,
            "resources" => Icon::Gauge,
            "tasks" => Icon::ListChecks,
            "overview" => Icon::LayoutDashboard,
            "assistant" => Icon::Sparkles,
            _ => return None,
        })
    }
}

/// Serves the embedded icons; install with `Application::with_assets`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(Icon::ALL
            .iter()
            .find(|icon| icon.path() == path)
            .map(|icon| Cow::Borrowed(icon.bytes())))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Icon::ALL
            .iter()
            .filter(|icon| icon.path().starts_with(path))
            .map(|icon| SharedString::from(icon.path()))
            .collect())
    }
}

/// `icon` drawn `size` px square in `color`. It does not shrink in a flex row.
pub fn icon(icon: Icon, size: f32, color: Hsla) -> Svg {
    svg()
        .path(icon.path())
        .flex_none()
        .size(px(size))
        .text_color(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_served_and_is_an_svg() {
        let assets = Assets;
        for icon in Icon::ALL {
            let bytes = assets
                .load(icon.path())
                .unwrap()
                .unwrap_or_else(|| panic!("{icon:?} is not served"));
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(text.contains("<svg"), "{icon:?} is not an SVG");
            assert!(text.contains("currentColor"), "{icon:?} cannot be tinted");
        }
        assert_eq!(assets.list("icons/").unwrap().len(), Icon::ALL.len());
        assert!(assets.load("icons/nope.svg").unwrap().is_none());
    }

    #[test]
    fn paths_are_unique() {
        let mut paths: Vec<&str> = Icon::ALL.iter().map(|i| i.path()).collect();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), Icon::ALL.len());
    }
}
