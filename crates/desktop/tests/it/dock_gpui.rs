#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! DK-1, recoverable docks: ⌘B between the expanded sidebar and its 44 pt icon rail, the rail's
//! icons, the right side's 40 pt activity rail (active icon collapses the body, any icon opens
//! it, ⌘L), the titlebar buttons that always bring a dock back, drag past a dock's minimum
//! (collapse on release, Escape cancels), the keyboard leaving a collapsed dock, the responsive
//! auto-collapse at 1280 / 1180 / 900 / 640 pt, and what is saved. No dock collapse ever stops a
//! session.

use crate::common;

use common::{TestDaemon, draw, offline_connection, open_with, wait_until};
use gpui::{
    Bounds, Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext,
    point, px, size,
};
use hutan_desktop::workbench::actions::{self, OpenPanel};
use hutan_desktop::workbench::model::{
    AutoState, DockMode, DockPosition, ItemRef, PanelId, WorkbenchState,
};
use hutan_desktop::workbench::persistence::{STATE_FILE, load_state};
use hutan_desktop::workbench::{Region, SAVE_DEBOUNCE, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};
use std::path::PathBuf;

use DockMode::{Compact, Expanded, Hidden};
use DockPosition::{Bottom, Left, Right};

fn open_at(
    cx: &mut TestAppContext,
    assistant: bool,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, SettingsStore, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    if assistant {
        settings
            .write_user("assistant.enabled", toml::Value::Boolean(true))
            .unwrap();
    }
    let (view, cx) = open_with(cx, settings.clone(), connection, state_path);
    (view, settings, cx)
}

fn open(cx: &mut TestAppContext) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (view, _, cx) = open_at(cx, false, None);
    (view, cx)
}

fn state(view: &Entity<Workbench>, cx: &VisualTestContext) -> WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

/// What the window draws of `pos`.
fn shown(view: &Entity<Workbench>, cx: &VisualTestContext, pos: DockPosition) -> DockMode {
    view.read_with(cx, |wb, _| wb.dock_mode(pos))
}

/// What the user chose for `pos` (the saved state).
fn chosen(view: &Entity<Workbench>, cx: &VisualTestContext, pos: DockPosition) -> DockMode {
    view.read_with(cx, |wb, _| wb.state().docks.get(pos).mode())
}

fn painted(view: &Entity<Workbench>, cx: &VisualTestContext, region: Region) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .is_some()
}

fn bounds(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> Bounds<Pixels> {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        painted(view, cx, region)
    });
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap()
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    let at = bounds(view, cx, region).center();
    cx.simulate_click(at, Modifiers::none());
    draw(cx);
}

#[track_caller]
fn assert_size(actual: Pixels, expected: f32) {
    let actual = f32::from(actual);
    assert!(
        (actual - expected).abs() <= 1.0,
        "expected {expected} (±1 border), got {actual}"
    );
}

/// The window at `width` × 800 pt, drawn.
fn resize(cx: &mut VisualTestContext, width: f32) {
    cx.simulate_resize(size(px(width), px(800.0)));
    draw(cx);
    draw(cx);
}

fn press_and_move(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for i in 1..=8 {
        let t = i as f32 / 8.0;
        let p = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        cx.simulate_mouse_move(p, MouseButton::Left, Modifiers::none());
    }
    draw(cx);
}

fn release(cx: &mut VisualTestContext, at: Point<Pixels>) {
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    draw(cx);
}

// ---- left sidebar ---------------------------------------------------------------------------

#[gpui::test]
fn cmd_b_goes_between_the_expanded_sidebar_and_its_icon_rail(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    assert_eq!(shown(&view, cx, Left), Expanded);
    assert_size(bounds(&view, cx, Region::Dock(Left)).size.width, 280.0);
    let center_before = bounds(&view, cx, Region::Center).size.width;

    cx.simulate_keystrokes("cmd-b");
    draw(cx);
    assert_eq!(chosen(&view, cx, Left), Compact);
    assert!(!painted(&view, cx, Region::Dock(Left)), "the body is gone");
    assert_size(bounds(&view, cx, Region::Rail(Left)).size.width, 44.0);
    assert!(bounds(&view, cx, Region::Center).size.width > center_before + px(200.0));
    assert_eq!(state(&view, cx).docks.left.size, 280.0, "the width is kept");

    cx.simulate_keystrokes("cmd-b");
    draw(cx);
    assert_eq!(chosen(&view, cx, Left), Expanded);
    assert!(!painted(&view, cx, Region::Rail(Left)));
    assert_size(bounds(&view, cx, Region::Dock(Left)).size.width, 280.0);
}

#[gpui::test]
fn the_compact_rail_has_every_nav_icon_projects_and_the_toolbar(cx: &mut TestAppContext) {
    let (view, _, cx) = open_at(cx, true, None);
    cx.simulate_keystrokes("cmd-b");
    draw(cx);
    let rail = bounds(&view, cx, Region::Rail(Left));
    let order = [
        Region::Nav("agents"),
        Region::Nav("assistant"),
        Region::Nav("projects"),
    ];
    let mut last = f32::MIN;
    for region in order {
        let b = bounds(&view, cx, region);
        assert!(rail.contains(&b.center()), "{region:?} is on the rail");
        assert_size(b.size.width, 32.0);
        assert_size(b.size.height, 32.0);
        let y = f32::from(b.origin.y);
        assert!(y > last, "{region:?} comes after the one before");
        last = y;
    }
    // Start and Settings at the bottom of the rail.
    assert!(!painted(&view, cx, Region::Nav("search")), "no Search icon");
    assert!(
        !painted(&view, cx, Region::Nav("tasks")),
        "no Tasks icon before a task"
    );
    let new = bounds(&view, cx, Region::Toolbar("start"));
    let settings = bounds(&view, cx, Region::Toolbar("settings"));
    assert!(new.origin.y > bounds(&view, cx, Region::Nav("projects")).origin.y);
    assert!(settings.origin.y > new.origin.y);
    assert!(rail.contains(&settings.center()));
}

#[gpui::test]
fn compact_rail_icons_open_their_pages(cx: &mut TestAppContext) {
    let (view, _, cx) = open_at(cx, true, None);
    cx.simulate_keystrokes("cmd-b");
    draw(cx);

    // Projects is a page of the sidebar: the rail opens the expanded tree.
    click(&view, cx, Region::Nav("projects"));
    assert_eq!(chosen(&view, cx, Left), Expanded);
    assert_eq!(
        state(&view, cx).docks.left.active,
        Some(PanelId::new("sessions"))
    );
    cx.simulate_keystrokes("cmd-b");
    draw(cx);
    assert_eq!(chosen(&view, cx, Left), Compact);

    // Start is the Start sheet (offline it says so) and does not widen the rail.
    click(&view, cx, Region::Toolbar("start"));
    assert!(view.read_with(cx, |wb, _| wb.notice().is_some()));
    assert_eq!(chosen(&view, cx, Left), Compact);

    // Agents opens the Agents page as a tab of the center, Mawas its Home tab; neither widens it.
    click(&view, cx, Region::Nav("agents"));
    assert_eq!(
        state(&view, cx).active_center_item(),
        Some(ItemRef::panel("agents-dashboard"))
    );
    assert_eq!(chosen(&view, cx, Left), Compact);
    click(&view, cx, Region::Nav("assistant"));
    assert_eq!(
        state(&view, cx).active_center_item(),
        Some(ItemRef::panel("assistant"))
    );
    assert_eq!(chosen(&view, cx, Left), Compact);
    assert!(painted(&view, cx, Region::Rail(Left)));
}

#[gpui::test]
fn the_titlebar_button_always_brings_the_sidebar_back(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    // Hide Left Sidebar, from the palette's action.
    cx.dispatch_action(actions::HideLeftSidebar);
    draw(cx);
    assert_eq!(chosen(&view, cx, Left), Hidden);
    assert!(!painted(&view, cx, Region::Dock(Left)));
    assert!(!painted(&view, cx, Region::Rail(Left)));
    assert!(
        !painted(&view, cx, Region::Nav("agents")),
        "no hitboxes of a hidden sidebar"
    );
    // The button is outside the sidebar and after the traffic lights (80 pt).
    let button = bounds(&view, cx, Region::DockToggle(Left));
    assert!(button.origin.x >= px(80.0) - px(1.0), "{button:?}");
    click(&view, cx, Region::DockToggle(Left));
    assert_eq!(chosen(&view, cx, Left), Expanded);
    assert_size(bounds(&view, cx, Region::Dock(Left)).size.width, 280.0);
    // Visible, it hides: Hidden is one click from every state.
    click(&view, cx, Region::DockToggle(Left));
    assert_eq!(chosen(&view, cx, Left), Hidden);

    // ⌘B brings it back in the mode it was hidden from.
    cx.simulate_keystrokes("cmd-b");
    draw(cx);
    assert_eq!(chosen(&view, cx, Left), Expanded);
    cx.simulate_keystrokes("cmd-b");
    cx.dispatch_action(actions::HideLeftSidebar);
    draw(cx);
    cx.simulate_keystrokes("cmd-b");
    draw(cx);
    assert_eq!(chosen(&view, cx, Left), Compact);
}

#[gpui::test]
fn a_right_click_on_the_titlebar_button_lists_the_three_states(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-b");
    draw(cx);
    let at = bounds(&view, cx, Region::DockToggle(Left)).center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
    draw(cx);
    let entries = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("a menu");
    let entries: Vec<(String, bool)> = entries
        .into_iter()
        .map(|(label, checked)| (label.to_string(), checked))
        .collect();
    assert_eq!(
        entries,
        [
            ("Expanded".to_owned(), false),
            ("Compact (icons)".to_owned(), true),
            ("Hidden".to_owned(), false),
        ]
    );
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(2, window, cx));
    draw(cx);
    assert_eq!(chosen(&view, cx, Left), Hidden);
}

// ---- right rail -----------------------------------------------------------------------------

#[gpui::test]
fn the_right_rail_is_outside_the_body_and_the_active_icon_collapses_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    let body = bounds(&view, cx, Region::Dock(Right));
    let rail = bounds(&view, cx, Region::Rail(Right));
    assert_size(body.size.width, 350.0);
    assert_size(rail.size.width, 40.0);
    assert!(rail.left() >= body.right(), "the rail is outside the body");
    for i in 0..3 {
        let icon = bounds(&view, cx, Region::DockTab(Right, i));
        assert!(rail.contains(&icon.center()), "icon {i} is on the rail");
        assert_size(icon.size.width, 32.0);
    }
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("changes"))
    );

    // The active icon collapses the body; the rail stays and remembers the panel and width.
    click(&view, cx, Region::DockTab(Right, 0));
    assert_eq!(chosen(&view, cx, Right), Compact);
    assert!(!painted(&view, cx, Region::Dock(Right)));
    assert_size(bounds(&view, cx, Region::Rail(Right)).size.width, 40.0);
    assert!(painted(&view, cx, Region::DockTab(Right, 2)));
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("changes"))
    );
    assert_eq!(state(&view, cx).docks.right.size, 350.0);

    // Any icon expands that panel while collapsed, the remembered one included.
    click(&view, cx, Region::DockTab(Right, 0));
    assert_eq!(chosen(&view, cx, Right), Expanded);
    assert_size(bounds(&view, cx, Region::Dock(Right)).size.width, 350.0);
    click(&view, cx, Region::DockTab(Right, 0));
    assert_eq!(chosen(&view, cx, Right), Compact);
    click(&view, cx, Region::DockTab(Right, 1));
    assert_eq!(chosen(&view, cx, Right), Expanded);
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("agents"))
    );

    // Another icon while open switches the panel and stays open.
    click(&view, cx, Region::DockTab(Right, 0));
    assert_eq!(chosen(&view, cx, Right), Expanded);
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("changes"))
    );
}

#[gpui::test]
fn cmd_l_toggles_the_body_and_keeps_the_rail_and_the_panel(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    click(&view, cx, Region::DockTab(Right, 2));
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("history"))
    );
    cx.simulate_keystrokes("cmd-l");
    draw(cx);
    assert_eq!(chosen(&view, cx, Right), Compact);
    assert!(painted(&view, cx, Region::Rail(Right)));
    assert!(!painted(&view, cx, Region::Dock(Right)));
    cx.simulate_keystrokes("cmd-l");
    draw(cx);
    assert_eq!(chosen(&view, cx, Right), Expanded);
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("history")),
        "the last panel comes back"
    );
    // The rail's own button does the same.
    click(&view, cx, Region::Toolbar("right-collapse"));
    assert_eq!(chosen(&view, cx, Right), Compact);
    click(&view, cx, Region::Toolbar("right-collapse"));
    assert_eq!(chosen(&view, cx, Right), Expanded);
}

#[gpui::test]
fn hide_right_rail_is_a_full_hide_with_a_titlebar_recovery_button(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.dispatch_action(actions::HideRightRail);
    draw(cx);
    assert_eq!(chosen(&view, cx, Right), Hidden);
    assert!(!painted(&view, cx, Region::Rail(Right)));
    assert!(!painted(&view, cx, Region::Dock(Right)));
    assert!(!painted(&view, cx, Region::DockTab(Right, 0)));
    // ⌘L (and the titlebar button) bring it back open.
    click(&view, cx, Region::DockToggle(Right));
    assert_eq!(chosen(&view, cx, Right), Expanded);
    assert!(painted(&view, cx, Region::Rail(Right)));
    // The titlebar button collapses to the rail, then opens again, like ⌘L.
    click(&view, cx, Region::DockToggle(Right));
    assert_eq!(chosen(&view, cx, Right), Compact);
    click(&view, cx, Region::DockToggle(Right));
    assert_eq!(chosen(&view, cx, Right), Expanded);
}

#[gpui::test]
fn opening_a_panel_by_command_expands_a_collapsed_right_panel(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-l");
    draw(cx);
    assert_eq!(chosen(&view, cx, Right), Compact);
    cx.dispatch_action(OpenPanel("resources".into()));
    draw(cx);
    assert_eq!(chosen(&view, cx, Right), Expanded);
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("resources"))
    );
}

// ---- bottom -----------------------------------------------------------------------------------

#[gpui::test]
fn cmd_alt_j_toggles_the_bottom_body_with_its_tab_strip(cx: &mut TestAppContext) {
    let (view, settings, cx) = open_at(cx, false, None);
    // ⌘⌥J is Hutan's own key for the bottom dock (the Zed base has ⌘J).
    settings
        .write_user("keymap.base", toml::Value::String("hutan".into()))
        .unwrap();
    cx.run_until_parked();
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.open_panel(&PanelId::new("notifications"), Bottom);
            s.set_dock_mode(Bottom, Hidden);
        });
    });
    draw(cx);
    assert!(!painted(&view, cx, Region::Dock(Bottom)));
    cx.simulate_keystrokes("cmd-alt-j");
    draw(cx);
    assert_eq!(chosen(&view, cx, Bottom), Expanded);
    assert!(painted(&view, cx, Region::Dock(Bottom)));
    assert!(
        painted(&view, cx, Region::DockTab(Bottom, 0)),
        "its tab strip"
    );
    cx.simulate_keystrokes("cmd-alt-j");
    draw(cx);
    assert_eq!(chosen(&view, cx, Bottom), Hidden);
    assert!(!painted(&view, cx, Region::Dock(Bottom)));
    assert!(!painted(&view, cx, Region::DockTab(Bottom, 0)));
}

// ---- drag past the minimum ----------------------------------------------------------------------

#[gpui::test]
fn dragging_the_left_edge_24_pt_below_its_minimum_collapses_it_on_release(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    let dock = bounds(&view, cx, Region::Dock(Left));
    let edge = point(dock.right() - px(2.0), px(400.0));

    // A little below the minimum (220): the dock stops at it, nothing is armed.
    press_and_move(cx, edge, edge + point(px(-70.0), px(0.0)));
    assert_eq!(state(&view, cx).docks.left.size, 220.0);
    assert!(!painted(&view, cx, Region::CollapsePreview(Left)));
    release(cx, edge + point(px(-70.0), px(0.0)));
    assert_eq!(chosen(&view, cx, Left), Expanded);
    assert_eq!(state(&view, cx).docks.left.size, 220.0);

    // 24 pt further arms the collapse, with a visible preview of the rail; release commits it.
    let dock = bounds(&view, cx, Region::Dock(Left));
    let edge = point(dock.right() - px(2.0), px(400.0));
    let end = edge + point(px(-60.0), px(0.0));
    press_and_move(cx, edge, end);
    assert!(
        painted(&view, cx, Region::CollapsePreview(Left)),
        "a snap preview"
    );
    assert_size(
        bounds(&view, cx, Region::CollapsePreview(Left)).size.width,
        44.0,
    );
    assert_eq!(chosen(&view, cx, Left), Expanded, "not before the release");
    release(cx, end);
    assert_eq!(chosen(&view, cx, Left), Compact);
    assert!(!painted(&view, cx, Region::CollapsePreview(Left)));
    assert!(painted(&view, cx, Region::Rail(Left)));
    assert_eq!(
        state(&view, cx).docks.left.size,
        220.0,
        "the width before the collapse is the one it reopens at"
    );
    // Dragging the compact rail's edge does not hide it.
    let rail = bounds(&view, cx, Region::Rail(Left));
    let edge = point(rail.right() - px(2.0), px(400.0));
    press_and_move(cx, edge, edge + point(px(-30.0), px(0.0)));
    release(cx, edge + point(px(-30.0), px(0.0)));
    assert_eq!(chosen(&view, cx, Left), Compact);
    assert!(painted(&view, cx, Region::Rail(Left)));
}

#[gpui::test]
fn dragging_the_right_edge_past_its_minimum_collapses_it_and_escape_cancels(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open(cx);
    let dock = bounds(&view, cx, Region::Dock(Right));
    let edge = point(dock.left() + px(2.0), px(400.0));
    // Escape cancels an armed drag: the size is what it was, nothing collapses on release.
    let end = edge + point(px(160.0), px(0.0));
    press_and_move(cx, edge, end);
    assert!(painted(&view, cx, Region::CollapsePreview(Right)));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!painted(&view, cx, Region::CollapsePreview(Right)));
    assert_eq!(state(&view, cx).docks.right.size, 350.0);
    release(cx, end);
    assert_eq!(chosen(&view, cx, Right), Expanded);
    assert_eq!(state(&view, cx).docks.right.size, 350.0);
    assert_size(bounds(&view, cx, Region::Dock(Right)).size.width, 350.0);

    // Without Escape the same drag collapses to the rail and keeps 350 for reopening.
    let dock = bounds(&view, cx, Region::Dock(Right));
    let edge = point(dock.left() + px(2.0), px(400.0));
    let end = edge + point(px(160.0), px(0.0));
    press_and_move(cx, edge, end);
    release(cx, end);
    assert_eq!(chosen(&view, cx, Right), Compact);
    assert!(painted(&view, cx, Region::Rail(Right)));
    assert_eq!(state(&view, cx).docks.right.size, 350.0);
    cx.simulate_keystrokes("cmd-l");
    draw(cx);
    assert_size(bounds(&view, cx, Region::Dock(Right)).size.width, 350.0);
}

#[gpui::test]
fn escape_still_reaches_the_workbench_when_nothing_is_dragged(cx: &mut TestAppContext) {
    // The resize key context exists only during a drag: Escape is not claimed otherwise.
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(chosen(&view, cx, Left), Expanded);
    assert_eq!(state(&view, cx).docks.left.size, 280.0);
}

// ---- focus -------------------------------------------------------------------------------------

fn dock_holds_the_keyboard(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    pos: DockPosition,
) -> bool {
    let handle = view.read_with(cx, |wb, _| wb.dock_focus_handle(pos));
    cx.update(|window, cx| handle.contains_focused(window, cx))
}

fn workbench_holds_the_keyboard(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> bool {
    let handle = view.read_with(cx, gpui::Focusable::focus_handle);
    cx.update(|window, cx| handle.contains_focused(window, cx))
}

#[gpui::test]
fn collapsing_a_dock_that_holds_the_keyboard_moves_it_to_the_center(cx: &mut TestAppContext) {
    for pos in [Left, Right] {
        let (view, cx) = open(cx);
        // The keyboard goes into the dock, as a click on a panel's input would put it.
        let handle = view.read_with(cx, |wb, _| wb.dock_focus_handle(pos));
        cx.update(|window, cx| window.focus(&handle, cx));
        view.update(cx, |wb, cx| wb.edit_layout(cx, |s| s.focus_dock(pos)));
        draw(cx);
        assert!(dock_holds_the_keyboard(&view, cx, pos), "{pos:?}");

        cx.simulate_keystrokes(if pos == Left { "cmd-b" } else { "cmd-l" });
        draw(cx);
        assert_eq!(chosen(&view, cx, pos), Compact);
        assert!(!dock_holds_the_keyboard(&view, cx, pos), "{pos:?} let go");
        assert!(
            workbench_holds_the_keyboard(&view, cx),
            "{pos:?}: the center (the workbench) has it"
        );
        assert!(
            matches!(
                state(&view, cx).focus,
                hutan_desktop::workbench::model::FocusTarget::Pane(_)
            ),
            "{pos:?}: the model agrees"
        );
    }
}

#[gpui::test]
fn collapsing_never_steals_the_keyboard_from_outside_the_dock(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    let pane = state(&view, cx).active_pane_id();
    assert!(workbench_holds_the_keyboard(&view, cx));
    cx.simulate_keystrokes("cmd-l");
    draw(cx);
    assert_eq!(chosen(&view, cx, Right), Compact);
    assert!(workbench_holds_the_keyboard(&view, cx));
    assert_eq!(state(&view, cx).active_pane_id(), pane);
}

#[gpui::test]
fn the_auto_collapse_takes_the_keyboard_out_of_the_dock_too(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    let handle = view.read_with(cx, |wb, _| wb.dock_focus_handle(Right));
    cx.update(|window, cx| window.focus(&handle, cx));
    draw(cx);
    assert!(dock_holds_the_keyboard(&view, cx, Right));
    resize(cx, 1000.0);
    assert_eq!(shown(&view, cx, Right), Compact);
    assert!(!dock_holds_the_keyboard(&view, cx, Right));
    assert!(workbench_holds_the_keyboard(&view, cx));
}

// ---- responsive auto-collapse ---------------------------------------------------------------

#[gpui::test]
fn the_window_width_decides_what_is_shown_at_1280_1180_900_and_640(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    for (width, left, right) in [
        (1280.0, Expanded, Expanded),
        (1180.0, Expanded, Expanded),
        (900.0, Expanded, Compact),
        (640.0, Compact, Compact),
    ] {
        resize(cx, width);
        assert_eq!(shown(&view, cx, Left), left, "left at {width}");
        assert_eq!(shown(&view, cx, Right), right, "right at {width}");
        assert_eq!(painted(&view, cx, Region::Dock(Left)), left == Expanded);
        assert_eq!(painted(&view, cx, Region::Rail(Left)), left == Compact);
        assert_eq!(painted(&view, cx, Region::Dock(Right)), right == Expanded);
        assert!(
            painted(&view, cx, Region::Rail(Right)),
            "the activity rail is always there at {width}"
        );
        // The center keeps at least 480 pt.
        let center = bounds(&view, cx, Region::Center).size.width;
        assert!(center >= px(480.0) - px(2.0), "{center:?} at {width}");
        // The saved choice is never overwritten by the window.
        assert_eq!(chosen(&view, cx, Left), Expanded, "{width}");
        assert_eq!(chosen(&view, cx, Right), Expanded, "{width}");
        assert_eq!(state(&view, cx).docks.left.size, 280.0);
        assert_eq!(state(&view, cx).docks.right.size, 350.0);
    }
    // 640 pt: compact left, right rail, the center still usable.
    assert!(bounds(&view, cx, Region::Center).size.width >= px(480.0));
}

#[gpui::test]
fn a_collapsed_dock_comes_back_only_80_pt_above_its_threshold(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    resize(cx, 1000.0);
    assert_eq!(shown(&view, cx, Right), Compact);
    for width in [1180.0, 1259.0] {
        resize(cx, width);
        assert_eq!(
            shown(&view, cx, Right),
            Compact,
            "still collapsed at {width}"
        );
    }
    resize(cx, 1260.0);
    assert_eq!(shown(&view, cx, Right), Expanded);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.dock_auto_state(Right)),
        AutoState::Off
    );
    // And the left sidebar: compact below 900, back at 980.
    resize(cx, 800.0);
    assert_eq!(shown(&view, cx, Left), Compact);
    resize(cx, 979.0);
    assert_eq!(shown(&view, cx, Left), Compact);
    resize(cx, 980.0);
    assert_eq!(shown(&view, cx, Left), Expanded);
}

#[gpui::test]
fn an_explicit_hide_is_never_auto_restored_and_compact_stays_compact(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.dispatch_action(actions::HideLeftSidebar);
    cx.dispatch_action(actions::HideRightRail);
    draw(cx);
    for width in [640.0, 1000.0, 1280.0, 1800.0] {
        resize(cx, width);
        assert_eq!(shown(&view, cx, Left), Hidden, "{width}");
        assert_eq!(shown(&view, cx, Right), Hidden, "{width}");
        assert!(!painted(&view, cx, Region::Rail(Left)));
        assert!(!painted(&view, cx, Region::Rail(Right)));
    }
    // Collapsed by the user (⌘B, ⌘L): wide windows leave it that way.
    resize(cx, 1280.0);
    click(&view, cx, Region::DockToggle(Left));
    click(&view, cx, Region::DockToggle(Right));
    cx.simulate_keystrokes("cmd-b");
    cx.simulate_keystrokes("cmd-l");
    draw(cx);
    assert_eq!(chosen(&view, cx, Left), Compact);
    assert_eq!(chosen(&view, cx, Right), Compact);
    for width in [640.0, 1800.0] {
        resize(cx, width);
        assert_eq!(shown(&view, cx, Left), Compact);
        assert_eq!(shown(&view, cx, Right), Compact);
    }
}

#[gpui::test]
fn opening_a_dock_in_a_narrow_window_keeps_it_open_until_the_window_grows(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    resize(cx, 640.0);
    assert_eq!(shown(&view, cx, Left), Compact);
    // ⌘B opens the sidebar the window collapsed; the saved choice was Expanded all along.
    cx.simulate_keystrokes("cmd-b");
    draw(cx);
    assert_eq!(shown(&view, cx, Left), Expanded);
    assert!(painted(&view, cx, Region::Dock(Left)));
    assert_eq!(chosen(&view, cx, Left), Expanded);
    draw(cx);
    assert_eq!(shown(&view, cx, Left), Expanded, "it stays");
    // A rail icon opens the right panel the same way.
    click(&view, cx, Region::DockTab(Right, 1));
    assert_eq!(shown(&view, cx, Right), Expanded);
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("agents"))
    );
    // ⌘B again is the user's own collapse.
    cx.simulate_keystrokes("cmd-b");
    draw(cx);
    assert_eq!(chosen(&view, cx, Left), Compact);
    assert_eq!(shown(&view, cx, Left), Compact);
}

#[gpui::test]
fn with_the_setting_off_a_narrow_window_collapses_nothing(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let settings = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
    settings
        .write_user("layout.auto_collapse", toml::Value::Boolean(false))
        .unwrap();
    let (connection, _, _) = offline_connection();
    let (view, cx) = open_with(cx, settings.clone(), connection, None);
    resize(cx, 640.0);
    assert_eq!(shown(&view, cx, Left), Expanded);
    assert_eq!(shown(&view, cx, Right), Expanded);
    // Turning it on collapses at once, and off again brings the docks back.
    settings
        .write_user("layout.auto_collapse", toml::Value::Boolean(true))
        .unwrap();
    wait_until(cx, "the setting to apply", |cx| {
        shown(&view, cx, Right) == Compact
    });
    assert_eq!(shown(&view, cx, Left), Compact);
    settings
        .write_user("layout.auto_collapse", toml::Value::Boolean(false))
        .unwrap();
    wait_until(cx, "the setting to apply", |cx| {
        shown(&view, cx, Right) == Expanded
    });
    assert_eq!(shown(&view, cx, Left), Expanded);
}

// ---- persistence ----------------------------------------------------------------------------

#[gpui::test]
fn the_dock_modes_are_saved_and_restored(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    // A layout that was saved before (the very first launch starts the right sidebar on its
    // rail already): both sidebars are open, ⌘B and ⌘L take them down to their rails.
    std::fs::write(&path, WorkbenchState::default().to_json().unwrap()).unwrap();
    let (view, _, cx) = open_at(cx, false, Some(path.clone()));
    cx.simulate_keystrokes("cmd-b");
    cx.simulate_keystrokes("cmd-l");
    draw(cx);
    cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
    cx.run_until_parked();
    view.update(cx, |wb, _| wb.flush_save());
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(on_disk["version"], 6);
    assert_eq!(on_disk["docks"]["left"]["collapsed"], true);
    assert_eq!(on_disk["docks"]["right"]["collapsed"], true);
    let loaded = load_state(&path);
    assert_eq!(loaded.docks.left.mode(), Compact);
    assert_eq!(loaded.docks.right.mode(), Compact);
    assert_eq!(loaded.docks.left.size, 280.0);
}

#[gpui::test]
fn a_saved_collapsed_layout_opens_collapsed(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    let mut saved = WorkbenchState::default();
    saved.set_dock_mode(Left, Compact);
    saved.set_dock_mode(Right, Hidden);
    std::fs::write(&path, saved.to_json().unwrap()).unwrap();
    let (view, _, cx) = open_at(cx, false, Some(path));
    draw(cx);
    assert!(painted(&view, cx, Region::Rail(Left)));
    assert!(!painted(&view, cx, Region::Dock(Left)));
    assert!(!painted(&view, cx, Region::Rail(Right)));
    assert_eq!(shown(&view, cx, Right), Hidden);
}

// ---- nothing stops --------------------------------------------------------------------------

#[gpui::test]
fn no_collapse_stops_a_session_or_closes_a_tab(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let running = daemon.sh("sleep 60");
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    let items_before = state(&view, cx).all_items();

    cx.simulate_keystrokes("cmd-b");
    cx.simulate_keystrokes("cmd-l");
    cx.simulate_keystrokes("cmd-alt-y");
    draw(cx);
    assert!(!painted(&view, cx, Region::Rail(Left)));
    resize(cx, 640.0);
    cx.dispatch_action(actions::ToggleLeftDock);
    resize(cx, 1280.0);
    draw(cx);

    assert_eq!(state(&view, cx).all_items(), items_before, "no tab closed");
    assert!(
        daemon.session(running.id).state.is_live(),
        "the session is still running"
    );
    assert!(
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(running.id)
                .is_some_and(|s| s.state.is_live())
        }),
        "and the sidebar's store agrees"
    );
}

// ---- approvals -------------------------------------------------------------------------------

/// A permission request neither expands a right panel the user collapsed nor takes the keyboard:
/// the rail keeps the Requests icon (with its badge) for the user to choose.
#[gpui::test]
fn a_new_approval_does_not_expand_a_collapsed_right_panel(cx: &mut TestAppContext) {
    let env = common::chat::ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, _| wb.chat_view_of(info.id).is_some())
    });
    cx.simulate_keystrokes("cmd-l");
    draw(cx);
    assert_eq!(chosen(&view, cx, Right), Compact);

    cx.simulate_input("Run `touch hi.txt` in a shell, then reply with the word done.");
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "the permission request", |cx| {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).approvals().len() == 1)
    });
    draw(cx);
    assert_eq!(chosen(&view, cx, Right), Compact, "still collapsed");
    assert!(!painted(&view, cx, Region::Dock(Right)));
    assert!(
        painted(&view, cx, Region::DockTab(Right, 3)),
        "the Requests icon is on the rail"
    );
    assert!(
        matches!(
            state(&view, cx).focus,
            hutan_desktop::workbench::model::FocusTarget::Pane(_)
        ),
        "the keyboard stays where it was"
    );
    // The panel the rail remembers does not change; the Requests icon opens it.
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("changes"))
    );
    click(&view, cx, Region::DockTab(Right, 3));
    assert_eq!(chosen(&view, cx, Right), Expanded);
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("inspector"))
    );
}
