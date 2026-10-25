//! `Connection::set_autostart`: a daemon stopped while autostart is off stays down, and turning
//! autostart on starts it at once, inside the 10 s autostart interval.
//!
//! Own test binary: it sets `HUTAN_HOME` for the daemon process it spawns.

#![allow(clippy::unwrap_used, clippy::expect_used)]
// `set_var` is unsafe in edition 2024.
#![allow(unsafe_code)]

use crate::common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::*;
use hutan_client::{ConnectOptions, Connection, ConnectionState};
use hutan_daemon::{DaemonConfig, Paths};
use hutan_protocol::messages::{
    ClientInfo, ClientKind, DaemonShutdown, DaemonStatusInfo, Empty, Request,
};
use tokio::time::{sleep, timeout};

/// `hutand` next to the test binaries (same profile). Tests never build it: `just test`
/// builds the workspace binaries first.
fn hutand() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(|deps| deps.parent()).unwrap();
    let binary = profile_dir.join("hutand");
    assert!(
        binary.is_file(),
        "hutand not built: run `cargo build -p hutan-daemon --bin hutand` (or `just test`)"
    );
    binary
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_daemon_stays_down_until_autostart_is_turned_on_again() {
    let binary = hutand();
    let home = tempfile::tempdir().unwrap();
    // Stops the daemon the connection starts, also when the test fails half-way.
    let _reaper = hutan_daemon::testing::DaemonReaper::new(home.path());
    // SAFETY: this binary has a single test, so nothing else reads or writes the environment
    // while it is changed; the spawned daemon inherits it.
    unsafe { std::env::set_var("HUTAN_HOME", home.path()) };
    let paths = Paths::in_home(home.path());
    // In this process, so the connection has not used its autostart interval yet.
    let first = hutan_daemon::start(DaemonConfig::new(paths.clone()))
        .await
        .unwrap();

    let connection = Connection::start(ConnectOptions {
        endpoint: Some(paths.socket.clone().into()),
        autostart: Some(binary),
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: "test".into(),
            origin: None,
        },
    });
    assert!(connection.autostart());
    let mut state = connection.state();
    let first_epoch = wait_connected(&mut state).await.epoch;

    connection.set_autostart(false);
    assert!(!connection.autostart());
    connection
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: true,
            forget: false,
            restart: false,
        }))
        .await
        .unwrap();
    timeout(WAIT, first.wait()).await.unwrap().unwrap();
    wait_reconnecting(&mut state).await;

    // With autostart on, the first reconnect attempt (100 ms) would have started a daemon.
    sleep(Duration::from_millis(1500)).await;
    assert!(
        !paths.socket.exists(),
        "hutand was started although autostart is off"
    );
    assert!(matches!(
        state.borrow().connection,
        ConnectionState::Reconnecting { .. }
    ));

    // Turning it on does not wait for the backoff: the daemon is back well before 5 s.
    let asked = Instant::now();
    connection.set_autostart(true);
    let after = wait_connected(&mut state).await;
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "reconnected after {:?}",
        asked.elapsed()
    );
    assert_ne!(after.epoch, first_epoch, "a new daemon, not the old one");
    let status: DaemonStatusInfo = connection
        .call(Request::DaemonStatus(Empty {}))
        .await
        .unwrap();
    assert_ne!(status.pid, std::process::id(), "spawned as its own process");

    connection
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: true,
            forget: false,
            restart: false,
        }))
        .await
        .unwrap();
    connection.shutdown();
}
