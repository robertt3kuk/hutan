#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests of the default keymaps (`keymap.base`): Zed's keys toggle the docks (also while a
//! terminal has focus), the status bar's dock buttons do the same, ⌘K chords split, the palette
//! shows the active base's keys, and switching the base rebinds live.

use crate::common;

use std::time::Duration;

use common::{TestDaemon, offline_connection, open_with, wait_until};
use gpui::{Entity, Focusable as _, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::settings::KeymapBase;
use hutan_desktop::workbench::actions::{self, default_bindings};
use hutan_desktop::workbench::model::{
    DockMode, DockPosition, FocusTarget, PanelId, WorkbenchState,
};
use hutan_desktop::workbench::{PickerKind, Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

/// A workbench without a daemon: layout behavior only.
fn open(cx: &mut TestAppContext) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    open_with(cx, settings, connection, None)
}

/// Like [`open`] under a preset whose status bar names the dock buttons (the built-in presets
/// no longer show them, CL-CHROME): the docks of the default, the status bar of before.
fn open_with_dock_buttons(cx: &mut TestAppContext) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    let preset: toml::Value = toml::from_str(
        r#"
left = { panels = ["sessions", "tasks"], size = 280, visible = true }
right = { panels = ["changes", "agents", "history", "inspector", "resources"], size = 350, visible = true, more = ["inspector", "resources"] }
bottom = { panels = [], size = 200, visible = false }
status_bar = { left = ["connection", "attention"], right = ["palette_hint", "dock_left", "dock_bottom", "dock_right"] }
"#,
    )
    .unwrap();
    settings.write_user("layout.presets.docks", preset).unwrap();
    settings
        .write_user("layout.preset", toml::Value::String("docks".into()))
        .unwrap();
    open_with(cx, settings, connection, None)
}

fn state(view: &Entity<Workbench>, cx: &VisualTestContext) -> WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

fn visible(view: &Entity<Workbench>, cx: &VisualTestContext) -> [bool; 3] {
    let docks = state(view, cx).docks;
    [
        docks.left.is_expanded(),
        docks.right.is_expanded(),
        docks.bottom.is_expanded(),
    ]
}

/// The default bottom dock is empty (Resources moved to the right sidebar's strip), and a hidden
/// dock without panels has an inert button: give it a panel, hidden.
fn give_the_bottom_dock_a_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.open_panel(&PanelId::new("notifications"), DockPosition::Bottom);
            s.set_dock_mode(DockPosition::Bottom, DockMode::Hidden);
        });
    });
}

fn set_base(view: &Entity<Workbench>, cx: &mut VisualTestContext, base: &str) {
    view.read_with(cx, |wb, _| {
        wb.settings_store()
            .write_user("keymap.base", toml::Value::String(base.into()))
    })
    .unwrap();
    cx.run_until_parked();
}

fn hint(cx: &mut VisualTestContext, action: &dyn gpui::Action) -> Option<String> {
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
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: Region) {
    wait_until(cx, &format!("{what:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(what)).is_some()
    });
    let bounds = view.read_with(cx, |wb, _| wb.region_bounds(what)).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
}

#[gpui::test]
fn every_default_binding_names_an_action_and_a_context_gpui_understands(cx: &mut TestAppContext) {
    for base in KeymapBase::ALL {
        cx.update(|cx| {
            for spec in default_bindings(base) {
                cx.build_action(&spec.action, None)
                    .unwrap_or_else(|e| panic!("{base:?}: `{}`: {e}", spec.action));
                if let Some(context) = &spec.context {
                    gpui::KeyBindingContextPredicate::parse(context)
                        .unwrap_or_else(|e| panic!("{base:?}: context `{context}`: {e}"));
                }
            }
        });
    }
}

#[gpui::test]
fn zed_is_the_default_base_with_its_palette_jump_and_dock_keys(cx: &mut TestAppContext) {
    let (_view, cx) = open(cx);
    assert_eq!(
        hint(cx, &actions::TogglePalette).as_deref(),
        Some("⌘⇧P"),
        "the palette hint is the Zed key, ⌘K stays an alias"
    );
    assert_eq!(hint(cx, &actions::OpenJump).as_deref(), Some("⌘P"));
    assert_eq!(hint(cx, &actions::ToggleLeftDock).as_deref(), Some("⌘B"));
    assert_eq!(hint(cx, &actions::ToggleRightDock).as_deref(), Some("⌘L"));
    assert_eq!(hint(cx, &actions::ToggleBottomDock).as_deref(), Some("⌘J"));
    assert_eq!(hint(cx, &actions::CloseAllDocks).as_deref(), Some("⌥⌘Y"));
}

#[gpui::test]
fn cmd_b_r_j_toggle_the_docks_both_ways_and_cmd_alt_y_closes_them_all(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    // Default layout: left and right showing, bottom hidden.
    assert_eq!(visible(&view, cx), [true, true, false]);

    cx.simulate_keystrokes("cmd-b");
    assert_eq!(visible(&view, cx), [false, true, false]);
    cx.simulate_keystrokes("cmd-b");
    assert_eq!(visible(&view, cx), [true, true, false]);

    cx.simulate_keystrokes("cmd-r");
    assert_eq!(visible(&view, cx), [true, false, false]);
    cx.simulate_keystrokes("cmd-r");
    assert_eq!(visible(&view, cx), [true, true, false]);

    // ⌘L is the right sidebar's key now (Orca's); ⌘R stays as the Zed key.
    cx.simulate_keystrokes("cmd-l");
    assert_eq!(visible(&view, cx), [true, false, false]);
    cx.simulate_keystrokes("cmd-l");
    assert_eq!(visible(&view, cx), [true, true, false]);

    cx.simulate_keystrokes("cmd-j");
    assert_eq!(visible(&view, cx), [true, true, true]);
    cx.simulate_keystrokes("cmd-j");
    assert_eq!(visible(&view, cx), [true, true, false]);

    // Zed's ⌃` also toggles the bottom dock; ⌘⌥B the right one.
    cx.simulate_keystrokes("ctrl-`");
    assert_eq!(visible(&view, cx), [true, true, true]);
    cx.simulate_keystrokes("ctrl-`");
    cx.simulate_keystrokes("cmd-alt-b");
    assert_eq!(visible(&view, cx), [true, false, false]);
    cx.simulate_keystrokes("cmd-alt-b");

    cx.simulate_keystrokes("cmd-j");
    cx.simulate_keystrokes("cmd-alt-y");
    assert_eq!(visible(&view, cx), [false, false, false]);
    // Nothing left to close: the key is harmless.
    cx.simulate_keystrokes("cmd-alt-y");
    assert_eq!(visible(&view, cx), [false, false, false]);
}

#[gpui::test]
fn dock_keys_work_while_a_terminal_has_focus(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo focus-test; sleep 600");
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let (view, cx) = open_with(cx, settings, daemon.connection(), None);
    wait_until(cx, "the session", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == 1
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    wait_until(cx, "the terminal to take focus", |cx| {
        let focus = view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id).map(|v| v.focus_handle(cx))
        });
        focus.is_some_and(|f| cx.update(|window, _| f.is_focused(window)))
    });

    for (key, index) in [("cmd-b", 0), ("cmd-l", 1), ("cmd-r", 1), ("cmd-j", 2)] {
        let before = visible(&view, cx)[index];
        cx.simulate_keystrokes(key);
        assert_eq!(visible(&view, cx)[index], !before, "{key} (first press)");
        cx.simulate_keystrokes(key);
        assert_eq!(visible(&view, cx)[index], before, "{key} (second press)");
    }
}

#[gpui::test]
fn the_status_bar_dock_buttons_toggle_the_docks(cx: &mut TestAppContext) {
    let (view, cx) = open_with_dock_buttons(cx);
    give_the_bottom_dock_a_panel(&view, cx);
    assert_eq!(visible(&view, cx), [true, true, false]);

    click(&view, cx, Region::StatusItem("dock_left"));
    assert_eq!(visible(&view, cx), [false, true, false]);
    click(&view, cx, Region::StatusItem("dock_left"));
    assert_eq!(visible(&view, cx), [true, true, false]);

    click(&view, cx, Region::StatusItem("dock_bottom"));
    assert_eq!(visible(&view, cx), [true, true, true]);
    click(&view, cx, Region::StatusItem("dock_bottom"));
    assert_eq!(visible(&view, cx), [true, true, false]);

    click(&view, cx, Region::StatusItem("dock_right"));
    assert_eq!(visible(&view, cx), [true, false, false]);
    click(&view, cx, Region::StatusItem("dock_right"));
    assert_eq!(visible(&view, cx), [true, true, false]);

    // Left to right: left dock, bottom dock, right dock, at the right end of the bar.
    let x = |cx: &VisualTestContext, id: &'static str| {
        view.read_with(cx, |wb, _| wb.region_bounds(Region::StatusItem(id)))
            .unwrap()
            .origin
            .x
    };
    assert!(x(cx, "dock_left") < x(cx, "dock_bottom"));
    assert!(x(cx, "dock_bottom") < x(cx, "dock_right"));
    assert!(x(cx, "palette_hint") < x(cx, "dock_left"));
}

#[gpui::test]
fn a_hidden_dock_without_panels_has_a_disabled_button(cx: &mut TestAppContext) {
    let (view, cx) = open_with_dock_buttons(cx);
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let bottom = s.docks.get_mut(DockPosition::Bottom);
            bottom.panels.clear();
            bottom.active = None;
            bottom.visible = false;
        });
    });
    click(&view, cx, Region::StatusItem("dock_bottom"));
    assert_eq!(
        visible(&view, cx),
        [true, true, false],
        "the click is inert"
    );

    // The key still opens the (empty) dock, and then the button can close it again.
    cx.simulate_keystrokes("cmd-j");
    assert_eq!(visible(&view, cx), [true, true, true]);
    click(&view, cx, Region::StatusItem("dock_bottom"));
    assert_eq!(visible(&view, cx), [true, true, false]);
}

#[gpui::test]
fn switching_the_keymap_base_rebinds_live(cx: &mut TestAppContext) {
    let (view, cx) = open_with_dock_buttons(cx);
    // Zed: ⌘J is the bottom dock, ⌘P jumps.
    cx.simulate_keystrokes("cmd-j");
    assert_eq!(visible(&view, cx), [true, true, true]);
    cx.simulate_keystrokes("cmd-j");
    assert_eq!(visible(&view, cx), [true, true, false]);
    cx.simulate_keystrokes("cmd-p");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.picker().map(|p| p.kind)),
        Some(PickerKind::Jump)
    );
    cx.simulate_keystrokes("escape");

    set_base(&view, cx, "hutan");
    assert_eq!(hint(cx, &actions::TogglePalette).as_deref(), Some("⌘K"));
    assert_eq!(hint(cx, &actions::ToggleBottomDock).as_deref(), Some("⌥⌘J"));
    // Hutan: ⌘J jumps, ⌘⌥J is the bottom dock, ⌘P and ⌘R are free again.
    cx.simulate_keystrokes("cmd-j");
    assert_eq!(
        visible(&view, cx),
        [true, true, false],
        "⌘J no longer toggles"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.picker().map(|p| p.kind)),
        Some(PickerKind::Jump)
    );
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("cmd-alt-j");
    assert_eq!(visible(&view, cx), [true, true, true]);
    cx.simulate_keystrokes("cmd-alt-j");
    cx.simulate_keystrokes("cmd-r");
    assert_eq!(visible(&view, cx), [true, true, false], "⌘R is unbound");
    // ⌘L toggles the right sidebar in both bases.
    assert_eq!(hint(cx, &actions::ToggleRightDock).as_deref(), Some("⌘L"));
    cx.simulate_keystrokes("cmd-l");
    assert_eq!(visible(&view, cx), [true, false, false]);
    cx.simulate_keystrokes("cmd-l");
    assert_eq!(visible(&view, cx), [true, true, false]);
    cx.simulate_keystrokes("cmd-k");
    assert!(view.read_with(cx, |wb, _| wb.palette_open()));
    cx.simulate_keystrokes("escape");

    // The status bar tooltips follow too: the dock buttons still work.
    give_the_bottom_dock_a_panel(&view, cx);
    click(&view, cx, Region::StatusItem("dock_bottom"));
    assert_eq!(visible(&view, cx), [true, true, true]);
    click(&view, cx, Region::StatusItem("dock_bottom"));

    set_base(&view, cx, "zed");
    assert_eq!(hint(cx, &actions::TogglePalette).as_deref(), Some("⌘⇧P"));
    cx.simulate_keystrokes("cmd-j");
    assert_eq!(visible(&view, cx), [true, true, true]);
    cx.simulate_keystrokes("cmd-j");
    cx.simulate_keystrokes("cmd-shift-p");
    assert!(view.read_with(cx, |wb, _| wb.palette_open()));
}

#[gpui::test]
fn a_user_keymap_merges_on_top_of_the_chosen_base(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("keymap.toml"),
        "[[binding]]\nkeys = \"cmd-r\"\naction = \"none\"\n\n\
         [[binding]]\nkeys = \"cmd-shift-r\"\naction = \"workbench::ToggleRightDock\"\n",
    )
    .unwrap();
    let settings = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
    let (connection, _, _) = offline_connection();
    let (view, cx) = open_with(cx, settings, connection, None);
    // ⌘R is unbound by the user, ⌘⇧R added; the rest of the Zed base is untouched.
    cx.simulate_keystrokes("cmd-r");
    assert_eq!(visible(&view, cx), [true, true, false]);
    cx.simulate_keystrokes("cmd-shift-r");
    assert_eq!(visible(&view, cx), [true, false, false]);
    cx.simulate_keystrokes("cmd-b");
    assert_eq!(visible(&view, cx), [false, false, false]);
    assert_eq!(hint(cx, &actions::ToggleRightDock).as_deref(), Some("⌘⇧R"));
}

#[gpui::test]
fn cmd_k_arrows_split_and_cmd_k_cmd_arrows_focus_the_neighbor(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    let pane = |cx: &VisualTestContext| match state(&view, cx).focus {
        FocusTarget::Pane(id) => id,
        other => panic!("a pane should have focus, got {other:?}"),
    };
    let bounds = |cx: &mut VisualTestContext, id| {
        common::draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Pane(id)))
            .expect("the pane is drawn")
    };
    let first = pane(cx);

    cx.simulate_keystrokes("cmd-k left");
    assert_eq!(state(&view, cx).panes().len(), 2);
    let left = pane(cx);
    assert_ne!(left, first, "the new pane takes focus");
    assert!(bounds(cx, left).origin.x < bounds(cx, first).origin.x);

    // ⌘K ⌘→ focuses the neighbor on the right; it does not split.
    cx.simulate_keystrokes("cmd-k cmd-right");
    assert_eq!(state(&view, cx).panes().len(), 2);
    assert_eq!(pane(cx), first);
    cx.simulate_keystrokes("cmd-k cmd-left");
    assert_eq!(pane(cx), left);
    // ⌘⌥→ is the same action.
    cx.simulate_keystrokes("cmd-alt-right");
    assert_eq!(pane(cx), first);

    cx.simulate_keystrokes("cmd-k right");
    assert_eq!(state(&view, cx).panes().len(), 3);
    let right = pane(cx);
    assert!(bounds(cx, right).origin.x > bounds(cx, first).origin.x);

    cx.simulate_keystrokes("cmd-k up");
    assert_eq!(state(&view, cx).panes().len(), 4);
    let up = pane(cx);
    assert!(bounds(cx, up).origin.y < bounds(cx, right).origin.y);

    cx.simulate_keystrokes("cmd-k down");
    assert_eq!(state(&view, cx).panes().len(), 5);
    let down = pane(cx);
    assert!(bounds(cx, down).origin.y > bounds(cx, up).origin.y);
    cx.simulate_keystrokes("cmd-k cmd-up");
    assert_eq!(pane(cx), up);
    assert_eq!(state(&view, cx).panes().len(), 5);

    // ⌘D and ⌘⇧D keep splitting right and down.
    cx.simulate_keystrokes("cmd-d");
    assert_eq!(state(&view, cx).panes().len(), 6);
}

#[gpui::test]
fn cmd_k_alone_is_no_palette_key_in_the_zed_base(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-k");
    cx.executor().advance_clock(Duration::from_millis(1100));
    cx.run_until_parked();
    assert!(
        !view.read_with(cx, |wb, _| wb.palette_open()),
        "⌘K is only a chord prefix in the Zed base"
    );
    assert_eq!(
        hint(cx, &actions::FocusNeighborLeft).as_deref(),
        Some("⌘K ⌘←")
    );
    assert_eq!(hint(cx, &actions::SplitLeft).as_deref(), Some("⌘K ←"));
}

#[gpui::test]
fn palette_rows_show_the_keys_of_the_active_base(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    // The row hints are read from the live keymap, so they are what `hint` answers.
    assert_eq!(hint(cx, &actions::SplitRight).as_deref(), Some("⌘D"));
    assert_eq!(hint(cx, &actions::IncreaseFontSize).as_deref(), Some("⌘+"));
    assert_eq!(hint(cx, &actions::DecreaseFontSize).as_deref(), Some("⌘-"));
    assert_eq!(hint(cx, &actions::ResetFontSize).as_deref(), Some("⌘0"));
    let titles = |cx: &mut VisualTestContext| {
        cx.simulate_keystrokes("cmd-shift-p");
        let titles = view.read_with(cx, |wb, _| wb.palette_titles());
        cx.simulate_keystrokes("escape");
        titles
    };
    let zed = titles(cx);
    for wanted in [
        "View: Close All Docks",
        "View: Increase Terminal Font Size",
        "Layout: Split Left",
        "Layout: Split Up",
    ] {
        assert!(zed.iter().any(|t| t.as_ref() == wanted), "{wanted}");
    }
    set_base(&view, cx, "hutan");
    assert_eq!(hint(cx, &actions::SplitRight).as_deref(), Some("⌘D"));
    assert_eq!(hint(cx, &actions::IncreaseFontSize), None);
}

#[gpui::test]
fn font_size_keys_write_the_user_setting(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    let size = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| {
            wb.settings_store()
                .get::<hutan_desktop::settings::TerminalSettings>()
                .font_size
        })
    };
    assert_eq!(size(cx), 13.0);
    cx.simulate_keystrokes("cmd-=");
    cx.simulate_keystrokes("cmd-+");
    assert_eq!(size(cx), 15.0);
    cx.simulate_keystrokes("cmd--");
    assert_eq!(size(cx), 14.0);
    cx.simulate_keystrokes("cmd-0");
    assert_eq!(size(cx), 13.0);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size),
        13.0
    );
    cx.simulate_keystrokes("cmd-=");
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size),
        14.0,
        "open terminals follow at once"
    );
    // Without the Zed keys the font keys are free.
    set_base(&view, cx, "hutan");
    cx.simulate_keystrokes("cmd-=");
    assert_eq!(size(cx), 14.0);
}
