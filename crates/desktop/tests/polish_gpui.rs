#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Orca-style polish (OL-4): the window title follows the center, the nav rows' metrics, the
//! Agents page, the Assistant header at narrow widths, and the icons.

mod common;

use common::{offline_connection, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::icons::Icon;
use hutan_desktop::workbench::model::{DockPosition, FocusTarget, ItemRef, PanelId};
use hutan_desktop::workbench::{Region, Workbench};

fn open_offline(
    cx: &mut TestAppContext,
    assistant: bool,
) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    if assistant {
        settings
            .write_user("assistant.enabled", toml::Value::Boolean(true))
            .unwrap();
    }
    open_with(cx, settings, connection, None)
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    region: Region,
) -> gpui::Bounds<gpui::Pixels> {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region))
            .is_some()
    });
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap()
}

fn painted(view: &Entity<Workbench>, cx: &VisualTestContext, region: Region) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .is_some()
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    let at = bounds(view, cx, region).center();
    cx.simulate_click(at, Modifiers::none());
}

// ---- the window title -------------------------------------------------------------------------

#[gpui::test]
fn the_window_title_follows_the_center_item_never_a_sidebar_page(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx, true);
    common::draw(cx);
    assert_eq!(cx.window_title().as_deref(), Some("Welcome — Hutan"));

    // Left pages: Tasks, then the Assistant.
    click(&view, cx, Region::Nav("tasks"));
    common::draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().focus),
        FocusTarget::Dock(DockPosition::Left),
        "the page has the keyboard"
    );
    assert_eq!(view.read_with(cx, |wb, cx| wb.active_title(cx)), "Welcome");
    assert_eq!(cx.window_title().as_deref(), Some("Welcome — Hutan"));
    // Mawas Home is a center tab: it is the title while it shows.
    click(&view, cx, Region::Nav("assistant"));
    common::draw(cx);
    assert_eq!(cx.window_title().as_deref(), Some("Mawas — Hutan"));
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.close_panel(&hutan_desktop::workbench::model::PanelId::new("assistant"))
                .unwrap();
        })
    });
    common::draw(cx);
    assert_eq!(cx.window_title().as_deref(), Some("Welcome — Hutan"));

    // Every tab of the right sidebar: History, Changes, Requests, Agents, Resources.
    // (History is open already: its icon collapses the body, so it goes last.)
    for index in [1, 2, 3, 4, 0] {
        click(&view, cx, Region::DockTab(DockPosition::Right, index));
        common::draw(cx);
        assert_eq!(
            view.read_with(cx, |wb, _| wb.state().focus),
            FocusTarget::Dock(DockPosition::Right)
        );
        assert_eq!(
            cx.window_title().as_deref(),
            Some("Welcome — Hutan"),
            "right tab {index}"
        );
    }
}

// ---- the nav ----------------------------------------------------------------------------------

#[gpui::test]
fn nav_rows_are_thirty_pixels_with_a_real_icon(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx, true);
    let entries = view.read_with(cx, |wb, cx| wb.nav_entries(cx));
    let icons: Vec<(String, Icon)> = entries.into_iter().map(|e| (e.key, e.icon)).collect();
    assert_eq!(
        icons,
        [
            ("search".to_owned(), Icon::NavSearch),
            ("tasks".to_owned(), Icon::NavTasks),
            ("agents".to_owned(), Icon::NavAgents),
            ("assistant".to_owned(), Icon::NavMawas),
        ]
    );
    let mut previous_bottom = None;
    for key in ["search", "tasks", "agents", "assistant"] {
        let row = bounds(&view, cx, Region::Nav(key));
        let height = f32::from(row.size.height);
        assert!(
            (28.0..=32.0).contains(&height),
            "{key} is {height} px high, Orca's rows are about 30"
        );
        if let Some(bottom) = previous_bottom {
            // 2 px between two rows.
            let gap: f32 = f32::from(row.origin.y) - bottom;
            assert!((gap - 2.0).abs() <= 1.0, "{key}: gap {gap}");
        }
        previous_bottom = Some(f32::from(row.bottom()));
        // 8 px of padding around the list: the row spans the sidebar minus 2 × 8 px.
        let dock = bounds(&view, cx, Region::Dock(DockPosition::Left));
        let inset: f32 = f32::from(row.origin.x - dock.origin.x);
        assert!((inset - 8.0).abs() <= 1.0, "{key}: inset {inset}");
    }
}

#[gpui::test]
fn every_registered_panel_has_a_semantic_icon(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx, false);
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.open_panel(&PanelId::new("notifications"), DockPosition::Left);
        });
    });
    let entries = view.read_with(cx, |wb, cx| wb.nav_entries(cx));
    let notifications = entries.iter().find(|e| e.key == "notifications").unwrap();
    assert_eq!(notifications.icon, Icon::PanelNotifications);
    assert_eq!(Icon::for_panel("resources"), Icon::PanelResources);
}

// ---- the Agents page ---------------------------------------------------------------------------

#[gpui::test]
fn the_agents_row_opens_one_retained_center_page_and_a_second_click_only_focuses_it(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open_offline(cx, false);
    let page = ItemRef::panel("agents-dashboard");
    assert_ne!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(page.clone())
    );
    click(&view, cx, Region::Nav("agents"));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(page.clone()),
        "the nav row shows the page as a center tab"
    );
    let selected: Vec<String> = view.read_with(cx, |wb, cx| {
        wb.nav_entries(cx)
            .into_iter()
            .filter(|e| e.selected)
            .map(|e| e.key)
            .collect()
    });
    assert_eq!(selected, ["agents"], "the open page marks its row");
    assert_eq!(cx.window_title().as_deref(), Some("Agents — Hutan"));
    // Again: it focuses the existing tab and closes nothing.
    click(&view, cx, Region::Nav("agents"));
    let tabs = view.read_with(cx, |wb, _| {
        wb.state()
            .panes()
            .iter()
            .flat_map(|p| p.items.iter())
            .filter(|i| **i == ItemRef::panel("agents-dashboard"))
            .count()
    });
    assert_eq!(tabs, 1, "one instance");
    // The palette command shows it too, and the drawer's Escape is gone with the drawer.
    cx.dispatch_action(hutan_desktop::workbench::actions::ShowAgentsPage);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(page)
    );
}

// ---- the Assistant page at narrow widths ---------------------------------------------------------

#[gpui::test]
fn mawas_homes_header_is_one_row_whose_controls_stay_inside_it(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx, true);
    click(&view, cx, Region::Nav("assistant"));
    for width in [220.0_f32, 280.0] {
        view.update(cx, |wb, cx| {
            wb.edit_layout(cx, |s| s.resize_dock(DockPosition::Left, width));
        });
        common::draw(cx);
        let header = bounds(&view, cx, Region::Assistant("header"));
        assert!(
            f32::from(header.size.height) <= 40.0,
            "one row at {width}: {} px",
            header.size.height
        );
        // The mode chip, New chat, Stop and Today are there once connected; offline the page
        // shows its header with the controls that need no daemon.
        for region in ["scope", "backend", "mode", "new", "stop", "rail"] {
            let element = bounds(&view, cx, Region::Assistant(region));
            assert!(
                element.origin.x >= header.origin.x - gpui::px(1.0)
                    && element.right() <= header.right() + gpui::px(1.0),
                "{region} sticks out of the header at {width}: {element:?} in {header:?}"
            );
            assert!(
                element.origin.y >= header.origin.y - gpui::px(1.0)
                    && element.bottom() <= header.bottom() + gpui::px(1.0),
                "{region} is in the header's row at {width}"
            );
        }
        // The retired second row and its buttons are gone.
        assert!(!painted(&view, cx, Region::Assistant("global")));
        assert!(!painted(&view, cx, Region::Assistant("workspace")));
    }
}

#[gpui::test]
fn opening_the_agents_overview_shows_the_page_in_the_center_not_in_the_sidebar(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open_offline(cx, false);
    cx.dispatch_action(hutan_desktop::workbench::actions::ToggleOverview);
    let s = view.read_with(cx, |wb, _| wb.state().clone());
    assert_eq!(s.preset, "overview");
    assert!(
        s.docks
            .left
            .panels
            .iter()
            .all(|p| p.as_str() != "agents-dashboard")
    );
    assert!(
        s.active_pane()
            .unwrap()
            .items
            .contains(&ItemRef::panel("agents-dashboard"))
    );
}
