//! A `hutand` that cannot start says why: stderr, `hutand.log` and the start-error record next
//! to the socket (architecture.md § Start errors); a successful start removes the record. Runs
//! the real binary on temporary homes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use hutan_client::{
    Client, ConnectOptions, DaemonFiles, StartErrorKind, StartErrorRecord, StartFailure,
};
use hutan_daemon::Paths;
use hutan_daemon::start_error::{START_ERROR_FILE, record_path};
use hutan_daemon::testing::DaemonReaper;
use hutan_protocol::messages::{ClientInfo, ClientKind};
use tokio::time::{sleep, timeout};

use crate::common::WAIT;

/// The newest schema this build understands.
fn supported() -> u32 {
    hutan_storage::MIGRATIONS.last().map_or(0, |m| m.version)
}

/// A database that says a newer Hutan wrote it (what `PRAGMA user_version` would fake
/// elsewhere: Hutan keeps its schema version in `app_meta`).
pub fn newer_database(path: &Path, version: u32) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO app_meta (key, value) VALUES ('schema_version', '{version}');"
    ))
    .unwrap();
}

fn hutand(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hutand"));
    command
        .env("HUTAN_HOME", home)
        .env_remove("HUTAN_SOCKET")
        .env_remove("HUTAN_LOG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null());
    command
}

/// Runs `hutand` to its end (it fails at once in these tests).
async fn run_to_end(home: &Path) -> Output {
    let mut command = tokio::process::Command::from(hutand(home));
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    timeout(WAIT, command.output())
        .await
        .expect("hutand exited")
        .unwrap()
}

fn read_record(paths: &Paths) -> StartErrorRecord {
    let text = std::fs::read_to_string(record_path(paths)).expect("a start-error record");
    serde_json::from_str(&text).unwrap()
}

fn options(paths: &Paths) -> ConnectOptions {
    ConnectOptions {
        endpoint: Some(paths.socket.clone().into()),
        autostart: None,
        client: ClientInfo {
            kind: ClientKind::Cli,
            version: "test".into(),
            origin: None,
        },
    }
}

async fn wait_connectable(paths: &Paths) -> Client {
    timeout(WAIT, async {
        loop {
            match Client::connect(options(paths)).await {
                Ok(client) => return client,
                Err(_) => sleep(Duration::from_millis(25)).await,
            }
        }
    })
    .await
    .expect("hutand started")
}

async fn wait_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    timeout(WAIT, async {
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("hutand exited")
}

fn terminate(child: &std::process::Child) {
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_on_a_newer_database_exits_and_says_why_in_the_log_and_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let paths = Paths::in_home(&home);
    let _reaper = DaemonReaper::new(&home);
    std::fs::create_dir_all(&home).unwrap();
    let found = supported() + 1;
    newer_database(&paths.database, found);

    let out = run_to_end(&home).await;
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("written by a newer Hutan"), "{stderr}");
    assert!(!paths.socket.exists(), "never served");

    // The log has the whole chain.
    let log = std::fs::read_to_string(paths.log_file()).unwrap();
    assert!(log.contains("hutand could not start"), "{log}");
    assert!(
        log.contains(&format!("schema version {found}")) && log.contains("written by a newer"),
        "{log}"
    );

    // The record, as the clients read it.
    let record = read_record(&paths);
    assert_eq!(
        record.kind,
        StartErrorKind::SchemaTooNew {
            found,
            supported: supported(),
            db: paths.database.clone(),
        }
    );
    assert_eq!(record.version, env!("CARGO_PKG_VERSION"));
    assert!(record.message.contains("newer Hutan"), "{}", record.message);
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(START_ERROR_FILE)).unwrap())
            .unwrap();
    assert_eq!(raw["kind"], "schema_too_new");
    assert_eq!(raw["found"], found);
    assert_eq!(raw["supported"], supported());
    let failure = StartFailure::recorded(&DaemonFiles::for_socket(&paths.socket)).unwrap();
    assert!(
        failure
            .sentence()
            .starts_with("Your Hutan data was written by a newer Hutan"),
        "{}",
        failure.sentence()
    );
    // Nothing was touched: the database is still the newer one.
    assert!(paths.database.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_successful_start_removes_the_record_and_a_second_daemon_records_the_holder() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let paths = Paths::in_home(&home);
    let _reaper = DaemonReaper::new(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        record_path(&paths),
        r#"{"at_ms":1,"pid":1,"version":"0.0.1","kind":"other","message":"stale"}"#,
    )
    .unwrap();

    let mut first = hutand(&home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let client = wait_connectable(&paths).await;
    assert!(
        !record_path(&paths).exists(),
        "a successful start removes the record"
    );

    // A second daemon on the same home finds the lock held: exit 2, and a record naming the
    // first one.
    let second = run_to_end(&home).await;
    assert_eq!(second.status.code(), Some(2), "{second:?}");
    let record = read_record(&paths);
    match record.kind {
        StartErrorKind::AlreadyRunning { holder_pid, lock } => {
            assert_eq!(holder_pid, Some(first.id()));
            assert_eq!(lock.as_deref(), Some(paths.lock.as_path()));
        }
        other => panic!("expected already_running, got {other:?}"),
    }
    drop(client);

    // The first one stops normally and takes that record with it.
    terminate(&first);
    assert!(wait_exit(&mut first).await.success());
    assert!(!record_path(&paths).exists());
}
