//! Test support for the crates whose tests run `hutand` as a real process (feature
//! `test-support`): [`DaemonReaper`] stops such a daemon whatever way its test ends.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::paths::LOCK_FILE;

/// How long a daemon gets after SIGTERM (and again after SIGKILL) to be gone.
const GRACE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);

/// Stops, when dropped (or on [`reap`](Self::reap)), every `hutand` a test started on one
/// `HUTAN_HOME`: the one holding its `hutand.lock`, and every pid listed in an optional pid file
/// (one per line; a launcher script appends `$$` before it `exec`s the daemon, so a daemon that
/// has not taken the lock yet is found too). Each gets SIGTERM — a daemon stops its sessions on
/// it — then SIGKILL if it is still there. Runs after a failed assertion or a panic as well, so
/// no test leaves a daemon behind.
///
/// Declare it after the temporary directory that holds the home, so it drops first.
#[derive(Debug)]
pub struct DaemonReaper {
    home: PathBuf,
    pid_file: Option<PathBuf>,
}

impl DaemonReaper {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            pid_file: None,
        }
    }

    /// Also stops the processes listed in `pid_file`.
    pub fn with_pid_file(mut self, pid_file: impl Into<PathBuf>) -> Self {
        self.pid_file = Some(pid_file.into());
        self
    }

    /// Stops every daemon of the home now; returns the pids that had to be signalled.
    pub fn reap(&self) -> Vec<u32> {
        let mut pids: Vec<u32> = self
            .pid_file
            .as_deref()
            .and_then(|file| std::fs::read_to_string(file).ok())
            .map(|text| text.lines().filter_map(|l| l.trim().parse().ok()).collect())
            .unwrap_or_default();
        pids.extend(lock_holder(&self.home.join(LOCK_FILE)));
        pids.sort_unstable();
        pids.dedup();
        // An in-process daemon of the same home (a test's next step) is the test's own business.
        let me = std::process::id();
        pids.into_iter()
            .filter(|pid| *pid != me && stop(*pid))
            .collect()
    }
}

impl Drop for DaemonReaper {
    fn drop(&mut self) {
        self.reap();
    }
}

/// The pid in `lock` while a daemon holds it; `None` when nobody does (the pid written there is
/// then stale and may belong to another process by now).
fn lock_holder(lock: &Path) -> Option<u32> {
    let file = File::open(lock).ok()?;
    if file.try_lock().is_ok() {
        // Free: released again when `file` drops.
        return None;
    }
    std::fs::read_to_string(lock).ok()?.trim().parse().ok()
}

/// Whether `pid` is a live (not zombie) process whose command names `hutand` (the daemon, or the
/// launcher script that is about to become it).
fn running(pid: u32) -> bool {
    let Ok(out) = Command::new("ps")
        .args(["-o", "stat=,command=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim();
    !line.is_empty() && !line.starts_with('Z') && line.contains("hutand")
}

/// SIGTERM, then SIGKILL, each followed by a wait for the process to be gone; `true` when it
/// was running.
fn stop(pid: u32) -> bool {
    if !running(pid) {
        return false;
    }
    for signal in ["-TERM", "-KILL"] {
        let _ = Command::new("kill")
            .args([signal, &pid.to_string()])
            .stderr(Stdio::null())
            .status();
        let deadline = Instant::now() + GRACE;
        while running(pid) && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        if !running(pid) {
            break;
        }
    }
    true
}
