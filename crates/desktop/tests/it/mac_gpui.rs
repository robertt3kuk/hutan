#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The macOS integration (M6-C) against an in-process daemon, with fakes for every system API:
//! start at login (login item), keep running, the menu bar item, the Dock badge and the global shortcut. No test
//! touches `SMAppService`, `NSStatusBar`, the Dock tile or the hotkey API.

use crate::common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use common::chat::ChatEnv;
use common::mac::{Fakes, open, open_with_services};
use common::{TestDaemon, draw, open_options, wait_until};
use gpui::{Entity, OwnedMenuItem, TestAppContext, VisualTestContext};
use hutan_client::{ConnectOptions, Connection};
use hutan_core::SessionId;
use hutan_desktop::app::start_connection;
use hutan_desktop::macos::status_item::START_AT_LOGIN;
use hutan_desktop::macos::{
    FakeLoginItem, LoginItemCall, LoginItemStatus, MacEvent, MacServices, MenuCommand, MenuSpec,
    PresenceCall, event_channel,
};
use hutan_desktop::sessions::AttentionKind;
use hutan_desktop::workbench::actions::{self, daemon as daemon_actions};
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::{Workbench, WorkbenchOptions};
use hutan_protocol::chat::{ChatSend, ChatSent};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, ClientInfo, ClientKind, Request,
};
use hutan_settings::{SettingsPaths, SettingsStore};

/// A session that fails (exit 3) once `go` exists.
fn failing_session(
    daemon: &TestDaemon,
    title: &str,
    go: &Path,
) -> hutan_protocol::messages::SessionInfo {
    daemon.sh_titled(
        title,
        &format!(
            "while [ ! -e {0} ]; do sleep 0.05; done; exit 3",
            go.display()
        ),
    )
}

fn wait_synced(view: &Entity<Workbench>, cx: &mut VisualTestContext, sessions: usize) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == sessions
        })
    });
}

fn write_settings(daemon: &TestDaemon, text: &str) {
    std::fs::write(daemon.home().join("settings.toml"), text).unwrap();
}

fn set(view: &Entity<Workbench>, cx: &mut VisualTestContext, key: &str, value: toml::Value) {
    view.read_with(cx, |wb, _| wb.settings_store().write_user(key, value))
        .unwrap();
    cx.run_until_parked();
}

fn notice(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
}

fn clear_notice(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    view.update(cx, |wb, cx| wb.clear_notice(cx));
}

fn start_at_login_setting(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.start_at_login_enabled())
}

fn dispatch(cx: &mut VisualTestContext, action: impl gpui::Action) {
    cx.dispatch_action(action);
    cx.run_until_parked();
}

/// Title, checked and enabled state of the `Start Daemon at Login` item of the `Daemon` menu.
fn start_at_login_menu_item(cx: &mut VisualTestContext) -> (String, bool, bool) {
    let menus = cx.update(|_, cx| cx.get_menus()).expect("menus are set");
    let menu = menus.iter().find(|m| m.name.as_ref() == "Daemon").unwrap();
    menu.items
        .iter()
        .find_map(|item| match item {
            OwnedMenuItem::Action {
                name,
                checked,
                disabled,
                ..
            } if name.starts_with(START_AT_LOGIN) => Some((name.to_string(), *checked, !disabled)),
            _ => None,
        })
        .expect("a start-at-login item in the Daemon menu")
}

fn login_items_menu_enabled(cx: &mut VisualTestContext) -> bool {
    let menus = cx.update(|_, cx| cx.get_menus()).expect("menus are set");
    let menu = menus.iter().find(|m| m.name.as_ref() == "Daemon").unwrap();
    menu.items
        .iter()
        .find_map(|item| match item {
            OwnedMenuItem::Action { name, disabled, .. } if name == "Open Login Items" => {
                Some(!disabled)
            }
            _ => None,
        })
        .expect("an Open Login Items item in the Daemon menu")
}

fn pump(
    cx: &mut VisualTestContext,
    what: &str,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    wait_until(cx, what, |cx| {
        cx.executor().advance_clock(Duration::from_millis(50));
        condition(cx)
    });
}

/// Keeps pumping GPUI for `span` so anything that would still happen does.
fn pump_for(cx: &mut VisualTestContext, span: Duration) {
    let until = std::time::Instant::now() + span;
    while std::time::Instant::now() < until {
        draw(cx);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn report_blocked(daemon: &TestDaemon, session: SessionId) {
    let applied: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        pid: None,
        session_id: session,
        source: "test".into(),
        agent: "omp".into(),
        state: Some(AgentState::Blocked),
        message: Some("needs a decision".into()),
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
    assert!(applied.applied);
}

// ---- start at login ------------------------------------------------------------------------

#[gpui::test]
fn the_start_at_login_toggle_registers_and_unregisters_the_launch_agent(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);

    // Off by default; nothing was registered by opening the window.
    assert!(!start_at_login_setting(&view, cx));
    assert!(fakes.login.calls().is_empty());
    assert_eq!(
        start_at_login_menu_item(cx),
        (START_AT_LOGIN.to_owned(), false, true)
    );

    // On (the Daemon menu's action): registered, enabled, shown as such.
    dispatch(cx, daemon_actions::ToggleStartAtLogin);
    assert_eq!(fakes.login.calls(), [LoginItemCall::Register]);
    assert!(start_at_login_setting(&view, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.start_at_login_status()),
        LoginItemStatus::Enabled
    );
    assert_eq!(
        start_at_login_menu_item(cx),
        (START_AT_LOGIN.to_owned(), true, true)
    );
    assert!(!login_items_menu_enabled(cx), "nothing to approve");
    assert!(
        std::fs::read_to_string(daemon.home().join("settings.toml"))
            .unwrap()
            .contains("start_at_login = true"),
        "the setting is what is saved"
    );

    // Off: unregistered.
    dispatch(cx, daemon_actions::ToggleStartAtLogin);
    assert_eq!(
        fakes.login.calls(),
        [LoginItemCall::Register, LoginItemCall::Unregister]
    );
    assert!(!start_at_login_setting(&view, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.start_at_login_status()),
        LoginItemStatus::NotRegistered
    );
}

#[gpui::test]
fn the_setting_in_the_file_and_the_settings_sheet_drive_the_same_registration(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    write_settings(&daemon, "[app]\nstart_at_login = true\n");
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert_eq!(
        fakes.login.calls(),
        [LoginItemCall::Register],
        "a window opened with the setting on registers the agent"
    );

    // Settings → General: the toggle of the sheet goes through the same path.
    view.update_in(cx, |wb, window, cx| {
        wb.open_settings(Some("app"), window, cx)
    });
    view.update(cx, |wb, cx| wb.sheet_toggle("start_at_login", cx));
    cx.run_until_parked();
    assert_eq!(
        fakes.login.calls(),
        [LoginItemCall::Register, LoginItemCall::Unregister]
    );
    assert!(!start_at_login_setting(&view, cx));

    // The file edited by hand (hot reload) registers it again.
    set(&view, cx, "app.start_at_login", toml::Value::Boolean(true));
    assert_eq!(fakes.login.calls().last(), Some(&LoginItemCall::Register));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.start_at_login_status()),
        LoginItemStatus::Enabled
    );
}

#[gpui::test]
fn requires_approval_is_shown_and_opens_login_items(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    fakes
        .login
        .set_after_register(LoginItemStatus::RequiresApproval);
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);

    dispatch(cx, daemon_actions::ToggleStartAtLogin);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.start_at_login_status()),
        LoginItemStatus::RequiresApproval
    );
    assert!(start_at_login_setting(&view, cx), "the toggle stays on");
    assert_eq!(
        start_at_login_menu_item(cx).0,
        format!("{START_AT_LOGIN} (requires approval)")
    );
    assert!(notice(&view, cx).is_some_and(|n| n.contains("Login Items")));
    assert!(login_items_menu_enabled(cx));
    // Registered but not allowed: the daemon is not launchd's yet.
    assert!(!view.read_with(cx, |wb, _| wb.start_at_login_active()));

    dispatch(cx, daemon_actions::OpenLoginItems);
    assert_eq!(
        fakes.login.calls().last(),
        Some(&LoginItemCall::OpenSettings)
    );

    // The user allows it in System Settings; coming back to the window picks that up.
    fakes.login.set_status(LoginItemStatus::Enabled);
    view.update(cx, |wb, cx| wb.refresh_login_status(cx));
    assert!(view.read_with(cx, |wb, _| wb.start_at_login_active()));
    assert_eq!(start_at_login_menu_item(cx).0, START_AT_LOGIN);
    assert!(!login_items_menu_enabled(cx));
}

/// Closes the window under test. Dropping its view does not: the workbench lives on, still
/// watching the shared `settings.toml` and writing back there when a late settings change makes
/// its own fake login item refuse. A later window reads that file too, and a stale
/// `start_at_login = false` from the earlier phase made it call `unregister` a second time.
fn close_window(cx: &mut VisualTestContext) {
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

#[gpui::test]
fn a_registration_the_system_refuses_is_reported_and_the_setting_goes_back(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    fakes.login.fail_register("Operation not permitted");
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);

    dispatch(cx, daemon_actions::ToggleStartAtLogin);
    assert_eq!(fakes.login.calls(), [LoginItemCall::Register]);
    assert!(!start_at_login_setting(&view, cx), "setting restored");
    assert!(notice(&view, cx).is_some_and(|n| n.contains("Operation not permitted")));

    // The system cannot find the plist in the bundle.
    fakes.login.set_after_register(LoginItemStatus::NotFound);
    fakes.login.fail_register("");
    clear_notice(&view, cx);
    let fakes2 = Fakes::new();
    fakes2.login.set_after_register(LoginItemStatus::NotFound);
    close_window(cx);
    drop(view);
    let (view, cx) = open(cx, &daemon, &fakes2);
    dispatch(cx, daemon_actions::ToggleStartAtLogin);
    assert!(!start_at_login_setting(&view, cx));
    assert!(notice(&view, cx).is_some_and(|n| n.contains("missing from Hutan.app")));

    // Unregistering can fail too: the setting stays on, as the system still has it.
    let fakes3 = Fakes::new();
    fakes3.login.set_status(LoginItemStatus::Enabled);
    fakes3.login.fail_unregister("busy");
    // The window above still watches the file and answers a change to it (it tries to register
    // and writes `false` back); let it do that before the next window reads the file.
    write_settings(&daemon, "[app]\nstart_at_login = true\n");
    pump_for(cx, Duration::from_millis(600));
    write_settings(&daemon, "[app]\nstart_at_login = true\n");
    close_window(cx);
    drop(view);
    let (view, cx) = open(cx, &daemon, &fakes3);
    dispatch(cx, daemon_actions::ToggleStartAtLogin);
    assert_eq!(fakes3.login.calls(), [LoginItemCall::Unregister]);
    assert!(start_at_login_setting(&view, cx));
    assert!(notice(&view, cx).is_some_and(|n| n.contains("busy")));
}

#[gpui::test]
fn outside_the_bundle_the_toggle_is_disabled_and_registers_nothing(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open_with_services(cx, &daemon, MacServices::inert());
    wait_synced(&view, cx, 0);

    assert!(!view.read_with(cx, |wb, _| wb.start_at_login_available()));
    assert_eq!(
        start_at_login_menu_item(cx),
        (
            format!("{START_AT_LOGIN} (available in Hutan.app)"),
            false,
            false
        )
    );
    dispatch(cx, daemon_actions::ToggleStartAtLogin);
    assert!(!start_at_login_setting(&view, cx), "nothing was written");
    assert!(notice(&view, cx).is_some_and(|n| n.contains("available in Hutan.app")));
    assert!(!daemon.home().join("settings.toml").exists());
    assert!(!view.read_with(cx, |wb, _| wb.start_at_login_active()));
}

#[gpui::test]
fn quitting_with_start_at_login_on_never_asks_and_never_stops_the_daemon(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let fakes = Fakes::new();
    write_settings(&daemon, "[app]\nstart_at_login = true\n");
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);
    assert!(view.read_with(cx, |wb, _| wb.start_at_login_active()));

    dispatch(cx, actions::Quit);
    wait_until(cx, "the quit", |cx| {
        view.read_with(cx, |wb, _| wb.quitting())
    });
    assert!(
        view.read_with(cx, |wb, _| wb.modal().is_none()),
        "no quit dialog"
    );
    assert!(daemon.paths.socket.exists(), "the daemon was left running");
}

#[gpui::test]
fn quitting_with_start_at_login_on_and_no_sessions_leaves_the_daemon_too(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    write_settings(&daemon, "[app]\nstart_at_login = true\n");
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);

    dispatch(cx, actions::Quit);
    wait_until(cx, "the quit", |cx| {
        view.read_with(cx, |wb, _| wb.quitting())
    });
    draw(cx);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        daemon.paths.socket.exists(),
        "without start at login an empty daemon stops with the app; with it, launchd owns it"
    );
}

#[gpui::test]
fn quitting_still_asks_when_start_at_login_is_off_or_not_approved(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string())),
        Some("Quit Hutan and stop 1 live session?".to_owned()),
        "off: the usual question"
    );
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));

    // On, but the user has not approved the login item (or switched it off again).
    fakes
        .login
        .set_after_register(LoginItemStatus::RequiresApproval);
    dispatch(cx, daemon_actions::ToggleStartAtLogin);
    assert!(start_at_login_setting(&view, cx));
    dispatch(cx, actions::Quit);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string())),
        Some("Quit Hutan and stop 1 live session?".to_owned()),
        "not approved: nothing launchd-owned to leave"
    );
    assert!(!view.read_with(cx, |wb, _| wb.quitting()));
}

// ---- keep running (the daemon outlives the app, no login item) ---------------------------------

#[gpui::test]
fn keep_running_alone_leaves_the_daemon_at_quit_and_registers_nothing(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let fakes = Fakes::new();
    write_settings(
        &daemon,
        "[app]\nkeep_running = true\nstart_at_login = false\n",
    );
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);
    assert!(view.read_with(cx, |wb, _| wb.keep_running_enabled()));
    assert!(!view.read_with(cx, |wb, _| wb.start_at_login_active()));
    assert!(fakes.login.calls().is_empty(), "opening registers nothing");

    dispatch(cx, actions::Quit);
    wait_until(cx, "the quit", |cx| {
        view.read_with(cx, |wb, _| wb.quitting())
    });
    assert!(
        view.read_with(cx, |wb, _| wb.modal().is_none()),
        "no quit dialog: the daemon was asked to stay"
    );
    assert!(daemon.paths.socket.exists(), "the daemon was left running");
    assert!(fakes.login.calls().is_empty());
}

#[gpui::test]
fn the_menu_bar_item_offers_both_toggles_and_each_does_its_own_thing(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (tx, rx) = event_channel();
    let fakes = Fakes::new();
    let mut mac = fakes.services();
    mac.events = Some(rx);
    let (view, cx) = open_with_services(cx, &daemon, mac);
    wait_synced(&view, cx, 0);
    let items = |fakes: &Fakes| -> Vec<(String, bool)> {
        fakes
            .item
            .shown()
            .unwrap()
            .menu()
            .into_iter()
            .filter_map(|row| match row {
                MenuSpec::Item {
                    command: MenuCommand::ToggleKeepRunning | MenuCommand::ToggleStartAtLogin,
                    title,
                    checked,
                    ..
                } => Some((title, checked)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        items(&fakes),
        [
            ("Keep Daemon Running After Hutan Quits".to_owned(), false),
            (START_AT_LOGIN.to_owned(), false)
        ]
    );

    tx.try_send(MacEvent::Menu(MenuCommand::ToggleKeepRunning))
        .unwrap();
    wait_until(cx, "keep running", |cx| {
        view.read_with(cx, |wb, _| wb.keep_running_enabled())
    });
    assert!(fakes.login.calls().is_empty(), "no login item for that");
    wait_until(cx, "the item to follow", |_| {
        items(&fakes)[0].1 && !items(&fakes)[1].1
    });

    tx.try_send(MacEvent::Menu(MenuCommand::ToggleStartAtLogin))
        .unwrap();
    wait_until(cx, "the registration", |_| {
        fakes.login.calls() == [LoginItemCall::Register]
    });
    wait_until(cx, "both on", |_| items(&fakes).iter().all(|(_, on)| *on));
}

#[gpui::test]
fn a_settings_file_from_before_keeps_its_login_item_and_is_told_so(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    // Before D-2 `keep_running = true` meant the launch agent was registered.
    write_settings(&daemon, "[app]\nkeep_running = true\n");
    let paths = SettingsPaths::in_dir(daemon.home());
    let store = SettingsStore::new(paths.clone());
    assert!(hutan_desktop::settings::migrate_start_at_login(
        &store,
        &paths.user
    ));
    let saved = std::fs::read_to_string(&paths.user).unwrap();
    assert!(saved.contains("start_at_login = true"), "{saved}");
    assert!(saved.contains("keep_running = true"), "{saved}");
    assert!(
        !hutan_desktop::settings::migrate_start_at_login(&store, &paths.user),
        "once"
    );

    // The agent is registered and allowed already: the window changes nothing and holds on to
    // the launchd daemon.
    let fakes = Fakes::new();
    fakes.login.set_status(LoginItemStatus::Enabled);
    let (connection, launchd) = start_connection(
        connect_options(&daemon.dir.path().join("none.sock"), None),
        &store,
        &fakes.login,
    );
    assert!(launchd, "the daemon is launchd's");
    connection.shutdown();
    let options =
        WorkbenchOptions::builtin(store, daemon.connection(), None).with_mac(fakes.services());
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx, 0);
    view.read_with(cx, |wb, _| {
        assert!(wb.keep_running_enabled() && wb.start_at_login_enabled());
        assert!(wb.start_at_login_active());
    });
    assert!(
        fakes.login.calls().is_empty(),
        "nothing registered or removed"
    );
    let notes = view.read_with(cx, |wb, _| wb.settings_store().errors());
    assert!(
        notes
            .iter()
            .any(|n| n.key_path == "app.start_at_login" && n.message.contains("keep_running")),
        "the migration is noted: {notes:?}"
    );

    // Without the old login item nothing registers: a file that already names both keys (or a
    // keep_running of the new kind) is left as written.
    let other = TestDaemon::start();
    write_settings(
        &other,
        "[app]\nkeep_running = true\nstart_at_login = false\n",
    );
    let paths = SettingsPaths::in_dir(other.home());
    let store = SettingsStore::new(paths.clone());
    assert!(!hutan_desktop::settings::migrate_start_at_login(
        &store,
        &paths.user
    ));
    let fakes = Fakes::new();
    let options =
        WorkbenchOptions::builtin(store, other.connection(), None).with_mac(fakes.services());
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx, 0);
    view.read_with(cx, |wb, _| {
        assert!(wb.keep_running_enabled() && !wb.start_at_login_enabled());
    });
    assert!(fakes.login.calls().is_empty());
}

fn connect_options(socket: &Path, autostart: Option<&Path>) -> ConnectOptions {
    ConnectOptions {
        socket: Some(socket.to_owned()),
        autostart: autostart.map(Path::to_owned),
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: "desktop-test".into(),
            origin: None,
        },
    }
}

fn spawn_marker_script(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let marker = dir.join("spawned");
    let script = dir.join("fake-hutand");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, marker)
}

#[gpui::test]
fn the_desktop_waits_for_the_launchd_daemon_instead_of_spawning_one(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (script, marker) = spawn_marker_script(dir.path());
    let socket = dir.path().join("hutand.sock");
    let settings = |text: &str| {
        let home = tempfile::tempdir_in(dir.path()).unwrap();
        std::fs::write(home.path().join("settings.toml"), text).unwrap();
        (SettingsStore::new(SettingsPaths::in_dir(home.path())), home)
    };

    // Start at login on and approved: held, so the first dial cannot race launchd.
    let enabled = FakeLoginItem::new();
    enabled.set_status(LoginItemStatus::Enabled);
    let (store, _home) = settings("[app]\nstart_at_login = true\n");
    let (held, launchd) =
        start_connection(connect_options(&socket, Some(&script)), &store, &enabled);
    assert!(launchd);
    assert!(!held.autostart());
    held.shutdown();

    // Every other combination autostarts as before.
    let (store, _home) = settings("[app]\nstart_at_login = false\n");
    let (conn, launchd) = start_connection(
        connect_options(&dir.path().join("a.sock"), None),
        &store,
        &enabled,
    );
    assert!(!launchd);
    assert!(!conn.autostart(), "no binary to autostart anyway");
    conn.shutdown();
    let pending = FakeLoginItem::new();
    pending.set_status(LoginItemStatus::RequiresApproval);
    let (store, _home) = settings("[app]\nstart_at_login = true\n");
    let (conn, launchd) = start_connection(
        connect_options(&dir.path().join("b.sock"), Some(&script)),
        &store,
        &pending,
    );
    assert!(
        !launchd,
        "registered but not approved: launchd does not run it"
    );
    assert!(conn.autostart());
    conn.shutdown();
    assert!(
        !marker.exists(),
        "nothing was spawned by the held connection"
    );
    let _ = cx;
}

#[gpui::test]
fn without_a_launchd_daemon_after_the_grace_the_desktop_starts_one(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (script, marker) = spawn_marker_script(dir.path());
    let socket = dir.path().join("hutand.sock");
    let connection = Connection::start_held(connect_options(&socket, Some(&script)));
    let fakes = Fakes::new();
    let settings = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
    let mut mac = fakes.services();
    mac.launchd_daemon = true;
    mac.launchd_grace = Duration::from_millis(400);
    let options = WorkbenchOptions::builtin(settings, connection.clone(), None).with_mac(mac);
    let (_view, cx) = open_options(cx, options);

    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !marker.exists(),
        "inside the grace period nothing is started"
    );
    assert!(!connection.autostart());

    pump(cx, "the grace period to end", |_| connection.autostart());
    wait_until(cx, "hutand to be started", |_| marker.exists());
    connection.shutdown();
}

#[gpui::test]
fn a_launchd_daemon_that_answers_in_time_is_left_alone(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let (script, marker) = spawn_marker_script(dir.path());
    // The daemon is already up (launchd started it): dialled, not spawned.
    let connection = Connection::start_held(connect_options(&daemon.paths.socket, Some(&script)));
    let fakes = Fakes::new();
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let mut mac = fakes.services();
    mac.launchd_daemon = true;
    mac.launchd_grace = Duration::from_millis(300);
    let options = WorkbenchOptions::builtin(settings, connection.clone(), None).with_mac(mac);
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx, 0);
    for _ in 0..8 {
        cx.executor().advance_clock(Duration::from_millis(100));
        draw(cx);
    }
    assert!(!connection.autostart());
    assert!(!marker.exists());
    connection.shutdown();
}

// ---- menu bar item and Dock badge --------------------------------------------------------

#[gpui::test]
fn the_status_item_and_the_badge_follow_the_attention_model(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let blocked = daemon.sh_titled("agent", "sleep 600");
    let go = daemon.home().join("go");
    let failing = failing_session(&daemon, "build", &go);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 2);

    // Nothing needs the user yet: the item is on the menu bar without a count.
    let shown = fakes.item.shown().expect("menu_bar defaults to on");
    assert_eq!(shown.daemon, "hutand connected");
    assert!(shown.needs_you.is_empty());
    assert_eq!(shown.title(), "");
    assert_eq!(fakes.dock.badge(), 0);

    // An agent reports blocked: counted and listed, and its row focuses the session.
    report_blocked(&daemon, blocked.id);
    wait_until(cx, "the blocked agent to be counted", |_| {
        fakes.dock.badge() == 1
    });
    let shown = fakes.item.shown().unwrap();
    assert_eq!(shown.title(), "1");
    assert_eq!(shown.needs_you.len(), 1);
    assert_eq!(shown.needs_you[0].session, blocked.id);
    assert_eq!(shown.needs_you[0].kind, AttentionKind::Blocked);
    assert!(shown.menu().iter().any(|row| matches!(
        row,
        MenuSpec::Item { command: MenuCommand::FocusSession(id), title, .. }
            if *id == blocked.id && title.starts_with("agent — blocked")
    )));

    // A session fails while the user is elsewhere: it counts too.
    std::fs::write(&go, "").unwrap();
    wait_until(cx, "the failure to be counted", |_| fakes.dock.badge() == 2);
    let shown = fakes.item.shown().unwrap();
    assert_eq!(shown.title(), "2");
    let kinds: Vec<_> = shown
        .needs_you
        .iter()
        .map(|e| (e.session, e.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            (blocked.id, AttentionKind::Blocked),
            (failing.id, AttentionKind::Failed)
        ]
    );

    // Looking at the failed session clears it; the badge steps down.
    view.update_in(cx, |wb, window, cx| wb.open_session(failing.id, window, cx));
    wait_until(cx, "the failure to be read", |_| fakes.dock.badge() == 1);
    assert_eq!(fakes.item.shown().unwrap().title(), "1");

    // The agent is unblocked: nothing is left, and the badge is gone.
    let applied: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        pid: None,
        session_id: blocked.id,
        source: "test".into(),
        agent: "omp".into(),
        state: Some(AgentState::Working),
        message: None,
        seq: 2,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
    assert!(applied.applied);
    wait_until(cx, "the badge to clear", |_| fakes.dock.badge() == 0);
    assert_eq!(fakes.item.shown().unwrap().title(), "");

    // Every step was one transition, never a repeated value.
    assert_eq!(fakes.dock.history(), [0, 1, 2, 1, 0]);
}

#[gpui::test]
fn a_pending_approval_is_counted_with_the_session_it_blocks(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let chat = daemon.create_chat(&env.cwd());
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);
    assert_eq!(fakes.dock.badge(), 0);

    daemon.call::<ChatSent>(Request::ChatSend(ChatSend {
        session_id: chat.id,
        text: "Run `touch hi.txt` in a shell, then reply with the word done.".to_owned(),
        attachments: Vec::new(),
    }));
    pump(cx, "the approval to be counted", |_| {
        fakes.dock.badge() == 1
    });
    let shown = fakes.item.shown().unwrap();
    assert_eq!(shown.needs_you.len(), 1);
    assert_eq!(shown.needs_you[0].session, chat.id);
    assert_eq!(shown.needs_you[0].kind, AttentionKind::Approval);
    assert_eq!(shown.title(), "1");
}

#[gpui::test]
fn the_status_item_names_the_daemon_state(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert_eq!(fakes.item.shown().unwrap().daemon, "hutand connected");

    // Stopped from the app: the item says so (and nothing is reconnecting behind it).
    dispatch(cx, daemon_actions::Stop);
    pump(cx, "the daemon to stop", |_| {
        fakes
            .item
            .shown()
            .is_some_and(|m| m.daemon == "hutand stopped")
    });
}

#[gpui::test]
fn the_menu_bar_item_comes_and_goes_with_its_setting(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert!(fakes.item.shown().is_some());
    assert_eq!(fakes.item.hides(), 0);

    set(&view, cx, "app.menu_bar", toml::Value::Boolean(false));
    assert!(
        fakes.item.shown().is_none(),
        "removed when the setting goes off"
    );
    assert_eq!(fakes.item.hides(), 1);

    // Unrelated changes do not bring it back or remove it twice.
    set(&view, cx, "app.dock_badge", toml::Value::Boolean(false));
    assert!(fakes.item.shown().is_none());
    assert_eq!(fakes.item.hides(), 1);

    let shows = fakes.item.shows();
    set(&view, cx, "app.menu_bar", toml::Value::Boolean(true));
    assert!(fakes.item.shown().is_some(), "and back, without a restart");
    assert_eq!(fakes.item.shows(), shows + 1);
}

#[gpui::test]
fn the_dock_badge_follows_its_setting(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let blocked = daemon.sh("sleep 600");
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);
    report_blocked(&daemon, blocked.id);
    wait_until(cx, "the count", |_| fakes.dock.badge() == 1);

    set(&view, cx, "app.dock_badge", toml::Value::Boolean(false));
    assert_eq!(fakes.dock.badge(), 0, "off: the badge is cleared");
    // The menu bar item still counts: the two are separate settings.
    assert_eq!(fakes.item.shown().unwrap().title(), "1");

    set(&view, cx, "app.dock_badge", toml::Value::Boolean(true));
    assert_eq!(fakes.dock.badge(), 1);
    assert_eq!(fakes.dock.history(), [0, 1, 0, 1]);
}

#[gpui::test]
fn menu_bar_choices_drive_the_window(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let go = daemon.home().join("go");
    let failing = failing_session(&daemon, "build", &go);
    let (tx, rx) = event_channel();
    let fakes = Fakes::new();
    let mut mac = fakes.services();
    mac.events = Some(rx);
    let (view, cx) = open_with_services(cx, &daemon, mac);
    wait_synced(&view, cx, 1);
    std::fs::write(&go, "").unwrap();
    wait_until(cx, "the failure", |_| fakes.dock.badge() == 1);

    // "build — failed" in the menu: its tab opens and the failure counts as seen.
    tx.try_send(MacEvent::Menu(MenuCommand::FocusSession(failing.id)))
        .unwrap();
    wait_until(cx, "the session to open", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(failing.id).is_some())
    });
    wait_until(cx, "the badge to clear", |_| fakes.dock.badge() == 0);

    // New Terminal.
    tx.try_send(MacEvent::Menu(MenuCommand::NewTerminal))
        .unwrap();
    wait_synced(&view, cx, 2);

    // Start at login (checkbox).
    tx.try_send(MacEvent::Menu(MenuCommand::ToggleStartAtLogin))
        .unwrap();
    wait_until(cx, "the registration", |_| {
        fakes.login.calls() == [LoginItemCall::Register]
    });

    // A session that is gone is said so.
    tx.try_send(MacEvent::Menu(MenuCommand::FocusSession(SessionId::new())))
        .unwrap();
    wait_until(cx, "the notice", |cx| {
        notice(&view, cx).is_some_and(|n| n.contains("no longer exists"))
    });

    // Quit Hutan runs the usual flow (start at login is on: nothing to ask, daemon stays).
    tx.try_send(MacEvent::Menu(MenuCommand::Quit)).unwrap();
    wait_until(cx, "the quit", |cx| {
        view.read_with(cx, |wb, _| wb.quitting())
    });
    assert!(daemon.paths.socket.exists());
}

// ---- global shortcut ---------------------------------------------------------------------

#[gpui::test]
fn nothing_is_registered_by_default_and_a_configured_shortcut_opens_the_assistant(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert!(
        fakes.hotkey.attempts().is_empty(),
        "no system-wide shortcut unless the user sets one"
    );
    drop(view);

    let daemon = TestDaemon::start();
    write_settings(
        &daemon,
        "[assistant]\nenabled = true\nglobal_shortcut = \"alt-space\"\n",
    );
    let (tx, rx) = event_channel();
    let fakes = Fakes::new();
    let mut mac = fakes.services();
    mac.events = Some(rx);
    let (view, cx) = open_with_services(cx, &daemon, mac);
    wait_synced(&view, cx, 0);
    assert_eq!(fakes.hotkey.active().as_deref(), Some("⌥Space"));
    assert_eq!(fakes.hotkey.attempts(), ["⌥Space"]);
    assert_eq!(notice(&view, cx), None);

    // Pressed while another app is in front: the window comes forward with the Assistant.
    assert_ne!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(ItemRef::panel("assistant"))
    );
    tx.try_send(MacEvent::Hotkey).unwrap();
    wait_until(cx, "the Assistant to open", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state().active_center_item() == Some(ItemRef::panel("assistant"))
        })
    });
}

#[gpui::test]
fn a_taken_shortcut_is_reported_once_and_the_setting_is_left_alone(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let configured = "[assistant]\nenabled = true\nglobal_shortcut = \"alt-space\"\n";
    write_settings(&daemon, configured);
    let fakes = Fakes::new();
    fakes.hotkey.take("⌥Space");
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);

    assert_eq!(fakes.hotkey.active(), None);
    let text = notice(&view, cx).expect("a notice");
    assert!(
        text.contains("⌥Space") && text.contains("another app"),
        "{text}"
    );
    assert_eq!(
        std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap(),
        configured,
        "the setting was not touched"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_store()
            .get::<hutan_desktop::settings::AssistantSettings>()
            .global_shortcut
            .clone()),
        "alt-space"
    );

    // Other settings reloading does not retry or repeat the notice.
    clear_notice(&view, cx);
    set(&view, cx, "app.menu_bar", toml::Value::Boolean(false));
    set(&view, cx, "app.menu_bar", toml::Value::Boolean(true));
    assert_eq!(notice(&view, cx), None);
    assert_eq!(fakes.hotkey.attempts(), ["⌥Space"]);

    // A different shortcut works; the other app letting go makes the original possible again
    // once the setting is touched.
    set(
        &view,
        cx,
        "assistant.global_shortcut",
        toml::Value::String("cmd-alt-space".into()),
    );
    assert_eq!(fakes.hotkey.active().as_deref(), Some("⌥⌘Space"));
    assert_eq!(notice(&view, cx), None);
    fakes.hotkey.release("⌥Space");
    set(
        &view,
        cx,
        "assistant.global_shortcut",
        toml::Value::String("alt-space".into()),
    );
    assert_eq!(fakes.hotkey.active().as_deref(), Some("⌥Space"));
}

#[gpui::test]
fn an_empty_shortcut_turns_it_off_and_garbage_is_reported(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(
        &daemon,
        "[assistant]\nenabled = true\nglobal_shortcut = \"\"\n",
    );
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert!(
        fakes.hotkey.attempts().is_empty(),
        "empty: nothing is registered"
    );
    assert_eq!(notice(&view, cx), None);

    set(
        &view,
        cx,
        "assistant.global_shortcut",
        toml::Value::String("ctrl-alt-k".into()),
    );
    assert_eq!(fakes.hotkey.active().as_deref(), Some("⌃⌥K"));

    set(
        &view,
        cx,
        "assistant.global_shortcut",
        toml::Value::String(String::new()),
    );
    assert_eq!(fakes.hotkey.active(), None);
    assert_eq!(fakes.hotkey.unregistered(), 1);

    set(
        &view,
        cx,
        "assistant.global_shortcut",
        toml::Value::String("space".into()),
    );
    assert_eq!(fakes.hotkey.active(), None);
    assert!(notice(&view, cx).is_some_and(|n| n.contains("not valid")));
}

// ---- schema ------------------------------------------------------------------------------

#[gpui::test]
fn the_new_namespaces_and_fields_are_in_the_settings_sheet(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    view.update_in(cx, |wb, window, cx| {
        wb.open_settings(Some("app"), window, cx)
    });
    let app = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .fields()
            .iter()
            .map(|f| f.dotted())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        app,
        [
            "keep_running",
            "start_at_login",
            "setup_done",
            "on_last_window_close",
            "menu_bar",
            "dock_badge"
        ]
    );
    view.update(cx, |wb, cx| wb.select_settings_namespace("assistant", cx));
    let assistant = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .fields()
            .iter()
            .map(|f| f.dotted())
            .collect::<Vec<_>>()
    });
    assert_eq!(assistant, ["enabled", "global_shortcut"]);
    // Drawing the sheet with the status next to the toggle must not panic.
    view.update(cx, |wb, cx| wb.select_settings_namespace("app", cx));
    draw(cx);
}

// ---- closing the last window -------------------------------------------------------------

fn menu_bar_mode(daemon: &TestDaemon) {
    write_settings(daemon, "[app]\non_last_window_close = \"menu_bar\"\n");
}

#[gpui::test]
fn closing_the_last_window_quits_by_default_like_command_q(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);

    assert!(
        !cx.simulate_close(),
        "the window is kept until it is decided"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string())),
        Some("Quit Hutan and stop 1 live session?".to_owned()),
        "the same question as for ⌘Q"
    );
    assert!(
        fakes.presence.calls().is_empty(),
        "the app does not go to the menu bar"
    );
    assert!(!view.read_with(cx, |wb, _| wb.in_menu_bar()));
}

#[gpui::test]
fn closing_the_last_window_can_keep_hutan_in_the_menu_bar(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let session = daemon.sh("sleep 600");
    menu_bar_mode(&daemon);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);

    assert!(!cx.simulate_close(), "the window is kept, hidden");
    assert!(view.read_with(cx, |wb, _| wb.in_menu_bar()));
    assert_eq!(
        fakes.presence.calls(),
        [PresenceCall::Accessory(true), PresenceCall::Hide],
        "no Dock icon, no windows"
    );
    assert!(!view.read_with(cx, |wb, _| wb.quitting()));
    assert!(
        view.read_with(cx, |wb, _| wb.modal().is_none()),
        "nothing is asked"
    );
    pump_for(cx, Duration::from_millis(300));
    assert!(daemon.paths.socket.exists(), "the daemon keeps running");
    assert!(daemon.session(session.id).state.is_live());
    // The menu bar item is still there, serving.
    assert!(fakes.item.shown().is_some());

    // Closing again while hidden changes nothing.
    assert!(!cx.simulate_close());
    assert_eq!(fakes.presence.calls().len(), 2);
}

#[gpui::test]
fn the_menu_bar_choices_bring_the_window_back(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    menu_bar_mode(&daemon);
    let (tx, rx) = event_channel();
    let fakes = Fakes::new();
    let mut mac = fakes.services();
    mac.events = Some(rx);
    let (view, cx) = open_with_services(cx, &daemon, mac);
    wait_synced(&view, cx, 0);

    for (event, what) in [
        (MacEvent::Menu(MenuCommand::OpenHutan), "Open Hutan"),
        (MacEvent::Hotkey, "the shortcut"),
        (MacEvent::Menu(MenuCommand::NewTerminal), "New Terminal"),
    ] {
        assert!(!cx.simulate_close());
        assert!(
            view.read_with(cx, |wb, _| wb.in_menu_bar()),
            "{what}: hidden"
        );
        let before = fakes.presence.calls().len();
        tx.try_send(event).unwrap();
        wait_until(cx, what, |cx| !view.read_with(cx, |wb, _| wb.in_menu_bar()));
        assert_eq!(
            fakes.presence.calls()[before..],
            [PresenceCall::Accessory(false), PresenceCall::Show],
            "{what}: a Dock icon and the windows again"
        );
    }
    assert!(!fakes.presence.accessory());
}

#[gpui::test]
fn quitting_from_the_menu_bar_while_hidden_stops_an_idle_daemon_or_asks(cx: &mut TestAppContext) {
    // Idle: silent.
    let idle = TestDaemon::start();
    menu_bar_mode(&idle);
    let (tx, rx) = event_channel();
    let fakes = Fakes::new();
    let mut mac = fakes.services();
    mac.events = Some(rx);
    let (view, cx) = open_with_services(cx, &idle, mac);
    wait_synced(&view, cx, 0);
    assert!(!cx.simulate_close());
    tx.try_send(MacEvent::Menu(MenuCommand::Quit)).unwrap();
    wait_until(cx, "the quit", |cx| {
        view.read_with(cx, |wb, _| wb.quitting())
    });
    wait_until(cx, "the daemon to stop", |_| !idle.paths.socket.exists());
}

#[gpui::test]
fn quitting_from_the_menu_bar_with_live_sessions_shows_the_window_and_asks(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let session = daemon.sh("sleep 600");
    menu_bar_mode(&daemon);
    let (tx, rx) = event_channel();
    let fakes = Fakes::new();
    let mut mac = fakes.services();
    mac.events = Some(rx);
    let (view, cx) = open_with_services(cx, &daemon, mac);
    wait_synced(&view, cx, 1);
    assert!(!cx.simulate_close());

    tx.try_send(MacEvent::Menu(MenuCommand::Quit)).unwrap();
    wait_until(cx, "the question", |cx| {
        view.read_with(cx, |wb, _| wb.modal().is_some())
    });
    assert!(
        !view.read_with(cx, |wb, _| wb.in_menu_bar()),
        "the question is asked in the window, which is shown for it"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string())),
        Some("Quit Hutan and stop 1 live session?".to_owned())
    );
    assert!(daemon.session(session.id).state.is_live());
}

#[gpui::test]
fn the_menu_bar_mode_needs_the_menu_bar_item(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(
        &daemon,
        "[app]\non_last_window_close = \"menu_bar\"\nmenu_bar = false\n",
    );
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);

    // Nothing would be left to reopen Hutan from: closing quits, and the settings say why.
    let diagnostics = view.read_with(cx, |wb, _| wb.settings_store().errors());
    assert!(
        diagnostics.iter().any(|d| d.key_path == "app.on_last_window_close"
            && d.message.contains("app.menu_bar")),
        "{diagnostics:?}"
    );
    assert!(!cx.simulate_close());
    assert!(fakes.presence.calls().is_empty());
    wait_until(cx, "the quit", |cx| {
        view.read_with(cx, |wb, _| wb.quitting())
    });

    // The diagnostic goes away with the contradiction.
    set(&view, cx, "app.menu_bar", toml::Value::Boolean(true));
    assert!(
        view.read_with(cx, |wb, _| wb.settings_store().errors())
            .iter()
            .all(|d| d.key_path != "app.on_last_window_close")
    );
}

#[gpui::test]
fn turning_the_menu_bar_item_off_while_hidden_brings_the_window_back(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    menu_bar_mode(&daemon);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert!(!cx.simulate_close());
    assert!(view.read_with(cx, |wb, _| wb.in_menu_bar()));

    // The item that was the only way back is gone: show the window rather than strand Hutan.
    set(&view, cx, "app.menu_bar", toml::Value::Boolean(false));
    assert!(!view.read_with(cx, |wb, _| wb.in_menu_bar()));
    assert!(!fakes.presence.accessory());
    assert!(fakes.item.shown().is_none());
}

#[gpui::test]
fn a_second_desktop_or_the_dock_brings_the_window_forward(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    menu_bar_mode(&daemon);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert!(!cx.simulate_close());
    assert!(view.read_with(cx, |wb, _| wb.in_menu_bar()));

    let reopened = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = reopened.clone();
    let found =
        cx.update(|_, cx| hutan_desktop::app::activate_main_window(cx, move |_| flag.set(true)));
    cx.run_until_parked();
    assert!(found);
    assert!(
        !reopened.get(),
        "the window still exists: nothing is recreated"
    );
    assert!(!view.read_with(cx, |wb, _| wb.in_menu_bar()));
    assert_eq!(fakes.presence.calls().last(), Some(&PresenceCall::Show));
}

#[gpui::test]
fn without_a_window_the_activation_reopens_one(cx: &mut TestAppContext) {
    let reopened = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = reopened.clone();
    let found =
        cx.update(|cx| hutan_desktop::app::activate_main_window(cx, move |_| flag.set(true)));
    cx.run_until_parked();
    assert!(!found, "there is no workbench window");
    assert!(reopened.get(), "so the caller's reopen ran");
}

#[gpui::test]
fn an_old_on_quit_key_is_ignored_with_a_diagnostic(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(
        &daemon,
        "[app]\non_quit = \"stop_daemon\"\nstart_at_login = false\n",
    );
    daemon.sh("sleep 600");
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);

    let diagnostics = view.read_with(cx, |wb, _| wb.settings_store().errors());
    let note = diagnostics
        .iter()
        .find(|d| d.key_path.contains("on_quit"))
        .unwrap_or_else(|| panic!("no diagnostic for on_quit in {diagnostics:?}"));
    assert_eq!(
        note.severity,
        hutan_settings::Severity::Warning,
        "not an error"
    );
    // The old value changes nothing: the daemon follows the app, and asks first.
    dispatch(cx, actions::Quit);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string())),
        Some("Quit Hutan and stop 1 live session?".to_owned())
    );
}

#[gpui::test]
fn quitting_from_the_menu_bar_with_live_sessions_brings_a_background_window_forward(
    cx: &mut TestAppContext,
) {
    // The item's Quit (or the Dock's) is clicked while Hutan is behind other apps: the question
    // is in the window, so the window has to come forward or the click looks like it did nothing.
    let daemon = TestDaemon::start();
    let _session = daemon.sh("sleep 600");
    let (tx, rx) = event_channel();
    let fakes = Fakes::new();
    let mut mac = fakes.services();
    mac.events = Some(rx);
    let (view, cx) = open_with_services(cx, &daemon, mac);
    wait_synced(&view, cx, 1);
    cx.deactivate_window();
    assert!(!cx.update(|window, _| window.is_window_active()));

    tx.try_send(MacEvent::Menu(MenuCommand::Quit)).unwrap();
    wait_until(cx, "the question", |cx| {
        view.read_with(cx, |wb, _| wb.modal().is_some())
    });
    assert!(
        cx.update(|window, _| window.is_window_active()),
        "the window came forward with the question"
    );
    assert!(!view.read_with(cx, |wb, _| wb.quitting()));
}
