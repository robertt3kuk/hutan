//! `Connection` starts `hutand` when the socket is absent, and not more than once per 10 s.
//!
//! Own test binary: it sets `HUTAN_HOME` for the daemon process it spawns.

#![allow(clippy::unwrap_used, clippy::expect_used)]
// `set_var` is unsafe in edition 2024.
#![allow(unsafe_code)]

use crate::common;

use std::path::PathBuf;
use std::time::Duration;

use common::*;
use hutan_client::{ConnectOptions, Connection, ConnectionState};
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
async fn autostarts_a_missing_daemon_at_most_once_per_interval() {
    let binary = hutand();
    let home = tempfile::tempdir().unwrap();
    // SAFETY: this binary has a single test, so nothing else reads or writes the environment
    // while it is changed; the daemon inherits it.
    unsafe { std::env::set_var("HUTAN_HOME", home.path()) };
    let socket = home.path().join("hutand.sock");

    let connection = Connection::start(ConnectOptions {
        socket: Some(socket.clone()),
        autostart: Some(binary),
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: "test".into(),
            origin: None,
        },
    });
    let mut state = connection.state();
    wait_connected(&mut state).await;
    let first: DaemonStatusInfo = connection
        .call(Request::DaemonStatus(Empty {}))
        .await
        .unwrap();

    // Stop the daemon it started. The connection wants it back, but the interval since the
    // last autostart has not passed, so it must not be started again.
    connection
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: true,
            forget: false,
        }))
        .await
        .unwrap();
    wait_reconnecting(&mut state).await;
    timeout(WAIT, async {
        while socket.exists() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the daemon never removed its socket");
    sleep(Duration::from_millis(2500)).await;
    assert!(!socket.exists(), "autostarted again within the interval");
    assert!(matches!(
        state.borrow().connection,
        ConnectionState::Reconnecting { .. }
    ));

    // After the interval the next attempt starts a daemon again: a new epoch.
    let state_after = wait_state(&mut state, "second autostart", |s| {
        s.connection == ConnectionState::Connected
    })
    .await;
    assert_ne!(state_after.epoch, Some(first.epoch));
    let second: DaemonStatusInfo = connection
        .call(Request::DaemonStatus(Empty {}))
        .await
        .unwrap();
    assert_eq!(state_after.epoch, Some(second.epoch));

    connection
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: true,
            forget: false,
        }))
        .await
        .unwrap();
    connection.shutdown();
}
