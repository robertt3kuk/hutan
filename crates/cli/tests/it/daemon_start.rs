//! `hutan daemon start` on data a newer Hutan wrote: the reason, a non-zero exit, `daemon
//! status` / `daemon why` showing the record, and `daemon reset-data --yes` moving the data
//! aside so the next start works. A real `hutand` beside the `hutan` binary, temporary home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use crate::common::{HUTAN, command, finish, hutand_dir};
use hutan_daemon::testing::DaemonReaper;

fn newer_database(path: &Path, version: u32) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO app_meta (key, value) VALUES ('schema_version', '{version}');"
    ))
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_database_is_explained_and_reset_data_recovers() {
    hutand_dir();
    let home = tempfile::tempdir().unwrap();
    let _guard = DaemonReaper::new(home.path());
    let database = home.path().join("hutan.db");
    newer_database(&database, 999);
    let supported = hutan_storage::MIGRATIONS.last().unwrap().version;
    let run = |args: &'static [&'static str]| {
        let mut cmd = command(Path::new(HUTAN), None, args);
        cmd.env("HUTAN_HOME", home.path());
        finish(cmd)
    };
    let sentence = format!(
        "Your Hutan data was written by a newer Hutan (database version 999; this build reads \
         up to {supported}). Update Hutan: `brew upgrade --cask hutan`."
    );

    let start = run(&["daemon", "start"]).await;
    assert_eq!(start.code, 3, "{start:?}");
    assert!(start.stderr.contains(&sentence), "{}", start.stderr);
    assert!(
        start.stderr.contains("hutan daemon why"),
        "{}",
        start.stderr
    );
    assert!(!home.path().join("hutand.sock").exists());

    let status = run(&["daemon", "status"]).await;
    assert_eq!(status.code, 3, "{status:?}");
    assert!(
        status
            .stderr
            .contains(&format!("The last start failed: {sentence}")),
        "{}",
        status.stderr
    );
    let status = run(&["daemon", "status", "--json"]).await;
    let json = status.json();
    assert_eq!(json["running"], false);
    assert_eq!(json["start_error"]["kind"], "schema_too_new");
    assert_eq!(json["start_error"]["found"], 999);
    assert_eq!(json["start_error"]["supported"], supported);

    let why = run(&["daemon", "why"]).await;
    assert_eq!(why.code, 0, "{why:?}");
    assert!(
        why.stdout.contains("hutand is not running"),
        "{}",
        why.stdout
    );
    assert!(why.stdout.contains(&sentence), "{}", why.stdout);
    assert!(
        why.stdout.contains("hutan daemon reset-data --yes"),
        "{}",
        why.stdout
    );
    assert!(
        why.stdout.contains("hutand could not start"),
        "the log's end: {}",
        why.stdout
    );
    let why = run(&["daemon", "why", "--json"]).await.json();
    assert_eq!(why["start_error"]["kind"], "schema_too_new");
    assert_eq!(why["sentence"], sentence.as_str());

    // Nothing moves without --yes.
    let refused = run(&["daemon", "reset-data"]).await;
    assert_eq!(refused.code, 2, "{refused:?}");
    assert!(database.exists());

    let reset = run(&["daemon", "reset-data", "--yes", "--json"]).await;
    assert_eq!(reset.code, 0, "{reset:?}");
    let backup = reset.json()["backup"].as_str().unwrap().to_owned();
    assert!(
        backup.starts_with(&home.path().join("backups").display().to_string())
            && backup.ends_with("-v999-newer.db"),
        "{backup}"
    );
    assert!(Path::new(&backup).is_file());
    assert!(!database.exists());

    let started = run(&["daemon", "start", "--json"]).await;
    assert_eq!(started.code, 0, "{started:?}");
    assert_eq!(started.json()["started"], true);
    let why = run(&["daemon", "why", "--json"]).await.json();
    assert_eq!(why["running"], true);
    assert_eq!(why["start_error"], serde_json::Value::Null);

    // Refused while the daemon runs.
    let busy = run(&["daemon", "reset-data", "--yes"]).await;
    assert_eq!(busy.code, 1, "{busy:?}");
    assert!(busy.stderr.contains("hutand is running"), "{}", busy.stderr);

    assert_eq!(run(&["daemon", "stop"]).await.code, 0);
    // The backup is still there: nothing was deleted.
    assert!(Path::new(&backup).is_file());
}
