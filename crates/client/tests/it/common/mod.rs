//! Harness for the `Connection` tests: an in-process daemon on a temporary `HUTAN_HOME`, a
//! small Unix-socket proxy between it and the connection under test (so tests can sever the
//! link without restarting the daemon), and a client-side model of one terminal screen.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hutan_client::{
    Client, ConnectOptions, Connection, ConnectionState, Notice, StoreState, TerminalHandle,
    TerminalUpdate,
};
use hutan_core::{SessionId, TerminalId, WorkspaceId};
use hutan_daemon::bus::RingLimits;
use hutan_daemon::{DaemonConfig, Paths, RunningDaemon};
use hutan_protocol::messages::{
    ClientInfo, ClientKind, Empty, Request, SessionCreate, SessionInfo, SessionResult,
    SessionSnapshot, SessionSpec, TerminalSize, TerminalSpec, WorkspaceCreate, WorkspaceInfo,
};
use hutan_protocol::render::{Row, TerminalRender};
use tempfile::TempDir;
use tokio::io::copy_bidirectional;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;

pub const WAIT: Duration = Duration::from_secs(20);

/// Forwards every connection on its own socket to the daemon's socket. [`Proxy::sever`] closes
/// the connections that exist right now; [`Proxy::block`] makes new ones fail immediately.
pub struct Proxy {
    pub path: PathBuf,
    blocked: Arc<AtomicBool>,
    sever: broadcast::Sender<()>,
    task: JoinHandle<()>,
}

impl Proxy {
    fn start(path: PathBuf, upstream: PathBuf) -> Self {
        let listener = UnixListener::bind(&path).unwrap();
        let blocked = Arc::new(AtomicBool::new(false));
        let (sever, _) = broadcast::channel(1);
        let task = tokio::spawn({
            let blocked = blocked.clone();
            let sever = sever.clone();
            async move {
                loop {
                    let Ok((mut client, _)) = listener.accept().await else {
                        return;
                    };
                    if blocked.load(Ordering::SeqCst) {
                        drop(client);
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
            blocked,
            sever,
            task,
        }
    }

    /// Closes every connection that currently passes through.
    pub fn sever(&self) {
        let _ = self.sever.send(());
    }

    /// While blocked, new connections are accepted and closed at once.
    pub fn block(&self) {
        self.blocked.store(true, Ordering::SeqCst);
    }

    pub fn unblock(&self) {
        self.blocked.store(false, Ordering::SeqCst);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct Env {
    #[allow(dead_code)] // keeps the temp dir alive
    pub dir: TempDir,
    pub paths: Paths,
    pub daemon: Option<RunningDaemon>,
    pub proxy: Proxy,
    ring: Option<RingLimits>,
}

impl Env {
    pub async fn start() -> Self {
        Self::start_with_ring(None).await
    }

    pub async fn start_with_ring(ring: Option<RingLimits>) -> Self {
        // Daemon logs show up for failing tests; filter with RUST_LOG.
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_home(dir.path());
        let daemon = start_daemon(&paths, ring).await;
        let proxy = Proxy::start(dir.path().join("proxy.sock"), paths.socket.clone());
        Self {
            dir,
            paths,
            daemon: Some(daemon),
            proxy,
            ring,
        }
    }

    /// Options for a connection that goes through the proxy.
    pub fn options(&self) -> ConnectOptions {
        ConnectOptions {
            endpoint: Some(self.proxy.path.clone().into()),
            autostart: None,
            client: ClientInfo {
                kind: ClientKind::Desktop,
                version: "test".into(),
                origin: None,
            },
        }
    }

    /// A plain client straight to the daemon (not through the proxy).
    pub async fn direct(&self) -> Client {
        let mut opts = self.options();
        opts.endpoint = Some(self.paths.socket.clone().into());
        Client::connect(opts).await.unwrap()
    }

    pub async fn stop_daemon(&mut self) {
        let daemon = self.daemon.take().expect("daemon is running");
        daemon.shutdown();
        timeout(WAIT, daemon.wait()).await.unwrap().unwrap();
    }

    /// Starts a new daemon on the same home (new epoch; stored workspaces and sessions are restored).
    pub async fn start_daemon(&mut self) {
        assert!(self.daemon.is_none(), "daemon is already running");
        self.daemon = Some(start_daemon(&self.paths, self.ring).await);
    }
}

async fn start_daemon(paths: &Paths, ring: Option<RingLimits>) -> RunningDaemon {
    let mut config = DaemonConfig::new(paths.clone());
    if let Some(ring) = ring {
        config.ring = ring;
    }
    hutan_daemon::start(config).await.unwrap()
}

pub fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// A terminal spec running `argv` with a plain `$ ` prompt.
pub fn spec(argv: &[&str]) -> TerminalSpec {
    TerminalSpec {
        command: argv.iter().map(|a| (*a).to_owned()).collect(),
        env: [("PS1".to_owned(), "$ ".to_owned())].into(),
        ..TerminalSpec::default()
    }
}

pub async fn create_session(client: &Client, spec: TerminalSpec) -> SessionInfo {
    client
        .call::<SessionResult>(Request::SessionCreate(SessionCreate {
            workspace_id: None,
            spec: SessionSpec::Terminal(spec),
        }))
        .await
        .unwrap()
        .session
}

pub async fn create_workspace(client: &Client, name: &str) -> WorkspaceInfo {
    client
        .call(Request::WorkspaceCreate(WorkspaceCreate {
            name: name.to_owned(),
            paths: Vec::new(),
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap()
}

pub fn terminal_of(session: &SessionInfo) -> TerminalId {
    session.terminal_id.expect("session has a terminal")
}

pub async fn snapshot(client: &Client) -> SessionSnapshot {
    client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap()
}

/// Waits (up to [`WAIT`]) for a state satisfying `pred`.
pub async fn wait_state(
    rx: &mut watch::Receiver<Arc<StoreState>>,
    what: &str,
    pred: impl Fn(&StoreState) -> bool,
) -> Arc<StoreState> {
    let found = timeout(WAIT, async {
        loop {
            let state = rx.borrow_and_update().clone();
            if pred(&state) {
                return state;
            }
            rx.changed().await.expect("connection task ended");
        }
    })
    .await;
    match found {
        Ok(state) => state,
        Err(_) => panic!(
            "timed out waiting for state: {what}; last state: {:?}",
            *rx.borrow()
        ),
    }
}

pub async fn wait_connected(rx: &mut watch::Receiver<Arc<StoreState>>) -> Arc<StoreState> {
    wait_state(rx, "connected", |s| {
        s.connection == ConnectionState::Connected
    })
    .await
}

pub async fn wait_reconnecting(rx: &mut watch::Receiver<Arc<StoreState>>) -> Arc<StoreState> {
    wait_state(rx, "reconnecting", |s| {
        matches!(s.connection, ConnectionState::Reconnecting { .. })
    })
    .await
}

/// Next notice satisfying `pick`; fails after [`WAIT`].
pub async fn wait_notice<T>(
    notices: &mut broadcast::Receiver<Notice>,
    what: &str,
    mut pick: impl FnMut(&Notice) -> Option<T>,
) -> T {
    let result = timeout(WAIT, async {
        loop {
            let notice = notices.recv().await.expect("notice stream failed");
            if let Some(found) = pick(&notice) {
                return found;
            }
        }
    })
    .await;
    result.unwrap_or_else(|_| panic!("timed out waiting for notice: {what}"))
}

/// Everything the daemon's `session.snapshot` says, in the order a `StoreState` keeps.
pub fn expected_view(snapshot: SessionSnapshot) -> (Vec<WorkspaceInfo>, Vec<SessionInfo>) {
    let mut sessions = snapshot.sessions;
    sessions.sort_by_key(|s| (s.created_at_ms, s.id));
    (snapshot.workspaces, sessions)
}

pub fn row_text(row: &Row) -> String {
    row.runs.iter().map(|run| run.text.as_str()).collect()
}

/// The client's picture of one terminal, rebuilt only from the renders it was given.
#[derive(Debug, Default, Clone)]
pub struct Screen {
    pub lines: Vec<String>,
    pub frame: u64,
    pub snapshots: usize,
    pub frames: usize,
}

impl Screen {
    pub fn apply(&mut self, render: &TerminalRender) {
        match render {
            TerminalRender::Snapshot(snapshot) => {
                self.lines = snapshot.lines.iter().map(row_text).collect();
                self.frame = snapshot.frame;
                self.snapshots += 1;
            }
            TerminalRender::Frame(frame) => {
                assert!(
                    frame.frame > self.frame,
                    "frame numbers must increase: {} after {}",
                    frame.frame,
                    self.frame
                );
                for (index, row) in &frame.dirty {
                    self.lines[usize::from(*index)] = row_text(row);
                }
                self.frame = frame.frame;
                self.frames += 1;
            }
        }
    }

    pub fn contains(&self, needle: &str) -> bool {
        self.lines.iter().any(|line| line.contains(needle))
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

/// A handle plus the screen built from its updates.
pub struct View {
    pub handle: TerminalHandle,
    pub screen: Screen,
    pub reconnects: usize,
}

impl View {
    pub fn new(handle: TerminalHandle) -> Self {
        Self {
            handle,
            screen: Screen::default(),
            reconnects: 0,
        }
    }

    /// Next update, with renders applied and acked.
    pub async fn next(&mut self) -> TerminalUpdate {
        let update = timeout(WAIT, self.handle.updates().recv())
            .await
            .expect("timed out waiting for a terminal update")
            .expect("terminal update stream ended");
        match &update {
            TerminalUpdate::Render(render) => {
                self.screen.apply(render);
                self.handle.ack(self.screen.frame);
            }
            TerminalUpdate::Reconnecting => self.reconnects += 1,
            TerminalUpdate::Gone { .. } => {}
        }
        update
    }

    /// Consumes updates until the screen satisfies `pred`; `Gone` fails the test.
    pub async fn wait_screen(&mut self, what: &str, pred: impl Fn(&Screen) -> bool) {
        while !pred(&self.screen) {
            let update = self.next().await;
            assert!(
                !matches!(update, TerminalUpdate::Gone { .. }),
                "{what}: terminal gone: {update:?}"
            );
        }
    }

    /// Consumes updates until `pick` accepts one.
    pub async fn wait_update<T>(
        &mut self,
        what: &str,
        mut pick: impl FnMut(&TerminalUpdate) -> Option<T>,
    ) -> T {
        loop {
            let update = self.next().await;
            if let Some(found) = pick(&update) {
                return found;
            }
            if matches!(update, TerminalUpdate::Gone { .. }) {
                panic!("{what}: terminal gone: {update:?}");
            }
        }
    }
}

#[allow(dead_code)]
pub fn session_id_of(session: &SessionInfo) -> SessionId {
    session.id
}

pub fn workspace_ids(workspaces: &[WorkspaceInfo]) -> Vec<WorkspaceId> {
    workspaces.iter().map(|w| w.id).collect()
}

pub fn connect(env: &Env) -> Connection {
    Connection::start(env.options())
}
