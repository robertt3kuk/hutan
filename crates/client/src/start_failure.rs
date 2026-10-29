//! Why `hutand` did not start (architecture.md § Start errors), as the clients see it.
//!
//! A daemon that cannot start leaves a record next to its socket, `hutand-start-error.json`
//! (written by `hutan_daemon::start_error`; the format is repeated here because the client may
//! not depend on the daemon). An autostart also keeps what the daemon printed to stderr, in
//! `hutand-start.log` in the log directory. [`StartFailure`] puts both together with how the
//! start ended; [`StartFailure::sentence`] is the one sentence every client shows.
//!
//! [`move_data_aside`] is "start with empty data": it moves the database a daemon refused into
//! `backups/` (never deleting anything) while it holds the daemon's lock, so no daemon can
//! start meanwhile.

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::time::{local_time_seconds_text, now_ms};

/// The record's file name, next to the socket.
pub const START_ERROR_FILE: &str = "hutand-start-error.json";
/// What an autostarted `hutand` wrote to stderr, in the log directory; truncated at each
/// autostart.
pub const START_LOG_FILE: &str = "hutand-start.log";
/// The daemon's own log, in the log directory.
pub const DAEMON_LOG_FILE: &str = "hutand.log";
/// The daemon's database, lock and backup folder, next to each other.
const DATABASE_FILE: &str = "hutan.db";
const LOCK_FILE: &str = "hutand.lock";
const BACKUP_DIR: &str = "backups";
/// The most of a log file that is ever read (the daemon writes no more than this to stderr).
const TAIL_BYTES: u64 = 64 * 1024;
/// Lines of a log's end shown with a failure.
pub const TAIL_LINES: usize = 20;
/// The command that updates a Homebrew install.
pub const UPDATE_COMMAND: &str = hutan_protocol::update::CASK_COMMAND;

/// `hutand-start-error.json`: what the last failed start of the daemon recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartErrorRecord {
    /// When the start failed (Unix ms).
    pub at_ms: u64,
    /// The process that failed.
    pub pid: u32,
    /// Its version (`CARGO_PKG_VERSION`).
    pub version: String,
    #[serde(flatten)]
    pub kind: StartErrorKind,
    /// The whole error chain.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartErrorKind {
    /// The database was written by a newer Hutan: schema `found`, this daemon reads up to
    /// `supported`.
    SchemaTooNew {
        found: u32,
        supported: u32,
        db: PathBuf,
    },
    /// Another daemon holds the lock (`holder_pid`, when the lock file names it).
    AlreadyRunning {
        #[serde(default)]
        holder_pid: Option<u32>,
        #[serde(default)]
        lock: Option<PathBuf>,
    },
    /// Anything else (also a kind this client does not know yet).
    #[serde(other)]
    Other,
}

/// How a start the client asked for ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    /// The process exited before the socket answered (`code` is `None` after a signal).
    Exited { code: Option<i32> },
    /// The socket did not answer in time.
    TimedOut,
    /// Nothing was started this time: the socket does not answer and an earlier start left
    /// this record (a daemon that launchd or another client started, or an autostart that is
    /// not allowed right now).
    Recorded,
}

/// Why the daemon is not there: the record of its last failed start, what it printed, and how
/// the start ended. One type for every client: the error of an autostart
/// ([`ClientError::DaemonStart`](crate::ClientError::DaemonStart)), the connection's state
/// ([`StoreState::daemon_start`](crate::StoreState::daemon_start)), `hutan daemon status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFailure {
    pub socket: PathBuf,
    pub outcome: StartOutcome,
    /// The daemon's record, when it got far enough to write one (for an autostart: only one
    /// written by the process this client started).
    pub record: Option<StartErrorRecord>,
    /// The end of `hutand-start.log` (autostarts only).
    pub stderr_tail: Option<String>,
    /// Where that came from.
    pub start_log: PathBuf,
}

impl StartFailure {
    /// After an autostart: the record if the started process wrote it (at or after
    /// `spawned_ms`), and the end of its stderr.
    pub(crate) fn collect(files: &DaemonFiles, outcome: StartOutcome, spawned_ms: u64) -> Self {
        Self {
            socket: files.socket.clone(),
            outcome,
            record: files.read_record().filter(|r| r.at_ms >= spawned_ms),
            stderr_tail: tail(&files.start_log(), TAIL_LINES),
            start_log: files.start_log(),
        }
    }

    /// Without a start of our own: what the record says, if there is one.
    pub fn recorded(files: &DaemonFiles) -> Option<Self> {
        let record = files.read_record()?;
        Some(Self {
            socket: files.socket.clone(),
            outcome: StartOutcome::Recorded,
            record: Some(record),
            stderr_tail: None,
            start_log: files.start_log(),
        })
    }

    pub fn kind(&self) -> Option<&StartErrorKind> {
        self.record.as_ref().map(|r| &r.kind)
    }

    /// The database a newer Hutan wrote, with its versions (`found`, `supported`).
    pub fn schema_too_new(&self) -> Option<(&Path, u32, u32)> {
        match self.kind()? {
            StartErrorKind::SchemaTooNew {
                found,
                supported,
                db,
            } => Some((db, *found, *supported)),
            _ => None,
        }
    }

    /// The process that holds the lock while the socket does not answer.
    pub fn lock_holder(&self) -> Option<u32> {
        match self.kind()? {
            StartErrorKind::AlreadyRunning { holder_pid, .. } => *holder_pid,
            _ => None,
        }
    }

    /// What happened, in one sentence (with the way out where there is one).
    pub fn sentence(&self) -> String {
        let socket = self.socket.display();
        match self.record.as_ref().map(|r| (&r.kind, r)) {
            Some((
                StartErrorKind::SchemaTooNew {
                    found, supported, ..
                },
                _,
            )) => format!(
                "Your Hutan data was written by a newer Hutan (database version {found}; this \
                 build reads up to {supported}). Update Hutan: `{UPDATE_COMMAND}`."
            ),
            Some((StartErrorKind::AlreadyRunning { holder_pid, .. }, _)) => match holder_pid {
                Some(pid) => format!(
                    "Another hutand (pid {pid}) holds the lock but does not answer on {socket}."
                ),
                None => format!("Another hutand holds the lock but does not answer on {socket}."),
            },
            Some((StartErrorKind::Other, record)) => {
                format!("hutand could not start: {}", record.message)
            }
            None => {
                let last = self.stderr_tail.as_deref().and_then(|tail| {
                    tail.lines()
                        .rev()
                        .map(str::trim)
                        .find(|line| !line.is_empty())
                });
                match (self.outcome, last) {
                    (StartOutcome::Exited { code }, Some(line)) => {
                        format!("hutand could not start ({}): {line}", exit_text(code))
                    }
                    (StartOutcome::Exited { code }, None) => format!(
                        "hutand stopped ({}) before it accepted connections on {socket}.",
                        exit_text(code)
                    ),
                    (_, Some(line)) => format!(
                        "hutand did not start accepting connections on {socket} in time; it \
                         last printed: {line}"
                    ),
                    (_, None) => {
                        format!("hutand did not start accepting connections on {socket} in time.")
                    }
                }
            }
        }
    }

    /// Everything known, for *Copy details*, the CLI and bug reports: the sentence, the record
    /// (or, without one, the end of what the daemon printed) and how the start ended.
    pub fn details(&self) -> String {
        let mut out = self.sentence();
        out.push('\n');
        if let Some(record) = &self.record {
            out.push_str(&format!(
                "\nhutand {} (pid {}) failed at {}:\n{}\n",
                record.version,
                record.pid,
                local_time_seconds_text(record.at_ms),
                record.message
            ));
        } else if let Some(tail) = self.stderr_tail.as_deref().filter(|t| !t.trim().is_empty()) {
            out.push_str(&format!(
                "\nEnd of {}:\n{}\n",
                self.start_log.display(),
                tail.trim_end()
            ));
        }
        match self.outcome {
            StartOutcome::Exited { code } => {
                out.push_str(&format!(
                    "\nThe hutand started now exited ({}).\n",
                    exit_text(code)
                ));
            }
            StartOutcome::TimedOut => {
                out.push_str("\nThe hutand started now did not answer in time.\n");
            }
            StartOutcome::Recorded => {}
        }
        out
    }
}

impl fmt::Display for StartFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sentence())
    }
}

fn exit_text(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("exit code {code}"),
        None => "killed by a signal".to_owned(),
    }
}

/// Where the files of the daemon behind a socket are: the record next to the socket, the logs
/// in `~/Library/Logs/Hutan` for the standard socket and in `logs/` next to any other (the
/// `HUTAN_HOME` layout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonFiles {
    pub socket: PathBuf,
    /// The socket's directory: also where the daemon keeps its database and lock unless
    /// `HUTAN_SOCKET` moved the socket elsewhere.
    pub base: PathBuf,
    pub log_dir: PathBuf,
}

impl DaemonFiles {
    pub fn for_socket(socket: &Path) -> Self {
        Self::resolve(socket, std::env::home_dir())
    }

    fn resolve(socket: &Path, user_home: Option<PathBuf>) -> Self {
        let base = socket
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_owned();
        let standard = user_home
            .as_ref()
            .filter(|home| base == home.join("Library/Application Support/Hutan"));
        let log_dir = match standard {
            Some(home) => home.join("Library/Logs/Hutan"),
            None => base.join("logs"),
        };
        Self {
            socket: socket.to_owned(),
            base,
            log_dir,
        }
    }

    pub fn start_error(&self) -> PathBuf {
        self.base.join(START_ERROR_FILE)
    }

    pub fn start_log(&self) -> PathBuf {
        self.log_dir.join(START_LOG_FILE)
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.log_dir.join(DAEMON_LOG_FILE)
    }

    /// The record, if there is a readable one.
    pub fn read_record(&self) -> Option<StartErrorRecord> {
        let path = self.start_error();
        let text = fs::read(&path).ok()?;
        match serde_json::from_slice(&text) {
            Ok(record) => Some(record),
            Err(error) => {
                tracing::debug!(path = %path.display(), %error, "unreadable start-error record");
                None
            }
        }
    }

    /// Opens `hutand-start.log` for a new autostart (truncated, owner-only).
    pub(crate) fn open_start_log(&self) -> io::Result<File> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.log_dir)?;
        OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(self.start_log())
    }
}

/// The last `lines` lines of `path` (read from at most its last 64 KiB); `None` when it is
/// missing or empty.
pub fn tail(path: &Path, lines: usize) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text.lines().collect();
    let kept = all[all.len().saturating_sub(lines)..].join("\n");
    (!kept.trim().is_empty()).then_some(kept)
}

/// What [`move_data_aside`] moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedData {
    /// The database's new path in `backups/`.
    pub backup: PathBuf,
    /// Every file moved, as `(from, to)`: the database and its `-wal` / `-shm` files.
    pub moved: Vec<(PathBuf, PathBuf)>,
}

#[derive(Debug, thiserror::Error)]
pub enum ResetError {
    #[error(
        "hutand is running{}; stop it first (`hutan daemon stop`)",
        pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
    )]
    Running { pid: Option<u32> },
    #[error("there is no database at {}", path.display())]
    NoDatabase { path: PathBuf },
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
}

fn io_error(context: impl Into<String>) -> impl FnOnce(io::Error) -> ResetError {
    let context = context.into();
    move |source| ResetError::Io { context, source }
}

/// "Start with empty data": moves the database (and its `-wal` / `-shm`) into `backups/` next
/// to it, as `hutan-<ms>-v<found>-newer.db` when `failure` says a newer Hutan wrote it, else
/// `hutan-<ms>-reset.db`. Nothing is deleted, and pruning of migration backups never counts
/// these. Holds the daemon's lock throughout, so it refuses while a daemon runs and no daemon
/// can start halfway. The database is the one the record names, else `hutan.db` in
/// `files.base`. The start-error record goes too: the next start makes a new database.
pub fn move_data_aside(
    files: &DaemonFiles,
    failure: Option<&StartErrorRecord>,
) -> Result<MovedData, ResetError> {
    let (database, found) = match failure.map(|r| &r.kind) {
        Some(StartErrorKind::SchemaTooNew { db, found, .. }) => (db.clone(), Some(*found)),
        _ => (files.base.join(DATABASE_FILE), None),
    };
    let dir = database
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_owned();
    let lock_path = dir.join(LOCK_FILE);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(io_error(format!("opening {}", lock_path.display())))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let pid = fs::read_to_string(&lock_path)
                .ok()
                .and_then(|text| text.trim().parse().ok());
            return Err(ResetError::Running { pid });
        }
        Err(TryLockError::Error(source)) => {
            return Err(io_error(format!("locking {}", lock_path.display()))(source));
        }
    }
    if !database.is_file() {
        return Err(ResetError::NoDatabase { path: database });
    }
    let backups = dir.join(BACKUP_DIR);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&backups)
        .map_err(io_error(format!("creating {}", backups.display())))?;
    let mut stamp = now_ms();
    let backup = loop {
        let name = match found {
            Some(found) => format!("hutan-{stamp:013}-v{found}-newer.db"),
            None => format!("hutan-{stamp:013}-reset.db"),
        };
        let candidate = backups.join(name);
        if !candidate.exists() {
            break candidate;
        }
        stamp += 1;
    };
    let mut moved = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let from = with_suffix(&database, suffix);
        let to = with_suffix(&backup, suffix);
        match fs::rename(&from, &to) {
            Ok(()) => moved.push((from, to)),
            // Only the database itself must exist.
            Err(error) if error.kind() == io::ErrorKind::NotFound && !suffix.is_empty() => {}
            Err(error) => {
                return Err(io_error(format!(
                    "moving {} to {}",
                    from.display(),
                    to.display()
                ))(error));
            }
        }
    }
    match fs::remove_file(files.start_error()) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(%error, "cannot remove the start-error record"),
    }
    drop(lock);
    Ok(MovedData { backup, moved })
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: StartErrorKind) -> StartErrorRecord {
        StartErrorRecord {
            at_ms: 10,
            pid: 7,
            version: "0.1.0".into(),
            kind,
            message: "boom".into(),
        }
    }

    fn failure(record: Option<StartErrorRecord>, tail: Option<&str>) -> StartFailure {
        StartFailure {
            socket: "/h/hutand.sock".into(),
            outcome: StartOutcome::Exited { code: Some(1) },
            record,
            stderr_tail: tail.map(str::to_owned),
            start_log: "/h/logs/hutand-start.log".into(),
        }
    }

    #[test]
    fn the_record_reads_as_the_daemon_writes_it() {
        let text = r#"{"at_ms":1,"pid":2,"version":"0.2.0","kind":"schema_too_new",
            "found":13,"supported":12,"db":"/h/hutan.db","message":"m"}"#;
        let parsed: StartErrorRecord = serde_json::from_str(text).unwrap();
        assert_eq!(
            parsed.kind,
            StartErrorKind::SchemaTooNew {
                found: 13,
                supported: 12,
                db: "/h/hutan.db".into()
            }
        );
        let held = r#"{"at_ms":1,"pid":2,"version":"0.2.0","kind":"already_running",
            "holder_pid":9,"lock":"/h/hutand.lock","message":"m"}"#;
        let parsed: StartErrorRecord = serde_json::from_str(held).unwrap();
        assert_eq!(
            parsed.kind,
            StartErrorKind::AlreadyRunning {
                holder_pid: Some(9),
                lock: Some("/h/hutand.lock".into())
            }
        );
        let future = r#"{"at_ms":1,"pid":2,"version":"9.0.0","kind":"quantum","message":"m"}"#;
        let parsed: StartErrorRecord = serde_json::from_str(future).unwrap();
        assert_eq!(parsed.kind, StartErrorKind::Other);
    }

    #[test]
    fn one_sentence_per_kind() {
        let newer = failure(
            Some(record(StartErrorKind::SchemaTooNew {
                found: 13,
                supported: 12,
                db: "/h/hutan.db".into(),
            })),
            None,
        );
        assert_eq!(
            newer.sentence(),
            "Your Hutan data was written by a newer Hutan (database version 13; this build \
             reads up to 12). Update Hutan: `brew update && brew upgrade --cask robertt3kuk/hutan/hutan`."
        );
        let held = failure(
            Some(record(StartErrorKind::AlreadyRunning {
                holder_pid: Some(42),
                lock: None,
            })),
            None,
        );
        assert_eq!(
            held.sentence(),
            "Another hutand (pid 42) holds the lock but does not answer on /h/hutand.sock."
        );
        assert_eq!(held.lock_holder(), Some(42));
        let other = failure(Some(record(StartErrorKind::Other)), Some("x"));
        assert_eq!(other.sentence(), "hutand could not start: boom");
        let printed = failure(None, Some("first\nhutand: cannot bind\n\n"));
        assert_eq!(
            printed.sentence(),
            "hutand could not start (exit code 1): hutand: cannot bind"
        );
        assert!(
            printed
                .details()
                .contains("End of /h/logs/hutand-start.log")
        );
        let silent = StartFailure {
            outcome: StartOutcome::TimedOut,
            ..failure(None, None)
        };
        assert_eq!(
            silent.sentence(),
            "hutand did not start accepting connections on /h/hutand.sock in time."
        );
    }

    #[test]
    fn files_follow_the_socket() {
        let home = Some(PathBuf::from("/Users/u"));
        let standard = DaemonFiles::resolve(
            Path::new("/Users/u/Library/Application Support/Hutan/hutand.sock"),
            home.clone(),
        );
        assert_eq!(
            standard.start_error(),
            Path::new("/Users/u/Library/Application Support/Hutan/hutand-start-error.json")
        );
        assert_eq!(
            standard.start_log(),
            Path::new("/Users/u/Library/Logs/Hutan/hutand-start.log")
        );
        let isolated = DaemonFiles::resolve(Path::new("/tmp/hh/hutand.sock"), home);
        assert_eq!(
            isolated.start_log(),
            Path::new("/tmp/hh/logs/hutand-start.log")
        );
        assert_eq!(isolated.daemon_log(), Path::new("/tmp/hh/logs/hutand.log"));
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        assert_eq!(tail(&path, 2), None);
        fs::write(&path, "a\nb\nc\n").unwrap();
        assert_eq!(tail(&path, 2).as_deref(), Some("b\nc"));
    }

    #[test]
    fn moving_data_aside_keeps_everything_and_refuses_while_locked() {
        let dir = tempfile::tempdir().unwrap();
        let files = DaemonFiles::for_socket(&dir.path().join("hutand.sock"));
        let db = dir.path().join("hutan.db");
        fs::write(&db, "db").unwrap();
        fs::write(dir.path().join("hutan.db-wal"), "wal").unwrap();
        fs::write(files.start_error(), "{}").unwrap();
        let newer = record(StartErrorKind::SchemaTooNew {
            found: 13,
            supported: 12,
            db: db.clone(),
        });

        // A running daemon holds the lock: nothing moves.
        let held = File::create(dir.path().join("hutand.lock")).unwrap();
        held.lock().unwrap();
        fs::write(dir.path().join("hutand.lock"), "4242\n").unwrap();
        assert!(matches!(
            move_data_aside(&files, Some(&newer)),
            Err(ResetError::Running { pid: Some(4242) })
        ));
        assert!(db.exists());
        drop(held);

        let moved = move_data_aside(&files, Some(&newer)).unwrap();
        let name = moved
            .backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("hutan-") && name.ends_with("-v13-newer.db"),
            "{name}"
        );
        assert_eq!(moved.backup.parent().unwrap(), dir.path().join("backups"));
        assert_eq!(fs::read_to_string(&moved.backup).unwrap(), "db");
        assert_eq!(
            fs::read_to_string(with_suffix(&moved.backup, "-wal")).unwrap(),
            "wal"
        );
        assert_eq!(moved.moved.len(), 2);
        assert!(!db.exists());
        assert!(!files.start_error().exists());

        // Nothing left to move.
        assert!(matches!(
            move_data_aside(&files, None),
            Err(ResetError::NoDatabase { .. })
        ));
    }
}
