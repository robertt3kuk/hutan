//! Color packs over real files: the embedded built-ins, discovery, resolution, selection,
//! confinement, and hot reload with the real OS watcher.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hutan_settings::themes::{
    self, Builtin, Color, ColorOverrides, DensityKey, FontRole, Metric, OverrideMemory, PackSource,
    Provenance, SelectionRequest, SystemAppearance, ThemeCatalogSnapshot, ThemeRegistry,
    builtin_theme,
};
use hutan_settings::{Density, ThemeOverrides};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(15);

// ---- fixtures -------------------------------------------------------------------------------

fn sparse(id: &str, appearance: &str, colors: &str) -> String {
    format!(
        "schema_version = 1\nid = \"{id}\"\nname = \"{id}\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nappearance = \"{appearance}\"\nextends = \"builtin:hutan-{appearance}\"\n\n[colors]\n{colors}\n"
    )
}

fn write_pack(data: &Path, id: &str, text: &str) {
    let dir = data.join("themes").join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("theme.toml"), text).unwrap();
}

/// Atomic save: write next to the target, then rename over it.
fn save_atomically(data: &Path, id: &str, text: &str) {
    let dir = data.join("themes").join(id);
    let tmp = dir.join(".theme.toml.tmp");
    fs::write(&tmp, text).unwrap();
    fs::rename(&tmp, dir.join("theme.toml")).unwrap();
}

fn registry() -> (TempDir, ThemeRegistry) {
    let dir = tempfile::tempdir().unwrap();
    let registry = ThemeRegistry::new(dir.path());
    (dir, registry)
}

fn select(
    registry: &ThemeRegistry,
    polarity: SystemAppearance,
    light: &str,
    dark: &str,
    overrides: &BTreeMap<String, ThemeOverrides>,
) -> themes::Selected {
    let snapshot = registry.snapshot();
    registry.select(
        &snapshot,
        SelectionRequest {
            polarity,
            light_theme: light,
            dark_theme: dark,
            overrides,
        },
    )
}

fn hex(theme: &themes::ResolvedTheme, token: Color) -> String {
    theme.color(token).to_string()
}

fn ids(snapshot: &ThemeCatalogSnapshot) -> Vec<&str> {
    snapshot
        .packs()
        .iter()
        .map(|p| p.descriptor.id.as_str())
        .collect()
}

// ---- built-ins ------------------------------------------------------------------------------

fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.snapshot.txt"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{} is missing; run with UPDATE_GOLDEN=1", path.display()));
    assert_eq!(
        actual, expected,
        "{name} changed; review the diff and run with UPDATE_GOLDEN=1 if intended"
    );
}

#[test]
fn the_resolved_builtins_match_their_golden_snapshots() {
    golden("hutan-dark", &builtin_theme(Builtin::HutanDark).snapshot());
    golden(
        "hutan-light",
        &builtin_theme(Builtin::HutanLight).snapshot(),
    );
}

/// The palettes the desktop and the terminal view used to hard-code, as literals independent of
/// the embedded TOML.
#[test]
fn the_builtins_keep_every_value_the_old_literals_had() {
    let chrome = |theme: &themes::ResolvedTheme, expected: [u32; 16]| {
        let tokens = [
            Color::Background,
            Color::SidebarBackground,
            Color::SidebarSelected,
            Color::SidebarRightBackground,
            Color::SidebarRightSelected,
            Color::Surface,
            Color::SurfaceHover,
            Color::Border,
            Color::Foreground,
            Color::TextMuted,
            Color::TextFaint,
            Color::Accent,
            Color::AccentSoft,
            Color::StateSuccess,
            Color::StateWarning,
            Color::StateError,
        ];
        for (token, want) in tokens.into_iter().zip(expected) {
            let got = theme.color(token).rgb();
            assert_eq!(
                (u32::from(got.r) << 16) | (u32::from(got.g) << 8) | u32::from(got.b),
                want,
                "{}",
                token.id()
            );
        }
    };
    let dark = builtin_theme(Builtin::HutanDark);
    let light = builtin_theme(Builtin::HutanLight);
    chrome(
        &light,
        [
            0xFAF9F5, 0xF3F1EA, 0xE9E6DC, 0xF3F1EA, 0xE9E6DC, 0xFFFFFF, 0xF0EEE6, 0xE6E3D9,
            0x1F1E1D, 0x66645D, 0xA8A59C, 0xE0692E, 0xFBE9DF, 0x2F7A48, 0x8A6300, 0xB93A26,
        ],
    );
    // The dark hairline is white; its alpha is checked below.
    chrome(
        &dark,
        [
            0x262624, 0x1F1E1D, 0x2E2D2B, 0x1F1E1D, 0x2E2D2B, 0x30302E, 0x3A3936, 0xFFFFFF,
            0xF5F4EF, 0xAEACA4, 0x6F6D66, 0xEA7A43, 0x3E2C23, 0x6BBF85, 0xE2AE45, 0xF08C7A,
        ],
    );
    // Alpha quantized to 8 bits: within half a step of the old 0.07 / 0.40 / 0.50 / 0.12 / 0.28.
    let near = |got: f32, want: f32| (got - want).abs() <= 0.5 / 255.0 + 1e-6;
    assert!(near(dark.color(Color::Border).a, 0.07));
    assert!(near(dark.color(Color::Shadow).a, 0.40));
    assert!(near(dark.color(Color::Scrim).a, 0.50));
    assert!(near(light.color(Color::Shadow).a, 0.12));
    assert!(near(light.color(Color::Scrim).a, 0.28));
    assert_eq!(light.color(Color::Border).a, 1.0);
    assert_eq!(
        dark.color(Color::OnAccent).rgb(),
        themes::Rgb::new(0x1f, 0x1e, 0x1d)
    );
    assert_eq!(
        light.color(Color::OnAccent).rgb(),
        themes::Rgb::new(0x1f, 0x1e, 0x1d)
    );
    for (theme, shadow) in [(&dark, Color::Shadow), (&light, Color::Shadow)] {
        let c = theme.color(shadow);
        assert_eq!((c.r, c.g, c.b), (0, 0, 0));
    }

    // Derived roles keep the exact old fractions.
    assert_eq!(dark.color(Color::SidebarHover).a, 0.08);
    assert_eq!(dark.color(Color::DropTarget).a, 0.28);
    assert_eq!(light.color(Color::DropTarget).a, 0.22);
    assert_eq!(light.color(Color::DiffAddedBackground).a, 0.16);
    assert_eq!(light.color(Color::DiffHunkBackground).a, 0.10);
    assert_eq!(dark.color(Color::BlockHighlight).a, 0.13);
    assert_eq!(dark.color(Color::BlockHover).a, 0.07);
    assert_eq!(
        dark.color(Color::DiffAdded),
        dark.color(Color::StateSuccess)
    );
    assert_eq!(dark.color(Color::DiffModified), dark.color(Color::Accent));
    assert_eq!(
        dark.color(Color::StateBlocked),
        dark.color(Color::StateWarning)
    );

    // Terminal: every value of the two old tables.
    let t = |theme: &themes::ResolvedTheme| theme.terminal();
    let dt = t(&dark);
    assert_eq!(dt.name, "hutan-dark");
    assert_eq!(dt.foreground.to_string(), "#eae8e1");
    assert_eq!(dt.background.to_string(), "#262624");
    assert_eq!(dt.cursor.to_string(), "#ea7a43");
    assert_eq!(dt.selection.to_string(), "#4b3b30");
    assert_eq!(
        dt.ansi.map(|c| c.to_string()),
        [
            "#4a4944", "#e87562", "#6bbf85", "#e2ae45", "#72a5ec", "#c996d8", "#5fc2c6", "#d6d4cc",
            "#7a7870", "#f08c7a", "#86d19c", "#eec261", "#8fb9f2", "#daaee6", "#7dd3d6", "#f5f4ef",
        ]
    );
    let lt = t(&light);
    assert_eq!(lt.foreground.to_string(), "#1f1e1d");
    assert_eq!(lt.background.to_string(), "#faf9f5");
    assert_eq!(lt.cursor.to_string(), "#e0692e");
    assert_eq!(lt.selection.to_string(), "#f3d5c3");
    assert_eq!(
        lt.ansi.map(|c| c.to_string()),
        [
            "#1f1e1d", "#b93a26", "#2f7a48", "#8a6300", "#2b62b8", "#8e4ea8", "#167780", "#6f6d66",
            "#57554f", "#c8452f", "#3f8f5a", "#9a6f00", "#3d77d1", "#a35fbf", "#23909a", "#8c8a82",
        ]
    );
    // No selection foreground, no declared cursor glyph or fills: the old behavior.
    assert_eq!(dt.selection_foreground, None);
    assert_eq!(dt.cursor_foreground, None);
    assert_eq!(dt.blocks.separator, None);
    assert_eq!(dt.blocks.success, dark.color(Color::StateSuccess).rgb());
    assert_eq!(dt.blocks.neutral, dark.color(Color::TextMuted).rgb());
    // Polarity and the terminal background equals the chrome background in the built-ins.
    assert_eq!(dark.appearance, SystemAppearance::Dark);
    assert_eq!(
        hex(&dark, Color::TerminalBackground),
        hex(&dark, Color::Background)
    );
    assert_eq!(
        hex(&light, Color::TerminalBackground),
        hex(&light, Color::Background)
    );
}

#[test]
fn the_builtins_carry_today_s_density_metrics_and_fonts() {
    for builtin in Builtin::ALL {
        let theme = builtin_theme(builtin);
        let want = |density: Density| match density {
            Density::Compact => [
                26.0, 30.0, 30.0, 24.0, 4.0, 6.0, 10.0, 12.0, 11.0, 11.0, 15.0, 1.6, 28.0, 24.0,
                8.0, 3.0, 42.0, 8.0, 16.0, 8.0, 13.0, 10.0, 9.5,
            ],
            Density::Comfortable => [
                30.0, 34.0, 36.0, 24.0, 6.0, 6.0, 10.0, 13.0, 12.0, 11.0, 15.0, 1.6, 28.0, 24.0,
                12.0, 4.0, 48.0, 8.0, 16.0, 8.0, 13.0, 10.0, 9.5,
            ],
        };
        for density in [Density::Compact, Density::Comfortable] {
            let got: Vec<f32> = DensityKey::ALL
                .iter()
                .map(|k| theme.density(density).get(*k))
                .collect();
            assert_eq!(got, want(density), "{} {density:?}", builtin.id());
        }
        let metrics: Vec<f32> = Metric::ALL.iter().map(|m| theme.metric(*m)).collect();
        assert_eq!(
            metrics,
            [
                36.0, 999.0, 720.0, 96.0, 13.0, 16.0, 18.0, 8.0, 6.0, 2.0, 28.0, 4.0, 12.0, 8.0,
                28.0, 16.0, 0.45, 13.0, 1.2, 0.0, 8.0, 24.0, 0.0, 16.0, 0.075, 14.0, 8.0
            ]
        );
        assert_eq!(theme.fonts().role(FontRole::Ui), [".SystemUIFont"]);
        // One family app-wide but for code: the prose face is the UI face unless a pack says
        // otherwise (FX-E2E4).
        assert_eq!(theme.fonts().role(FontRole::Prose), [".SystemUIFont"]);
        assert_eq!(theme.fonts().role(FontRole::Mono), ["Menlo"]);
    }
}

#[test]
fn the_builtins_contrast_baseline_is_reviewed() {
    // The warnings the built-ins themselves have, as a reviewed baseline (spike §3.8): they are
    // reported, not suppressed, and a user pack is only blamed for what it makes worse.
    for builtin in Builtin::ALL {
        let issues = themes::contrast_issues(&builtin_theme(builtin));
        let pairs: Vec<String> = issues
            .iter()
            .map(|i| format!("{} on {}", i.foreground, i.background))
            .collect();
        let expected: &[&str] = match builtin {
            Builtin::HutanDark => &[],
            Builtin::HutanLight => &[],
        };
        assert_eq!(pairs, expected, "{}", builtin.id());
    }
}

#[test]
fn the_current_find_match_is_clearly_stronger_than_the_others_in_both_builtins() {
    // FX-E2E5: on the light paper every match was a pale tan and the current one hardly
    // different. The current match must stand out from the others and from the background,
    // composed over the background the terminal and the editor draw them on.
    for builtin in Builtin::ALL {
        let theme = builtin_theme(builtin);
        let ground = theme.color(Color::EditorBackground).rgb();
        let every = theme.color(Color::EditorSearchMatch).over(ground);
        let current = theme.color(Color::EditorSearchMatchActive).over(ground);
        let apart = themes::contrast_ratio(every, current);
        let from_ground = themes::contrast_ratio(current, ground);
        assert!(
            apart >= 1.25,
            "{}: the current match vs the others is {apart:.2}:1",
            builtin.id()
        );
        assert!(
            from_ground >= 1.8,
            "{}: the current match vs the background is {from_ground:.2}:1",
            builtin.id()
        );
    }
}

// ---- editor and syntax tokens (E-4) ---------------------------------------------------------

#[test]
fn the_syntax_tokens_are_exactly_the_names_of_the_syntax_crates_tokens() {
    use hutan_syntax::Token;
    let registry: Vec<&str> = Color::SYNTAX.iter().map(|c| c.id()).collect();
    let tokens: Vec<&str> = Token::ALL.iter().map(|t| t.name()).collect();
    assert_eq!(registry, tokens, "same names in the same order");
    // Nothing else in the registry claims the namespace.
    let claimed = themes::COLOR_SPECS
        .iter()
        .filter(|s| s.id.starts_with("syntax."))
        .count();
    assert_eq!(claimed, Token::ALL.len());
    // The resolved table is indexed by the token's discriminant.
    for builtin in Builtin::ALL {
        let theme = builtin_theme(builtin);
        let editor = theme.editor();
        for token in Token::ALL {
            let id = Color::from_id(token.name()).unwrap();
            assert_eq!(editor.syntax[token as usize], theme.color(id).rgb());
        }
    }
}

fn without_syntax(builtin: Builtin) -> themes::ResolvedTheme {
    let file = match builtin {
        Builtin::HutanDark => "hutan-dark",
        Builtin::HutanLight => "hutan-light",
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/themes")
        .join(file)
        .join("theme.toml");
    let text: String = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("\"syntax.") && !line.starts_with("\"editor."))
        .map(|line| format!("{line}\n"))
        .collect();
    let parsed = themes::parse(&text, Some(&path));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    themes::resolve(
        &parsed.declaration.unwrap(),
        None,
        &ColorOverrides::default(),
    )
    .unwrap()
}

#[test]
fn a_pack_without_editor_or_syntax_colors_derives_all_of_them() {
    for builtin in Builtin::ALL {
        let theme = without_syntax(builtin);
        let mut seen = 0;
        for spec in &themes::COLOR_SPECS {
            if !(spec.id.starts_with("editor.") || spec.id.starts_with("syntax.")) {
                continue;
            }
            seen += 1;
            assert_eq!(
                theme.provenance(spec.token),
                Provenance::Derived,
                "{}",
                spec.id
            );
            assert!(
                theme.color(spec.token).is_opaque() || !spec.opaque,
                "{} must stay opaque",
                spec.id
            );
        }
        assert_eq!(seen, 22 + 22);
        let at = |id: &str| theme.color(Color::from_id(id).unwrap());
        for (derived, source) in [
            ("editor.background", "ui.background"),
            ("editor.foreground", "ui.foreground"),
            ("editor.gutter.background", "ui.background"),
            ("editor.line-number.active", "ui.foreground"),
            ("editor.cursor", "ui.accent"),
            ("editor.diagnostic.error", "state.error"),
            ("editor.diagnostic.warning", "state.warning"),
            ("editor.diagnostic.info", "ui.accent"),
            ("editor.diagnostic.hint", "ui.text.muted"),
            ("editor.diff-gutter.added", "diff.added"),
            ("editor.diff-gutter.modified", "diff.modified"),
            ("editor.diff-gutter.removed", "diff.removed"),
            ("syntax.comment", "ui.text.muted"),
            ("syntax.string", "terminal.ansi.green"),
            ("syntax.keyword", "terminal.ansi.magenta"),
            ("syntax.function", "terminal.ansi.blue"),
            ("syntax.function.method", "syntax.function"),
            ("syntax.variable.parameter", "editor.foreground"),
            ("syntax.punctuation.bracket", "ui.text.muted"),
        ] {
            assert_eq!(at(derived), at(source), "{derived} <- {source}");
        }
        // The tints follow what they are tinted from.
        let accent = at("ui.accent");
        for tint in [
            "editor.selection",
            "editor.bracket-match",
            "editor.agent-changed",
        ] {
            let c = at(tint);
            assert_eq!((c.r, c.g, c.b), (accent.r, accent.g, accent.b), "{tint}");
            assert!(c.a > 0.0 && c.a < 1.0, "{tint}");
        }
        assert_eq!(
            at("editor.selection.inactive").a,
            at("editor.selection").a * 0.5
        );
        // And the derived syntax colors are readable on the editor background, like the
        // palette they come from.
        let issues = themes::contrast_issues(&theme);
        assert!(
            !issues
                .iter()
                .any(|i| i.foreground.starts_with("syntax.") || i.foreground.starts_with("editor.")),
            "{issues:?}"
        );
    }
}

#[test]
fn a_sparse_pack_gets_a_complete_editor_that_follows_its_core_colors() {
    let (dir, registry) = registry();
    write_pack(
        dir.path(),
        "paper-night",
        &sparse(
            "paper-night",
            "dark",
            "\"ui.background\" = \"#101418\"\n\"state.error\" = \"#ff5555\"\n\"diff.added\" = \"#44dd66\"",
        ),
    );
    registry.rescan();
    let snapshot = registry.snapshot();
    let theme = snapshot.pack("paper-night").unwrap().resolved();
    let editor = theme.editor();
    assert_eq!(editor.name, "paper-night");
    assert_eq!(editor.background.to_string(), "#101418");
    assert_eq!(editor.gutter_background.to_string(), "#101418");
    assert_eq!(editor.diagnostic_error.to_string(), "#ff5555");
    assert_eq!(editor.diff_added.to_string(), "#44dd66");
    // The base's syntax colors are inherited; no `unknown token` or `unsupported` anywhere.
    let dark = builtin_theme(Builtin::HutanDark);
    assert_eq!(editor.syntax, dark.editor().syntax);
    assert!(
        snapshot
            .diagnostics()
            .iter()
            .all(|d| !d.message.contains("unknown") && !d.message.contains("unsupported")),
        "{:?}",
        snapshot.diagnostics()
    );
}

#[test]
fn the_builtins_syntax_colors_meet_aa_on_the_editor_background() {
    for builtin in Builtin::ALL {
        let theme = builtin_theme(builtin);
        let background = theme
            .color(Color::EditorBackground)
            .over(theme.color(Color::Background).rgb());
        for token in Color::SYNTAX {
            let ratio = themes::contrast_ratio(theme.color(token).rgb(), background);
            assert!(
                ratio >= 4.5,
                "{} {}: {ratio:.2}:1",
                builtin.id(),
                token.id()
            );
        }
        let ratio = themes::contrast_ratio(theme.color(Color::EditorForeground).rgb(), background);
        assert!(
            ratio >= 4.5,
            "{} editor.foreground: {ratio:.2}:1",
            builtin.id()
        );
        assert!(
            themes::contrast_issues(&theme)
                .iter()
                .all(|i| i.background != "editor.background"),
            "{}",
            builtin.id()
        );
    }
}

#[test]
fn a_syntax_color_that_is_hard_to_read_is_reported_as_contrast_advice() {
    let (dir, registry) = registry();
    write_pack(
        dir.path(),
        "dim-comments",
        &sparse("dim-comments", "dark", "\"syntax.comment\" = \"#2e2e2c\""),
    );
    registry.rescan();
    let snapshot = registry.snapshot();
    let diagnostics = snapshot.diagnostics();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(!diagnostics[0].is_error());
    assert!(
        diagnostics[0]
            .message
            .starts_with("syntax.comment on editor.background: contrast"),
        "{}",
        diagnostics[0].message
    );
    // Hot reload is unchanged: the pack is loaded and usable.
    assert!(snapshot.pack("dim-comments").is_some());
}

// ---- loading --------------------------------------------------------------------------------

#[test]
fn a_missing_or_empty_themes_folder_leaves_the_builtins() {
    let (dir, registry) = registry();
    assert_eq!(ids(&registry.snapshot()), ["hutan-dark", "hutan-light"]);
    assert!(registry.snapshot().diagnostics().is_empty());
    fs::create_dir(dir.path().join("themes")).unwrap();
    assert!(!registry.rescan(), "an empty folder changes nothing");
    assert_eq!(registry.generation(), 0);
}

#[test]
fn sparse_packs_extend_a_builtin_and_override_single_tokens() {
    let (dir, registry) = registry();
    write_pack(
        dir.path(),
        "canopy-night",
        &sparse(
            "canopy-night",
            "dark",
            "\"ui.accent\" = \"#d99a58\"\n\"ui.on-accent\" = \"#ffffff\"",
        ),
    );
    assert!(registry.rescan());
    let snapshot = registry.snapshot();
    assert_eq!(
        ids(&snapshot),
        ["hutan-dark", "hutan-light", "canopy-night"]
    );
    let pack = snapshot.pack("canopy-night").unwrap();
    let dark = builtin_theme(Builtin::HutanDark);
    let theme = pack.resolved();
    assert_eq!(hex(theme, Color::Accent), "#d99a58");
    assert_eq!(hex(theme, Color::Background), hex(&dark, Color::Background));
    assert_eq!(theme.terminal().ansi, dark.terminal().ansi);
    assert_eq!(theme.provenance(Color::Accent), Provenance::Declared);
    assert!(matches!(pack.descriptor.source, PackSource::User(_)));
    // The only thing to say: this pack keeps white text on that accent, which the built-in's
    // dark ink does not.
    let diagnostics = snapshot.diagnostics();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(!diagnostics[0].is_error());
    assert!(
        diagnostics[0]
            .message
            .starts_with("ui.on-accent on ui.accent")
    );
}

#[test]
fn state_diff_and_block_roles_follow_the_core_colors_they_derive_from() {
    let (dir, registry) = registry();
    write_pack(
        dir.path(),
        "greener",
        &sparse(
            "greener",
            "dark",
            "\"state.success\" = \"#00ff00\"\n\"terminal.background\" = \"#000000\"\n\"terminal.foreground\" = \"#ffffff\"",
        ),
    );
    registry.rescan();
    let snapshot = registry.snapshot();
    let theme = snapshot.pack("greener").unwrap().resolved();
    assert_eq!(hex(theme, Color::DiffAdded), "#00ff00");
    assert_eq!(hex(theme, Color::StateCompleted), "#00ff00");
    assert_eq!(hex(theme, Color::BlockSuccess), "#00ff00");
    let tint = theme.color(Color::DiffAddedBackground);
    assert_eq!((tint.r, tint.g, tint.b, tint.a), (0, 255, 0, 0.16));
    assert_eq!(theme.provenance(Color::DiffAdded), Provenance::Derived);
    // The separator mixes the pack's own terminal colors: 16 % of the way to white.
    assert_eq!(hex(theme, Color::BlockSeparator), "#292929");
    // Derived fills stay `None` for the terminal view, which derives them from live colors.
    assert_eq!(theme.terminal().blocks.separator, None);
}

#[test]
fn an_explicit_optional_role_wins_and_reaches_the_terminal() {
    let (dir, registry) = registry();
    write_pack(
        dir.path(),
        "explicit",
        &sparse(
            "explicit",
            "dark",
            "\"terminal.selection.foreground\" = \"#ffffff\"\n\"terminal.cursor.foreground\" = \"#101010\"\n\"diff.added\" = \"#123456\"\n\"terminal.block.separator\" = \"#ffffff30\"",
        ),
    );
    registry.rescan();
    let snapshot = registry.snapshot();
    let theme = snapshot.pack("explicit").unwrap().resolved();
    assert_eq!(hex(theme, Color::DiffAdded), "#123456");
    assert_eq!(hex(theme, Color::DiffAddedBackground), "#12345629");
    let terminal = theme.terminal();
    assert_eq!(
        terminal.selection_foreground.unwrap().to_string(),
        "#ffffff"
    );
    assert_eq!(terminal.cursor_foreground.unwrap().to_string(), "#101010");
    assert_eq!(terminal.blocks.separator.unwrap().to_string(), "#ffffff30");
}

#[test]
fn a_root_pack_is_complete_and_extends_nothing() {
    let (dir, registry) = registry();
    let dark = include_str!("../../../../assets/themes/hutan-dark/theme.toml");
    let root = dark
        .replace("id = \"hutan-dark\"", "id = \"my-root\"")
        .replace("\"#ea7a43\"", "\"#00aaff\"");
    write_pack(dir.path(), "my-root", &root);
    // A root pack missing one required token is refused, naming it.
    let broken = root
        .replace("id = \"my-root\"", "id = \"broken-root\"")
        .replace("\"ui.surface\" = \"#30302e\"\n", "");
    write_pack(dir.path(), "broken-root", &broken);
    registry.rescan();
    let snapshot = registry.snapshot();
    let pack = snapshot.pack("my-root").unwrap();
    assert_eq!(hex(pack.resolved(), Color::Accent), "#00aaff");
    assert!(snapshot.pack("broken-root").is_none());
    assert!(
        snapshot
            .diagnostics()
            .iter()
            .any(|d| d.is_error() && d.key_path == "colors" && d.message.contains("ui.surface")),
        "{:?}",
        snapshot.diagnostics()
    );
    assert_eq!(snapshot.disabled().len(), 1);
}

#[test]
fn invalid_packs_are_listed_disabled_with_their_errors_and_never_panic() {
    let (dir, registry) = registry();
    write_pack(dir.path(), "garbage", "not toml at all [[[");
    write_pack(
        dir.path(),
        "future",
        "schema_version = 2\nid = \"future\"\n",
    );
    write_pack(
        dir.path(),
        "bad-color",
        &sparse("bad-color", "dark", "\"ui.border\" = \"red\""),
    );
    fs::create_dir_all(dir.path().join("themes/empty-folder")).unwrap();
    fs::write(dir.path().join("themes/notes.txt"), "ignored").unwrap();
    fs::write(dir.path().join("themes/.DS_Store"), "ignored").unwrap();
    registry.rescan();
    let snapshot = registry.snapshot();
    assert_eq!(ids(&snapshot), ["hutan-dark", "hutan-light"]);
    assert_eq!(snapshot.disabled().len(), 4);
    let text: Vec<String> = snapshot
        .diagnostics()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(text.iter().any(|t| t.contains("invalid TOML")), "{text:?}");
    assert!(
        text.iter()
            .any(|t| t.contains("unsupported schema_version 2")),
        "{text:?}"
    );
    assert!(
        text.iter()
            .any(|t| t.contains("colors.ui.border: expected #rrggbb[aa]")),
        "{text:?}"
    );
    assert!(
        text.iter()
            .any(|t| t.contains("empty-folder") && t.contains("theme.toml")),
        "{text:?}"
    );
    assert!(snapshot.diagnostics().iter().all(|d| d.file.is_some()));
}

#[test]
fn a_single_file_pack_works_and_a_duplicate_id_disables_every_claimant() {
    let (dir, registry) = registry();
    let themes = dir.path().join("themes");
    fs::create_dir_all(&themes).unwrap();
    fs::write(
        themes.join("solo.toml"),
        sparse("solo", "light", "\"ui.accent\" = \"#336699\""),
    )
    .unwrap();
    write_pack(dir.path(), "twin-a", &sparse("twin", "dark", ""));
    fs::write(themes.join("twin-b.toml"), sparse("twin", "dark", "")).unwrap();
    registry.rescan();
    let snapshot = registry.snapshot();
    assert_eq!(ids(&snapshot), ["hutan-dark", "hutan-light", "solo"]);
    let errors: Vec<_> = snapshot
        .diagnostics()
        .iter()
        .filter(|d| d.is_error())
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    for e in errors {
        assert!(
            e.message.contains("twin-a") && e.message.contains("twin-b.toml"),
            "{e}"
        );
    }
    assert_eq!(snapshot.disabled().len(), 2);
    // Removing one claimant lets the other load.
    fs::remove_file(themes.join("twin-b.toml")).unwrap();
    registry.rescan();
    assert!(registry.snapshot().pack("twin").is_some());
}

#[test]
fn a_user_pack_may_shadow_a_builtin_and_still_extend_the_embedded_one() {
    let (dir, registry) = registry();
    write_pack(
        dir.path(),
        "hutan-dark",
        &sparse("hutan-dark", "dark", "\"ui.accent\" = \"#ffa0ff\""),
    );
    registry.rescan();
    let snapshot = registry.snapshot();
    assert_eq!(ids(&snapshot), ["hutan-dark", "hutan-light"]);
    let pack = snapshot.pack("hutan-dark").unwrap();
    assert!(pack.descriptor.overrides_builtin);
    assert_eq!(hex(pack.resolved(), Color::Accent), "#ffa0ff");
    // `builtin:` always means the immutable embedded content: no cycle, no recursion.
    assert_eq!(
        hex(pack.resolved(), Color::Background),
        hex(&builtin_theme(Builtin::HutanDark), Color::Background)
    );
    assert_eq!(
        hex(&builtin_theme(Builtin::HutanDark), Color::Accent),
        "#ea7a43"
    );
    assert!(
        snapshot.diagnostics().is_empty(),
        "{:?}",
        snapshot.diagnostics()
    );
}

// ---- confinement ----------------------------------------------------------------------------

#[test]
fn symlinks_are_refused_for_packs_manifests_and_the_folder() {
    let (dir, registry) = registry();
    let themes = dir.path().join("themes");
    fs::create_dir_all(&themes).unwrap();
    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("theme.toml"), sparse("stolen", "dark", "")).unwrap();
    // A pack folder that is a symlink, a manifest that is a symlink, a single file that is one.
    symlink(&outside, themes.join("linked-dir")).unwrap();
    fs::create_dir_all(themes.join("linked-manifest")).unwrap();
    symlink(
        outside.join("theme.toml"),
        themes.join("linked-manifest/theme.toml"),
    )
    .unwrap();
    symlink(outside.join("theme.toml"), themes.join("linked-file.toml")).unwrap();
    registry.rescan();
    let snapshot = registry.snapshot();
    assert_eq!(
        ids(&snapshot),
        ["hutan-dark", "hutan-light"],
        "nothing was loaded through a link"
    );
    let refused = snapshot
        .diagnostics()
        .iter()
        .filter(|d| d.is_error() && d.message.contains("symlink"))
        .count();
    assert_eq!(refused, 3, "{:?}", snapshot.diagnostics());

    // The folder itself.
    let (dir, registry) = self::registry();
    let real = dir.path().join("elsewhere");
    fs::create_dir_all(real.join("p")).unwrap();
    fs::write(real.join("p/theme.toml"), sparse("p", "dark", "")).unwrap();
    symlink(&real, dir.path().join("themes")).unwrap();
    registry.rescan();
    let snapshot = registry.snapshot();
    assert_eq!(ids(&snapshot), ["hutan-dark", "hutan-light"]);
    assert!(
        snapshot
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("symlink"))
    );
}

#[test]
fn license_file_paths_are_confined() {
    let (dir, registry) = registry();
    let text = |path: &str| {
        sparse("licensed", "dark", "")
            .replace("[colors]", &format!("license_file = \"{path}\"\n[colors]"))
    };
    write_pack(dir.path(), "licensed", &text("LICENSE"));
    fs::write(dir.path().join("themes/licensed/LICENSE"), "MIT").unwrap();
    registry.rescan();
    assert!(registry.snapshot().pack("licensed").is_some());
    assert!(registry.snapshot().diagnostics().is_empty());
    for bad in ["../outside", "/etc/passwd", "~/x", "a\\\\b"] {
        save_atomically(dir.path(), "licensed", &text(bad));
        registry.rescan();
        let snapshot = registry.snapshot();
        assert!(
            snapshot
                .diagnostics()
                .iter()
                .any(|d| d.is_error() && d.key_path == "license_file"),
            "{bad}: {:?}",
            snapshot.diagnostics()
        );
        // The earlier version stays: the bad edit is an error, not a removal.
        assert!(
            snapshot.pack("licensed").unwrap().descriptor.kept_previous,
            "{bad}"
        );
    }
    // A symlinked license file is refused.
    let link = dir.path().join("themes/licensed/LICENSE");
    fs::remove_file(&link).unwrap();
    symlink(dir.path().join("settings.toml"), &link).unwrap();
    save_atomically(dir.path(), "licensed", &text("LICENSE"));
    registry.rescan();
    assert!(
        registry
            .snapshot()
            .diagnostics()
            .iter()
            .any(|d| d.is_error() && d.message.contains("symlink"))
    );
}

#[test]
fn caps_bound_the_scan_and_say_so() {
    let (dir, registry) = registry();
    let themes = dir.path().join("themes");
    fs::create_dir_all(&themes).unwrap();
    for i in 0..themes::MAX_CANDIDATES + 20 {
        fs::write(themes.join(format!("p{i:04}.toml")), "x = 1").unwrap();
    }
    registry.rescan();
    let snapshot = registry.snapshot();
    let diagnostics = snapshot.diagnostics();
    // One error per examined candidate, at most the cap + a summary, all visible.
    assert!(diagnostics.len() <= themes::MAX_DIAGNOSTICS + 1);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("20 more were not read")),
        "{:?}",
        diagnostics.last()
    );
    assert_eq!(ids(&snapshot).len(), 2);

    // A manifest over the size limit is refused unread.
    let (dir, registry) = self::registry();
    let big = format!(
        "{}\n# {}\n",
        sparse("big", "dark", ""),
        "x".repeat(themes::MAX_MANIFEST_BYTES)
    );
    write_pack(dir.path(), "big", &big);
    registry.rescan();
    let snapshot = registry.snapshot();
    assert!(snapshot.pack("big").is_none());
    assert!(
        snapshot
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("the limit is"))
    );
}

#[test]
fn a_flood_of_diagnostics_is_truncated_with_a_summary() {
    let (dir, registry) = registry();
    // One pack with more problems than the cap: every token wrong.
    let mut colors = String::new();
    for i in 0..themes::MAX_DIAGNOSTICS + 50 {
        colors.push_str(&format!("\"ui.unknown-{i}\" = \"#000000\"\n"));
    }
    write_pack(dir.path(), "noisy", &sparse("noisy", "dark", &colors));
    registry.rescan();
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.diagnostics().len(), themes::MAX_DIAGNOSTICS + 1);
    assert!(
        snapshot
            .diagnostics()
            .last()
            .unwrap()
            .message
            .contains("more theme diagnostics")
    );
    assert!(
        snapshot.pack("noisy").is_some(),
        "warnings do not disable a pack"
    );
}

// ---- selection ------------------------------------------------------------------------------

#[test]
fn light_and_dark_slots_are_independent_and_follow_the_polarity() {
    let (dir, registry) = registry();
    write_pack(
        dir.path(),
        "canopy-night",
        &sparse("canopy-night", "dark", "\"ui.accent\" = \"#d99a58\""),
    );
    write_pack(
        dir.path(),
        "paper",
        &sparse("paper", "light", "\"ui.accent\" = \"#336699\""),
    );
    registry.rescan();
    let none = BTreeMap::new();
    let dark = select(
        &registry,
        SystemAppearance::Dark,
        "paper",
        "canopy-night",
        &none,
    );
    assert_eq!(dark.theme.id, "canopy-night");
    let light = select(
        &registry,
        SystemAppearance::Light,
        "paper",
        "canopy-night",
        &none,
    );
    assert_eq!(light.theme.id, "paper");
    assert!(dark.diagnostics.is_empty() && light.diagnostics.is_empty());
    // Pinning the mode picks that slot, not a theme by its declared appearance.
    let pinned = select(
        &registry,
        SystemAppearance::Dark,
        "hutan-light",
        "hutan-dark",
        &none,
    );
    assert_eq!(pinned.theme.id, "hutan-dark");
}

#[test]
fn a_missing_or_wrong_polarity_selection_falls_back_visibly_and_recovers() {
    let (dir, registry) = registry();
    let none = BTreeMap::new();
    let missing = select(
        &registry,
        SystemAppearance::Dark,
        "hutan-light",
        "canopy-night",
        &none,
    );
    assert_eq!(missing.theme.id, "hutan-dark");
    assert_eq!(missing.requested, "canopy-night", "the request is retained");
    assert_eq!(missing.diagnostics.len(), 1);
    assert_eq!(missing.diagnostics[0].key_path, "appearance.dark_theme");
    assert!(missing.diagnostics[0].message.contains("not installed"));

    write_pack(
        dir.path(),
        "canopy-night",
        &sparse("canopy-night", "dark", ""),
    );
    registry.rescan();
    let found = select(
        &registry,
        SystemAppearance::Dark,
        "hutan-light",
        "canopy-night",
        &none,
    );
    assert_eq!(found.theme.id, "canopy-night");
    assert!(found.diagnostics.is_empty());

    // A dark pack in the light slot is refused with a reason.
    let wrong = select(
        &registry,
        SystemAppearance::Light,
        "canopy-night",
        "hutan-dark",
        &none,
    );
    assert_eq!(wrong.theme.id, "hutan-light");
    assert!(wrong.diagnostics[0].message.contains("dark theme"));
}

#[test]
fn terminal_pinning_picks_only_the_terminal_section() {
    let (dir, registry) = registry();
    write_pack(
        dir.path(),
        "canopy-night",
        &sparse(
            "canopy-night",
            "dark",
            "\"ui.accent\" = \"#d99a58\"\n\"terminal.background\" = \"#101820\"",
        ),
    );
    registry.rescan();
    let none = BTreeMap::new();
    let snapshot = registry.snapshot();
    let active = select(
        &registry,
        SystemAppearance::Dark,
        "hutan-light",
        "canopy-night",
        &none,
    )
    .theme;
    let terminal = |selector: &str| registry.terminal(&snapshot, selector, &active, &none);

    // auto: the active theme's own terminal section.
    let (auto, diagnostics) = terminal("auto");
    assert_eq!(auto.background.to_string(), "#101820");
    assert!(diagnostics.is_empty());
    // A built-in id under a dark custom chrome: the light palette, the chrome unchanged.
    let (light, _) = terminal("hutan-light");
    assert_eq!(light.background.to_string(), "#faf9f5");
    assert_eq!(active.id, "canopy-night");
    // A user pack's id picks its terminal section.
    let (named, _) = terminal("canopy-night");
    assert_eq!(named.background.to_string(), "#101820");
    // The old built-in ids keep working without a compatibility resolver.
    let (dark, _) = terminal("hutan-dark");
    assert_eq!(dark.background.to_string(), "#262624");
    // An unknown id falls back to auto, with a warning that names it.
    let (unknown, diagnostics) = terminal("nope");
    assert_eq!(unknown.background.to_string(), "#101820");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].key_path, "terminal.theme");
}

#[test]
fn user_overrides_apply_and_a_bad_edit_keeps_the_previous_accepted_ones() {
    let (_dir, registry) = registry();
    let mut overrides = BTreeMap::new();
    let set = |overrides: &mut BTreeMap<String, ThemeOverrides>, pairs: &[(&str, &str)]| {
        overrides.insert(
            "hutan-dark".to_owned(),
            ThemeOverrides {
                colors: pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            },
        );
    };
    set(&mut overrides, &[("ui.accent", "#00ff00")]);
    let selected = select(
        &registry,
        SystemAppearance::Dark,
        "hutan-light",
        "hutan-dark",
        &overrides,
    );
    assert_eq!(hex(&selected.theme, Color::Accent), "#00ff00");
    assert!(selected.diagnostics.is_empty());
    // Derived roles follow the overridden core color.
    assert_eq!(hex(&selected.theme, Color::DiffModified), "#00ff00");
    // The base built-in is untouched.
    assert_eq!(
        hex(&builtin_theme(Builtin::HutanDark), Color::Accent),
        "#ea7a43"
    );

    set(&mut overrides, &[("ui.accent", "not a color")]);
    let kept = select(
        &registry,
        SystemAppearance::Dark,
        "hutan-light",
        "hutan-dark",
        &overrides,
    );
    assert_eq!(
        hex(&kept.theme, Color::Accent),
        "#00ff00",
        "the previous accepted override stays"
    );
    assert!(kept.diagnostics.iter().any(|d| d.is_error()));
    assert!(
        kept.diagnostics
            .iter()
            .any(|d| d.message.contains("previous accepted"))
    );

    // Removing the overrides returns the pack's own colors.
    overrides.clear();
    let clean = select(
        &registry,
        SystemAppearance::Dark,
        "hutan-light",
        "hutan-dark",
        &overrides,
    );
    assert_eq!(hex(&clean.theme, Color::Accent), "#ea7a43");
    assert!(
        Arc::ptr_eq(&clean.theme, &builtin_theme(Builtin::HutanDark)),
        "no copy without overrides"
    );

    // Overrides do not poison the underlying pack: a bad first edit just applies nothing.
    let mut memory = OverrideMemory::default();
    let (parsed, diagnostics) = ColorOverrides::parse(
        &BTreeMap::from([("ui.accent".to_owned(), "#fff".to_owned())]),
        "k",
    );
    assert!(parsed.is_empty() && diagnostics.iter().any(|d| d.is_error()));
    let snapshot = registry.snapshot();
    let bad = BTreeMap::from([(
        "hutan-dark".to_owned(),
        ThemeOverrides {
            colors: BTreeMap::from([("ui.accent".to_owned(), "#fff".to_owned())]),
        },
    )]);
    let selected = snapshot.select(
        SelectionRequest {
            polarity: SystemAppearance::Dark,
            light_theme: "hutan-light",
            dark_theme: "hutan-dark",
            overrides: &bad,
        },
        &mut memory,
    );
    assert_eq!(hex(&selected.theme, Color::Accent), "#ea7a43");
}

// ---- hot reload -----------------------------------------------------------------------------

async fn until(
    registry: &ThemeRegistry,
    what: &str,
    mut cond: impl FnMut(&ThemeCatalogSnapshot) -> bool,
) {
    let mut rx = registry.changes();
    let result = tokio::time::timeout(WAIT, async {
        loop {
            if cond(&registry.snapshot()) {
                return;
            }
            rx.changed().await.unwrap();
        }
    })
    .await;
    assert!(result.is_ok(), "timed out waiting for: {what}");
}

fn accent_of(snapshot: &ThemeCatalogSnapshot, id: &str) -> Option<String> {
    snapshot
        .pack(id)
        .map(|p| hex(p.resolved(), Color::SidebarHover))
}

#[tokio::test]
async fn editing_adding_breaking_fixing_and_removing_a_pack_is_seen_live() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ThemeRegistry::new(dir.path());
    let _watch = registry.start_watching().unwrap();
    let first = registry.generation();

    // The folder does not exist yet: the data dir is watched and the creation is seen.
    write_pack(
        dir.path(),
        "live",
        &sparse("live", "dark", "\"ui.sidebar.hover\" = \"#111111\""),
    );
    until(&registry, "the new pack", |s| {
        accent_of(s, "live").as_deref() == Some("#111111")
    })
    .await;
    assert!(registry.generation() > first);

    // An atomic save (write + rename) is an edit.
    save_atomically(
        dir.path(),
        "live",
        &sparse("live", "dark", "\"ui.sidebar.hover\" = \"#222222\""),
    );
    until(&registry, "the atomic save", |s| {
        accent_of(s, "live").as_deref() == Some("#222222")
    })
    .await;

    // An invalid edit keeps the last good frame and reports why.
    let before = registry.snapshot();
    save_atomically(
        dir.path(),
        "live",
        &sparse("live", "dark", "\"ui.sidebar.hover\" = \"nope\""),
    );
    until(&registry, "the diagnostic", |s| {
        s.diagnostics().iter().any(|d| d.is_error())
    })
    .await;
    let broken = registry.snapshot();
    assert_eq!(
        accent_of(&broken, "live").as_deref(),
        Some("#222222"),
        "last good stays"
    );
    assert!(broken.pack("live").unwrap().descriptor.kept_previous);
    assert!(
        broken
            .diagnostics()
            .iter()
            .any(|d| d.message.contains("kept the previous version"))
    );
    assert!(Arc::ptr_eq(
        before.pack("live").unwrap().resolved(),
        broken.pack("live").unwrap().resolved()
    ));

    // Fixing it recovers.
    save_atomically(
        dir.path(),
        "live",
        &sparse("live", "dark", "\"ui.sidebar.hover\" = \"#333333\""),
    );
    until(&registry, "the fix", |s| {
        accent_of(s, "live").as_deref() == Some("#333333") && s.diagnostics().is_empty()
    })
    .await;
    assert!(
        !registry
            .snapshot()
            .pack("live")
            .unwrap()
            .descriptor
            .kept_previous
    );

    // Removal, then reappearance.
    fs::remove_dir_all(dir.path().join("themes/live")).unwrap();
    until(&registry, "the removal", |s| s.pack("live").is_none()).await;
    write_pack(
        dir.path(),
        "live",
        &sparse("live", "dark", "\"ui.sidebar.hover\" = \"#444444\""),
    );
    until(&registry, "the reappearance", |s| {
        accent_of(s, "live").as_deref() == Some("#444444")
    })
    .await;

    // An invalid pack that was never good cannot be selected.
    write_pack(dir.path(), "never-good", "schema_version = 1\n");
    until(&registry, "the disabled pack", |s| !s.disabled().is_empty()).await;
    assert!(registry.snapshot().pack("never-good").is_none());
}

#[tokio::test]
async fn a_pack_that_is_selected_but_not_installed_recovers_when_it_appears() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ThemeRegistry::new(dir.path());
    let _watch = registry.start_watching().unwrap();
    let none = BTreeMap::new();
    assert_eq!(
        select(
            &registry,
            SystemAppearance::Dark,
            "hutan-light",
            "late",
            &none
        )
        .theme
        .id,
        "hutan-dark"
    );
    write_pack(dir.path(), "late", &sparse("late", "dark", ""));
    until(&registry, "the pack", |s| s.pack("late").is_some()).await;
    assert_eq!(
        select(
            &registry,
            SystemAppearance::Dark,
            "hutan-light",
            "late",
            &none
        )
        .theme
        .id,
        "late"
    );
}

#[tokio::test]
async fn an_inactive_pack_changes_the_picker_but_not_the_active_theme() {
    let dir = tempfile::tempdir().unwrap();
    write_pack(
        dir.path(),
        "other",
        &sparse("other", "dark", "\"ui.sidebar.hover\" = \"#111111\""),
    );
    let registry = ThemeRegistry::new(dir.path());
    let _watch = registry.start_watching().unwrap();
    let none = BTreeMap::new();
    let active = select(
        &registry,
        SystemAppearance::Dark,
        "hutan-light",
        "hutan-dark",
        &none,
    )
    .theme;
    let generation = registry.generation();

    save_atomically(
        dir.path(),
        "other",
        &sparse("other", "dark", "\"ui.sidebar.hover\" = \"#999999\""),
    );
    until(&registry, "the inactive edit", |s| {
        accent_of(s, "other").as_deref() == Some("#999999")
    })
    .await;
    assert!(
        registry.generation() > generation,
        "the catalog (picker, diagnostics) did change"
    );
    let again = select(
        &registry,
        SystemAppearance::Dark,
        "hutan-light",
        "hutan-dark",
        &none,
    )
    .theme;
    assert!(
        Arc::ptr_eq(&active, &again),
        "the active theme is the same snapshot: nothing restyles"
    );
    assert_eq!(*active, *again);
}

#[tokio::test]
async fn rewriting_a_pack_with_the_same_content_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let text = sparse("same", "dark", "\"ui.sidebar.hover\" = \"#111111\"");
    write_pack(dir.path(), "same", &text);
    let registry = ThemeRegistry::new(dir.path());
    let _watch = registry.start_watching().unwrap();
    let generation = registry.generation();
    save_atomically(dir.path(), "same", &text);
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(
        registry.generation(),
        generation,
        "unchanged source, no new generation"
    );
    assert!(!registry.rescan());
}

#[tokio::test]
async fn an_event_flood_coalesces_into_one_consistent_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ThemeRegistry::new(dir.path());
    let _watch = registry.start_watching().unwrap();
    let themes = dir.path().join("themes");
    fs::create_dir_all(&themes).unwrap();
    // Far more events than the bounded queue holds.
    for i in 0..400 {
        fs::write(themes.join(format!("note{i}.txt")), "x").unwrap();
    }
    write_pack(
        dir.path(),
        "after-flood",
        &sparse("after-flood", "dark", ""),
    );
    until(&registry, "the pack after the flood", |s| {
        s.pack("after-flood").is_some()
    })
    .await;
    assert!(registry.snapshot().diagnostics().is_empty());
}

#[tokio::test]
async fn a_second_watcher_is_refused_and_dropping_the_guard_stops_reload() {
    let dir = tempfile::tempdir().unwrap();
    let registry = ThemeRegistry::new(dir.path());
    let guard = registry.start_watching().unwrap();
    assert!(registry.start_watching().is_err());
    drop(guard);
    write_pack(dir.path(), "unseen", &sparse("unseen", "dark", ""));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        registry.snapshot().pack("unseen").is_none(),
        "no watcher, no reload"
    );
    assert!(registry.rescan(), "an explicit rescan still works");
    assert!(registry.snapshot().pack("unseen").is_some());
    // And a watcher can start again.
    let _again = registry.start_watching().unwrap();
}

// ---- structure ------------------------------------------------------------------------------

#[test]
fn the_settings_crate_has_no_gpui_dependency() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(!manifest.contains("gpui"), "Cargo.toml mentions gpui");
    let lock = fs::read_to_string(root.join("../../Cargo.lock")).unwrap();
    let block = lock
        .split("[[package]]")
        .find(|p| p.contains("name = \"hutan-settings\""))
        .expect("hutan-settings is in Cargo.lock");
    assert!(
        !block.contains("gpui"),
        "hutan-settings depends on gpui:\n{block}"
    );
}

// ---- docs -----------------------------------------------------------------------------------

fn fallback_text(fallback: themes::Fallback) -> String {
    use themes::Fallback::{Absent, Alias, Alpha, AlphaByPolarity, Declared, Mix};
    match fallback {
        Declared => "required in a root pack".to_owned(),
        Alias(of) => format!("`{}`", of.id()),
        Alpha(of, a) => format!("`{}` at alpha {a}", of.id()),
        AlphaByPolarity { of, dark, light } => {
            format!("`{}` at alpha {dark} (dark) / {light} (light)", of.id())
        }
        Mix {
            from,
            toward,
            amount,
        } => {
            format!(
                "`{}` mixed {amount} of the way to `{}`",
                from.id(),
                toward.id()
            )
        }
        Absent => "none: not drawn".to_owned(),
    }
}

/// The token reference of `docs/themes.md`, generated from the registry.
fn token_reference() -> String {
    let dark = builtin_theme(Builtin::HutanDark);
    let mut out = String::new();
    out.push_str("#### Colors\n\n| Token | Alpha | If not set | Used for |\n|---|---|---|---|\n");
    for spec in &themes::COLOR_SPECS {
        let alpha = if spec.opaque {
            "opaque"
        } else {
            "may be translucent"
        };
        out.push_str(&format!(
            "| `{}` | {alpha} | {} | {} |\n",
            spec.id,
            fallback_text(spec.fallback),
            spec.doc
        ));
    }
    out.push_str("\n#### Density (`[density.compact]`, `[density.comfortable]`)\n\n| Key | Range | Compact | Comfortable | Used for |\n|---|---|---|---|---|\n");
    for spec in &themes::DENSITY_SPECS {
        let key = DensityKey::from_id(spec.id).unwrap();
        out.push_str(&format!(
            "| `{}` | {} to {} | {} | {} | {} |\n",
            spec.id,
            spec.min,
            spec.max,
            dark.density(Density::Compact).get(key),
            dark.density(Density::Comfortable).get(key),
            spec.doc
        ));
    }
    out.push_str("\n#### Metrics (`[metrics]`)\n\n| Key | Range | Built-in | Used for |\n|---|---|---|---|\n");
    for spec in &themes::METRIC_SPECS {
        let metric = Metric::from_id(spec.id).unwrap();
        out.push_str(&format!(
            "| `{}` | {} to {} | {} | {} |\n",
            spec.id,
            spec.min,
            spec.max,
            dark.metric(metric),
            spec.doc
        ));
    }
    out.push_str("\n#### Fonts (`[fonts]`)\n\n| Key | Built-in | Used for |\n|---|---|---|\n");
    for role in FontRole::ALL {
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            role.id(),
            dark.fonts()
                .role(role)
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", "),
            role.doc()
        ));
    }
    out
}

/// The contrast of every `syntax.*` color (and the editor's default text) on `editor.background`
/// in both built-ins, for `docs/themes.md`.
fn syntax_contrast_table() -> String {
    let themes = [
        builtin_theme(Builtin::HutanDark),
        builtin_theme(Builtin::HutanLight),
    ];
    let mut out = String::from(
        "| Token (`syntax.` prefix) | hutan-dark | Ratio | hutan-light | Ratio |\n|---|---|---|---|---|\n",
    );
    let background = |theme: &themes::ResolvedTheme| {
        theme
            .color(Color::EditorBackground)
            .over(theme.color(Color::Background).rgb())
    };
    let mut row = |label: String, token: Color| {
        out.push_str(&format!("| {label} |"));
        for theme in &themes {
            let color = theme.color(token).rgb();
            let ratio = themes::contrast_ratio(color, background(theme));
            out.push_str(&format!(" `{color}` | {ratio:.2}:1 |"));
        }
        out.push('\n');
    };
    row(
        "default text (`editor.foreground`)".to_owned(),
        Color::EditorForeground,
    );
    for token in Color::SYNTAX {
        let name = token.id().strip_prefix("syntax.").unwrap_or(token.id());
        row(format!("`{name}`"), token);
    }
    let [dark, light] = &themes;
    out.push_str(&format!(
        "\nOn `editor.background`: `{}` (hutan-dark) and `{}` (hutan-light).\n",
        background(dark),
        background(light)
    ));
    out
}

#[test]
fn the_token_reference_in_the_docs_is_generated_from_the_registry() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/themes.md");
    let text = fs::read_to_string(&path).expect("docs/themes.md exists");
    let blocks = [
        (
            "<!-- tokens:begin -->\n",
            "<!-- tokens:end -->",
            token_reference(),
        ),
        (
            "<!-- syntax-contrast:begin -->\n",
            "<!-- syntax-contrast:end -->",
            syntax_contrast_table(),
        ),
    ];
    let mut updated = text.clone();
    for (begin, end, body) in &blocks {
        let from = updated.find(begin).expect("begin marker") + begin.len();
        let to = updated.find(end).expect("end marker");
        let wanted = format!("\n{body}\n");
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            updated = format!("{}{}{}", &updated[..from], wanted, &updated[to..]);
        } else {
            assert_eq!(
                &updated[from..to],
                wanted,
                "docs/themes.md is out of date; run `UPDATE_GOLDEN=1 cargo nextest run -p hutan-settings --test it -E 'test(/^themes::/)'`"
            );
        }
    }
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::write(&path, &updated).unwrap();
        return;
    }
    // Every token id is documented exactly once.
    for spec in &themes::COLOR_SPECS {
        assert_eq!(
            text.matches(&format!("\n| `{}` |", spec.id)).count(),
            1,
            "{}",
            spec.id
        );
    }
}
