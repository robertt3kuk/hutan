//! Semantic icons: GPUI sees only validated in-memory, content-addressed pack assets.
mod raster;
pub use raster::rasterized;
use std::borrow::Cow;
use std::sync::{Arc, LazyLock, RwLock};

use gpui::{AssetSource, Hsla, ParentElement as _, SharedString, Styled as _, canvas, div, px};
pub use hutan_settings::themes::icons::IconId as Icon;
pub use hutan_settings::themes::icons::SMALL_MAX_PX;
use hutan_settings::themes::icons::{self, ResolvedIcons};

struct Active {
    current: Arc<ResolvedIcons>,
    // One retired generation keeps the preceding submitted frame servable. No unbounded
    // content-addressed byte map: at most two 8 MiB user snapshots plus the embedded default.
    retired: Option<Arc<ResolvedIcons>>,
}
static ACTIVE: LazyLock<RwLock<Active>> = LazyLock::new(|| {
    RwLock::new(Active {
        current: icons::builtin(),
        retired: None,
    })
});

/// Install a validated snapshot. Returns whether any control's geometry changed.
pub fn install(next: Arc<ResolvedIcons>) -> bool {
    let mut active = ACTIVE.write().unwrap_or_else(|e| e.into_inner());
    if active
        .current
        .assets()
        .map(|a| &a.key)
        .eq(next.assets().map(|a| &a.key))
    {
        return false;
    }
    active.retired = Some(std::mem::replace(&mut active.current, next));
    true
}

pub fn path(id: Icon) -> SharedString {
    let active = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    SharedString::from(Arc::clone(&active.current.get(id).key))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let active = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
        Ok(std::iter::once(&active.current)
            .chain(active.retired.iter())
            .flat_map(|p| p.assets())
            .find(|a| a.key.as_ref() == path)
            .map(|a| Cow::Owned(a.bytes.to_vec())))
    }
    fn list(&self, prefix: &str) -> gpui::Result<Vec<SharedString>> {
        let active = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
        Ok(active
            .current
            .assets()
            .filter(|a| a.key.starts_with(prefix))
            .map(|a| SharedString::from(Arc::clone(&a.key)))
            .collect())
    }
}

/// Fixed control geometry and location-supplied color; packs cannot change hit targets. An icon
/// of [`SMALL_MAX_PX`] points or less draws the pack's `[small]` artwork (Phosphor Bold in the
/// built-in pack) where it has one, a larger one the regular artwork.
pub fn icon(id: Icon, size: f32, color: Hsla) -> gpui::Div {
    element(id, size).text_color(color)
}

/// An icon of `size` px that paints in its parent's text color (a hover that changes the text
/// color changes the icon).
pub fn sized(id: Icon, size: f32) -> gpui::Div {
    element(id, size)
}

fn element(id: Icon, size: f32) -> gpui::Div {
    let active = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    let asset = Arc::clone(active.current.get_sized(id, size));
    drop(active);
    element_of(asset, size)
}

/// The icon `id` of `pack`, a pack that is only being previewed (a Settings picker): it is not
/// installed, so nothing else in the app changes. The artwork is the pack's validated bytes,
/// painted through the same bounded raster cache as every icon.
pub fn icon_in(pack: &ResolvedIcons, id: Icon, size: f32, color: Hsla) -> gpui::Div {
    element_of(Arc::clone(pack.get_sized(id, size)), size).text_color(color)
}

fn element_of(asset: Arc<icons::IconAsset>, size: f32) -> gpui::Div {
    div().flex_none().size(px(size.clamp(1.0, 128.0))).child(
        canvas(
            |_, _, _| (),
            move |bounds, _, window, cx| {
                raster::paint(&asset, bounds, window.text_style().color, window, cx);
            },
        )
        .size_full(),
    )
}

/// Inline semantic artwork inheriting its parent's foreground (state tones stay at call sites).
pub fn glyph(id: Icon) -> gpui::Div {
    element(id, 14.0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_default_semantic_asset_is_served_and_tintable() {
        let builtin = icons::builtin();
        for &id in Icon::ALL {
            let bytes = Assets.load(&builtin.get(id).key).unwrap().unwrap();
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(
                text.contains("<svg") && text.contains("currentColor"),
                "{}",
                id.id()
            );
        }
        assert!(Assets.load("file:///etc/passwd").unwrap().is_none());
        assert!(Assets.load("icons/search.svg").unwrap().is_none());
    }

    #[test]
    fn every_small_asset_is_served_and_tintable() {
        let builtin = icons::builtin();
        let mut small = 0;
        for &id in Icon::ALL {
            let Some(asset) = builtin.small(id) else {
                continue;
            };
            small += 1;
            let bytes = Assets.load(&asset.key).unwrap().unwrap();
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(
                text.contains("<svg") && text.contains("currentColor"),
                "{}",
                id.id()
            );
        }
        assert!(small > 100);
    }

    #[test]
    fn a_pack_that_changes_only_a_small_artwork_is_a_new_snapshot() {
        use hutan_settings::themes::{SystemAppearance, ThemeRegistry};
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("icon-themes/small-only");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("icon-theme.toml"),
            "schema_version = 1\nid = \"small-only\"\nname = \"S\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nextends = \"builtin:phosphor\"\n\n[icons]\n\n[small]\n\"action.close\" = \"c.svg\"\n",
        )
        .unwrap();
        std::fs::write(
            pack.join("c.svg"),
            "<svg viewBox=\"0 0 24 24\"><path fill=\"currentColor\" d=\"M1 1L20 20\"/></svg>",
        )
        .unwrap();
        let registry = ThemeRegistry::new(dir.path());
        let (resolved, notes) = registry.select_icons("small-only", SystemAppearance::Dark);
        assert!(notes.is_empty(), "{notes:?}");
        // Regular artwork is Phosphor's; only the small one differs from the built-in pack.
        assert_eq!(
            resolved.get(Icon::ActionClose).key,
            icons::builtin().get(Icon::ActionClose).key
        );
        assert!(
            install(Arc::clone(&resolved)),
            "a changed small asset is a change"
        );
        assert!(!install(resolved), "the same pack twice is not");
        assert!(install(icons::builtin()), "and back");
    }

    #[test]
    fn the_composers_send_is_an_up_arrow_and_its_stop_a_solid_square() {
        let builtin = icons::builtin();
        let art = |id: Icon| {
            let bytes = Assets.load(&builtin.get(id).key).unwrap().unwrap();
            String::from_utf8(bytes.into_owned()).unwrap()
        };
        // Phosphor `arrow-up` (Regular): the shaft runs from y=216 to the head at y=59.
        assert!(art(Icon::ActionForward).contains("M205.66,117.66"));
        // Phosphor `stop` (Fill): one closed rounded square, no pause bars.
        let stop = art(Icon::ActionInterrupt);
        assert!(stop.contains("M216,56V200") && stop.matches("<path").count() == 1);
    }

    #[test]
    fn only_the_current_and_one_retired_generation_are_served() {
        use hutan_settings::themes::{SystemAppearance, ThemeRegistry};
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("icon-themes/gen");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("icon-theme.toml"),
            "schema_version = 1\nid = \"gen\"\nname = \"Gen\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nextends = \"builtin:phosphor\"\n\n[icons]\n\"action.close\" = \"c.svg\"\n",
        )
        .unwrap();
        let registry = ThemeRegistry::new(dir.path());
        let mut keys = Vec::new();
        for generation in 0..3 {
            std::fs::write(
                pack.join("c.svg"),
                format!(
                    "<svg viewBox=\"0 0 24 24\"><path fill=\"currentColor\" d=\"M{generation} 1L20 20\"/></svg>"
                ),
            )
            .unwrap();
            registry.rescan();
            let (resolved, _) = registry.select_icons("gen", SystemAppearance::Dark);
            assert!(install(resolved), "a changed asset changes the snapshot");
            keys.push(path(Icon::ActionClose));
        }
        assert_ne!(keys[0], keys[1]);
        assert_ne!(keys[1], keys[2]);
        assert!(Assets.load(&keys[2]).unwrap().is_some(), "current");
        assert!(Assets.load(&keys[1]).unwrap().is_some(), "previous frame");
        assert!(
            Assets.load(&keys[0]).unwrap().is_none(),
            "older generation released"
        );
    }
}
