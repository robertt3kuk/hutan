#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The quiet chrome (CL-CHROME): the clean default preset of a new home and the one-time offer
//! for a layout saved before it (which is never rewritten), the rail's three icons with
//! Requests and Resources under ⋯, the focus shown by one tab's weight, and the title bar
//! without commands. No daemon: layout only.

use crate::common;

use common::{offline_connection, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::settings::LayoutSettings;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::{
    Dock, DockMode, DockPosition, Docks, Edge, FocusTarget, ItemRef, LayoutPreset, PaneId, PanelId,
    WorkbenchState,
};
use hutan_desktop::workbench::persistence::{STATE_FILE, load_state};
use hutan_desktop::workbench::{Region, SAVE_DEBOUNCE, Workbench};
use hutan_settings::SettingsStore;
use std::path::{Path, PathBuf};

fn open_at<'a>(
    cx: &'a mut TestAppContext,
    settings: Option<SettingsStore>,
    path: &Path,
) -> (Entity<Workbench>, SettingsStore, &'a mut VisualTestContext) {
    let (connection, fresh_settings, _) = offline_connection();
    let settings = settings.unwrap_or(fresh_settings);
    let (view, cx) = open_with(cx, settings.clone(), connection, Some(path.to_owned()));
    (view, settings, cx)
}

fn state(view: &Entity<Workbench>, cx: &VisualTestContext) -> WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

fn ids(panels: &[&str]) -> Vec<PanelId> {
    panels.iter().map(|p| PanelId::new(*p)).collect()
}

fn painted(view: &Entity<Workbench>, cx: &VisualTestContext, region: Region) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .is_some()
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        painted(view, cx, region)
    });
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap()
        .center();
    cx.simulate_click(at, Modifiers::none());
}

fn offered(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.layout_offer_shown())
}

fn save(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
    cx.run_until_parked();
    view.update(cx, |wb, _| wb.flush_save());
}

/// What a layout saved before CL-CHROME looks like on disk: version 6, the five icons on the
/// right rail (no `more`), the body collapsed to its rail at a width the user chose, a tab.
fn pre_clean_layout(dir: &Path) -> PathBuf {
    let mut state = WorkbenchState::default();
    state.docks = Docks {
        left: Dock::new(&["sessions", "tasks"], 300.0, true),
        right: Dock::new(
            &["history", "changes", "inspector", "agents", "resources"],
            400.0,
            true,
        ),
        bottom: Dock::new(&[], 200.0, false),
    };
    state.docks.right.collapsed = true;
    let pane = state.first_pane_id();
    state.add_item(pane, ItemRef::placeholder("Notes")).unwrap();
    let path = dir.join(STATE_FILE);
    std::fs::write(&path, state.to_json().unwrap()).unwrap();
    path
}

fn on_disk(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

// ---- the clean default and the offer --------------------------------------------------------

/// The golden of the docks a new home saves (`tests/golden/clean-default-docks.json`); rewrite
/// it with `UPDATE_GOLDEN=1` after an intentional change of the default preset.
#[gpui::test]
fn a_new_home_gets_the_clean_default_and_saves_it_as_the_golden(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    let (view, _settings, cx) = open_at(cx, None, &path);
    let s = state(&view, cx);
    assert_eq!(s.preset, "default");
    assert_eq!(
        s.docks.right.panels,
        ids(&["changes", "agents", "history", "inspector", "resources"])
    );
    assert_eq!(s.docks.right.more, ids(&["inspector", "resources"]));
    assert_eq!(s.docks.right.active, Some(PanelId::new("changes")));
    assert_eq!(s.docks.right.mode(), DockMode::Compact, "the rail only");
    assert!(!offered(&view, cx), "a new home is not offered what it has");

    // The rail: three icons and ⋯, nothing for Requests or Resources.
    wait_until(cx, "the rail", |cx| {
        painted(&view, cx, Region::Rail(DockPosition::Right))
    });
    for index in 0..3 {
        assert!(painted(
            &view,
            cx,
            Region::DockTab(DockPosition::Right, index)
        ));
    }
    for index in 3..5 {
        assert!(!painted(
            &view,
            cx,
            Region::DockTab(DockPosition::Right, index)
        ));
    }
    assert!(painted(&view, cx, Region::StripMore));

    // A new home has no file until the first save writes the layout it starts with.
    view.update(cx, |wb, _| wb.flush_save());
    let docks = on_disk(&path)["docks"].clone();
    let golden =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/clean-default-docks.json");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        std::fs::write(
            &golden,
            serde_json::to_string_pretty(&docks).unwrap() + "\n",
        )
        .unwrap();
    }
    let want: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&golden).unwrap()).unwrap();
    assert_eq!(docks, want, "the saved docks of a new home");
}

#[gpui::test]
fn a_saved_layout_is_untouched_and_offered_the_new_default_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = pre_clean_layout(dir.path());
    let before = on_disk(&path);
    let settings = {
        let (view, settings, cx) = open_at(cx, None, &path);

        // Read as it is: every icon on the rail, the user's width and mode.
        let s = state(&view, cx);
        assert_eq!(
            s.docks.right.panels,
            ids(&["history", "changes", "inspector", "agents", "resources"])
        );
        assert!(s.docks.right.more.is_empty());
        assert_eq!(s.docks.right.size, 400.0);
        assert_eq!(s.docks.right.mode(), DockMode::Compact);
        assert!(offered(&view, cx));
        wait_until(cx, "the offer", |cx| {
            painted(&view, cx, Region::Offer("layout"))
        });
        for index in 0..5 {
            assert!(painted(
                &view,
                cx,
                Region::DockTab(DockPosition::Right, index)
            ));
        }

        // Saving writes the same docks back (version 7, no `more`).
        view.update(cx, |wb, cx| {
            wb.edit_layout(cx, |s| {
                let pane = s.first_pane_id();
                s.add_item(pane, ItemRef::placeholder("Scratch")).unwrap();
            })
        });
        save(&view, cx);
        let after = on_disk(&path);
        assert_eq!(after["version"], 7);
        assert_eq!(
            after["docks"], before["docks"],
            "the saved layout is untouched"
        );

        // Keep mine: nothing changes, and the offer is not made again.
        click(&view, cx, Region::Offer("layout-keep"));
        assert!(!offered(&view, cx));
        assert_eq!(state(&view, cx).docks, s.docks);
        assert!(!settings.get::<LayoutSettings>().offer_new_default);
        settings
    };
    let (again, _, cx) = open_at(cx, Some(settings), &path);
    assert!(!offered(&again, cx), "asked once");
}

#[gpui::test]
fn use_new_default_takes_the_panels_and_keeps_the_modes_sizes_and_center(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = pre_clean_layout(dir.path());
    let (view, settings, cx) = open_at(cx, None, &path);
    let before = state(&view, cx);
    click(&view, cx, Region::Offer("layout-use-new"));

    let s = state(&view, cx);
    assert!(!offered(&view, cx));
    assert!(!settings.get::<LayoutSettings>().offer_new_default);
    assert_eq!(
        s.docks.right.panels,
        LayoutPreset::default_preset().docks.right.panels
    );
    assert_eq!(s.docks.right.more, ids(&["inspector", "resources"]));
    assert_eq!(s.docks.left.panels, ids(&["sessions", "tasks", "files"]));
    // The user's modes and widths stay; so does the center.
    assert_eq!(s.docks.right.mode(), DockMode::Compact);
    assert_eq!((s.docks.left.size, s.docks.right.size), (300.0, 400.0));
    assert_eq!(s.center, before.center);

    // The next save carries `more`, and loading it again offers nothing.
    save(&view, cx);
    assert_eq!(
        on_disk(&path)["docks"]["right"]["more"],
        serde_json::json!(["inspector", "resources"])
    );
    assert_eq!(load_state(&path).docks, s.docks);

    // The View menu's command does it at any time, without an offer.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.docks.right.more.clear();
        })
    });
    cx.dispatch_action(actions::UseNewDefaultLayout);
    assert_eq!(
        state(&view, cx).docks.right.more,
        ids(&["inspector", "resources"])
    );
}

// ---- the rail ---------------------------------------------------------------------------------

#[gpui::test]
fn the_more_menu_lists_requests_and_resources_and_opens_them(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    let (view, _settings, cx) = open_at(cx, None, &path);
    click(&view, cx, Region::StripMore);
    let labels: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the More menu opened")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert_eq!(labels, ["Requests", "Resources"]);
    // Resources opens in the body and is an icon while it shows there.
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(1, window, cx));
    let s = state(&view, cx);
    assert_eq!(s.docks.right.active, Some(PanelId::new("resources")));
    assert!(s.docks.right.is_expanded());
    wait_until(cx, "the Resources icon", |cx| {
        painted(&view, cx, Region::DockTab(DockPosition::Right, 4))
    });
    assert!(!painted(&view, cx, Region::DockTab(DockPosition::Right, 3)));
    // The ⌘⌥R-style panel command opens a panel listed in ⋯ the same way.
    cx.dispatch_action(actions::OpenPanel("inspector".into()));
    assert_eq!(
        state(&view, cx).docks.right.active,
        Some(PanelId::new("inspector"))
    );
}

// ---- focus --------------------------------------------------------------------------------------

/// Focusing a pane changes only its tab's weight and color (and the other pane's): the active
/// tabs keep their connected fill, inactive tabs do not change, nothing is outlined.
#[gpui::test]
fn focusing_a_pane_changes_only_its_tabs_weight(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    let (view, _settings, cx) = open_at(cx, None, &path);
    let (left, right) = view.update(cx, |wb, cx| {
        let mut panes = (PaneId(0), PaneId(0));
        wb.edit_layout(cx, |s| {
            let left = s.first_pane_id();
            s.add_item(left, ItemRef::placeholder("Notes")).unwrap();
            let right = s
                .split_pane(left, Edge::Right, ItemRef::placeholder("Logs"))
                .unwrap();
            panes = (left, right);
        });
        panes
    });
    let looks = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| {
            [
                wb.tab_look(left, 0).unwrap(),
                wb.tab_look(left, 1).unwrap(),
                wb.tab_look(right, 0).unwrap(),
            ]
        })
    };
    // The split focused the new (right) pane; Notes is the left pane's active tab.
    assert_eq!(state(&view, cx).focus, FocusTarget::Pane(right));
    let right_focused = looks(cx);
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| s.focus_pane(left).unwrap())
    });
    let left_focused = looks(cx);

    let [welcome_a, notes_a, logs_a] = right_focused;
    let [welcome_b, notes_b, logs_b] = left_focused;
    // The inactive tab is the same either way: muted, regular, no fill.
    assert_eq!(welcome_a, welcome_b);
    assert_eq!(welcome_a.fill, None);
    // Both active tabs keep their connected `bg` fill.
    assert!(notes_a.fill.is_some() && notes_a.fill == logs_a.fill);
    assert_eq!((notes_a.fill, logs_a.fill), (notes_b.fill, logs_b.fill));
    // Only the weight and color move to the focused pane's active tab.
    assert_eq!(logs_a.weight, gpui::FontWeight::MEDIUM);
    assert_eq!(notes_a.weight, gpui::FontWeight::NORMAL);
    assert_eq!(notes_b.weight, gpui::FontWeight::MEDIUM);
    assert_eq!(logs_b.weight, gpui::FontWeight::NORMAL);
    assert_eq!(
        notes_a.text, welcome_a.text,
        "an unfocused active tab is muted"
    );
    assert_eq!((notes_b.text, logs_b.text), (logs_a.text, notes_a.text));
}

// ---- title bar -----------------------------------------------------------------------------------

#[gpui::test]
fn the_title_bar_holds_the_title_and_the_two_dock_buttons_only(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    let (view, _settings, cx) = open_at(cx, None, &path);
    wait_until(cx, "the title bar", |cx| {
        painted(&view, cx, Region::Titlebar)
    });
    assert!(painted(&view, cx, Region::DockToggle(DockPosition::Left)));
    assert!(painted(&view, cx, Region::DockToggle(DockPosition::Right)));
    // ⌘⇧O still reaches the Agents overview without a title bar button.
    cx.dispatch_action(actions::ToggleOverview);
    assert_eq!(state(&view, cx).preset, "overview");
}
