//! `Connection::start_held`: a connection that only dials until autostart is allowed, so a
//! daemon that launchd starts ("keep running") is not raced by a second one the first dial
//! would spawn. The "daemon" here is a script that leaves a marker file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use hutan_client::{ConnectOptions, Connection};
use hutan_protocol::messages::{ClientInfo, ClientKind};
use tokio::time::sleep;

#[tokio::test(flavor = "multi_thread")]
async fn a_held_connection_dials_but_never_spawns_until_it_is_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("spawned");
    let script = dir.path().join("hutand");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let connection = Connection::start_held(ConnectOptions {
        endpoint: Some((dir.path().join("hutand.sock")).into()),
        autostart: Some(script),
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: "test".into(),
            origin: None,
        },
    });
    assert!(!connection.autostart(), "held from the first dial");

    // Several dial attempts (100 ms, 200 ms, 400 ms backoff) pass without a spawn.
    sleep(Duration::from_millis(1200)).await;
    assert!(!marker.exists(), "a held connection started the daemon");

    connection.set_autostart(true);
    let asked = Instant::now();
    while !marker.exists() {
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "allowing autostart never started the daemon"
        );
        sleep(Duration::from_millis(20)).await;
    }
    connection.shutdown();
}
