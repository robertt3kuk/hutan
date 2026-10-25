//! The protocol over byte streams that are not the daemon's listener (slice R-1, ADR 0022): an
//! in-memory duplex and a socket pair carry the full handshake and request/terminal flows, a
//! stream that moves a few bytes at a time splits every frame arbitrarily, and nothing in the
//! daemon, client, CLI or protocol crates opens a network listener.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::path::{Path, PathBuf};

use bytes::BytesMut;
use common::*;
use futures::StreamExt;
use hutan_client::{Client, ClientError, Resumed};
use hutan_protocol::messages::{
    ClientInfo, ClientKind, DaemonStatusInfo, Empty, Hello, Request, ResumeOutcome,
    WorkspaceCreate, WorkspaceInfo, WorkspaceList,
};
use hutan_protocol::{ControlMessage, FrameCodec, PROTOCOL_VERSION};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::timeout;
use tokio_util::codec::{Encoder, FramedRead};

fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Cli,
        version: "transport-test".into(),
        origin: None,
    }
}

async fn status(client: &Client) -> DaemonStatusInfo {
    client.call(Request::DaemonStatus(Empty {})).await.unwrap()
}

/// Handshakes over `stream` with the real client and runs the flows every transport must carry:
/// the welcome, requests with their responses, and a terminal's snapshot, frames and input.
async fn run_flows<S>(t: &TestDaemon, stream: S) -> Client
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let Resumed { client, .. } = Client::connect_stream(stream, client_info(), None)
        .await
        .unwrap();

    let welcome = client.welcome();
    assert_eq!(welcome.protocol, PROTOCOL_VERSION);
    assert_eq!(welcome.resume, ResumeOutcome::SnapshotRequired);
    assert_eq!(client.epoch(), t.daemon.epoch());
    assert!(status(&client).await.clients >= 1);

    let created: WorkspaceInfo = client
        .call(Request::WorkspaceCreate(WorkspaceCreate {
            name: "over-a-stream".into(),
            paths: vec![],
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap();
    let listed: WorkspaceList = client.call(Request::WorkspaceList(Empty {})).await.unwrap();
    assert!(
        listed
            .workspaces
            .iter()
            .any(|w: &WorkspaceInfo| w.id == created.id)
    );

    let session = create(&client, sh("echo hello; exec cat")).await;
    let terminal = terminal_of(&session);
    let mut view = View::attach(&client, terminal, None).await;
    view.wait_for("output rendered", |s| s.contains("hello"))
        .await;
    client.send_input(terminal, b"ping\r").await.unwrap();
    view.wait_for("typed input rendered", |s| s.contains("ping"))
        .await;
    drop(view);
    client
}

#[tokio::test(flavor = "multi_thread")]
async fn full_protocol_runs_over_an_in_memory_duplex() {
    let t = TestDaemon::start().await;
    let (near, far) = tokio::io::duplex(64 * 1024);
    let served = t.daemon.serve_stream(far);

    let client = run_flows(&t, near).await;

    drop(client);
    timeout(WAIT, served).await.unwrap().unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn full_protocol_runs_over_a_socketpair() {
    let t = TestDaemon::start().await;
    let (near, far) = UnixStream::pair().unwrap();
    let served = t.daemon.serve_stream(far);

    let client = run_flows(&t, near).await;

    drop(client);
    timeout(WAIT, served).await.unwrap().unwrap();
    t.stop().await;
}

/// A stream that carries three bytes at a time in each direction: every frame header, payload
/// and terminal snapshot is cut into pieces at arbitrary places.
#[tokio::test(flavor = "multi_thread")]
async fn frames_split_at_arbitrary_places_are_handled() {
    let t = TestDaemon::start().await;
    let (near, far) = tokio::io::duplex(3);
    let served = t.daemon.serve_stream(far);

    let client = run_flows(&t, near).await;

    drop(client);
    timeout(WAIT, served).await.unwrap().unwrap();
    t.stop().await;
}

/// The daemon reads a `hello` that arrives one byte per write, each flushed on its own.
#[tokio::test(flavor = "multi_thread")]
async fn hello_written_byte_by_byte_is_answered() {
    let t = TestDaemon::start().await;
    let (near, far) = tokio::io::duplex(16);
    let served = t.daemon.serve_stream(far);
    let (read, mut write) = tokio::io::split(near);

    let hello = ControlMessage::request(
        0,
        &Request::Hello(Hello {
            protocol: PROTOCOL_VERSION,
            client: client_info(),
            capabilities: vec![],
            resume: None,
        }),
    )
    .unwrap()
    .to_frame()
    .unwrap();
    let mut bytes = BytesMut::new();
    FrameCodec.encode(hello, &mut bytes).unwrap();
    for byte in bytes {
        write.write_all(&[byte]).await.unwrap();
        write.flush().await.unwrap();
    }

    let mut reader = FramedRead::new(read, FrameCodec);
    let frame = timeout(WAIT, reader.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match ControlMessage::from_frame(&frame).unwrap() {
        ControlMessage::Response { id: 0, outcome } => assert!(outcome.is_ok()),
        other => panic!("expected the welcome, got {other:?}"),
    }

    // Closing the stream's write side ends the connection on the daemon's side.
    write.shutdown().await.unwrap();
    timeout(WAIT, served).await.unwrap().unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_that_does_not_greet_first_is_closed() {
    let t = TestDaemon::start().await;
    let (near, far) = tokio::io::duplex(1024);
    let served = t.daemon.serve_stream(far);
    let (read, mut write) = tokio::io::split(near);

    // Not a hello: a request before the handshake.
    let frame = ControlMessage::request(1, &Request::DaemonStatus(Empty {}))
        .unwrap()
        .to_frame()
        .unwrap();
    let mut bytes = BytesMut::new();
    FrameCodec.encode(frame, &mut bytes).unwrap();
    write.write_all(&bytes).await.unwrap();
    write.flush().await.unwrap();

    let mut reader = FramedRead::new(read, FrameCodec);
    // An error response may come first; the stream must then end.
    while let Some(item) = timeout(WAIT, reader.next()).await.unwrap() {
        if item.is_err() {
            break;
        }
    }
    timeout(WAIT, served).await.unwrap().unwrap();
    t.stop().await;
}

/// Dropping the client ends the daemon's connection task and its client count; no half of
/// either side is left running.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_client_ends_the_connection_task() {
    let t = TestDaemon::start().await;
    let observer = t.connect().await;
    let before = status(&observer).await.clients;

    let (near, far) = tokio::io::duplex(64 * 1024);
    let served = t.daemon.serve_stream(far);
    let client = Client::connect_stream(near, client_info(), None)
        .await
        .unwrap()
        .client;
    assert_eq!(status(&client).await.clients, before + 1);

    drop(client);
    timeout(WAIT, served).await.unwrap().unwrap();
    assert_eq!(status(&observer).await.clients, before);
    t.stop().await;
}

/// A daemon that stops closes a stream connection too, and the client sees it as a disconnect.
#[tokio::test(flavor = "multi_thread")]
async fn daemon_shutdown_closes_stream_connections() {
    let t = TestDaemon::start().await;
    let (near, far) = tokio::io::duplex(64 * 1024);
    let served = t.daemon.serve_stream(far);
    let client = Client::connect_stream(near, client_info(), None)
        .await
        .unwrap()
        .client;
    status(&client).await;

    t.daemon.shutdown();
    timeout(WAIT, served).await.unwrap().unwrap();
    let outcome = timeout(WAIT, async {
        loop {
            match client
                .call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {}))
                .await
            {
                Err(ClientError::Disconnected) => return,
                Err(other) => panic!("unexpected error: {other:?}"),
                Ok(_) => tokio::task::yield_now().await,
            }
        }
    })
    .await;
    assert!(outcome.is_ok(), "the client never saw the disconnect");
    timeout(WAIT, t.daemon.wait()).await.unwrap().unwrap();
}

// No network listener ---------------------------------------------------------------------------

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// ADR 0022: until device authentication exists no non-local byte stream is accepted. The
/// crates that speak the protocol (daemon, client, CLI, protocol) open no TCP/UDP socket and
/// pull in no HTTP/WebSocket server; the only listener is the daemon's Unix socket.
#[test]
fn no_network_listener_exists() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let forbidden = [
        "TcpListener",
        "TcpSocket",
        "UdpSocket",
        "hyper::server",
        "axum",
        "tungstenite",
        "warp::",
    ];
    for name in ["daemon", "client", "cli", "protocol"] {
        let mut files = Vec::new();
        rust_files(&crates.join(name).join("src"), &mut files);
        assert!(!files.is_empty(), "no sources found for {name}");
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
                for needle in forbidden {
                    assert!(
                        !line.contains(needle),
                        "{} mentions `{needle}`: {line}",
                        file.display()
                    );
                }
            }
        }
        let manifest = std::fs::read_to_string(crates.join(name).join("Cargo.toml")).unwrap();
        for dependency in ["hyper", "axum", "tungstenite", "warp"] {
            assert!(
                !manifest
                    .lines()
                    .any(|l| l.trim_start().starts_with(dependency)),
                "{name}/Cargo.toml depends on {dependency}"
            );
        }
    }
}
