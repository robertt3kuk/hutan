use super::*;
use crate::themes::{SystemAppearance, ThemeRegistry};
use std::fs;

const SAFE: &str = "<svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\"><path d=\"M6 6 18 18M18 6 6 18\"/></svg>";
fn example(root: &Path, extra: &str) -> PathBuf {
    let pack = root.join("icon-themes/canopy-lines");
    fs::create_dir_all(pack.join("icons")).unwrap();
    fs::write(pack.join("icon-theme.toml"),format!("schema_version = 1\nid = \"canopy-lines\"\nname = \"Canopy Lines\"\nauthors = [\"Hutan Theme Authors\"]\nlicense = \"MIT OR Apache-2.0\"\nextends = \"builtin:phosphor\"\n{extra}\n[icons]\n\"nav.search\" = \"icons/search.svg\"\n\"action.close\" = \"icons/close.svg\"\n")).unwrap();
    fs::write(pack.join("icons/close.svg"), SAFE).unwrap();
    fs::write(
        pack.join("icons/search.svg"),
        SAFE.replace("M6 6 18 18M18 6 6 18", "M1 1L12 12"),
    )
    .unwrap();
    pack
}
#[test]
fn builtin_catalog_is_exhaustive_safe_licensed_and_distinct() {
    assert!(LICENSE.contains("MIT License") && LICENSE.contains("Copyright"));
    let (declaration, diagnostics) = parse(BUILTIN, None);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let declaration = declaration.unwrap();
    assert_eq!(declaration.icons.len(), IconId::COUNT);
    let base = builtin();
    let mut keys = BTreeSet::new();
    for &id in IconId::ALL {
        assert_eq!(IconId::from_id(id.id()), Some(id));
        let asset = base.get(id);
        assert!(keys.insert(Arc::clone(&asset.key)));
        assert!(
            std::str::from_utf8(&asset.bytes)
                .unwrap()
                .contains("currentColor")
        );
        assert!(svg::sanitize(&asset.bytes).is_ok(), "{}", id.id());
    }
    assert_ne!(
        base.get(IconId::PanelAgents).bytes,
        base.get(IconId::PanelChanges).bytes
    );
    assert_ne!(
        base.get(IconId::PanelAgents).bytes,
        base.get(IconId::VcsBranch).bytes
    );
    assert_ne!(
        base.get(IconId::PanelInspector).bytes,
        base.get(IconId::StateInfo).bytes
    );
    assert_eq!(
        base.get(IconId::PanelAgents).bytes,
        base.get(IconId::NavAgents).bytes
    );
}
#[test]
fn two_icon_example_changes_exactly_two_ids_and_not_colors() {
    let dir = tempfile::tempdir().unwrap();
    example(dir.path(), "");
    let registry = ThemeRegistry::new(dir.path());
    let colors = registry.snapshot();
    let (selected, notes) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    assert!(notes.is_empty(), "{notes:?}");
    let base = builtin();
    let changed: Vec<_> = IconId::ALL
        .iter()
        .copied()
        .filter(|&id| selected.get(id).bytes != base.get(id).bytes)
        .collect();
    assert_eq!(changed, vec![IconId::NavSearch, IconId::ActionClose]);
    let (light, _) = registry.select_icons("canopy-lines", SystemAppearance::Light);
    assert_eq!(
        selected.get(IconId::ActionClose).key,
        light.get(IconId::ActionClose).key
    );
    assert!(Arc::ptr_eq(&colors, &registry.snapshot()));
}
#[test]
fn same_file_multiple_ids_unknown_warning_and_per_id_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let pack = example(dir.path(), "");
    let file = pack.join("icon-theme.toml");
    let manifest = fs::read_to_string(&file)
        .unwrap()
        .replace("icons/search.svg", "icons/close.svg");
    fs::write(
        &file,
        format!(
            "{manifest}\n\"future.icon\" = \"ignored.svg\"\n\"brand.menu-bar\" = \"ignored.svg\"\n"
        ),
    )
    .unwrap();
    let registry = ThemeRegistry::new(dir.path());
    assert_eq!(registry.icon_snapshot().diagnostics.len(), 2);
    let (selected, _) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    assert_eq!(
        selected.get(IconId::ActionClose).bytes,
        selected.get(IconId::NavSearch).bytes
    );
    assert_eq!(
        selected.get(IconId::DockRight).key,
        builtin().get(IconId::DockRight).key
    );
}
#[test]
fn reload_changes_cache_key_and_actual_bytes_and_preserves_last_good() {
    let dir = tempfile::tempdir().unwrap();
    let pack = example(dir.path(), "");
    let registry = ThemeRegistry::new(dir.path());
    let (old, _) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    assert!(!registry.rescan());
    fs::write(
        pack.join("icons/close.svg"),
        SAFE.replace("M6 6 18 18M18 6 6 18", "M2 2L20 20"),
    )
    .unwrap();
    assert!(registry.rescan());
    let (next, _) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    assert_ne!(
        old.get(IconId::ActionClose).key,
        next.get(IconId::ActionClose).key
    );
    assert_ne!(
        old.get(IconId::ActionClose).bytes,
        next.get(IconId::ActionClose).bytes
    );
    fs::write(pack.join("icons/close.svg"), "<svg><script/></svg>").unwrap();
    assert!(registry.rescan());
    let (kept, notes) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    assert!(!notes.is_empty());
    assert_eq!(
        kept.get(IconId::ActionClose).key,
        next.get(IconId::ActionClose).key
    );
    fs::write(pack.join("icon-theme.toml"), "bad = [").unwrap();
    assert!(registry.rescan());
    let (kept, _) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    assert_eq!(
        kept.get(IconId::ActionClose).key,
        next.get(IconId::ActionClose).key
    );
}
#[test]
fn invalid_initial_assets_use_default_without_disabling_other_ids() {
    let dir = tempfile::tempdir().unwrap();
    let pack = example(dir.path(), "");
    fs::write(pack.join("icons/close.svg"), "<svg><script/></svg>").unwrap();
    let registry = ThemeRegistry::new(dir.path());
    let (selected, notes) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    assert!(!notes.is_empty());
    assert_eq!(
        selected.get(IconId::ActionClose).key,
        builtin().get(IconId::ActionClose).key
    );
    assert_ne!(
        selected.get(IconId::NavSearch).key,
        builtin().get(IconId::NavSearch).key
    );
}
#[test]
fn wrong_polarity_missing_invalid_and_duplicate_packs_fall_back() {
    let dir = tempfile::tempdir().unwrap();
    let pack = example(dir.path(), "appearance = \"dark\"");
    let registry = ThemeRegistry::new(dir.path());
    for id in ["missing", "canopy-lines"] {
        let (selected, notes) = registry.select_icons(id, SystemAppearance::Light);
        assert_eq!(selected.id, DEFAULT);
        assert!(!notes.is_empty());
    }
    let duplicate = dir.path().join("icon-themes/duplicate");
    fs::create_dir_all(&duplicate).unwrap();
    fs::copy(
        pack.join("icon-theme.toml"),
        duplicate.join("icon-theme.toml"),
    )
    .unwrap();
    assert!(registry.rescan());
    assert_eq!(registry.icon_snapshot().disabled.len(), 2);
    assert_eq!(
        registry
            .select_icons("canopy-lines", SystemAppearance::Dark)
            .0
            .id,
        DEFAULT
    );
    fs::remove_dir_all(&duplicate).unwrap();
    fs::write(
        pack.join("icon-theme.toml"),
        "schema_version=2\nid='canopy-lines'",
    )
    .unwrap();
    registry.rescan();
    assert_eq!(registry.icon_snapshot().disabled.len(), 1);
}
#[test]
fn unsafe_asset_paths_and_nonregular_files_are_refused() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let pack = example(dir.path(), "");
    let manifest = pack.join("icon-theme.toml");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(dir.path().join("outside.svg"), SAFE).unwrap();
    symlink(dir.path().join("outside.svg"), pack.join("icons/link.svg")).unwrap();
    fs::create_dir(pack.join("icons/directory.svg")).unwrap();
    for path in [
        "../outside.svg",
        "/etc/passwd",
        "icons/link.svg",
        "icons/directory.svg",
        "icons/a/b/c/d/e.svg",
        "file://evil",
        "icons\\evil.svg",
    ] {
        let escaped = path.replace('\\', "\\\\");
        fs::write(&manifest, text.replace("icons/close.svg", &escaped)).unwrap();
        let registry = ThemeRegistry::new(dir.path());
        let (selected, notes) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
        assert!(!notes.is_empty(), "{path}");
        assert_eq!(
            selected.get(IconId::ActionClose).key,
            builtin().get(IconId::ActionClose).key,
            "{path}"
        );
    }
}
#[test]
fn nested_svg_atomic_save_is_observed_by_the_shared_watcher() {
    let dir = tempfile::tempdir().unwrap();
    let pack = example(dir.path(), "");
    let registry = ThemeRegistry::new(dir.path());
    let _guard = registry.start_watching().unwrap();
    let (old, _) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    fs::write(
        pack.join("icons/new.svg"),
        SAFE.replace("M6 6 18 18M18 6 6 18", "M3 3L19 19"),
    )
    .unwrap();
    fs::rename(pack.join("icons/new.svg"), pack.join("icons/close.svg")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while registry.generation() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let (next, _) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    assert_ne!(
        old.get(IconId::ActionClose).key,
        next.get(IconId::ActionClose).key
    );
}
#[test]
fn registry_retains_only_one_active_snapshot_not_retired_generations() {
    let dir = tempfile::tempdir().unwrap();
    let pack = example(dir.path(), "");
    let registry = ThemeRegistry::new(dir.path());
    let weak = {
        let (old, _) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
        Arc::downgrade(&old)
    };
    for i in 1..20 {
        fs::write(
            pack.join("icons/close.svg"),
            SAFE.replace("M6 6 18 18M18 6 6 18", &format!("M{i} 1L20 20")),
        )
        .unwrap();
        registry.rescan();
        registry.select_icons("canopy-lines", SystemAppearance::Dark);
    }
    assert!(weak.upgrade().is_none());
}
#[test]
fn documented_icon_catalog_cannot_drift() {
    let docs = include_str!("../../../../../docs/themes.md");
    let list = docs
        .split("<!-- icons:start -->")
        .nth(1)
        .unwrap()
        .split("<!-- icons:end -->")
        .next()
        .unwrap();
    let expected = IconId::ALL
        .iter()
        .map(|id| format!("- `{}`\n", id.id()))
        .collect::<String>();
    assert_eq!(list.trim(), expected.trim());
}

#[test]
fn offline_schema_is_generated_from_the_runtime_registry() {
    let committed: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../docs/schemas/icon-theme-v1.json"
    ))
    .unwrap();
    assert_eq!(committed, schema());
}
