//! Harness for the desktop's GPUI tests: an in-process daemon on a temporary `HUTAN_HOME`
//! (driven from its own Tokio runtime, since GPUI has no reactor), a real `Connection`, and
//! helpers to pump GPUI while real I/O happens on other threads.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

pub mod agents;
pub mod assistant;
pub mod chat;
pub mod control;
pub mod history;
pub mod mac;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_client::{Client, ConnectOptions, Connection};
use hutan_core::{SessionId, WorkspaceId};
use hutan_daemon::testing::DaemonReaper;
use hutan_daemon::{DaemonConfig, Paths, RunningDaemon};
use hutan_desktop::workbench::{Workbench, WorkbenchOptions};
use hutan_protocol::messages::{
    ClientInfo, ClientKind, CreatedBy, Request, SessionCreate, SessionInfo, SessionResult,
    SessionSpec, TerminalSpec, WorkspaceCreate, WorkspaceInfo, WorkspaceOpen, WorkspaceOpened,
    WorkspaceRename,
};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde::de::DeserializeOwned;
use tempfile::TempDir;

pub const WAIT: Duration = Duration::from_secs(20);

fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Desktop,
        version: "desktop-test".into(),
        origin: None,
    }
}

/// Forwards every connection on its own socket to the daemon's socket, so a test can cut the
/// link without restarting the daemon.
pub struct Proxy {
    #[allow(dead_code)]
    pub path: PathBuf,
    sever: tokio::sync::broadcast::Sender<()>,
    blocked: std::sync::Arc<std::sync::atomic::AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Proxy {
    fn start(runtime: &tokio::runtime::Runtime, path: PathBuf, upstream: PathBuf) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::io::copy_bidirectional;
        use tokio::net::{UnixListener, UnixStream};
        let listener = {
            let _guard = runtime.enter();
            UnixListener::bind(&path).unwrap()
        };
        let blocked = std::sync::Arc::new(AtomicBool::new(false));
        let (sever, _) = tokio::sync::broadcast::channel(1);
        let task = runtime.spawn({
            let blocked = blocked.clone();
            let sever = sever.clone();
            async move {
                loop {
                    let Ok((mut client, _)) = listener.accept().await else {
                        return;
                    };
                    if blocked.load(Ordering::SeqCst) {
                        continue;
                    }
                    let upstream = upstream.clone();
                    let mut cut = sever.subscribe();
                    tokio::spawn(async move {
                        let Ok(mut daemon) = UnixStream::connect(&upstream).await else {
                            return;
                        };
                        tokio::select! {
                            _ = copy_bidirectional(&mut client, &mut daemon) => {}
                            _ = cut.recv() => {}
                        }
                    });
                }
            }
        });
        Self {
            path,
            sever,
            blocked,
            task,
        }
    }

    /// Closes every connection that passes through right now.
    pub fn sever(&self) {
        let _ = self.sever.send(());
    }

    /// While blocked, new connections are accepted and closed at once.
    pub fn block(&self, blocked: bool) {
        self.blocked
            .store(blocked, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The switches of [`TestDaemon::held_connection`]'s proxy.
#[derive(Clone, Default)]
pub struct Hold {
    up: Valve,
    down: Valve,
}

impl Hold {
    /// Holds back (or lets go of) what the client sends.
    pub fn requests(&self, held: bool) {
        self.up.set(held);
    }

    /// Holds back (or lets go of) what the daemon sends.
    pub fn answers(&self, held: bool) {
        self.down.set(held);
    }

    /// Whether what the daemon sent and is held back contains `needle`.
    pub fn holds_answer_with(&self, needle: &str) -> bool {
        let held = self.down.0.held.lock().unwrap();
        held.windows(needle.len()).any(|w| w == needle.as_bytes())
    }
}

/// One direction of the held proxy: bytes read are kept while it is closed.
#[derive(Clone, Default)]
struct Valve(std::sync::Arc<ValveState>);

#[derive(Default)]
struct ValveState {
    closed: std::sync::atomic::AtomicBool,
    held: std::sync::Mutex<Vec<u8>>,
    opened: tokio::sync::Notify,
}

impl Valve {
    fn set(&self, closed: bool) {
        self.0
            .closed
            .store(closed, std::sync::atomic::Ordering::SeqCst);
        if !closed {
            self.0.opened.notify_one();
        }
    }

    async fn forward(
        self,
        mut from: tokio::net::unix::OwnedReadHalf,
        mut to: tokio::net::unix::OwnedWriteHalf,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            tokio::select! {
                read = from.read(&mut chunk) => match read {
                    Ok(0) | Err(_) => return,
                    Ok(n) => self.0.held.lock().unwrap().extend_from_slice(&chunk[..n]),
                },
                () = self.0.opened.notified() => {}
            }
            if self.0.closed.load(std::sync::atomic::Ordering::SeqCst) {
                continue;
            }
            let out = std::mem::take(&mut *self.0.held.lock().unwrap());
            if to.write_all(&out).await.is_err() {
                return;
            }
        }
    }
}

/// A daemon in this process on a fresh `HUTAN_HOME`.
pub struct TestDaemon {
    pub dir: TempDir,
    pub paths: Paths,
    runtime: Option<tokio::runtime::Runtime>,
    daemon: Option<RunningDaemon>,
    proxy: Option<Proxy>,
    older: Option<tokio::task::JoinHandle<()>>,
}

impl TestDaemon {
    pub fn start() -> Self {
        Self::start_with(None)
    }

    /// A daemon whose `daemon.toml` is `daemon_toml` (written before it starts).
    pub fn start_with_config(daemon_toml: &str) -> Self {
        Self::start_with(Some(daemon_toml))
    }

    /// A daemon whose `daemon.toml` is `daemon_toml` and whose login environment is captured from
    /// `login_shell` (a stand-in script) as soon as it starts, so no test session waits for it.
    pub fn start_with_login_shell(daemon_toml: &str, login_shell: &Path) -> Self {
        Self::start_configured(
            Some(daemon_toml),
            None,
            hutan_daemon::GatewayConfig::default(),
            Some(login_shell.to_owned()),
            |_| {},
        )
    }

    /// A daemon whose `codex app-server` is `codex` (a replay of a recorded transcript).
    pub fn start_with_codex(codex: hutan_agents::CodexConfig) -> Self {
        Self::start_inner(None, Some(codex))
    }

    /// Like [`start_with_codex`](Self::start_with_codex), with the daemon's configuration
    /// changed by `tweak` before it starts (checkpoints' git environment, say).
    #[allow(dead_code)]
    pub fn start_with_codex_tweaked(
        codex: hutan_agents::CodexConfig,
        tweak: impl FnOnce(&mut DaemonConfig),
    ) -> Self {
        Self::start_configured(
            None,
            Some(codex),
            hutan_daemon::GatewayConfig::default(),
            None,
            tweak,
        )
    }

    /// [`start_with_codex_tweaked`](Self::start_with_codex_tweaked) with a `daemon.toml`
    /// (`[assistant] backend = "codex"`, say).
    #[allow(dead_code)]
    pub fn start_with_codex_config(
        daemon_toml: &str,
        codex: hutan_agents::CodexConfig,
        tweak: impl FnOnce(&mut DaemonConfig),
    ) -> Self {
        Self::start_configured(
            Some(daemon_toml),
            Some(codex),
            hutan_daemon::GatewayConfig::default(),
            None,
            tweak,
        )
    }

    pub fn start_with_gateway(gateway: hutan_daemon::GatewayConfig) -> Self {
        Self::start_configured(None, None, gateway, None, |_| {})
    }

    pub fn http_mock(&self) -> hutan_gateway::testing::MockServer {
        self.rt()
            .block_on(hutan_gateway::testing::MockServer::start())
    }

    fn start_with(daemon_toml: Option<&str>) -> Self {
        Self::start_inner(daemon_toml, None)
    }

    fn start_inner(daemon_toml: Option<&str>, codex: Option<hutan_agents::CodexConfig>) -> Self {
        Self::start_configured(
            daemon_toml,
            codex,
            hutan_daemon::GatewayConfig::default(),
            None,
            |_| {},
        )
    }

    fn start_configured(
        daemon_toml: Option<&str>,
        codex: Option<hutan_agents::CodexConfig>,
        gateway: hutan_daemon::GatewayConfig,
        login_shell: Option<PathBuf>,
        tweak: impl FnOnce(&mut DaemonConfig),
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_home(dir.path());
        // No test ever runs a real harness or reads a real login: unless the test names its
        // own, OMP and Claude Code are programs that do not exist, and Codex's account lookup
        // meets a transcript that does not exist.
        let mut text = daemon_toml.unwrap_or_default().to_owned();
        if !text.contains("[harness.omp]") {
            text.push_str(&format!(
                "\n[harness.omp]\nbin = {0:?}\ncli = {0:?}\n",
                dir.path().join("no-such-omp")
            ));
        }
        if !text.contains("[harness.claude]") {
            text.push_str(&format!(
                "\n[harness.claude]\nbin = {0:?}\ncli = {0:?}\n",
                dir.path().join("no-such-claude")
            ));
        }
        if login_shell.is_none() && !text.contains("[terminal]") {
            text.push_str("\n[terminal]\nenv_inherit = \"daemon\"\n");
        }
        std::fs::write(&paths.config_file, text).unwrap();
        let codex = codex.or_else(|| {
            Some(hutan_agents::CodexConfig {
                program: dir.path().join("no-such-codex"),
                args: Vec::new(),
                env: Vec::new(),
            })
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("test-daemon")
            .enable_all()
            .build()
            .unwrap();
        let mut config = DaemonConfig::new(paths.clone());
        config.codex = codex;
        config.gateway = gateway;
        config.login_shell = login_shell;
        // A GPUI test window answers `ui.command`s only while the test pumps it; on a loaded
        // machine that can take longer than the 5 s a real window gets. No desktop test is about
        // the timeout itself (the daemon's own tests cover it).
        config.ui_command_timeout = std::time::Duration::from_secs(30);
        tweak(&mut config);
        let daemon = runtime.block_on(hutan_daemon::start(config)).unwrap();
        Self {
            dir,
            paths,
            runtime: Some(runtime),
            daemon: Some(daemon),
            proxy: None,
            older: None,
        }
    }

    /// A connection to this daemon as if it were older than protocol 0.28: the proxy it goes
    /// through renames the capability `daemon.settings` in the daemon's welcome (to a name of
    /// the same length, so the frame stays valid), and passes everything else on.
    pub fn older_connection(&mut self) -> Connection {
        self.connection_without("daemon.settings", "older.sock")
    }

    /// A connection to this daemon as if it predated the capability `hooks` (protocol 0.49): the
    /// Automation page then says the daemon is older.
    pub fn connection_without_hooks(&mut self) -> Connection {
        self.connection_without("\"hooks", "no-hooks.sock")
    }

    /// A connection to this daemon as if it predated the capability `history.sessions`
    /// (protocol 0.29): the panel then falls back to the search alone.
    pub fn connection_without_history_sessions(&mut self) -> Connection {
        self.connection_without("history.sessions", "no-sessions.sock")
    }

    /// A connection to this daemon as if it predated the capability `checkpoint` (protocol
    /// 0.37). The welcome lists `checkpoint` before `checkpoint.restore`, so the first match
    /// of the quoted prefix is the former.
    #[allow(dead_code)]
    pub fn connection_without_checkpoints(&mut self) -> Connection {
        self.connection_without("\"checkpoint", "no-checkpoints.sock")
    }

    /// A connection through a proxy that renames `capability` in the daemon's welcome (to a name
    /// of the same length, so the frame stays valid) and passes everything else on.
    fn connection_without(&mut self, capability: &'static str, socket: &str) -> Connection {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{UnixListener, UnixStream};
        let path = self.dir.path().join(socket);
        let upstream = self.paths.socket.clone();
        let listener = {
            let _guard = self.rt().enter();
            UnixListener::bind(&path).unwrap()
        };
        let task = self.rt().spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    return;
                };
                let upstream = upstream.clone();
                tokio::spawn(async move {
                    let Ok(daemon) = UnixStream::connect(&upstream).await else {
                        return;
                    };
                    let (mut daemon_read, mut daemon_write) = daemon.into_split();
                    let (mut client_read, mut client_write) = client.into_split();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 64 * 1024];
                        let mut renamed = false;
                        loop {
                            let Ok(n) = daemon_read.read(&mut buf).await else {
                                return;
                            };
                            if n == 0 {
                                return;
                            }
                            let chunk = &mut buf[..n];
                            if !renamed {
                                let needle = capability.as_bytes();
                                if let Some(at) =
                                    chunk.windows(needle.len()).position(|w| w == needle)
                                {
                                    chunk[at + needle.len() - 1] = b'z';
                                    renamed = true;
                                }
                            }
                            if client_write.write_all(chunk).await.is_err() {
                                return;
                            }
                        }
                    });
                    let _ = tokio::io::copy(&mut client_read, &mut daemon_write).await;
                });
            }
        });
        self.older = Some(task);
        let mut options = self.options();
        options.socket = Some(path);
        Connection::start(options)
    }

    /// A connection that goes through a proxy whose link [`TestDaemon::sever`] can cut.
    pub fn proxied_connection(&mut self) -> Connection {
        let path = self.dir.path().join("proxy.sock");
        if self.proxy.is_none() {
            let proxy = Proxy::start(self.rt(), path.clone(), self.paths.socket.clone());
            self.proxy = Some(proxy);
        }
        let mut options = self.options();
        options.socket = Some(path);
        Connection::start(options)
    }

    pub fn proxy(&self) -> &Proxy {
        self.proxy.as_ref().expect("proxied_connection was called")
    }

    /// A connection through a proxy that holds back, while told to, what the client sends
    /// ([`Hold::requests`]) or what the daemon sends back ([`Hold::answers`]), and passes it on
    /// in order once let go: a test decides when a request is answered.
    pub fn held_connection(&self) -> (Connection, Hold) {
        use tokio::net::{UnixListener, UnixStream};
        let path = self.dir.path().join("held.sock");
        let upstream = self.paths.socket.clone();
        let listener = {
            let _guard = self.rt().enter();
            UnixListener::bind(&path).unwrap()
        };
        let hold = Hold::default();
        let held = hold.clone();
        self.rt().spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let Ok(daemon) = UnixStream::connect(&upstream).await else {
                    return;
                };
                let (client_read, client_write) = client.into_split();
                let (daemon_read, daemon_write) = daemon.into_split();
                tokio::spawn(held.up.clone().forward(client_read, daemon_write));
                tokio::spawn(held.down.clone().forward(daemon_read, client_write));
            }
        });
        let mut options = self.options();
        options.socket = Some(path);
        (Connection::start(options), hold)
    }

    fn rt(&self) -> &tokio::runtime::Runtime {
        self.runtime.as_ref().unwrap()
    }

    pub fn home(&self) -> &Path {
        self.dir.path()
    }

    pub fn options(&self) -> ConnectOptions {
        ConnectOptions {
            socket: Some(self.paths.socket.clone()),
            autostart: None,
            client: client_info(),
        }
    }

    pub fn connection(&self) -> Connection {
        Connection::start(self.options())
    }

    /// One request on a fresh connection, answered synchronously.
    pub fn call<T: DeserializeOwned + Send + 'static>(&self, request: Request) -> T {
        let options = self.options();
        self.rt().block_on(async move {
            let client = Client::connect(options).await.unwrap();
            client.call(request).await.unwrap()
        })
    }

    /// Like [`call`](Self::call), on a connection that says it is a `kind` client.
    pub fn call_as<T: DeserializeOwned + Send + 'static>(
        &self,
        kind: ClientKind,
        request: Request,
    ) -> T {
        let mut options = self.options();
        options.client.kind = kind;
        self.rt().block_on(async move {
            let client = Client::connect(options).await.unwrap();
            client.call(request).await.unwrap()
        })
    }

    /// What the desktop of this version shows as one flat workspace: a project (protocol 0.19)
    /// for the first of `roots` — or, with none, an empty workspace — named `name`. The project
    /// sidebar and its workspaces-containing-projects model arrive with M18-B.
    pub fn create_workspace(&self, name: &str, roots: &[&Path]) -> WorkspaceInfo {
        self.create_workspace_by(name, roots, None)
    }

    pub fn create_workspace_by(
        &self,
        name: &str,
        roots: &[&Path],
        created_by: Option<CreatedBy>,
    ) -> WorkspaceInfo {
        let Some(root) = roots.first() else {
            return self.call(Request::WorkspaceCreate(WorkspaceCreate {
                name: name.to_owned(),
                paths: Vec::new(),
                project_ids: Vec::new(),
                created_by,
            }));
        };
        let opened: WorkspaceOpened = self.call(Request::WorkspaceOpen(WorkspaceOpen {
            path: root.to_path_buf(),
            created_by,
        }));
        self.call(Request::WorkspaceRename(WorkspaceRename {
            workspace_id: opened.workspace.id,
            name: name.to_owned(),
        }))
    }

    /// A terminal session running `argv` with a plain `$ ` prompt.
    pub fn create_session(&self, argv: &[&str], workspace: Option<WorkspaceId>) -> SessionInfo {
        let created: SessionResult = self.call(Request::SessionCreate(SessionCreate {
            workspace_id: workspace,
            spec: SessionSpec::Terminal(TerminalSpec {
                command: argv.iter().map(|a| (*a).to_owned()).collect(),
                env: [("PS1".to_owned(), "$ ".to_owned())].into(),
                ..TerminalSpec::default()
            }),
        }));
        created.session
    }

    /// Types `bytes` into `terminal` from a connection of its own (no desktop involved).
    pub fn send_input(&self, terminal: hutan_core::TerminalId, bytes: &[u8]) {
        let options = self.options();
        self.rt().block_on(async move {
            let client = Client::connect(options).await.unwrap();
            client.send_input(terminal, bytes).await.unwrap();
            // The input frame is queued; a round trip proves the daemon has read it.
            let _: serde_json::Value = client
                .call(Request::DaemonStatus(Default::default()))
                .await
                .unwrap();
        });
    }

    /// The daemon's blocks of `terminal`, oldest first.
    pub fn blocks(&self, terminal: hutan_core::TerminalId) -> Vec<hutan_protocol::blocks::Block> {
        let result: hutan_protocol::blocks::TerminalBlocksResult = self.call(
            Request::TerminalBlocks(hutan_protocol::blocks::TerminalBlocks {
                terminal_id: terminal,
                before_id: None,
                limit: 500,
            }),
        );
        let mut blocks = result.blocks;
        blocks.reverse();
        blocks
    }

    /// What `terminal.block_text` answers for a block's output.
    pub fn block_output(&self, terminal: hutan_core::TerminalId, block_id: u64) -> String {
        let result: hutan_protocol::blocks::TerminalBlockTextResult = self.call(
            Request::TerminalBlockText(hutan_protocol::blocks::TerminalBlockText {
                terminal_id: terminal,
                block_id,
                part: hutan_protocol::blocks::BlockPart::Output,
                max_bytes: 1 << 20,
            }),
        );
        result.text
    }

    /// `sh -c script` as a session.
    pub fn sh(&self, script: &str) -> SessionInfo {
        self.create_session(&["sh", "-c", script], None)
    }

    /// `sh -c script` as a session named `title`.
    pub fn sh_titled(&self, title: &str, script: &str) -> SessionInfo {
        let created: SessionResult = self.call(Request::SessionCreate(SessionCreate {
            workspace_id: None,
            spec: SessionSpec::Terminal(TerminalSpec {
                command: vec!["sh".into(), "-c".into(), script.to_owned()],
                title: Some(title.to_owned()),
                ..TerminalSpec::default()
            }),
        }));
        created.session
    }

    pub fn session(&self, id: SessionId) -> SessionInfo {
        let list: hutan_protocol::messages::SessionList =
            self.call(Request::SessionList(Default::default()));
        list.sessions.into_iter().find(|s| s.id == id).unwrap()
    }

    pub fn stop(&mut self) {
        let daemon = self.daemon.take().expect("daemon is running");
        daemon.shutdown();
        self.rt()
            .block_on(async { tokio::time::timeout(WAIT, daemon.wait()).await })
            .unwrap()
            .unwrap();
    }

    /// A new daemon on the same home: new epoch; stored workspaces and sessions are restored.
    pub fn restart(&mut self) {
        self.restart_with(|_| {});
    }

    /// [`restart`](Self::restart) with `tweak` applied to the new daemon's config (a `codex`
    /// that replays the next transcript, say).
    pub fn restart_with(&mut self, tweak: impl FnOnce(&mut DaemonConfig)) {
        assert!(self.daemon.is_none(), "daemon is already running");
        let mut config = DaemonConfig::new(self.paths.clone());
        tweak(&mut config);
        self.daemon = Some(self.rt().block_on(hutan_daemon::start(config)).unwrap());
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let (Some(daemon), Some(runtime)) = (self.daemon.take(), self.runtime.take()) {
            daemon.shutdown();
            let _ = runtime.block_on(async { tokio::time::timeout(WAIT, daemon.wait()).await });
            runtime.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

thread_local! {
    /// Temp dirs of tests that have no daemon; dropped when the test thread ends.
    static KEEP: RefCell<Vec<TempDir>> = const { RefCell::new(Vec::new()) };
}

/// `settings` with the assistant on: it is off by default since the Orca-style layout, and the
/// tests of its page, its commands and its shortcut need it.
pub fn with_assistant(settings: SettingsStore) -> SettingsStore {
    settings
        .write_user("assistant.enabled", toml::Value::Boolean(true))
        .expect("enable the assistant");
    settings
}

/// A connection to a daemon that does not exist (layout-only tests).
pub fn offline_connection() -> (Connection, SettingsStore, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("none.sock");
    let settings = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
    let state = dir.path().join("desktop-state.json");
    KEEP.with(|keep| keep.borrow_mut().push(dir));
    let connection = Connection::start(ConnectOptions {
        socket: Some(socket),
        autostart: None,
        client: client_info(),
    });
    (connection, settings, state)
}

/// Opens a workbench over `settings` and `connection`.
pub fn open_with(
    cx: &mut TestAppContext,
    settings: SettingsStore,
    connection: Connection,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, &mut VisualTestContext) {
    let options = WorkbenchOptions::builtin(settings, connection, state_path);
    open_options(cx, options)
}

/// Like [`open_with`] for options built by the test (macOS services, say).
pub fn open_options(
    cx: &mut TestAppContext,
    options: WorkbenchOptions,
) -> (Entity<Workbench>, &mut VisualTestContext) {
    // Real sockets and threads wake GPUI tasks from other threads.
    cx.executor().allow_parking();
    let (view, cx) = cx.add_window_view(|window, cx| Workbench::new(options, window, cx));
    // A real window is active; the test platform starts windows inactive.
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    (view, cx)
}

/// One frame: draw, then the next-frame callbacks the platform runs before the following draw
/// (terminal views ack there).
pub fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.simulate_next_frame(cx);
    });
    cx.run_until_parked();
}

/// Lets every motion run out (the longest is 250 ms, `ui::motion`) and draws: for a test that
/// measures geometry right after something opened, which slides in while it opens.
pub fn settle(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(300));
    draw(cx);
    draw(cx);
}

/// Pumps GPUI (and draws) until `condition` holds; panics with `what` after [`WAIT`].
pub fn wait_until(
    cx: &mut VisualTestContext,
    what: &str,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    loop {
        draw(cx);
        if condition(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Text of the terminal view's screen, rows joined by newlines.
pub fn screen_text(model: &hutan_terminal_view::model::TerminalModel) -> String {
    model
        .viewport()
        .iter()
        .map(hutan_terminal_view::model::TerminalModel::row_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// A real `hutand` process on its own temporary home, started by the connection's autostart
/// (and again by `Connection::set_autostart`). Stopped on drop, however the test ended: the
/// launcher script refuses to start another daemon from then on (a window's connection that
/// outlives this guard would otherwise autostart a new one on a deleted home), and every daemon
/// it started is SIGTERMed, then SIGKILLed ([`DaemonReaper`]).
pub struct ProcessDaemon {
    pub dir: TempDir,
    wrapper: PathBuf,
    runtime: tokio::runtime::Runtime,
    reaper: DaemonReaper,
    /// Daemons [`spawn_from`](Self::spawn_from) started, waited for on drop.
    spawned: RefCell<Vec<std::process::Child>>,
}

impl ProcessDaemon {
    pub fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let hutand = hutand_beside_tests();
        assert!(
            hutand.is_file(),
            "hutand not built: run `cargo build -p hutan-daemon --bin hutand` (or `just test`)"
        );
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        // `ConnectOptions` cannot carry environment, so a wrapper pins the daemon to the home.
        // It notes its pid (the daemon's, after `exec`) before it looks for `closed`: a daemon
        // started after the guard closed the wrapper exits at once, one started before is in
        // `pids` by the time the guard reads it.
        let wrapper = dir.path().join("hutand-wrapper.sh");
        let pids = dir.path().join("pids");
        let closed = dir.path().join("closed");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\necho $$ >> '{}'\n[ -e '{}' ] && exit 75\nexport HUTAN_HOME='{}'\nexec '{}'\n",
                pids.display(),
                closed.display(),
                home.display(),
                hutand.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        Self {
            reaper: DaemonReaper::new(&home).with_pid_file(pids),
            dir,
            wrapper,
            runtime,
            spawned: RefCell::new(Vec::new()),
        }
    }

    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub fn socket(&self) -> PathBuf {
        self.home().join("hutand.sock")
    }

    pub fn options(&self) -> ConnectOptions {
        ConnectOptions {
            socket: Some(self.socket()),
            autostart: Some(self.wrapper.clone()),
            client: client_info(),
        }
    }

    /// The `hutand` the launcher starts, symlinks resolved.
    #[allow(dead_code)]
    pub fn hutand(&self) -> PathBuf {
        hutand_beside_tests().canonicalize().unwrap()
    }

    /// How many daemons were started for this home so far (by the launcher or
    /// [`spawn_from`](Self::spawn_from)).
    #[allow(dead_code)]
    pub fn starts(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("pids"))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    /// Starts `exe` (a `hutand` elsewhere: another install) on this home, and waits until it
    /// answers. Stopped on drop like the launcher's daemons.
    #[allow(dead_code)]
    pub fn spawn_from(&self, exe: &Path) {
        use std::io::Write as _;
        use std::os::unix::process::CommandExt as _;
        let child = std::process::Command::new(exe)
            .env("HUTAN_HOME", self.home())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut pids = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.path().join("pids"))
            .unwrap();
        writeln!(pids, "{}", child.id()).unwrap();
        self.spawned.borrow_mut().push(child);
        let deadline = Instant::now() + WAIT;
        while std::os::unix::net::UnixStream::connect(self.socket()).is_err() {
            assert!(
                Instant::now() < deadline,
                "{} did not come up",
                exe.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// One request on a fresh connection (which starts the daemon if it is not running).
    pub fn call<T: DeserializeOwned + Send + 'static>(&self, request: Request) -> T {
        let options = self.options();
        self.runtime.block_on(async move {
            let client = Client::connect(options).await.unwrap();
            client.call(request).await.unwrap()
        })
    }

    /// One request from a `kind` client (`hutan daemon stop` is a CLI's `daemon.shutdown`)
    /// that never starts a daemon.
    #[allow(dead_code)]
    pub fn call_as<T: DeserializeOwned + Send + 'static>(
        &self,
        kind: ClientKind,
        request: Request,
    ) -> T {
        let options = ConnectOptions {
            socket: Some(self.socket()),
            autostart: None,
            client: ClientInfo {
                kind,
                version: "test".into(),
                origin: None,
            },
        };
        self.runtime.block_on(async move {
            let client = Client::connect(options).await.unwrap();
            client.call(request).await.unwrap()
        })
    }

    /// A terminal session running `argv`.
    pub fn create_session(&self, argv: &[&str]) -> SessionInfo {
        let created: SessionResult = self.call(Request::SessionCreate(SessionCreate {
            workspace_id: None,
            spec: SessionSpec::Terminal(TerminalSpec {
                command: argv.iter().map(|a| (*a).to_owned()).collect(),
                ..TerminalSpec::default()
            }),
        }));
        created.session
    }
}

impl Drop for ProcessDaemon {
    fn drop(&mut self) {
        // From now on the wrapper starts nothing (ignored error: then the reaper still runs).
        let _ = std::fs::write(self.dir.path().join("closed"), "");
        self.reaper.reap();
        // The reaper ended them; collect their exit status (ignored: they were signalled).
        for mut child in self.spawned.take() {
            let _ = child.wait();
        }
    }
}

/// `target/<profile>/hutand`, beside this test binary's `deps/`.
fn hutand_beside_tests() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(|deps| deps.parent()).unwrap();
    profile_dir.join("hutand")
}

// Real shells -----------------------------------------------------------------------------

/// A shell the daemon injects its OSC 133 integration into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    #[allow(dead_code)]
    pub const ALL: [Shell; 3] = [Shell::Zsh, Shell::Bash, Shell::Fish];

    /// The shell's executable, if it is installed (bash is macOS's 3.2 `/bin/bash`).
    pub fn path(self) -> Option<&'static str> {
        let candidates: &[&str] = match self {
            Shell::Zsh => &["/bin/zsh"],
            Shell::Bash => &["/bin/bash"],
            Shell::Fish => &[
                "/opt/homebrew/bin/fish",
                "/usr/local/bin/fish",
                "/usr/bin/fish",
            ],
        };
        candidates.iter().copied().find(|p| Path::new(p).exists())
    }
}

/// A shell session with a plain `$ ` prompt in a home of its own; keeps the home alive.
pub struct ShellSession {
    #[allow(dead_code)] // keeps the temp dir alive
    pub home: TempDir,
    pub info: SessionInfo,
}

impl TestDaemon {
    /// Starts `shell` as a terminal session with the user's startup files reduced to a `$ `
    /// prompt, so the automatic shell integration is the only thing that marks blocks.
    pub fn shell_session(&self, shell: Shell, path: &str) -> ShellSession {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let write = |file: &str, content: &str| {
            let file = home.join(file);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, content).unwrap();
        };
        match shell {
            Shell::Zsh => write(".zshrc", "PS1='$ '\nunsetopt PROMPT_SP\n"),
            Shell::Bash => {
                write(".bash_profile", ". \"$HOME/.bashrc\"\n");
                write(".bashrc", "PS1='$ '\n");
            }
            Shell::Fish => write(
                ".config/fish/config.fish",
                "function fish_prompt\n    echo -n '$ '\nend\n",
            ),
        }
        let env = [
            ("HOME", home.to_string_lossy().into_owned()),
            ("SHELL", path.to_owned()),
            ("ZDOTDIR", home.to_string_lossy().into_owned()),
            (
                "XDG_CONFIG_HOME",
                home.join(".config").to_string_lossy().into_owned(),
            ),
            (
                "XDG_DATA_HOME",
                home.join(".local/share").to_string_lossy().into_owned(),
            ),
            (
                "XDG_CACHE_HOME",
                home.join(".cache").to_string_lossy().into_owned(),
            ),
            (
                "HISTFILE",
                home.join(".hist").to_string_lossy().into_owned(),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        let created: SessionResult = self.call(Request::SessionCreate(SessionCreate {
            workspace_id: None,
            spec: SessionSpec::Terminal(TerminalSpec {
                env,
                cwd: Some(home.clone()),
                ..TerminalSpec::default()
            }),
        }));
        ShellSession {
            home: tmp,
            info: created.session,
        }
    }
}
