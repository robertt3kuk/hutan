//! PTY ownership around the headless [`Emulator`].
//!
//! Threads per terminal:
//! * `reader` — blocking read of the PTY master → [`Emulator::feed`] → publish damage, forward
//!   engine replies and metadata.
//! * `writer` — the single PTY writer. [`Terminal::write_input`] and engine replies (DA, DSR,
//!   DECRQM, kitty, colour and window-size queries) all go through its bounded queue, so bytes
//!   never interleave.
//! * `sync` — services the DEC 2026 deadline: a never-closed synchronized batch is force-closed
//!   after the engine's 150 ms timeout and rendered.
//! * `waiter` — reaps the child exactly once, lets the other threads release their PTY
//!   descriptors, then reports the exit status.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use hutan_protocol::blocks::{Block, BlockPart};
use hutan_protocol::messages::TerminalSize;
use hutan_protocol::render::{Row, TerminalFrame, TerminalRender, TerminalSnapshot};
use parking_lot::{Condvar, Mutex};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{broadcast, watch};

use crate::blocks::{BlockText, BlockTextError};
use crate::emulator::{DefaultColors, Emulator, ScanFlow, ScanProgress, SearchLine};
use crate::env::{self, EnvInherit, SystemFacts};
use crate::error::TerminalError;
use crate::render::RenderTracker;
use crate::shell_integration;
use crate::sniffer::TerminalMeta;
use crate::sys::{self, Reaped, Signal};

const READ_BUFFER: usize = 64 * 1024;
/// Queued writer messages (input chunks + replies); input blocks when full, replies are dropped.
const WRITE_QUEUE: usize = 256;
const META_CAPACITY: usize = 256;
/// After the child exits, wait at most this long for the helper threads to release their PTY
/// file descriptors (a background job holding the slave keeps the reader alive).
const TEARDOWN_WAIT: Duration = Duration::from_secs(2);
/// Helper threads holding PTY descriptors: reader, writer, sync.
const HELPER_THREADS: u32 = 3;

/// Spawns are serialized: `portable-pty` sets `FD_CLOEXEC` after `openpty`, so a concurrent
/// fork/exec elsewhere in the process could inherit the master (ADR 0004, spike § PTY).
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone)]
pub struct TerminalConfig {
    /// Program and arguments; empty = the user's login shell with `-l` ([`default_shell`]).
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Applied on top of the base environment ([`env_inherit`](Self::env_inherit)); `TERM`,
    /// `COLORTERM`, `TERM_PROGRAM` and the shell-integration variables are forced afterwards.
    pub env: BTreeMap<String, String>,
    /// Where the base environment comes from (see [`crate::env`]).
    pub env_inherit: EnvInherit,
    pub size: TerminalSize,
    /// History lines retained; default 10 000.
    pub scrollback_lines: usize,
    /// Colours reported for OSC 10/11/12 queries while the application has not set its own.
    pub default_colors: DefaultColors,
    /// Inject OSC 133 / OSC 7 shell integration when the session runs zsh, bash or fish
    /// (default true). The session environment variable `HUTAN_SHELL_INTEGRATION=0` opts
    /// out as well.
    pub shell_integration: bool,
    /// Directory written by [`crate::shell_integration::install`]; `None` = nothing to inject.
    pub shell_integration_dir: Option<PathBuf>,
    /// A program and arguments placed before the command line, which they execute: a sandbox
    /// wrapper (`sandbox-exec -p <profile> --`). Empty: the command runs as it is.
    pub wrap: Vec<String>,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            argv: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            env_inherit: EnvInherit::default(),
            size: TerminalSize {
                cols: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            },
            scrollback_lines: 10_000,
            default_colors: DefaultColors::default(),
            shell_integration: true,
            shell_integration_dir: None,
            wrap: Vec::new(),
        }
    }
}

/// How the child ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitStatus {
    /// Exit code; `None` when the child was killed by a signal (or the status is unavailable).
    pub code: Option<i32>,
    /// Raw number of the terminating signal; `None` when the child exited normally (or the
    /// status is unavailable, in which case `code` is `None` too).
    pub signal: Option<i32>,
}

impl ExitStatus {
    pub fn success(&self) -> bool {
        self.signal.is_none() && self.code == Some(0)
    }
}

impl From<Reaped> for ExitStatus {
    fn from(reaped: Reaped) -> Self {
        match reaped {
            Reaped::Exited(code) => Self {
                code: Some(code),
                signal: None,
            },
            Reaped::Signaled(signal) => Self {
                code: None,
                signal: Some(signal),
            },
        }
    }
}

struct Core {
    engine: Mutex<Emulator>,
    /// Wakes the `sync` thread when a batch opens or at shutdown; always used with `engine`.
    sync_wake: Condvar,
    /// Set (while holding `engine`, so the sync thread cannot miss it) at teardown.
    shutdown: AtomicBool,
    damage: watch::Sender<u64>,
    meta: broadcast::Sender<TerminalMeta>,
    writer: Mutex<Option<SyncSender<Vec<u8>>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    /// 0 = no live child (not yet started, or already reaped).
    pid: AtomicU32,
    exit_status: Mutex<Option<ExitStatus>>,
    exit_changed: Condvar,
    exit_tx: watch::Sender<Option<ExitStatus>>,
    /// Helper threads still holding PTY descriptors.
    live: Mutex<u32>,
    live_changed: Condvar,
}

/// Decrements [`Core::live`] when a helper thread ends. Must drop after the thread's PTY
/// handle: parameters drop after the body's locals, so the loops move their handle into a
/// local declared after the guard.
struct LiveGuard(Arc<Core>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        *self.0.live.lock() -= 1;
        self.0.live_changed.notify_all();
    }
}

impl Core {
    /// Forwards engine output produced by the last `feed`/`stop_sync` and announces damage.
    /// Called with the engine lock held; never blocks.
    fn publish(&self, engine: &mut Emulator, visible: bool) {
        let replies = engine.drain_replies();
        if !replies.is_empty() {
            self.queue_reply(replies);
        }
        for meta in engine.drain_meta() {
            // No receivers is fine: metadata is best effort for late subscribers.
            let _ = self.meta.send(meta);
        }
        if visible {
            self.damage.send_modify(|generation| *generation += 1);
        }
    }

    fn queue_reply(&self, bytes: Vec<u8>) {
        let sender = self.writer.lock().clone();
        if let Some(Err(TrySendError::Full(_))) = sender.map(|tx| tx.try_send(bytes)) {
            tracing::warn!("PTY write queue full; dropping an engine reply");
        }
    }

    fn terminate(self: &Arc<Self>, grace: Duration) -> Result<(), TerminalError> {
        let pid = self.pid.load(Ordering::Acquire);
        if pid == 0 {
            return Ok(());
        }
        sys::signal_group(pid, Signal::Hangup).map_err(TerminalError::Signal)?;
        let core = Arc::clone(self);
        thread::Builder::new()
            .name("hutan-term-kill".into())
            .spawn(move || {
                let deadline = Instant::now() + grace;
                let mut status = core.exit_status.lock();
                while status.is_none() && core.pid.load(Ordering::Acquire) == pid {
                    if core
                        .exit_changed
                        .wait_until(&mut status, deadline)
                        .timed_out()
                    {
                        break;
                    }
                }
                // The waiter zeroes `pid` right after reaping, so a non-matching pid means the
                // child is gone and the number may already belong to someone else.
                if core.pid.load(Ordering::Acquire) == pid
                    && let Err(error) = sys::signal_group(pid, Signal::Kill)
                {
                    tracing::debug!(%error, pid, "SIGKILL after grace failed");
                }
            })
            .map_err(|source| TerminalError::Thread {
                name: "hutan-term-kill",
                source,
            })?;
        Ok(())
    }
}

/// Hangs up the child when the last [`Terminal`] handle goes away.
struct DropGuard(Arc<Core>);

impl Drop for DropGuard {
    fn drop(&mut self) {
        if let Err(error) = self.0.terminate(Duration::from_secs(1)) {
            tracing::debug!(%error, "terminate on drop failed");
        }
    }
}

/// A shell or program running in a PTY, with canonical terminal state kept in the daemon.
/// Cheap to clone; all clones refer to the same terminal.
#[derive(Clone)]
pub struct Terminal {
    core: Arc<Core>,
    _guard: Arc<DropGuard>,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("pid", &self.pid())
            .finish_non_exhaustive()
    }
}

/// The shell a terminal runs when no command is given, in order: an explicit `SHELL` in the
/// session's environment, the user's login shell from the account database (independent of
/// whoever started the daemon), the daemon's own `$SHELL`, then `/bin/zsh`.
pub fn default_shell(env: &BTreeMap<String, String>) -> String {
    env.get("SHELL")
        .filter(|shell| !shell.is_empty())
        .cloned()
        .or_else(crate::sys::login_shell)
        .or_else(|| {
            std::env::var("SHELL")
                .ok()
                .filter(|shell| !shell.is_empty())
        })
        .unwrap_or_else(|| "/bin/zsh".to_owned())
}

/// What the base environment is built from: the system facts for [`EnvInherit::Login`], the
/// daemon's environment for [`EnvInherit::Daemon`].
struct EnvSource {
    facts: SystemFacts,
    /// The login shell's own environment, for sessions with an explicit command (login mode);
    /// layered over the facts.
    login_env: Option<BTreeMap<String, String>>,
    daemon_env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

impl EnvSource {
    /// This machine and this process.
    fn system(cfg: &TerminalConfig) -> Self {
        match cfg.env_inherit {
            EnvInherit::Login => {
                let facts = env::system_facts();
                // A login shell builds its own environment; a plain command has none to do it.
                let login_env = (!cfg.argv.is_empty())
                    .then(|| env::login_env(&default_shell(&cfg.env), &facts))
                    .flatten();
                Self {
                    facts,
                    login_env,
                    daemon_env: Vec::new(),
                }
            }
            EnvInherit::Daemon => Self {
                facts: SystemFacts::default(),
                login_env: None,
                daemon_env: std::env::vars_os().collect(),
            },
        }
    }
}

fn command(cfg: &TerminalConfig, source: &EnvSource) -> CommandBuilder {
    let login_shell = cfg.argv.is_empty().then(|| default_shell(&cfg.env));
    let argv: Vec<String> = match &login_shell {
        Some(shell) => vec![shell.clone(), "-l".to_owned()],
        None => cfg.argv.clone(),
    };
    let mut cmd = CommandBuilder::from_argv(argv.into_iter().map(Into::into).collect());
    // `CommandBuilder` starts from the daemon's own environment; the base is set explicitly.
    cmd.env_clear();
    match cfg.env_inherit {
        EnvInherit::Login => {
            let mut base = env::base_env(&source.facts);
            if let Some(login) = &source.login_env {
                base.extend(login.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            for (key, value) in base {
                cmd.env(key, value);
            }
        }
        EnvInherit::Daemon => {
            for (key, value) in &source.daemon_env {
                cmd.env(key, value);
            }
        }
    }
    for (key, value) in &cfg.env {
        cmd.env(key, value);
    }
    // A login shell sees itself as `$SHELL`, like Terminal.app and Ghostty set it.
    if let Some(shell) = &login_shell {
        cmd.env("SHELL", shell);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "hutan");
    let integration_dir = cfg
        .shell_integration_dir
        .as_deref()
        .filter(|_| cfg.shell_integration);
    shell_integration::inject(&mut cmd, integration_dir, cfg.cwd.as_deref());
    // After the injection, which looks at the shell it starts: the wrapper runs it in place.
    if !cfg.wrap.is_empty() {
        cmd.get_argv_mut()
            .splice(0..0, cfg.wrap.iter().map(Into::into));
    }
    if let Some(cwd) = &cfg.cwd {
        cmd.cwd(cwd);
    }
    cmd
}

fn pty_size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    }
}

fn open_error(error: impl std::fmt::Display) -> TerminalError {
    TerminalError::OpenPty(error.to_string())
}

impl Terminal {
    pub fn spawn(cfg: TerminalConfig) -> Result<Self, TerminalError> {
        let engine =
            Emulator::new(cfg.size, cfg.scrollback_lines)?.with_default_colors(cfg.default_colors);
        let program = cfg
            .argv
            .first()
            .cloned()
            .unwrap_or_else(|| "$SHELL".to_owned());
        let cmd = command(&cfg, &EnvSource::system(&cfg));

        let (child, master, reader, writer) = {
            let _spawn = SPAWN_LOCK.lock();
            let pair = native_pty_system()
                .openpty(pty_size(cfg.size))
                .map_err(open_error)?;
            let mut child = pair
                .slave
                .spawn_command(cmd)
                .map_err(|e| TerminalError::Spawn {
                    program,
                    message: format!("{e:#}"),
                })?;
            // The slave must not stay open in the daemon or the reader never sees EOF.
            drop(pair.slave);
            let master = pair.master;
            match (master.try_clone_reader(), master.take_writer()) {
                (Ok(reader), Ok(writer)) => (child, master, reader, writer),
                (Err(error), _) | (_, Err(error)) => {
                    kill_and_reap(child.as_mut());
                    return Err(open_error(format!("{error:#}")));
                }
            }
        };

        let (damage, _) = watch::channel(0u64);
        let (meta, _) = broadcast::channel(META_CAPACITY);
        let (exit_tx, _) = watch::channel(None);
        let (write_tx, write_rx) = mpsc::sync_channel::<Vec<u8>>(WRITE_QUEUE);
        let core = Arc::new(Core {
            engine: Mutex::new(engine),
            sync_wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            damage,
            meta,
            writer: Mutex::new(Some(write_tx)),
            master: Mutex::new(Some(master)),
            pid: AtomicU32::new(child.process_id().unwrap_or(0)),
            exit_status: Mutex::new(None),
            exit_changed: Condvar::new(),
            exit_tx,
            live: Mutex::new(HELPER_THREADS),
            live_changed: Condvar::new(),
        });

        // The child is parked in a slot so it can be recovered if the waiter cannot start.
        let child_slot = Arc::new(Mutex::new(Some(child)));
        if let Err(error) = start_threads(&core, &child_slot, reader, writer, write_rx) {
            core.pid.store(0, Ordering::Release);
            if let Some(mut child) = child_slot.lock().take() {
                kill_and_reap(child.as_mut());
            }
            return Err(error);
        }
        Ok(Terminal {
            core: Arc::clone(&core),
            _guard: Arc::new(DropGuard(core)),
        })
    }

    /// Queues `bytes` for the child's stdin. Blocks only while the (bounded) write queue is
    /// full, i.e. the child is not reading.
    pub fn write_input(&self, bytes: &[u8]) -> Result<(), TerminalError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let sender = self
            .core
            .writer
            .lock()
            .clone()
            .ok_or(TerminalError::Closed)?;
        sender
            .send(bytes.to_vec())
            .map_err(|_| TerminalError::Closed)
    }

    /// Resizes the PTY (`TIOCSWINSZ`, delivering SIGWINCH) and the engine.
    pub fn resize(&self, size: TerminalSize) -> Result<(), TerminalError> {
        if size.cols == 0 || size.rows == 0 {
            return Err(TerminalError::InvalidSize {
                cols: size.cols,
                rows: size.rows,
            });
        }
        {
            let master = self.core.master.lock();
            let master = master.as_ref().ok_or(TerminalError::Closed)?;
            master
                .resize(pty_size(size))
                .map_err(|e| TerminalError::Resize(format!("{e:#}")))?;
        }
        let mut engine = self.core.engine.lock();
        engine.resize(size)?;
        self.core.publish(&mut engine, true);
        Ok(())
    }

    /// Changes the colours answering OSC 10/11/12 queries (see
    /// [`Emulator::set_default_colors`]). Takes effect for queries read after this returns.
    pub fn set_default_colors(&self, colors: DefaultColors) {
        let mut engine = self.core.engine.lock();
        engine.set_default_colors(colors);
        // A program that asked for colour-scheme notifications hears of a change at once.
        self.core.publish(&mut engine, false);
    }

    pub fn size(&self) -> TerminalSize {
        self.core.engine.lock().size()
    }

    /// Full visible state; resets `tracker` to it.
    pub fn snapshot(&self, tracker: &mut RenderTracker) -> TerminalSnapshot {
        self.core.engine.lock().snapshot(tracker)
    }

    /// Changes since `tracker`'s state; `None` if nothing changed or the tracker needs a
    /// snapshot (see [`Self::render`]).
    pub fn frame(&self, tracker: &mut RenderTracker) -> Option<TerminalFrame> {
        self.core.engine.lock().frame(tracker)
    }

    /// The next thing to send a client: a snapshot when the tracker needs one (fresh, resized,
    /// palette changed), otherwise a frame if anything changed.
    pub fn render(&self, tracker: &mut RenderTracker) -> Option<TerminalRender> {
        let mut engine = self.core.engine.lock();
        if engine.needs_snapshot(tracker) {
            return Some(TerminalRender::Snapshot(engine.snapshot(tracker)));
        }
        engine.frame(tracker).map(TerminalRender::Frame)
    }

    /// Scrollback rows, newest first; `from_line` 0 is the line just above the viewport.
    pub fn history(&self, from_line: u32, count: u32) -> Vec<Row> {
        self.core.engine.lock().history(from_line, count)
    }

    /// [`Self::history`] that stops before the first row `keep` rejects (see
    /// [`Emulator::history_while`]). `keep` runs with the engine locked: keep it cheap.
    pub fn history_while(
        &self,
        from_line: u32,
        count: u32,
        keep: impl FnMut(&Row) -> bool,
    ) -> Vec<Row> {
        self.core
            .engine
            .lock()
            .history_while(from_line, count, keep)
    }

    /// Blocks newest first: at most `limit`, only ids below `before_id` when given; with the
    /// absolute line number of the viewport's first row at the same instant.
    pub fn blocks(&self, before_id: Option<u64>, limit: usize) -> (Vec<Block>, u64) {
        let engine = self.core.engine.lock();
        (engine.blocks(before_id, limit), engine.viewport_top())
    }

    /// Plain text of a block's command or output, at most `max_bytes` (see
    /// [`Emulator::block_text`]).
    pub fn block_text(
        &self,
        id: u64,
        part: BlockPart,
        max_bytes: usize,
    ) -> Result<BlockText, BlockTextError> {
        self.core.engine.lock().block_text(id, part, max_bytes)
    }

    /// Shows logical lines of the text to `visit`, oldest first (see [`Emulator::scan_lines`]).
    /// `visit` runs with the engine locked: keep it cheap, and never call back into this
    /// terminal from it.
    pub fn scan_lines(
        &self,
        from: u64,
        max_rows: usize,
        block: Option<u64>,
        visit: impl FnMut(&SearchLine<'_>) -> ScanFlow,
    ) -> Result<ScanProgress, BlockTextError> {
        self.core
            .engine
            .lock()
            .scan_lines(from, max_rows, block, visit)
    }

    /// Generation counter bumped whenever visible state changed; coalescing (watch semantics).
    pub fn damage(&self) -> watch::Receiver<u64> {
        self.core.damage.subscribe()
    }

    /// Title, bell, notification, cwd, prompt-mark and block events. Lagging receivers lose the
    /// oldest events (broadcast semantics).
    pub fn metadata(&self) -> broadcast::Receiver<TerminalMeta> {
        self.core.meta.subscribe()
    }

    /// Process id while the child is running; `None` once it exited and was reaped.
    pub fn pid(&self) -> Option<u32> {
        match self.core.pid.load(Ordering::Acquire) {
            0 => None,
            pid => Some(pid),
        }
    }

    /// Hangs up the child's process group now and kills it after `grace` if it is still
    /// running. Returns immediately; use [`Self::wait_exit`] to observe the end.
    pub fn terminate(&self, grace: Duration) -> Result<(), TerminalError> {
        self.core.terminate(grace)
    }

    /// Sends SIGKILL to the child's whole process group now. Returns immediately; use
    /// [`Self::wait_exit`] to observe the end. A child that is already gone is not an error.
    pub fn kill(&self) -> Result<(), TerminalError> {
        let pid = self.core.pid.load(Ordering::Acquire);
        if pid == 0 {
            return Ok(());
        }
        sys::signal_group(pid, Signal::Kill).map_err(TerminalError::Signal)
    }

    /// Completes when the child has exited, been reaped (exactly once) and the PTY
    /// descriptors were released (bounded wait, see [`TEARDOWN_WAIT`]).
    pub async fn wait_exit(&self) -> ExitStatus {
        let mut rx = self.core.exit_tx.subscribe();
        match rx.wait_for(Option::is_some).await {
            Ok(status) => status.clone().unwrap_or_else(lost_status),
            Err(_) => lost_status(),
        }
    }
}

/// Neither a code nor a signal: the status could not be collected.
fn lost_status() -> ExitStatus {
    ExitStatus {
        code: None,
        signal: None,
    }
}

fn kill_and_reap(child: &mut (dyn Child + Send + Sync)) {
    if let Some(pid) = child.process_id()
        && let Err(error) = sys::signal_group(pid, Signal::Kill)
    {
        tracing::debug!(%error, pid, "kill during failed spawn");
    }
    if let Err(error) = child.wait() {
        tracing::debug!(%error, "reaping during failed spawn");
    }
}

type ChildSlot = Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>>;

/// Starts the helper threads. `Core::live` was preset to [`HELPER_THREADS`]; a failed start
/// releases the shares of the threads that did not start.
fn start_threads(
    core: &Arc<Core>,
    child: &ChildSlot,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    write_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<(), TerminalError> {
    let release = |unstarted: u32, error: TerminalError| {
        *core.live.lock() -= unstarted;
        core.live_changed.notify_all();
        // Unblock the threads that did start so they exit.
        drop(core.writer.lock().take());
        error
    };
    let spawn = |name: &'static str, body: Box<dyn FnOnce() + Send>| {
        thread::Builder::new()
            .name(name.into())
            .spawn(body)
            .map(drop)
            .map_err(|source| TerminalError::Thread { name, source })
    };

    let c = Arc::clone(core);
    spawn("hutan-term-read", Box::new(move || reader_loop(c, reader)))
        .map_err(|e| release(HELPER_THREADS, e))?;
    let c = Arc::clone(core);
    spawn(
        "hutan-term-write",
        Box::new(move || writer_loop(c, writer, write_rx)),
    )
    .map_err(|e| release(HELPER_THREADS - 1, e))?;
    let c = Arc::clone(core);
    spawn("hutan-term-sync", Box::new(move || sync_loop(c)))
        .map_err(|e| release(HELPER_THREADS - 2, e))?;
    let c = Arc::clone(core);
    let slot = Arc::clone(child);
    spawn(
        "hutan-term-wait",
        Box::new(move || {
            // `take` can only fail if the slot was already emptied by the failure path.
            let child = slot.lock().take();
            if let Some(child) = child {
                waiter_loop(c, child);
            }
        }),
    )
    .map_err(|e| release(0, e))
}

fn reader_loop(core: Arc<Core>, reader: Box<dyn Read + Send>) {
    let _live = LiveGuard(Arc::clone(&core));
    let mut reader = reader;
    let mut buf = vec![0u8; READ_BUFFER];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => {
                let mut engine = core.engine.lock();
                let visible = engine.feed(&buf[..read]);
                core.publish(&mut engine, visible);
                if engine.sync_deadline().is_some() {
                    core.sync_wake.notify_one();
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                tracing::debug!(%error, "PTY read ended");
                break;
            }
        }
    }
}

fn writer_loop(core: Arc<Core>, writer: Box<dyn Write + Send>, rx: mpsc::Receiver<Vec<u8>>) {
    let _live = LiveGuard(core);
    let mut writer = writer;
    while let Ok(bytes) = rx.recv() {
        if let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
            tracing::debug!(%error, "PTY write failed; closing the input side");
            break;
        }
    }
}

fn sync_loop(core: Arc<Core>) {
    let _live = LiveGuard(Arc::clone(&core));
    let mut engine = core.engine.lock();
    while !core.shutdown.load(Ordering::Acquire) {
        match engine.sync_deadline() {
            None => core.sync_wake.wait(&mut engine),
            Some(deadline) if Instant::now() >= deadline => {
                engine.stop_sync();
                core.publish(&mut engine, true);
            }
            Some(deadline) => {
                core.sync_wake.wait_until(&mut engine, deadline);
            }
        }
    }
}

fn waiter_loop(core: Arc<Core>, child: Box<dyn Child + Send + Sync>) {
    // The raw wait status carries the signal number, which `portable-pty`'s own `wait` turns
    // into `strsignal` text; reap the pid directly. `child` stays alive until after the wait.
    let status = match child.process_id() {
        Some(pid) => match sys::wait_child(pid) {
            Ok(reaped) => ExitStatus::from(reaped),
            Err(error) => {
                tracing::warn!(%error, "waiting for the child failed");
                lost_status()
            }
        },
        None => {
            tracing::warn!("child has no pid; exit status unavailable");
            lost_status()
        }
    };
    drop(child);
    // Reaped: the pid may be recycled from here on, never signal it again.
    core.pid.store(0, Ordering::Release);
    release_pty(&core);
    {
        let mut slot = core.exit_status.lock();
        *slot = Some(status.clone());
        core.exit_changed.notify_all();
    }
    core.exit_tx.send_replace(Some(status));
}

/// Closes the input queue, stops the sync thread, waits (bounded) for the reader to see EOF
/// — so the child's final output reaches the engine before the exit is reported — and then
/// closes the PTY master.
fn release_pty(core: &Core) {
    drop(core.writer.lock().take());
    {
        // Set under the engine lock: the sync thread checks the flag and waits atomically
        // with respect to it, so the wake-up below cannot be lost.
        let _engine = core.engine.lock();
        core.shutdown.store(true, Ordering::Release);
    }
    core.sync_wake.notify_all();

    let deadline = Instant::now() + TEARDOWN_WAIT;
    let mut live = core.live.lock();
    while *live > 0 {
        if core
            .live_changed
            .wait_until(&mut live, deadline)
            .timed_out()
        {
            tracing::debug!(
                live = *live,
                "PTY helper threads still running after the child exited"
            );
            break;
        }
    }
    drop(live);
    drop(core.master.lock().take());
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::*;

    /// `Debug` is not enough to assert on a `CommandBuilder`; read its resulting environment.
    fn env_of(cmd: &CommandBuilder) -> BTreeMap<String, String> {
        cmd.iter_full_env_as_str()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    }

    fn source() -> EnvSource {
        EnvSource {
            facts: SystemFacts {
                home: Some("/Users/ada".into()),
                user: Some("ada".into()),
                shell: Some("/bin/zsh".into()),
                temp_dir: Some("/var/folders/xx/T/".into()),
                path_helper: Some("PATH=\"/usr/local/bin:/usr/bin:/bin\"; export PATH;".into()),
                ..SystemFacts::default()
            },
            login_env: None,
            daemon_env: vec![
                (OsString::from("HUTAN_SENTINEL"), OsString::from("leaked")),
                (OsString::from("PATH"), OsString::from("/daemon/bin")),
                (
                    OsString::from("VIRTUAL_ENV"),
                    OsString::from("/daemon/venv"),
                ),
                (OsString::from("TERM"), OsString::from("dumb")),
            ],
        }
    }

    fn config(env_inherit: EnvInherit, env: &[(&str, &str)]) -> TerminalConfig {
        TerminalConfig {
            argv: vec!["/usr/bin/env".to_owned()],
            env_inherit,
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..TerminalConfig::default()
        }
    }

    #[test]
    fn login_mode_starts_from_the_facts_and_never_sees_the_daemons_environment() {
        let env = env_of(&command(&config(EnvInherit::Login, &[]), &source()));
        assert_eq!(
            env.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "COLORTERM",
                "HOME",
                "LANG",
                "LOGNAME",
                "PATH",
                "SHELL",
                "TERM",
                "TERM_PROGRAM",
                "TMPDIR",
                "USER"
            ]
        );
        assert_eq!(env["PATH"], "/usr/local/bin:/usr/bin:/bin");
        assert_eq!(env["TERM"], "xterm-256color", "not the daemon's TERM");
        assert!(!env.contains_key("HUTAN_SENTINEL") && !env.contains_key("VIRTUAL_ENV"));
    }

    #[test]
    fn daemon_mode_keeps_the_daemons_environment_and_none_of_the_facts() {
        let env = env_of(&command(&config(EnvInherit::Daemon, &[]), &source()));
        assert_eq!(env["HUTAN_SENTINEL"], "leaked");
        assert_eq!(env["PATH"], "/daemon/bin");
        assert_eq!(env["VIRTUAL_ENV"], "/daemon/venv");
        assert!(!env.contains_key("HOME") && !env.contains_key("TMPDIR"));
        assert_eq!(
            env["TERM"], "xterm-256color",
            "terminal variables still win"
        );
    }

    #[test]
    fn precedence_is_base_then_client_then_terminal_variables() {
        let cfg = config(
            EnvInherit::Login,
            &[
                ("PATH", "/client/bin"),
                ("EDITOR", "vim"),
                ("HOME", "/client/home"),
                ("TERM", "vt100"),
                ("COLORTERM", "no"),
                ("TERM_PROGRAM", "other"),
            ],
        );
        let env = env_of(&command(&cfg, &source()));
        assert_eq!(env["PATH"], "/client/bin", "client beats the base");
        assert_eq!(env["HOME"], "/client/home");
        assert_eq!(env["EDITOR"], "vim");
        assert_eq!(
            env["TERM"], "xterm-256color",
            "terminal variables beat the client"
        );
        assert_eq!(env["COLORTERM"], "truecolor");
        assert_eq!(env["TERM_PROGRAM"], "hutan");
        assert_eq!(env["USER"], "ada", "untouched base facts stay");
    }

    #[test]
    fn explicit_commands_start_from_base_then_login_shell_then_client_then_terminal() {
        let mut source = source();
        source.login_env = Some(BTreeMap::from([
            ("PATH".to_owned(), "/login/bin:/usr/bin".to_owned()),
            ("HOMEBREW_PREFIX".to_owned(), "/opt/homebrew".to_owned()),
            ("EDITOR".to_owned(), "from-login".to_owned()),
            ("TERM".to_owned(), "from-login".to_owned()),
            ("HUTAN_SESSION_ID".to_owned(), "from-login".to_owned()),
        ]));
        let cfg = config(
            EnvInherit::Login,
            &[
                ("EDITOR", "from-client"),
                ("HUTAN_SESSION_ID", "daemon-managed"),
            ],
        );
        let env = env_of(&command(&cfg, &source));
        assert_eq!(
            env["PATH"], "/login/bin:/usr/bin",
            "login shell beats the base"
        );
        assert_eq!(env["HOMEBREW_PREFIX"], "/opt/homebrew");
        assert_eq!(
            env["USER"], "ada",
            "base facts the login shell did not set stay"
        );
        assert_eq!(env["EDITOR"], "from-client", "client beats the login shell");
        assert_eq!(
            env["TERM"], "xterm-256color",
            "terminal variables beat both"
        );
        assert_eq!(env["HUTAN_SESSION_ID"], "daemon-managed");
        assert!(!env.contains_key("VIRTUAL_ENV") && !env.contains_key("HUTAN_SENTINEL"));
    }

    #[test]
    fn integration_variables_beat_the_client_and_the_daemon_managed_ones_survive() {
        let root = Path::new("/integration");
        let mut cfg = config(
            EnvInherit::Login,
            &[
                ("HUTAN_SESSION_ID", "daemon-managed"),
                ("HUTAN_SHELL_INTEGRATION_DIR", "/client/dir"),
            ],
        );
        cfg.argv = vec!["/bin/zsh".to_owned(), "-l".to_owned()];
        cfg.shell_integration_dir = Some(root.to_owned());
        let env = env_of(&command(&cfg, &source()));
        assert_eq!(env["HUTAN_SHELL_INTEGRATION_DIR"], "/integration");
        assert_eq!(env["ZDOTDIR"], "/integration/zsh");
        assert_eq!(env["HUTAN_SESSION_ID"], "daemon-managed");
    }

    #[test]
    fn an_explicit_shell_in_the_session_env_still_selects_the_shell() {
        let mut cfg = config(EnvInherit::Login, &[("SHELL", "/bin/sh")]);
        cfg.argv.clear();
        let cmd = command(&cfg, &source());
        assert_eq!(
            cmd.get_argv()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["/bin/sh", "-l"]
        );
        assert_eq!(env_of(&cmd)["SHELL"], "/bin/sh");

        // Without one the account's login shell is used and `$SHELL` says so.
        let mut cfg = config(EnvInherit::Login, &[]);
        cfg.argv.clear();
        let cmd = command(&cfg, &source());
        let shell = default_shell(&BTreeMap::new());
        assert_eq!(cmd.get_argv()[0].to_string_lossy(), shell);
        assert_eq!(env_of(&cmd)["SHELL"], shell);
    }
}
