//! Integration tests: the daemon runs in-process on a temporary `HUTAN_HOME` layout and is
//! driven over its real Unix socket by `hutan-client` (or raw frames where the client
//! deliberately cannot misbehave).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::os::unix::net::UnixListener as StdUnixListener;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use hutan_client::{Client, ClientError, ConnectOptions};
use hutan_core::{Epoch, Seq, SessionId, WorkspaceId};
use hutan_daemon::{DaemonConfig, DaemonError, Paths, RunningDaemon};
use hutan_protocol::messages::{
    ClientInfo, ClientKind, DaemonShutdown, DaemonStatusInfo, Empty, Event, Hello, Request,
    ResumeOutcome, ResumePoint, SessionRef, SessionSnapshot, Subscribed, WorkspaceCreate,
    WorkspaceInfo, WorkspaceKind, WorkspaceList, WorkspaceRef, WorkspaceRename,
};
use hutan_protocol::{
    ControlMessage, ErrorBody, ErrorCode, FrameCodec, PROTOCOL_VERSION, ProtocolVersion,
};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::broadcast;
use tokio::time::timeout;
use tokio_util::codec::{FramedRead, FramedWrite};

const WAIT: Duration = Duration::from_secs(10);

struct TestDaemon {
    _dir: TempDir,
    paths: Paths,
    daemon: RunningDaemon,
}

impl TestDaemon {
    async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    async fn start_with(tweak: impl FnOnce(&mut DaemonConfig)) -> Self {
        // Daemon logs show up for failing tests; filter with RUST_LOG.
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .try_init();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_home(dir.path());
        let mut config = DaemonConfig::new(paths.clone());
        tweak(&mut config);
        let daemon = hutan_daemon::start(config).await.unwrap();
        Self {
            _dir: dir,
            paths,
            daemon,
        }
    }

    fn client_info() -> ClientInfo {
        ClientInfo {
            kind: ClientKind::Cli,
            version: "test".into(),
            origin: None,
        }
    }

    fn options(&self) -> ConnectOptions {
        ConnectOptions {
            socket: Some(self.paths.socket.clone()),
            autostart: None,
            client: Self::client_info(),
        }
    }

    async fn connect(&self) -> Client {
        Client::connect(self.options()).await.unwrap()
    }

    async fn stop(self) {
        self.daemon.shutdown();
        self.daemon.wait().await.unwrap();
    }
}

fn subscribe() -> Request {
    Request::EventsSubscribe(Empty {})
}

async fn create_workspace(client: &Client, name: &str) -> WorkspaceInfo {
    client
        .call(Request::WorkspaceCreate(WorkspaceCreate {
            name: name.into(),
            paths: vec![],
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap()
}

async fn next_event(rx: &mut broadcast::Receiver<(Seq, Event)>) -> (Seq, Event) {
    timeout(WAIT, rx.recv()).await.unwrap().unwrap()
}

async fn assert_no_event(rx: &mut broadcast::Receiver<(Seq, Event)>) {
    let got = timeout(Duration::from_millis(300), rx.recv()).await;
    assert!(got.is_err(), "unexpected event: {got:?}");
}

/// A bare protocol peer for exercising paths `hutan-client` never takes.
struct Raw {
    reader: FramedRead<OwnedReadHalf, FrameCodec>,
    writer: FramedWrite<OwnedWriteHalf, FrameCodec>,
}

impl Raw {
    async fn connect(daemon: &TestDaemon) -> Self {
        let stream = UnixStream::connect(&daemon.paths.socket).await.unwrap();
        let (read, write) = stream.into_split();
        Self {
            reader: FramedRead::new(read, FrameCodec),
            writer: FramedWrite::new(write, FrameCodec),
        }
    }

    async fn send(&mut self, msg: ControlMessage) {
        self.writer.send(msg.to_frame().unwrap()).await.unwrap();
    }

    async fn send_request(&mut self, id: u64, request: &Request) {
        self.send(ControlMessage::request(id, request).unwrap())
            .await;
    }

    /// Next control message; `None` once the daemon closed the connection.
    async fn recv(&mut self) -> Option<ControlMessage> {
        match timeout(WAIT, self.reader.next()).await.unwrap() {
            Some(Ok(frame)) => Some(ControlMessage::from_frame(&frame).unwrap()),
            Some(Err(_)) | None => None,
        }
    }

    async fn recv_error(&mut self) -> ErrorBody {
        match self.recv().await.expect("connection closed") {
            ControlMessage::Response {
                outcome: Err(error),
                ..
            } => error,
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    fn hello(major: u16) -> Request {
        Request::Hello(Hello {
            protocol: ProtocolVersion { major, minor: 0 },
            client: TestDaemon::client_info(),
            capabilities: vec![],
            resume: None,
        })
    }

    async fn handshake(&mut self) {
        self.send_request(0, &Self::hello(PROTOCOL_VERSION.major))
            .await;
        match self.recv().await.unwrap() {
            ControlMessage::Response { id: 0, outcome } => assert!(outcome.is_ok()),
            other => panic!("expected welcome, got {other:?}"),
        }
    }
}

// 1 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn handshake_then_status_reports_epoch_and_pid() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;

    assert_eq!(client.epoch(), t.daemon.epoch());
    let welcome = client.welcome();
    assert_eq!(welcome.protocol, PROTOCOL_VERSION);
    assert_eq!(
        welcome.capabilities,
        [
            "terminal.render.v1",
            "terminal.colors",
            "session.restore",
            "agent.report",
            "terminal.blocks",
            "workspace.projects",
            "history.search",
            "chat.codex",
            "agent.tree",
            "chat.direct",
            "assistant",
            "notify.intents",
            "worktree",
            "chat.provenance",
            "task",
            "harness.acp",
            "accounts",
            "resources",
            "projects.auto",
            "assistant.backend",
            "control",
            "worktree.review",
            "chat.rich_input",
            "chat.questions",
            "task.review",
            "session.recovery",
            "task.chat_model",
            "daemon.settings",
            "history.sessions",
            "session.close",
            "control.grants",
            "memory",
            "fs",
            "fs.watch",
            "notify.mute",
            "chat.retry",
            "worktree.index",
            "chat.mode"
        ]
    );
    assert_eq!(welcome.resume, ResumeOutcome::SnapshotRequired);

    let status: DaemonStatusInfo = client.call(Request::DaemonStatus(Empty {})).await.unwrap();
    assert_eq!(status.epoch, t.daemon.epoch());
    assert_eq!(status.pid, std::process::id());
    assert_eq!(status.protocol, PROTOCOL_VERSION);
    assert_eq!(status.clients, 1);
    assert_eq!(status.sessions, 0);
    t.stop().await;
}

// 2 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn incompatible_major_gets_unsupported_version_then_close() {
    let t = TestDaemon::start().await;
    let mut raw = Raw::connect(&t).await;
    raw.send_request(0, &Raw::hello(PROTOCOL_VERSION.major + 1))
        .await;

    let error = raw.recv_error().await;
    assert_eq!(error.code, ErrorCode::UnsupportedVersion);
    assert!(raw.recv().await.is_none(), "connection must close");

    // The daemon is unaffected.
    let client = t.connect().await;
    client
        .call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {}))
        .await
        .unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn first_frame_must_be_hello() {
    let t = TestDaemon::start().await;
    let mut raw = Raw::connect(&t).await;
    raw.send_request(5, &Request::DaemonStatus(Empty {})).await;
    assert_eq!(raw.recv_error().await.code, ErrorCode::BadRequest);
    assert!(raw.recv().await.is_none());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_methods_and_failed_requests_do_not_close_the_connection() {
    let t = TestDaemon::start().await;
    let mut raw = Raw::connect(&t).await;
    raw.handshake().await;

    raw.send(ControlMessage::Request {
        id: 1,
        method: "nope.nothing".into(),
        params: serde_json::json!({}),
    })
    .await;
    assert_eq!(raw.recv_error().await.code, ErrorCode::BadRequest);

    // Methods fail with their own error, not with a closed connection.
    raw.send_request(
        2,
        &Request::SessionStop(SessionRef {
            session_id: SessionId::new(),
        }),
    )
    .await;
    assert_eq!(raw.recv_error().await.code, ErrorCode::NotFound);

    raw.send_request(3, &Raw::hello(PROTOCOL_VERSION.major))
        .await;
    assert_eq!(raw.recv_error().await.code, ErrorCode::BadRequest);

    // Binary frames from clients are accepted and ignored.
    raw.writer
        .send(hutan_protocol::Frame::terminal_input(
            hutan_core::TerminalId(7),
            b"ls\n",
        ))
        .await
        .unwrap();
    raw.send_request(4, &Request::DaemonStatus(Empty {})).await;
    match raw.recv().await.unwrap() {
        ControlMessage::Response { id: 4, outcome } => assert!(outcome.is_ok()),
        other => panic!("expected status response, got {other:?}"),
    }
    t.stop().await;
}

// 3 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn workspace_mutations_emit_events_in_seq_order() {
    let t = TestDaemon::start().await;
    let subscriber = t.connect().await;
    let mut events = subscriber.events();
    let subscribed: Subscribed = subscriber.call(subscribe()).await.unwrap();
    assert_eq!(subscribed.seq, Seq(0));

    let actor = t.connect().await;
    let created = create_workspace(&actor, "alpha").await;
    assert_eq!(created.name, "alpha");
    let renamed: WorkspaceInfo = actor
        .call(Request::WorkspaceRename(WorkspaceRename {
            workspace_id: created.id,
            name: "beta".into(),
        }))
        .await
        .unwrap();
    assert_eq!(renamed.id, created.id);
    actor
        .call::<Empty>(Request::WorkspaceClose(WorkspaceRef {
            workspace_id: created.id,
        }))
        .await
        .unwrap();

    let (s1, e1) = next_event(&mut events).await;
    let (s2, e2) = next_event(&mut events).await;
    let (s3, e3) = next_event(&mut events).await;
    assert_eq!((s1, s2, s3), (Seq(1), Seq(2), Seq(3)));
    assert_eq!(
        e1,
        Event::WorkspaceUpdated {
            workspace: created.clone()
        }
    );
    assert_eq!(e2, Event::WorkspaceUpdated { workspace: renamed });
    assert_eq!(
        e3,
        Event::WorkspaceClosed {
            workspace_id: created.id
        }
    );
    assert_eq!(subscriber.last_seq(), Seq(3));

    // The actor never subscribed and saw no events.
    assert_eq!(actor.last_seq(), Seq(0));

    // Errors: unknown workspace, empty name.
    let missing = actor
        .call::<Empty>(Request::WorkspaceClose(WorkspaceRef {
            workspace_id: WorkspaceId::new(),
        }))
        .await;
    assert!(matches!(
        missing,
        Err(ClientError::Remote(ErrorBody {
            code: ErrorCode::NotFound,
            ..
        }))
    ));
    let empty = actor
        .call::<WorkspaceInfo>(Request::WorkspaceCreate(WorkspaceCreate {
            name: "  ".into(),
            paths: vec![],
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await;
    assert!(matches!(
        empty,
        Err(ClientError::Remote(ErrorBody {
            code: ErrorCode::BadRequest,
            ..
        }))
    ));
    assert_no_event(&mut events).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn home_workspace_exists_and_snapshot_carries_seq() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;

    let list: WorkspaceList = client.call(Request::WorkspaceList(Empty {})).await.unwrap();
    assert_eq!(list.workspaces.len(), 1);
    assert_eq!(list.workspaces[0].name, "Home");
    assert_eq!(list.workspaces[0].kind, WorkspaceKind::Home);

    create_workspace(&client, "second").await;
    let snapshot: SessionSnapshot = client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap();
    assert_eq!(snapshot.workspaces.len(), 2);
    assert!(snapshot.sessions.is_empty());
    assert_eq!(snapshot.seq, Seq(1));
    t.stop().await;
}

// 4 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn resume_in_same_epoch_replays_exactly_the_missed_events() {
    let t = TestDaemon::start().await;

    let first = t.connect().await;
    let mut first_events = first.events();
    first.call::<Subscribed>(subscribe()).await.unwrap();
    create_workspace(&first, "seen").await;
    next_event(&mut first_events).await;
    let resume = ResumePoint {
        epoch: first.epoch(),
        seq: first.last_seq(),
    };
    assert_eq!(resume.seq, Seq(1));
    drop(first_events);
    drop(first);

    let other = t.connect().await;
    let missed: Vec<_> = {
        let mut v = Vec::new();
        for name in ["m1", "m2", "m3"] {
            v.push(create_workspace(&other, name).await);
        }
        v
    };

    let resumed = Client::connect_resuming(t.options(), Some(resume))
        .await
        .unwrap();
    let mut events = resumed.events;
    assert_eq!(resumed.client.welcome().resume, ResumeOutcome::Replayed);
    for (offset, workspace) in missed.iter().enumerate() {
        let (seq, event) = next_event(&mut events).await;
        assert_eq!(seq, Seq(2 + offset as u64));
        assert_eq!(
            event,
            Event::WorkspaceUpdated {
                workspace: workspace.clone()
            }
        );
    }
    assert_no_event(&mut events).await;
    assert_eq!(resumed.client.last_seq(), Seq(4));

    // Replay also subscribed the connection: live events keep flowing without events.subscribe.
    let live = create_workspace(&other, "live").await;
    let (seq, event) = next_event(&mut events).await;
    assert_eq!(seq, Seq(5));
    assert_eq!(event, Event::WorkspaceUpdated { workspace: live });

    // Resuming exactly at the head replays nothing.
    let head = Client::connect_resuming(
        t.options(),
        Some(ResumePoint {
            epoch: other.epoch(),
            seq: Seq(5),
        }),
    )
    .await
    .unwrap();
    assert_eq!(head.client.welcome().resume, ResumeOutcome::Replayed);
    let mut head_events = head.events;
    assert_no_event(&mut head_events).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_with_unknown_epoch_requires_snapshot() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    create_workspace(&client, "one").await;

    let resumed = Client::connect_resuming(
        t.options(),
        Some(ResumePoint {
            epoch: Epoch::new(),
            seq: Seq(0),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        resumed.client.welcome().resume,
        ResumeOutcome::SnapshotRequired
    );
    let mut events = resumed.events;

    // Not subscribed and nothing replayed until the client asks.
    create_workspace(&client, "two").await;
    assert_no_event(&mut events).await;

    let snapshot: SessionSnapshot = resumed
        .client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap();
    assert_eq!(snapshot.seq, Seq(2));
    resumed
        .client
        .call::<Subscribed>(subscribe())
        .await
        .unwrap();
    create_workspace(&client, "three").await;
    let (seq, _) = next_event(&mut events).await;
    assert_eq!(seq, Seq(3));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_beyond_the_ring_requires_snapshot() {
    let t = TestDaemon::start_with(|c| c.ring.max_events = 2).await;
    let client = t.connect().await;
    for name in ["a", "b", "c", "d", "e"] {
        create_workspace(&client, name).await;
    }
    // Ring holds seq 4 and 5.
    let epoch = client.epoch();
    let evicted = Client::connect_resuming(t.options(), Some(ResumePoint { epoch, seq: Seq(2) }))
        .await
        .unwrap();
    assert_eq!(
        evicted.client.welcome().resume,
        ResumeOutcome::SnapshotRequired
    );

    let edge = Client::connect_resuming(t.options(), Some(ResumePoint { epoch, seq: Seq(3) }))
        .await
        .unwrap();
    assert_eq!(edge.client.welcome().resume, ResumeOutcome::Replayed);
    let mut events = edge.events;
    assert_eq!(next_event(&mut events).await.0, Seq(4));
    assert_eq!(next_event(&mut events).await.0, Seq(5));

    let future = Client::connect_resuming(
        t.options(),
        Some(ResumePoint {
            epoch,
            seq: Seq(99),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        future.client.welcome().resume,
        ResumeOutcome::SnapshotRequired
    );
    t.stop().await;
}

// 5 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn client_that_never_reads_is_disconnected_while_others_keep_receiving() {
    const RENAMES: usize = 300;
    let t = TestDaemon::start_with(|c| c.outbound_queue = 16).await;

    let good = t.connect().await;
    let mut good_events = good.events();
    good.call::<Subscribed>(subscribe()).await.unwrap();

    // Subscribes, then never reads again.
    let mut lazy = Raw::connect(&t).await;
    lazy.handshake().await;
    lazy.send_request(1, &subscribe()).await;
    match lazy.recv().await.unwrap() {
        ControlMessage::Response { id: 1, outcome } => assert!(outcome.is_ok()),
        other => panic!("expected subscribed, got {other:?}"),
    }

    let workspace = create_workspace(&good, "flood").await;
    let padding = "x".repeat(32 * 1024);
    for i in 0..RENAMES {
        let _: WorkspaceInfo = good
            .call(Request::WorkspaceRename(WorkspaceRename {
                workspace_id: workspace.id,
                name: format!("{i}-{padding}"),
            }))
            .await
            .unwrap();
    }

    // The healthy client saw every event, in order.
    let mut last = Seq(0);
    for _ in 0..=RENAMES {
        let (seq, _) = next_event(&mut good_events).await;
        assert_eq!(seq, last.next());
        last = seq;
    }

    // The daemon closed the lazy connection: reading it ends in EOF after at most the
    // buffered data, long before all events arrived.
    let mut received = 0usize;
    while lazy.recv().await.is_some() {
        received += 1;
    }
    assert!(received < RENAMES, "lazy client received {received} events");

    // And is still healthy for everyone else.
    let status: DaemonStatusInfo = good.call(Request::DaemonStatus(Empty {})).await.unwrap();
    assert_eq!(status.clients, 1);
    t.stop().await;
}

// 6 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn second_daemon_on_the_same_home_fails_to_start() {
    let t = TestDaemon::start().await;

    let second = hutan_daemon::start(DaemonConfig::new(t.paths.clone())).await;
    match second {
        Err(DaemonError::AlreadyRunning { lock }) => assert_eq!(lock, t.paths.lock),
        other => panic!("expected AlreadyRunning, got {other:?}"),
    }

    // The first daemon is untouched, socket included.
    let client = t.connect().await;
    client
        .call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {}))
        .await
        .unwrap();
    t.stop().await;
}

// 7 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_replies_removes_socket_and_releases_lock() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    assert!(t.paths.socket.exists());

    let reply = client
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: false,
            forget: false,
        }))
        .await;
    assert!(reply.is_ok(), "shutdown must be acknowledged: {reply:?}");

    timeout(WAIT, t.daemon.wait()).await.unwrap().unwrap();
    assert!(!t.paths.socket.exists(), "socket must be removed");

    // The lock is free again: a new daemon starts on the same home.
    let again = hutan_daemon::start(DaemonConfig::new(t.paths.clone()))
        .await
        .unwrap();
    again.shutdown();
    again.wait().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_socket_from_a_dead_daemon_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_home(dir.path());
    paths.prepare().unwrap();
    // A bound-then-dropped listener leaves the socket file behind, like a crashed daemon.
    drop(StdUnixListener::bind(&paths.socket).unwrap());
    assert!(paths.socket.exists());

    let daemon = hutan_daemon::start(DaemonConfig::new(paths.clone()))
        .await
        .unwrap();
    let client = Client::connect(ConnectOptions {
        socket: Some(paths.socket.clone()),
        autostart: None,
        client: TestDaemon::client_info(),
    })
    .await
    .unwrap();
    client
        .call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {}))
        .await
        .unwrap();
    daemon.shutdown();
    daemon.wait().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_whose_socket_is_deleted_or_replaced_stops_and_spares_the_new_file() {
    // Deleted, as when its `HUTAN_HOME` is removed: unreachable, so it stops.
    let t = TestDaemon::start_with(|c| c.socket_check_interval = Duration::from_millis(50)).await;
    std::fs::remove_file(&t.paths.socket).unwrap();
    timeout(WAIT, t.daemon.wait()).await.unwrap().unwrap();

    // Replaced by another socket: it stops and leaves that file where it is.
    let t = TestDaemon::start_with(|c| c.socket_check_interval = Duration::from_millis(50)).await;
    std::fs::remove_file(&t.paths.socket).unwrap();
    let other = StdUnixListener::bind(&t.paths.socket).unwrap();
    timeout(WAIT, t.daemon.wait()).await.unwrap().unwrap();
    assert!(t.paths.socket.exists(), "the other socket is not removed");
    drop(other);

    // A daemon whose socket stays put keeps running.
    let t = TestDaemon::start_with(|c| c.socket_check_interval = Duration::from_millis(50)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let client = t.connect().await;
    client
        .call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {}))
        .await
        .unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn socket_is_private_and_pending_calls_fail_when_the_daemon_stops() {
    use std::os::unix::fs::PermissionsExt;

    let t = TestDaemon::start().await;
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&t.paths.socket), 0o600);
    assert_eq!(mode(&t.paths.base), 0o700);

    let client = t.connect().await;
    let mut events = client.events();
    let socket = t.paths.socket.clone();
    t.daemon.shutdown();
    t.daemon.wait().await.unwrap();
    assert!(!socket.exists());

    // The connection was closed by the daemon: calls fail instead of hanging.
    let result = timeout(
        WAIT,
        client.call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {})),
    )
    .await
    .unwrap();
    assert!(
        matches!(result, Err(ClientError::Disconnected)),
        "{result:?}"
    );
    assert!(timeout(WAIT, events.recv()).await.unwrap().is_err());
}

// client autostart -------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn client_autostarts_hutand_when_the_socket_is_absent() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_home(dir.path().join("home"));
    // `ConnectOptions` cannot carry environment, so a wrapper pins the daemon to the temp home.
    let wrapper = dir.path().join("hutand-wrapper.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport HUTAN_HOME='{}'\nexec '{}'\n",
            paths.base.display(),
            env!("CARGO_BIN_EXE_hutand"),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!paths.socket.exists());

    let client = Client::connect(ConnectOptions {
        socket: Some(paths.socket.clone()),
        autostart: Some(wrapper),
        client: TestDaemon::client_info(),
    })
    .await
    .unwrap();
    let status: DaemonStatusInfo = client.call(Request::DaemonStatus(Empty {})).await.unwrap();
    assert_ne!(
        status.pid,
        std::process::id(),
        "daemon runs in its own process"
    );
    assert_eq!(status.epoch, client.epoch());

    client
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: false,
            forget: false,
        }))
        .await
        .unwrap();
    timeout(WAIT, async {
        while paths.socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

// Handshake timeout ----------------------------------------------------------------------------

/// Waits for the daemon to close `stream`; fails if it stays open past `WAIT`.
async fn closed_by_daemon(stream: &mut UnixStream) -> Duration {
    use tokio::io::AsyncReadExt;
    let started = std::time::Instant::now();
    let mut buf = [0u8; 64];
    match timeout(WAIT, stream.read(&mut buf)).await {
        Ok(Ok(0)) => started.elapsed(),
        Ok(Ok(n)) => panic!("daemon sent {n} unexpected bytes to a silent peer"),
        Ok(Err(_)) => started.elapsed(), // reset counts as closed
        Err(_) => panic!("the daemon kept a silent connection open"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_that_never_says_hello_is_closed_while_others_work() {
    use tokio::io::AsyncWriteExt;
    const TIMEOUT: Duration = Duration::from_millis(400);
    let t = TestDaemon::start_with(|c| c.handshake_timeout = TIMEOUT).await;
    let client = t.connect().await;

    // One peer says nothing at all, one starts a frame header and stalls.
    let mut silent = UnixStream::connect(&t.paths.socket).await.unwrap();
    let mut stalled = UnixStream::connect(&t.paths.socket).await.unwrap();
    stalled.write_all(&[0x00, 0x00]).await.unwrap();

    // The daemon keeps serving the well-behaved client meanwhile.
    let _: DaemonStatusInfo = client.call(Request::DaemonStatus(Empty {})).await.unwrap();

    let waited = closed_by_daemon(&mut silent).await;
    assert!(
        waited >= TIMEOUT - Duration::from_millis(100),
        "closed after {waited:?}, before the timeout"
    );
    closed_by_daemon(&mut stalled).await;

    // The timeout covers `hello` only: a handshaken client may idle for longer than it.
    tokio::time::sleep(TIMEOUT + Duration::from_millis(200)).await;
    let status: DaemonStatusInfo = client.call(Request::DaemonStatus(Empty {})).await.unwrap();
    assert_eq!(status.clients, 1, "only the handshaken client is counted");
    t.stop().await;
}
