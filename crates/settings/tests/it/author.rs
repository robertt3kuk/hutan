//! The authoring helpers behind `hutan theme` and the Settings pickers: listings, validation by
//! id and by path, scaffolds, exports and duplicates, over real files.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use hutan_settings::ThemeOverrides;
use hutan_settings::themes::author::{
    AuthorError, ExportOptions, NewPack, Origin, PackKind, duplicate, export, list_colors,
    new_pack, validate_installed, validate_path,
};
use hutan_settings::themes::icons::IconId;
use hutan_settings::themes::{SystemAppearance, ThemeRegistry};
use tempfile::TempDir;

const SPARSE: &str = "schema_version = 1\nid = \"canopy\"\nname = \"Canopy\"\nauthors = [\"Me\"]\nlicense = \"MIT\"\nlicense_file = \"LICENSE\"\nversion = \"1.2.0\"\nappearance = \"dark\"\nextends = \"builtin:hutan-dark\"\n\n# keep this comment\n[colors]\n\"ui.accent\" = \"#d99a58\"\n\"terminal.selection.foreground\" = \"#ffffff\"\n\"state.success\" = \"#55cc88\"\n\n[metrics]\n\"terminal.font-size\" = 14\n\n[provenance]\nsource = \"hand made\"\n";

fn icon_manifest(id: &str, body: &str) -> String {
    format!(
        "schema_version = 1\nid = \"{id}\"\nname = \"{id}\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nextends = \"builtin:phosphor\"\n\n[icons]\n{body}\n"
    )
}

const CLOSE: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"><path stroke=\"currentColor\" fill=\"none\" d=\"M6 6 18 18M18 6 6 18\"/></svg>";

/// An icon pack fixture: id, manifest text, extra files.
type IconCase<'a> = (&'a str, String, Vec<(&'a str, &'a str)>);

fn data() -> TempDir {
    tempfile::tempdir().unwrap()
}

fn install_color(data: &Path, id: &str, text: &str, license: Option<&str>) -> PathBuf {
    let dir = data.join("themes").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("theme.toml"), text).unwrap();
    if let Some(license) = license {
        fs::write(dir.join("LICENSE"), license).unwrap();
    }
    dir
}

fn install_icons(data: &Path, id: &str, manifest: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = data.join("icon-themes").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("icon-theme.toml"), manifest).unwrap();
    for (rel, text) in files {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    dir
}

fn resolved(registry: &ThemeRegistry, id: &str) -> hutan_settings::themes::ResolvedTheme {
    (**registry.snapshot().pack(id).unwrap().resolved()).clone()
}

/// Problems other than contrast advice (the fixture's accent is deliberately pale).
fn problems(registry: &ThemeRegistry) -> Vec<String> {
    registry
        .snapshot()
        .diagnostics()
        .iter()
        .filter(|d| d.key_path != "contrast")
        .map(ToString::to_string)
        .collect()
}

fn leftovers(parent: &Path) -> Vec<String> {
    fs::read_dir(parent)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect()
}

// ---- export ------------------------------------------------------------------------------------

#[test]
fn a_plain_export_keeps_the_manifest_comments_provenance_and_license_and_resolves_equal() {
    let source = data();
    install_color(source.path(), "canopy", SPARSE, Some("MIT text\n"));
    let registry = ThemeRegistry::new(source.path());
    let out = data();
    let written = export(
        PackKind::Color,
        &registry,
        "canopy",
        &out.path().join("themes"),
        &ExportOptions::default(),
    )
    .unwrap();
    assert_eq!(written.files, ["theme.toml", "LICENSE"]);
    assert_eq!(fs::read_to_string(&written.manifest).unwrap(), SPARSE);
    assert_eq!(
        fs::read_to_string(written.dir.join("LICENSE")).unwrap(),
        "MIT text\n"
    );
    let reread = ThemeRegistry::new(out.path());
    assert_eq!(problems(&reread), Vec::<String>::new());
    assert_eq!(resolved(&reread, "canopy"), resolved(&registry, "canopy"));
}

#[test]
fn a_resolved_export_is_a_complete_root_pack_that_resolves_to_the_same_theme() {
    let source = data();
    install_color(source.path(), "canopy", SPARSE, Some("MIT text\n"));
    let registry = ThemeRegistry::new(source.path());
    let out = data();
    let written = export(
        PackKind::Color,
        &registry,
        "canopy",
        &out.path().join("themes"),
        &ExportOptions {
            resolved: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    let text = fs::read_to_string(&written.manifest).unwrap();
    assert!(text.contains("root = true"), "{text}");
    assert!(!text.contains("extends"), "{text}");
    assert!(
        text.contains("source = \"hand made\""),
        "provenance kept: {text}"
    );
    assert!(text.contains("exported_from = \"canopy\""), "{text}");
    assert!(written.dir.join("LICENSE").is_file(), "the notice travels");
    let reread = ThemeRegistry::new(out.path());
    assert_eq!(problems(&reread), Vec::<String>::new());
    // The advice about the pale accent travels with the values.
    assert_eq!(
        reread.snapshot().diagnostics().len(),
        registry.snapshot().diagnostics().len()
    );
    let pack = reread.snapshot().pack("canopy").cloned().unwrap();
    assert!(pack.declaration().meta.root);
    assert_eq!(resolved(&reread, "canopy"), resolved(&registry, "canopy"));
    // Derived roles stayed derived: they follow the colors they derive from.
    assert!(!text.contains("diff.added.background"), "{text}");
}

#[test]
fn an_exported_built_in_is_a_user_override_that_resolves_like_the_embedded_one() {
    let empty = data();
    let registry = ThemeRegistry::new(empty.path());
    let out = data();
    for id in ["hutan-dark", "hutan-light"] {
        export(
            PackKind::Color,
            &registry,
            id,
            &out.path().join("themes"),
            &ExportOptions::default(),
        )
        .unwrap();
    }
    let reread = ThemeRegistry::new(out.path());
    for id in ["hutan-dark", "hutan-light"] {
        assert_eq!(resolved(&reread, id), resolved(&registry, id), "{id}");
    }
    let listing = list_colors(&reread.snapshot());
    assert!(listing.iter().all(|p| p.origin == Origin::UserOverride));
}

#[test]
fn a_resolved_export_lays_the_users_overrides_of_the_pack_over_it() {
    let source = data();
    install_color(source.path(), "canopy", SPARSE, Some("x"));
    let registry = ThemeRegistry::new(source.path());
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "canopy".to_owned(),
        ThemeOverrides {
            colors: BTreeMap::from([("ui.accent".to_owned(), "#112233".to_owned())]),
        },
    );
    let out = data();
    let written = export(
        PackKind::Color,
        &registry,
        "canopy",
        &out.path().join("themes"),
        &ExportOptions {
            resolved: true,
            overrides: Some(&overrides),
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert!(written.notes.iter().any(|n| n.contains("1 color override")));
    let text = fs::read_to_string(&written.manifest).unwrap();
    assert!(text.contains("\"ui.accent\" = \"#112233\""), "{text}");
}

#[test]
fn exports_and_scaffolds_refuse_to_replace_and_leave_no_partial_pack() {
    let source = data();
    install_color(source.path(), "canopy", SPARSE, Some("x"));
    let registry = ThemeRegistry::new(source.path());
    let out = data();
    let themes = out.path().join("themes");
    fs::create_dir_all(themes.join("canopy")).unwrap();
    fs::write(themes.join("canopy/mine.txt"), "mine").unwrap();
    let error = export(
        PackKind::Color,
        &registry,
        "canopy",
        &themes,
        &ExportOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(&error, AuthorError::Failed(m) if m.contains("already exists")),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(themes.join("canopy/mine.txt")).unwrap(),
        "mine"
    );
    assert!(!themes.join("canopy/theme.toml").exists());
    assert!(leftovers(&themes).is_empty(), "{:?}", leftovers(&themes));

    let error = new_pack(
        &NewPack {
            kind: PackKind::Color,
            id: "canopy",
            name: "Canopy",
            author: "me",
            license: "MIT",
            extends: None,
        },
        &themes,
    )
    .unwrap_err();
    assert!(matches!(&error, AuthorError::Failed(m) if m.contains("already exists")));
    assert!(leftovers(&themes).is_empty());

    // A symlink in the way is a pack that exists too: nothing is written through it.
    let elsewhere = data();
    std::os::unix::fs::symlink(elsewhere.path(), themes.join("linked")).unwrap();
    new_pack(
        &NewPack {
            kind: PackKind::Color,
            id: "linked",
            name: "Linked",
            author: "me",
            license: "MIT",
            extends: None,
        },
        &themes,
    )
    .unwrap_err();
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn an_icon_export_carries_assets_and_notices_and_the_resolved_form_is_complete() {
    let source = data();
    install_icons(
        source.path(),
        "lines",
        &icon_manifest(
            "lines",
            "\"action.close\" = \"icons/close.svg\"\n\"nav.search\" = \"icons/close.svg\"",
        ),
        &[("icons/close.svg", CLOSE)],
    );
    let registry = ThemeRegistry::new(source.path());
    assert!(registry.icon_snapshot().diagnostics.is_empty());

    // Plain: manifest and the one shared asset, once.
    let plain = data();
    let written = export(
        PackKind::Icon,
        &registry,
        "lines",
        &plain.path().join("icon-themes"),
        &ExportOptions::default(),
    )
    .unwrap();
    assert_eq!(written.files, ["icon-theme.toml", "icons/close.svg"]);
    assert_eq!(
        fs::read_to_string(written.dir.join("icons/close.svg")).unwrap(),
        CLOSE
    );

    // Resolved: a root pack covering every id, with the Phosphor notice beside its artwork.
    let full = data();
    let written = export(
        PackKind::Icon,
        &registry,
        "lines",
        &full.path().join("icon-themes"),
        &ExportOptions {
            resolved: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert!(written.dir.join("LICENSE-phosphor").is_file());
    assert!(written.dir.join("PROVENANCE-phosphor").is_file());
    let manifest = fs::read_to_string(&written.manifest).unwrap();
    assert!(
        manifest.contains("root = true") && !manifest.contains("extends"),
        "{manifest}"
    );
    assert!(manifest.contains("(MIT) AND MIT") || manifest.contains("license = \"MIT\""));
    let reread = ThemeRegistry::new(full.path());
    let catalog = reread.icon_snapshot();
    assert!(catalog.diagnostics.is_empty(), "{:?}", catalog.diagnostics);
    let pack = catalog
        .packs
        .iter()
        .find(|p| p.declaration.meta.id == "lines")
        .unwrap();
    assert!(pack.declaration.meta.root);
    assert_eq!(pack.declaration.icons.len(), IconId::COUNT);
    // Every icon draws what it drew before the export.
    let (before, _) = registry.select_icons("lines", SystemAppearance::Dark);
    let (after, _) = reread.select_icons("lines", SystemAppearance::Dark);
    for id in IconId::ALL {
        assert_eq!(before.get(*id).bytes, after.get(*id).bytes, "{}", id.id());
    }
}

#[test]
fn duplicating_picks_a_free_id_and_a_copy_name_and_the_copy_is_valid() {
    let source = data();
    install_color(source.path(), "canopy", SPARSE, Some("x"));
    let registry = ThemeRegistry::new(source.path());
    let (first, written) = duplicate(PackKind::Color, &registry, "canopy").unwrap();
    assert_eq!(first, "canopy-copy");
    assert!(written.dir.ends_with("themes/canopy-copy"));
    registry.rescan();
    let (second, _) = duplicate(PackKind::Color, &registry, "canopy").unwrap();
    assert_eq!(second, "canopy-copy-2");
    registry.rescan();
    let snapshot = registry.snapshot();
    assert_eq!(problems(&registry), Vec::<String>::new());
    let copy = snapshot.pack("canopy-copy").unwrap();
    assert_eq!(copy.descriptor.name, "Canopy (copy)");
    // The comments of the source survive the rename.
    let text = fs::read_to_string(written.manifest).unwrap();
    assert!(text.contains("# keep this comment"), "{text}");
    assert!(text.contains("\"ui.accent\" = \"#d99a58\""));

    // A built-in becomes an editable root copy.
    let (copy_id, written) = duplicate(PackKind::Color, &registry, "hutan-light").unwrap();
    assert_eq!(copy_id, "hutan-light-copy");
    assert!(
        fs::read_to_string(written.manifest)
            .unwrap()
            .contains("root = true")
    );
    let (icon_copy, _) = duplicate(PackKind::Icon, &registry, "phosphor").unwrap();
    assert_eq!(icon_copy, "phosphor-copy");
    registry.rescan();
    assert!(
        registry
            .icon_snapshot()
            .packs
            .iter()
            .any(|p| p.declaration.meta.id == "phosphor-copy")
    );
    assert!(registry.icon_snapshot().diagnostics.is_empty());
}

// ---- new -------------------------------------------------------------------------------------------

#[test]
fn a_new_pack_of_either_kind_validates_without_a_single_diagnostic() {
    let home = data();
    let registry_dir = home.path();
    for (kind, extends) in [
        (PackKind::Color, None),
        (PackKind::Color, Some("builtin:hutan-light")),
        (PackKind::Icon, None),
    ] {
        let id = format!("fresh-{}-{}", kind.name(), extends.map_or("d", |_| "l"));
        let written = new_pack(
            &NewPack {
                kind,
                id: &id,
                name: "Fresh \"quoted\" name",
                author: "A. Author",
                license: "MIT OR Apache-2.0",
                extends,
            },
            &registry_dir.join(kind.folder()),
        )
        .unwrap();
        let verdict = validate_path(kind, &written.dir).unwrap();
        assert_eq!(verdict.id.as_deref(), Some(id.as_str()));
        assert!(verdict.diagnostics.is_empty(), "{:?}", verdict.diagnostics);
    }
    let registry = ThemeRegistry::new(home.path());
    assert!(registry.snapshot().diagnostics().is_empty());
    assert!(registry.icon_snapshot().diagnostics.is_empty());
    let light = registry.snapshot().pack("fresh-color-l").cloned().unwrap();
    assert_eq!(light.descriptor.appearance, SystemAppearance::Light);
    let icons = registry.icon_snapshot();
    let pack = icons
        .packs
        .iter()
        .find(|p| p.declaration.meta.id == "fresh-icon-d")
        .unwrap();
    assert_eq!(pack.declaration.icons.len(), 1);
}

#[test]
fn a_new_pack_with_invalid_input_is_a_usage_error_and_writes_nothing() {
    let home = data();
    let parent = home.path().join("themes");
    let base = NewPack {
        kind: PackKind::Color,
        id: "ok",
        name: "Ok",
        author: "me",
        license: "MIT",
        extends: None,
    };
    for spec in [
        NewPack {
            id: "Bad_Id",
            ..base
        },
        NewPack { id: "auto", ..base },
        NewPack {
            name: "bad\nname",
            ..base
        },
        NewPack {
            extends: Some("builtin:nope"),
            ..base
        },
        NewPack {
            extends: Some("builtin:phosphor"),
            ..base
        },
        NewPack {
            kind: PackKind::Icon,
            extends: Some("builtin:hutan-dark"),
            ..base
        },
    ] {
        let error = new_pack(&spec, &parent).unwrap_err();
        assert!(matches!(error, AuthorError::Usage(_)), "{spec:?}: {error}");
    }
    assert!(!parent.exists(), "nothing was created");
}

// ---- validate ------------------------------------------------------------------------------------------

#[test]
fn validating_an_icon_folder_gives_the_diagnostics_the_catalog_gives() {
    let home = data();
    let cases: [IconCase; 5] = [
        (
            "good",
            icon_manifest("good", "\"action.close\" = \"c.svg\""),
            vec![("c.svg", CLOSE)],
        ),
        (
            "bad-svg",
            icon_manifest("bad-svg", "\"action.close\" = \"c.svg\""),
            vec![("c.svg", "<svg viewBox=\"0 0 24 24\"><script/></svg>")],
        ),
        (
            "missing",
            icon_manifest("missing", "\"action.close\" = \"gone.svg\""),
            vec![],
        ),
        (
            "escape",
            icon_manifest("escape", "\"action.close\" = \"../good/c.svg\""),
            vec![],
        ),
        (
            "no-license",
            icon_manifest("no-license", "\"action.close\" = \"c.svg\"").replacen(
                "\n[icons]",
                "\nlicense_file = \"NOTICE\"\n\n[icons]",
                1,
            ),
            vec![("c.svg", CLOSE)],
        ),
    ];
    for (id, manifest, files) in &cases {
        install_icons(home.path(), id, manifest, files);
    }
    let registry = ThemeRegistry::new(home.path());
    for (id, ..) in &cases {
        let by_id = validate_installed(PackKind::Icon, &registry, id);
        let by_path = validate_path(PackKind::Icon, &home.path().join("icon-themes").join(id));
        let (Ok(by_id), Ok(by_path)) = (by_id, by_path) else {
            panic!("{id}: both validate");
        };
        let strip = |v: &hutan_settings::themes::author::Validation| {
            v.diagnostics
                .iter()
                .map(|d| (d.severity, d.key_path.clone(), d.message.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(strip(&by_id), strip(&by_path), "{id}");
    }
    let bad = validate_path(PackKind::Icon, &home.path().join("icon-themes/bad-svg")).unwrap();
    assert!(
        bad.diagnostics
            .iter()
            .any(|d| d.key_path == "icons.action.close"),
        "{:?}",
        bad.diagnostics
    );
    assert_eq!(
        bad.errors(),
        0,
        "a broken asset falls back per id: a warning"
    );
    let escape = validate_path(PackKind::Icon, &home.path().join("icon-themes/escape")).unwrap();
    assert!(escape.diagnostics.iter().any(|d| d.message.contains("..")));
}

#[test]
fn validation_names_the_other_kind_when_the_target_is_of_that_kind() {
    let home = data();
    let color = install_color(home.path(), "canopy", SPARSE, Some("x"));
    let icons = install_icons(
        home.path(),
        "lines",
        &icon_manifest("lines", "\"action.close\" = \"c.svg\""),
        &[("c.svg", CLOSE)],
    );
    let wrong = validate_path(PackKind::Icon, &color).unwrap_err();
    assert!(wrong.to_string().contains("--kind color"), "{wrong}");
    let wrong = validate_path(PackKind::Color, &icons).unwrap_err();
    assert!(wrong.to_string().contains("--kind icon"), "{wrong}");
    let registry = ThemeRegistry::new(home.path());
    let wrong = validate_installed(PackKind::Color, &registry, "lines").unwrap_err();
    assert!(wrong.to_string().contains("--kind icon"), "{wrong}");
    let wrong = validate_installed(PackKind::Icon, &registry, "nope").unwrap_err();
    assert!(
        wrong.to_string().contains("no icon theme `nope`"),
        "{wrong}"
    );
}

#[test]
fn validating_by_id_finds_a_pack_that_never_loaded_by_its_folder() {
    let home = data();
    install_color(
        home.path(),
        "broken",
        "schema_version = 1\nid = \"broken\"\nname = \"B\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nappearance = \"dark\"\nextends = \"builtin:hutan-dark\"\n[colors]\n\"ui.border\" = \"oops\"\n",
        None,
    );
    let registry = ThemeRegistry::new(home.path());
    let verdict = validate_installed(PackKind::Color, &registry, "broken").unwrap();
    assert_eq!(verdict.errors(), 1);
    let error = &verdict.diagnostics[0];
    assert_eq!(error.key_path, "colors.ui.border");
    assert!(
        error
            .file
            .as_ref()
            .unwrap()
            .ends_with("themes/broken/theme.toml")
    );
    let listing = list_colors(&registry.snapshot());
    let broken = listing.iter().find(|p| !p.valid).unwrap();
    assert_eq!(broken.name, "broken");
    assert_eq!(broken.errors().count(), 1);
}

#[test]
fn a_license_that_is_not_an_spdx_expression_or_is_unknown_is_a_warning() {
    let home = data();
    for (id, license, warns) in [
        ("fine", "MIT OR Apache-2.0", false),
        (
            "parens",
            "(MIT AND BSD-3-Clause) OR GPL-2.0-or-later WITH Classpath-exception-2.0",
            false,
        ),
        ("ref", "LicenseRef-Mine", false),
        ("unknown", "LicenseRef-Unknown", true),
        ("prose", "All rights reserved", true),
        ("dangling", "MIT OR", true),
    ] {
        let text = SPARSE
            .replace("id = \"canopy\"", &format!("id = \"{id}\""))
            .replace("license = \"MIT\"", &format!("license = \"{license}\""))
            .replace("license_file = \"LICENSE\"\n", "");
        install_color(home.path(), id, &text, None);
        let verdict =
            validate_installed(PackKind::Color, &ThemeRegistry::new(home.path()), id).unwrap();
        let noted = verdict.diagnostics.iter().any(|d| d.key_path == "license");
        assert_eq!(noted, warns, "{id}: {:?}", verdict.diagnostics);
        assert_eq!(verdict.errors(), 0);
    }
}
