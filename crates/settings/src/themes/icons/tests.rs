use super::*;
use crate::error::Severity;
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

#[test]
fn builtin_small_artwork_is_phosphor_bold_and_chosen_by_size() {
    let base = builtin();
    let (declaration, _) = parse(BUILTIN, None);
    let declaration = declaration.unwrap();
    assert!(
        declaration.small.len() > 100,
        "the Phosphor-drawn ids have a Bold file"
    );
    for (&id, rel) in &declaration.small {
        assert!(rel.starts_with("bold/"), "{}: {rel}", id.id());
        let small = base.small(id).expect("a declared small entry resolves");
        assert!(svg::sanitize(&small.bytes).is_ok(), "{}", id.id());
        assert_ne!(
            small.bytes,
            base.get(id).bytes,
            "{}: small differs from regular",
            id.id()
        );
    }
    // The threshold is inclusive: 14 points is small, 15 is not.
    let id = IconId::NavProjectFolder;
    let (regular, small) = (base.get(id), base.small(id).unwrap());
    assert!(Arc::ptr_eq(base.get_sized(id, 12.0), small));
    assert!(Arc::ptr_eq(base.get_sized(id, SMALL_MAX_PX), small));
    assert!(Arc::ptr_eq(base.get_sized(id, SMALL_MAX_PX + 0.5), regular));
    assert!(Arc::ptr_eq(base.get_sized(id, 16.0), regular));
    // Bold's stroke is heavier: its folder outline path is not Regular's.
    assert_ne!(small.key, regular.key);
    // The harness monograms are our own filled glyphs: no Bold twin, one artwork at every size.
    for id in [
        IconId::HarnessOmp,
        IconId::HarnessClaude,
        IconId::HarnessCodex,
    ] {
        assert!(base.small(id).is_none(), "{}", id.id());
        assert!(Arc::ptr_eq(base.get_sized(id, 12.0), base.get(id)));
    }
}

#[test]
fn a_pack_that_replaces_an_id_without_small_artwork_shows_it_at_every_size() {
    let dir = tempfile::tempdir().unwrap();
    example(dir.path(), "");
    let registry = ThemeRegistry::new(dir.path());
    let (selected, notes) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    assert!(notes.is_empty(), "{notes:?}");
    let base = builtin();
    // `nav.search` is replaced: no Phosphor Bold under the pack's own icon at 12 px.
    assert!(selected.small(IconId::NavSearch).is_none());
    assert!(Arc::ptr_eq(
        selected.get_sized(IconId::NavSearch, 12.0),
        selected.get(IconId::NavSearch)
    ));
    // An id it inherits keeps the built-in pack's small artwork, and so no key changes.
    assert_eq!(
        selected.small(IconId::VcsBranch).map(|a| &a.key),
        base.small(IconId::VcsBranch).map(|a| &a.key)
    );
}

#[test]
fn a_packs_small_table_is_used_up_to_fourteen_points_and_falls_back_per_id() {
    let dir = tempfile::tempdir().unwrap();
    let pack = example(dir.path(), "");
    fs::write(
        pack.join("icons/tiny.svg"),
        SAFE.replace("M6 6 18 18M18 6 6 18", "M3 3L9 9"),
    )
    .unwrap();
    let file = pack.join("icon-theme.toml");
    let manifest = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        format!(
            "{manifest}\n[small]\n\"action.close\" = \"icons/tiny.svg\"\n\"nav.search\" = \"icons/gone.svg\"\n\"future.icon\" = \"x.svg\"\n"
        ),
    )
    .unwrap();
    let registry = ThemeRegistry::new(dir.path());
    let diagnostics = &registry.icon_snapshot().diagnostics;
    assert!(
        diagnostics
            .iter()
            .any(|d| d.key_path == "small.nav.search" && d.severity == Severity::Warning),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.key_path == "small.future.icon"),
        "{diagnostics:?}"
    );
    let (selected, _) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    let close = IconId::ActionClose;
    assert!(
        std::str::from_utf8(&selected.get_sized(close, 12.0).bytes)
            .unwrap()
            .contains("M3 3L9 9")
    );
    assert!(
        std::str::from_utf8(&selected.get_sized(close, 16.0).bytes)
            .unwrap()
            .contains("M6 6 18 18M18 6 6 18")
    );
    // The small file of `nav.search` is missing: its regular file serves every size.
    assert!(selected.small(IconId::NavSearch).is_none());
    assert!(Arc::ptr_eq(
        selected.get_sized(IconId::NavSearch, 12.0),
        selected.get(IconId::NavSearch)
    ));

    // An edit to the small file changes the served artwork and its key.
    fs::write(
        pack.join("icons/tiny.svg"),
        SAFE.replace("M6 6 18 18M18 6 6 18", "M4 4L10 10"),
    )
    .unwrap();
    assert!(registry.rescan());
    let (next, _) = registry.select_icons("canopy-lines", SystemAppearance::Dark);
    assert_ne!(
        selected.small(close).unwrap().key,
        next.small(close).unwrap().key
    );
    assert_eq!(selected.get(close).key, next.get(close).key);
}

#[test]
fn a_root_pack_without_small_artwork_has_none_at_any_size() {
    let dir = tempfile::tempdir().unwrap();
    let pack = dir.path().join("icon-themes/flat");
    fs::create_dir_all(&pack).unwrap();
    fs::write(pack.join("a.svg"), SAFE).unwrap();
    let mut text = String::from(
        "schema_version = 1\nid = \"flat\"\nname = \"Flat\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nroot = true\n\n[icons]\n",
    );
    for id in IconId::ALL {
        text.push_str(&format!("\"{}\" = \"a.svg\"\n", id.id()));
    }
    fs::write(pack.join("icon-theme.toml"), text).unwrap();
    let registry = ThemeRegistry::new(dir.path());
    let (selected, notes) = registry.select_icons("flat", SystemAppearance::Dark);
    assert!(notes.is_empty(), "{notes:?}");
    for &id in IconId::ALL {
        assert!(selected.small(id).is_none(), "{}", id.id());
    }
    assert_eq!(selected.assets().count(), IconId::COUNT);
}
