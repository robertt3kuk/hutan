//! The daemon's lifecycle as a connection keeps it (`docs/architecture.md` § Daemon lifecycle):
//! which build answers (protocol 0.42), autostarts that keep failing stop after three, and a
//! stop someone asked for is kept until the user starts the daemon again. Real `hutand`
//! processes on temporary homes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::{WAIT, wait_connected, wait_state};

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_client::identity::{OWN_VERSION, own_git_sha};
use hutan_client::{AUTOSTART_GIVE_UP, Client, ConnectOptions, Connection, ConnectionState};
use hutan_daemon::testing::DaemonReaper;
use hutan_protocol::messages::{ClientInfo, ClientKind, DaemonShutdown, Empty, Request, StopBy};
use tokio::time::{sleep, timeout};

fn hutand() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .and_then(|deps| deps.parent())
        .unwrap()
        .join("hutand")
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn options(socket: &Path, autostart: PathBuf, kind: ClientKind) -> ConnectOptions {
    ConnectOptions {
        socket: Some(socket.to_owned()),
        autostart: Some(autostart),
        client: ClientInfo {
            kind,
            version: "test".into(),
            origin: None,
        },
    }
}

/// A home with `hutand` pinned to it by a wrapper that notes every start in `starts`.
struct Home {
    // Dropped first (fields drop in order): stops every daemon of the home before it goes.
    _reaper: DaemonReaper,
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        script(
            dir.path(),
            "hutand-wrapper.sh",
            &format!(
                "echo start >> '{}'\nexport HUTAN_HOME='{}'\nexec '{}'",
                dir.path().join("starts").display(),
                home.display(),
                hutand().display()
            ),
        );
        Self {
            _reaper: DaemonReaper::new(&home),
            dir,
        }
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("home/hutand.sock")
    }

    fn options(&self, kind: ClientKind) -> ConnectOptions {
        options(
            &self.socket(),
            self.dir.path().join("hutand-wrapper.sh"),
            kind,
        )
    }

    /// How many times the wrapper started `hutand`.
    fn starts(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("starts"))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }
}

fn attempt(state: &hutan_client::StoreState) -> u32 {
    match state.connection {
        ConnectionState::Reconnecting { attempt, .. } => attempt,
        _ => 0,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_state_names_the_build_that_answers() {
    let home = Home::new();
    let connection = Connection::start(home.options(ClientKind::Desktop));
    let mut state = connection.state();
    let connected = wait_connected(&mut state).await;
    let daemon = connected.daemon.clone().expect("the daemon's identity");
    assert_eq!(daemon.version, OWN_VERSION);
    assert_eq!(daemon.git_sha.as_deref(), own_git_sha());
    assert_eq!(daemon.exe_path, Some(hutand().canonicalize().unwrap()));
    assert!(daemon.started_at_ms.is_some());
    // The build this client starts is that one.
    let own = hutan_client::DaemonIdentity::own(Some(&hutand()));
    assert_eq!(hutan_client::identity::compare(&own, &daemon), None);
    connection
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown::default()))
        .await
        .unwrap();
    connection.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn autostarts_that_fail_the_same_way_stop_after_three_until_retry() {
    let dir = tempfile::tempdir().unwrap();
    let starts = dir.path().join("starts");
    let broken = script(
        dir.path(),
        "hutand",
        &format!(
            "echo start >> '{}'\necho 'hutand: cannot open the frobnicator' >&2\nexit 3",
            starts.display()
        ),
    );
    let count = || {
        std::fs::read_to_string(&starts)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    };
    let connection = Connection::start(options(
        &dir.path().join("hutand.sock"),
        broken,
        ClientKind::Desktop,
    ));
    let mut state = connection.state();

    let halted = wait_state(&mut state, "autostart to give up", |s| s.autostart_halted).await;
    assert_eq!(count(), AUTOSTART_GIVE_UP as usize);
    let failure = halted
        .daemon_start
        .clone()
        .expect("the failure stays shown");
    assert_eq!(
        failure.sentence(),
        "hutand could not start (exit code 3): hutand: cannot open the frobnicator"
    );
    // The connection keeps dialing, quietly: more attempts, past the 4 s the next start would
    // otherwise wait, start nothing.
    let from = attempt(&halted);
    wait_state(&mut state, "two more dials", |s| attempt(s) >= from + 2).await;
    assert_eq!(count(), AUTOSTART_GIVE_UP as usize);
    assert!(state.borrow().autostart_halted);

    // Retry starts it again at once.
    connection.start_daemon_now();
    wait_state(&mut state, "the retry", |s| !s.autostart_halted).await;
    timeout(WAIT, async {
        while count() <= AUTOSTART_GIVE_UP as usize {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Retry started hutand");
    connection.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_the_user_asked_for_is_kept_until_they_start_it() {
    let home = Home::new();
    let connection = Connection::start(home.options(ClientKind::Desktop));
    let mut state = connection.state();
    let first = wait_connected(&mut state).await.epoch;
    assert_eq!(home.starts(), 1);

    // `hutan daemon stop` from another client.
    let cli = Client::connect(home.options(ClientKind::Cli))
        .await
        .unwrap();
    cli.call::<Empty>(Request::DaemonShutdown(DaemonShutdown::default()))
        .await
        .unwrap();
    let stopped = wait_state(&mut state, "the announced stop", |s| {
        s.stopped_by == Some(StopBy::Cli)
    })
    .await;
    assert!(!connection.autostart(), "autostart is off");
    // It keeps dialing and starts nothing.
    let from = attempt(&stopped);
    wait_state(&mut state, "three more dials", |s| attempt(s) >= from + 3).await;
    assert_eq!(home.starts(), 1);
    assert!(!home.socket().exists());
    assert_eq!(state.borrow().stopped_by, Some(StopBy::Cli));

    // Start.
    connection.start_daemon_now();
    let again = wait_state(&mut state, "a new daemon", |s| {
        s.connection == ConnectionState::Connected && s.epoch != first
    })
    .await;
    assert_eq!(again.stopped_by, None);
    assert_eq!(home.starts(), 2);

    // A restart is no stop: the connection brings a daemon back by itself.
    let cli = Client::connect(home.options(ClientKind::Cli))
        .await
        .unwrap();
    cli.call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
        restart: true,
        ..DaemonShutdown::default()
    }))
    .await
    .unwrap();
    let restarted = wait_state(&mut state, "the daemon after the restart", |s| {
        s.connection == ConnectionState::Connected && s.epoch != again.epoch
    })
    .await;
    assert_eq!(restarted.stopped_by, None);
    assert!(connection.autostart());
    connection
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown::default()))
        .await
        .unwrap();
    connection.shutdown();
}
