#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Orca-style shell (OL-2): the default regions and their sizes, the left sidebar's nav pages,
//! the right sidebar's icon strip, Mawas that is off by default and opens as a tab of the center,
//! ⌘B / ⌘L, the presets, and the one-time migration of a saved layout. No daemon: layout only.

use crate::common;

use common::{draw, offline_connection, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::actions::{self, OpenAssistant, OpenPanel};
use hutan_desktop::workbench::model::{
    Dock, DockPosition, Docks, FocusTarget, ItemRef, PanelDest, PanelId, WorkbenchState,
};
use hutan_desktop::workbench::persistence::{STATE_FILE, load_state};
use hutan_desktop::workbench::{Region, SAVE_DEBOUNCE, Workbench};
use hutan_settings::SettingsStore;
use std::path::PathBuf;

fn open_in(
    cx: &mut TestAppContext,
    assistant: Option<bool>,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, SettingsStore, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    // `None`: the default, which is on.
    if let Some(enabled) = assistant {
        settings
            .write_user("assistant.enabled", toml::Value::Boolean(enabled))
            .unwrap();
    }
    let (view, cx) = open_with(cx, settings.clone(), connection, state_path);
    (view, settings, cx)
}

fn open(cx: &mut TestAppContext) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (view, _, cx) = open_in(cx, None, None);
    (view, cx)
}

fn state(view: &Entity<Workbench>, cx: &VisualTestContext) -> WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

fn ids(panels: &[&str]) -> Vec<PanelId> {
    panels.iter().map(|p| PanelId::new(*p)).collect()
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

/// Probes measure the content box, so a 1 px border makes a dock or bar read 1 px smaller.
#[track_caller]
fn assert_size(actual: gpui::Pixels, expected: f32) {
    let actual = f32::from(actual);
    assert!(
        (actual - expected).abs() <= 1.0,
        "expected {expected} (±1 border), got {actual}"
    );
}

fn nav_keys(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, cx| {
        wb.nav_entries(cx).into_iter().map(|e| e.key).collect()
    })
}

fn selected_nav(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, cx| {
        wb.nav_entries(cx)
            .into_iter()
            .filter(|e| e.selected)
            .map(|e| e.key)
            .collect()
    })
}

fn visible(view: &Entity<Workbench>, cx: &VisualTestContext) -> [bool; 3] {
    let s = state(view, cx);
    [
        s.docks.left.is_expanded(),
        s.docks.right.is_expanded(),
        s.docks.bottom.is_expanded(),
    ]
}

// ---- regions --------------------------------------------------------------------------------

#[gpui::test]
fn the_default_layout_is_three_columns_between_a_36px_titlebar_row_and_a_24px_status_bar(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open(cx);
    let s = state(&view, cx);
    assert_eq!(s.preset, "default");
    assert_eq!(s.docks.left.panels, ids(&["sessions", "tasks"]));
    assert_eq!(s.docks.left.active, Some(PanelId::new("sessions")));
    assert_eq!(
        s.docks.right.panels,
        ids(&[
            "files",
            "changes",
            "agents",
            "history",
            "inspector",
            "resources"
        ])
    );
    assert_eq!(s.docks.right.more, ids(&["inspector", "resources"]));
    assert_eq!(s.docks.right.active, Some(PanelId::new("changes")));
    assert!(s.docks.bottom.panels.is_empty() && !s.docks.bottom.visible);
    assert!(s.docks.left.visible && s.docks.right.visible);
    assert_eq!((s.docks.left.size, s.docks.right.size), (280.0, 350.0));

    let left = bounds(&view, cx, Region::Dock(DockPosition::Left));
    let right = bounds(&view, cx, Region::Dock(DockPosition::Right));
    let center = bounds(&view, cx, Region::Center);
    let titlebar = bounds(&view, cx, Region::Titlebar);
    let status = bounds(&view, cx, Region::StatusBar);
    assert_size(left.size.width, 280.0);
    assert_size(right.size.width, 350.0);
    assert_size(titlebar.size.height, 36.0);
    assert_size(status.size.height, 24.0);
    // Left | center | right, side by side; the right sidebar starts below the title bar row.
    assert!(left.right() <= center.origin.x + gpui::px(1.0));
    assert!(center.right() <= right.origin.x + gpui::px(1.0));
    // The right sidebar is a column of its own: its strip is the title bar row of that side.
    assert!((f32::from(right.origin.y) - f32::from(titlebar.origin.y)).abs() <= 1.0);

    // The left nav: Agents, Mawas (on by default) and Memory under it.
    assert_eq!(nav_keys(&view, cx), ["agents", "assistant", "mawas-memory"]);
    assert_eq!(
        selected_nav(&view, cx),
        Vec::<String>::new(),
        "Projects shows"
    );
    for key in ["agents", "assistant"] {
        assert!(painted(&view, cx, Region::Nav(key)), "{key}");
    }
    for key in ["search", "tasks"] {
        assert!(!painted(&view, cx, Region::Nav(key)), "no {key} row");
    }
    assert!(
        !painted(&view, cx, Region::Nav("projects")),
        "no back row on Projects"
    );
    assert!(painted(&view, cx, Region::Toolbar("start")));
    assert!(painted(&view, cx, Region::Toolbar("settings")));
    // The right rail: four icons (Files first) and ⋯, 32 pt targets under the title bar row, outside the
    // body; Requests and Resources are in ⋯ (CL-CHROME).
    let rail = bounds(&view, cx, Region::Rail(DockPosition::Right));
    assert_size(rail.size.width, 40.0);
    assert!(rail.left() >= right.right());
    for index in 0..4 {
        let icon = bounds(&view, cx, Region::DockTab(DockPosition::Right, index));
        assert!(rail.contains(&icon.center()), "icon {index}");
        assert!(icon.origin.y >= titlebar.bottom(), "icon {index}");
        assert_size(icon.size.height, 32.0);
    }
    for index in 4..6 {
        assert!(!painted(
            &view,
            cx,
            Region::DockTab(DockPosition::Right, index)
        ));
    }
    assert!(rail.contains(&bounds(&view, cx, Region::StripMore).center()));
}

#[gpui::test]
fn the_left_nav_toolbar_starts_a_session_and_opens_settings(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    click(&view, cx, Region::Toolbar("start"));
    // Start is the Start sheet (launcher_gpui drives it against a daemon); offline it says so.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string)),
        Some("Not connected to hutand".to_owned()),
    );
    click(&view, cx, Region::Toolbar("settings"));
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_some()));
}

// ---- left nav ---------------------------------------------------------------------------------

#[gpui::test]
fn nav_items_switch_the_left_page_and_the_back_row_returns_to_projects(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    let page = |cx: &VisualTestContext| state(&view, cx).docks.left.active.unwrap();
    assert_eq!(page(cx).as_str(), "sessions");

    // Tasks has no row until a task exists; its page can still be opened (⌘K, Start sheet) and
    // then the row of the page you are on is there, selected.
    assert!(!painted(&view, cx, Region::Nav("tasks")));
    cx.dispatch_action(OpenPanel("tasks".into()));
    draw(cx);
    assert_eq!(page(cx).as_str(), "tasks");
    assert_eq!(selected_nav(&view, cx), ["tasks"]);
    assert!(
        painted(&view, cx, Region::Nav("projects")),
        "a page shows its way back"
    );
    // The panel list of the dock is untouched: pages are the dock's panels.
    assert_eq!(
        state(&view, cx).docks.left.panels,
        ids(&["sessions", "tasks"])
    );

    // Agents is no page of the sidebar: it opens the Agents page in the center and leaves the
    // sidebar's page as it is; a second click focuses the same tab.
    click(&view, cx, Region::Nav("agents"));
    assert_eq!(page(cx).as_str(), "tasks");
    // One selected row: the center page on screen wins over the sidebar's page.
    assert_eq!(selected_nav(&view, cx), ["agents"]);
    click(&view, cx, Region::Nav("agents"));
    // One selected row: the center page on screen wins over the sidebar's page.
    assert_eq!(selected_nav(&view, cx), ["agents"]);

    // The back row, and a second click on the selected row, both return to Projects.
    click(&view, cx, Region::Nav("projects"));
    assert_eq!(page(cx).as_str(), "sessions");
    assert!(!painted(&view, cx, Region::Nav("projects")));
    assert!(
        !painted(&view, cx, Region::Nav("tasks")),
        "no task and no page: no row"
    );
    cx.dispatch_action(OpenPanel("tasks".into()));
    draw(cx);
    click(&view, cx, Region::Nav("tasks"));
    assert_eq!(page(cx).as_str(), "sessions");
    assert_eq!(
        state(&view, cx).focus,
        FocusTarget::Dock(DockPosition::Left)
    );
}

#[gpui::test]
fn a_panel_left_in_the_left_dock_gets_its_own_nav_row(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.move_panel(
                &PanelId::new("resources"),
                PanelDest::Dock(DockPosition::Left),
            )
            .unwrap();
        });
    });
    assert_eq!(
        nav_keys(&view, cx),
        ["agents", "assistant", "mawas-memory", "resources"]
    );
    assert_eq!(
        selected_nav(&view, cx),
        ["resources"],
        "the move made it the page"
    );
    // It is the dock's panel at index 2 (after Projects and Tasks): the same probe the right
    // strip uses.
    assert!(painted(&view, cx, Region::DockTab(DockPosition::Left, 2)));
}

// ---- assistant ---------------------------------------------------------------------------------

#[gpui::test]
fn mawas_turned_off_has_no_place_anywhere(cx: &mut TestAppContext) {
    let (view, _settings, cx) = open_in(cx, Some(false), None);
    assert!(!view.read_with(cx, |wb, _| wb.assistant_enabled()));
    assert!(!nav_keys(&view, cx).contains(&"assistant".to_owned()));
    let s = state(&view, cx);
    assert!(s.find_panel(&PanelId::new("assistant")).is_none());
    // The commands are gone and opening it does nothing.
    assert!(!view.read_with(cx, |wb, _| {
        wb.commands().iter().any(|c| c.title.contains("Mawas"))
    }));
    cx.dispatch_action(OpenAssistant);
    cx.dispatch_action(OpenPanel("assistant".into()));
    assert!(
        state(&view, cx)
            .find_panel(&PanelId::new("assistant"))
            .is_none()
    );
    for preset in [
        LayoutPresetName::Default,
        LayoutPresetName::Focus,
        LayoutPresetName::Overview,
    ] {
        let layout = hutan_desktop::workbench::model::LayoutPreset::builtin()
            .into_iter()
            .find(|p| p.name == preset.name())
            .unwrap();
        for pos in DockPosition::ALL {
            assert!(
                layout
                    .docks
                    .get(pos)
                    .panels
                    .iter()
                    .all(|p| p.as_str() != "assistant"),
                "{} lists Mawas in {pos:?}",
                layout.name
            );
        }
    }
}

enum LayoutPresetName {
    Default,
    Focus,
    Overview,
}

impl LayoutPresetName {
    fn name(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Focus => "focus",
            Self::Overview => "overview",
        }
    }
}

#[gpui::test]
fn the_enabled_mawas_is_one_center_tab_opened_from_the_nav_and_closable(cx: &mut TestAppContext) {
    let (view, _settings, cx) = open_in(cx, Some(true), None);
    assert!(view.read_with(cx, |wb, _| wb.assistant_enabled()));
    assert_eq!(nav_keys(&view, cx), ["agents", "assistant", "mawas-memory"]);
    let labels: Vec<String> = view.read_with(cx, |wb, cx| {
        wb.nav_entries(cx)
            .into_iter()
            .map(|e| e.label.to_string())
            .collect()
    });
    assert_eq!(labels.last().map(String::as_str), Some("Memory"));
    // Enabled is not open: no preset places it, nothing opened it.
    assert!(
        state(&view, cx)
            .find_panel(&PanelId::new("assistant"))
            .is_none()
    );
    assert!(selected_nav(&view, cx).is_empty());

    // The nav row opens Home as a tab of the center, the active one, and is selected while it is.
    let home = ItemRef::panel("assistant");
    click(&view, cx, Region::Nav("assistant"));
    let s = state(&view, cx);
    assert_eq!(s.active_center_item(), Some(home.clone()));
    assert!(
        s.docks.find(&PanelId::new("assistant")).is_none(),
        "not in a dock"
    );
    assert_eq!(s.docks.left.active, Some(PanelId::new("sessions")));
    assert_eq!(selected_nav(&view, cx), ["assistant"]);
    let tabs = |cx: &VisualTestContext| {
        state(&view, cx)
            .all_items()
            .into_iter()
            .filter(|i| *i == home)
            .count()
    };
    assert_eq!(tabs(cx), 1);

    // Every other way in lands on the same tab: there is never a second one.
    cx.dispatch_action(OpenPanel("assistant".into()));
    cx.dispatch_action(OpenAssistant);
    click(&view, cx, Region::Nav("assistant"));
    assert_eq!(tabs(cx), 1, "one Home");
    assert_eq!(state(&view, cx).active_center_item(), Some(home.clone()));

    // Closing the tab closes Home; the nav row opens it again.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.close_panel(&PanelId::new("assistant")).unwrap();
        })
    });
    assert_eq!(tabs(cx), 0);
    assert!(selected_nav(&view, cx).is_empty());
    click(&view, cx, Region::Nav("assistant"));
    assert_eq!(tabs(cx), 1);

    // The palette's titles.
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.commands().iter().map(|c| c.title.to_string()).collect()
    });
    assert!(titles.iter().any(|t| t == "Mawas: Open"), "{titles:?}");
    assert!(titles.iter().any(|t| t == "Mawas: New chat"), "{titles:?}");
    assert!(
        !titles.iter().any(|t| t.contains("Assistant")),
        "no command still says Assistant: {titles:?}"
    );

    // Other tabs next to it keep working; the right-click menu of the row is the panel's.
    let right_click = |cx: &mut VisualTestContext| {
        let at = bounds(&view, cx, Region::Nav("assistant")).center();
        cx.simulate_mouse_down(at, gpui::MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(at, gpui::MouseButton::Right, Modifiers::none());
    };
    right_click(cx);
    let labels: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("a menu opened")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert!(labels.iter().any(|l| l == "Close"), "{labels:?}");
    cx.simulate_keystrokes("escape");
}

#[gpui::test]
fn turning_mawas_off_closes_its_tab_and_its_nav_row(cx: &mut TestAppContext) {
    let (view, settings, cx) = open_in(cx, Some(true), None);
    click(&view, cx, Region::Nav("assistant"));
    assert!(view.read_with(cx, |wb, _| wb.panel_open("assistant")));
    settings
        .write_user("assistant.enabled", toml::Value::Boolean(false))
        .unwrap();
    wait_until(cx, "the setting to apply", |cx| {
        view.read_with(cx, |wb, _| {
            !wb.assistant_enabled() && !wb.panel_open("assistant")
        })
    });
    assert_eq!(nav_keys(&view, cx), ["agents"]);
    assert_eq!(
        state(&view, cx).docks.left.active.unwrap().as_str(),
        "sessions",
        "Projects stays the left page"
    );
}

// ---- right strip -------------------------------------------------------------------------------

#[gpui::test]
fn the_right_strip_switches_tabs(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    let active = |cx: &VisualTestContext| state(&view, cx).docks.right.active.unwrap();
    assert_eq!(active(cx).as_str(), "changes", "Changes is the default tab");
    for (index, panel) in [(2, "agents"), (3, "history"), (0, "files"), (1, "changes")] {
        click(&view, cx, Region::DockTab(DockPosition::Right, index));
        assert_eq!(active(cx).as_str(), panel);
        assert_eq!(
            state(&view, cx).focus,
            FocusTarget::Dock(DockPosition::Right)
        );
    }
}

#[gpui::test]
fn icons_that_do_not_fit_move_into_the_more_menu(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    // A window 240 pt high leaves the rail room for three slots, one of them More.
    cx.simulate_resize(gpui::size(gpui::px(1280.0), gpui::px(240.0)));
    common::draw(cx);
    common::draw(cx);
    let more = bounds(&view, cx, Region::StripMore);
    // Files and Changes fit.
    assert!(painted(&view, cx, Region::DockTab(DockPosition::Right, 0)));
    assert!(painted(&view, cx, Region::DockTab(DockPosition::Right, 1)));
    assert!(!painted(&view, cx, Region::DockTab(DockPosition::Right, 2)));

    cx.simulate_click(more.center(), Modifiers::none());
    let labels: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the More menu opened")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    // The icon that did not fit, then the panels the dock lists in ⋯.
    assert_eq!(labels, ["Agents", "History", "Requests", "Resources"]);

    // Choosing one makes it the active tab, and the active tab never hides.
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(0, window, cx));
    let s = state(&view, cx);
    assert_eq!(s.docks.right.active, Some(PanelId::new("agents")));
    assert!(painted(&view, cx, Region::DockTab(DockPosition::Right, 2)));
}

// ---- keys --------------------------------------------------------------------------------------

#[gpui::test]
fn cmd_b_toggles_the_left_sidebar_and_cmd_l_the_right_one(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    assert_eq!(visible(&view, cx), [true, true, false]);
    cx.simulate_keystrokes("cmd-b");
    assert_eq!(visible(&view, cx), [false, true, false]);
    cx.simulate_keystrokes("cmd-b");
    assert_eq!(visible(&view, cx), [true, true, false]);
    cx.simulate_keystrokes("cmd-l");
    assert_eq!(visible(&view, cx), [true, false, false]);
    cx.simulate_keystrokes("cmd-l");
    assert_eq!(visible(&view, cx), [true, true, false]);
    // The tooltip hint of the right dock is the new key.
    cx.update(|window, _| {
        let binding = window
            .highest_precedence_binding_for_action(&actions::ToggleRightDock)
            .expect("bound");
        let keys: Vec<String> = binding.keystrokes().iter().map(|k| k.to_string()).collect();
        assert_eq!(keys.join(" "), "⌘L");
    });
}

// ---- presets -----------------------------------------------------------------------------------

#[gpui::test]
fn presets_arrange_the_sidebars(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.dispatch_action(actions::ApplyPreset("focus".into()));
    let s = state(&view, cx);
    assert_eq!(visible(&view, cx), [false, false, false]);
    assert_eq!(s.docks.left.panels, ids(&["sessions", "tasks"]));
    assert_eq!(
        s.docks.right.panels,
        ids(&[
            "files",
            "changes",
            "agents",
            "history",
            "inspector",
            "resources"
        ])
    );
    assert!(s.zoomed.is_some());

    cx.dispatch_action(actions::ApplyPreset("default".into()));
    assert_eq!(visible(&view, cx), [true, true, false]);

    // The Agents overview shows the Agents page in the center.
    cx.dispatch_action(actions::ToggleOverview);
    let s = state(&view, cx);
    assert_eq!(s.preset, "overview");
    assert_eq!(s.docks.left.panels, ids(&["sessions", "tasks"]));
    assert!(
        s.active_pane()
            .unwrap()
            .items
            .contains(&ItemRef::panel("agents-dashboard"))
    );
    cx.dispatch_action(actions::ToggleOverview);
    let s = state(&view, cx);
    assert_eq!(s.preset, "default");
    assert_eq!(s.docks.left.panels, ids(&["sessions", "tasks"]));
}

#[gpui::test]
fn the_palette_lists_the_new_panels_and_the_sidebar_toggles(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.commands().iter().map(|c| c.title.to_string()).collect()
    });
    for wanted in [
        "Panel: Open Requests",
        "Panel: Open Changes",
        "Panel: Open History",
        "Panel: Open Tasks",
        "Panel: Open Resources",
        "View: Toggle Left Dock",
        "View: Toggle Right Dock",
    ] {
        assert!(titles.iter().any(|t| t == wanted), "{wanted} in {titles:?}");
    }
    assert!(!titles.iter().any(|t| t.contains("Inspector")));
    assert!(
        !titles.iter().any(|t| t == "Panel: Open Assistant"),
        "off by default"
    );

    // Opening Resources by its command lands in the right sidebar's strip now.
    cx.dispatch_action(OpenPanel("resources".into()));
    let s = state(&view, cx);
    assert_eq!(
        s.find_panel(&PanelId::new("resources")).map(|_| ()),
        Some(())
    );
    assert_eq!(s.docks.right.active, Some(PanelId::new("resources")));
    // A panel that is not open goes where its registry says: Tasks closed, then opened.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| s.close_panel(&PanelId::new("tasks")).unwrap());
    });
    cx.dispatch_action(OpenPanel("tasks".into()));
    assert_eq!(
        state(&view, cx).docks.left.active,
        Some(PanelId::new("tasks"))
    );
}

// ---- migration ---------------------------------------------------------------------------------

/// What a Hutan from before the Orca-style layout saved: Projects and History on the left,
/// Inspector / Agents / Tasks and a stray Assistant on the right, Resources in the bottom dock,
/// the old widths, two center tabs.
fn version_1_file(dir: &std::path::Path) -> PathBuf {
    let mut state = WorkbenchState::default();
    state.docks = Docks {
        left: Dock {
            panels: ids(&["sessions", "history"]),
            active: Some(PanelId::new("history")),
            size: 240.0,
            visible: true,
            collapsed: false,
            more: Vec::new(),
        },
        right: Dock {
            panels: ids(&["inspector", "agents", "tasks", "assistant"]),
            active: Some(PanelId::new("assistant")),
            size: 300.0,
            visible: true,
            collapsed: false,
            more: Vec::new(),
        },
        bottom: Dock::new(&["resources"], 200.0, false),
    };
    let pane = state.first_pane_id();
    state.add_item(pane, ItemRef::placeholder("Notes")).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&state.to_json().unwrap()).unwrap();
    value["version"] = 1.into();
    let path = dir.join(STATE_FILE);
    std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    path
}

#[gpui::test]
fn an_old_saved_layout_is_migrated_once_and_keeps_the_center(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = version_1_file(dir.path());
    let (view, _settings, cx) = open_in(cx, None, Some(path.clone()));
    let s = state(&view, cx);

    // History on the right and active; Assistant out; Tasks on the left behind Projects.
    assert_eq!(s.docks.left.panels, ids(&["sessions", "tasks"]));
    assert_eq!(s.docks.left.active, Some(PanelId::new("sessions")));
    assert_eq!(
        s.docks.right.panels,
        ids(&["history", "changes", "inspector", "agents", "resources"])
    );
    assert_eq!(s.docks.right.active, Some(PanelId::new("history")));
    assert!(s.find_panel(&PanelId::new("assistant")).is_none());
    assert!(s.docks.bottom.panels.is_empty());
    // Sizes the user never changed follow the new defaults; the center is theirs.
    assert_eq!((s.docks.left.size, s.docks.right.size), (280.0, 350.0));
    assert_eq!(
        s.active_pane().unwrap().items,
        vec![
            ItemRef::placeholder("Welcome"),
            ItemRef::placeholder("Notes")
        ]
    );

    // The next save is version 8, and loading that again changes nothing.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| s.toggle_dock(DockPosition::Bottom))
    });
    cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
    cx.run_until_parked();
    view.update(cx, |wb, _| wb.flush_save());
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(on_disk["version"], 8);
    let reloaded = load_state(&path);
    assert_eq!(reloaded.docks, state(&view, cx).docks);
    assert_eq!(reloaded.center, state(&view, cx).center);
}

#[gpui::test]
fn a_migrated_layout_keeps_what_the_user_changed(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = version_1_file(dir.path());
    // The user had widened the left dock and shown the bottom dock with Resources in it.
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    value["docks"]["left"]["size"] = 333.0.into();
    value["docks"]["bottom"]["visible"] = true.into();
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

    let (view, _settings, cx) = open_in(cx, None, Some(path));
    let s = state(&view, cx);
    assert_eq!(s.docks.left.size, 333.0);
    assert_eq!(s.docks.right.size, 350.0, "untouched, so the new default");
    assert_eq!(s.docks.bottom.panels, ids(&["resources"]));
    assert!(s.docks.bottom.visible);
    assert!(
        s.docks
            .right
            .panels
            .iter()
            .all(|p| p.as_str() != "resources"),
        "a panel is placed once"
    );
}

// ---- settings ----------------------------------------------------------------------------------

#[test]
fn mawas_is_on_unless_the_user_turned_it_off() {
    use hutan_desktop::settings::{AssistantSettings, register_all};
    use hutan_settings::SettingsPaths;

    let enabled = |user_file: Option<&str>| -> bool {
        let dir = tempfile::tempdir().unwrap();
        let paths = SettingsPaths::in_dir(dir.path());
        if let Some(text) = user_file {
            std::fs::write(&paths.user, text).unwrap();
        }
        let store = SettingsStore::new(paths);
        register_all(&store);
        store.get::<AssistantSettings>().enabled
    };

    // A new settings file, and one that never names the key: Mawas is on.
    assert!(enabled(None));
    assert!(enabled(Some("[terminal]\nfont_size = 14\n")));
    // An explicit value (either way) is theirs.
    assert!(!enabled(Some("[assistant]\nenabled = false\n")));
    assert!(enabled(Some("[assistant]\nenabled = true\n")));
    assert!(AssistantSettings::default().enabled);
}

#[gpui::test]
fn the_very_first_launch_opens_with_the_right_sidebar_on_its_rail(cx: &mut TestAppContext) {
    // No layout file yet: the right sidebar (History first) waits on its rail.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    let (view, _settings, cx) = open_in(cx, None, Some(path.clone()));
    let docks = state(&view, cx).docks;
    assert_eq!(
        docks.right.mode(),
        hutan_desktop::workbench::model::DockMode::Compact
    );
    assert_eq!(docks.right.active, Some(PanelId::new("changes")));
    assert!(
        docks.left.is_expanded(),
        "the left sidebar is as it always was"
    );
    // ⌘L opens the body on the first tab.
    cx.simulate_keystrokes("cmd-l");
    assert!(state(&view, cx).docks.right.is_expanded());
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("changes"))
    );
}

#[gpui::test]
fn a_saved_layout_keeps_its_right_sidebar_open(cx: &mut TestAppContext) {
    // A layout file that exists is the user's: nothing about it changes.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    std::fs::write(&path, WorkbenchState::default().to_json().unwrap()).unwrap();
    let (view, _settings, cx) = open_in(cx, None, Some(path));
    assert!(state(&view, cx).docks.right.is_expanded());
}
