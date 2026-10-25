//! `Connection` starts `hutand` when the socket is absent, and again when the daemon it runs
//! goes away without being asked to (a stop someone asked for is kept: `lifecycle.rs`).
//!
//! Own test binary: it sets `HUTAN_HOME` for the daemon process it spawns.

#![allow(clippy::unwrap_used, clippy::expect_used)]
// `set_var` is unsafe in edition 2024.
#![allow(unsafe_code)]

use crate::common;

use std::path::PathBuf;

use common::*;
use hutan_client::{ConnectOptions, Connection, ConnectionState};
use hutan_protocol::messages::{
    ClientInfo, ClientKind, DaemonShutdown, DaemonStatusInfo, Empty, Request,
};

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
async fn autostarts_a_missing_daemon_and_again_after_it_died() {
    let binary = hutand();
    let home = tempfile::tempdir().unwrap();
    // Stops the daemons the connection starts, also when the test fails half-way.
    let _reaper = hutan_daemon::testing::DaemonReaper::new(home.path());
    // SAFETY: this binary has a single test, so nothing else reads or writes the environment
    // while it is changed; the daemon inherits it.
    unsafe { std::env::set_var("HUTAN_HOME", home.path()) };
    let socket = home.path().join("hutand.sock");

    let connection = Connection::start(ConnectOptions {
        endpoint: Some(socket.clone().into()),
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

    // The daemon goes away unasked (a signal: no `daemon.stopping`). The connection wants it
    // back and starts a new one.
    let pid = i32::try_from(first.pid).unwrap();
    // SAFETY: a plain syscall on the pid of the hutand this test started.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    wait_reconnecting(&mut state).await;
    assert_eq!(state.borrow().stopped_by, None);
    assert!(connection.autostart());

    // The next attempt starts a daemon again: a new epoch.
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
            restart: false,
        }))
        .await
        .unwrap();
    connection.shutdown();
}
