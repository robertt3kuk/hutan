#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests of a daemon that cannot start (architecture.md § Start errors): the line under
//! the title bar with the reason and its action, the status item, the `Daemon` menu, and
//! *Start with Empty Data…* moving a newer database into `backups/` and connecting. A real
//! `hutand` process on a prepared home.

use crate::common;

use std::path::Path;

use common::{ProcessDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, OwnedMenuItem, TestAppContext, VisualTestContext};
use hutan_client::{ConnectOptions, Connection, ConnectionState};
use hutan_desktop::workbench::actions::daemon as daemon_actions;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{ClientInfo, ClientKind};
use hutan_settings::{SettingsPaths, SettingsStore};

fn settings_in(dir: &Path) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(dir))
}

fn newer_database(path: &Path, version: u32) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO app_meta (key, value) VALUES ('schema_version', '{version}');"
    ))
    .unwrap();
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

fn modal_title(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string()))
}

fn confirm(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    cx.run_until_parked();
}

fn notice(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
}

/// `(name, enabled)` of the `Daemon` menu's actions.
fn daemon_menu(cx: &mut VisualTestContext) -> Vec<(String, bool)> {
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

fn enabled(menu: &[(String, bool)], name: &str) -> Option<bool> {
    menu.iter().find(|(n, _)| n == name).map(|(_, on)| *on)
}

#[gpui::test]
fn newer_data_shows_the_reason_and_start_with_empty_data_moves_it_and_connects(
    cx: &mut TestAppContext,
) {
    let process = ProcessDaemon::new();
    let home = process.home();
    std::fs::create_dir_all(&home).unwrap();
    let database = home.join("hutan.db");
    newer_database(&database, 999);
    let supported = hutan_storage::MIGRATIONS.last().unwrap().version;
    let connection = Connection::start(process.options());
    let (view, cx) = open_with(
        cx,
        settings_in(&process.dir.path().join("config")),
        connection,
        None,
    );

    wait_until(cx, "the start failure", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.daemon_start_failure(cx)
                .is_some_and(|f| f.schema_too_new().is_some())
        })
    });
    let sentence = view.read_with(cx, |wb, cx| wb.daemon_start_failure(cx).unwrap().sentence());
    assert_eq!(
        sentence,
        format!(
            "Your Hutan data was written by a newer Hutan (database version 999; this build \
             reads up to {supported}). Update Hutan: `brew upgrade --cask hutan`."
        )
    );
    // The line under the title bar, with the update command and the way out.
    assert!(drawn(&view, cx, Region::Offer("daemon-start-failure")));
    assert!(drawn(&view, cx, Region::Offer("daemon-copy-update")));
    assert!(drawn(&view, cx, Region::Offer("daemon-empty-data")));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.daemon_status_line(cx)),
        "hutand: data from a newer Hutan"
    );
    // The Daemon menu says the same and lists the fitting actions only.
    wait_until(cx, "the menu to follow", |cx| {
        enabled(&daemon_menu(cx), "Start with Empty Data…") == Some(true)
    });
    let menu = daemon_menu(cx);
    assert_eq!(
        enabled(&menu, "Not Running: Data From a Newer Hutan"),
        Some(false)
    );
    assert_eq!(enabled(&menu, "Copy Update Command"), Some(true));
    assert_eq!(enabled(&menu, "Stop the Stuck Daemon…"), None);
    assert_eq!(enabled(&menu, "Copy Start Error Details"), Some(true));
    // And the popover.
    cx.dispatch_action(daemon_actions::Status);
    cx.run_until_parked();
    assert!(drawn(&view, cx, Region::Popover("daemon-empty-data")));
    view.update_in(cx, |wb, window, cx| wb.close_daemon_popover(window, cx));

    // Start with Empty Data… asks first; nothing moves before the answer.
    click(&view, cx, Region::Offer("daemon-empty-data"));
    assert_eq!(
        modal_title(&view, cx).as_deref(),
        Some("Start with empty data?")
    );
    assert!(database.exists());
    confirm(&view, cx);

    wait_until(cx, "the daemon on empty data", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).connection_state() == ConnectionState::Connected
                && wb.daemon_busy().is_none()
        })
    });
    let backups: Vec<_> = std::fs::read_dir(home.join("backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        backups.iter().any(|name| name.ends_with("-v999-newer.db")),
        "{backups:?}"
    );
    assert!(
        database.exists(),
        "the new daemon made a fresh database in its place"
    );
    assert!(
        notice(&view, cx)
            .unwrap_or_default()
            .starts_with("Your data was moved to "),
        "{:?}",
        notice(&view, cx)
    );
    assert!(!drawn(&view, cx, Region::Offer("daemon-start-failure")));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.daemon_start_failure(cx)),
        None
    );
    assert!(!home.join("hutand-start-error.json").exists());
    assert_eq!(enabled(&daemon_menu(cx), "Start with Empty Data…"), None);
}

#[gpui::test]
fn a_stuck_daemon_is_shown_and_only_a_hutand_is_ever_stopped(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hutand.sock");
    // What a second hutand records when the lock's holder does not answer; the "holder" here
    // is this test process, which Stop It must refuse to signal.
    let me = std::process::id();
    std::fs::write(
        dir.path().join("hutand-start-error.json"),
        format!(
            r#"{{"at_ms":1,"pid":1,"version":"0.2.0","kind":"already_running",
                "holder_pid":{me},"message":"another hutand is already running"}}"#
        ),
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
        settings_in(&dir.path().join("config")),
        connection,
        None,
    );

    wait_until(cx, "the recorded failure", |cx| {
        view.read_with(cx, |wb, cx| wb.daemon_start_failure(cx).is_some())
    });
    assert!(drawn(&view, cx, Region::Offer("daemon-show-stuck")));
    assert!(drawn(&view, cx, Region::Offer("daemon-stop-stuck")));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.daemon_start_failure(cx).unwrap().sentence()),
        format!(
            "Another hutand (pid {me}) holds the lock but does not answer on {}.",
            dir.path().join("hutand.sock").display()
        )
    );

    click(&view, cx, Region::Offer("daemon-show-stuck"));
    let info = view.read_with(cx, |wb, _| wb.stuck_daemon_info().map(str::to_owned));
    let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
    assert_eq!(info, Some(format!("pid {me}: {}", exe.display())));

    click(&view, cx, Region::Offer("daemon-stop-stuck"));
    assert_eq!(
        modal_title(&view, cx),
        Some(format!("Stop hutand (pid {me})?"))
    );
    confirm(&view, cx);
    assert_eq!(
        notice(&view, cx),
        Some(format!(
            "pid {me} is not a running hutand; nothing was stopped"
        ))
    );
}
