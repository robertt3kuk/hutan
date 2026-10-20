//! Shared harness for the session tests: an in-process daemon on a temporary `HUTAN_HOME`,
//! `hutan-client` connections to its real socket, and a client-side model of a terminal screen
//! that applies the render snapshots and frames the daemon sends.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::time::Duration;

use hutan_client::{Client, ClientError, ConnectOptions};
use hutan_core::{Seq, SessionId, TerminalId, WorkspaceId};
use hutan_daemon::{DaemonConfig, Paths, RunningDaemon};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{
    ClientInfo, ClientKind, Empty, Event, Request, SessionCreate, SessionInfo, SessionResult,
    Subscribed, TerminalAck, TerminalAttach, TerminalAttached, TerminalSize, TerminalSpec,
};
use hutan_protocol::render::{Row, TerminalRender};
use tempfile::TempDir;
use tokio::sync::{broadcast, mpsc};
use tokio::time::timeout;

pub const WAIT: Duration = Duration::from_secs(15);

pub struct TestDaemon {
    pub _dir: TempDir,
    pub paths: Paths,
    pub daemon: RunningDaemon,
}

impl TestDaemon {
    pub async fn start() -> Self {
        Self::start_with("", |_| {}).await
    }

    /// Starts with `daemon.toml` containing `toml` and `tweak` applied to the config. The Home
    /// folder defaults to `Hutan` inside the temporary `HUTAN_HOME`, never to `~/Hutan`.
    pub async fn start_with(toml: &str, tweak: impl FnOnce(&mut DaemonConfig)) -> Self {
        // Daemon logs show up for failing tests; filter with RUST_LOG.
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_home(dir.path());
        if !toml.is_empty() {
            std::fs::write(&paths.config_file, toml).unwrap();
        }
        let mut config = DaemonConfig::new(paths.clone());
        tweak(&mut config);
        let daemon = hutan_daemon::start(config).await.unwrap();
        Self {
            _dir: dir,
            paths,
            daemon,
        }
    }

    pub fn options(&self) -> ConnectOptions {
        ConnectOptions {
            socket: Some(self.paths.socket.clone()),
            autostart: None,
            client: ClientInfo {
                kind: ClientKind::Cli,
                version: "test".into(),
                origin: None,
            },
        }
    }

    pub async fn connect(&self) -> Client {
        Client::connect(self.options()).await.unwrap()
    }

    /// A connection that says it is a `kind` client in its `hello`.
    pub async fn connect_as(&self, kind: ClientKind) -> Client {
        let mut options = self.options();
        options.client.kind = kind;
        Client::connect(options).await.unwrap()
    }

    /// A connection that is already subscribed to events; the receiver sees everything after.
    pub async fn connect_subscribed(&self) -> (Client, broadcast::Receiver<(Seq, Event)>) {
        let client = self.connect().await;
        let events = client.events();
        client
            .call::<Subscribed>(Request::EventsSubscribe(Empty {}))
            .await
            .unwrap();
        (client, events)
    }

    pub async fn stop(self) {
        self.daemon.shutdown();
        timeout(WAIT, self.daemon.wait()).await.unwrap().unwrap();
    }

    /// Stops this daemon, then starts a new one on the same `HUTAN_HOME` (a daemon restart:
    /// new epoch, same database).
    pub async fn restart(self) -> Self {
        let Self {
            _dir,
            paths,
            daemon,
        } = self;
        daemon.shutdown();
        timeout(WAIT, daemon.wait()).await.unwrap().unwrap();
        let daemon = hutan_daemon::start(DaemonConfig::new(paths.clone()))
            .await
            .unwrap();
        Self {
            _dir,
            paths,
            daemon,
        }
    }

    /// [`restart`](Self::restart) with `tweak` applied to the new daemon's config.
    pub async fn restart_with(self, tweak: impl FnOnce(&mut DaemonConfig)) -> Self {
        let Self {
            _dir,
            paths,
            daemon,
        } = self;
        daemon.shutdown();
        timeout(WAIT, daemon.wait()).await.unwrap().unwrap();
        let mut config = DaemonConfig::new(paths.clone());
        tweak(&mut config);
        // Tests share one process: another test spawning a child at this very moment can hold
        // a copy of the lock's descriptor until its `exec`, so the lock may be released a few
        // milliseconds late.
        let daemon = timeout(WAIT, async {
            loop {
                match hutan_daemon::start(config.clone()).await {
                    Err(hutan_daemon::DaemonError::AlreadyRunning { .. }) => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    other => return other.unwrap(),
                }
            }
        })
        .await
        .unwrap();
        Self {
            _dir,
            paths,
            daemon,
        }
    }

    /// Waits for a daemon that stopped itself (a `daemon.shutdown` request), then starts a new
    /// one on the same `HUTAN_HOME`.
    pub async fn restart_after_shutdown_request(self) -> Self {
        let Self {
            _dir,
            paths,
            daemon,
        } = self;
        timeout(WAIT, daemon.wait()).await.unwrap().unwrap();
        let daemon = hutan_daemon::start(DaemonConfig::new(paths.clone()))
            .await
            .unwrap();
        Self {
            _dir,
            paths,
            daemon,
        }
    }
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

pub fn sh(script: &str) -> TerminalSpec {
    spec(&["sh", "-c", script])
}

pub async fn create_in(
    client: &Client,
    workspace_id: Option<WorkspaceId>,
    spec: TerminalSpec,
) -> SessionInfo {
    client
        .call::<SessionResult>(Request::SessionCreate(SessionCreate {
            workspace_id,
            spec: hutan_protocol::messages::SessionSpec::Terminal(spec),
        }))
        .await
        .unwrap()
        .session
}

pub async fn create(client: &Client, spec: TerminalSpec) -> SessionInfo {
    create_in(client, None, spec).await
}

pub fn terminal_of(session: &SessionInfo) -> TerminalId {
    session.terminal_id.expect("session has a terminal")
}

pub async fn stop(client: &Client, id: SessionId) -> Result<SessionInfo, ClientError> {
    client
        .call::<SessionResult>(Request::SessionStop(hutan_protocol::messages::SessionRef {
            session_id: id,
        }))
        .await
        .map(|c| c.session)
}

pub fn remote_code<T: std::fmt::Debug>(result: Result<T, ClientError>) -> ErrorCode {
    match result {
        Err(ClientError::Remote(error)) => error.code,
        other => panic!("expected a remote error, got {other:?}"),
    }
}

/// Next event satisfying `pick`, skipping the others; fails after [`WAIT`].
pub async fn wait_event<T>(
    events: &mut broadcast::Receiver<(Seq, Event)>,
    what: &str,
    mut pick: impl FnMut(&Event) -> Option<T>,
) -> T {
    let result = timeout(WAIT, async {
        loop {
            let (_, event) = events.recv().await.expect("event stream closed");
            if let Some(found) = pick(&event) {
                return found;
            }
        }
    })
    .await;
    result.unwrap_or_else(|_| panic!("timed out waiting for event: {what}"))
}

pub async fn assert_no_event(events: &mut broadcast::Receiver<(Seq, Event)>, within: Duration) {
    if let Ok(got) = timeout(within, events.recv()).await {
        panic!("unexpected event: {got:?}");
    }
}

pub fn row_text(row: &Row) -> String {
    row.runs.iter().map(|run| run.text.as_str()).collect()
}

/// The client's picture of one terminal, rebuilt only from what the daemon sent.
#[derive(Debug, Default, Clone)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<String>,
    pub frame: u64,
    pub history_len: u32,
    /// Absolute line of row 0 (protocol 0.6).
    pub viewport_top: u64,
    pub snapshots: usize,
    pub frames: usize,
}

impl Screen {
    pub fn apply(&mut self, render: &TerminalRender) {
        match render {
            TerminalRender::Snapshot(snapshot) => {
                assert_eq!(snapshot.lines.len(), usize::from(snapshot.rows));
                self.cols = snapshot.cols;
                self.rows = snapshot.rows;
                self.lines = snapshot.lines.iter().map(row_text).collect();
                self.frame = snapshot.frame;
                self.history_len = snapshot.history_len;
                self.viewport_top = snapshot.viewport_top;
                self.snapshots += 1;
            }
            TerminalRender::Frame(frame) => {
                assert!(
                    frame.frame > self.frame,
                    "frame numbers must increase: {} after {}",
                    frame.frame,
                    self.frame
                );
                assert_eq!(frame.scrolled, 0);
                for (index, row) in &frame.dirty {
                    self.lines[usize::from(*index)] = row_text(row);
                }
                self.frame = frame.frame;
                self.history_len = frame.history_len;
                self.viewport_top = frame.viewport_top;
                self.frames += 1;
            }
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn contains(&self, needle: &str) -> bool {
        self.lines.iter().any(|line| line.contains(needle))
    }

    /// Text following `prefix` on the first line containing it.
    pub fn after(&self, prefix: &str) -> Option<String> {
        self.lines.iter().find_map(|line| {
            line.find(prefix)
                .map(|at| line[at + prefix.len()..].trim().to_owned())
        })
    }
}

/// One attachment of one connection, with its own [`Screen`].
pub struct View {
    pub client: Client,
    pub terminal: TerminalId,
    pub rx: mpsc::Receiver<TerminalRender>,
    pub screen: Screen,
    /// Ack every render as it is applied inside [`View::wait_for`].
    pub auto_ack: bool,
    pub attached: TerminalAttached,
    /// Frame number most recently acked through this view.
    pub acked: u64,
}

impl View {
    /// Attaches and applies the snapshot the daemon queued before its response.
    pub async fn attach(client: &Client, terminal: TerminalId, size: Option<TerminalSize>) -> Self {
        // The sink must exist before the daemon's snapshot frame arrives.
        let rx = client.terminal_frames(terminal);
        let attached: TerminalAttached = client
            .call(Request::TerminalAttach(TerminalAttach {
                terminal_id: terminal,
                size,
            }))
            .await
            .unwrap();
        let mut view = Self {
            client: client.clone(),
            terminal,
            rx,
            screen: Screen::default(),
            auto_ack: true,
            attached,
            acked: 0,
        };
        let first = view.next_render().await;
        assert!(
            matches!(&first, TerminalRender::Snapshot(s) if s.frame == attached.frame),
            "the first render must be the snapshot announced by the response: {first:?}"
        );
        view
    }

    pub async fn next_render(&mut self) -> TerminalRender {
        let render = timeout(WAIT, self.rx.recv())
            .await
            .expect("timed out waiting for a render")
            .expect("render stream closed");
        self.screen.apply(&render);
        render
    }

    pub async fn ack(&mut self) {
        self.acked = self.screen.frame;
        self.client
            .call::<Empty>(Request::TerminalAck(TerminalAck {
                terminal_id: self.terminal,
                frame: self.screen.frame,
            }))
            .await
            .unwrap();
    }

    /// Applies renders (acking each when `auto_ack`) until the screen satisfies `pred`.
    pub async fn wait_for(&mut self, what: &str, pred: impl Fn(&Screen) -> bool) {
        let outcome = timeout(WAIT, async {
            loop {
                if pred(&self.screen) {
                    return;
                }
                // Credit is one frame: the render being waited for needs the last one acked.
                if self.auto_ack && self.acked != self.screen.frame {
                    self.ack().await;
                }
                self.next_render().await;
            }
        })
        .await;
        assert!(
            outcome.is_ok(),
            "{what}: timed out; screen:\n{}",
            self.screen.text()
        );
    }

    /// Fails if any render arrives within `quiet`.
    pub async fn expect_silence(&mut self, quiet: Duration) {
        if let Ok(got) = timeout(quiet, self.rx.recv()).await {
            panic!("unexpected render while no credit was available: {got:?}");
        }
    }

    /// Drops renders that were already delivered.
    pub fn drain(&mut self) -> usize {
        let mut dropped = 0;
        while self.rx.try_recv().is_ok() {
            dropped += 1;
        }
        dropped
    }
}
