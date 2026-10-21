#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests for the daemon's lifetime (design.md § Daemon lifecycle): the quit flow and
//! the daemon following the app (`app.keep_running`, `app.start_at_login`), the termination-signal path, the daemon controls (popover, palette, menu),
//! and the working-mode control. Daemons are in-process on a temporary `HUTAN_HOME`, or a
//! real `hutand` process where the test starts one again.

use crate::common;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use common::mac::{Fakes, open as open_mac};
use common::{ProcessDaemon, TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, OwnedMenuItem, TestAppContext, VisualTestContext};
use hutan_client::{Connection, ConnectionState};
use hutan_core::SessionId;
use hutan_desktop::macos::LoginItemCall;
use hutan_desktop::workbench::actions::{self, daemon as daemon_actions, palette};
use hutan_desktop::workbench::model::DockPosition;
use hutan_desktop::workbench::{DaemonBusy, Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};
use hutan_terminal_view::ViewStatus;

fn settings_in(dir: &Path) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(dir))
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_with(
        cx,
        settings_in(daemon.home()),
        daemon.connection(),
        state_path,
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

fn connection_state(view: &Entity<Workbench>, cx: &VisualTestContext) -> ConnectionState {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).connection_state())
}

fn quitting(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.quitting())
}

fn modal_title(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string()))
}

fn status_line(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, cx| wb.daemon_status_line(cx))
}

fn dispatch(cx: &mut VisualTestContext, action: impl gpui::Action) {
    cx.dispatch_action(action);
    cx.run_until_parked();
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    draw(cx);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} was not drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn confirm(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    cx.run_until_parked();
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

/// Keeps pumping GPUI for `span` so anything that would still happen does.
fn pump_for(cx: &mut VisualTestContext, span: Duration) {
    let until = Instant::now() + span;
    while Instant::now() < until {
        draw(cx);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_socket_gone(cx: &mut VisualTestContext, socket: &Path) {
    wait_until(cx, "the daemon to remove its socket", |_| !socket.exists());
}

fn open_session(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
}

fn terminal_status(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    id: SessionId,
) -> Option<ViewStatus> {
    view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(id).map(|v| v.read(cx).status().clone())
    })
}

/// Whether the open view of `id` is attached to a terminal of the daemon the window is
/// connected to now (terminal ids start over with every daemon, so the epoch tells).
fn attached_to_current_daemon(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    id: SessionId,
) -> bool {
    view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        store
            .attached(id)
            .is_some_and(|t| t.read(cx).daemon_epoch() == store.state().epoch)
    })
}

/// Name and enabled state of the actions of the `Daemon` menu, as the menu bar holds them right
/// now.
type NamedFlags = Vec<(String, bool)>;

fn daemon_menu(cx: &mut VisualTestContext) -> NamedFlags {
    let menus = cx.update(|_, cx| cx.get_menus()).expect("menus are set");
    let menu = menus
        .iter()
        .find(|m| m.name.as_ref() == "Daemon")
        .expect("a Daemon menu");
    menu.items
        .iter()
        .filter_map(|item| match item {
            OwnedMenuItem::Action { name, disabled, .. } => Some((name.clone(), !disabled)),
            _ => None,
        })
        .collect()
}

// quit: no live sessions ------------------------------------------------------------------

#[gpui::test]
fn quitting_without_live_sessions_stops_the_daemon_and_saves_the_layout(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let state = daemon.home().join("desktop-state.json");
    let (view, cx) = open(cx, &daemon, Some(state.clone()));
    wait_synced(&view, cx, 0);

    // A layout change whose debounced save has not run yet must still reach the disk.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.toggle_dock(DockPosition::Bottom);
        });
    });
    assert!(!state.exists());

    dispatch(cx, actions::Quit);
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    assert!(state.exists(), "the layout was flushed before quitting");
    assert_eq!(
        connection_state(&view, cx),
        ConnectionState::Stopped,
        "the connection was closed"
    );
}

#[gpui::test]
fn closing_the_last_window_runs_the_same_flow(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);

    // The window stays until the flow has decided; the flow then quits the app.
    assert!(!cx.simulate_close(), "the window is kept");
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
}

#[gpui::test]
fn closing_the_last_window_with_live_sessions_asks(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    assert!(!cx.simulate_close());
    assert_eq!(modal_title(&view, cx).as_deref(), Some(QUIT_TITLE));
    assert!(!quitting(&view, cx));
    assert!(daemon.paths.socket.exists());
}

#[gpui::test]
fn quitting_while_disconnected_quits_at_once(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    daemon.stop();
    wait_until(cx, "the connection to drop", |cx| {
        connection_state(&view, cx) != ConnectionState::Connected
    });
    dispatch(cx, actions::Quit);
    assert!(quitting(&view, cx), "no question, no wait");
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
}

// quit: live sessions ------------------------------------------------------------------------

const QUIT_TITLE: &str = "Quit Hutan and stop 1 live session?";

#[gpui::test]
fn with_live_sessions_quitting_asks_to_stop_them(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    assert_eq!(modal_title(&view, cx).as_deref(), Some(QUIT_TITLE));
    let (confirm_label, choice, checkbox) = view.read_with(cx, |wb, _| {
        let modal = wb.modal().unwrap();
        (
            modal.confirm_label.to_string(),
            modal.choice_label().map(ToOwned::to_owned),
            modal.checkbox.is_some(),
        )
    });
    assert_eq!(confirm_label, "Stop Sessions & Quit");
    assert_eq!(choice.as_deref(), Some("Keep Running in Background"));
    assert!(
        !checkbox,
        "there is nothing to remember: the toggle is the setting"
    );
    assert!(!quitting(&view, cx), "nothing happens before the choice");
    assert_eq!(read(daemon.home(), "settings.toml"), "");
}

#[gpui::test]
fn stop_sessions_and_quit_stops_everything_then_quits(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    confirm(&view, cx);
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    assert_eq!(connection_state(&view, cx), ConnectionState::Stopped);
}

#[gpui::test]
fn cancelling_the_quit_changes_nothing(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    pump_for(cx, Duration::from_millis(300));
    assert!(!quitting(&view, cx));
    assert_eq!(connection_state(&view, cx), ConnectionState::Connected);
    assert!(daemon.paths.socket.exists());
    assert_eq!(read(daemon.home(), "settings.toml"), "");
}

#[gpui::test]
fn keep_running_in_background_sets_only_keep_running_and_leaves_the_daemon(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let session = daemon.sh("sleep 600");
    let fakes = Fakes::new();
    let (view, cx) = open_mac(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    view.update_in(cx, |wb, window, cx| wb.choose_modal_alternative(window, cx));
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    assert!(
        fakes.login.calls().is_empty(),
        "the login item is a separate choice: {:?}",
        fakes.login.calls()
    );
    let saved = read(daemon.home(), "settings.toml");
    assert!(saved.contains("keep_running = true"), "{saved}");
    assert!(!saved.contains("start_at_login = true"), "{saved}");
    view.read_with(cx, |wb, _| {
        assert!(wb.keep_running_enabled() && !wb.start_at_login_enabled());
    });
    assert_eq!(connection_state(&view, cx), ConnectionState::Stopped);
    assert!(daemon.paths.socket.exists(), "the daemon keeps running");
    assert!(daemon.session(session.id).state.is_live());
}

#[gpui::test]
fn keep_running_works_outside_the_bundle_too(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let session = daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    view.update_in(cx, |wb, window, cx| wb.choose_modal_alternative(window, cx));
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    assert!(
        read(daemon.home(), "settings.toml").contains("keep_running = true"),
        "it needs no login item"
    );
    assert!(daemon.paths.socket.exists());
    assert!(daemon.session(session.id).state.is_live());
}

#[gpui::test]
fn keep_running_leaves_the_daemon_without_asking_even_when_nothing_runs(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write(
        daemon.home(),
        "settings.toml",
        "[app]\nkeep_running = true\n",
    );
    let fakes = Fakes::new();
    let (view, cx) = open_mac(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert!(!view.read_with(cx, |wb, _| wb.start_at_login_active()));

    dispatch(cx, actions::Quit);
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()), "no question");
    pump_for(cx, Duration::from_millis(300));
    assert!(daemon.paths.socket.exists(), "the daemon was left running");
    assert!(fakes.login.calls().is_empty(), "and nothing was registered");
}

#[gpui::test]
fn the_background_choice_does_not_depend_on_the_login_item(cx: &mut TestAppContext) {
    // A login item that would refuse changes nothing: the choice does not register anything.
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let fakes = Fakes::new();
    fakes.login.fail_register("Operation not permitted");
    let (view, cx) = open_mac(cx, &daemon, &fakes);
    wait_synced(&view, cx, 1);

    dispatch(cx, actions::Quit);
    view.update_in(cx, |wb, window, cx| wb.choose_modal_alternative(window, cx));
    wait_until(cx, "the quit", |cx| quitting(&view, cx));
    assert!(fakes.login.calls().is_empty());
    assert!(view.read_with(cx, |wb, _| wb.notice().is_none()));
    assert!(daemon.paths.socket.exists());
}

#[gpui::test]
fn a_platform_quit_stops_an_idle_daemon_and_leaves_one_with_sessions(cx: &mut TestAppContext) {
    // The Dock's Quit, logout, shutdown: no window to ask in.
    let idle = TestDaemon::start();
    let (view, cx) = open(cx, &idle, None);
    wait_synced(&view, cx, 0);
    view.update(cx, |wb, cx| {
        let goodbye = wb.on_platform_quit(cx);
        cx.spawn(async move |_, _| goodbye.await).detach();
    });
    wait_socket_gone(cx, &idle.paths.socket);
}

#[gpui::test]
fn a_platform_quit_leaves_a_daemon_with_live_sessions_alone(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let session = daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);
    view.update(cx, |wb, cx| {
        let goodbye = wb.on_platform_quit(cx);
        cx.spawn(async move |_, _| goodbye.await).detach();
    });
    pump_for(cx, Duration::from_millis(300));
    assert!(daemon.paths.socket.exists());
    assert!(daemon.session(session.id).state.is_live());
}

#[gpui::test]
fn a_termination_signal_saves_disconnects_and_never_stops_the_daemon(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let session = daemon.sh("sleep 600");
    let state = daemon.home().join("desktop-state.json");
    let (view, cx) = open(cx, &daemon, Some(state.clone()));
    wait_synced(&view, cx, 1);
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.toggle_dock(DockPosition::Bottom);
        });
    });

    // What the SIGTERM / SIGINT / SIGHUP watcher calls (`app::watch_signals`).
    view.update(cx, |wb, cx| wb.quit_leaving_daemon(cx));
    cx.run_until_parked();
    assert!(quitting(&view, cx));
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()), "never asks");
    assert!(state.exists(), "layout saved");
    assert_eq!(connection_state(&view, cx), ConnectionState::Stopped);
    pump_for(cx, Duration::from_millis(300));
    assert!(daemon.paths.socket.exists(), "the daemon is untouched");
    assert!(daemon.session(session.id).state.is_live());
}

// daemon controls ---------------------------------------------------------------------------

#[gpui::test]
fn the_connection_status_item_opens_the_popover_with_the_daemons_status(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);

    click(&view, cx, Region::StatusItem("connection"));
    assert!(view.read_with(cx, |wb, _| wb.daemon_popover_open()));
    wait_until(cx, "daemon.status", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_status().is_some())
    });
    let status = view
        .read_with(cx, |wb, _| wb.daemon_status().cloned())
        .unwrap();
    assert_eq!(status.pid, std::process::id());
    assert_eq!(status.sessions, 1);
    let epoch = view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch);
    assert_eq!(Some(status.epoch), epoch);
    draw(cx);
    for control in ["daemon-restart", "daemon-stop", "daemon-logs"] {
        assert!(
            view.read_with(cx, |wb, _| wb
                .region_bounds(Region::Popover(control))
                .is_some()),
            "{control} is offered while connected"
        );
    }
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::Popover("daemon-start"))
            .is_none()),
        "Start only appears when the daemon is down"
    );

    // Escape closes it.
    dispatch(cx, palette::Dismiss);
    assert!(!view.read_with(cx, |wb, _| wb.daemon_popover_open()));
    // Clicking the item opens it again; clicking it while open closes it (the backdrop).
    click(&view, cx, Region::StatusItem("connection"));
    assert!(view.read_with(cx, |wb, _| wb.daemon_popover_open()));
    click(&view, cx, Region::StatusItem("connection"));
    assert!(!view.read_with(cx, |wb, _| wb.daemon_popover_open()));
}

#[gpui::test]
fn open_logs_reveals_the_log_file_and_has_no_other_side_effects(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);
    let log = daemon.paths.log_file();
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    let revealed: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    view.update(cx, |wb, _| {
        wb.set_daemon_log_path(log.clone());
        let revealed = Rc::clone(&revealed);
        wb.set_reveal_handler(move |path, _| revealed.borrow_mut().push(path.to_owned()));
    });

    // No log file yet: a notice, nothing revealed.
    dispatch(cx, daemon_actions::OpenLogs);
    assert!(revealed.borrow().is_empty());
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .is_some_and(|n| n.starts_with("No daemon log yet"))
    );

    write(log.parent().unwrap(), "hutand.log", "started\n");
    dispatch(cx, daemon_actions::OpenLogs);
    pump_for(cx, Duration::from_millis(200));
    assert_eq!(*revealed.borrow(), std::slice::from_ref(&log));
    assert_eq!(read(log.parent().unwrap(), "hutand.log"), "started\n");
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
    assert!(!quitting(&view, cx));
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));
    assert_eq!(view.read_with(cx, |wb, _| wb.daemon_busy()), None);
    assert_eq!(connection_state(&view, cx), ConnectionState::Connected);
    assert!(daemon.paths.socket.exists());
}

#[gpui::test]
fn the_palette_lists_every_daemon_command(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);

    view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
    view.update(cx, |wb, cx| wb.set_palette_query("Daemon:", cx));
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.palette_titles()
            .iter()
            .map(ToString::to_string)
            .collect()
    });
    for expected in [
        "Daemon: Status",
        "Daemon: Restart Daemon",
        "Daemon: Stop Daemon",
        "Daemon: Start Daemon",
        "Daemon: Open Logs",
        "Daemon: Toggle Keep Daemon Running After Hutan Quits",
        "Daemon: Open Login Items",
    ] {
        assert!(
            titles.iter().any(|t| t == expected),
            "{expected} in {titles:?}"
        );
    }
}

#[gpui::test]
fn stop_from_the_palette_confirms_with_live_sessions_and_keeps_the_app_open(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let session = daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, session.id);
    wait_until(cx, "the terminal", |cx| {
        terminal_status(&view, cx, session.id) == Some(ViewStatus::Live)
    });

    view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
    view.update(cx, |wb, cx| wb.set_palette_query("Daemon: Stop Daemon", cx));
    cx.simulate_keystrokes("enter");
    assert_eq!(modal_title(&view, cx).as_deref(), Some("Stop the daemon?"));
    assert!(daemon.paths.socket.exists(), "not before the confirmation");
    // Cancelling leaves everything as it was.
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));

    dispatch(cx, daemon_actions::Stop);
    confirm(&view, cx);
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the stop to finish", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_busy().is_none())
    });
    assert!(!quitting(&view, cx), "the app stays open");
    assert!(view.read_with(cx, |wb, _| wb.daemon_stopped()));
    assert_eq!(status_line(&view, cx), "hutand stopped");
    assert!(
        !view.read_with(cx, |wb, cx| wb.sessions().read(cx).connection().autostart()),
        "autostart is off so the daemon is not brought back"
    );
    wait_until(cx, "the terminal to say so", |cx| {
        matches!(
            terminal_status(&view, cx, session.id),
            Some(ViewStatus::Ended(text)) if text.contains("daemon is stopped")
        )
    });
    // The menu offers Start, not Stop/Restart.
    let actions = daemon_menu(cx);
    let enabled = |name: &str| actions.iter().find(|(n, _)| n == name).map(|(_, on)| *on);
    assert_eq!(enabled("Start Daemon"), Some(true));
    assert_eq!(enabled("Stop Daemon"), Some(false));
    assert_eq!(enabled("Restart Daemon"), Some(false));
}

#[gpui::test]
fn stop_without_live_sessions_needs_no_confirmation(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);

    dispatch(cx, daemon_actions::Stop);
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
    wait_socket_gone(cx, &daemon.paths.socket);
    wait_until(cx, "the stop to finish", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_busy().is_none() && wb.daemon_stopped()
        })
    });
    // The popover shows the way back: Start (button and banner), no Restart/Stop.
    click(&view, cx, Region::StatusItem("connection"));
    draw(cx);
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::Popover("daemon-start")).is_some()
    }));
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::Popover("daemon-banner-start"))
            .is_some()
    }));
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::Popover("daemon-stop")).is_none()
    }));
}

#[gpui::test]
fn the_popover_controls_stop_restart_and_start_a_real_daemon(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    let first = process.create_session(&["sleep", "600"]);
    let settings = settings_in(&process.dir.path().join("config"));
    let connection = Connection::start(process.options());
    let (view, cx) = open_with(cx, settings, connection, None);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, first.id);
    wait_until(cx, "the terminal", |cx| {
        terminal_status(&view, cx, first.id) == Some(ViewStatus::Live)
    });
    let epoch =
        |cx: &VisualTestContext| view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch);
    let epoch_before = epoch(cx);

    // Restart from the popover: confirms because a session is live.
    click(&view, cx, Region::StatusItem("connection"));
    wait_until(cx, "daemon.status", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_status().is_some())
    });
    let pid_before = view.read_with(cx, |wb, _| wb.daemon_status().unwrap().pid);
    click(&view, cx, Region::Popover("daemon-restart"));
    assert_eq!(
        modal_title(&view, cx).as_deref(),
        Some("Restart the daemon?")
    );
    assert!(process.socket().exists(), "not before the confirmation");
    confirm(&view, cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.daemon_busy()),
        Some(DaemonBusy::Restarting)
    );
    wait_until(cx, "the new daemon", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.daemon_busy().is_none()
                && wb.sessions().read(cx).connection_state() == ConnectionState::Connected
                && wb.daemon_status().is_some()
        })
    });
    assert_ne!(epoch(cx), epoch_before, "a new daemon epoch");
    assert_ne!(
        view.read_with(cx, |wb, _| wb.daemon_status().unwrap().pid),
        pid_before,
        "a new daemon process"
    );
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));
    // The session was respawned with the same id: the open terminal follows it to its new
    // terminal instead of ending.
    wait_until(cx, "the terminal to follow the respawned session", |cx| {
        terminal_status(&view, cx, first.id) == Some(ViewStatus::Live)
            && attached_to_current_daemon(&view, cx, first.id)
    });
    assert!(view.read_with(cx, |wb, _| wb.modal().is_none()));

    // Stop with a live session in the new daemon: confirmed, then it stays down.
    let second = process.create_session(&["sleep", "600"]);
    // The restarted daemon restored the first session next to the new one.
    wait_synced(&view, cx, 2);
    open_session(&view, cx, second.id);
    wait_until(cx, "the second terminal", |cx| {
        terminal_status(&view, cx, second.id) == Some(ViewStatus::Live)
    });
    click(&view, cx, Region::Popover("daemon-stop"));
    assert_eq!(modal_title(&view, cx).as_deref(), Some("Stop the daemon?"));
    confirm(&view, cx);
    wait_socket_gone(cx, &process.socket());
    wait_until(cx, "the stop to finish", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_busy().is_none() && wb.daemon_stopped()
        })
    });
    assert_eq!(status_line(&view, cx), "hutand stopped");
    // Autostart would bring a daemon back within a few hundred milliseconds.
    pump_for(cx, Duration::from_millis(1500));
    assert!(
        !process.socket().exists(),
        "the stopped daemon stayed stopped"
    );
    assert!(matches!(
        terminal_status(&view, cx, second.id),
        Some(ViewStatus::Ended(text)) if text.contains("daemon is stopped")
    ));

    // Start from the popover: a daemon comes back and the old terminal follows its session.
    click(&view, cx, Region::Popover("daemon-start"));
    wait_until(cx, "the daemon to start again", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.daemon_busy().is_none()
                && wb.sessions().read(cx).connection_state() == ConnectionState::Connected
        })
    });
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));
    assert_eq!(status_line(&view, cx), "hutand connected");
    wait_until(
        cx,
        "the second terminal to follow its respawned session",
        |cx| {
            terminal_status(&view, cx, second.id) == Some(ViewStatus::Live)
                && attached_to_current_daemon(&view, cx, second.id)
        },
    );
    assert!(process.socket().exists());
}

#[gpui::test]
fn start_and_restart_from_the_palette_work_on_a_stopped_daemon(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    process.create_session(&["true"]);
    let connection = Connection::start(process.options());
    let (view, cx) = open_with(
        cx,
        settings_in(&process.dir.path().join("config")),
        connection,
        None,
    );
    wait_until(cx, "the connection", |cx| {
        connection_state(&view, cx) == ConnectionState::Connected
    });

    // Starting a running daemon says so and changes nothing.
    dispatch(cx, daemon_actions::Start);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .as_deref(),
        Some("The daemon is already running")
    );

    dispatch(cx, daemon_actions::Stop);
    wait_socket_gone(cx, &process.socket());
    wait_until(cx, "the stop to finish", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_busy().is_none() && wb.daemon_stopped()
        })
    });

    // Restart on a stopped daemon starts it.
    view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
    view.update(cx, |wb, cx| {
        wb.set_palette_query("Daemon: Restart Daemon", cx)
    });
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the daemon to start", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.daemon_busy().is_none()
                && wb.sessions().read(cx).connection_state() == ConnectionState::Connected
        })
    });
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));
    assert!(process.socket().exists());
}

// keep running and start at login in the popover, the menu and the settings -----------------------

/// Whether the `Daemon` menu item whose title starts with `prefix` is checked.
fn menu_checked(cx: &mut VisualTestContext, prefix: &str) -> bool {
    let menus = cx.update(|_, cx| cx.get_menus()).expect("menus are set");
    let menu = menus.iter().find(|m| m.name.as_ref() == "Daemon").unwrap();
    menu.items
        .iter()
        .find_map(|item| match item {
            OwnedMenuItem::Action { name, checked, .. } if name.starts_with(prefix) => {
                Some(*checked)
            }
            _ => None,
        })
        .unwrap()
}

#[gpui::test]
fn the_popover_switch_registers_start_at_login_and_follows_external_edits(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open_mac(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let on = |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.start_at_login_enabled());
    let checked = |cx: &mut VisualTestContext| menu_checked(cx, "Start Daemon at Login");
    assert!(!on(cx), "off by default");
    assert!(!checked(cx));

    // The switch in the popover writes settings.toml and registers the launch agent.
    click(&view, cx, Region::StatusItem("connection"));
    click(&view, cx, Region::Popover("start_at_login"));
    assert!(on(cx));
    assert_eq!(fakes.login.calls(), [LoginItemCall::Register]);
    assert!(read(daemon.home(), "settings.toml").contains("start_at_login = true"));
    assert!(checked(cx));

    // The palette and menu action flip it back.
    dispatch(cx, daemon_actions::ToggleStartAtLogin);
    assert!(!on(cx));
    assert!(read(daemon.home(), "settings.toml").contains("start_at_login = false"));
    assert!(!checked(cx));

    // An external edit shows up (hot reload) in the setting, the menu and the registration.
    write(
        daemon.home(),
        "settings.toml",
        "[app]\nstart_at_login = true\n",
    );
    wait_until(cx, "the edit to reach the menu", |cx| on(cx) && checked(cx));
    assert_eq!(
        fakes.login.calls(),
        [
            LoginItemCall::Register,
            LoginItemCall::Unregister,
            LoginItemCall::Register
        ]
    );
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::Popover("start_at_login"))
            .is_some()),
        "the popover is still open and shows the switch"
    );
}

#[gpui::test]
fn the_popover_and_menu_toggle_keep_running_without_the_system(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open_mac(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let on = |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.keep_running_enabled());
    let checked = |cx: &mut VisualTestContext| menu_checked(cx, "Keep Daemon Running");
    assert!(!on(cx) && !checked(cx), "off by default");

    click(&view, cx, Region::StatusItem("connection"));
    click(&view, cx, Region::Popover("keep_running"));
    assert!(on(cx) && checked(cx));
    let saved = read(daemon.home(), "settings.toml");
    assert!(saved.contains("keep_running = true"), "{saved}");
    assert!(!saved.contains("start_at_login = true"), "{saved}");
    assert!(fakes.login.calls().is_empty(), "nothing is registered");

    dispatch(cx, daemon_actions::ToggleKeepRunning);
    assert!(!on(cx) && !checked(cx));
    assert!(read(daemon.home(), "settings.toml").contains("keep_running = false"));
    assert!(fakes.login.calls().is_empty());
}

#[gpui::test]
fn outside_the_bundle_only_the_login_switch_is_unavailable(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);
    click(&view, cx, Region::StatusItem("connection"));
    click(&view, cx, Region::Popover("start_at_login"));
    assert!(!view.read_with(cx, |wb, _| wb.start_at_login_enabled()));
    assert_eq!(read(daemon.home(), "settings.toml"), "");

    click(&view, cx, Region::Popover("keep_running"));
    assert!(view.read_with(cx, |wb, _| wb.keep_running_enabled()));
}

#[gpui::test]
fn the_settings_sheet_lists_the_app_namespace(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);

    cx.dispatch_action(actions::OpenSettingsFor("app".into()));
    draw(cx);
    let fields: Vec<String> = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .map(|s| s.fields().iter().map(|f| f.dotted()).collect())
            .unwrap_or_default()
    });
    assert_eq!(
        fields,
        [
            "keep_running",
            "start_at_login",
            "setup_done",
            "ask_notifications",
            "ask_hooks",
            "on_last_window_close",
            "menu_bar",
            "dock_badge"
        ]
    );
}

#[gpui::test]
fn the_daemon_menu_follows_the_connection(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);
    let actions = daemon_menu(cx);
    let names: Vec<&str> = actions.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "Status…",
            "Restart Daemon",
            "Stop Daemon",
            "Start Daemon",
            "Open Logs",
            "Keep Daemon Running After Hutan Quits",
            // Outside Hutan.app (no login item here): present but disabled.
            "Start Daemon at Login (available in Hutan.app)",
            "Open Login Items"
        ]
    );
    let enabled = |name: &str, actions: &[(String, bool)]| {
        actions.iter().find(|(n, _)| n == name).map(|(_, on)| *on)
    };
    assert_eq!(enabled("Stop Daemon", &actions), Some(true));
    assert_eq!(enabled("Start Daemon", &actions), Some(false));
    assert_eq!(
        enabled("Keep Daemon Running After Hutan Quits", &actions),
        Some(true),
        "it needs no login item"
    );
    assert_eq!(
        enabled("Start Daemon at Login (available in Hutan.app)", &actions),
        Some(false)
    );
    assert_eq!(enabled("Open Login Items", &actions), Some(false));

    // A stop from elsewhere (the CLI) shows through the reconnect state.
    daemon.stop();
    wait_until(cx, "the connection to drop", |cx| {
        connection_state(&view, cx) != ConnectionState::Connected
    });
    assert!(status_line(&view, cx).starts_with("reconnecting"));
    let actions = daemon_menu(cx);
    assert_eq!(enabled("Stop Daemon", &actions), Some(false));
    assert_eq!(enabled("Start Daemon", &actions), Some(true));
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));
}
