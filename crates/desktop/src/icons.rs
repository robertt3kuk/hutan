//! Semantic icons: GPUI sees only validated in-memory, content-addressed pack assets.
mod raster;
pub use raster::rasterized;
use std::borrow::Cow;
use std::sync::{Arc, LazyLock, RwLock};

use gpui::{AssetSource, Hsla, ParentElement as _, SharedString, Styled as _, canvas, div, px};
pub use hutan_settings::themes::icons::IconId as Icon;
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
        .zip(next.assets())
        .all(|(a, b)| a.key == b.key)
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

/// Fixed control geometry and location-supplied color; packs cannot change hit targets.
pub fn icon(id: Icon, size: f32, color: Hsla) -> gpui::Div {
    element(id, size).text_color(color)
}

fn element(id: Icon, size: f32) -> gpui::Div {
    let active = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    let asset = Arc::clone(active.current.get(id));
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
