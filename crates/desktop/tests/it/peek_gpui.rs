#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! EX-2, the rail peek: in a window too narrow for the center to keep 480 pt beside a side
//! panel, a rail icon (or ⌘L, ⌘⇧E, a panel command) opens the panel as a layer floating over
//! the center instead of squeezing it. The §3.7 table of `docs/spikes/2026-10-27-file-explorer.md`
//! window by window, what the peek is (the dock's width, beside the rail, under the title bar,
//! the center untouched, nothing saved), what closes it (Esc, a click outside, the same icon,
//! ⌘L, opening a file, a window wide enough to push), the keyboard going in and coming back,
//! the left sidebar's Files icon, and `layout.rail_peek` off.

use crate::common;

use common::{TestDaemon, draw, offline_connection, open_with, wait_until};
use gpui::{
    Bounds, Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext,
    point, px, size,
};
use hutan_desktop::workbench::actions::{self, OpenPanel};
use hutan_desktop::workbench::model::{
    AutoState, DockMode, DockPosition, FocusTarget, PanelDest, PanelId, WorkbenchState,
};
use hutan_desktop::workbench::{FileOpen, Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

use DockMode::{Compact, Expanded, Hidden};
use DockPosition::{Left, Right};

fn open(cx: &mut TestAppContext) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    let (view, cx) = open_with(cx, settings, connection, None);
    (view, cx)
}

fn open_without_peek(cx: &mut TestAppContext) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    settings
        .write_user("layout.rail_peek", toml::Value::Boolean(false))
        .unwrap();
    let (view, cx) = open_with(cx, settings, connection, None);
    (view, cx)
}

fn state(view: &Entity<Workbench>, cx: &VisualTestContext) -> WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

fn shown(view: &Entity<Workbench>, cx: &VisualTestContext, pos: DockPosition) -> DockMode {
    view.read_with(cx, |wb, _| wb.dock_mode(pos))
}

fn chosen(view: &Entity<Workbench>, cx: &VisualTestContext, pos: DockPosition) -> DockMode {
    view.read_with(cx, |wb, _| wb.state().docks.get(pos).mode())
}

fn peeking(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<DockPosition> {
    view.read_with(cx, |wb, _| wb.peek_dock())
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
fn assert_close(actual: Pixels, expected: f32) {
    let actual = f32::from(actual);
    assert!(
        (actual - expected).abs() <= 1.5,
        "expected {expected} (±1.5), got {actual}"
    );
}

fn resize(cx: &mut VisualTestContext, width: f32) {
    cx.simulate_resize(size(px(width), px(800.0)));
    draw(cx);
    draw(cx);
}

fn active(view: &Entity<Workbench>, cx: &VisualTestContext, pos: DockPosition) -> Option<PanelId> {
    state(view, cx).docks.get(pos).active.clone()
}

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

/// A point of the center that no peek covers: near its left edge, halfway down.
fn center_point(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Point<Pixels> {
    let center = bounds(view, cx, Region::Center);
    point(center.left() + px(40.0), center.center().y)
}

/// The rail icon of the right side's panel `index` (Files 0, Changes 1, Agents 2, History 3).
fn icon(index: usize) -> Region {
    Region::DockTab(Right, index)
}

// ---- the §3.7 table ------------------------------------------------------------------------

/// Window by window: a rail click pushes where the center keeps 480 pt and peeks where it would
/// not. The right panel starts on its rail by the user's own choice (⌘L), so the window's own
/// collapse is not what is being tested.
#[gpui::test]
fn the_window_width_decides_push_or_peek(cx: &mut TestAppContext) {
    for (width, peeks) in [
        (1440.0, false),
        (1280.0, false),
        (1180.0, false),
        (1100.0, true),
        (900.0, true),
        (640.0, true),
    ] {
        let (view, cx) = open(cx);
        cx.simulate_keystrokes("cmd-l");
        resize(cx, width);
        assert_eq!(chosen(&view, cx, Right), Compact, "{width}");
        click(&view, cx, icon(2));
        if peeks {
            assert_eq!(peeking(&view, cx), Some(Right), "{width}: a peek");
            assert!(painted(&view, cx, Region::Peek), "{width}");
            assert!(!painted(&view, cx, Region::Dock(Right)), "{width}");
            assert_eq!(chosen(&view, cx, Right), Compact, "{width}: nothing saved");
        } else {
            assert_eq!(peeking(&view, cx), None, "{width}: pushed");
            assert!(!painted(&view, cx, Region::Peek), "{width}");
            assert!(painted(&view, cx, Region::Dock(Right)), "{width}");
            assert_eq!(chosen(&view, cx, Right), Expanded, "{width}");
        }
        assert_eq!(active(&view, cx, Right), Some(PanelId::new("agents")));
    }
}

/// The same table from the window's own collapse: at 1100 the default layout shows the rail
/// alone, and the click is a peek.
#[gpui::test]
fn the_rail_the_window_left_behind_peeks(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    for (width, rest, peek) in [
        (1280.0, Expanded, false),
        (1180.0, Expanded, false),
        (1100.0, Compact, true),
        (900.0, Compact, true),
        (640.0, Compact, true),
    ] {
        resize(cx, width);
        assert_eq!(shown(&view, cx, Right), rest, "{width}");
        assert_eq!(peeking(&view, cx), None, "{width}: at rest nothing floats");
        assert!(painted(&view, cx, Region::Rail(Right)), "{width}");
        if peek {
            click(&view, cx, icon(0));
            assert_eq!(peeking(&view, cx), Some(Right), "{width}");
            cx.simulate_keystrokes("escape");
            draw(cx);
            assert_eq!(peeking(&view, cx), None, "{width}");
        }
    }
}

// ---- what the peek is ------------------------------------------------------------------------

#[gpui::test]
fn the_peek_floats_over_the_center_at_the_docks_width_and_changes_nothing(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    resize(cx, 1100.0);
    let saved = state(&view, cx);
    let center = bounds(&view, cx, Region::Center);
    let rail = bounds(&view, cx, Region::Rail(Right));
    let titlebar = bounds(&view, cx, Region::Titlebar);

    click(&view, cx, icon(0));
    let peek = bounds(&view, cx, Region::Peek);
    assert_close(peek.size.width, 350.0);
    assert_close(peek.right(), f32::from(rail.left()));
    assert!(
        peek.top() >= titlebar.bottom() - px(1.0),
        "under the title bar, whose buttons stay usable"
    );
    assert!(peek.right() <= center.right() + px(60.0));
    // The center never moves.
    assert_eq!(bounds(&view, cx, Region::Center), center);
    assert_eq!(bounds(&view, cx, Region::Rail(Right)), rail);
    // The dock stays on its rail; only the panel it remembers is the one that was clicked.
    assert_eq!(shown(&view, cx, Right), Compact);
    assert_eq!(
        chosen(&view, cx, Right),
        Expanded,
        "the user's choice stands"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.dock_auto_state(Right)),
        AutoState::Collapsed,
        "the window's collapse is not turned into an override"
    );
    assert_eq!(active(&view, cx, Right), Some(PanelId::new("files")));
    let mut expected = saved.clone();
    expected.docks.right.active = Some(PanelId::new("files"));
    expected.focus = state(&view, cx).focus;
    assert_eq!(state(&view, cx), expected, "nothing but the panel is saved");
}

#[gpui::test]
fn every_right_panel_peeks_and_a_second_icon_switches_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    resize(cx, 1100.0);
    for (index, panel) in [(0, "files"), (1, "changes"), (2, "agents"), (3, "history")] {
        click(&view, cx, icon(index));
        assert_eq!(peeking(&view, cx), Some(Right), "{panel}");
        assert_eq!(active(&view, cx, Right), Some(PanelId::new(panel)));
        assert!(painted(&view, cx, Region::Peek), "{panel}");
    }
    // The icon of the panel shown closes it.
    click(&view, cx, icon(3));
    assert_eq!(peeking(&view, cx), None);
    assert!(!painted(&view, cx, Region::Peek));
    assert_eq!(shown(&view, cx, Right), Compact);
}

#[gpui::test]
fn in_a_window_under_900_the_peek_is_at_most_300_wide(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    resize(cx, 640.0);
    click(&view, cx, icon(0));
    let peek = bounds(&view, cx, Region::Peek);
    assert_close(peek.size.width, 300.0);
    assert_close(
        peek.right(),
        f32::from(bounds(&view, cx, Region::Rail(Right)).left()),
    );
    // Resizing the window under an open peek keeps it a peek.
    resize(cx, 700.0);
    assert_eq!(peeking(&view, cx), Some(Right));
    assert!(painted(&view, cx, Region::Peek));
}

// ---- what closes it --------------------------------------------------------------------------

#[gpui::test]
fn escape_closes_the_peek_and_gives_the_keyboard_back(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    resize(cx, 1100.0);
    assert!(workbench_holds_the_keyboard(&view, cx));
    click(&view, cx, icon(0));
    // The keyboard is in the peek while it is open …
    assert!(dock_holds_the_keyboard(&view, cx, Right));
    assert_eq!(state(&view, cx).focus, FocusTarget::Dock(Right));
    // … and Escape (nothing deeper takes it) closes it and hands it back to the center.
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(peeking(&view, cx), None);
    assert!(!painted(&view, cx, Region::Peek));
    assert!(!dock_holds_the_keyboard(&view, cx, Right));
    assert!(workbench_holds_the_keyboard(&view, cx));
    assert!(matches!(state(&view, cx).focus, FocusTarget::Pane(_)));
    assert_eq!(shown(&view, cx, Right), Compact);
    // Escape with no peek is not claimed.
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(chosen(&view, cx, Left), Expanded);
}

#[gpui::test]
fn a_click_outside_closes_the_peek_and_does_not_reach_what_is_under_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    resize(cx, 1100.0);
    click(&view, cx, icon(1));
    assert_eq!(peeking(&view, cx), Some(Right));
    let before = state(&view, cx);
    let at = center_point(&view, cx);
    cx.simulate_click(at, Modifiers::none());
    draw(cx);
    assert_eq!(peeking(&view, cx), None);
    assert!(workbench_holds_the_keyboard(&view, cx));
    assert_eq!(
        state(&view, cx).center,
        before.center,
        "the click only closed the peek"
    );
    // A click inside the peek keeps it.
    click(&view, cx, icon(1));
    let inside = bounds(&view, cx, Region::Peek).center();
    cx.simulate_click(inside, Modifiers::none());
    draw(cx);
    assert_eq!(peeking(&view, cx), Some(Right));
}

#[gpui::test]
fn the_same_icon_and_cmd_l_close_the_peek(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    resize(cx, 1100.0);
    click(&view, cx, icon(2));
    click(&view, cx, icon(2));
    assert_eq!(peeking(&view, cx), None);
    // ⌘L opens the remembered panel as a peek and closes it again.
    cx.simulate_keystrokes("cmd-l");
    draw(cx);
    assert_eq!(peeking(&view, cx), Some(Right));
    assert_eq!(active(&view, cx, Right), Some(PanelId::new("agents")));
    assert!(dock_holds_the_keyboard(&view, cx, Right));
    cx.simulate_keystrokes("cmd-l");
    draw(cx);
    assert_eq!(peeking(&view, cx), None);
    assert!(workbench_holds_the_keyboard(&view, cx));
    // The titlebar's right button closes an open peek...
    click(&view, cx, icon(2));
    assert_eq!(peeking(&view, cx), Some(Right));
    click(&view, cx, Region::DockToggle(Right));
    assert_eq!(peeking(&view, cx), None);
    assert_eq!(chosen(&view, cx, Right), Expanded, "never changed");
    // ...and otherwise hides the rail and brings it back like the left one's does: a rail
    // again in this window (the body would only have been a peek), never the body pushed in.
    click(&view, cx, Region::DockToggle(Right));
    assert_eq!(shown(&view, cx, Right), Hidden);
    assert!(!painted(&view, cx, Region::Rail(Right)));
    click(&view, cx, Region::DockToggle(Right));
    assert_eq!(shown(&view, cx, Right), Compact);
    assert!(painted(&view, cx, Region::Rail(Right)));
    assert_eq!(peeking(&view, cx), None);
    assert_eq!(chosen(&view, cx, Right), Expanded);
}

#[gpui::test]
fn a_window_wide_enough_to_push_ends_the_peek(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    resize(cx, 1100.0);
    click(&view, cx, icon(0));
    assert_eq!(peeking(&view, cx), Some(Right));
    // 1200: still under the 1260 that brings the body back, but the center would keep 510.
    resize(cx, 1200.0);
    assert_eq!(peeking(&view, cx), None);
    assert!(!painted(&view, cx, Region::Peek));
    assert!(workbench_holds_the_keyboard(&view, cx));
    assert_eq!(shown(&view, cx, Right), Compact);
}

#[gpui::test]
fn opening_a_file_closes_the_peek(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let (view, cx) = open_with(cx, settings, daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().canonicalize().unwrap().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();

    resize(cx, 1100.0);
    // ⌘⇧E: the Files panel, as a peek.
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    assert_eq!(peeking(&view, cx), Some(Right));
    assert_eq!(active(&view, cx, Right), Some(PanelId::new("files")));
    assert!(painted(&view, cx, Region::Peek));

    let wanted = file.display().to_string();
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&file), window, cx);
    });
    wait_until(cx, "the file in the focused tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state()
                .active_center_item()
                .and_then(|item| item.as_file().map(str::to_owned))
                .is_some_and(|path| path == wanted)
        })
    });
    wait_until(cx, "the peek to close", |cx| peeking(&view, cx).is_none());
    assert!(!painted(&view, cx, Region::Peek));
    assert!(!dock_holds_the_keyboard(&view, cx, Right));
    assert!(matches!(state(&view, cx).focus, FocusTarget::Pane(_)));
    assert_eq!(shown(&view, cx, Right), Compact);
}

// ---- commands ---------------------------------------------------------------------------------

#[gpui::test]
fn panel_commands_peek_too(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    resize(cx, 1100.0);
    cx.dispatch_action(OpenPanel("history".into()));
    draw(cx);
    assert_eq!(peeking(&view, cx), Some(Right));
    assert_eq!(active(&view, cx, Right), Some(PanelId::new("history")));
    assert!(dock_holds_the_keyboard(&view, cx, Right));
    // ⇧⌘E on top of it shows Files in the same peek.
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    assert_eq!(peeking(&view, cx), Some(Right));
    assert_eq!(active(&view, cx, Right), Some(PanelId::new("files")));
    assert_eq!(chosen(&view, cx, Right), Expanded);
    assert_eq!(shown(&view, cx, Right), Compact);
    // Something else taking the focus ends it: moving to the pane on its left.
    cx.dispatch_action(actions::FocusNeighborLeft);
    draw(cx);
    assert_eq!(peeking(&view, cx), None);
}

#[gpui::test]
fn the_titlebar_menu_state_open_is_a_push_not_a_peek(cx: &mut TestAppContext) {
    // Choosing "Panel open" explicitly is the user's own decision: the body is pushed in.
    let (view, cx) = open(cx);
    resize(cx, 1100.0);
    let at = bounds(&view, cx, Region::DockToggle(Right)).center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
    draw(cx);
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(0, window, cx));
    draw(cx);
    assert_eq!(peeking(&view, cx), None);
    assert_eq!(shown(&view, cx, Right), Expanded);
    assert!(painted(&view, cx, Region::Dock(Right)));
}

// ---- the left sidebar ---------------------------------------------------------------------------

fn files_on_the_left(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    view.update(cx, |wb, cx| {
        wb.place_panel(&PanelId::new("files"), PanelDest::Dock(Left), cx);
    });
    draw(cx);
    assert_eq!(
        state(view, cx).docks.find(&PanelId::new("files")),
        Some(Left)
    );
}

/// The Explorer-left preset (EX-1) makes Files the left sidebar's active page: in a narrow window
/// the compact rail's Files icon is still a peek.
#[gpui::test]
fn the_explorer_left_preset_keeps_files_reachable_in_a_narrow_window(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.apply_preset(&hutan_desktop::workbench::model::LayoutPreset::explorer_left_preset());
        });
    });
    draw(cx);
    resize(cx, 700.0);
    assert_eq!(shown(&view, cx, Left), Compact);
    assert_eq!(
        state(&view, cx).docks.find(&PanelId::new("files")),
        Some(Left)
    );
    click(&view, cx, Region::Nav("files"));
    assert_eq!(peeking(&view, cx), Some(Left));
    assert!(painted(&view, cx, Region::Peek));
    assert_eq!(shown(&view, cx, Left), Compact);
    click(&view, cx, Region::Nav("files"));
    assert_eq!(peeking(&view, cx), None);
}

#[gpui::test]
fn files_on_the_left_stays_reachable_from_the_compact_rail(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    files_on_the_left(&view, cx);
    resize(cx, 700.0);
    assert_eq!(shown(&view, cx, Left), Compact);
    let center = bounds(&view, cx, Region::Center);
    let rail = bounds(&view, cx, Region::Rail(Left));

    click(&view, cx, Region::Nav("files"));
    assert_eq!(peeking(&view, cx), Some(Left));
    let peek = bounds(&view, cx, Region::Peek);
    assert_close(peek.left(), f32::from(rail.right()));
    assert_close(peek.size.width, 280.0);
    assert_eq!(bounds(&view, cx, Region::Center), center, "nothing moved");
    assert_eq!(shown(&view, cx, Left), Compact);
    assert_eq!(active(&view, cx, Left), Some(PanelId::new("files")));
    assert!(dock_holds_the_keyboard(&view, cx, Left));

    // Its icon closes it again, then Escape closes a second one.
    click(&view, cx, Region::Nav("files"));
    assert_eq!(peeking(&view, cx), None);
    click(&view, cx, Region::Nav("files"));
    assert_eq!(peeking(&view, cx), Some(Left));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(peeking(&view, cx), None);
    assert!(workbench_holds_the_keyboard(&view, cx));
}

#[gpui::test]
fn the_left_page_pushes_where_the_center_keeps_its_room(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    files_on_the_left(&view, cx);
    // 850: compact left (44) + right rail (40) leave 766; the 280 body leaves 486 ≥ 480.
    resize(cx, 850.0);
    assert_eq!(shown(&view, cx, Left), Compact);
    click(&view, cx, Region::Nav("files"));
    assert_eq!(peeking(&view, cx), None);
    assert_eq!(shown(&view, cx, Left), Expanded);
    assert_eq!(active(&view, cx, Left), Some(PanelId::new("files")));
    // Once the window has grown the sidebar is back as its own width, nothing left over.
    resize(cx, 1280.0);
    assert_eq!(shown(&view, cx, Left), Expanded);
}

// ---- the setting -------------------------------------------------------------------------------

#[gpui::test]
fn with_rail_peek_off_the_body_is_pushed_in_as_before(cx: &mut TestAppContext) {
    let (view, cx) = open_without_peek(cx);
    resize(cx, 1100.0);
    click(&view, cx, icon(0));
    assert_eq!(peeking(&view, cx), None);
    assert!(!painted(&view, cx, Region::Peek));
    assert_eq!(shown(&view, cx, Right), Expanded);
    assert!(painted(&view, cx, Region::Dock(Right)));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.dock_auto_state(Right)),
        AutoState::Overridden
    );
    // ⌘L and commands push as well.
    cx.simulate_keystrokes("cmd-l");
    cx.simulate_keystrokes("cmd-l");
    draw(cx);
    assert_eq!(peeking(&view, cx), None);
}

#[gpui::test]
fn turning_the_setting_on_or_off_applies_to_the_next_click(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let settings = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
    let (connection, _, _) = offline_connection();
    let (view, cx) = open_with(cx, settings.clone(), connection, None);
    resize(cx, 1100.0);
    click(&view, cx, icon(0));
    assert_eq!(peeking(&view, cx), Some(Right));
    // Off while a peek is open: it goes (the dock is not pushed open by it).
    settings
        .write_user("layout.rail_peek", toml::Value::Boolean(false))
        .unwrap();
    wait_until(cx, "the peek to end", |cx| peeking(&view, cx).is_none());
    assert_eq!(shown(&view, cx, Right), Compact);
    click(&view, cx, icon(0));
    assert_eq!(shown(&view, cx, Right), Expanded);
    assert_eq!(peeking(&view, cx), None);
}
