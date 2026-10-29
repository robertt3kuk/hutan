#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The settings sheet in five groups and fourteen pages (SETTINGS-IA): every old page id still
//! opens its page scrolled to its section, the groups are in order, the search finds every key
//! on the page of its section, and the `@modified` and `@restart` filters (typed, or switched on
//! by the chips under the search box) leave the rows they name.

use crate::common;

use std::path::Path;

use common::{TestDaemon, click_at, draw, open_with, settle, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::settings_schema::{Filters, NavGroup, PAGES};
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_settings::{SettingsPaths, SettingsStore};

fn store(dir: &Path) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(dir))
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, store(daemon.home()), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn wait_listed(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the daemon settings", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_settings().settings().is_some())
    });
}

fn pages(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<&'static str> {
    view.read_with(cx, |wb, _| wb.sheet_pages())
}

/// Every page of the sheet before SETTINGS-IA, with its page and its section now. The id is
/// still a deep link: ⌘K, a panel's gear, `OpenSettingsFor` and the links of the app use it.
const OLD_PAGES: [(&str, &str, &str); 34] = [
    ("appearance", "appearance", "appearance"),
    ("layout", "appearance", "layout"),
    ("status", "appearance", "status"),
    ("app", "app", "app"),
    ("notifications", "app", "notifications"),
    ("sessions_restore", "app", "sessions_restore"),
    ("terminal", "terminal", "terminal"),
    ("editor", "editor", "editor"),
    ("keymap", "editor", "keymap"),
    ("assistant", "assistant", "assistant"),
    ("agents", "agents", "agents"),
    ("chat", "agents", "chat"),
    ("launcher", "agents", "launcher"),
    ("panels.inspector", "agents", "panels.inspector"),
    ("accounts", "accounts", "accounts"),
    ("models", "accounts", "models"),
    ("integrations", "integrations", "integrations"),
    ("automation", "integrations", "automation"),
    ("panels.sessions", "panels.sessions", "panels.sessions"),
    // The sidebar's organization is a part of the Projects Sidebar section.
    ("sidebar", "panels.sessions", "panels.sessions"),
    ("workspaces", "panels.sessions", "workspaces"),
    ("tasks", "panels.sessions", "tasks"),
    ("panels.files", "panels.files", "panels.files"),
    ("changes", "panels.files", "changes"),
    // The Changes panel's options are a part of the Changes section.
    ("panels.changes", "panels.files", "changes"),
    ("panels.search", "panels.files", "panels.search"),
    ("panels.problems", "panels.files", "panels.problems"),
    ("panels.instructions", "panels.files", "panels.instructions"),
    ("history", "history", "history"),
    ("data", "data", "data"),
    ("agents_tools", "agents_tools", "agents_tools"),
    ("control", "agents_tools", "control"),
    ("resources", "agents_tools", "resources"),
    ("experimental", "experimental", "experimental"),
];

/// Whether the page is scrolled so that `section` starts at its top, or as near as the end of
/// the page allows.
fn section_in_place(view: &Entity<Workbench>, cx: &mut VisualTestContext, section: &str) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| {
        let sheet = wb.settings_sheet().unwrap();
        let scroll = sheet.scroll();
        let offset = f32::from(scroll.offset().y);
        let most = f32::from(scroll.max_offset().y);
        let first = PAGES
            .iter()
            .find(|p| p.id == sheet.page())
            .and_then(|p| p.sections.first())
            .copied();
        if first == Some(section) {
            return offset == 0.0;
        }
        let Some(head) = wb.region_bounds(Region::SettingsSection(section_id(section))) else {
            return false;
        };
        let top = f32::from(head.top()) - f32::from(scroll.bounds().top());
        top.abs() <= 1.5 || (offset - -most).abs() <= 0.5 && top >= -1.5
    })
}

/// The `'static` id of a section named by the table above.
fn section_id(section: &str) -> &'static str {
    PAGES
        .iter()
        .flat_map(|p| p.sections.iter().copied())
        .find(|s| *s == section)
        .unwrap()
}

#[gpui::test]
fn there_are_fourteen_pages_in_five_groups_in_order_and_only_advanced_is_folded(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    cx.dispatch_action(actions::OpenSettings);
    draw(cx);
    settle(cx);
    let nav = view.read_with(cx, |wb, _| wb.sheet_nav());
    let summary: Vec<(&str, bool, Vec<&str>)> = nav
        .iter()
        .map(|(group, open, pages)| (group.title(), *open, pages.clone()))
        .collect();
    assert_eq!(
        summary,
        [
            (
                "General",
                true,
                vec!["appearance", "app", "terminal", "editor"]
            ),
            (
                "Agents",
                true,
                vec!["assistant", "agents", "accounts", "integrations"]
            ),
            ("Workspace", true, vec!["panels.sessions", "panels.files"]),
            ("Data", true, vec!["history", "data"]),
            ("Advanced", false, vec!["agents_tools", "experimental"]),
        ]
    );
    assert_eq!(
        nav.iter().map(|(g, _, _)| *g).collect::<Vec<_>>(),
        NavGroup::ALL
    );
    assert_eq!(pages(&view, cx).len(), 14);
    // ⌘, without a page opens the first one.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.settings_sheet().map(|s| s.page().to_owned())),
        Some("appearance".to_owned())
    );
}

#[gpui::test]
fn every_old_page_id_opens_its_page_scrolled_to_its_section(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    assert_eq!(
        OLD_PAGES.len(),
        34,
        "the 34 pages of the sheet before the regrouping"
    );
    for (old, page, section) in OLD_PAGES {
        cx.dispatch_action(actions::OpenSettingsFor(old.into()));
        draw(cx);
        settle(cx);
        let (opened, at) = view.read_with(cx, |wb, _| {
            let sheet = wb.settings_sheet().unwrap();
            (sheet.page().to_owned(), sheet.namespace().to_owned())
        });
        assert_eq!(opened, page, "{old} opens the page {page}");
        assert_eq!(at, section, "{old} opens at the section {section}");
        // The page list shows the page, and the page is scrolled to the section.
        assert!(pages(&view, cx).contains(&page), "{old}");
        wait_until(cx, &format!("{old} to be scrolled to {section}"), |cx| {
            section_in_place(&view, cx, section)
        });
    }
}

#[gpui::test]
fn a_page_names_its_sections_under_heads_and_a_single_section_page_has_none(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
    draw(cx);
    settle(cx);
    // Appearance is the page's own title: no head over its first section; Layout and the
    // status bar have theirs.
    let head = |view: &Entity<Workbench>, cx: &mut VisualTestContext, section: &'static str| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::SettingsSection(section))
        })
    };
    assert!(head(&view, cx, "appearance").is_none());
    assert!(head(&view, cx, "layout").is_some());
    assert!(head(&view, cx, "status").is_some());
    // A page of one section has no head at all.
    cx.dispatch_action(actions::OpenSettingsFor("terminal".into()));
    draw(cx);
    settle(cx);
    assert!(head(&view, cx, "terminal").is_none());
}

#[gpui::test]
fn the_search_finds_every_key_on_the_page_of_its_section_and_opens_it_there(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let mut checked = 0;
    for page in PAGES {
        for section in page.sections {
            cx.dispatch_action(actions::OpenSettingsFor((*section).into()));
            draw(cx);
            let keys: Vec<String> = view.read_with(cx, |wb, _| {
                wb.settings_sheet()
                    .unwrap()
                    .fields()
                    .iter()
                    .map(|f| f.dotted())
                    .collect()
            });
            for key in keys {
                // From another page: the sheet goes to the page of the key.
                view.update(cx, |wb, cx| {
                    wb.select_settings_namespace("appearance", cx);
                    wb.set_settings_query(&key, cx);
                });
                let found = pages(&view, cx);
                assert!(
                    found.contains(&page.id),
                    "`{section}.{key}` is not found on {}: {found:?}",
                    page.id
                );
                view.update(cx, |wb, cx| wb.select_settings_namespace(page.id, cx));
                let (at_page, at) = view.read_with(cx, |wb, _| {
                    let sheet = wb.settings_sheet().unwrap();
                    (sheet.page().to_owned(), sheet.namespace().to_owned())
                });
                assert_eq!(at_page, page.id, "`{section}.{key}`");
                // It opens at the first section the words match: this one, or one before it.
                let place = |s: &str| page.sections.iter().position(|x| *x == s).unwrap();
                assert!(
                    place(&at) <= place(section),
                    "`{section}.{key}` opens at {at}"
                );
                checked += 1;
            }
        }
    }
    // No key was skipped silently.
    assert!(checked > 100, "{checked} keys searched");
}

#[gpui::test]
fn a_search_scrolls_the_page_to_the_section_that_matched(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    cx.dispatch_action(actions::OpenSettingsFor("app".into()));
    draw(cx);
    settle(cx);
    // The page stays the page; the section that says "Dock badge" comes to its top.
    view.update(cx, |wb, cx| wb.set_settings_query("Dock badge", cx));
    assert_eq!(
        view.read_with(cx, |wb, _| {
            let sheet = wb.settings_sheet().unwrap();
            (sheet.page().to_owned(), sheet.namespace().to_owned())
        }),
        ("app".to_owned(), "notifications".to_owned())
    );
    wait_until(cx, "the section to come to the top", |cx| {
        section_in_place(&view, cx, "notifications")
    });
}

// ---- the filters ----------------------------------------------------------------------------

#[gpui::test]
fn modified_leaves_the_rows_a_file_sets_and_the_pages_that_have_one(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    wait_listed(&view, cx);
    cx.dispatch_action(actions::OpenSettingsFor("terminal".into()));
    draw(cx);
    settle(cx);

    // The test daemon's own daemon.toml sets a few keys; no setting of the app is changed.
    view.update(cx, |wb, cx| wb.set_settings_query("@modified", cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().filters()),
        Filters {
            modified: true,
            restart: false
        }
    );
    let before = pages(&view, cx);
    assert!(
        !before.contains(&"appearance") && !before.contains(&"terminal"),
        "{before:?}"
    );
    for row in view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx)) {
        assert!(row.starts_with("d:"), "{row} is not a daemon setting");
    }
    // Nothing matches words that nothing has.
    view.update(cx, |wb, cx| wb.set_settings_query("@modified zzzz", cx));
    assert!(pages(&view, cx).is_empty());
    assert!(
        view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx))
            .is_empty()
    );
    view.update(cx, |wb, cx| wb.set_settings_query("", cx));

    // A setting is changed (a file sets it): it is what `@modified` leaves, with its page.
    view.update(cx, |wb, cx| {
        wb.select_settings_namespace("appearance", cx);
        wb.sheet_choose("density", "comfortable", cx);
        wb.set_settings_query("@modified", cx);
    });
    assert_eq!(pages(&view, cx)[0], "appearance");
    view.update(cx, |wb, cx| wb.select_settings_namespace("appearance", cx));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx)),
        ["f:density"]
    );
    // The words beside it narrow the pages further; the filter stays.
    view.update(cx, |wb, cx| wb.set_settings_query("@MODIFIED zzzz", cx));
    assert!(pages(&view, cx).is_empty());
    view.update(cx, |wb, cx| wb.set_settings_query("density @modified", cx));
    assert_eq!(pages(&view, cx), ["appearance"]);

    // A daemon setting the file sets counts as well.
    view.update(cx, |wb, cx| wb.set_settings_query("", cx));
    view.update(cx, |wb, cx| wb.daemon_toggle("notify.enabled", cx));
    wait_until(cx, "the daemon to write the file", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_settings()
                .entry("notify.enabled")
                .is_some_and(|e| e.set_in_file)
        })
    });
    view.update(cx, |wb, cx| wb.set_settings_query("@modified", cx));
    assert!(pages(&view, cx).contains(&"app"));
    view.update(cx, |wb, cx| wb.select_settings_namespace("app", cx));
    assert!(
        view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx))
            .contains(&"d:notify.enabled".to_owned())
    );
}

#[gpui::test]
fn restart_leaves_the_daemon_settings_that_apply_after_a_restart(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    wait_listed(&view, cx);
    cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
    draw(cx);
    view.update(cx, |wb, cx| wb.set_settings_query("@restart", cx));
    let found = pages(&view, cx);
    assert!(
        found.contains(&"app") && found.contains(&"agents_tools"),
        "{found:?}"
    );
    // The app's own settings apply at once: none is left, and no page of them.
    assert!(
        !found.contains(&"terminal") && !found.contains(&"appearance"),
        "{found:?}"
    );
    // Every row the keyboard can reach is a daemon setting that waits for the restart.
    for page in found {
        view.update(cx, |wb, cx| wb.select_settings_namespace(page, cx));
        let rows = view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx));
        assert!(!rows.is_empty(), "{page}");
        for row in rows {
            let key = row
                .strip_prefix("d:")
                .unwrap_or_else(|| panic!("{page}: {row}"));
            let applies = view.read_with(cx, |wb, _| {
                wb.daemon_settings().entry(key).map(|e| e.applies)
            });
            assert_eq!(
                applies,
                Some(hutan_protocol::daemon_settings::Applies::Restart),
                "{page}: {key}"
            );
        }
    }
    // Both at once: only a setting that waits for the restart and is set in the file.
    view.update(cx, |wb, cx| wb.set_settings_query("@modified @restart", cx));
    for page in pages(&view, cx) {
        view.update(cx, |wb, cx| wb.select_settings_namespace(page, cx));
        for row in view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx)) {
            let key = row
                .strip_prefix("d:")
                .unwrap_or_else(|| panic!("{page}: {row}"));
            let entry = view.read_with(cx, |wb, _| wb.daemon_settings().entry(key).cloned());
            assert!(entry.is_some_and(|e| e.set_in_file), "{page}: {key}");
        }
    }
}

#[gpui::test]
fn the_chips_under_the_search_box_write_the_filters_into_it(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    wait_listed(&view, cx);
    cx.dispatch_action(actions::OpenSettings);
    draw(cx);
    settle(cx);
    let chip = |view: &Entity<Workbench>, cx: &mut VisualTestContext, name: &'static str| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Settings(name)))
            .unwrap_or_else(|| panic!("no chip {name}"))
    };
    let query = |view: &Entity<Workbench>, cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().query().to_owned())
    };

    let at = chip(&view, cx, "filter-restart").center();
    click_at(cx, at);
    assert_eq!(query(&view, cx), "@restart");
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().filters().restart));
    assert!(pages(&view, cx).len() < 14);
    // A second click on the chip switches it off again.
    let at = chip(&view, cx, "filter-restart").center();
    click_at(cx, at);
    assert_eq!(query(&view, cx), "");
    assert_eq!(pages(&view, cx).len(), 14);
    // Typed words stay when a chip is clicked, and typing the filter is the same thing.
    view.update(cx, |wb, cx| wb.set_settings_query("font", cx));
    let at = chip(&view, cx, "filter-modified").center();
    click_at(cx, at);
    assert_eq!(query(&view, cx), "font @modified");
    view.update(cx, |wb, cx| wb.set_settings_query("font", cx));
    assert!(!view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().filters().any()));
}
