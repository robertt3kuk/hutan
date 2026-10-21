#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests: actions mutate the workbench, the palette filters and runs commands, and layout
//! state survives a restart through `desktop-state.json` (driven with a temp `HUTAN_HOME`).

use crate::common;

use common::{offline_connection, open_with};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::theme::Density;
use hutan_desktop::workbench::actions::{self, palette};
use hutan_desktop::workbench::model::{
    DockPosition, FocusTarget, ItemRef, PanelId, WorkbenchState,
};
use hutan_desktop::workbench::persistence::{STATE_FILE, state_path_from};
use hutan_desktop::workbench::{Region, SAVE_DEBOUNCE, Workbench};
use std::path::{Path, PathBuf};

/// A workbench without a daemon: layout behavior only. `state_path` `None` disables persistence.
fn open(
    cx: &mut TestAppContext,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    open_with(cx, settings, connection, state_path)
}

/// Adds a placeholder tab to the focused pane (layouts the UI has no gesture for).
fn new_tab(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.add_item(pane, ItemRef::placeholder("New tab")).unwrap();
        });
    });
}

fn state(view: &Entity<Workbench>, cx: &VisualTestContext) -> WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

/// The path `HUTAN_HOME=<home>` resolves to.
fn hutan_home_state_path(home: &Path) -> PathBuf {
    state_path_from(Some(home.into()), None).expect("HUTAN_HOME resolves")
}

#[gpui::test]
fn opens_with_default_layout_and_title(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    let state = state(&view, cx);
    assert_eq!(state.preset, "default");
    assert!(state.docks.left.visible && state.docks.right.visible && !state.docks.bottom.visible);
    assert_eq!(
        state.docks.left.panels,
        vec![
            PanelId::new("sessions"),
            PanelId::new("tasks"),
            PanelId::new("files")
        ]
    );
    assert_eq!(state.docks.right.active, Some(PanelId::new("changes")));
    assert_eq!(state.panes().len(), 1);
    assert_eq!(cx.window_title().as_deref(), Some("Welcome — Hutan"));
}

#[gpui::test]
fn split_actions_and_keys_change_the_pane_tree(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.dispatch_action(actions::SplitRight);
    assert_eq!(state(&view, cx).panes().len(), 2);
    // Without a daemon the pane is an empty placeholder and a toast says why.
    let notice = view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string));
    assert!(notice.unwrap().contains("Not connected"));
    // ⌘D goes through the real keymap.
    cx.simulate_keystrokes("cmd-d");
    assert_eq!(state(&view, cx).panes().len(), 3);
    cx.simulate_keystrokes("cmd-shift-d");
    let s = state(&view, cx);
    assert_eq!(s.panes().len(), 4);
    // The newest pane has focus and shows the placeholder title.
    let FocusTarget::Pane(focused) = s.focus else {
        panic!("a pane should be focused");
    };
    assert_eq!(
        s.pane(focused).unwrap().items,
        vec![ItemRef::placeholder("New pane")]
    );
    assert_eq!(cx.window_title().as_deref(), Some("New pane — Hutan"));
}

#[gpui::test]
fn dock_toggles_zoom_and_density(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.dispatch_action(actions::ToggleLeftDock);
    assert!(!state(&view, cx).docks.left.is_expanded());
    cx.simulate_keystrokes("cmd-b");
    assert!(state(&view, cx).docks.left.is_expanded());
    cx.simulate_keystrokes("cmd-alt-b");
    assert!(!state(&view, cx).docks.right.is_expanded());
    cx.simulate_keystrokes("cmd-j");
    assert!(state(&view, cx).docks.bottom.visible);

    cx.dispatch_action(actions::SplitRight);
    cx.simulate_keystrokes("cmd-shift-enter");
    assert!(state(&view, cx).zoomed.is_some());
    cx.dispatch_action(actions::ToggleZoom);
    assert!(state(&view, cx).zoomed.is_none());

    assert_eq!(view.read_with(cx, |wb, _| wb.density()), Density::Compact);
    cx.dispatch_action(actions::SetDensityComfortable);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.density()),
        Density::Comfortable
    );
    cx.dispatch_action(actions::ToggleDensity);
    assert_eq!(view.read_with(cx, |wb, _| wb.density()), Density::Compact);
}

#[gpui::test]
fn presets_and_overview_toggle(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.dispatch_action(actions::SplitRight);
    cx.dispatch_action(actions::ApplyPreset("focus".into()));
    let s = state(&view, cx);
    assert_eq!(s.preset, "focus");
    assert!(s.zoomed.is_some());
    assert!(!s.docks.left.visible && !s.docks.right.visible);

    // ⌘⇧1 returns to the default preset, ⌘⇧O toggles the overview and back.
    cx.simulate_keystrokes("cmd-shift-1");
    let s = state(&view, cx);
    assert_eq!(s.preset, "default");
    assert!(s.zoomed.is_none() && s.docks.left.visible);

    cx.simulate_keystrokes("cmd-shift-o");
    let s = state(&view, cx);
    assert_eq!(s.preset, "overview");
    assert_eq!(
        s.active_pane().unwrap().items[0],
        ItemRef::panel("agents-dashboard")
    );
    assert_eq!(s.panes().len(), 1);
    assert_eq!(
        s.panes()[0].items,
        vec![ItemRef::panel("agents-dashboard")],
        "the board is the only visible center item"
    );
    assert_eq!(
        s.all_items()
            .iter()
            .filter(|i| matches!(i, ItemRef::Placeholder { .. }))
            .count(),
        2,
        "the placeholder panes wait in the stash"
    );

    cx.simulate_keystrokes("cmd-shift-o");
    let s = state(&view, cx);
    assert_eq!(s.preset, "default");
    assert!(
        s.panes()
            .iter()
            .all(|p| !p.items.contains(&ItemRef::panel("agents-dashboard"))),
        "the board left the center (it is the left sidebar's Agents page again)"
    );
}

#[gpui::test]
fn closing_the_last_item_leaves_an_empty_pane(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    new_tab(&view, cx);
    assert_eq!(state(&view, cx).active_pane().unwrap().items.len(), 2);
    cx.simulate_keystrokes("cmd-w");
    cx.simulate_keystrokes("cmd-w");
    let s = state(&view, cx);
    assert_eq!(s.panes().len(), 1);
    assert!(s.active_pane().unwrap().items.is_empty());
    // Closing again with nothing left is a harmless no-op.
    cx.dispatch_action(actions::ClosePaneItem);
    assert!(state(&view, cx).active_pane().unwrap().items.is_empty());
    assert_eq!(cx.window_title().as_deref(), Some("Hutan — Hutan"));
}

#[gpui::test]
fn closing_a_split_pane_collapses_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.dispatch_action(actions::SplitRight);
    assert_eq!(state(&view, cx).panes().len(), 2);
    cx.dispatch_action(actions::ClosePaneItem);
    assert_eq!(state(&view, cx).panes().len(), 1);
}

#[gpui::test]
fn focus_neighbor_and_tab_selection(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.dispatch_action(actions::SplitRight);
    let before = state(&view, cx).focus;
    cx.simulate_keystrokes("cmd-alt-left");
    assert_ne!(state(&view, cx).focus, before);
    new_tab(&view, cx);
    new_tab(&view, cx);
    cx.simulate_keystrokes("cmd-1");
    assert_eq!(state(&view, cx).active_pane().unwrap().active, 0);
    cx.simulate_keystrokes("cmd-3");
    assert_eq!(state(&view, cx).active_pane().unwrap().active, 2);
}

#[gpui::test]
fn panel_actions_move_panels_between_docks(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.dispatch_action(actions::OpenPanel("notifications".into()));
    let s = state(&view, cx);
    assert_eq!(s.docks.bottom.panels, vec![PanelId::new("notifications")]);
    assert!(s.docks.bottom.visible);
    assert_eq!(s.focus, FocusTarget::Dock(DockPosition::Bottom));

    cx.dispatch_action(actions::MoveActivePanel(
        hutan_desktop::workbench::model::PanelDest::Dock(DockPosition::Left),
    ));
    let s = state(&view, cx);
    assert!(s.docks.bottom.panels.is_empty());
    assert!(s.docks.left.panels.contains(&PanelId::new("notifications")));

    cx.dispatch_action(actions::OpenActivePanelAsTab);
    let s = state(&view, cx);
    assert!(!s.docks.left.panels.contains(&PanelId::new("notifications")));
    assert!(
        s.active_pane()
            .unwrap()
            .items
            .contains(&ItemRef::panel("notifications"))
    );
}

#[gpui::test]
fn palette_opens_filters_navigates_and_runs(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.simulate_keystrokes("cmd-shift-p");
    assert!(view.read_with(cx, |wb, _| wb.palette_open()));
    let all = view.read_with(cx, |wb, _| wb.palette_titles());
    assert!(
        all.len() > 20,
        "empty query lists every command, got {}",
        all.len()
    );

    cx.simulate_input("split");
    let titles = view.read_with(cx, |wb, _| wb.palette_titles());
    assert!(!titles.is_empty() && titles.len() < all.len());
    assert!(
        titles.iter().all(|t| t.to_lowercase().contains("s")),
        "{titles:?}"
    );
    assert_eq!(titles[0].as_ref(), "Layout: Split Right");
    assert!(titles.iter().any(|t| t.as_ref() == "Layout: Split Down"));
    assert!(
        !titles
            .iter()
            .any(|t| t.as_ref() == "View: Toggle Left Dock")
    );

    // Backspace widens the filter again, down moves the selection.
    cx.simulate_keystrokes("backspace backspace backspace backspace backspace");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.palette_titles().len()),
        all.len()
    );
    cx.simulate_input("split");
    cx.simulate_keystrokes("down");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.palette().map(|p| p.selected)),
        Some(1)
    );
    cx.simulate_keystrokes("up enter");
    // Enter ran "Split Right" and closed the palette.
    assert!(!view.read_with(cx, |wb, _| wb.palette_open()));
    assert_eq!(state(&view, cx).panes().len(), 2);

    // Escape dismisses without running anything.
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("zzzzqq");
    assert!(view.read_with(cx, |wb, _| wb.palette_titles().is_empty()));
    cx.simulate_keystrokes("enter");
    assert!(!view.read_with(cx, |wb, _| wb.palette_open()));
    assert_eq!(state(&view, cx).panes().len(), 2);
    cx.simulate_keystrokes("cmd-shift-p escape");
    assert!(!view.read_with(cx, |wb, _| wb.palette_open()));
}

#[gpui::test]
fn palette_hints_come_from_the_keymap(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.simulate_keystrokes("cmd-shift-p");
    // Hints come from the live keymap (defaults + keymap.toml), shown in the palette rows.
    let hint = |cx: &mut VisualTestContext, action: &dyn gpui::Action| {
        cx.update(|window, _| {
            window
                .highest_precedence_binding_for_action(action)
                .map(|b| {
                    b.keystrokes()
                        .iter()
                        .map(|k| k.to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
        })
    };
    let split = hint(cx, &actions::SplitRight);
    let toggle = hint(cx, &actions::ToggleOverview);
    assert_eq!(split.as_deref(), Some("⌘D"));
    assert_eq!(toggle.as_deref(), Some("⌘⇧O"));
    cx.dispatch_action(palette::Dismiss);
    assert!(!view.read_with(cx, |wb, _| wb.palette_open()));
}

#[gpui::test]
fn state_is_saved_debounced_and_restored(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let path = hutan_home_state_path(home.path());
    assert_eq!(path, home.path().join(STATE_FILE));

    let saved = {
        let (view, cx) = open(cx, Some(path.clone()));
        cx.dispatch_action(actions::SplitRight);
        cx.dispatch_action(actions::ToggleRightDock);
        cx.dispatch_action(actions::ApplyPreset("focus".into()));
        cx.dispatch_action(actions::ApplyPreset("default".into()));
        cx.dispatch_action(actions::SplitDown);
        // Nothing is written before the debounce elapses.
        assert!(!path.exists());
        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        assert!(path.exists(), "state written after the debounce");
        state(&view, cx)
    };
    assert_eq!(saved.panes().len(), 3);

    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(on_disk["version"], 6);

    // A second window (a "restart") restores exactly that layout.
    let (view2, cx2) = open(cx, Some(path.clone()));
    assert_eq!(state(&view2, cx2), saved);
}

#[gpui::test]
fn many_changes_coalesce_into_one_write(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(STATE_FILE);
    let (view, cx) = open(cx, Some(path.clone()));
    cx.dispatch_action(actions::SplitRight);
    cx.executor().advance_clock(SAVE_DEBOUNCE / 2);
    cx.dispatch_action(actions::SplitRight);
    cx.executor().advance_clock(SAVE_DEBOUNCE / 2);
    cx.run_until_parked();
    assert!(!path.exists(), "second change restarted the debounce");
    cx.executor().advance_clock(SAVE_DEBOUNCE);
    cx.run_until_parked();
    let restored = WorkbenchState::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(restored, state(&view, cx));
    assert_eq!(restored.panes().len(), 3);
}

#[gpui::test]
fn corrupt_or_unknown_version_state_falls_back_to_default(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(STATE_FILE);

    std::fs::write(&path, "{ this is not json").unwrap();
    let (view, cx) = open(cx, Some(path.clone()));
    assert_eq!(state(&view, cx), WorkbenchState::default());

    std::fs::write(&path, r#"{ "version": 99, "docks": {} }"#).unwrap();
    let (view, cx) = open(cx, Some(path.clone()));
    assert_eq!(state(&view, cx), WorkbenchState::default());

    // Missing file (the very first launch): the default, with the right sidebar on its rail;
    // nothing is created until the layout changes.
    std::fs::remove_file(&path).unwrap();
    let (view, cx) = open(cx, Some(path.clone()));
    let mut first_launch = WorkbenchState::default();
    first_launch.docks.right.collapsed = true;
    assert_eq!(state(&view, cx), first_launch);
    assert!(!path.exists());
}

#[gpui::test]
fn flush_writes_immediately_for_quit(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(STATE_FILE);
    let (view, cx) = open(cx, Some(path.clone()));
    cx.dispatch_action(actions::SplitRight);
    view.update(cx, |wb, _| wb.flush_save());
    assert!(path.exists());
}

// ---- painted layout --------------------------------------------------------------------
//
// These assert the real flex layout (no screenshots are taken in this slice).

fn bounds(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> gpui::Bounds<gpui::Pixels> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} was not painted"))
}

fn f(p: gpui::Pixels) -> f32 {
    f32::from(p)
}

/// Probes measure the content box, so a 1 px border makes a dock/bar read 1 px smaller.
#[track_caller]
fn assert_size(actual: gpui::Pixels, expected: f32) {
    assert!(
        (f(actual) - expected).abs() <= 1.0,
        "expected {expected} (±1 border), got {}",
        f(actual)
    );
}

#[gpui::test]
fn default_layout_geometry_matches_the_design_tokens(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    let left = bounds(&view, cx, Region::Dock(DockPosition::Left));
    let right = bounds(&view, cx, Region::Dock(DockPosition::Right));
    let center = bounds(&view, cx, Region::Center);
    let title = bounds(&view, cx, Region::Titlebar);
    let status = bounds(&view, cx, Region::StatusBar);

    assert_size(left.size.width, 280.0);
    assert_size(right.size.width, 350.0);
    assert_size(title.size.height, 36.0);
    assert_size(status.size.height, 24.0); // the status bar token, in both densities
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::Dock(DockPosition::Bottom)))
            .is_none()
    );

    // Left dock | center | right dock fill the window width exactly.
    assert!(f(left.origin.x) <= 1.0);
    assert!((f(left.right()) - f(center.left())).abs() < 2.5);
    assert!((f(center.right()) - f(right.left())).abs() < 2.5);
    let rail = bounds(&view, cx, Region::Rail(DockPosition::Right));
    let window_width =
        f(left.size.width) + f(center.size.width) + f(right.size.width) + f(rail.size.width);
    assert!((window_width - f(status.size.width)).abs() < 5.0);
    // Title bar sits above the center; the left dock spans the full height above the status bar.
    assert!(f(title.origin.y) <= 1.0);
    assert!(f(center.top()) >= f(title.bottom()) - 1.0);
    assert!((f(left.bottom()) - f(status.top())).abs() < 2.5);
    assert!(f(center.size.height) > 100.0);

    cx.dispatch_action(actions::SetDensityComfortable);
    assert_size(bounds(&view, cx, Region::StatusBar).size.height, 24.0);
}

#[gpui::test]
fn splits_divide_the_center_by_ratio_and_zoom_fills_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.dispatch_action(actions::SplitRight);
    let s = state(&view, cx);
    let ids: Vec<_> = s.panes().iter().map(|p| p.id).collect();
    let center = bounds(&view, cx, Region::Center);
    let a = bounds(&view, cx, Region::Pane(ids[0]));
    let b = bounds(&view, cx, Region::Pane(ids[1]));
    assert!(
        (f(a.size.width) - f(b.size.width)).abs() < 1.0,
        "50/50 split"
    );
    assert!((f(a.size.width) + f(b.size.width) - f(center.size.width)).abs() < 1.5);
    assert!((f(a.size.height) - f(center.size.height)).abs() < 1.0);
    assert!(f(a.left()) < f(b.left()));

    // Split the right pane downwards: two stacked panes, each a quarter of the center.
    cx.dispatch_action(actions::SplitDown);
    let s = state(&view, cx);
    let ids: Vec<_> = s.panes().iter().map(|p| p.id).collect();
    assert_eq!(ids.len(), 3);
    let top = bounds(&view, cx, Region::Pane(ids[1]));
    let bottom = bounds(&view, cx, Region::Pane(ids[2]));
    assert!((f(top.size.height) - f(bottom.size.height)).abs() < 1.0);
    assert!((f(top.left()) - f(bottom.left())).abs() < 0.5);
    assert!((f(top.bottom()) - f(bottom.top())).abs() < 1.5);

    // Zoom shows only the focused pane, filling the center.
    cx.dispatch_action(actions::ToggleZoom);
    let FocusTarget::Pane(zoomed) = state(&view, cx).focus else {
        panic!("pane focus expected")
    };
    let z = bounds(&view, cx, Region::Pane(zoomed));
    let center = bounds(&view, cx, Region::Center);
    assert!((f(z.size.width) - f(center.size.width)).abs() < 1.0);
    assert!((f(z.size.height) - f(center.size.height)).abs() < 1.0);
}

#[gpui::test]
fn dock_visibility_and_size_drive_the_layout(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    let before = bounds(&view, cx, Region::Center);
    cx.dispatch_action(actions::ToggleLeftDock);
    let after = bounds(&view, cx, Region::Center);
    // ⌘B leaves the 44 pt icon rail: the center gains the rest.
    assert!((f(after.size.width) - f(before.size.width) - 236.0).abs() < 1.0);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::Dock(DockPosition::Left)))
            .is_none()
    );

    cx.dispatch_action(actions::ToggleBottomDock);
    let bottom = bounds(&view, cx, Region::Dock(DockPosition::Bottom));
    assert_size(bottom.size.height, 200.0);
    let center = bounds(&view, cx, Region::Center);
    assert!(
        (f(center.bottom()) - f(bottom.top())).abs() < 2.5,
        "center {center:?} bottom {bottom:?}"
    );

    // Focus preset hides every dock; the center then spans the window.
    cx.dispatch_action(actions::ApplyPreset("focus".into()));
    let center = bounds(&view, cx, Region::Center);
    let status = bounds(&view, cx, Region::StatusBar);
    assert!((f(center.size.width) - f(status.size.width)).abs() < 1.0);
}

// ---- mouse interaction -----------------------------------------------------------------

use gpui::{Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, point, px};
use hutan_desktop::workbench::model::{Axis, DropTarget, Edge, ItemSource, PaneTree};

fn center_of(b: gpui::Bounds<Pixels>) -> Point<Pixels> {
    point(
        b.origin.x + b.size.width / 2.0,
        b.origin.y + b.size.height / 2.0,
    )
}

/// Press at `from`, move in small steps with the button held, release at `to`.
fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    let steps = 8;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let p = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        cx.simulate_mouse_move(p, MouseButton::Left, Modifiers::none());
    }
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
}

fn ratios(state: &WorkbenchState) -> Vec<f32> {
    match &state.center {
        PaneTree::Split { ratios, .. } => ratios.clone(),
        PaneTree::Pane(_) => vec![],
    }
}

#[gpui::test]
fn dragging_a_splitter_resizes_the_panes(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.dispatch_action(actions::SplitRight);
    let ids: Vec<_> = state(&view, cx).panes().iter().map(|p| p.id).collect();
    let a = bounds(&view, cx, Region::Pane(ids[0]));
    let center = bounds(&view, cx, Region::Center);
    let divider = point(a.right(), a.origin.y + a.size.height / 2.0);
    drag(cx, divider, divider + point(px(150.0), px(0.0)));
    let r = ratios(&state(&view, cx));
    let expected = (f(a.size.width) + 150.0) / f(center.size.width);
    assert!(
        (r[0] - expected).abs() < 0.01,
        "ratio {} vs {expected}",
        r[0]
    );
    assert!((r[0] + r[1] - 1.0).abs() < 1e-4);
    // The painted layout follows the new ratio.
    let a2 = bounds(&view, cx, Region::Pane(ids[0]));
    assert!((f(a2.size.width) - f(a.size.width) - 150.0).abs() < 2.0);

    // Dragging far past the edge clamps at the minimum share.
    let divider = point(a2.right(), a2.origin.y + px(50.0));
    drag(cx, divider, point(px(0.0), divider.y));
    let r = ratios(&state(&view, cx));
    assert!((r[0] - 0.05).abs() < 1e-3, "{r:?}");
}

#[gpui::test]
fn vertical_splitter_drag(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.dispatch_action(actions::SplitDown);
    let ids: Vec<_> = state(&view, cx).panes().iter().map(|p| p.id).collect();
    let top = bounds(&view, cx, Region::Pane(ids[0]));
    let center = bounds(&view, cx, Region::Center);
    let divider = point(top.origin.x + px(200.0), top.bottom());
    drag(cx, divider, divider + point(px(0.0), px(-100.0)));
    let s = state(&view, cx);
    let PaneTree::Split { axis, ratios, .. } = &s.center else {
        panic!("split expected")
    };
    assert_eq!(*axis, Axis::Vertical);
    let expected = (f(top.size.height) - 100.0) / f(center.size.height);
    assert!(
        (ratios[0] - expected).abs() < 0.01,
        "{} vs {expected}",
        ratios[0]
    );
}

#[gpui::test]
fn dragging_a_dock_edge_resizes_the_dock(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    let left = bounds(&view, cx, Region::Dock(DockPosition::Left));
    let edge = point(left.right() - px(2.0), px(400.0));
    drag(cx, edge, edge + point(px(60.0), px(0.0)));
    assert_eq!(state(&view, cx).docks.left.size, 340.0);

    let right = bounds(&view, cx, Region::Dock(DockPosition::Right));
    let edge = point(right.left() + px(2.0), px(400.0));
    drag(cx, edge, edge + point(px(-50.0), px(0.0)));
    assert_eq!(state(&view, cx).docks.right.size, 400.0);

    cx.dispatch_action(actions::ToggleBottomDock);
    let bottom = bounds(&view, cx, Region::Dock(DockPosition::Bottom));
    let edge = point(px(600.0), bottom.top() + px(2.0));
    drag(cx, edge, edge + point(px(0.0), px(-40.0)));
    assert_eq!(state(&view, cx).docks.bottom.size, 240.0);

    // The minimum is enforced: a few points below it the dock stays at 220 (a drag 24 pt below
    // collapses it instead, see `dock_gpui.rs`).
    let left = bounds(&view, cx, Region::Dock(DockPosition::Left));
    let edge = point(left.right() - px(2.0), px(400.0));
    drag(cx, edge, point(px(215.0), px(400.0)));
    assert_eq!(
        state(&view, cx).docks.left.size,
        DockPosition::Left.size_range().0
    );
    assert!(state(&view, cx).docks.left.is_expanded());
}

#[gpui::test]
fn clicking_tabs_activates_and_close_button_closes(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    new_tab(&view, cx);
    new_tab(&view, cx);
    let pane = state(&view, cx).first_pane_id();
    assert_eq!(state(&view, cx).pane(pane).unwrap().active, 2);

    let tab0 = bounds(&view, cx, Region::Tab(pane, 0));
    cx.simulate_click(
        point(tab0.origin.x + px(6.0), center_of(tab0).y),
        Modifiers::none(),
    );
    assert_eq!(state(&view, cx).pane(pane).unwrap().active, 0);

    // The × sits at the right end of the tab.
    let tab1 = bounds(&view, cx, Region::Tab(pane, 1));
    cx.simulate_click(
        point(tab1.right() - px(10.0), center_of(tab1).y),
        Modifiers::none(),
    );
    let s = state(&view, cx);
    assert_eq!(s.pane(pane).unwrap().items.len(), 2);
    assert_eq!(
        s.pane(pane).unwrap().items[1],
        ItemRef::placeholder("New tab")
    );

    // Middle click closes too.
    let tab1 = bounds(&view, cx, Region::Tab(pane, 1));
    let p = center_of(tab1);
    cx.simulate_mouse_down(p, MouseButton::Middle, Modifiers::none());
    cx.simulate_mouse_up(p, MouseButton::Middle, Modifiers::none());
    assert_eq!(state(&view, cx).pane(pane).unwrap().items.len(), 1);
}

#[gpui::test]
fn double_clicking_a_tab_zooms_the_pane(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.dispatch_action(actions::SplitRight);
    let s = state(&view, cx);
    let pane = s.first_pane_id();
    let tab = bounds(&view, cx, Region::Tab(pane, 0));
    let p = point(tab.origin.x + px(6.0), center_of(tab).y);
    cx.simulate_event(MouseDownEvent {
        position: p,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(MouseUpEvent {
        position: p,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
    });
    assert_eq!(state(&view, cx).zoomed, Some(pane));
}

#[gpui::test]
fn dragging_a_tab_onto_another_tab_strip_moves_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    cx.dispatch_action(actions::SplitRight);
    let s = state(&view, cx);
    let ids: Vec<_> = s.panes().iter().map(|p| p.id).collect();
    // Pane 0 holds "Welcome"; add a second tab, then drag it into pane 1's content.
    cx.dispatch_action(actions::FocusNeighborLeft);
    new_tab(&view, cx);
    assert_eq!(state(&view, cx).pane(ids[0]).unwrap().items.len(), 2);

    let tab = bounds(&view, cx, Region::Tab(ids[0], 1));
    let target = bounds(&view, cx, Region::Pane(ids[1]));
    drag(cx, center_of(tab), center_of(target));
    let s = state(&view, cx);
    assert_eq!(s.pane(ids[0]).unwrap().items.len(), 1);
    let moved = &s.pane(ids[1]).unwrap().items;
    assert_eq!(moved.len(), 2);
    assert_eq!(moved[1], ItemRef::placeholder("New tab"));
    assert_eq!(s.focus, FocusTarget::Pane(ids[1]));
}

#[gpui::test]
fn dragging_a_tab_onto_a_pane_edge_splits_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    new_tab(&view, cx);
    let pane = state(&view, cx).first_pane_id();
    let tab = bounds(&view, cx, Region::Tab(pane, 1));
    let target = bounds(&view, cx, Region::Pane(pane));
    // Right-edge zone: x in the last quarter, y mid-height of the content area.
    let to = point(
        target.right() - target.size.width * 0.1,
        target.origin.y + target.size.height * 0.6,
    );
    drag(cx, center_of(tab), to);
    let s = state(&view, cx);
    assert_eq!(s.panes().len(), 2);
    let PaneTree::Split { axis, .. } = &s.center else {
        panic!("expected a split")
    };
    assert_eq!(*axis, Axis::Horizontal);
    // The dragged tab is in the new right-hand pane.
    assert_eq!(s.panes()[1].items, vec![ItemRef::placeholder("New tab")]);
    assert_eq!(s.panes()[0].items, vec![ItemRef::placeholder("Welcome")]);
}

#[gpui::test]
fn dragging_a_panel_tab_to_a_pane_and_to_another_dock(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    // Dock tab → center pane (opens the panel as a tab). Requests is listed in the rail's ⋯
    // until it shows, so it is opened first to have an icon (the fourth).
    let pane = state(&view, cx).first_pane_id();
    cx.dispatch_action(actions::OpenPanel("inspector".into()));
    let tab = bounds(&view, cx, Region::DockTab(DockPosition::Right, 3));
    let target = bounds(&view, cx, Region::Pane(pane));
    drag(cx, center_of(tab), center_of(target));
    let s = state(&view, cx);
    assert_eq!(
        s.docks.right.panels,
        vec![
            PanelId::new("changes"),
            PanelId::new("agents"),
            PanelId::new("history"),
            PanelId::new("resources")
        ]
    );
    assert!(
        s.pane(pane)
            .unwrap()
            .items
            .contains(&ItemRef::panel("inspector"))
    );

    // Center panel tab → the left sidebar (it joins the pages, after the others).
    let inspector_index = state(&view, cx).pane(pane).unwrap().items.len() - 1;
    let tab = bounds(&view, cx, Region::Tab(pane, inspector_index));
    let left = bounds(&view, cx, Region::Dock(DockPosition::Left));
    drag(cx, center_of(tab), center_of(left));
    let s = state(&view, cx);
    assert_eq!(
        s.docks.left.panels,
        vec![
            PanelId::new("sessions"),
            PanelId::new("tasks"),
            PanelId::new("files"),
            PanelId::new("inspector")
        ]
    );
    assert_eq!(s.docks.left.active, Some(PanelId::new("inspector")));
    assert!(
        !s.pane(pane)
            .unwrap()
            .items
            .contains(&ItemRef::panel("inspector"))
    );
}

#[gpui::test]
fn plain_tabs_cannot_be_dropped_into_docks(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    new_tab(&view, cx);
    let pane = state(&view, cx).first_pane_id();
    let before = state(&view, cx);
    let tab = bounds(&view, cx, Region::Tab(pane, 1));
    let dock = bounds(&view, cx, Region::Dock(DockPosition::Left));
    drag(cx, center_of(tab), center_of(dock));
    assert_eq!(state(&view, cx).docks, before.docks);
    assert_eq!(state(&view, cx).center, before.center);
}

#[gpui::test]
fn right_click_menus_list_the_documented_entries(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, None);
    // The Tasks row of the left nav (the dock's panel 1) is there while its page shows.
    cx.dispatch_action(actions::OpenPanel("tasks".into()));
    common::draw(cx);
    let tab = bounds(&view, cx, Region::DockTab(DockPosition::Left, 1));
    let p = center_of(tab);
    cx.simulate_mouse_down(p, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(p, MouseButton::Right, Modifiers::none());
    let entries = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("menu open");
    let labels: Vec<&str> = entries.iter().map(|(l, _)| l.as_ref()).collect();
    assert_eq!(
        labels,
        vec![
            "Move to Left Dock",
            "Move to Right Dock",
            "Move to Bottom Dock",
            "Open as Tab",
            "Close"
        ]
    );
    assert!(entries[0].1, "the current dock is checked");

    // "Move to Bottom Dock".
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(2, window, cx));
    let s = state(&view, cx);
    assert_eq!(s.docks.bottom.panels, vec![PanelId::new("tasks")]);
    assert!(
        view.read_with(cx, |wb, _| wb.context_menu_entries())
            .is_none()
    );

    // Escape dismisses a menu without running anything.
    let tab = bounds(&view, cx, Region::DockTab(DockPosition::Right, 0));
    let p = center_of(tab);
    cx.simulate_mouse_down(p, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(p, MouseButton::Right, Modifiers::none());
    assert!(
        view.read_with(cx, |wb, _| wb.context_menu_entries())
            .is_some()
    );
    cx.simulate_keystrokes("escape");
    assert!(
        view.read_with(cx, |wb, _| wb.context_menu_entries())
            .is_none()
    );
    assert_eq!(
        state(&view, cx).docks.right.panels,
        vec![
            PanelId::new("changes"),
            PanelId::new("agents"),
            PanelId::new("history"),
            PanelId::new("inspector"),
            PanelId::new("resources")
        ]
    );

    // "Close" on the center tab menu.
    let pane = state(&view, cx).first_pane_id();
    new_tab(&view, cx);
    let tab = bounds(&view, cx, Region::Tab(pane, 1));
    let p = center_of(tab);
    cx.simulate_mouse_down(p, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(p, MouseButton::Right, Modifiers::none());
    let n = view
        .read_with(cx, |wb, _| wb.context_menu_entries().map(|e| e.len()))
        .unwrap();
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(n - 1, window, cx));
    assert_eq!(state(&view, cx).pane(pane).unwrap().items.len(), 1);

    // Unused imports guard: model drop/target types stay part of the public API.
    let _ = (
        DropTarget::Split {
            pane,
            edge: Edge::Left,
        },
        ItemSource::Pane { pane, index: 0 },
    );
}

/// The title bar has no preset switcher (CL-CHROME): View → Layout Preset lists the presets
/// with the current one checked, and follows a change of preset.
#[gpui::test]
fn the_view_menu_checks_the_current_preset(cx: &mut TestAppContext) {
    use gpui::OwnedMenuItem;
    let (view, cx) = open(cx, None);
    let presets = |cx: &mut VisualTestContext| -> Vec<(String, bool)> {
        let menus = cx.update(|_, cx| cx.get_menus()).expect("menus are set");
        let view_menu = menus
            .iter()
            .find(|m| m.name.as_ref() == "View")
            .expect("a View menu");
        let submenu = view_menu
            .items
            .iter()
            .find_map(|item| match item {
                OwnedMenuItem::Submenu(menu) if menu.name.as_ref() == "Layout Preset" => {
                    Some(menu.clone())
                }
                _ => None,
            })
            .expect("View → Layout Preset");
        submenu
            .items
            .iter()
            .filter_map(|item| match item {
                OwnedMenuItem::Action { name, checked, .. } => Some((name.clone(), *checked)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        presets(cx),
        [
            ("Default".to_owned(), true),
            ("Focus".to_owned(), false),
            ("Overview".to_owned(), false)
        ]
    );
    cx.dispatch_action(actions::ApplyPreset("focus".into()));
    assert_eq!(state(&view, cx).preset, "focus");
    assert_eq!(
        presets(cx),
        [
            ("Default".to_owned(), false),
            ("Focus".to_owned(), true),
            ("Overview".to_owned(), false)
        ]
    );
    assert!(
        !view.read_with(cx, |wb, _| wb.layout_offer_shown()),
        "a layout handed in is never offered the new default"
    );
}

#[gpui::test]
fn a_saved_overview_layout_restores_with_its_stash(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(STATE_FILE);
    {
        let (view, cx) = open(cx, Some(path.clone()));
        cx.dispatch_action(actions::SplitRight);
        cx.dispatch_action(actions::ApplyPreset("overview".into()));
        view.update(cx, |wb, _| wb.flush_save());
    }
    let (view, cx) = open(cx, Some(path));
    let s = state(&view, cx);
    assert_eq!(s.preset, "overview");
    assert_eq!(
        s.active_pane().unwrap().active_item(),
        Some(&ItemRef::panel("agents-dashboard"))
    );
    cx.dispatch_action(actions::ApplyPreset("default".into()));
    assert_eq!(
        state(&view, cx).panes().len(),
        2,
        "the stashed split layout comes back"
    );
}
