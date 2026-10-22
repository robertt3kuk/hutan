#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Every widget configurable where it is (WIDGET-CONFIG): the panel menu (⋯ in a panel's header,
//! right click on its rail icon), *Move to ▸*, *Hide*, the panel settings popover, the rail's,
//! the docks' and the status bar's checklists and their reordering, the sidebar's sections, and
//! the one path they share: written into the current preset of `settings.toml` (a built-in one is
//! never edited in place: a *Custom* preset appears), live, and undone from the toast or with ⌘Z.
//! No daemon: layout and settings only.

use crate::common;

use common::{offline_connection, open_with, settle};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::settings::LayoutSettings;
use hutan_desktop::workbench::model::{
    DockPosition, LayoutPreset, PanelDest, PanelId, WorkbenchState,
};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_settings::SettingsStore;
use std::path::PathBuf;

fn open_in(
    cx: &mut TestAppContext,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, SettingsStore, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    let (view, cx) = open_with(cx, settings.clone(), connection, state_path);
    (view, settings, cx)
}

fn open(cx: &mut TestAppContext) -> (Entity<Workbench>, SettingsStore, &mut VisualTestContext) {
    open_in(cx, None)
}

fn state(view: &Entity<Workbench>, cx: &VisualTestContext) -> WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

fn ids(panels: &[&str]) -> Vec<PanelId> {
    panels.iter().map(|p| PanelId::new(*p)).collect()
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    common::wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region))
            .is_some()
    });
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap()
        .center();
    cx.simulate_click(at, Modifiers::none());
}

fn menu(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<(String, bool)> {
    view.read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("a menu is open")
        .into_iter()
        .map(|(label, checked)| (label.to_string(), checked))
        .collect()
}

fn labels(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    menu(view, cx).into_iter().map(|(label, _)| label).collect()
}

fn run_entry(view: &Entity<Workbench>, cx: &mut VisualTestContext, label: &str) {
    let index = labels(view, cx)
        .iter()
        .position(|l| l == label)
        .unwrap_or_else(|| panic!("{label} in {:?}", labels(view, cx)));
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(index, window, cx));
}

fn layout(settings: &SettingsStore) -> std::sync::Arc<LayoutSettings> {
    settings.get::<LayoutSettings>()
}

// ---- the panel menu ---------------------------------------------------------------------------

#[gpui::test]
fn a_move_from_the_panel_menu_goes_into_a_custom_preset_and_the_built_in_one_stays(
    cx: &mut TestAppContext,
) {
    let (view, settings, cx) = open(cx);
    assert!(layout(&settings).presets.is_empty());

    // The ⋯ in the right panel's header: Changes has no options of its own, so only placement.
    click(&view, cx, Region::PanelMenu("right"));
    assert_eq!(labels(&view, cx), ["Move to", "Hide"]);
    run_entry(&view, cx, "Move to");
    assert_eq!(labels(&view, cx), ["Left", "Right", "Bottom", "Center tab"]);
    assert_eq!(
        menu(&view, cx).iter().filter(|(_, on)| *on).count(),
        1,
        "the current place is checked"
    );
    run_entry(&view, cx, "Left");

    // Live: Changes is a page of the left dock now.
    let s = state(&view, cx);
    assert!(s.docks.left.panels.contains(&PanelId::new("changes")));
    assert!(!s.docks.right.panels.contains(&PanelId::new("changes")));

    // Kept: the built-in preset is untouched and a Custom one holds the change and is current.
    assert_eq!(s.preset, "custom");
    let saved = layout(&settings);
    assert_eq!(saved.preset, "custom");
    let custom = saved.presets.get("custom").expect("a Custom preset");
    assert!(custom.left.panels.iter().any(|p| p == "changes"));
    assert!(!custom.right.panels.iter().any(|p| p == "changes"));
    let defs = saved.preset_defs();
    let default = defs.iter().find(|d| d.layout.name == "default").unwrap();
    assert_eq!(default.layout.docks, LayoutPreset::default_preset().docks);
    assert!(!saved.presets.contains_key("default"));
    // The bar was never customised: Custom keeps following the built-in bar.
    let text = std::fs::read_to_string(settings.paths().unwrap().user).unwrap();
    assert!(!text.contains("status_bar"), "{text}");

    // The toast says what happened and, in one line, why Custom exists.
    let toast = view
        .read_with(cx, |wb, _| wb.layout_toast().cloned())
        .unwrap();
    assert_eq!(toast.label(), "Moved Changes to the left");
    assert!(
        toast.note().unwrap().contains("Default is built in"),
        "{:?}",
        toast.note()
    );

    // A second change goes into Custom itself; no third preset appears and no note is shown.
    view.update(cx, |wb, cx| {
        wb.place_panel(
            &PanelId::new("history"),
            PanelDest::Dock(DockPosition::Left),
            cx,
        );
    });
    assert_eq!(layout(&settings).presets.len(), 1);
    let toast = view
        .read_with(cx, |wb, _| wb.layout_toast().cloned())
        .unwrap();
    assert_eq!(toast.label(), "Moved History to the left");
    assert!(toast.note().is_none());
}

#[gpui::test]
fn undo_from_the_toast_restores_the_panel_and_the_settings(cx: &mut TestAppContext) {
    let (view, settings, cx) = open(cx);
    let before = state(&view, cx).docks;
    view.update(cx, |wb, cx| {
        wb.place_panel(
            &PanelId::new("files"),
            PanelDest::Dock(DockPosition::Left),
            cx,
        );
    });
    assert_ne!(state(&view, cx).docks, before);
    settle(cx);
    click(&view, cx, Region::LayoutToast("undo"));
    let s = state(&view, cx);
    assert_eq!(s.docks.left, before.left);
    assert_eq!(s.docks.right, before.right);
    assert_eq!(s.preset, "default");
    assert!(view.read_with(cx, |wb, _| wb.layout_toast().is_none()));
    let saved = layout(&settings);
    assert!(saved.presets.is_empty(), "Custom is gone again");
    assert_eq!(saved.preset, "default");
    let text = std::fs::read_to_string(settings.paths().unwrap().user).unwrap();
    assert!(!text.contains("custom"), "{text}");
}

#[gpui::test]
fn cmd_z_undoes_the_last_layout_change_while_its_toast_shows(cx: &mut TestAppContext) {
    let (view, settings, cx) = open(cx);
    let before = state(&view, cx).docks;
    view.update(cx, |wb, cx| {
        wb.place_panel(
            &PanelId::new("agents"),
            PanelDest::Dock(DockPosition::Bottom),
            cx,
        );
    });
    assert_eq!(
        state(&view, cx).docks.bottom.panels,
        ids(&["agents"]),
        "the bottom dock holds it now"
    );
    cx.simulate_keystrokes("cmd-z");
    assert_eq!(state(&view, cx).docks, before);
    assert!(layout(&settings).presets.is_empty());
    // With nothing to undo ⌘Z does nothing (and the next one is a text field's).
    cx.simulate_keystrokes("cmd-z");
    assert_eq!(state(&view, cx).docks, before);
}

#[gpui::test]
fn the_toast_goes_after_ten_seconds_and_takes_the_undo_with_it(cx: &mut TestAppContext) {
    let (view, _settings, cx) = open(cx);
    view.update(cx, |wb, cx| {
        wb.place_panel(
            &PanelId::new("agents"),
            PanelDest::Dock(DockPosition::Left),
            cx,
        );
    });
    assert!(view.read_with(cx, |wb, _| wb.layout_toast().is_some()));
    cx.executor().advance_clock(
        hutan_desktop::workbench::LAYOUT_TOAST_TIME + std::time::Duration::from_secs(1),
    );
    cx.run_until_parked();
    assert!(view.read_with(cx, |wb, _| wb.layout_toast().is_none()));
    let moved = state(&view, cx).docks;
    cx.simulate_keystrokes("cmd-z");
    assert_eq!(state(&view, cx).docks, moved, "too late to undo");
}

// ---- every kind of widget moves between the docks and the center, and back --------------------

#[gpui::test]
fn each_kind_of_widget_moves_between_docks_and_the_center_and_back(cx: &mut TestAppContext) {
    use hutan_desktop::workbench::actions::OpenPanel;
    let (view, _settings, cx) = open(cx);
    cx.dispatch_action(OpenPanel("problems".into()));
    cx.dispatch_action(OpenPanel("assistant".into()));
    cx.dispatch_action(OpenPanel("agents-dashboard".into()));
    // A left page (Tasks), a right panel (History), a bottom panel (Problems), and the two pages
    // of the center (Mawas Home and the Agents page).
    for (panel, home) in [
        ("tasks", Some(DockPosition::Left)),
        ("history", Some(DockPosition::Right)),
        ("problems", Some(DockPosition::Bottom)),
        ("assistant", None),
        ("agents-dashboard", None),
    ] {
        let id = PanelId::new(panel);
        let at = |cx: &VisualTestContext| state(&view, cx).docks.find(&id);
        assert_eq!(at(cx), home, "{panel} starts at {home:?}");
        let mut route = vec![
            PanelDest::Dock(DockPosition::Left),
            PanelDest::Dock(DockPosition::Right),
            PanelDest::Dock(DockPosition::Bottom),
            PanelDest::Center,
        ];
        route.push(match home {
            Some(home) => PanelDest::Dock(home),
            None => PanelDest::Center,
        });
        for dest in route {
            let id = id.clone();
            view.update(cx, |wb, cx| wb.place_panel(&id, dest, cx));
            let s = state(&view, cx);
            match dest {
                PanelDest::Dock(pos) => {
                    assert_eq!(s.docks.find(&id), Some(pos), "{panel} → {pos:?}");
                }
                PanelDest::Center => {
                    assert_eq!(s.docks.find(&id), None, "{panel} → center");
                    assert!(
                        s.find_panel(&id).is_some(),
                        "{panel} is a tab of the center: {:?}",
                        s.center
                    );
                }
            }
        }
        assert_eq!(at(cx), home, "{panel} is back");
    }
}

#[gpui::test]
fn hide_closes_the_panel_and_the_preset_keeps_it_hidden(cx: &mut TestAppContext) {
    let (view, settings, cx) = open(cx);
    click(&view, cx, Region::PanelMenu("right"));
    run_entry(&view, cx, "Hide");
    let s = state(&view, cx);
    assert!(!s.docks.right.panels.contains(&PanelId::new("changes")));
    let saved = layout(&settings);
    let custom = saved.presets.get("custom").expect("Custom");
    assert!(!custom.right.panels.iter().any(|p| p == "changes"));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.layout_toast().map(|t| t.label().to_owned())),
        Some("Hid Changes".to_owned())
    );
    // Opening it again from the palette's command brings it back where the registry says.
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenPanel(
        "changes".into(),
    ));
    assert!(
        state(&view, cx)
            .docks
            .right
            .panels
            .contains(&PanelId::new("changes"))
    );
}

// ---- the panel settings popover -----------------------------------------------------------------

#[gpui::test]
fn the_panel_settings_popover_edits_the_panels_keys_live(cx: &mut TestAppContext) {
    use hutan_desktop::settings::SessionsPanelSettings;
    let (view, settings, cx) = open(cx);
    // The Projects panel's namespace is `panels.sessions`; its ⋯ is the sidebar's, so open the
    // popover the way every menu does: *Panel settings…* of the Tasks page's menu.
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenPanel(
        "sessions".into(),
    ));
    view.update_in(cx, |wb, window, cx| {
        wb.open_panel_settings(
            &PanelId::new("sessions"),
            gpui::point(gpui::px(300.0), gpui::px(60.0)),
            window,
            cx,
        );
    });
    common::draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.panel_settings_open().cloned()),
        Some(PanelId::new("sessions"))
    );
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::PanelSettings("popover")).is_some()
    }));
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("panels.sessions".to_owned())
    );
    assert!(settings.get::<SessionsPanelSettings>().needs_you);

    // The same rows as the Settings sheet: the toggle writes `panels.sessions.needs_you`.
    view.update(cx, |wb, cx| wb.sheet_toggle("needs_you", cx));
    assert!(!settings.get::<SessionsPanelSettings>().needs_you);
    let text = std::fs::read_to_string(settings.paths().unwrap().user).unwrap();
    assert!(text.contains("needs_you = false"), "{text}");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_value("needs_you")),
        Some(serde_json::Value::Bool(false)),
        "the popover shows the new value"
    );

    // *Open in Settings* turns the popover into the sheet on the same page.
    click(&view, cx, Region::PanelSettings("open"));
    assert!(view.read_with(cx, |wb, _| wb.panel_settings_open().is_none()));
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("panels.sessions".to_owned())
    );
    // Escape closes it.
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_none()));
}

#[gpui::test]
fn a_panel_without_options_has_no_settings_entry(cx: &mut TestAppContext) {
    let (view, _settings, cx) = open(cx);
    // Changes: placement only.
    click(&view, cx, Region::PanelMenu("right"));
    assert!(!labels(&view, cx).iter().any(|l| l == "Panel settings…"));
    cx.simulate_keystrokes("escape");
    // Files has `panels.files`.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.open_panel(&PanelId::new("files"), DockPosition::Right)
        });
    });
    common::draw(cx);
    click(&view, cx, Region::PanelMenu("right"));
    assert!(labels(&view, cx).iter().any(|l| l == "Panel settings…"));
}

// ---- rails and docks ----------------------------------------------------------------------------

fn right_click(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    at: gpui::Point<gpui::Pixels>,
) {
    let _ = view;
    cx.simulate_mouse_down(at, gpui::MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(at, gpui::MouseButton::Right, Modifiers::none());
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    region: Region,
) -> gpui::Bounds<gpui::Pixels> {
    common::wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region))
            .is_some()
    });
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap()
}

/// A point of the right rail below its icons, where the rail's own menu opens.
fn rail_filler(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> gpui::Point<gpui::Pixels> {
    let rail = bounds(view, cx, Region::Rail(DockPosition::Right));
    gpui::point(rail.center().x, rail.bottom() - gpui::px(90.0))
}

#[gpui::test]
fn the_right_rail_checklist_shows_and_hides_panels_and_stays_open(cx: &mut TestAppContext) {
    let (view, settings, cx) = open(cx);
    let at = rail_filler(&view, cx);
    right_click(&view, cx, at);
    let entries = menu(&view, cx);
    assert_eq!(entries[0].0, "Right rail panels", "{entries:?}");
    let shown: Vec<&str> = entries
        .iter()
        .filter(|(_, on)| *on)
        .map(|(l, _)| l.as_str())
        .collect();
    // In the order of the registry, which does not change while the list is used.
    assert_eq!(
        shown,
        [
            "Files",
            "History",
            "Changes",
            "Requests",
            "Agents",
            "Resources"
        ]
    );
    assert!(
        entries.iter().any(|(l, on)| l == "Search" && !on),
        "an available panel that is not on the rail is unchecked"
    );
    assert!(
        entries.last().unwrap().0.starts_with("Reset layout to"),
        "{entries:?}"
    );

    // Hide History: the icon goes, the list stays open with it unchecked.
    run_entry(&view, cx, "History");
    assert!(
        !state(&view, cx)
            .docks
            .right
            .panels
            .contains(&PanelId::new("history"))
    );
    let entries = menu(&view, cx);
    assert!(entries.iter().any(|(l, on)| l == "History" && !on));
    // Show it again: it comes back at the end of the rail.
    run_entry(&view, cx, "History");
    let s = state(&view, cx);
    assert_eq!(s.docks.right.panels.last(), Some(&PanelId::new("history")));

    // Custom holds it; the built-in preset is untouched.
    let saved = layout(&settings);
    assert_eq!(saved.preset, "custom");
    assert_eq!(
        saved.presets["custom"]
            .right
            .panels
            .last()
            .map(String::as_str),
        Some("history")
    );
    let defs = saved.preset_defs();
    let default = defs.iter().find(|d| d.layout.name == "default").unwrap();
    assert_eq!(default.layout.docks, LayoutPreset::default_preset().docks);
    // Search shown on the right too.
    run_entry(&view, cx, "Search");
    assert_eq!(
        state(&view, cx).docks.right.panels.last(),
        Some(&PanelId::new("search"))
    );
}

#[gpui::test]
fn a_panel_checked_in_another_docks_list_moves_there(cx: &mut TestAppContext) {
    let (view, _settings, cx) = open(cx);
    let at = rail_filler(&view, cx);
    right_click(&view, cx, at);
    // Tasks lives in the left sidebar: checking it here moves it.
    run_entry(&view, cx, "Tasks");
    let s = state(&view, cx);
    assert!(s.docks.right.panels.contains(&PanelId::new("tasks")));
    assert!(!s.docks.left.panels.contains(&PanelId::new("tasks")));
}

#[gpui::test]
fn the_bottom_docks_strip_and_the_left_nav_have_their_checklists(cx: &mut TestAppContext) {
    let (view, _settings, cx) = open(cx);
    // The left sidebar: right click in the nav's empty room.
    let nav = bounds(&view, cx, Region::Nav("agents"));
    right_click(
        &view,
        cx,
        // The padding around the rows belongs to the nav list.
        gpui::point(nav.left() - gpui::px(4.0), nav.center().y),
    );
    let entries = menu(&view, cx);
    assert_eq!(entries[0].0, "Left sidebar pages", "{entries:?}");
    assert!(entries.iter().any(|(l, on)| l == "Projects" && *on));
    cx.simulate_keystrokes("escape");

    // The bottom dock: show it with Problems, then right click the tab strip.
    view.update(cx, |wb, cx| {
        wb.show_panel(&PanelId::new("problems"), DockPosition::Bottom, cx);
    });
    common::draw(cx);
    let dock = bounds(&view, cx, Region::Dock(DockPosition::Bottom));
    right_click(
        &view,
        cx,
        gpui::point(dock.right() - gpui::px(40.0), dock.top() + gpui::px(14.0)),
    );
    let entries = menu(&view, cx);
    assert_eq!(entries[0].0, "Bottom panels", "{entries:?}");
    assert!(entries.iter().any(|(l, on)| l == "Problems" && *on));
    // Hiding the last panel of the bottom dock leaves it empty.
    run_entry(&view, cx, "Problems");
    assert!(state(&view, cx).docks.bottom.panels.is_empty());
}

/// Press at `from`, move in small steps with the button held, release at `to`.
fn drag(
    cx: &mut VisualTestContext,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
) {
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::none());
    for i in 1..=8 {
        let t = i as f32 / 8.0;
        let p = gpui::point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        cx.simulate_mouse_move(p, gpui::MouseButton::Left, Modifiers::none());
    }
    cx.simulate_mouse_up(to, gpui::MouseButton::Left, Modifiers::none());
}

#[gpui::test]
fn dragging_a_rail_icon_reorders_the_rail_and_the_order_survives_a_restart(
    cx: &mut TestAppContext,
) {
    let home = tempfile::tempdir().unwrap();
    let path = home
        .path()
        .join(hutan_desktop::workbench::persistence::STATE_FILE);
    let (connection, settings, _) = offline_connection();
    let order = {
        let (view, cx) = open_with(cx, settings.clone(), connection, Some(path.clone()));
        let before = state(&view, cx).docks.right.panels;
        assert_eq!(before[0], PanelId::new("files"));
        // Files is dropped on Agents' icon (the third).
        let from = bounds(&view, cx, Region::DockTab(DockPosition::Right, 0)).center();
        let to = bounds(&view, cx, Region::DockTab(DockPosition::Right, 2)).center();
        drag(cx, from, to);
        let after = state(&view, cx).docks.right.panels;
        assert_ne!(after, before);
        assert_ne!(after[0], PanelId::new("files"), "{after:?}");
        assert_eq!(after.len(), before.len());
        // Written into Custom, with a toast.
        let saved = layout(&settings);
        assert_eq!(saved.preset, "custom");
        let custom: Vec<PanelId> = saved.presets["custom"]
            .right
            .panels
            .iter()
            .map(|p| PanelId::new(p.as_str()))
            .collect();
        assert_eq!(custom, after);
        assert_eq!(
            view.read_with(cx, |wb, _| wb.layout_toast().map(|t| t.label().to_owned())),
            Some("Reordered the right panels".to_owned())
        );
        view.update(cx, |wb, _| wb.flush_save());
        after
    };

    // A restart with the saved layout: the same order.
    let (connection, _, _) = offline_connection();
    let (view, cx) = open_with(cx, settings.clone(), connection, Some(path.clone()));
    assert_eq!(state(&view, cx).docks.right.panels, order);
    assert_eq!(state(&view, cx).preset, "custom");
    // And a fresh start without the layout file takes the order from the preset.
    let (connection, _, _) = offline_connection();
    let (view, cx) = open_with(cx, settings, connection, None);
    assert_eq!(state(&view, cx).preset, "custom");
    assert_eq!(state(&view, cx).docks.right.panels, order);
}
