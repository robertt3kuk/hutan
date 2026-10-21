//! The server's process: started in its own process group so that it and everything it starts
//! (rust-analyzer's proc-macro servers, `cargo check`, tsserver's workers) can be ended
//! together, even descendants whose parent already died (they keep the group). A descendant
//! that leaves the group (`setsid`) is out of reach here; the daemon's Resources still see it
//! by parent pid.
//!
//! One reaper task owns the [`Child`]: it waits for it, ends the group when asked or when every
//! [`ProcessHandle`] is gone, and reports how the process ended. [`ProcessHandle::kill_now`] is
//! synchronous so that dropping a client kills the tree even when no runtime is left to run
//! the reaper.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::StreamExt;
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_util::codec::{FramedRead, LinesCodec};
use tracing::{debug, warn};

use crate::error::LspError;

/// stderr lines remembered for error messages.
const STDERR_LINES: usize = 20;
/// Longest stderr line kept, in bytes (longer ones are skipped).
const STDERR_LINE_BYTES: usize = 4096;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded data is never left half-updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How to start the server: `<program> <args...>` in `cwd` with `env` added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

/// How the process ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub stderr_tail: String,
}

impl ExitInfo {
    pub fn describe(&self) -> String {
        let how = match (self.code, self.signal) {
            (Some(code), _) => format!("exited with code {code}"),
            (None, Some(signal)) => format!("killed by signal {signal}"),
            (None, None) => "ended".to_owned(),
        };
        match self.stderr_tail.lines().last() {
            Some(line) if !line.is_empty() => format!("{how}: {line}"),
            _ => how,
        }
    }
}

/// Ends the process group of a live server. Cheap to clone; the group is ended when the last
/// clone is dropped.
#[derive(Debug, Clone)]
pub(crate) struct ProcessHandle {
    /// The leader's pid (= the group id); 0 once it was reaped.
    pid: Arc<AtomicU32>,
    kill: mpsc::Sender<()>,
}

impl ProcessHandle {
    pub fn pid(&self) -> Option<u32> {
        match self.pid.load(Ordering::Acquire) {
            0 => None,
            pid => Some(pid),
        }
    }

    /// SIGKILLs the whole group now, without waiting.
    pub fn kill_now(&self) {
        if let Some(pid) = self.pid() {
            kill_group(pid);
        }
    }

    /// Asks the reaper to end the process: it gets the exit grace given at spawn to leave on
    /// its own (after `exit`), then its group is killed.
    pub fn end(&self) {
        // Full: an end is already requested; closed: the reaper is done.
        let _ = self.kill.try_send(());
    }
}

fn kill_group(pid: u32) {
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return;
    };
    // `pid` leads its own group (`process_group(0)` at spawn) and has not been reaped, so the
    // group id still names our process tree. ESRCH: the group is already gone.
    if let Err(error) = kill_process_group(pid, Signal::KILL) {
        debug!(%error, pid = pid.as_raw_nonzero().get(), "could not kill the server's group");
    }
}

pub(crate) struct Spawned {
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub handle: ProcessHandle,
    pub exit: oneshot::Receiver<ExitInfo>,
    pub stderr: Arc<Mutex<VecDeque<String>>>,
}

/// Starts the server and its reaper and stderr tasks.
pub(crate) fn spawn(launch: &Launch, exit_grace: Duration) -> Result<Spawned, LspError> {
    let mut command = Command::new(&launch.program);
    command
        .args(&launch.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    if let Some(cwd) = &launch.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &launch.env {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|source| LspError::Spawn {
        program: launch.program.display().to_string(),
        source,
    })?;
    // `Stdio::piped()` was requested for all three, so the handles exist.
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(LspError::Closed {
            detail: "child pipes missing".to_owned(),
        });
    };
    let pid = Arc::new(AtomicU32::new(child.id().unwrap_or(0)));
    let (kill, kill_rx) = mpsc::channel(1);
    let (exit_tx, exit) = oneshot::channel();
    let tail = Arc::new(Mutex::new(VecDeque::new()));
    tokio::spawn(stderr_loop(stderr, tail.clone()));
    tokio::spawn(reap(
        child,
        kill_rx,
        exit_tx,
        tail.clone(),
        pid.clone(),
        exit_grace,
    ));
    Ok(Spawned {
        stdin,
        stdout,
        handle: ProcessHandle { pid, kill },
        exit,
        stderr: tail,
    })
}

async fn stderr_loop(stderr: ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = FramedRead::new(stderr, LinesCodec::new_with_max_length(STDERR_LINE_BYTES));
    while let Some(line) = lines.next().await {
        let Ok(line) = line else {
            // An over-long line is skipped up to its end; an I/O error ends the stream.
            continue;
        };
        debug!(target: "hutan_lsp_client::stderr", "{line}");
        let mut tail = lock(&tail);
        if tail.len() == STDERR_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

pub(crate) fn stderr_tail(tail: &Mutex<VecDeque<String>>) -> String {
    lock(tail).iter().cloned().collect::<Vec<_>>().join("\n")
}

async fn reap(
    mut child: Child,
    mut kill: mpsc::Receiver<()>,
    exit: oneshot::Sender<ExitInfo>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    pid: Arc<AtomicU32>,
    grace: Duration,
) {
    let leader = pid.load(Ordering::Acquire);
    let status = tokio::select! {
        status = child.wait() => status,
        request = kill.recv() => {
            // Asked to end it: the process gets the grace to leave on its own. Every handle
            // gone: nobody talks to it any more.
            let waited = match request {
                Some(()) => timeout(grace, child.wait()).await.ok(),
                None => None,
            };
            match waited {
                Some(status) => status,
                None => {
                    kill_group(leader);
                    child.wait().await
                }
            }
        }
    };
    // Descendants that outlived the leader are still in its group (the group id is not reused
    // while it has members). Without members this fails with ESRCH.
    kill_group(leader);
    pid.store(0, Ordering::Release);
    let (code, signal) = match status {
        Ok(status) => {
            use std::os::unix::process::ExitStatusExt;
            (status.code(), status.signal())
        }
        Err(error) => {
            warn!(%error, "waiting for the language server failed");
            (None, None)
        }
    };
    // Let the stderr task drain what the process wrote last.
    tokio::task::yield_now().await;
    let _ = exit.send(ExitInfo {
        code,
        signal,
        stderr_tail: stderr_tail(&stderr),
    });
}
