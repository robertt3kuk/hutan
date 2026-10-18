#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! MCP servers in Settings → Agents & tools (R4-8): the list with Edit and Remove, the "Add
//! server…" form, the daemon's refusal inside the form, the confirmation before a removal, and
//! the restart banner. The daemon is in-process on a temporary home; its `daemon.toml` is the
//! file the assertions read.

mod common;

use std::path::Path;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::{Value, json};

const START: &str = "# my servers\n[mcp.keep]\ncommand = \"keep\"  # stays\n";

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let store = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let (view, cx) = open_with(cx, store, daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    cx.dispatch_action(actions::OpenSettingsFor("agents_tools".into()));
    draw(cx);
    wait_until(cx, "the daemon settings", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_settings().settings().is_some())
    });
    (view, cx)
}

fn daemon_toml(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("daemon.toml")).unwrap_or_default()
}

fn value(view: &Entity<Workbench>, cx: &VisualTestContext, key: &str) -> Value {
    view.read_with(cx, |wb, _| {
        wb.daemon_settings()
            .value(key)
            .cloned()
            .unwrap_or(Value::Null)
    })
}

fn wait_value(view: &Entity<Workbench>, cx: &mut VisualTestContext, key: &str, want: Value) {
    wait_until(cx, &format!("{key} to be {want}"), |cx| {
        value(view, cx, key) == want
    });
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

/// Scrolls the page of the sheet until `region` is well inside it: the page is taller than the
/// sheet, and a click only reaches what is in view (a click outside the sheet closes it).
fn reveal(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    // The sheet is 860 × 600 px, 48 px from the top and centred; its footer takes the bottom
    // 110 px. The page scrolls inside it, so the band where a click lands on the page is:
    let size = cx.update(|window, _| window.viewport_size());
    let at = gpui::point(size.width / 2.0, gpui::px(300.0));
    let (band_top, band_bottom) = (gpui::px(150.0), gpui::px(470.0));
    for _ in 0..200 {
        let bounds = view
            .read_with(cx, |wb, _| wb.region_bounds(region))
            .unwrap_or_else(|| panic!("{region:?} is drawn"));
        let delta = if bounds.top() < band_top {
            40.0
        } else if bounds.bottom() > band_bottom {
            -40.0
        } else {
            return;
        };
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: at,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(delta))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        draw(cx);
    }
    panic!("{region:?} never came into view");
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    reveal(view, cx, region);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

/// Types `text` into the box that is being typed in and presses Return.
fn type_and_return(view: &Entity<Workbench>, cx: &mut VisualTestContext, text: &str) {
    view.update(cx, |wb, cx| wb.sheet_set_edit_text(text, cx));
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
}

fn form_open(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.mcp_form().is_some())
}

fn banner(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> bool {
    drawn(view, cx, Region::Settings("restart-now"))
}

#[gpui::test]
fn mcp_servers_are_added_edited_and_removed_on_the_agents_and_tools_page(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start_with_config(START);
    let (view, cx) = open(cx, &daemon);
    let initial = daemon_toml(daemon.home());
    assert!(initial.starts_with(START));

    // The server of the file is listed with Edit and Remove…, and there is a way to add one.
    assert!(drawn(&view, cx, Region::Settings("mcp-add")));
    assert!(drawn(&view, cx, Region::SettingsItem("mcp-edit", 0)));
    assert!(drawn(&view, cx, Region::SettingsItem("mcp-remove", 0)));
    assert!(!banner(&view, cx));
    // The keyboard reaches the servers and the Add row.
    let rows = view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx));
    assert!(rows.contains(&"d:mcp.keep".to_owned()), "{rows:?}");
    assert!(rows.contains(&"d:mcp.*".to_owned()), "{rows:?}");

    // ---- Add a program with arguments ----
    click(&view, cx, Region::Settings("mcp-add"));
    assert!(form_open(&view, cx));
    assert!(
        !drawn(&view, cx, Region::Settings("mcp-add")),
        "the form replaces it"
    );
    // The name box is ready; Return moves on to the program, then to the arguments.
    type_and_return(&view, cx, "docs");
    type_and_return(&view, cx, "npx");
    view.update(cx, |wb, cx| {
        wb.sheet_set_edit_text("-y \"docs server\"", cx)
    });
    // Save keeps what is typed in the box that is open.
    click(&view, cx, Region::Settings("mcp-save"));
    wait_value(
        &view,
        cx,
        "mcp.docs",
        json!({"command": "npx", "args": ["-y", "docs server"]}),
    );
    assert!(!form_open(&view, cx), "a saved server closes the form");
    let text = daemon_toml(daemon.home());
    assert!(
        text.starts_with(START),
        "everything before is kept:\n{text}"
    );
    assert!(text.contains("[mcp.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs server\"]\n"));
    // Like every setting that needs a restart: the banner, and the server says so.
    assert!(banner(&view, cx), "the restart banner");
    view.read_with(cx, |wb, _| {
        let docs = wb.daemon_settings().entry("mcp.docs").unwrap();
        assert!(docs.pending_restart && docs.set_in_file);
    });

    // ---- Edit it into a remote server ----
    // The list is in the order of the daemon's: docs, keep.
    click(&view, cx, Region::SettingsItem("mcp-edit", 0));
    view.read_with(cx, |wb, _| {
        let form = wb.mcp_form().expect("the form of docs");
        assert_eq!(form.editing.as_deref(), Some("docs"));
        assert_eq!(
            (form.command.as_str(), form.args.as_str()),
            ("npx", "-y \"docs server\"")
        );
        assert!(!form.remote);
    });
    click(&view, cx, Region::Settings("mcp-kind-remote"));
    click(&view, cx, Region::Settings("mcp-transport-sse"));
    click(&view, cx, Region::SettingsItem("mcp-field", 0));
    view.update(cx, |wb, cx| {
        wb.sheet_set_edit_text("https://mcp.example.com/mcp", cx);
    });
    click(&view, cx, Region::Settings("mcp-save"));
    wait_value(
        &view,
        cx,
        "mcp.docs",
        json!({"url": "https://mcp.example.com/mcp", "transport": "sse"}),
    );
    assert!(!form_open(&view, cx));
    let text = daemon_toml(daemon.home());
    assert!(
        !text.contains("npx"),
        "the program's keys are gone:\n{text}"
    );
    assert!(text.contains("# stays"), "comments are kept:\n{text}");

    // ---- What is missing stays in the form; the daemon's refusal too ----
    click(&view, cx, Region::Settings("mcp-add"));
    click(&view, cx, Region::Settings("mcp-save"));
    view.read_with(cx, |wb, _| {
        let error = wb.mcp_form().unwrap().error.clone().unwrap();
        assert!(error.contains("name"), "{error}");
    });
    let before = daemon_toml(daemon.home());
    click(&view, cx, Region::SettingsItem("mcp-field", 0));
    type_and_return(&view, cx, "Bad Name");
    type_and_return(&view, cx, "tool");
    click(&view, cx, Region::Settings("mcp-save"));
    wait_until(cx, "the daemon's refusal", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_settings().error("mcp.Bad Name").is_some()
        })
    });
    view.read_with(cx, |wb, _| {
        let refusal = wb.daemon_settings().error("mcp.Bad Name").unwrap();
        assert!(refusal.contains("name"), "{refusal}");
    });
    assert!(
        form_open(&view, cx),
        "the form stays so the name can be fixed"
    );
    assert_eq!(daemon_toml(daemon.home()), before, "nothing was written");
    // Cancel: the form and its error are gone.
    click(&view, cx, Region::Settings("mcp-cancel"));
    assert!(!form_open(&view, cx));
    assert!(view.read_with(cx, |wb, _| {
        wb.daemon_settings().error("mcp.Bad Name").is_none()
    }));
    assert!(drawn(&view, cx, Region::Settings("mcp-add")));

    // ---- Remove asks first ----
    click(&view, cx, Region::SettingsItem("mcp-remove", 0));
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));
    let title = view.read_with(cx, |wb, _| wb.modal().unwrap().title.to_string());
    assert!(title.contains("docs"), "{title}");
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
    assert_ne!(value(&view, cx, "mcp.docs"), Value::Null, "Cancel keeps it");
    assert!(daemon_toml(daemon.home()).contains("[mcp.docs]"));

    click(&view, cx, Region::SettingsItem("mcp-remove", 0));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_value(&view, cx, "mcp.docs", Value::Null);
    let text = daemon_toml(daemon.home());
    assert_eq!(text, initial, "back to the file it was, comments and all");
    assert_eq!(
        value(&view, cx, "mcp.keep"),
        json!({"command": "keep"}),
        "the other server is untouched"
    );
    // The server never ran, so removing it leaves nothing pending.
    assert!(!banner(&view, cx));
}

#[gpui::test]
fn the_page_offers_add_server_when_there_is_none_and_the_keyboard_opens_the_form(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let before = daemon_toml(daemon.home());
    assert!(
        drawn(&view, cx, Region::Settings("mcp-add")),
        "an empty list still has Add"
    );
    assert!(!drawn(&view, cx, Region::SettingsItem("mcp-edit", 0)));

    // Tab to the controls, down to the Add row, Return.
    view.update(cx, |wb, cx| {
        wb.sheet_tab(false, cx);
        wb.sheet_tab(false, cx);
    });
    let rows = view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx));
    for _ in 0..rows.len() {
        if view.read_with(cx, |wb, _| wb.sheet_focused_row() == Some("d:mcp.*")) {
            break;
        }
        view.update(cx, |wb, cx| wb.sheet_arrow(1, cx));
    }
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_focused_row().map(str::to_owned)),
        Some("d:mcp.*".to_owned())
    );
    view.update_in(cx, |wb, window, cx| wb.sheet_activate(window, cx));
    assert!(form_open(&view, cx));

    // Escape leaves the box first; the form stays until Cancel.
    cx.simulate_keystrokes("escape");
    assert!(form_open(&view, cx));
    click(&view, cx, Region::Settings("mcp-cancel"));
    assert!(!form_open(&view, cx));
    assert_eq!(daemon_toml(daemon.home()), before, "nothing was written");
}
