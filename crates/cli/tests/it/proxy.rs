//! `hutan proxy` (slice R-2, ADR 0022): the bytes-only bridge between standard input/output and
//! the daemon's socket carries the whole protocol, as an in-process bridge over a pipe pair and
//! as the real `hutan proxy` process with the client on its stdio, the way `ssh` runs it.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::io;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};

use hutan_cli::proxy::{Ended, bridge};
use hutan_client::{Client, Resumed};
use hutan_core::TerminalId;
use hutan_protocol::messages::{
    ClientInfo, ClientKind, DaemonStatusInfo, Empty, Request, TerminalAck, TerminalAttach,
    TerminalAttached,
};
use hutan_protocol::render::TerminalRender;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UnixStream;
use tokio::process::{ChildStdin, ChildStdout};
use tokio::time::timeout;

use crate::common::*;

/// A child's standard input and output as one stream. Shutting it down closes the child's
/// input (`ChildStdin`'s own shutdown does not), as the SSH stream of the client does.
struct Stdio2 {
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
}

impl AsyncRead for Stdio2 {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stdio2 {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.stdin.as_mut() {
            Some(stdin) => Pin::new(stdin).poll_write(cx, data),
            None => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.stdin.as_mut() {
            Some(stdin) => Pin::new(stdin).poll_flush(cx),
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.stdin = None;
        Poll::Ready(Ok(()))
    }
}

fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Cli,
        version: "proxy-test".into(),
        origin: None,
    }
}

/// A session that echoes what is typed; its terminal.
async fn echo_terminal(t: &TestDaemon) -> TerminalId {
    let id = t
        .new_session(&[], &["sh", "-c", "echo ready; exec cat"])
        .await;
    let session = t.session(&id).await;
    serde_json::from_value(session["terminal_id"].clone()).unwrap()
}

/// Attaches, acks the snapshot, types a line and waits for the frame that follows: control
/// frames, terminal render frames and terminal input frames all crossed the bridge.
async fn run_flows(t: &TestDaemon, client: &Client) {
    let status: DaemonStatusInfo = client.call(Request::DaemonStatus(Empty {})).await.unwrap();
    assert!(status.clients >= 1);

    let terminal = echo_terminal(t).await;
    let mut frames = client.terminal_frames(terminal);
    let attached: TerminalAttached = client
        .call(Request::TerminalAttach(TerminalAttach {
            terminal_id: terminal,
            size: None,
        }))
        .await
        .unwrap();
    let first = timeout(WAIT, frames.recv()).await.unwrap().unwrap();
    assert!(matches!(first, TerminalRender::Snapshot(ref s) if s.frame == attached.frame));
    let _: Empty = client
        .call(Request::TerminalAck(TerminalAck {
            terminal_id: terminal,
            frame: attached.frame,
        }))
        .await
        .unwrap();
    client.send_input(terminal, b"ping\r").await.unwrap();
    loop {
        let render = timeout(WAIT, frames.recv()).await.unwrap().unwrap();
        if let TerminalRender::Frame(frame) = render {
            let _: Empty = client
                .call(Request::TerminalAck(TerminalAck {
                    terminal_id: terminal,
                    frame: frame.frame,
                }))
                .await
                .unwrap();
            if !frame.dirty.is_empty() {
                return;
            }
        }
    }
}

#[tokio::test]
async fn bridge_over_a_pipe_pair_carries_the_protocol() {
    let t = TestDaemon::start().await;
    let daemon = UnixStream::connect(&t.paths.socket).await.unwrap();
    // The client's stream is one end of an in-memory pipe pair; the bridge's standard input
    // and output are the other end.
    let (client_end, bridge_end) = tokio::io::duplex(4096);
    let (bridge_in, bridge_out) = tokio::io::split(bridge_end);
    let bridge = tokio::spawn(bridge(bridge_in, bridge_out, daemon));

    let Resumed { client, .. } = Client::connect_stream(client_end, client_info(), None)
        .await
        .unwrap();
    assert_eq!(client.epoch(), t.daemon.epoch());
    run_flows(&t, &client).await;

    // The client going away is end of input for the bridge, which ends and lets go of the daemon.
    drop(client);
    assert_eq!(
        timeout(WAIT, bridge).await.unwrap().unwrap().unwrap(),
        Ended::Input
    );
    t.stop().await;
}

#[tokio::test]
async fn the_proxy_process_carries_every_frame_kind_and_prints_only_protocol() {
    let t = TestDaemon::start().await;
    let mut command = command(std::path::Path::new(HUTAN), Some(&t.paths.socket), &["proxy"]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_child(&mut command);
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    // Any byte of the process's standard output that is not a frame would fail the handshake or
    // a later decode: the client reads that pipe directly.
    let Resumed { client, .. } =
        Client::connect_stream(
            Stdio2 {
                stdin: Some(stdin),
                stdout,
            },
            client_info(),
            None,
        )
            .await
            .unwrap();
    run_flows(&t, &client).await;

    // End of input ends the proxy cleanly, with nothing on standard error.
    drop(client);
    let output = timeout(WAIT, child.wait_with_output()).await.unwrap().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    t.stop().await;
}

#[tokio::test]
async fn the_proxy_process_ends_when_the_daemon_does() {
    let t = TestDaemon::start().await;
    let mut command = command(std::path::Path::new(HUTAN), Some(&t.paths.socket), &["proxy"]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_child(&mut command);
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let Resumed { client, .. } =
        Client::connect_stream(
            Stdio2 {
                stdin: Some(stdin),
                stdout,
            },
            client_info(),
            None,
        )
            .await
            .unwrap();
    let _ = &client;

    t.daemon.shutdown();
    // The client's stdin stays open: the daemon closing alone ends the proxy.
    let status = timeout(WAIT, child.wait()).await.unwrap().unwrap();
    assert_eq!(status.code(), Some(0));
    drop(client);
}
