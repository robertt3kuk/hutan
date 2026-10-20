#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Color packs in the live workbench (TH-1): editing a user theme changes the chrome, the
//! terminal, the fonts and the density without a restart; an invalid edit keeps the last good
//! frame and says why; each polarity has its own theme; the user's own font settings beat the
//! pack's; a pack that is not in use restyles nothing.

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{offline_connection, open_with, wait_until};
use gpui::{Entity, Rgba, TestAppContext, VisualTestContext};
use hutan_desktop::theme::{Density, DensityExt as _};
use hutan_desktop::workbench::Workbench;
use hutan_settings::SettingsStore;

fn open<'a>(
    cx: &'a mut TestAppContext,
    settings_toml: &str,
) -> (
    Entity<Workbench>,
    SettingsStore,
    PathBuf,
    &'a mut VisualTestContext,
) {
    let (connection, settings, _) = offline_connection();
    let home = settings
        .paths()
        .unwrap()
        .user
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(home.join("settings.toml"), settings_toml).unwrap();
    settings.reload();
    let (view, cx) = open_with(cx, settings.clone(), connection, None);
    (view, settings, home, cx)
}

fn pack_text(id: &str, appearance: &str, body: &str) -> String {
    format!(
        "schema_version = 1\nid = \"{id}\"\nname = \"{id}\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nappearance = \"{appearance}\"\nextends = \"builtin:hutan-{appearance}\"\n{body}\n"
    )
}

fn write_pack(home: &Path, id: &str, text: &str) {
    let dir = home.join("themes").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("theme.toml"), text).unwrap();
}

fn save_atomically(home: &Path, id: &str, text: &str) {
    let dir = home.join("themes").join(id);
    let tmp = dir.join(".theme.toml.tmp");
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, dir.join("theme.toml")).unwrap();
}

fn bg_hex(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> u32 {
    let color: Rgba = view.read_with(cx, |wb, _| wb.theme().bg).into();
    let byte = |v: f32| (v * 255.0).round() as u32;
    (byte(color.r) << 16) | (byte(color.g) << 8) | byte(color.b)
}

fn terminal_bg(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> hutan_terminal_view::Rgb {
    view.read_with(cx, |wb, _| wb.terminal_view_settings().palette.background)
}

fn theme_problems(settings: &SettingsStore) -> Vec<String> {
    settings
        .errors()
        .iter()
        .filter(|e| {
            // Contrast notes are advice, not problems with the theme the test is about.
            e.key_path != "contrast"
                && (e
                    .file
                    .as_ref()
                    .is_some_and(|f| f.components().any(|c| c.as_os_str() == "themes"))
                    || e.key_path.starts_with("appearance.")
                    || e.key_path.starts_with("terminal.theme"))
        })
        .map(ToString::to_string)
        .collect()
}

const CANOPY: &str = "\"ui.background\" = \"#101820\"\n\"terminal.background\" = \"#101820\"\n\n[density.compact]\n\"sidebar.row-height\" = 40\n\n[metrics]\n\"terminal.font-size\" = 17\n";

#[gpui::test]
fn editing_a_user_theme_changes_chrome_terminal_density_and_fonts_without_a_restart(
    cx: &mut TestAppContext,
) {
    let (connection, settings, _) = offline_connection();
    let home = settings
        .paths()
        .unwrap()
        .user
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(
        home.join("settings.toml"),
        "[appearance]\nappearance = \"dark\"\ndark_theme = \"canopy\"\n",
    )
    .unwrap();
    write_pack(
        &home,
        "canopy",
        &pack_text("canopy", "dark", &format!("[colors]\n{CANOPY}")),
    );
    settings.reload();
    let (view, cx) = open_with(cx, settings.clone(), connection, None);

    // Startup: the user's pack is in effect everywhere.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.resolved_theme().id.clone()),
        "canopy"
    );
    assert_eq!(bg_hex(&view, cx), 0x101820);
    assert_eq!(
        terminal_bg(&view, cx),
        hutan_terminal_view::Rgb(0x10, 0x18, 0x20)
    );
    assert_eq!(Density::Compact.ui().sidebar_row, 40.0);
    assert_eq!(Density::Comfortable.ui().sidebar_row, 30.0);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size),
        17.0
    );

    // A live edit (atomic save): colors, terminal palette and density follow.
    save_atomically(
        &home,
        "canopy",
        &pack_text(
            "canopy",
            "dark",
            "[colors]\n\"ui.background\" = \"#202830\"\n\"terminal.background\" = \"#202830\"\n\n[density.compact]\n\"sidebar.row-height\" = 44\n",
        ),
    );
    wait_until(cx, "the edited theme", |cx| bg_hex(&view, cx) == 0x202830);
    assert_eq!(
        terminal_bg(&view, cx),
        hutan_terminal_view::Rgb(0x20, 0x28, 0x30)
    );
    assert_eq!(Density::Compact.ui().sidebar_row, 44.0);
    // The pack no longer sets a terminal font size: the base's 13 applies.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size),
        13.0
    );

    // An invalid edit keeps the last good frame and shows a diagnostic.
    save_atomically(
        &home,
        "canopy",
        &pack_text("canopy", "dark", "[colors]\n\"ui.background\" = \"oops\"\n"),
    );
    wait_until(cx, "the diagnostic", |_| {
        theme_problems(&settings)
            .iter()
            .any(|p| p.contains("colors.ui.background"))
    });
    assert_eq!(bg_hex(&view, cx), 0x202830, "the last good frame stays");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.resolved_theme().id.clone()),
        "canopy"
    );

    // Fixing it recovers and clears the diagnostic.
    save_atomically(
        &home,
        "canopy",
        &pack_text(
            "canopy",
            "dark",
            "[colors]\n\"ui.background\" = \"#303840\"\n",
        ),
    );
    wait_until(cx, "the fix", |cx| bg_hex(&view, cx) == 0x303840);
    wait_until(cx, "the diagnostic to clear", |_| {
        theme_problems(&settings).is_empty()
    });
}

#[gpui::test]
fn each_polarity_uses_its_own_theme_and_switching_the_mode_picks_it(cx: &mut TestAppContext) {
    let (connection, settings, _) = offline_connection();
    let home = settings
        .paths()
        .unwrap()
        .user
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(
        home.join("settings.toml"),
        "[appearance]\nappearance = \"light\"\nlight_theme = \"paper\"\ndark_theme = \"canopy\"\n",
    )
    .unwrap();
    write_pack(
        &home,
        "canopy",
        &pack_text("canopy", "dark", &format!("[colors]\n{CANOPY}")),
    );
    write_pack(
        &home,
        "paper",
        &pack_text(
            "paper",
            "light",
            "[colors]\n\"ui.background\" = \"#eeeeee\"\n",
        ),
    );
    settings.reload();
    let (view, cx) = open_with(cx, settings.clone(), connection, None);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.resolved_theme().id.clone()),
        "paper"
    );
    assert_eq!(bg_hex(&view, cx), 0xeeeeee);
    // `terminal.theme = auto` follows the active chrome pack.
    assert_eq!(
        terminal_bg(&view, cx),
        hutan_terminal_view::Rgb(0xfa, 0xf9, 0xf5)
    );

    settings
        .write_user("appearance.appearance", toml::Value::String("dark".into()))
        .unwrap();
    wait_until(cx, "the dark theme", |cx| bg_hex(&view, cx) == 0x101820);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.resolved_theme().id.clone()),
        "canopy"
    );

    // A light pack in the dark slot falls back to hutan-dark, with a diagnostic that keeps the
    // request; it applies again once the slot is right.
    settings
        .write_user("appearance.dark_theme", toml::Value::String("paper".into()))
        .unwrap();
    wait_until(cx, "the fallback", |cx| bg_hex(&view, cx) == 0x262624);
    assert!(
        theme_problems(&settings)
            .iter()
            .any(|p| p.contains("appearance.dark_theme"))
    );
    settings
        .write_user(
            "appearance.dark_theme",
            toml::Value::String("canopy".into()),
        )
        .unwrap();
    wait_until(cx, "canopy again", |cx| bg_hex(&view, cx) == 0x101820);
    assert!(theme_problems(&settings).is_empty());
}

#[gpui::test]
fn a_selected_theme_that_is_not_installed_appears_when_it_is(cx: &mut TestAppContext) {
    let (view, settings, home, cx) = open(
        cx,
        "[appearance]\nappearance = \"dark\"\ndark_theme = \"late\"\n",
    );
    assert_eq!(bg_hex(&view, cx), 0x262624);
    assert!(
        theme_problems(&settings)
            .iter()
            .any(|p| p.contains("not installed"))
    );
    write_pack(
        &home,
        "late",
        &pack_text(
            "late",
            "dark",
            "[colors]\n\"ui.background\" = \"#112233\"\n",
        ),
    );
    wait_until(cx, "the theme to appear", |cx| {
        bg_hex(&view, cx) == 0x112233
    });
    assert!(theme_problems(&settings).is_empty());
}

#[gpui::test]
fn the_users_terminal_fonts_beat_the_pack_defaults(cx: &mut TestAppContext) {
    let (connection, settings, _) = offline_connection();
    let home = settings
        .paths()
        .unwrap()
        .user
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(
        home.join("settings.toml"),
        "[appearance]\nappearance = \"dark\"\ndark_theme = \"canopy\"\n",
    )
    .unwrap();
    write_pack(
        &home,
        "canopy",
        &pack_text(
            "canopy",
            "dark",
            "[metrics]\n\"terminal.font-size\" = 17\n\"terminal.line-height\" = 1.5\n",
        ),
    );
    settings.reload();
    let (view, cx) = open_with(cx, settings.clone(), connection, None);
    let font = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, _| {
            let s = wb.terminal_view_settings();
            (s.font_size, s.line_height)
        })
    };
    assert_eq!(
        font(cx),
        (17.0, 1.5),
        "no user setting: the pack's defaults"
    );

    settings
        .write_user("terminal.font_size", toml::Value::Float(20.0))
        .unwrap();
    wait_until(cx, "the user's size", |cx| font(cx).0 == 20.0);
    assert_eq!(font(cx).1, 1.5, "only the key the user wrote overrides");

    // A user value equal to the built-in default is still the user's.
    settings
        .write_user("terminal.font_size", toml::Value::Float(13.0))
        .unwrap();
    wait_until(cx, "13 as the user's size", |cx| font(cx).0 == 13.0);

    // (Removing a key whose value equals the default publishes nothing in the settings store, so
    // the user value moves off the default first.)
    settings
        .write_user("terminal.font_size", toml::Value::Float(20.0))
        .unwrap();
    wait_until(cx, "20 again", |cx| font(cx).0 == 20.0);
    settings
        .remove(hutan_settings::SettingsLayer::User, "terminal", "font_size")
        .unwrap();
    wait_until(cx, "the pack's size again", |cx| font(cx).0 == 17.0);
}

#[gpui::test]
fn a_pack_that_is_not_in_use_restyles_nothing(cx: &mut TestAppContext) {
    let (connection, settings, _) = offline_connection();
    let home = settings
        .paths()
        .unwrap()
        .user
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(
        home.join("settings.toml"),
        "[appearance]\nappearance = \"dark\"\n",
    )
    .unwrap();
    write_pack(
        &home,
        "other",
        &pack_text(
            "other",
            "dark",
            "[colors]\n\"ui.background\" = \"#111111\"\n",
        ),
    );
    settings.reload();
    let (view, cx) = open_with(cx, settings, connection, None);
    let before: Arc<_> = view.read_with(cx, |wb, _| wb.resolved_theme().clone());
    let style_before = view.read_with(cx, |wb, _| wb.terminal_view_settings().clone());
    let generation = view.read_with(cx, |wb, _| wb.theme_registry().generation());

    save_atomically(
        &home,
        "other",
        &pack_text(
            "other",
            "dark",
            "[colors]\n\"ui.background\" = \"#222222\"\n",
        ),
    );
    wait_until(cx, "the catalog to change", |cx| {
        view.read_with(cx, |wb, _| wb.theme_registry().generation()) > generation
    });
    for _ in 0..20 {
        common::draw(cx);
    }
    let after = view.read_with(cx, |wb, _| wb.resolved_theme().clone());
    assert!(
        Arc::ptr_eq(&before, &after),
        "the active theme is the same snapshot"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().clone()),
        style_before
    );
    assert_eq!(bg_hex(&view, cx), 0x262624);
}

#[gpui::test]
fn a_pinned_terminal_pack_follows_its_own_edits_while_the_chrome_stays(cx: &mut TestAppContext) {
    let (connection, settings, _) = offline_connection();
    let home = settings
        .paths()
        .unwrap()
        .user
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(
        home.join("settings.toml"),
        "[appearance]\nappearance = \"dark\"\n[terminal]\ntheme = \"other\"\n",
    )
    .unwrap();
    write_pack(
        &home,
        "other",
        &pack_text(
            "other",
            "dark",
            "[colors]\n\"terminal.background\" = \"#111111\"\n",
        ),
    );
    settings.reload();
    let (view, cx) = open_with(cx, settings, connection, None);
    assert_eq!(
        terminal_bg(&view, cx),
        hutan_terminal_view::Rgb(0x11, 0x11, 0x11)
    );
    assert_eq!(
        bg_hex(&view, cx),
        0x262624,
        "only the terminal section is taken"
    );
    save_atomically(
        &home,
        "other",
        &pack_text(
            "other",
            "dark",
            "[colors]\n\"terminal.background\" = \"#222222\"\n",
        ),
    );
    wait_until(cx, "the terminal to follow", |cx| {
        terminal_bg(&view, cx) == hutan_terminal_view::Rgb(0x22, 0x22, 0x22)
    });
}
