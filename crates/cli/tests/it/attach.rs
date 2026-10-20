//! `hutan attach` (and `new --attach`) driven through a real pseudo-terminal: the test plays
//! the host terminal, a `vt100` parser shows what the user would see, and a real daemon runs
//! the sessions.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{HUTAN, TestDaemon, WAIT};
use hutan_client::{Client, ConnectOptions};
use hutan_protocol::messages::{
    ClientInfo, ClientKind, Empty, Event, Request, SessionRef, SessionResult, Subscribed,
};
use parking_lot::Mutex;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::time::sleep;

const CTRL_BACKSLASH: u8 = 0x1c;

/// `hutan` running on a pseudo-terminal, with the screen it draws kept by a vt100 parser.
struct Host {
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn std::io::Write + Send>,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Every byte the child wrote, for what the alternate screen no longer shows.
    raw: Arc<Mutex<Vec<u8>>>,
}

impl Host {
    fn spawn(socket: &Path, args: &[&str]) -> Self {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new(HUTAN);
        cmd.arg("--socket");
        cmd.arg(socket);
        cmd.args(args);
        cmd.env_remove("HUTAN_HOME");
        cmd.env_remove("HUTAN_SOCKET");
        cmd.env("TERM", "xterm-256color");
        let child = pty.slave.spawn_command(cmd).unwrap();
        drop(pty.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
        let mut reader = pty.master.try_clone_reader().unwrap();
        let sink = Arc::clone(&parser);
        let raw = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&raw);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            // Ends with EIO/EOF once the child closed its side.
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                record.lock().extend_from_slice(&buf[..n]);
                sink.lock().process(&buf[..n]);
            }
        });
        let writer = pty.master.take_writer().unwrap();
        Self {
            child,
            master: pty.master,
            writer,
            parser,
            raw,
        }
    }

    fn type_bytes(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
    }

    fn screen(&self) -> String {
        self.parser.lock().screen().contents()
    }

    /// Whether `text` appeared anywhere in the bytes written so far.
    fn output_contains(&self, text: &str) -> bool {
        let raw = self.raw.lock();
        raw.windows(text.len()).any(|w| w == text.as_bytes())
    }

    fn in_alternate_screen(&self) -> bool {
        self.parser.lock().screen().alternate_screen()
    }

    fn resize(&self, cols: u16, rows: u16) {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        self.parser.lock().screen_mut().set_size(rows, cols);
    }

    async fn wait_for(&self, what: &str, pred: impl Fn(&str) -> bool) {
        let start = Instant::now();
        loop {
            let screen = self.screen();
            if pred(&screen) {
                return;
            }
            assert!(
                start.elapsed() < WAIT,
                "timed out waiting for {what}; screen:\n{screen}"
            );
            sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_for_text(&self, text: &str) {
        self.wait_for(&format!("{text:?}"), |screen| screen.contains(text))
            .await;
    }

    /// Exit code once the process ended.
    async fn wait_exit(&mut self) -> u32 {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.exit_code();
            }
            assert!(
                start.elapsed() < WAIT,
                "hutan did not exit; screen:\n{}",
                self.screen()
            );
            sleep(Duration::from_millis(20)).await;
        }
    }

    fn signal(&self, name: &str) {
        let pid = self.child.process_id().unwrap();
        let status = std::process::Command::new("kill")
            .args([format!("-{name}"), pid.to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Text after the first `marker` on the screen, up to the end of the line.
    fn after(&self, marker: &str) -> Option<String> {
        self.screen().lines().find_map(|line| {
            line.find(marker)
                .map(|at| line[at + marker.len()..].trim().to_owned())
        })
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

async fn client(t: &TestDaemon) -> Client {
    Client::connect(ConnectOptions {
        socket: Some(t.paths.socket.clone()),
        autostart: None,
        client: ClientInfo {
            kind: ClientKind::Cli,
            version: "test".into(),
            origin: None,
        },
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_shows_the_session_forwards_typing_and_detaches() {
    let t = TestDaemon::start().await;
    let id = t
        .new_session(
            &[],
            &[
                "sh",
                "-c",
                "echo ready; read line; echo \"got:$line\"; exec cat",
            ],
        )
        .await;

    let mut host = Host::spawn(&t.paths.socket, &["attach", &id[..12]]);
    host.wait_for_text("ready").await;
    assert!(
        host.in_alternate_screen(),
        "attach owns the alternate screen"
    );

    host.type_bytes(b"abc\r");
    host.wait_for_text("got:abc").await;

    // ctrl+\ then d, in separate writes.
    host.type_bytes(&[CTRL_BACKSLASH]);
    sleep(Duration::from_millis(100)).await;
    host.type_bytes(b"d");
    assert_eq!(host.wait_exit().await, 0);
    assert!(!host.in_alternate_screen(), "the host screen is given back");
    host.wait_for_text(&format!("[detached from {id}]")).await;

    // The session lives on; a second attach (as a new client) gets the screen as it was.
    t.wait_state(&id, "running").await;
    let mut again = Host::spawn(&t.paths.socket, &["attach", &id]);
    again.wait_for_text("got:abc").await;
    again.type_bytes(b"more\r");
    again.wait_for_text("more").await;
    again.type_bytes(&[CTRL_BACKSLASH, b'd']);
    assert_eq!(again.wait_exit().await, 0);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_attached_clients_see_each_others_typing() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["cat"]).await;
    let mut first = Host::spawn(&t.paths.socket, &["attach", &id]);
    let mut second = Host::spawn(&t.paths.socket, &["attach", "t:1"]);
    // Both clients attached before anyone types.
    sleep(Duration::from_millis(300)).await;

    first.type_bytes(b"from-first\r");
    second.type_bytes(b"from-second\r");
    for host in [&first, &second] {
        host.wait_for_text("from-first").await;
        host.wait_for_text("from-second").await;
    }
    first.type_bytes(&[CTRL_BACKSLASH, b'd']);
    assert_eq!(first.wait_exit().await, 0);

    // The other client is unaffected by the detach.
    second.type_bytes(b"still-here\r");
    second.wait_for_text("still-here").await;
    second.type_bytes(&[CTRL_BACKSLASH, b'd']);
    assert_eq!(second.wait_exit().await, 0);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_literal_ctrl_backslash_is_sent_by_pressing_it_twice() {
    let t = TestDaemon::start().await;
    // `od` shows the exact bytes the session received.
    let id = t
        .new_session(
            &[],
            &[
                "sh",
                "-c",
                "stty raw -echo; echo ready; head -c 3 | od -An -tx1; sleep 30",
            ],
        )
        .await;
    let mut host = Host::spawn(&t.paths.socket, &["attach", &id]);
    host.wait_for_text("ready").await;
    host.type_bytes(&[CTRL_BACKSLASH, CTRL_BACKSLASH, b'x', b'y']);
    // od pads its columns differently per platform.
    host.wait_for("the bytes 1c 78 79", |screen| {
        screen
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains("1c 78 79")
    })
    .await;
    host.type_bytes(&[CTRL_BACKSLASH, b'd']);
    assert_eq!(host.wait_exit().await, 0);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_reports_the_exit_code_when_the_session_ends() {
    let t = TestDaemon::start().await;
    let id = t
        .new_session(&[], &["sh", "-c", "echo up; read x; exit 3"])
        .await;
    let mut host = Host::spawn(&t.paths.socket, &["attach", &id]);
    host.wait_for_text("up").await;
    host.type_bytes(b"\r");
    assert_eq!(host.wait_exit().await, 0);
    assert!(!host.in_alternate_screen());
    host.wait_for_text("[session exited with code 3]").await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_leaves_when_the_session_is_stopped_from_elsewhere() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["cat"]).await;
    let mut host = Host::spawn(&t.paths.socket, &["attach", &id]);
    sleep(Duration::from_millis(300)).await;
    t.hutan(&["stop", &id]).await.ok();
    assert_eq!(host.wait_exit().await, 0);
    host.wait_for_text("[session stopped]").await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attaching_to_an_ended_session_shows_the_final_screen() {
    let t = TestDaemon::start().await;
    let id = t
        .new_session(&[], &["sh", "-c", "echo final-output; exit 3"])
        .await;
    t.wait_state(&id, "exited").await;
    let mut host = Host::spawn(&t.paths.socket, &["attach", &id]);
    assert_eq!(host.wait_exit().await, 0);
    assert!(!host.in_alternate_screen());
    host.wait_for_text("[session exited with code 3]").await;
    host.wait_for_text("hutan restart").await;
    host.wait_for_text("hutan history").await;
    // The alternate screen was entered and left: the final output must have been drawn on it,
    // so it is in the byte stream even though the main screen no longer shows it.
    assert!(
        host.output_contains("final-output"),
        "final screen never drawn"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn new_attach_creates_and_attaches_and_reports_a_fast_exit() {
    let t = TestDaemon::start().await;
    let mut host = Host::spawn(
        &t.paths.socket,
        &[
            "new",
            "--attach",
            "--",
            "sh",
            "-c",
            "echo hello-from-new; read x; exit 4",
        ],
    );
    host.wait_for_text("hello-from-new").await;
    assert!(host.in_alternate_screen());
    host.type_bytes(b"\r");
    assert_eq!(host.wait_exit().await, 0);
    host.wait_for_text("[session exited with code 4]").await;
    // The line printed before the screen was taken over is back on the host screen.
    host.wait_for_text("terminal 1").await;

    // A command that is gone before the attach starts still ends with its status line.
    let mut quick = Host::spawn(
        &t.paths.socket,
        &["new", "--attach", "--", "sh", "-c", "exit 5"],
    );
    assert_eq!(quick.wait_exit().await, 0);
    quick.wait_for_text("[session exited with code 5]").await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn host_resizes_reach_the_session() {
    let t = TestDaemon::start().await;
    let id = t
        .new_session(&[], &["sh", "-c", "while read x; do stty size; done"])
        .await;
    let mut host = Host::spawn(&t.paths.socket, &["attach", &id]);
    sleep(Duration::from_millis(300)).await;

    host.type_bytes(b"\r");
    host.wait_for_text("24 80").await;

    host.resize(100, 30);
    // Wait for SIGWINCH to land, then ask the session again.
    let start = Instant::now();
    loop {
        host.type_bytes(b"\r");
        sleep(Duration::from_millis(100)).await;
        if host.screen().contains("30 100") {
            break;
        }
        assert!(start.elapsed() < WAIT, "screen:\n{}", host.screen());
    }
    host.type_bytes(&[CTRL_BACKSLASH, b'd']);
    assert_eq!(host.wait_exit().await, 0);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn termination_signals_restore_the_terminal() {
    let t = TestDaemon::start().await;
    let id = t
        .new_session(&[], &["sh", "-c", "echo shown; exec cat"])
        .await;
    for (signal, code) in [("TERM", 143), ("HUP", 129)] {
        let mut host = Host::spawn(&t.paths.socket, &["attach", &id]);
        host.wait_for_text("shown").await;
        assert!(host.in_alternate_screen());
        host.signal(signal);
        assert_eq!(host.wait_exit().await, code, "SIG{signal}");
        assert!(
            !host.in_alternate_screen(),
            "SIG{signal} left the alternate screen"
        );
    }
    // Neither killed the session.
    t.wait_state(&id, "running").await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_follows_a_restart_to_the_new_terminal() {
    let t = TestDaemon::start().await;
    let id = t
        .new_session(&[], &["sh", "-c", "echo run-$$; read x"])
        .await;
    let c = client(&t).await;
    let mut events = c.events();
    let _: Subscribed = c.call(Request::EventsSubscribe(Empty {})).await.unwrap();

    let mut host = Host::spawn(&t.paths.socket, &["attach", &id]);
    host.wait_for_text("run-").await;
    let first = host.after("run-").unwrap();

    // Let the session end, then restart it at once: within the attach's grace period.
    host.type_bytes(b"\r");
    loop {
        let (_, event) = tokio::time::timeout(WAIT, events.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(&event, Event::SessionExited { .. }) {
            break;
        }
    }
    let restarted: SessionResult = c
        .call(Request::SessionRestart(SessionRef {
            session_id: id.parse().unwrap(),
        }))
        .await
        .unwrap();
    assert!(restarted.session.state.is_live());

    host.wait_for("output of the restarted process", |screen| {
        screen.contains("run-") && !screen.contains(&format!("run-{first}"))
    })
    .await;
    assert!(
        host.in_alternate_screen(),
        "still attached after the restart"
    );
    host.type_bytes(&[CTRL_BACKSLASH, b'd']);
    assert_eq!(host.wait_exit().await, 0);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn losing_the_daemon_ends_attach_with_exit_3() {
    let bin_dir = common::hutand_dir();
    let home = tempfile::tempdir().unwrap();
    let socket = home.path().join("hutand.sock");
    let run = |args: &[&str]| {
        let mut cmd = std::process::Command::new(bin_dir.join("hutan"));
        cmd.args(args)
            .env("HUTAN_HOME", home.path())
            .env_remove("HUTAN_SOCKET");
        cmd.output().unwrap()
    };
    let started = run(&["daemon", "start", "--json"]);
    assert!(started.status.success(), "{started:?}");
    let pid = serde_json::from_slice::<serde_json::Value>(&started.stdout).unwrap()["pid"]
        .as_u64()
        .unwrap();
    let created = run(&["new", "--json", "--", "sh", "-c", "echo alive; exec cat"]);
    assert!(created.status.success(), "{created:?}");
    let id = serde_json::from_slice::<serde_json::Value>(&created.stdout).unwrap()["session"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let mut host = Host::spawn(&socket, &["attach", &id]);
    host.wait_for_text("alive").await;
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(host.wait_exit().await, 3);
    assert!(!host.in_alternate_screen());
    host.wait_for_text("lost the connection to hutand").await;
}
