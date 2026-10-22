#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The settings sheet's clean look (CL-SETTINGS): rows of one rhythm with a chip and a ghost ↺
//! only where a file sets the value, choices and pickers as popovers that move nothing and give
//! the keyboard back to their row, the page list in groups, and a Problems footer that is one
//! line whatever it lists.

use crate::common;

use std::path::Path;

use common::{TestDaemon, draw, open_with, settle, wait_until};
use gpui::{Bounds, Entity, Pixels, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::settings_schema::NavGroup;
use hutan_desktop::workbench::{PackField, Region, SheetFocus, Workbench, actions};
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

fn open_page(view: &Entity<Workbench>, cx: &mut VisualTestContext, page: &str) {
    cx.dispatch_action(actions::OpenSettingsFor(page.into()));
    draw(cx);
    // The sheet slides in; the tests below measure where its parts sit.
    settle(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some(page.to_owned())
    );
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    region: Region,
) -> Option<Bounds<Pixels>> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn sheet_open(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.settings_sheet().is_some())
}

fn problems_open(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| {
        wb.settings_sheet().is_some_and(|s| s.problems_open())
    })
}

// ---- the Problems footer -------------------------------------------------------------------------

#[gpui::test]
fn the_footer_is_one_line_with_none_three_or_thirty_problems_and_its_list_opens_over_the_page(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "appearance");
    let footer = bounds(&view, cx, Region::Settings("sheet-footer")).unwrap();
    let body = bounds(&view, cx, Region::Settings("sheet-body")).unwrap();
    // The probe is inside the footer's hairline: 32 px measure 31.
    assert_eq!(f32::from(footer.size.height), 31.0);
    assert!(view.read_with(cx, |wb, _| wb.sheet_problems().is_empty()));
    // With nothing to list the footer does not open anything.
    view.update(cx, |wb, cx| wb.sheet_toggle_problems(cx));
    assert!(!problems_open(&view, cx));

    for count in [3_usize, 30] {
        let text: String = (0..count)
            .map(|n| format!("[unknown_{n}]\nx = 1\n"))
            .collect();
        std::fs::write(daemon.home().join("settings.toml"), text).unwrap();
        wait_until(cx, "the problems of the file", |cx| {
            view.read_with(cx, |wb, _| wb.sheet_problems().len() >= count)
        });
        assert_eq!(
            bounds(&view, cx, Region::Settings("sheet-footer")),
            Some(footer),
            "{count} problems"
        );
        assert_eq!(
            bounds(&view, cx, Region::Settings("sheet-body")),
            Some(body),
            "{count} problems"
        );

        // Open: the list is over the page, which and the footer keep their size.
        view.update(cx, |wb, cx| wb.sheet_toggle_problems(cx));
        assert!(problems_open(&view, cx));
        assert_eq!(
            bounds(&view, cx, Region::Settings("sheet-footer")),
            Some(footer),
            "{count} problems, list open"
        );
        assert_eq!(
            bounds(&view, cx, Region::Settings("sheet-body")),
            Some(body),
            "{count} problems, list open"
        );
        // Escape closes the list first, then the sheet.
        cx.simulate_keystrokes("escape");
        assert!(!problems_open(&view, cx));
        assert!(sheet_open(&view, cx));
    }

    // The list goes with the page it was opened on.
    view.update(cx, |wb, cx| wb.sheet_toggle_problems(cx));
    assert!(problems_open(&view, cx));
    view.update(cx, |wb, cx| wb.select_settings_namespace("terminal", cx));
    assert!(!problems_open(&view, cx));
    cx.simulate_keystrokes("escape");
    assert!(!sheet_open(&view, cx));
}

// ---- rows ----------------------------------------------------------------------------------------

#[gpui::test]
fn a_row_is_56_px_with_a_chip_and_a_ghost_reset_only_while_a_file_sets_its_value(
    cx: &mut TestAppContext,
) {
    // The probes are inside the row's 1 px hairline: a 56 px row measures 55.
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "appearance");
    let height = |view: &Entity<Workbench>, cx: &mut VisualTestContext| {
        f32::from(
            bounds(view, cx, Region::SettingsItem("row", 0))
                .unwrap()
                .size
                .height,
        )
    };
    // The page's first field, `density`, at its default (compact): a row of 52 px with neither
    // chip nor ↺.
    assert_eq!(height(&view, cx), 51.0);
    assert!(bounds(&view, cx, Region::SettingsItem("chip", 0)).is_none());
    assert!(bounds(&view, cx, Region::SettingsItem("reset", 0)).is_none());

    // Set in your file: the chip and ↺ appear, and the row is 56 px (comfortable).
    view.update(cx, |wb, cx| {
        wb.sheet_write("density", toml::Value::String("comfortable".into()), cx);
    });
    wait_until(cx, "the chip", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::SettingsItem("chip", 0)).is_some()
        })
    });
    assert!(bounds(&view, cx, Region::SettingsItem("reset", 0)).is_some());
    assert_eq!(height(&view, cx), 55.0);

    // ↺ removes the key: both go.
    view.update(cx, |wb, cx| wb.sheet_reset("density", cx));
    wait_until(cx, "the chip to go", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::SettingsItem("chip", 0)).is_none()
                && wb.region_bounds(Region::SettingsItem("reset", 0)).is_none()
        })
    });
    assert_eq!(height(&view, cx), 51.0);
}

// ---- a choice and a picker are popovers ------------------------------------------------------------------

#[gpui::test]
fn a_choice_is_a_popover_that_moves_no_row_and_escape_gives_the_keyboard_back_to_its_row(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "appearance");
    let below = Region::SettingsItem("pack-picker", PackField::LightTheme.index());
    let before = bounds(&view, cx, below).unwrap();
    assert!(bounds(&view, cx, Region::Settings("menu")).is_none());

    // Tab to the controls (search, pages, controls) and open the first row's choice.
    cx.simulate_keystrokes("tab tab");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_focus_zone()),
        Some(SheetFocus::Controls)
    );
    let row = view.read_with(cx, |wb, _| wb.sheet_focused_row().map(str::to_owned));
    assert_eq!(row.as_deref(), Some("f:density"));
    cx.simulate_keystrokes("enter");
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .and_then(|s| s.dropdown().map(str::to_owned))),
        Some("density".to_owned())
    );
    // The options hang in front of the page: the row below has not moved.
    assert!(bounds(&view, cx, Region::Settings("menu")).is_some());
    assert_eq!(bounds(&view, cx, below), Some(before));

    // Escape closes the menu only, and the keyboard is where it was: on that row.
    cx.simulate_keystrokes("escape");
    assert!(sheet_open(&view, cx));
    assert!(bounds(&view, cx, Region::Settings("menu")).is_none());
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_focus_zone()),
        Some(SheetFocus::Controls)
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_focused_row().map(str::to_owned)),
        row
    );

    // The same with the pointer: the click on the head opens it and leaves the row focused.
    let head = bounds(&view, cx, Region::Settings("dropdown")).unwrap();
    cx.simulate_click(head.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(view.read_with(cx, |wb, _| {
        wb.settings_sheet().is_some_and(|s| s.dropdown().is_some())
    }));
    cx.simulate_keystrokes("escape");
    assert!(sheet_open(&view, cx));
    assert!(view.read_with(cx, |wb, _| wb.sheet_focused_row().is_some()));
    cx.simulate_keystrokes("escape");
    assert!(!sheet_open(&view, cx));
}

#[gpui::test]
fn the_picker_closes_on_escape_after_its_actions_menu_and_restores_the_row_focus(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "appearance");
    let field = PackField::DarkTheme;
    let head = bounds(
        &view,
        cx,
        Region::SettingsItem("pack-picker", field.index()),
    )
    .unwrap();
    cx.simulate_click(head.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.open_pack_picker()),
        Some(field)
    );
    // The pack list is in the popover; the row below is where it was.
    assert!(bounds(&view, cx, Region::SettingsItem("pack-option", 0)).is_some());

    // The actions are behind ⋯, a menu of their own.
    assert!(
        bounds(
            &view,
            cx,
            Region::SettingsItem("pack-reload", field.index())
        )
        .is_none()
    );
    let more = bounds(&view, cx, Region::SettingsItem("pack-more", field.index())).unwrap();
    cx.simulate_click(more.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |wb, _| wb.settings_sheet().and_then(|s| s.pack_menu())),
        Some(field)
    );
    assert!(
        bounds(
            &view,
            cx,
            Region::SettingsItem("pack-reload", field.index())
        )
        .is_some()
    );

    // Escape closes the menu, then the picker, then the sheet.
    cx.simulate_keystrokes("escape");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.settings_sheet().and_then(|s| s.pack_menu())),
        None
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.open_pack_picker()),
        Some(field)
    );
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |wb, _| wb.open_pack_picker()), None);
    assert!(sheet_open(&view, cx));
    // The keyboard is on the row of the picker that closed.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_focused_row().map(str::to_owned)),
        Some("f:dark_theme".to_owned())
    );
    cx.simulate_keystrokes("escape");
    assert!(!sheet_open(&view, cx));
}

// ---- the page list ---------------------------------------------------------------------------------------

#[gpui::test]
fn the_page_list_is_in_groups_and_advanced_opens_on_demand_by_search_or_by_its_pages(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "appearance");
    let nav = |view: &Entity<Workbench>, cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| wb.sheet_nav())
    };

    let groups = nav(&view, cx);
    assert_eq!(
        groups.iter().map(|(g, _, _)| *g).collect::<Vec<_>>(),
        NavGroup::ALL
    );
    // Every page is in exactly one group, and only Advanced starts closed.
    let listed: usize = groups.iter().map(|(_, _, pages)| pages.len()).sum();
    assert_eq!(listed, view.read_with(cx, |wb, _| wb.sheet_pages().len()));
    for (group, open, _) in &groups {
        assert_eq!(*open, *group != NavGroup::Advanced, "{group:?}");
    }
    assert_eq!(groups[0].2[0], "appearance", "the first page leads");

    // The user opens it, and closes it again.
    view.update(cx, |wb, cx| wb.sheet_toggle_advanced(cx));
    assert!(nav(&view, cx).last().unwrap().1);
    view.update(cx, |wb, cx| wb.sheet_toggle_advanced(cx));
    assert!(!nav(&view, cx).last().unwrap().1);

    // Selecting one of its pages (the arrow keys can) keeps it open; so does a search.
    view.update(cx, |wb, cx| wb.select_settings_namespace("resources", cx));
    assert!(nav(&view, cx).last().unwrap().1);
    open_page(&view, cx, "appearance");
    assert!(!nav(&view, cx).last().unwrap().1);
    view.update(cx, |wb, cx| wb.set_settings_query("approval", cx));
    let found = nav(&view, cx);
    let advanced = found
        .iter()
        .find(|(g, _, _)| *g == NavGroup::Advanced)
        .unwrap();
    assert!(advanced.1 && advanced.2.contains(&"control"), "{found:?}");
}
