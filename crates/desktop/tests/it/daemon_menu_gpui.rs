#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The `Daemon` menu as AppKit sees it (FX-MENU-LOGIN, design.md § Daemon lifecycle): an item
//! is enabled when GPUI says its action is available (`App::is_action_available`, the call the
//! platform's menu validation makes), whatever holds the keyboard — a file tab, a terminal, a
//! chat, the sidebar, nothing, or no active window at all — and exactly when it can act. The
//! recovery items (and their palette commands) are there only while their failure is current.

use crate::common;

use std::path::Path;

use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Focusable as _, OwnedMenuItem, TestAppContext, VisualTestContext};
use hutan_client::{ConnectOptions, Connection, ConnectionState};
use hutan_desktop::workbench::actions::daemon as daemon_actions;
use hutan_desktop::workbench::chat::LoadStatus;
use hutan_desktop::workbench::model::DockPosition;
use hutan_desktop::workbench::{FileOpen, Workbench};
use hutan_protocol::messages::{ClientInfo, ClientKind};
use hutan_settings::{SettingsPaths, SettingsStore};

/// `(name, enabled)` of every action of the `Daemon` menu as the platform would validate it
/// now, against the last frame: the item's action asked of `App::is_action_available`,
/// outside any window update, as AppKit's `validateMenuItem:` does. Also checks that the
/// item's own `disabled` flag agrees.
fn validated(cx: &mut VisualTestContext) -> Vec<(String, bool)> {
    let menus = cx.update(|_, cx| cx.get_menus()).expect("menus are set");
    let menu = menus
        .into_iter()
        .find(|m| m.name.as_ref() == "Daemon")
        .expect("a Daemon menu");
    menu.items
        .into_iter()
        .filter_map(|item| match item {
            OwnedMenuItem::Action {
                name,
                action,
                disabled,
                ..
            } => {
                let available = cx.cx.update(|cx| cx.is_action_available(action.as_ref()));
                assert_eq!(
                    available, !disabled,
                    "{name}: validation and the flag agree"
                );
                Some((name, available))
            }
            _ => None,
        })
        .collect()
}

fn names(menu: &[(String, bool)]) -> Vec<&str> {
    menu.iter().map(|(name, _)| name.as_str()).collect()
}

/// The palette's `Daemon:` commands (empty query).
fn palette_daemon_commands(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<String> {
    view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
    let titles = view.read_with(cx, |wb, _| wb.palette_titles());
    view.update_in(cx, |wb, window, cx| wb.close_palette(window, cx));
    titles
        .into_iter()
        .map(|t| t.to_string())
        .filter(|t| t.starts_with("Daemon:"))
        .collect()
}

/// The menu of a running daemon outside Hutan.app: everything acts but Start, the login
/// item and Login Items.
const RUNNING: [(&str, bool); 8] = [
    ("Status…", true),
    ("Restart Daemon", true),
    ("Stop Daemon", true),
    ("Start Daemon", false),
    ("Open Logs", true),
    ("Keep Daemon Running After Hutan Quits", true),
    ("Start Daemon at Login (available in Hutan.app)", false),
    ("Open Login Items", false),
];

fn running() -> Vec<(String, bool)> {
    RUNNING
        .iter()
        .map(|(n, on)| ((*n).to_owned(), *on))
        .collect()
}

fn focused(cx: &mut VisualTestContext, handle: &gpui::FocusHandle) -> bool {
    cx.update(|window, cx| handle.contains_focused(window, cx))
}

#[gpui::test]
fn the_daemon_menu_acts_whatever_has_the_keyboard(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let chat = daemon.create_chat(&env.cwd());
    let shell = daemon.sh("sleep 600");
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == 2 && wb.active_workspace().is_some()
        })
    });

    // A file tab.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "hello\n").unwrap();
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&file), window, cx);
    });
    wait_until(cx, "the file tab to hold the keyboard", |cx| {
        let tab = view.read_with(cx, |wb, cx| {
            wb.focused_file_view().map(|v| v.read(cx).focus_handle(cx))
        });
        tab.is_some_and(|handle| focused(cx, &handle))
    });
    assert_eq!(validated(cx), running(), "focus in a file tab");

    // A terminal.
    view.update_in(cx, |wb, window, cx| wb.open_session(shell.id, window, cx));
    wait_until(cx, "the terminal to hold the keyboard", |cx| {
        let terminal = view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(shell.id)
                .map(|v| gpui::Focusable::focus_handle(v.read(cx), cx))
        });
        terminal.is_some_and(|handle| focused(cx, &handle))
    });
    assert_eq!(validated(cx), running(), "focus in a terminal");

    // A chat.
    view.update_in(cx, |wb, window, cx| wb.open_session(chat.id, window, cx));
    wait_until(cx, "the chat to hold the keyboard", |cx| {
        let chat_view = view.read_with(cx, |wb, cx| {
            wb.chat_view_of(chat.id).filter(|_| {
                wb.chat_model_of(chat.id)
                    .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
            })
        });
        chat_view
            .is_some_and(|v| cx.update(|window, cx| v.read(cx).focus_handle_is_focused(window)))
    });
    assert_eq!(validated(cx), running(), "focus in a chat");

    // The sidebar (the left dock).
    let sidebar = view.read_with(cx, |wb, _| wb.dock_focus_handle(DockPosition::Left));
    cx.update(|window, cx| window.focus(&sidebar, cx));
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| s.focus_dock(DockPosition::Left))
    });
    draw(cx);
    assert!(focused(cx, &sidebar), "the sidebar holds the keyboard");
    assert_eq!(validated(cx), running(), "focus in the sidebar");

    // Nothing focused in the window: the dispatch path is the window's root alone. (The
    // workbench gives the keyboard back to the focused pane at once, so focus is switched off
    // for the rest of the test.)
    cx.update(|window, cx| window.disable_focus(cx));
    draw(cx);
    assert!(cx.update(|window, cx| window.focused(cx).is_none()));
    assert_eq!(validated(cx), running(), "nothing focused");

    // No active window (the app's menu bar with its window in the background or minimised).
    cx.deactivate_window();
    assert!(cx.cx.update(|cx| cx.active_window()).is_none());
    assert_eq!(validated(cx), running(), "no active window");

    // And an item acts from there: Status… brings the window forward with the popover.
    cx.cx
        .update(|cx| cx.dispatch_action(&daemon_actions::Status));
    cx.run_until_parked();
    assert!(view.read_with(cx, |wb, _| wb.daemon_popover_open()));
    assert!(cx.update(|window, _| window.is_window_active()));
}

#[gpui::test]
fn the_daemon_menu_follows_what_can_act(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    assert_eq!(validated(cx), running());

    // Stop from a window without focus: the daemon goes, Start is the one that acts now.
    cx.update(|window, cx| window.blur(cx));
    cx.cx.update(|cx| cx.dispatch_action(&daemon_actions::Stop));
    wait_until(cx, "the stop", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.daemon_stopped()
                && wb.daemon_busy().is_none()
                && wb.sessions().read(cx).connection_state() != ConnectionState::Connected
        })
    });
    let menu = validated(cx);
    let on = |name: &str| menu.iter().find(|(n, _)| n == name).map(|(_, on)| *on);
    assert_eq!(on("Status…"), Some(true));
    assert_eq!(on("Restart Daemon"), Some(false));
    assert_eq!(on("Stop Daemon"), Some(false));
    assert_eq!(on("Start Daemon"), Some(true));
    assert_eq!(on("Open Logs"), Some(true));
}

// ---- recovery items --------------------------------------------------------------------------

const RECOVERY: [&str; 4] = [
    "Copy Update Command",
    "Start with Empty Data…",
    "Stop the Stuck Daemon…",
    "Copy Start Error Details",
];

const RECOVERY_COMMANDS: [&str; 6] = [
    "Daemon: Start with Empty Data…",
    "Daemon: Show the Stuck Daemon",
    "Daemon: Stop the Stuck Daemon…",
    "Daemon: Copy Start Error Details",
    "Daemon: Copy Update Command",
    "Daemon: Use This App's Service",
];

#[gpui::test]
fn a_healthy_daemon_has_no_recovery_items(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    let menu = validated(cx);
    for item in RECOVERY {
        assert!(!names(&menu).contains(&item), "{item} is not in the menu");
    }
    let commands = palette_daemon_commands(&view, cx);
    for command in RECOVERY_COMMANDS {
        assert!(
            !commands.iter().any(|c| c == command),
            "{command} is not in the palette"
        );
    }
    assert!(commands.iter().any(|c| c == "Daemon: Restart Daemon"));
}

/// A window whose daemon could not start, by the record a failed `hutand` leaves next to its
/// socket (`kind`: the record's kind and fields).
fn failed_start<'a>(
    cx: &'a mut TestAppContext,
    dir: &Path,
    kind: &str,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let socket = dir.join("hutand.sock");
    std::fs::write(
        dir.join("hutand-start-error.json"),
        format!(r#"{{"at_ms":1,"pid":1,"version":"0.2.0",{kind},"message":"it failed"}}"#),
    )
    .unwrap();
    let connection = Connection::start(ConnectOptions {
        socket: Some(socket),
        autostart: None,
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: "desktop-test".into(),
            origin: None,
        },
    });
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(&dir.join("config"))),
        connection,
        None,
    );
    wait_until(cx, "the recorded failure", |cx| {
        view.read_with(cx, |wb, cx| wb.daemon_start_failure(cx).is_some())
    });
    (view, cx)
}

/// What a failure of each kind lists: the menu's recovery items (all of them enabled), and
/// the palette's recovery commands.
fn recovery_listed(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
) -> (Vec<String>, Vec<String>) {
    let menu = validated(cx);
    let items = menu
        .iter()
        .filter(|(name, _)| RECOVERY.contains(&name.as_str()))
        .map(|(name, on)| {
            assert!(on, "{name} acts");
            name.clone()
        })
        .collect();
    // Not running: Start acts, Restart and Stop do not; the reason line never acts.
    let on = |name: &str| menu.iter().find(|(n, _)| n == name).map(|(_, on)| *on);
    assert_eq!(on("Start Daemon"), Some(true));
    assert_eq!(on("Restart Daemon"), Some(false));
    assert_eq!(on("Stop Daemon"), Some(false));
    assert!(
        menu.iter()
            .any(|(n, on)| n.starts_with("Not Running: ") && !on),
        "{menu:?}"
    );
    let commands = palette_daemon_commands(view, cx)
        .into_iter()
        .filter(|c| RECOVERY_COMMANDS.contains(&c.as_str()))
        .collect();
    (items, commands)
}

#[gpui::test]
fn data_from_a_newer_hutan_lists_update_and_empty_data(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("hutan.db");
    let (view, cx) = failed_start(
        cx,
        dir.path(),
        &format!(
            r#""kind":"schema_too_new","found":999,"supported":1,"db":"{}""#,
            db.display()
        ),
    );
    let (items, commands) = recovery_listed(&view, cx);
    assert_eq!(
        items,
        [
            "Copy Update Command",
            "Start with Empty Data…",
            "Copy Start Error Details"
        ]
    );
    assert_eq!(
        commands,
        [
            "Daemon: Start with Empty Data…",
            "Daemon: Copy Start Error Details",
            "Daemon: Copy Update Command"
        ]
    );
}

#[gpui::test]
fn a_stuck_daemon_lists_stopping_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (view, cx) = failed_start(
        cx,
        dir.path(),
        &format!(
            r#""kind":"already_running","holder_pid":{}"#,
            std::process::id()
        ),
    );
    let (items, commands) = recovery_listed(&view, cx);
    assert_eq!(
        items,
        ["Stop the Stuck Daemon…", "Copy Start Error Details"]
    );
    assert_eq!(
        commands,
        [
            "Daemon: Show the Stuck Daemon",
            "Daemon: Stop the Stuck Daemon…",
            "Daemon: Copy Start Error Details"
        ]
    );
}

#[gpui::test]
fn any_other_failure_lists_its_details(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (view, cx) = failed_start(cx, dir.path(), r#""kind":"other""#);
    let (items, commands) = recovery_listed(&view, cx);
    assert_eq!(items, ["Copy Start Error Details"]);
    assert_eq!(commands, ["Daemon: Copy Start Error Details"]);
}
