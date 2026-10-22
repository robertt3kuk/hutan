//! Why the last start of `hutand` failed (architecture.md § Start errors).
//!
//! A daemon that cannot start has nobody to tell over the protocol, so it leaves a small record
//! next to its socket, `hutand-start-error.json`, before it exits:
//!
//! ```json
//! {"at_ms": 1761100000000, "pid": 4242, "version": "0.2.0",
//!  "kind": "schema_too_new", "found": 13, "supported": 12, "db": "/…/hutan.db",
//!  "message": "cannot use the database …"}
//! ```
//!
//! `kind` is `schema_too_new` (with `found`, `supported`, `db`), `already_running` (with
//! `holder_pid` when the lock file names it) or `other`. A successful start removes the record,
//! and so does a daemon that stops normally (a record written meanwhile came from a start that
//! failed because this daemon held the lock). Clients read it (`hutan_client::StartFailure`)
//! when the socket does not answer. The format is repeated there because the client may not
//! depend on this crate; the tests of both crates run the real binary against each other.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hutan_storage::StorageError;
use serde_json::{Value, json};

use crate::error::DaemonError;
use crate::paths::Paths;

/// File name of the record, in the socket's directory.
pub const START_ERROR_FILE: &str = "hutand-start-error.json";

/// Where the record of `paths` lives: next to the socket.
pub fn record_path(paths: &Paths) -> PathBuf {
    paths.socket.with_file_name(START_ERROR_FILE)
}

/// The record for `error` as JSON.
pub fn record(error: &anyhow::Error, at_ms: u64) -> Value {
    let mut record = json!({
        "at_ms": at_ms,
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
        "kind": "other",
        "message": format!("{error:#}"),
    });
    for cause in error.chain() {
        // The storage error sits boxed inside `DaemonError::Storage`, so the chain yields the
        // box, not the error: look through it.
        let storage = match cause.downcast_ref::<DaemonError>() {
            Some(DaemonError::AlreadyRunning { lock }) => {
                record["kind"] = json!("already_running");
                record["lock"] = json!(lock);
                if let Some(pid) = lock_holder(lock) {
                    record["holder_pid"] = json!(pid);
                }
                break;
            }
            Some(DaemonError::Storage { source, .. }) => Some(&**source),
            _ => cause.downcast_ref::<StorageError>(),
        };
        if let Some(StorageError::SchemaTooNew {
            path,
            found,
            supported,
        }) = storage
        {
            record["kind"] = json!("schema_too_new");
            record["found"] = json!(found);
            record["supported"] = json!(supported);
            record["db"] = json!(path);
            break;
        }
    }
    record
}

/// Writes the record of `error` (best effort: a failure is only logged), replacing the file
/// atomically so a reader never sees half of it.
pub fn write(paths: &Paths, error: &anyhow::Error) {
    let path = record_path(paths);
    let record = record(error, now_ms());
    if let Err(write_error) = write_atomically(&path, &record) {
        tracing::warn!(
            path = %path.display(),
            error = %write_error,
            "cannot write the start-error record"
        );
    }
}

/// Removes the record; a missing one is fine.
pub fn clear(paths: &Paths) {
    let path = record_path(paths);
    match fs::remove_file(&path) {
        Ok(()) => tracing::info!(path = %path.display(), "removed the start-error record"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            path = %path.display(),
            %error,
            "cannot remove the start-error record"
        ),
    }
}

fn write_atomically(path: &Path, record: &Value) -> std::io::Result<()> {
    let temp = path.with_extension(format!("json.{}", std::process::id()));
    let mut text = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    text.push(b'\n');
    let written = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temp)
        .and_then(|mut file| file.write_all(&text))
        .and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// The pid the lock's holder wrote into it (informational, see `acquire_lock`).
fn lock_holder(lock: &Path) -> Option<u32> {
    fs::read_to_string(lock).ok()?.trim().parse().ok()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_database_is_recorded_with_its_versions() {
        let error = anyhow::Error::new(DaemonError::Storage {
            path: "/h/hutan.db".into(),
            source: Box::new(StorageError::SchemaTooNew {
                path: "/h/hutan.db".into(),
                found: 13,
                supported: 12,
            }),
        });
        let record = record(&error, 7);
        assert_eq!(record["kind"], "schema_too_new");
        assert_eq!(record["found"], 13);
        assert_eq!(record["supported"], 12);
        assert_eq!(record["db"], "/h/hutan.db");
        assert_eq!(record["at_ms"], 7);
        assert_eq!(record["version"], env!("CARGO_PKG_VERSION"));
        let message = record["message"].as_str().unwrap();
        assert!(message.contains("schema version 13"), "{message}");
    }

    #[test]
    fn a_held_lock_names_its_holder() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("hutand.lock");
        fs::write(&lock, "4242\n").unwrap();
        let error = anyhow::Error::new(DaemonError::AlreadyRunning { lock: lock.clone() })
            .context("starting");
        let record = record(&error, 1);
        assert_eq!(record["kind"], "already_running");
        assert_eq!(record["holder_pid"], 4242);
        assert_eq!(record["lock"], json!(lock));
    }

    #[test]
    fn anything_else_is_other_with_the_full_chain() {
        let error = anyhow::anyhow!("binding /x").context("starting the daemon");
        let record = record(&error, 1);
        assert_eq!(record["kind"], "other");
        assert_eq!(record["message"], "starting the daemon: binding /x");
    }

    #[test]
    fn write_then_clear() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_home(dir.path());
        write(&paths, &anyhow::anyhow!("boom"));
        let text = fs::read_to_string(record_path(&paths)).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["message"], "boom");
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temporary file left"
        );
        clear(&paths);
        assert!(!record_path(&paths).exists());
        clear(&paths);
    }
}
