//! `Endpoint::Ssh` (slice R-2, ADR 0022): the client reaches a daemon through the system `ssh`
//! running `hutan proxy`. No sshd runs in the gate: a fake `ssh` script stands in, records the
//! arguments it was given and execs the real `hutan proxy` against the test daemon's socket.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_client::{
    Client, ClientError, ConnectOptions, Connection, ConnectionState, Endpoint, Resumed, SshHost,
};
use hutan_core::Seq;
use hutan_protocol::messages::{
    ClientInfo, ClientKind, DaemonStatusInfo, Empty, Event, Request, ResumeOutcome, ResumePoint,
    Subscribed, WorkspaceCreate, WorkspaceInfo,
};
use hutan_client::StoreState;
use tempfile::TempDir;
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

use crate::common::*;

pub fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Cli,
        version: "ssh-test".into(),
        origin: None,
    }
}

/// Waits for a state satisfying `pred`.
async fn wait_state(
    rx: &mut watch::Receiver<std::sync::Arc<StoreState>>,
    what: &str,
    pred: impl Fn(&StoreState) -> bool,
) {
    let found = timeout(WAIT, async {
        loop {
            if pred(&rx.borrow_and_update()) {
                return;
            }
            rx.changed().await.expect("connection task ended");
        }
    })
    .await;
    assert!(found.is_ok(), "timed out waiting for {what}: {:?}", *rx.borrow());
}

async fn wait_connected(rx: &mut watch::Receiver<std::sync::Arc<StoreState>>) {
    wait_state(rx, "connected", |s| s.connection == ConnectionState::Connected).await;
}

/// A fake `ssh` in a temporary directory.
pub struct FakeSsh {
    pub dir: TempDir,
    pub script: PathBuf,
}

impl FakeSsh {
    /// Records its arguments (one per line, an `@@end@@` line after each run) and its pid, then runs
    /// `hutan proxy` against `socket` in its place, as `ssh host hutan proxy` would on the host.
    pub fn proxying(socket: &Path) -> Self {
        Self::with_body(&format!(
            "exec '{}' --socket '{}' proxy\n",
            HUTAN,
            socket.display()
        ))
    }

    /// Records, then runs `body`.
    pub fn with_body(body: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("ssh");
        let text = format!(
            "#!/bin/sh\nd='{}'\nprintf '%s\\n' \"$@\" '@@end@@' >> \"$d/args\"\necho $$ >> \"$d/pids\"\n{body}",
            dir.path().display()
        );
        std::fs::write(&script, text).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, script }
    }

    /// The host profile that runs this script as its `ssh`.
    pub fn host(&self, hutan: Option<&str>) -> SshHost {
        SshHost::new("fake", "me@fake.example", hutan.map(str::to_owned))
            .unwrap()
            .with_program(&self.script)
    }

    pub fn options(&self) -> ConnectOptions {
        ConnectOptions {
            endpoint: Some(Endpoint::Ssh(self.host(None))),
            autostart: None,
            client: client_info(),
        }
    }

    /// The arguments of every run, one list per run.
    pub fn runs(&self) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(self.dir.path().join("args")).unwrap_or_default();
        let mut runs = vec![Vec::new()];
        for line in text.lines() {
            if line == "@@end@@" {
                runs.push(Vec::new());
            } else if let Some(run) = runs.last_mut() {
                run.push(line.to_owned());
            }
        }
        runs.pop();
        runs
    }

    pub fn pids(&self) -> Vec<u32> {
        std::fs::read_to_string(self.dir.path().join("pids"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }
}

#[tokio::test]
async fn the_client_speaks_the_protocol_through_ssh_running_the_proxy() {
    let t = TestDaemon::start().await;
    let ssh = FakeSsh::proxying(&t.paths.socket);
    let host = SshHost::new("fake", "me@fake.example", Some("~/bin/my hutan".into()))
        .unwrap()
        .with_program(&ssh.script);
    let client = Client::connect(ConnectOptions {
        endpoint: Some(Endpoint::Ssh(host)),
        autostart: None,
        client: client_info(),
    })
    .await
    .unwrap();
    let status: DaemonStatusInfo = client.call(Request::DaemonStatus(Empty {})).await.unwrap();
    assert_eq!(status.epoch, t.daemon.epoch());

    // What `ssh` was asked: no terminal, no prompt, the destination after `--`, and the remote
    // command as one word for the host's shell.
    let runs = ssh.runs();
    assert_eq!(runs.len(), 1, "{runs:?}");
    let args = &runs[0];
    assert!(args.contains(&"-T".to_owned()), "{args:?}");
    assert!(args.contains(&"BatchMode=yes".to_owned()), "{args:?}");
    let dashes = args.iter().position(|a| a == "--").unwrap();
    assert_eq!(&args[dashes + 1..], ["me@fake.example", "~/'bin/my hutan' proxy"]);

    drop(client);
    t.stop().await;
}

#[tokio::test]
async fn the_client_never_starts_a_daemon_for_an_ssh_endpoint() {
    // `ssh` that cannot reach the host: the error is the one `ssh` printed, and `autostart`
    // (which names a `hutand` to spawn here) is not used for a remote endpoint.
    let ssh = FakeSsh::with_body("echo 'ssh: connect to host fake.example port 22: Connection refused' >&2\nexit 255\n");
    let started = tempfile::tempdir().unwrap();
    let marker = started.path().join("started");
    let hutand = started.path().join("hutand");
    std::fs::write(
        &hutand,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hutand, std::fs::Permissions::from_mode(0o755)).unwrap();

    let error = Client::connect(ConnectOptions {
        autostart: Some(hutand),
        ..ssh.options()
    })
    .await
    .unwrap_err();
    let ClientError::Ssh(text) = &error else {
        panic!("{error:?}")
    };
    assert!(text.contains("fake"), "{text}");
    assert!(text.contains("Connection refused"), "{text}");
    assert!(!marker.exists(), "a local hutand was started");
}

#[tokio::test]
async fn text_printed_by_the_hosts_shell_is_named_in_the_error() {
    let ssh = FakeSsh::with_body("echo 'Last login: Mon Oct 26 on ttys001'\nexec sleep 5\n");
    let error = Client::connect(ssh.options()).await.unwrap_err();
    let text = error.to_string();
    assert!(text.contains("Last login"), "{text}");
    assert!(text.contains("startup file"), "{text}");
}

#[tokio::test]
async fn a_missing_ssh_program_is_said_plainly() {
    let host = SshHost::new("fake", "me@fake.example", None)
        .unwrap()
        .with_program("/nonexistent/ssh");
    let error = Client::connect(ConnectOptions {
        endpoint: Some(Endpoint::Ssh(host)),
        autostart: None,
        client: client_info(),
    })
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("cannot run `/nonexistent/ssh`"),
        "{error}"
    );
}

#[tokio::test]
async fn a_host_without_hutan_shows_what_its_shell_said() {
    // `ssh` ends without answering `hello`: whatever the host printed on stderr is shown.
    let ssh = FakeSsh::with_body("echo 'zsh:1: command not found: hutan' >&2\nexit 127\n");
    let error = Client::connect(ssh.options()).await.unwrap_err();
    assert!(error.to_string().contains("command not found: hutan"), "{error}");
}

#[tokio::test]
async fn resume_through_a_second_ssh_replays_what_was_missed() {
    let t = TestDaemon::start().await;
    let ssh = FakeSsh::proxying(&t.paths.socket);

    let first = Client::connect(ssh.options()).await.unwrap();
    first.call::<Subscribed>(Request::EventsSubscribe(Empty {})).await.unwrap();
    let resume = ResumePoint {
        epoch: first.epoch(),
        seq: first.last_seq(),
    };
    drop(first);

    let missed: WorkspaceInfo = {
        let other = hutan_client::Client::connect(ConnectOptions {
            endpoint: Some(Endpoint::Local(t.paths.socket.clone())),
            autostart: None,
            client: client_info(),
        })
        .await
        .unwrap();
        other
            .call(Request::WorkspaceCreate(WorkspaceCreate {
                name: "while-away".into(),
                paths: vec![],
                project_ids: Vec::new(),
                created_by: None,
            }))
            .await
            .unwrap()
    };

    let Resumed { client, mut events } =
        Client::connect_resuming(ssh.options(), Some(resume)).await.unwrap();
    assert_eq!(client.welcome().resume, ResumeOutcome::Replayed);
    let (seq, event) = timeout(WAIT, events.recv()).await.unwrap().unwrap();
    assert_eq!(seq, Seq(resume.seq.0 + 1));
    assert_eq!(event, Event::WorkspaceUpdated { workspace: missed });
    assert_eq!(ssh.runs().len(), 2);
    drop(client);
    t.stop().await;
}

#[tokio::test]
async fn the_connection_reconnects_when_ssh_exits() {
    let t = TestDaemon::start().await;
    let ssh = FakeSsh::proxying(&t.paths.socket);
    let connection = Connection::start(ssh.options());
    let mut state = connection.state();
    wait_connected(&mut state).await;
    let epoch = state.borrow().epoch;
    let first_pid = ssh.pids()[0];

    // The `ssh` process dies (a dropped link, a sleeping laptop).
    let killed = std::process::Command::new("kill")
        .arg(first_pid.to_string())
        .status()
        .unwrap();
    assert!(killed.success());

    // A second `ssh` is started, and the connection catches up with what happened meanwhile.
    timeout(WAIT, async {
        while ssh.pids().len() < 2 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("ssh was not started again");
    assert_ne!(ssh.pids()[1], first_pid);
    let id = t.new_session(&[], &["sh", "-c", "exec cat"]).await;
    wait_state(&mut state, "the new session through the second ssh", |s| {
        s.connection == ConnectionState::Connected
            && s.sessions.iter().any(|session| session.id.to_string() == id)
    })
    .await;
    assert_eq!(state.borrow().epoch, epoch, "the same daemon, resumed");

    connection.shutdown();
    t.stop().await;
}
