//! Daemon lifecycle: single-instance lock, socket binding, accept loop, orderly shutdown.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use hutan_agents::CodexConfig;
use hutan_core::Epoch;
use hutan_storage::Storage;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::accounts::AccountService;
use crate::agent;
use crate::assistant::AssistantService;
use crate::bus::{EventBus, RingLimits};
use crate::chat;
use crate::checkpoints::turns::{self as checkpoint_turns, TurnCheckpoints};
use crate::config::DaemonSettings;
use crate::conn::{self, ConnContext};
use crate::control::ControlService;
use crate::daemon_settings::{SettingsService, Targets};
use crate::error::{DaemonError, MAX_SOCKET_PATH_BYTES};
use crate::files::FileService;
use crate::harness::Harnesses;
use crate::history::HistoryService;
use crate::hooks::{HooksInputs, HooksService};
use crate::lsp::{Inputs as LspInputs, LspHub, LspTuning};
use crate::mcp_wiring::McpWiring;
use crate::memory::MemoryService;
use crate::menu_bar::{self, MenuBar};
use crate::notify::NotifyService;
use crate::notify_native::NativePoster;
use crate::paths::Paths;
use crate::project_migration;
use crate::projects;
use crate::providers::{GatewayConfig, ProviderService};
use crate::registry::{self, RegistryConfig};
use crate::resources::{ResourceService, SystemProcesses};
use crate::restore::{self, Stored};
use crate::router::{Router, Services};
use crate::tasks::TaskService;
use crate::terminals::TerminalHub;
use crate::uihub::UiHub;
use crate::usage::UsageService;
use crate::worktrees::{WorktreeConfig, WorktreeService};

pub const DEFAULT_OUTBOUND_QUEUE: usize = 1024;

/// A connection that has not sent `hello` after this long is closed.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long shutdown waits for connections to flush before abandoning them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How long shutdown waits for the daemon's menu bar item to take itself off.
const MENU_BAR_STOP_WAIT: Duration = Duration::from_secs(2);

/// How long shutdown waits for session processes to end: the SIGHUP grace plus margin for the
/// SIGKILL and reaping that follow it.
const SESSION_STOP_WAIT: Duration = registry::STOP_GRACE.saturating_add(Duration::from_secs(5));

/// How long shutdown waits for the checkpoints of the turns the stop cut off.
const CHECKPOINT_DRAIN_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub paths: Paths,
    pub ring: RingLimits,
    /// Per-connection outbound queue length in control messages.
    pub outbound_queue: usize,
    /// How long a new connection may stay silent before `hello` arrives.
    pub handshake_timeout: Duration,
    /// The `hutan` CLI exported to sessions as `HUTAN_BIN`; `None` looks for it next to this
    /// executable, then on `PATH`.
    pub hutan_bin: Option<PathBuf>,
    /// Shortest time between two `last_active_ms` updates of one workspace.
    pub activity_interval: Duration,
    /// How to start `codex app-server`; `None` follows `[codex]` of `daemon.toml`.
    pub codex: Option<CodexConfig>,
    /// Credential store, HTTP policy and discovery targets of the model gateway; the default is
    /// the Keychain and the well-known local ports.
    pub gateway: GatewayConfig,
    /// The shell of sessions whose spec names none (as `SHELL` in the session environment),
    /// instead of the account's login shell; `None` keeps the account's. Also the shell whose
    /// login environment explicit-command sessions start from. For tests, which need a
    /// deterministic one.
    pub login_shell: Option<PathBuf>,
    /// Where intents are shown as system notifications: the real poster of a daemon that runs
    /// from `Hutan.app` (`notify_native::system_host`), a fake in tests; `None` posts nothing.
    /// `[notify] native = false` ignores it.
    pub native: Option<Arc<dyn NativePoster>>,
    /// The daemon's own menu bar item, shown while no desktop is connected: the real one of a
    /// daemon that runs from `Hutan.app` (`notify_native::system_host`), a fake in tests;
    /// `None` shows nothing. Not affected by `[notify] native`.
    pub menu_bar: Option<MenuBar>,
    /// How long `ui.run` and `ui.set_setting` wait for the desktop to answer (protocol 0.21;
    /// 5 s). Tests shorten it.
    pub ui_command_timeout: Duration,
    /// How often the daemon checks that its socket is still its own (default 5 s): a daemon
    /// whose socket was deleted (its `HUTAN_HOME` removed) or replaced can never be reached
    /// again and stops instead of running forever. Tests shorten it.
    pub socket_check_interval: Duration,
    /// Overrides `[assistant] idle_stop_secs` at start (a finer unit, and below its minimum of
    /// 60 s): how long an idle harness assistant keeps its agent process. `None` reads the
    /// setting. Tests shorten it.
    pub assistant_idle_stop: Option<Duration>,
    /// The bounds of the file service (protocol 0.33): 500 changes per `fs.changed` event,
    /// 30 s without a chunk before an upload is discarded. Tests lower them.
    pub fs_limits: crate::files::Limits,
    /// The start budget of a turn's checkpoint (5 s) and the environment of its git calls.
    /// Tests lower the budget and put a slow fake git first on `PATH`.
    pub checkpoints: checkpoint_turns::Tuning,
    /// What tests change of the language servers' timing (idle stop, coalescing, restarts).
    pub lsp: LspTuning,
    /// `usage.summary` with `refresh` asks the harnesses what is left of their plans: `omp usage
    /// --json` and a short-lived `codex app-server`. They answer over the network with the
    /// user's own logins, so only `hutand` turns this on; a test that wants it points the
    /// harness CLIs at fakes first.
    pub usage_probes: bool,
}

impl DaemonConfig {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            ring: RingLimits::default(),
            outbound_queue: DEFAULT_OUTBOUND_QUEUE,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            hutan_bin: None,
            activity_interval: registry::ACTIVITY_INTERVAL,
            codex: None,
            gateway: GatewayConfig::default(),
            login_shell: None,
            native: None,
            menu_bar: None,
            ui_command_timeout: Duration::from_millis(hutan_protocol::ui::COMMAND_TIMEOUT_MS),
            socket_check_interval: SOCKET_CHECK_INTERVAL,
            assistant_idle_stop: None,
            fs_limits: crate::files::Limits::default(),
            checkpoints: checkpoint_turns::Tuning::default(),
            lsp: LspTuning::default(),
            usage_probes: false,
        }
    }
}

/// A started daemon. Dropping it does not stop the daemon; call [`shutdown`](Self::shutdown).
#[derive(Debug)]
pub struct RunningDaemon {
    socket: PathBuf,
    epoch: Epoch,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl RunningDaemon {
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// Token that stops the daemon when cancelled (what signal handlers trigger).
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Requests an orderly stop; returns immediately. Pair with [`wait`](Self::wait).
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Resolves once the daemon has stopped: connections closed, socket removed, lock released.
    pub async fn wait(self) -> Result<(), DaemonError> {
        Ok(self.task.await?)
    }
}

/// Acquires the single-instance lock, binds the socket, and starts serving.
pub async fn start(config: DaemonConfig) -> Result<RunningDaemon, DaemonError> {
    let paths = &config.paths;
    let socket_len = paths.socket.as_os_str().as_bytes().len();
    if socket_len > MAX_SOCKET_PATH_BYTES {
        return Err(DaemonError::SocketPathTooLong {
            path: paths.socket.clone(),
            len: socket_len,
        });
    }

    paths.prepare().map_err(DaemonError::io(format!(
        "preparing {}",
        paths.base.display()
    )))?;
    let lock = acquire_lock(&paths.lock)?;
    // Under the lock nobody else touches the database. A daemon that cannot read its config or
    // open its database must not come up: it would serve an empty state and then overwrite
    // the real one.
    let settings = DaemonSettings::load(&paths.config_file, paths.user_home.as_deref())?;
    let home_folder = settings
        .home_folder
        .clone()
        .unwrap_or_else(|| paths.default_home_folder.clone());
    let home_folder = prepare_home_folder(home_folder).await?;
    let (storage, mut stored) = open_storage(&paths.database).await?;
    stored = convert_workspaces(&storage, stored, &paths.database).await?;
    remove_stale_socket(&paths.socket)?;
    let listener = UnixListener::bind(&paths.socket).map_err(DaemonError::io(format!(
        "binding {}",
        paths.socket.display()
    )))?;
    // From here on the guard removes the socket on every exit path (only while it is ours).
    let socket_identity = socket_identity(&paths.socket);
    let socket_guard = SocketGuard {
        path: paths.socket.clone(),
        identity: socket_identity,
    };
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600)).map_err(
        DaemonError::io(format!("securing {}", paths.socket.display())),
    )?;
    let daemon_uid = fs::metadata(&paths.socket)
        .map_err(DaemonError::io(format!(
            "inspecting {}",
            paths.socket.display()
        )))?
        .uid();

    let epoch = Epoch::new();
    if let Err(error) = storage.app_meta().record_epoch(epoch, registry::now_ms()) {
        warn!(%error, "could not record the epoch");
    }
    let shutdown = CancellationToken::new();
    let clients = Arc::new(AtomicU32::new(0));
    let bus = Arc::new(EventBus::new(epoch, config.ring));
    let hub = Arc::new(TerminalHub::new());
    // Best effort: without the scripts terminals still work, they just carry no prompt marks.
    let shell_integration_dir = match hutan_terminal::shell_integration::install(&paths.base) {
        Ok(dir) => Some(dir),
        Err(error) => {
            warn!(%error, "shell integration unavailable");
            None
        }
    };
    if settings.terminal.env_inherit == hutan_terminal::EnvInherit::Login {
        // The facts of the login environment come from a few subprocesses; do not make the
        // first session (or a restored one) wait for them.
        hutan_terminal::env::warm_up(
            config
                .login_shell
                .as_ref()
                .map(|shell| shell.to_string_lossy().into_owned()),
        );
    }
    let restoring = stored.sessions.len();
    let stored_tasks = std::mem::take(&mut stored.tasks);
    let providers = ProviderService::new(
        storage.clone(),
        config
            .gateway
            .credentials
            .unwrap_or_else(hutan_gateway::system_store),
        hutan_gateway::Http::new(config.gateway.http)
            .map_err(|e| DaemonError::Gateway(e.to_string()))?,
        config
            .gateway
            .discovery
            .unwrap_or_else(hutan_gateway::discovery::default_targets),
        std::mem::take(&mut stored.providers),
    );
    let login_shell = config
        .login_shell
        .as_ref()
        .map(|shell| shell.to_string_lossy().into_owned());
    let harnesses = Arc::new(Harnesses::new(
        settings.harness.clone(),
        &settings.mcp,
        settings.terminal.env_inherit,
        login_shell,
    ));
    let codex_config = config.codex.unwrap_or_else(|| settings.codex.to_config());
    let hutan_bin = config
        .hutan_bin
        .clone()
        .unwrap_or_else(agent::resolve_hutan_bin);
    // Checkpoints of chat turns: what earlier daemons left is reconciled in the background
    // (rows created from now on are this daemon's), then retention runs once a day.
    let checkpoints = TurnCheckpoints::new(
        storage.clone(),
        &paths.base,
        paths.user_home.clone(),
        settings.checkpoints.clone(),
        config.checkpoints.clone(),
    );
    tokio::spawn(checkpoints.clone().upkeep(shutdown.clone()));
    let usage = UsageService::new(
        storage.clone(),
        bus.clone(),
        harnesses.clone(),
        codex_config.clone(),
        config.usage_probes,
    );
    let registry = registry::spawn(
        bus.clone(),
        hub.clone(),
        RegistryConfig {
            socket: paths.socket.clone(),
            shell_integration_dir,
            env_inherit: settings.terminal.env_inherit,
            login_shell: config
                .login_shell
                .as_ref()
                .map(|shell| shell.to_string_lossy().into_owned()),
            hutan_bin: hutan_bin.clone(),
            storage: storage.clone(),
            restore: settings.restore_sessions,
            home_folder,
            user_home: paths.user_home.clone(),
            auto_projects: settings.projects.auto,
            activity_interval: config.activity_interval,
            codex: codex_config.clone(),
            harnesses: harnesses.clone(),
            providers,
            max_context_chars: settings.gateway.max_context_chars,
            idle_stop: config
                .assistant_idle_stop
                .unwrap_or(Duration::from_secs(settings.assistant.idle_stop_secs)),
            assistant_policy: crate::chat::AssistantPolicy {
                mode: settings.assistant.mode,
                max_actions: settings.assistant.max_actions_per_turn,
            },
            tmp_dir: paths.base.join("tmp"),
            scratch_dir: paths.base.join("scratch"),
            checkpoints: checkpoints.clone(),
            usage: usage.clone(),
        },
        stored,
    );
    let history = HistoryService::new(
        paths.clone(),
        settings.history.clone(),
        bus.clone(),
        hub.clone(),
        registry.clone(),
    );
    history.start().await;
    let accounts = AccountService::new(
        registry.clone(),
        harnesses.clone(),
        codex_config,
        bus.clone(),
        shutdown.clone(),
    );
    let assistant = AssistantService::new(
        settings.assistant.clone(),
        registry.clone(),
        hub.clone(),
        history.clone(),
        storage.clone(),
        accounts.clone(),
    );
    // Told when something the menu bar item lists may have changed: events, and intents that
    // leave the pending list (no event says so).
    let menu_bar = config.menu_bar.map(|menu_bar| {
        let (changes_tx, changes_rx) = mpsc::channel(1);
        bus.set_attention(Some(changes_tx.clone()));
        (menu_bar, changes_tx, changes_rx)
    });
    let notify = NotifyService::new(
        settings.notify.clone(),
        bus.clone(),
        registry.clone(),
        config.native,
    );
    if let Some((_, changes_tx, _)) = &menu_bar {
        notify.set_changed(changes_tx.clone());
    }
    // Observes the bus until shutdown; nothing needs its handle.
    drop(notify.start(shutdown.clone()));
    let worktrees = WorktreeService::new(
        registry.clone(),
        storage.clone(),
        bus.clone(),
        harnesses.clone(),
        WorktreeConfig {
            finish: settings.finish,
            worktrees: settings.worktrees.clone(),
        },
        paths.user_home.clone(),
        checkpoints.clone(),
    );
    // Observes the sessions until shutdown: a worktree's Setup session ends there.
    drop(worktrees.run(shutdown.clone()));
    // After the registry restored its sessions and the notifier listens: a task whose session
    // did not survive the last daemon ends here, and says so.
    let tasks = TaskService::open(
        registry.clone(),
        worktrees.clone(),
        checkpoints.clone(),
        storage.clone(),
        bus.clone(),
        &settings.tasks,
        stored_tasks,
    )
    .await
    .map_err(|error| DaemonError::Tasks(error.message))?;
    drop(tasks.run(shutdown.clone()));
    let processes = Arc::new(SystemProcesses);
    let lsp = LspHub::new(LspInputs {
        settings: settings.lsp.clone(),
        registry: registry.clone(),
        bus: bus.clone(),
        storage: storage.clone(),
        source: processes.clone(),
        harnesses: harnesses.clone(),
        tuning: config.lsp.clone(),
        shutdown: shutdown.clone(),
    })
    .map_err(DaemonError::Lsp)?;
    let resources = ResourceService::new(
        registry.clone(),
        hub.clone(),
        bus.clone(),
        processes,
        lsp.clone(),
    );
    let control = ControlService::new(
        settings.control,
        bus.clone(),
        storage.clone(),
        registry.clone(),
        shutdown.clone(),
    );
    let memory = MemoryService::new(
        &settings.assistant,
        bus.clone(),
        registry.clone(),
        storage.clone(),
        &paths.base,
    );
    let files = FileService::new(
        registry.clone(),
        bus.clone(),
        shutdown.clone(),
        config.fs_limits,
    );
    let ui = UiHub::with_timeout(bus.clone(), config.ui_command_timeout);
    let hooks = HooksService::new(HooksInputs {
        settings: settings.hooks.clone(),
        bus: bus.clone(),
        registry: registry.clone(),
        ui: ui.clone(),
        harnesses: harnesses.clone(),
        socket: paths.socket.clone(),
        hutan_bin: hutan_bin.clone(),
        home: paths.user_home.clone(),
    });
    hooks.start(shutdown.clone());
    let menu_bar = menu_bar.map(|(menu_bar, _, changes)| {
        (
            menu_bar,
            changes,
            notify.clone(),
            registry.clone(),
            ui.clone(),
        )
    });
    registry.chats().set_mcp_wiring(McpWiring {
        enabled: settings.control.mcp,
        hutan_bin,
        socket: paths.socket.clone(),
    });
    let settings_service = SettingsService::new(
        paths.config_file.clone(),
        paths.user_home.clone(),
        settings.clone(),
        Targets {
            history: history.clone(),
            assistant: assistant.clone(),
            notify: notify.clone(),
            tasks: tasks.clone(),
            control: control.clone(),
            memory: memory.clone(),
            registry: registry.clone(),
            worktrees: worktrees.clone(),
        },
    );
    let router = Router::new(
        registry,
        hub,
        Services {
            history,
            assistant,
            notify,
            worktrees,
            tasks,
            accounts,
            usage,
            resources,
            control,
            memory,
            files,
            lsp,
            ui,
            hooks,
            settings: settings_service,
        },
        bus.clone(),
        shutdown.clone(),
        clients.clone(),
    );
    let ctx = Arc::new(ConnContext::new(
        router,
        bus.clone(),
        shutdown.clone(),
        clients,
        config.outbound_queue.max(2),
        daemon_uid,
        config.handshake_timeout,
    ));
    // Shows the item while no desktop is connected, until shutdown takes it off.
    let menu_bar = menu_bar.map(|(menu_bar, changes, notify, registry, ui)| {
        menu_bar::start(menu_bar::Inputs {
            menu_bar,
            desktops: ctx.desktops.watch(),
            changes,
            notify,
            registry,
            bus,
            ui,
            settings_file: paths.base.join("settings.toml"),
            desktop_socket: paths.base.join(menu_bar::DESKTOP_SOCKET_FILE),
            shutdown: shutdown.clone(),
        })
    });

    info!(
        socket = %paths.socket.display(),
        database = %paths.database.display(),
        restored_sessions = restoring,
        restore_mode = ?settings.restore_sessions,
        pid = std::process::id(),
        %epoch,
        protocol = ?hutan_protocol::PROTOCOL_VERSION,
        "hutand listening"
    );

    let held = Held {
        _socket: socket_guard,
        _lock: lock,
    };
    tokio::spawn(watch_socket(
        paths.socket.clone(),
        socket_identity,
        config.socket_check_interval,
        shutdown.clone(),
    ));
    let task = tokio::spawn(accept_loop(
        listener,
        ctx,
        shutdown.clone(),
        held,
        storage,
        checkpoints,
        menu_bar,
        epoch,
    ));
    Ok(RunningDaemon {
        socket: paths.socket.clone(),
        epoch,
        shutdown,
        task,
    })
}

/// Resources released at the end of the daemon's life. Field order matters: the socket is
/// removed before the lock is released, so a successor never sees our socket.
#[derive(Debug)]
struct Held {
    _socket: SocketGuard,
    _lock: File,
}

/// The default of [`DaemonConfig::socket_check_interval`].
pub const SOCKET_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Device and inode of the socket file at `path`, when it is a socket.
fn socket_identity(path: &Path) -> Option<(u64, u64)> {
    fs::symlink_metadata(path)
        .ok()
        .filter(|meta| meta.file_type().is_socket())
        .map(|meta| (meta.dev(), meta.ino()))
}

/// Stops the daemon once its socket is gone or another file took its place: nobody can
/// connect to it any more (its home was deleted, or a successor bound the path), so staying
/// up would only leak the process and its sessions.
async fn watch_socket(
    socket: PathBuf,
    bound: Option<(u64, u64)>,
    every: Duration,
    shutdown: CancellationToken,
) {
    let Some(bound) = bound else {
        return;
    };
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = sleep(every) => {}
        }
        let path = socket.clone();
        let now = tokio::task::spawn_blocking(move || socket_identity(&path))
            .await
            .ok()
            .flatten();
        if now != Some(bound) {
            warn!(
                socket = %socket.display(),
                "the socket is gone or was replaced; nobody can reach this daemon, stopping"
            );
            shutdown.cancel();
            return;
        }
    }
}

/// Removes the daemon's socket when the daemon ends, unless the file at that path is no longer
/// the one it bound (a successor's socket is left alone).
#[derive(Debug)]
struct SocketGuard {
    path: PathBuf,
    identity: Option<(u64, u64)>,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        if self.identity.is_none() || socket_identity(&self.path) != self.identity {
            debug!(socket = %self.path.display(), "the socket is no longer ours; left alone");
            return;
        }
        match fs::remove_file(&self.path) {
            Ok(()) => debug!(socket = %self.path.display(), "socket removed"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(socket = %self.path.display(), error = %e, "could not remove socket"),
        }
    }
}

#[allow(clippy::too_many_arguments)] // What the daemon's last task owns until it stops.
async fn accept_loop(
    listener: UnixListener,
    ctx: Arc<ConnContext>,
    shutdown: CancellationToken,
    held: Held,
    storage: Storage,
    checkpoints: Arc<TurnCheckpoints>,
    menu_bar: Option<JoinHandle<()>>,
    epoch: Epoch,
) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    conns.spawn(conn::serve(stream, ctx.clone()));
                }
                Err(e) => {
                    // Typically fd exhaustion; back off instead of spinning.
                    error!(error = %e, "accept failed");
                    sleep(Duration::from_millis(100)).await;
                }
            },
            Some(finished) = conns.join_next(), if !conns.is_empty() => {
                log_connection_exit(finished);
            }
        }
    }
    info!("shutting down");
    drop(listener);

    // Session processes die with the daemon (ADR 0009): hang them up and reap them while the
    // connections (which watch the same token) flush their queues and close.
    let stop_sessions = async {
        match timeout(SESSION_STOP_WAIT, ctx.router.stop_sessions()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => error!(message = %error.message, "stopping sessions failed"),
            Err(_) => warn!("sessions did not end in time; leaving them to the kernel"),
        }
    };
    let drain_connections = async {
        let drained = timeout(SHUTDOWN_GRACE, async {
            while let Some(finished) = conns.join_next().await {
                log_connection_exit(finished);
            }
        })
        .await;
        if drained.is_err() {
            warn!(
                remaining = conns.len(),
                "connections did not close in time; abandoning them"
            );
            conns.abort_all();
        }
    };
    // Language servers die with the daemon too: asked to stop, their groups killed after a grace.
    let stop_servers = ctx.router.stop_lsp();
    tokio::join!(stop_sessions, drain_connections, stop_servers);
    // The turns the stop cut off are recorded (their end snapshots) before the database closes.
    checkpoints.drain(CHECKPOINT_DRAIN_WAIT).await;
    // The menu bar item leaves with the daemon: its task takes it off at shutdown.
    if let Some(menu_bar) = menu_bar
        && timeout(MENU_BAR_STOP_WAIT, menu_bar).await.is_err()
    {
        warn!("the menu bar item did not leave in time");
    }
    // Records from the sessions that just ended are applied before the indexer stops.
    ctx.router.stop_history().await;
    // A run that reaches this point hung up on its sessions on purpose; the next start tells
    // that from a crash by this note (`session.recovery`). Everything the registry queued while
    // stopping the sessions reaches the disk before the lock is released and a successor opens
    // the database.
    let closed = tokio::task::spawn_blocking(move || {
        if let Err(error) = storage
            .app_meta()
            .record_epoch_stop(epoch, registry::now_ms())
        {
            warn!(%error, "could not record the stop");
        }
        storage.close()
    })
    .await;
    if let Err(error) = closed {
        error!(%error, "closing the database failed");
    }
    drop(held);
    info!("hutand stopped");
}

/// Creates the Home folder when it is missing (and only then) and returns its canonical path.
async fn prepare_home_folder(folder: PathBuf) -> Result<PathBuf, DaemonError> {
    let context = format!("preparing the Home folder {}", folder.display());
    tokio::task::spawn_blocking(move || projects::ensure_home_folder(&folder))
        .await?
        .map_err(DaemonError::io(context))
}

/// Opens (and migrates) the database and reads its content, off the async threads.
async fn open_storage(database: &Path) -> Result<(Storage, Stored), DaemonError> {
    let path = database.to_owned();
    tokio::task::spawn_blocking(move || {
        let storage = Storage::open(&path)?;
        let mut stored = restore::load(&storage)?;
        // Agents died with the previous daemon; whatever they asked can no longer be answered.
        let (expired, expired_questions) = chat::expire_dangling_approvals(&storage)?;
        // Before this run records its own epoch: the latest one in the history is the run
        // that ended.
        stored.facts =
            crate::recovery::load(&storage, &stored.sessions, &expired, &expired_questions)?;
        Ok((storage, stored))
    })
    .await?
    .map_err(DaemonError::storage(database))
}

/// Turns the workspaces of earlier versions into workspaces with member projects, once
/// (`docs/orchestration.md` § Projects), off the async threads.
async fn convert_workspaces(
    storage: &Storage,
    mut stored: Stored,
    database: &Path,
) -> Result<Stored, DaemonError> {
    let storage = storage.clone();
    tokio::task::spawn_blocking(move || {
        project_migration::run(&storage, &mut stored)?;
        Ok(stored)
    })
    .await?
    .map_err(DaemonError::storage(database))
}

fn log_connection_exit(finished: Result<(), tokio::task::JoinError>) {
    if let Err(e) = finished
        && e.is_panic()
    {
        error!(error = %e, "connection task panicked");
    }
}

fn acquire_lock(path: &Path) -> Result<File, DaemonError> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(DaemonError::io(format!("opening lock {}", path.display())))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(DaemonError::AlreadyRunning {
                lock: path.to_owned(),
            });
        }
        Err(TryLockError::Error(e)) => {
            return Err(DaemonError::io(format!("locking {}", path.display()))(e));
        }
    }
    // Informational only: lets a human see who holds the lock.
    file.set_len(0)
        .and_then(|()| writeln!(file, "{}", std::process::id()))
        .map_err(DaemonError::io(format!("writing {}", path.display())))?;
    Ok(file)
}

/// Removes a socket file left by a dead daemon. Safe only while holding the lock.
fn remove_stale_socket(path: &Path) -> Result<(), DaemonError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            info!(socket = %path.display(), "removing stale socket");
            fs::remove_file(path).map_err(DaemonError::io(format!(
                "removing stale {}",
                path.display()
            )))
        }
        Ok(_) => Err(DaemonError::Io {
            context: format!("{} exists and is not a socket", path.display()),
            source: std::io::Error::from(std::io::ErrorKind::AlreadyExists),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DaemonError::io(format!("inspecting {}", path.display()))(e)),
    }
}
