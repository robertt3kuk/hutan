#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The theme, icon and terminal-theme pickers of Settings (TH-3), driven with the real keys and
//! the real pointer on a real workbench: the catalog behind the list follows the files, moving
//! previews without writing or touching any global, Return writes once, Escape, a click outside,
//! closing Settings cancel, selections of colors and icons are independent, Problems say what is
//! wrong with a pack, reset removes keys, and a second window never sees a preview.

use crate::common;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use common::{draw, offline_connection, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px};
use hutan_desktop::icons::{self, Icon};
use hutan_desktop::workbench::style::UiStyle;
use hutan_desktop::workbench::{PackField, Region, Workbench, WorkbenchOptions, actions};
use hutan_settings::SettingsStore;

const PINNED_DARK: &str = "[appearance]\nappearance = \"dark\"\n";
const CANOPY: &str = "schema_version = 1\nid = \"canopy\"\nname = \"Canopy Night\"\nauthors = [\"Ada Author\"]\nlicense = \"MIT\"\nappearance = \"dark\"\nextends = \"builtin:hutan-dark\"\n\n[colors]\n\"ui.background\" = \"#101820\"\n\"terminal.background\" = \"#102030\"\n";
const PAPER: &str = "schema_version = 1\nid = \"paper\"\nname = \"Paper\"\nauthors = [\"Bob\"]\nlicense = \"MIT\"\nappearance = \"light\"\nextends = \"builtin:hutan-light\"\n\n[colors]\n\"ui.background\" = \"#fff8e8\"\n";
const BROKEN: &str = "schema_version = 1\nid = \"broken\"\nname = \"Broken\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nappearance = \"dark\"\nextends = \"builtin:hutan-dark\"\n\n[colors]\n\"ui.border\" = \"oops\"\n";
// Text and background the same: a theme nobody can read.
const UNREADABLE: &str = "schema_version = 1\nid = \"void\"\nname = \"Void\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nappearance = \"dark\"\nextends = \"builtin:hutan-dark\"\n\n[colors]\n\"ui.foreground\" = \"#262624\"\n\"ui.text.muted\" = \"#262624\"\n\"ui.text.faint\" = \"#262624\"\n";
const CLOSE: &str = "<svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\"><path d=\"M6 6 18 18M18 6 6 18\"/></svg>";
const LINES: &str = "schema_version = 1\nid = \"lines\"\nname = \"Lines\"\nauthors = [\"Cy\"]\nlicense = \"MIT\"\nextends = \"builtin:phosphor\"\n\n[icons]\n\"action.close\" = \"icons/close.svg\"\n\"nav.search\" = \"icons/close.svg\"\n";

fn write_color(home: &Path, id: &str, text: &str) {
    let dir = home.join("themes").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("theme.toml"), text).unwrap();
}

fn write_icons(home: &Path) {
    let dir = home.join("icon-themes/lines");
    std::fs::create_dir_all(dir.join("icons")).unwrap();
    std::fs::write(dir.join("icon-theme.toml"), LINES).unwrap();
    std::fs::write(dir.join("icons/close.svg"), CLOSE).unwrap();
}

struct Fixture {
    home: PathBuf,
    settings: SettingsStore,
}

impl Fixture {
    fn settings_file(&self) -> String {
        std::fs::read_to_string(self.home.join("settings.toml")).unwrap_or_default()
    }

    fn write_settings(&self, text: &str) {
        std::fs::write(self.home.join("settings.toml"), text).unwrap();
        self.settings.reload();
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    prepare: impl FnOnce(&Path),
    settings_toml: &str,
) -> (Entity<Workbench>, Fixture, &'a mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    let home = settings
        .paths()
        .unwrap()
        .user
        .parent()
        .unwrap()
        .to_path_buf();
    prepare(&home);
    // The test platform's window is light: pin the mode so that the dark slot is in effect.
    let settings_toml = if settings_toml.is_empty() {
        PINNED_DARK
    } else {
        settings_toml
    };
    std::fs::write(home.join("settings.toml"), settings_toml).unwrap();
    settings.reload();
    let (view, cx) = open_with(cx, settings.clone(), connection, None);
    (view, Fixture { home, settings }, cx)
}

fn open_page(view: &Entity<Workbench>, cx: &mut VisualTestContext, page: &str) {
    cx.dispatch_action(actions::OpenSettingsFor(page.into()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some(page.to_owned())
    );
}

fn click_region(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
    draw(cx);
}

fn picker_index(field: PackField) -> usize {
    field.index()
}

/// Clicks the pack action `action` (`pack-open-folder`, `pack-reload`, `pack-duplicate`,
/// `pack-reset`) of the open picker of `field`: they are in its `⋯` menu, opened first.
fn pack_action(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    field: PackField,
    action: &'static str,
) {
    if view.read_with(cx, |wb, _| wb.settings_sheet().and_then(|s| s.pack_menu())) != Some(field) {
        click_region(view, cx, Region::SettingsItem("pack-more", field.index()));
    }
    click_region(view, cx, Region::SettingsItem(action, field.index()));
}

/// Opens a picker the way the head's click does, scrolling its row into view.
fn open_picker(view: &Entity<Workbench>, cx: &mut VisualTestContext, field: PackField) {
    if view.read_with(cx, |wb, _| wb.open_pack_picker()) == Some(field) {
        return;
    }
    view.update(cx, |wb, cx| {
        if wb.open_pack_picker().is_some() {
            wb.close_pack_picker(cx);
        }
        wb.toggle_pack_picker(field, cx);
    });
    draw(cx);
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.open_pack_picker()),
        Some(field),
        "the picker opened"
    );
}

fn ids(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    field: PackField,
    filter: &str,
) -> Vec<String> {
    view.read_with(cx, |wb, _| {
        wb.pack_rows(field, filter)
            .into_iter()
            .map(|r| r.value.unwrap_or_else(|| format!("({})", r.title)))
            .collect()
    })
}

fn cursor(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.pack_cursor().map(str::to_owned))
}

fn preview_id(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.pack_preview().map(|p| p.id.clone()))
}

fn resolved_id(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, _| wb.resolved_theme().id.clone())
}

fn style_of(cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| format!("{:?}", UiStyle::get(cx).theme))
}

fn problems(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| wb.sheet_problems())
}

fn generation(fixture: &Fixture) -> u64 {
    fixture.settings.changes().borrow().generation
}

// ---- the catalog behind the list ----------------------------------------------------------------

#[gpui::test]
fn the_list_follows_added_deleted_and_invalid_packs_and_fits_the_slot(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, |_| {}, "");
    open_page(&view, cx, "appearance");
    assert_eq!(ids(&view, cx, PackField::DarkTheme, ""), ["hutan-dark"]);
    assert_eq!(ids(&view, cx, PackField::LightTheme, ""), ["hutan-light"]);

    // A pack appears in the slot of its polarity, and only there.
    write_color(&fixture.home, "canopy", CANOPY);
    write_color(&fixture.home, "paper", PAPER);
    wait_until(cx, "the packs to be listed", |cx| {
        ids(&view, cx, PackField::DarkTheme, "").contains(&"canopy".to_owned())
            && ids(&view, cx, PackField::LightTheme, "").contains(&"paper".to_owned())
    });
    assert_eq!(
        ids(&view, cx, PackField::DarkTheme, ""),
        ["hutan-dark", "canopy"]
    );
    assert_eq!(
        ids(&view, cx, PackField::LightTheme, ""),
        ["hutan-light", "paper"]
    );
    // The terminal lists Automatic and every valid color pack, whatever its polarity.
    assert_eq!(
        ids(&view, cx, PackField::TerminalTheme, ""),
        ["auto", "hutan-dark", "hutan-light", "canopy", "paper"]
    );

    // Search by name, id or author.
    assert_eq!(ids(&view, cx, PackField::DarkTheme, "night"), ["canopy"]);
    assert_eq!(ids(&view, cx, PackField::DarkTheme, "ADA"), ["canopy"]);
    assert_eq!(
        ids(&view, cx, PackField::DarkTheme, "canopy mit"),
        Vec::<String>::new(),
        "license is not searched"
    );
    assert_eq!(
        ids(&view, cx, PackField::DarkTheme, "zzz"),
        Vec::<String>::new()
    );

    // An invalid pack is listed, disabled, with its error.
    write_color(&fixture.home, "broken", BROKEN);
    wait_until(cx, "the invalid pack to be listed", |cx| {
        ids(&view, cx, PackField::DarkTheme, "").contains(&"(broken)".to_owned())
    });
    let row = view.read_with(cx, |wb, _| {
        wb.pack_rows(PackField::DarkTheme, "")
            .into_iter()
            .find(|r| r.value.is_none())
            .unwrap()
    });
    let info = row.info.unwrap();
    assert!(!info.valid);
    let error = info.errors().next().unwrap();
    assert_eq!(error.key_path, "colors.ui.border");
    assert!(
        error
            .file
            .as_ref()
            .unwrap()
            .ends_with("themes/broken/theme.toml")
    );
    // It cannot be picked: not an option of the keys.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_kind("dark_theme")),
        Some(hutan_desktop::workbench::settings_schema::FieldKind::Enum(
            vec!["hutan-dark".to_owned(), "canopy".to_owned()]
        ))
    );

    // Deleting removes it, the broken one included.
    std::fs::remove_dir_all(fixture.home.join("themes/canopy")).unwrap();
    std::fs::remove_dir_all(fixture.home.join("themes/broken")).unwrap();
    wait_until(cx, "the packs to be gone", |cx| {
        ids(&view, cx, PackField::DarkTheme, "") == ["hutan-dark"]
    });
}

// ---- preview, apply, cancel ---------------------------------------------------------------------

#[gpui::test]
fn moving_previews_without_writing_and_return_writes_once(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, |home| write_color(home, "canopy", CANOPY), "");
    open_page(&view, cx, "appearance");
    let style = style_of(cx);
    let before_file = fixture.settings_file();
    let before_generation = generation(&fixture);
    open_picker(&view, cx, PackField::DarkTheme);
    assert_eq!(
        preview_id(&view, cx),
        None,
        "nothing is previewed before the cursor moves"
    );

    cx.simulate_keystrokes("down");
    assert_eq!(cursor(&view, cx).as_deref(), Some("canopy"));
    assert_eq!(preview_id(&view, cx).as_deref(), Some("canopy"));
    let background = view.read_with(cx, |wb, _| wb.pack_preview().unwrap().background());
    assert_eq!(
        background.as_deref(),
        Some("#101820"),
        "the preview is the pack's own values"
    );
    // Up and down again: still nothing written, nothing global changed.
    cx.simulate_keystrokes("up down");
    draw(cx);
    assert_eq!(fixture.settings_file(), before_file);
    assert_eq!(generation(&fixture), before_generation);
    assert_eq!(resolved_id(&view, cx), "hutan-dark");
    assert_eq!(
        style_of(cx),
        style,
        "the window's style is untouched by a preview"
    );

    // Return applies once.
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        fixture.settings_file().matches("dark_theme").count(),
        1,
        "{}",
        fixture.settings_file()
    );
    assert!(fixture.settings_file().contains("dark_theme = \"canopy\""));
    wait_until(cx, "the theme to apply", |cx| {
        resolved_id(&view, cx) == "canopy"
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.open_pack_picker()),
        None,
        "the picker closed"
    );
    assert_eq!(preview_id(&view, cx), None);
    assert_ne!(style_of(cx), style, "the applied theme restyled the window");
    let after = generation(&fixture);

    // Choosing what is already in effect writes nothing.
    open_picker(&view, cx, PackField::DarkTheme);
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(generation(&fixture), after);
    assert_eq!(fixture.settings_file().matches("dark_theme").count(), 1);
}

#[gpui::test]
fn escape_a_click_outside_and_closing_settings_cancel_without_a_write(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, |home| write_color(home, "canopy", CANOPY), "");
    open_page(&view, cx, "appearance");
    let style = style_of(cx);
    let file = fixture.settings_file();
    let generation_before = generation(&fixture);
    let assert_untouched = |view: &Entity<Workbench>, cx: &mut VisualTestContext| {
        assert_eq!(fixture.settings_file(), file);
        assert_eq!(generation(&fixture), generation_before);
        assert_eq!(resolved_id(view, cx), "hutan-dark");
        assert_eq!(style_of(cx), style);
    };

    // Escape: the picker closes, the sheet stays.
    open_picker(&view, cx, PackField::DarkTheme);
    cx.simulate_keystrokes("down");
    assert_eq!(preview_id(&view, cx).as_deref(), Some("canopy"));
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |wb, _| wb.open_pack_picker()), None);
    assert_eq!(preview_id(&view, cx), None);
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_some()));
    assert_untouched(&view, cx);

    // A click elsewhere in the sheet (the search box): the picker closes.
    open_picker(&view, cx, PackField::DarkTheme);
    cx.simulate_keystrokes("down");
    assert_eq!(preview_id(&view, cx).as_deref(), Some("canopy"));
    // The search box of the sheet, outside the picker's row.
    cx.simulate_click(point(px(640.0), px(75.0)), Modifiers::none());
    cx.run_until_parked();
    draw(cx);
    assert_eq!(view.read_with(cx, |wb, _| wb.open_pack_picker()), None);
    assert_eq!(preview_id(&view, cx), None);
    assert_untouched(&view, cx);

    // The backdrop (outside the sheet) closes Settings, and the preview with it.
    open_picker(&view, cx, PackField::DarkTheme);
    cx.simulate_keystrokes("down");
    assert_eq!(preview_id(&view, cx).as_deref(), Some("canopy"));
    cx.simulate_click(point(px(100.0), px(700.0)), Modifiers::none());
    cx.run_until_parked();
    draw(cx);
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_none()));
    assert_eq!(preview_id(&view, cx), None);
    assert_untouched(&view, cx);

    // Closing Settings with a preview open (the command) too, and a reopened sheet starts clean.
    open_page(&view, cx, "appearance");
    open_picker(&view, cx, PackField::DarkTheme);
    cx.simulate_keystrokes("down");
    view.update_in(cx, |wb, window, cx| wb.close_settings(window, cx));
    draw(cx);
    assert_untouched(&view, cx);
    open_page(&view, cx, "appearance");
    assert_eq!(view.read_with(cx, |wb, _| wb.open_pack_picker()), None);
    assert_eq!(preview_id(&view, cx), None);
}

#[gpui::test]
fn external_settings_changes_during_a_preview_are_what_cancelling_shows(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(
        cx,
        |home| {
            write_color(home, "canopy", CANOPY);
            write_color(home, "paper", PAPER);
        },
        "[appearance]\nappearance = \"dark\"\n",
    );
    open_page(&view, cx, "appearance");
    open_picker(&view, cx, PackField::DarkTheme);
    cx.simulate_keystrokes("down");
    assert_eq!(preview_id(&view, cx).as_deref(), Some("canopy"));

    // While the preview is open the mode changes (the system or an editor): light is in effect.
    fixture.write_settings("[appearance]\nappearance = \"light\"\nlight_theme = \"paper\"\n");
    wait_until(cx, "the light theme to apply", |cx| {
        resolved_id(&view, cx) == "paper"
    });
    let external = fixture.settings_file();
    // The preview of the dark slot is still drawn from the pack, not from what is in effect.
    assert_eq!(preview_id(&view, cx).as_deref(), Some("canopy"));

    cx.simulate_keystrokes("escape");
    assert_eq!(preview_id(&view, cx), None);
    assert_eq!(
        resolved_id(&view, cx),
        "paper",
        "cancelling shows the latest effective state"
    );
    assert_eq!(
        fixture.settings_file(),
        external,
        "and writes nothing over it"
    );

    // A pack that disappears under the cursor leaves nothing stale: the preview goes.
    open_picker(&view, cx, PackField::LightTheme);
    cx.simulate_keystrokes("up");
    assert_eq!(preview_id(&view, cx).as_deref(), Some("hutan-light"));
    cx.simulate_keystrokes("down");
    assert_eq!(preview_id(&view, cx).as_deref(), Some("paper"));
    std::fs::remove_dir_all(fixture.home.join("themes/paper")).unwrap();
    wait_until(cx, "the pack to vanish", |cx| {
        resolved_id(&view, cx) == "hutan-light"
    });
    assert_eq!(
        preview_id(&view, cx),
        None,
        "no preview of a pack that is gone"
    );
    cx.simulate_keystrokes("escape");
    assert_eq!(fixture.settings_file(), external);
}

#[gpui::test]
fn the_pointer_opens_previews_by_hovering_and_applies_with_a_click(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, |home| write_color(home, "canopy", CANOPY), "");
    open_page(&view, cx, "appearance");
    let file = fixture.settings_file();
    click_region(
        &view,
        cx,
        Region::SettingsItem("pack-picker", PackField::DarkTheme.index()),
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.open_pack_picker()),
        Some(PackField::DarkTheme)
    );
    // Pointing at a pack previews it, nothing else.
    wait_until(cx, "the second pack to be drawn", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::SettingsItem("pack-option", 1))
                .is_some()
        })
    });
    let at = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::SettingsItem("pack-option", 1))
        })
        .unwrap()
        .center();
    cx.simulate_mouse_move(at, None, Modifiers::none());
    draw(cx);
    assert_eq!(preview_id(&view, cx).as_deref(), Some("canopy"));
    assert_eq!(fixture.settings_file(), file);
    assert_eq!(resolved_id(&view, cx), "hutan-dark");
    // A click applies it once.
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    draw(cx);
    assert_eq!(view.read_with(cx, |wb, _| wb.open_pack_picker()), None);
    assert_eq!(
        fixture
            .settings_file()
            .matches("dark_theme = \"canopy\"")
            .count(),
        1
    );
    wait_until(cx, "the theme to apply", |cx| {
        resolved_id(&view, cx) == "canopy"
    });
}

#[gpui::test]
fn typing_searches_the_picker_and_the_preview_follows_the_first_match(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, |home| write_color(home, "canopy", CANOPY), "");
    open_page(&view, cx, "appearance");
    open_picker(&view, cx, PackField::DarkTheme);
    cx.simulate_keystrokes("a d a");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.pack_filter().to_owned()),
        "ada"
    );
    assert_eq!(preview_id(&view, cx).as_deref(), Some("canopy"));
    cx.simulate_keystrokes("x");
    assert_eq!(preview_id(&view, cx), None, "nothing matches adax");
    cx.simulate_keystrokes("backspace");
    assert_eq!(cursor(&view, cx).as_deref(), Some("canopy"));
    cx.simulate_keystrokes("enter");
    assert!(fixture.settings_file().contains("dark_theme = \"canopy\""));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.pack_filter().to_owned()),
        "",
        "the search is cleared"
    );
}

// ---- icons and colors are independent ------------------------------------------------------------

#[gpui::test]
fn the_icon_picker_previews_a_grid_with_a_fallback_and_changes_no_color(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(
        cx,
        |home| {
            write_icons(home);
            write_color(home, "canopy", CANOPY);
        },
        "[appearance]\ndark_theme = \"canopy\"\nappearance = \"dark\"\n",
    );
    open_page(&view, cx, "appearance");
    let style = style_of(cx);
    let close_before = icons::path(Icon::ActionClose);
    let file = fixture.settings_file();
    open_picker(&view, cx, PackField::IconTheme);
    cx.simulate_keystrokes("down");
    assert_eq!(cursor(&view, cx).as_deref(), Some("lines"));
    let (kind, split) = view.read_with(cx, |wb, _| {
        let p = wb.pack_preview().unwrap();
        (p.body_kind(), p.icon_split())
    });
    assert_eq!(kind, "icons");
    let (own, fallback) = split.unwrap();
    assert_eq!(own, 2, "action.close and nav.search");
    assert!(fallback > 50, "everything else falls back: {fallback}");
    draw(cx);
    // Nothing applied: the installed icons and colors are what they were.
    assert_eq!(icons::path(Icon::ActionClose), close_before);
    assert_eq!(style_of(cx), style);
    assert_eq!(fixture.settings_file(), file);

    cx.simulate_keystrokes("enter");
    draw(cx);
    wait_until(cx, "the icon pack to apply", |_| {
        icons::path(Icon::ActionClose).starts_with("theme-icons/lines/")
    });
    assert_ne!(icons::path(Icon::ActionClose), close_before);
    assert!(
        icons::path(Icon::NavTasks).starts_with("theme-icons/phosphor/"),
        "the other icons fall back individually"
    );
    assert_eq!(style_of(cx), style, "no color changed");
    let after = fixture.settings_file();
    assert!(after.contains("icon_theme = \"lines\""), "{after}");
    assert!(
        after.contains("dark_theme = \"canopy\""),
        "the color pick is untouched: {after}"
    );
    assert_eq!(resolved_id(&view, cx), "canopy");

    // And a color pick leaves the icons alone.
    open_picker(&view, cx, PackField::DarkTheme);
    cx.simulate_keystrokes("up enter");
    draw(cx);
    wait_until(cx, "the default theme", |cx| {
        resolved_id(&view, cx) == "hutan-dark"
    });
    assert!(icons::path(Icon::ActionClose).starts_with("theme-icons/lines/"));
    assert!(fixture.settings_file().contains("icon_theme = \"lines\""));
    assert!(
        !fixture.settings_file().contains("dark_theme"),
        "picking the embedded default removes the key instead of writing it: {}",
        fixture.settings_file()
    );
}

#[gpui::test]
fn the_terminal_picker_lists_automatic_and_the_packs_and_previews_the_ansi_row(
    cx: &mut TestAppContext,
) {
    let (view, fixture, cx) = open(
        cx,
        |home| {
            write_color(home, "canopy", CANOPY);
            write_color(home, "broken", BROKEN);
        },
        "",
    );
    open_page(&view, cx, "terminal");
    let ids = ids(&view, cx, PackField::TerminalTheme, "");
    assert_eq!(
        ids,
        ["auto", "hutan-dark", "hutan-light", "canopy", "(broken)"]
    );
    open_picker(&view, cx, PackField::TerminalTheme);
    cx.simulate_keystrokes("down down down");
    assert_eq!(cursor(&view, cx).as_deref(), Some("canopy"));
    let (kind, background) = view.read_with(cx, |wb, _| {
        let p = wb.pack_preview().unwrap();
        (p.body_kind(), p.background())
    });
    assert_eq!(kind, "terminal");
    assert_eq!(
        background.as_deref(),
        Some("#102030"),
        "the pack's terminal background"
    );
    assert_eq!(
        resolved_id(&view, cx),
        "hutan-dark",
        "the chrome is not touched"
    );
    cx.simulate_keystrokes("enter");
    assert!(
        fixture.settings_file().contains("theme = \"canopy\""),
        "{}",
        fixture.settings_file()
    );
    assert_eq!(
        resolved_id(&view, cx),
        "hutan-dark",
        "a terminal pick leaves the chrome alone"
    );
}

// ---- Problems, fallback, reset -------------------------------------------------------------------------

#[gpui::test]
fn a_diagnostics_only_generation_updates_problems_one_actionable_line_each(
    cx: &mut TestAppContext,
) {
    let (view, fixture, cx) = open(cx, |_| {}, "");
    open_page(&view, cx, "appearance");
    assert!(problems(&view, cx).is_empty(), "{:?}", problems(&view, cx));
    let style = style_of(cx);

    // A broken pack nobody uses: the active theme is unchanged, Problems has the line.
    write_color(&fixture.home, "broken", BROKEN);
    wait_until(cx, "the problem", |cx| {
        problems(&view, cx)
            .iter()
            .any(|p| p.contains("colors.ui.border"))
    });
    let lines: Vec<String> = problems(&view, cx)
        .into_iter()
        .filter(|p| p.contains("broken"))
        .collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("themes/broken/theme.toml"),
        "the file: {}",
        lines[0]
    );
    assert!(
        lines[0].contains("colors.ui.border"),
        "the key: {}",
        lines[0]
    );
    assert!(lines[0].contains("#rrggbb"), "the reason: {}", lines[0]);
    assert_eq!(style_of(cx), style);

    // A broken icon asset: its own line, with the icon id.
    write_icons(&fixture.home);
    std::fs::write(
        fixture.home.join("icon-themes/lines/icons/close.svg"),
        "<svg viewBox=\"0 0 24 24\"><script/></svg>",
    )
    .unwrap();
    wait_until(cx, "the icon problem", |cx| {
        problems(&view, cx)
            .iter()
            .any(|p| p.contains("icons.action.close"))
    });
    let icon_line = problems(&view, cx)
        .into_iter()
        .find(|p| p.contains("icons.action.close"))
        .unwrap();
    assert!(
        icon_line.contains("icon-themes/lines/icon-theme.toml"),
        "{icon_line}"
    );
    assert!(icon_line.contains("script"), "{icon_line}");

    // Fixing the first clears its line and only its line.
    write_color(&fixture.home, "broken", &BROKEN.replace("oops", "#112233"));
    wait_until(cx, "the fix", |cx| {
        !problems(&view, cx)
            .iter()
            .any(|p| p.contains("colors.ui.border"))
    });
    assert!(
        problems(&view, cx)
            .iter()
            .any(|p| p.contains("icons.action.close"))
    );
}

#[gpui::test]
fn an_invalid_selection_falls_back_visibly_and_reset_removes_the_keys(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(
        cx,
        |_| {},
        "[appearance]\nappearance = \"dark\"\ndark_theme = \"missing\"\nicon_theme = \"nowhere\"\n\n[appearance.theme_overrides.hutan-dark.colors]\n\"ui.accent\" = \"#336699\"\n\n[terminal]\ntheme = \"gone\"\n",
    );
    // Everything falls back to the embedded packs, and each request says why.
    assert_eq!(resolved_id(&view, cx), "hutan-dark");
    assert!(icons::path(Icon::ActionClose).starts_with("theme-icons/phosphor/"));
    open_page(&view, cx, "appearance");
    let lines = problems(&view, cx);
    assert!(
        lines
            .iter()
            .any(|p| p.contains("appearance.dark_theme") && p.contains("missing")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|p| p.contains("appearance.icon_theme") && p.contains("nowhere")),
        "{lines:?}"
    );
    // The request is kept: installing it applies it.
    write_color(
        &fixture.home,
        "missing",
        &CANOPY.replace("canopy", "missing"),
    );
    wait_until(cx, "the pack to appear", |cx| {
        resolved_id(&view, cx) == "missing"
    });

    // The row's Reset to embedded default removes the key; nothing is written in its place.
    open_picker(&view, cx, PackField::DarkTheme);
    pack_action(&view, cx, PackField::DarkTheme, "pack-reset");
    wait_until(cx, "the reset", |cx| resolved_id(&view, cx) == "hutan-dark");
    let file = fixture.settings_file();
    assert!(!file.contains("dark_theme"), "{file}");
    assert!(
        file.contains("icon_theme = \"nowhere\""),
        "other keys stay: {file}"
    );

    // "Reset Theme and Icons" is a command, and removes every selection and override.
    cx.dispatch_action(actions::ResetTheme);
    draw(cx);
    let file = fixture.settings_file();
    for key in [
        "icon_theme",
        "theme_overrides",
        "ui.accent",
        "theme = \"gone\"",
    ] {
        assert!(!file.contains(key), "{key} should be gone: {file}");
    }
    assert!(
        file.contains("appearance = \"dark\""),
        "the mode is not a theme: {file}"
    );
    assert!(view.read_with(cx, |wb, _| wb.notice().is_some_and(|n| n.contains("reset"))));
}

#[gpui::test]
fn an_unreadable_theme_never_removes_the_way_back(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(
        cx,
        |home| write_color(home, "void", UNREADABLE),
        "[appearance]\nappearance = \"dark\"\ndark_theme = \"void\"\n",
    );
    assert_eq!(resolved_id(&view, cx), "void");
    // The theme is in effect and unreadable; the picker row and its Reset are still there ...
    open_page(&view, cx, "appearance");
    click_region(
        &view,
        cx,
        Region::SettingsItem("pack-picker", picker_index(PackField::DarkTheme)),
    );
    // Its actions are in the ⋯ menu of the picker, Reset to embedded default among them.
    click_region(
        &view,
        cx,
        Region::SettingsItem("pack-more", PackField::DarkTheme.index()),
    );
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::SettingsItem(
            "pack-reset",
            PackField::DarkTheme.index(),
        ))
        .is_some()
    }));
    // The menu, the picker, then the sheet.
    cx.simulate_keystrokes("escape escape escape");
    // ... and so is the command, which does not depend on anything being legible.
    cx.dispatch_action(actions::ResetTheme);
    draw(cx);
    wait_until(cx, "the way back", |cx| {
        resolved_id(&view, cx) == "hutan-dark"
    });
    assert!(!fixture.settings_file().contains("void"));
}

// ---- the actions of the row -----------------------------------------------------------------------------

#[gpui::test]
fn the_row_opens_the_folder_reloads_and_duplicates_into_a_new_pack(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, |home| write_color(home, "canopy", CANOPY), "");
    let opened: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let sink = opened.clone();
    view.update(cx, |wb, _| {
        wb.set_open_handler(move |path, _| sink.borrow_mut().push(path.to_owned()));
    });
    open_page(&view, cx, "appearance");
    open_picker(&view, cx, PackField::DarkTheme);

    // Open pack folder: the folder of that kind, created when it is not there.
    pack_action(&view, cx, PackField::DarkTheme, "pack-open-folder");
    assert_eq!(opened.borrow().len(), 1);
    assert!(
        opened.borrow()[0].ends_with("themes"),
        "{:?}",
        opened.borrow()
    );
    open_picker_if_closed(&view, cx, PackField::IconTheme);
    pack_action(&view, cx, PackField::IconTheme, "pack-open-folder");
    assert!(
        opened.borrow()[1].ends_with("icon-themes"),
        "{:?}",
        opened.borrow()
    );
    assert!(fixture.home.join("icon-themes").is_dir());

    // Validate / Reload says what it found.
    open_picker_if_closed(&view, cx, PackField::DarkTheme);
    pack_action(&view, cx, PackField::DarkTheme, "pack-reload");
    let notice = view
        .read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
        .unwrap();
    assert!(
        notice.contains("3 color") && notice.contains("0 errors"),
        "{notice}"
    );

    // Duplicate as new pack: the highlighted one, in your folder, ready to edit; selection untouched.
    cx.simulate_keystrokes("down");
    assert_eq!(cursor(&view, cx).as_deref(), Some("canopy"));
    let file = fixture.settings_file();
    pack_action(&view, cx, PackField::DarkTheme, "pack-duplicate");
    assert!(fixture.home.join("themes/canopy-copy/theme.toml").is_file());
    assert_eq!(
        ids(&view, cx, PackField::DarkTheme, ""),
        ["hutan-dark", "canopy", "canopy-copy"]
    );
    assert_eq!(
        cursor(&view, cx).as_deref(),
        Some("canopy-copy"),
        "the copy is highlighted and previewed"
    );
    assert_eq!(preview_id(&view, cx).as_deref(), Some("canopy-copy"));
    assert_eq!(fixture.settings_file(), file, "duplicating selects nothing");
    let text = std::fs::read_to_string(fixture.home.join("themes/canopy-copy/theme.toml")).unwrap();
    assert!(
        text.contains("id = \"canopy-copy\"") && text.contains("Canopy Night (copy)"),
        "{text}"
    );
    // Editing the copy is live: the preview and the list follow the file.
    std::fs::write(
        fixture.home.join("themes/canopy-copy/theme.toml"),
        text.replace("#101820", "#202830"),
    )
    .unwrap();
    wait_until(cx, "the edit to show in the preview", |cx| {
        view.read_with(cx, |wb, _| {
            wb.pack_preview().and_then(|p| p.background()) == Some("#202830".to_owned())
        })
    });
}

fn open_picker_if_closed(view: &Entity<Workbench>, cx: &mut VisualTestContext, field: PackField) {
    if view.read_with(cx, |wb, _| wb.open_pack_picker()) != Some(field) {
        open_picker(view, cx, field);
    }
}

// ---- two windows -------------------------------------------------------------------------------------

#[gpui::test]
fn a_preview_in_one_window_never_reaches_another(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let open_window = |cx: &mut TestAppContext| {
        let (connection, settings, _) = offline_connection();
        let home = settings
            .paths()
            .unwrap()
            .user
            .parent()
            .unwrap()
            .to_path_buf();
        write_color(&home, "canopy", CANOPY);
        std::fs::write(home.join("settings.toml"), PINNED_DARK).unwrap();
        settings.reload();
        let options = WorkbenchOptions::builtin(settings, connection, None);
        let window = cx.add_window(|window, cx| Workbench::new(options, window, cx));
        (window, home)
    };
    let (a, _home_a) = open_window(cx);
    let (b, _home_b) = open_window(cx);
    cx.run_until_parked();
    let style_before = cx.update(|cx| format!("{:?}", UiStyle::get(cx).theme));
    let close_before = icons::path(Icon::ActionClose);

    // Window A previews canopy in its dark picker.
    a.update(cx, |wb, window, cx| {
        wb.open_settings(Some("appearance"), window, cx);
    })
    .unwrap();
    let preview = a
        .update(cx, |wb, _window, cx| {
            wb.toggle_pack_picker(PackField::DarkTheme, cx);
            wb.pack_point_at(PackField::DarkTheme, "canopy", cx);
            wb.pack_preview().map(|p| (p.id.clone(), p.background()))
        })
        .unwrap();
    assert_eq!(
        preview,
        Some(("canopy".to_owned(), Some("#101820".to_owned())))
    );

    // Window B has no sheet, no preview, and the same style and icons as before.
    let b_state = b
        .read_with(cx, |wb, _| {
            (
                wb.settings_sheet().is_some(),
                wb.pack_preview().is_some(),
                wb.resolved_theme().id.clone(),
            )
        })
        .unwrap();
    assert_eq!(b_state, (false, false, "hutan-dark".to_owned()));
    assert_eq!(
        cx.update(|cx| format!("{:?}", UiStyle::get(cx).theme)),
        style_before
    );
    assert_eq!(icons::path(Icon::ActionClose), close_before);

    // Even with a sheet open in B, its picker shows nothing of A's cursor.
    b.update(cx, |wb, window, cx| {
        wb.open_settings(Some("appearance"), window, cx);
    })
    .unwrap();
    let b_cursor = b
        .read_with(cx, |wb, _| {
            (
                wb.open_pack_picker(),
                wb.pack_cursor().map(str::to_owned),
                wb.pack_preview().is_some(),
            )
        })
        .unwrap();
    assert_eq!(b_cursor, (None, None, false));

    // Cancelling in A leaves both exactly as they were.
    a.update(cx, |wb, window, cx| wb.close_settings(window, cx))
        .unwrap();
    assert_eq!(
        cx.update(|cx| format!("{:?}", UiStyle::get(cx).theme)),
        style_before
    );
}
