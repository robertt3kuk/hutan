#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests of the app managing its daemon (architecture.md § Daemon lifecycle): a daemon of
//! another build is restarted onto the app's own `hutand` when idle and asked about otherwise,
//! a newer one asks for an app update, one from another executable is named and can be
//! replaced, autostarts that keep failing stop at the *Retry* banner, and a stop the user asked
//! for elsewhere (the CLI, the daemon's menu bar item) keeps the window from starting one until
//! *Start* or a relaunch. Real `hutand` processes on temporary homes; the app's own build is
//! set by the test (`WorkbenchOptions::with_own_daemon`).

use crate::common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{ProcessDaemon, TestDaemon, draw, open_options, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_client::identity::{Mismatch, OWN_VERSION};
use hutan_client::{Connection, ConnectionState, DaemonIdentity};
use hutan_daemon::menu_bar::MenuChoice;
use hutan_daemon::menu_bar::fake::{FakeLauncher, FakeStatusItem, menu_bar};
use hutan_desktop::workbench::{Region, Workbench, WorkbenchOptions};
use hutan_protocol::messages::{ClientKind, DaemonShutdown, Empty, Request, StopBy};
use hutan_settings::{SettingsPaths, SettingsStore};

fn settings_in(dir: &Path) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(dir))
}

/// The app's own `hutand`: the one the launcher starts, of `version` (the real daemon reports
/// this crate's version, so another `version` makes it "another build").
fn own(process: &ProcessDaemon, version: &str) -> DaemonIdentity {
    DaemonIdentity {
        version: version.to_owned(),
        ..DaemonIdentity::own(Some(&process.hutand()))
    }
}

/// A window on `connection` whose own `hutand` is `own`.
fn open<'a>(
    cx: &'a mut TestAppContext,
    process: &ProcessDaemon,
    connection: Connection,
    own: DaemonIdentity,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let options = WorkbenchOptions::builtin(
        settings_in(&process.dir.path().join("config")),
        connection,
        None,
    )
    .with_own_daemon(own);
    open_options(cx, options)
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    draw(cx);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} was not drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn notice(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
}

fn mismatch(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<(Mismatch, String)> {
    view.read_with(cx, |wb, _| {
        wb.daemon_mismatch().map(|m| (m.kind.clone(), m.sentence()))
    })
}

/// The build that answers, once connected and nothing is under way.
fn settled(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<DaemonIdentity> {
    view.read_with(cx, |wb, cx| {
        let state = wb.sessions().read(cx).state();
        (state.connection == ConnectionState::Connected && wb.daemon_busy().is_none())
            .then(|| state.daemon.clone())
            .flatten()
    })
}

fn status_line(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, cx| wb.daemon_status_line(cx))
}

/// Keeps pumping GPUI for `span` of real time, so whatever would still happen does.
fn pump_for(cx: &mut VisualTestContext, span: Duration) {
    let until = Instant::now() + span;
    while Instant::now() < until {
        draw(cx);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[gpui::test]
fn an_idle_daemon_of_another_build_is_restarted_onto_the_apps_hutand(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    let own = own(&process, "99.0.0");
    let (view, cx) = open(
        cx,
        &process,
        Connection::start(process.options()),
        own.clone(),
    );

    wait_until(cx, "the restart onto the app's hutand", |cx| {
        process.starts() == 2 && settled(&view, cx).is_some()
    });
    let running = settled(&view, cx).unwrap();
    assert_eq!(running.exe_path, own.exe_path, "the app's own hutand runs");
    assert_eq!(
        notice(&view, cx).as_deref(),
        Some("Updated the background service to 99.0.0")
    );
    // The hutand beside this app still reports its own build: it is kept, not restarted again
    // and again, and nothing is asked.
    pump_for(cx, Duration::from_millis(500));
    assert_eq!(process.starts(), 2);
    assert_eq!(mismatch(&view, cx), None);
    assert!(!drawn(&view, cx, Region::Offer("daemon-mismatch")));
}

#[gpui::test]
fn with_a_live_session_another_build_is_restarted_only_on_restart_now(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    let session = process.create_session(&["sleep", "600"]);
    assert_eq!(process.starts(), 1);
    let own = own(&process, "99.0.0");
    let (view, cx) = open(
        cx,
        &process,
        Connection::start(process.options()),
        own.clone(),
    );

    wait_until(cx, "the question", |cx| mismatch(&view, cx).is_some());
    let first = settled(&view, cx).unwrap();
    assert_eq!(
        mismatch(&view, cx),
        Some((
            Mismatch::Different,
            format!(
                "The background service still runs Hutan {OWN_VERSION} (from {}). Restart it \
                 with 99.0.0? Running terminals are restored; agent turns in progress are \
                 interrupted.",
                process.hutand().display()
            )
        ))
    );
    assert!(drawn(&view, cx, Region::Offer("daemon-mismatch")));
    assert!(drawn(&view, cx, Region::Offer("daemon-mismatch-dismiss")));
    // Nothing happens until the user says so.
    pump_for(cx, Duration::from_millis(500));
    assert_eq!(process.starts(), 1);
    assert_eq!(
        settled(&view, cx).unwrap().started_at_ms,
        first.started_at_ms
    );

    click(&view, cx, Region::Offer("daemon-mismatch-restart"));
    wait_until(cx, "the app's hutand", |cx| {
        process.starts() == 2 && settled(&view, cx).is_some()
    });
    assert_eq!(settled(&view, cx).unwrap().exe_path, own.exe_path);
    assert_eq!(
        notice(&view, cx).as_deref(),
        Some("Updated the background service to 99.0.0")
    );
    assert_eq!(mismatch(&view, cx), None);
    // The running terminal is restored by the new daemon.
    wait_until(cx, "the session in the new daemon", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .sessions()
                .iter()
                .any(|s| s.id == session.id && s.state.is_live())
        })
    });
}

#[gpui::test]
fn a_newer_daemon_asks_to_update_the_app_and_is_left_running(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    let own = own(&process, "0.0.1");
    let (view, cx) = open(cx, &process, Connection::start(process.options()), own);

    wait_until(cx, "the update notice", |cx| mismatch(&view, cx).is_some());
    assert_eq!(
        mismatch(&view, cx),
        Some((
            Mismatch::Newer,
            format!(
                "The background service runs Hutan {OWN_VERSION}, which is newer than this app \
                 (0.0.1): update the app."
            )
        ))
    );
    assert!(drawn(&view, cx, Region::Offer("daemon-mismatch-update")));
    pump_for(cx, Duration::from_millis(500));
    assert_eq!(process.starts(), 1, "a newer daemon is never replaced");

    // Later: the line goes, the daemon stays.
    click(&view, cx, Region::Offer("daemon-mismatch-dismiss"));
    assert_eq!(mismatch(&view, cx), None);
    assert!(!drawn(&view, cx, Region::Offer("daemon-mismatch")));
    assert_eq!(process.starts(), 1);
    assert!(settled(&view, cx).is_some());
}

#[gpui::test]
fn a_daemon_from_elsewhere_is_named_and_use_this_apps_service_switches(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    // Another install's daemon: the same build, copied elsewhere.
    let elsewhere = tempfile::tempdir().unwrap();
    let other = elsewhere.path().join("hutand");
    std::fs::copy(process.hutand(), &other).unwrap();
    let other = other.canonicalize().unwrap();
    process.spawn_from(&other);
    let own = own(&process, OWN_VERSION);
    let (view, cx) = open(
        cx,
        &process,
        Connection::start(process.options()),
        own.clone(),
    );

    wait_until(cx, "the line naming it", |cx| mismatch(&view, cx).is_some());
    assert_eq!(
        mismatch(&view, cx),
        Some((
            Mismatch::Foreign {
                path: other.clone()
            },
            format!("Connected to a Hutan service from {}.", other.display())
        ))
    );
    assert_eq!(settled(&view, cx).unwrap().exe_path, Some(other));
    assert_eq!(process.starts(), 1, "only the other one runs");

    click(&view, cx, Region::Offer("daemon-mismatch-restart"));
    wait_until(cx, "the app's hutand", |cx| {
        process.starts() == 2 && settled(&view, cx).is_some()
    });
    assert_eq!(settled(&view, cx).unwrap().exe_path, own.exe_path);
    assert_eq!(
        notice(&view, cx).as_deref(),
        Some("Using this app's background service")
    );
    assert_eq!(mismatch(&view, cx), None);
}

#[gpui::test]
fn three_failing_starts_stop_and_the_banner_offers_retry(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    // Data from a newer Hutan: every start fails the same way.
    let home = process.home();
    std::fs::create_dir_all(&home).unwrap();
    let database = home.join("hutan.db");
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute_batch(
        "CREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO app_meta (key, value) VALUES ('schema_version', '999');",
    )
    .unwrap();
    drop(conn);
    let own = own(&process, OWN_VERSION);
    let (view, cx) = open(cx, &process, Connection::start(process.options()), own);

    let halted = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().autostart_halted)
    };
    wait_until(cx, "autostart to give up", |cx| halted(cx));
    assert_eq!(process.starts(), hutan_client::AUTOSTART_GIVE_UP as usize);
    assert!(drawn(&view, cx, Region::Offer("daemon-start-failure")));
    assert!(drawn(&view, cx, Region::Offer("daemon-retry")));
    // It keeps quiet: no more starts.
    pump_for(cx, Duration::from_secs(2));
    assert_eq!(process.starts(), hutan_client::AUTOSTART_GIVE_UP as usize);

    // The cause gone, Retry starts it.
    std::fs::remove_file(&database).unwrap();
    click(&view, cx, Region::Offer("daemon-retry"));
    wait_until(cx, "the daemon after Retry", |cx| {
        settled(&view, cx).is_some()
    });
    assert!(!halted(cx));
    assert_eq!(
        process.starts(),
        hutan_client::AUTOSTART_GIVE_UP as usize + 1
    );
    assert!(!drawn(&view, cx, Region::Offer("daemon-retry")));
}

#[gpui::test]
fn hutan_daemon_stop_is_kept_and_a_relaunch_starts_the_daemon(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    let own = own(&process, OWN_VERSION);
    let connection = Connection::start(process.options());
    let (view, cx) = open(cx, &process, connection.clone(), own.clone());
    wait_until(cx, "the daemon", |cx| settled(&view, cx).is_some());
    assert_eq!(process.starts(), 1);

    // `hutan daemon stop`.
    let _: Empty = process.call_as(
        ClientKind::Cli,
        Request::DaemonShutdown(DaemonShutdown::default()),
    );
    wait_until(cx, "the stopped state", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_stopped())
    });
    assert_eq!(status_line(&view, cx), "Stopped by you · Start");
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().stopped_by),
        Some(StopBy::Cli)
    );
    assert!(!connection.autostart());
    // Thirty seconds on the window's clock (and a few real dials of the connection) start
    // nothing.
    for _ in 0..30 {
        cx.executor().advance_clock(Duration::from_secs(1));
        draw(cx);
    }
    pump_for(cx, Duration::from_secs(2));
    assert_eq!(process.starts(), 1, "nothing was started");
    assert!(!process.socket().exists());
    assert_eq!(status_line(&view, cx), "Stopped by you · Start");

    // A relaunch: the app needs its daemon and starts it.
    connection.shutdown();
    let (again, cx) = open(
        &mut *cx,
        &process,
        Connection::start(process.options()),
        own,
    );
    wait_until(cx, "the relaunched app's daemon", |cx| {
        settled(&again, cx).is_some()
    });
    assert_eq!(process.starts(), 2);
    assert!(!again.read_with(cx, |wb, _| wb.daemon_stopped()));
}

#[gpui::test]
fn stop_daemon_in_the_daemons_menu_bar_item_is_kept(cx: &mut TestAppContext) {
    let item = FakeStatusItem::new();
    let launcher = FakeLauncher::new();
    let bar = menu_bar(
        &item,
        &launcher,
        Duration::from_millis(50),
        Duration::from_secs(5),
    );
    let dir = tempfile::tempdir().unwrap();
    let daemon = TestDaemon::start_with_codex_tweaked(
        hutan_agents::CodexConfig {
            program: dir.path().join("no-such-codex"),
            args: Vec::new(),
            env: Vec::new(),
        },
        move |c| c.menu_bar = Some(bar),
    );
    let connection = daemon.connection();
    let options = WorkbenchOptions::builtin(settings_in(daemon.home()), connection.clone(), None);
    let (view, cx) = open_options(cx, options);
    wait_until(cx, "the daemon", |cx| settled(&view, cx).is_some());

    item.click(MenuChoice::StopDaemon);
    wait_until(cx, "the stopped state", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_stopped())
    });
    assert_eq!(status_line(&view, cx), "Stopped by you · Start");
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().stopped_by),
        Some(StopBy::MenuBar)
    );
    assert!(!connection.autostart(), "the window starts nothing");
    connection.shutdown();
}
