//! `hutand`: the Hutan daemon.
//!
//! Environment: `HUTAN_HOME` (base directory), `HUTAN_SOCKET` (socket path only),
//! `HUTAN_LOG` or `RUST_LOG` (log filter, default `info`). Exit codes: 0 clean stop,
//! 1 failure, 2 another daemon already holds the lock, 64 unknown argument.
//!
//! Arguments: none to run; `--version` / `-V` and `--help` / `-h` print and exit without
//! touching any directory (anything else is refused, so a mistyped flag never starts a daemon
//! on the real home).
//!
//! A start that fails says why in three places (architecture.md § Start errors): stderr, the
//! log (`hutand.log`, the whole error chain) and the start-error record next to the socket
//! (`hutand-start-error.json`, [`hutan_daemon::start_error`]), which a successful start
//! removes. Log lines go to stderr too while the daemon starts; once it serves, a stderr that
//! is not a terminal (the clients' `hutand-start.log`) gets nothing more, and it never gets
//! more than [`STDERR_BUDGET`] bytes of them.
//!
//! Threads: tokio runs on a thread of its own. When the daemon runs from `Hutan.app` the main
//! thread serves the Cocoa run loop of the native notifications and of the daemon's own menu
//! bar item (`notify_native`, `menu_bar`), because the system's notification centre delivers its
//! callbacks there and AppKit needs an event loop there; otherwise the main thread just waits
//! for the runtime thread.

use std::fs::OpenOptions;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use hutan_daemon::menu_bar::MenuBar;
use hutan_daemon::notify_native::{self, NativePoster, Stopper};
use hutan_daemon::{DaemonConfig, DaemonError, Paths, start_error};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

const EXIT_ALREADY_RUNNING: u8 = 2;
const EXIT_USAGE: u8 = 64;
/// Most bytes of log lines written to a stderr that is not a terminal.
const STDERR_BUDGET: usize = 64 * 1024;

const HELP: &str = "hutand: the Hutan daemon (normally started by `hutan` or the app)

Usage: hutand [--version | --help]

Environment: HUTAN_HOME (base directory), HUTAN_SOCKET (socket path), HUTAN_LOG (log filter)";

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("--version" | "-V") => {
            println!("hutand {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h") => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("hutand: unknown argument `{other}`\n\n{HELP}");
            return ExitCode::from(EXIT_USAGE);
        }
    }
    // Without paths there is neither a log nor a place for the record: stderr is all there is.
    let paths = match Paths::from_env() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("hutand: {error}");
            return ExitCode::FAILURE;
        }
    };
    let started = Arc::new(AtomicBool::new(false));
    match run(&paths, &started) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("hutand: {failure:#}");
            if started.load(Ordering::Acquire) {
                error!(error = %format!("{failure:#}"), "hutand failed");
            } else {
                error!(error = %format!("{failure:#}"), "hutand could not start");
                start_error::write(&paths, &failure);
            }
            let already_running = failure.chain().any(|cause| {
                matches!(
                    cause.downcast_ref(),
                    Some(DaemonError::AlreadyRunning { .. })
                )
            });
            if already_running {
                ExitCode::from(EXIT_ALREADY_RUNNING)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// The runtime thread's stack: what the main thread has, where the daemon used to run.
const RUNTIME_STACK_BYTES: usize = 8 * 1024 * 1024;

/// Runs the daemon until it stops; `started` turns true once it serves.
fn run(paths: &Paths, started: &Arc<AtomicBool>) -> anyhow::Result<()> {
    paths.prepare().context("preparing directories")?;
    let stderr = init_logging(paths)?;

    // `Some` only inside Hutan.app: nothing is asked of the system before the first post or
    // the first time the menu bar item has something to show.
    let host = notify_native::system_host();
    let poster = host.as_ref().map(notify_native::MainThreadHost::poster);
    let menu_bar = host.as_ref().map(notify_native::MainThreadHost::menu_bar);
    let stopper = host.as_ref().map(notify_native::MainThreadHost::stopper);
    let started = started.clone();
    let paths = paths.clone();
    let runtime = std::thread::Builder::new()
        .name("hutand-runtime".to_owned())
        .stack_size(RUNTIME_STACK_BYTES)
        .spawn(move || {
            // However the daemon ends (a panic included), the main thread's loop must too.
            let _stop = stopper.map(StopOnDrop);
            run_daemon(paths, poster, menu_bar, &started, &stderr)
        })
        .context("starting the runtime thread")?;
    if let Some(host) = host {
        host.run();
    }
    runtime.join().map_err(|payload| {
        let what = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".to_owned());
        anyhow::anyhow!("the runtime thread panicked: {what}")
    })?
}

/// Ends the main thread's run loop when dropped.
struct StopOnDrop(Stopper);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop();
    }
}

fn run_daemon(
    paths: Paths,
    native: Option<Arc<dyn NativePoster>>,
    menu_bar: Option<MenuBar>,
    started: &AtomicBool,
    stderr: &StderrBudget,
) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(async {
        let mut config = DaemonConfig::new(paths.clone());
        config.native = native;
        config.menu_bar = menu_bar;
        let daemon = hutan_daemon::start(config).await?;
        start_error::clear(&paths);
        started.store(true, Ordering::Release);
        stderr.quiet_unless_terminal();

        let token = daemon.shutdown_token();
        let mut sigterm = signal(SignalKind::terminate()).context("installing SIGTERM handler")?;
        let mut sigint = signal(SignalKind::interrupt()).context("installing SIGINT handler")?;
        tokio::spawn(async move {
            let name = tokio::select! {
                _ = sigterm.recv() => "SIGTERM",
                _ = sigint.recv() => "SIGINT",
                _ = token.cancelled() => return,
            };
            info!(signal = name, "signal received");
            token.cancel();
        });

        daemon.wait().await?;
        // A record written while this daemon ran came from a start that found the lock held
        // by it; with the lock released that no longer explains anything.
        start_error::clear(&paths);
        Ok(())
    })
}

/// Logs to `hutand.log` and, within [`StderrBudget`], to stderr.
fn init_logging(paths: &Paths) -> anyhow::Result<StderrBudget> {
    let log_path = paths.log_file();
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    let filter = EnvFilter::try_from_env("HUTAN_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let terminal = std::io::stderr().is_terminal();
    let stderr = StderrBudget {
        left: Arc::new(AtomicUsize::new(if terminal {
            usize::MAX
        } else {
            STDERR_BUDGET
        })),
        terminal,
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_ansi(terminal).with_writer(stderr.clone()))
        .with(fmt::layer().with_ansi(false).with_writer(Mutex::new(file)))
        .init();
    Ok(stderr)
}

/// What log output may still go to stderr: everything on a terminal; otherwise (a client's
/// `hutand-start.log`, launchd's `/dev/null`) at most [`STDERR_BUDGET`] bytes, and nothing once
/// the daemon serves.
#[derive(Clone)]
struct StderrBudget {
    left: Arc<AtomicUsize>,
    terminal: bool,
}

impl StderrBudget {
    fn quiet_unless_terminal(&self) {
        if !self.terminal {
            self.left.store(0, Ordering::Release);
        }
    }
}

impl<'a> MakeWriter<'a> for StderrBudget {
    type Writer = BudgetedStderr;

    fn make_writer(&'a self) -> Self::Writer {
        BudgetedStderr(self.left.clone())
    }
}

struct BudgetedStderr(Arc<AtomicUsize>);

impl Write for BudgetedStderr {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let before = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                Some(left.saturating_sub(buf.len()))
            })
            .unwrap_or(0);
        let allowed = before.min(buf.len());
        if allowed > 0 {
            std::io::stderr().write_all(&buf[..allowed])?;
        }
        // What is over the budget is dropped, not retried.
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}
