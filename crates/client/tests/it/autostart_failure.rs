//! An autostarted `hutand` that cannot start: the error carries its start-error record (or
//! what it printed to stderr), the connection publishes it, and "start with empty data" moves
//! the refused database aside so the next start works.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::{WAIT, wait_connected, wait_state};

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_client::{
    Client, ClientError, ConnectOptions, Connection, DaemonFiles, StartErrorKind, StartOutcome,
    move_data_aside,
};
use hutan_daemon::testing::DaemonReaper;
use hutan_protocol::messages::{ClientInfo, ClientKind, DaemonShutdown, Empty, Request};
use tokio::time::{sleep, timeout};

fn hutand() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let binary = exe
        .parent()
        .and_then(|deps| deps.parent())
        .unwrap()
        .join("hutand");
    assert!(
        binary.is_file(),
        "hutand not built: run `cargo build -p hutan-daemon --bin hutand` (or `just test`)"
    );
    binary
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// `hutand` pinned to `home` (`ConnectOptions` carries no environment).
fn wrapper(dir: &Path, home: &Path) -> PathBuf {
    script(
        dir,
        "hutand-wrapper.sh",
        &format!(
            "export HUTAN_HOME='{}'\nexec '{}'",
            home.display(),
            hutand().display()
        ),
    )
}

fn options(socket: &Path, autostart: PathBuf) -> ConnectOptions {
    ConnectOptions {
        socket: Some(socket.to_owned()),
        autostart: Some(autostart),
        client: ClientInfo {
            kind: ClientKind::Cli,
            version: "test".into(),
            origin: None,
        },
    }
}

fn newer_database(path: &Path, version: u32) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO app_meta (key, value) VALUES ('schema_version', '{version}');"
    ))
    .unwrap();
}

async fn connect_failure(options: ConnectOptions) -> hutan_client::StartFailure {
    let result = timeout(WAIT, Client::connect(options))
        .await
        .expect("the autostart ended");
    match result {
        Err(ClientError::DaemonStart(failure)) => *failure,
        Err(other) => panic!("expected a start failure, got {other}"),
        Ok(_) => panic!("a daemon came up"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_database_fails_the_autostart_with_the_record_until_the_data_is_moved_aside() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _reaper = DaemonReaper::new(&home);
    let socket = home.join("hutand.sock");
    let database = home.join("hutan.db");
    newer_database(&database, 999);
    let wrapper = wrapper(dir.path(), &home);

    let failure = connect_failure(options(&socket, wrapper.clone())).await;
    assert_eq!(failure.outcome, StartOutcome::Exited { code: Some(1) });
    let Some((db, found, supported)) = failure.schema_too_new() else {
        panic!("expected schema_too_new: {failure:?}");
    };
    assert_eq!((db, found), (database.as_path(), 999));
    assert!(supported < 999);
    assert_eq!(
        failure.sentence(),
        format!(
            "Your Hutan data was written by a newer Hutan (database version 999; this build \
             reads up to {supported}). Update Hutan: `brew upgrade --cask hutan`."
        )
    );
    // What the daemon printed was kept, in the home's log directory.
    assert_eq!(failure.start_log, home.join("logs/hutand-start.log"));
    let printed = failure.stderr_tail.clone().unwrap_or_default();
    assert!(printed.contains("newer Hutan"), "{printed}");

    // A connection publishes the same failure while nothing answers.
    let connection = Connection::start(options(&socket, wrapper.clone()));
    let mut state = connection.state();
    let failed = wait_state(&mut state, "the start failure", |s| {
        s.daemon_start
            .as_ref()
            .is_some_and(|f| matches!(f.kind(), Some(StartErrorKind::SchemaTooNew { .. })))
    })
    .await;
    assert!(failed.connection != hutan_client::ConnectionState::Connected);

    // Start with empty data: the database goes to backups/, the next start works and the
    // failure is gone.
    let files = DaemonFiles::for_socket(&socket);
    let moved = move_data_aside(&files, failure.record.as_ref()).unwrap();
    assert!(moved.backup.starts_with(home.join("backups")));
    assert!(moved.backup.is_file() && !database.exists());
    connection.start_daemon_now();
    let connected = wait_connected(&mut state).await;
    assert_eq!(connected.daemon_start, None);
    assert!(!files.start_error().exists());
    connection
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: true,
            forget: false,
            restart: false,
        }))
        .await
        .unwrap();
    connection.shutdown();
    timeout(WAIT, async {
        while socket.exists() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the daemon stopped");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_record_the_failure_carries_what_the_daemon_printed() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hutand.sock");
    let broken = script(
        dir.path(),
        "hutand",
        "echo 'starting' >&2\necho 'hutand: cannot open the frobnicator' >&2\nexit 3",
    );

    let failure = connect_failure(options(&socket, broken)).await;
    assert_eq!(failure.outcome, StartOutcome::Exited { code: Some(3) });
    assert_eq!(failure.record, None);
    assert_eq!(
        failure.sentence(),
        "hutand could not start (exit code 3): hutand: cannot open the frobnicator"
    );
    assert_eq!(
        failure.stderr_tail.as_deref(),
        Some("starting\nhutand: cannot open the frobnicator")
    );
    assert_eq!(failure.start_log, dir.path().join("logs/hutand-start.log"));

    // A stale record from an earlier start is not taken for this one.
    std::fs::write(
        dir.path().join("hutand-start-error.json"),
        r#"{"at_ms":1,"pid":1,"version":"0.0.1","kind":"other","message":"old"}"#,
    )
    .unwrap();
    let failure = connect_failure(options(
        &socket,
        script(dir.path(), "hutand2", "echo 'again' >&2\nexit 4"),
    ))
    .await;
    assert_eq!(failure.record, None);
    assert_eq!(
        failure.stderr_tail.as_deref(),
        Some("again"),
        "the start log is truncated at each autostart"
    );
}
