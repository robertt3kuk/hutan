//! `session.recovery` (protocol 0.26) for terminals: a real daemon on a temporary `HUTAN_HOME`
//! is stopped and a new one started on the same directory; the facts it found are in
//! `SessionInfo.recovery`. Real shells run in real PTYs.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::path::Path;
use std::time::Duration;

use common::*;
use hutan_client::Client;
use hutan_core::{SessionId, SessionState};
use hutan_daemon::{DaemonConfig, Paths};
use hutan_protocol::ErrorCode;
use hutan_protocol::blocks::BlockState;
use hutan_protocol::messages::{
    AgentReportRequest, AgentState, Empty, Event, Request, SessionInfo, SessionRef, SessionResult,
    SessionSnapshot, TerminalSpec,
};
use hutan_protocol::recovery::{DaemonStop, Recovery, RecoveryAck, RecoveryAcked, RecoveryKind};
use hutan_storage::{SessionRow, Storage};
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

async fn snapshot(client: &Client) -> SessionSnapshot {
    client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap()
}

async fn session(client: &Client, id: SessionId) -> SessionInfo {
    snapshot(client)
        .await
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("session {id} missing"))
}

async fn wait_session(
    client: &Client,
    id: SessionId,
    what: &str,
    pred: impl Fn(&SessionInfo) -> bool,
) -> SessionInfo {
    timeout(WAIT, async {
        loop {
            let found = session(client, id).await;
            if pred(&found) {
                return found;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("session {id}: timed out waiting for {what}"))
}

fn recovery_of(session: &SessionInfo) -> &Recovery {
    session
        .recovery
        .as_ref()
        .unwrap_or_else(|| panic!("session {} has no recovery", session.id))
}

fn zsh_available() -> bool {
    Path::new("/bin/zsh").exists()
}

/// Stops the daemon (a clean shutdown) and returns what is needed to start another one on the
/// same home, so the test can change the database in between.
async fn stop_keeping_home(t: TestDaemon) -> (TempDir, Paths) {
    let TestDaemon {
        _dir,
        paths,
        daemon,
    } = t;
    daemon.shutdown();
    timeout(WAIT, daemon.wait()).await.unwrap().unwrap();
    (_dir, paths)
}

async fn start_on(dir: TempDir, paths: Paths) -> TestDaemon {
    let daemon = start_when_free(DaemonConfig::new(paths.clone())).await;
    TestDaemon {
        _dir: dir,
        paths,
        daemon,
    }
}

/// Rewrites the `end` of the newest recorded run, the way an older or a killed daemon left it.
fn set_last_run_end(paths: &Paths, end: Option<serde_json::Value>) {
    let storage = Storage::open(&paths.database).unwrap();
    let mut history: Vec<serde_json::Value> =
        serde_json::from_str(&storage.app_meta().get("epoch_history").unwrap().unwrap()).unwrap();
    let last = history.last_mut().unwrap().as_object_mut().unwrap();
    match end {
        Some(end) => {
            last.insert("end".into(), end);
        }
        None => {
            last.remove("end");
        }
    }
    storage
        .app_meta()
        .set("epoch_history", &serde_json::to_string(&history).unwrap())
        .unwrap();
    storage.close();
}

async fn eventually_stored(paths: &Paths, what: &str, check: impl Fn(&[SessionRow]) -> bool) {
    let outcome = timeout(WAIT, async {
        loop {
            let storage = Storage::open(&paths.database).unwrap();
            let rows = storage.sessions().list().unwrap();
            storage.close();
            if check(&rows) {
                return;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(outcome.is_ok(), "{what}: never reached the database");
}

async fn ack(client: &Client, session_id: Option<SessionId>) -> Vec<SessionId> {
    client
        .call::<RecoveryAcked>(Request::SessionRecoveryAck(RecoveryAck { session_id }))
        .await
        .unwrap()
        .sessions
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clean_restart_says_a_shell_was_respawned_and_what_it_was_running() {
    if !zsh_available() {
        eprintln!("skipping: zsh is not installed");
        return;
    }
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let work = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(work.path()).unwrap();
    let zsh_home = tempfile::tempdir().unwrap();
    let mut zsh = spec(&["/bin/zsh", "-i"]);
    zsh.cwd = Some(root.clone());
    zsh.size = Some(size(200, 10));
    zsh.env.insert(
        "HOME".into(),
        zsh_home.path().to_string_lossy().into_owned(),
    );
    let created = create(&client, zsh).await;
    let mut view = View::attach(&client, terminal_of(&created), None).await;
    view.wait_for("a prompt", |s| s.contains("%")).await;
    client
        .send_input(terminal_of(&created), b"sleep 31\r")
        .await
        .unwrap();
    wait_event(&mut events, "the command running", |event| match event {
        Event::TerminalBlock { block, .. }
            if block.state == BlockState::Running
                && block.command.as_deref() == Some("sleep 31") =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    // The daemon writes what runs in a terminal at most once a second.
    let paths = t.paths.clone();
    timeout(WAIT, async {
        loop {
            let storage = Storage::open(&paths.database).unwrap();
            let stored = storage.app_meta().get("recovery.running_commands").unwrap();
            storage.close();
            if stored.is_some_and(|json| json.contains("sleep 31")) {
                return;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the running command reaches the database");
    drop((view, client, events));

    let first_epoch = t.daemon.epoch();
    let t = t.restart().await;
    let client = t.connect().await;
    let after = session(&client, created.id).await;
    assert_eq!(after.state, SessionState::Running);
    let recovery = recovery_of(&after);
    assert_eq!(
        recovery.kind,
        RecoveryKind::ShellRespawned {
            cwd: Some(root),
            was_running: Some("sleep 31".to_owned()),
        }
    );
    assert_eq!(recovery.previous_epoch, Some(first_epoch));
    assert!(
        matches!(recovery.daemon_stop, DaemonStop::Clean { at_ms } if at_ms <= recovery.restarted_at_ms),
        "{:?}",
        recovery.daemon_stop
    );
    assert!(recovery.lost.is_empty());

    // The new shell runs nothing: a later restart does not claim the old command again.
    let t = t.restart().await;
    let client = t.connect().await;
    let again = session(&client, created.id).await;
    assert!(
        matches!(
            &recovery_of(&again).kind,
            RecoveryKind::ShellRespawned {
                was_running: None,
                ..
            }
        ),
        "{:?}",
        again.recovery
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_say_what_became_of_them_and_old_endings_stay_quiet() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let live = create(&client, spec(&["/bin/cat"])).await;
    let ended = create(&client, sh("exit 3")).await;
    wait_session(&client, ended.id, "the exit", |s| {
        matches!(s.state, SessionState::Exited { .. })
    })
    .await;
    let stopped = create(&client, spec(&["/bin/cat"])).await;
    stop(&client, stopped.id).await.unwrap();
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    let live2 = session(&client, live.id).await;
    assert_eq!(live2.state, SessionState::Running);
    assert!(matches!(
        recovery_of(&live2).kind,
        RecoveryKind::ShellRespawned { .. }
    ));
    // Ended during the run that just ended: listed for history only.
    for id in [ended.id, stopped.id] {
        let old = session(&client, id).await;
        assert!(old.restored);
        assert_eq!(recovery_of(&old).kind, RecoveryKind::EndedBefore, "{id}");
    }

    // Ended before that run: only `restored`, nothing to say about this restart.
    let t = t.restart().await;
    let client = t.connect().await;
    assert!(matches!(
        recovery_of(&session(&client, live.id).await).kind,
        RecoveryKind::ShellRespawned { .. }
    ));
    for id in [ended.id, stopped.id] {
        let old = session(&client, id).await;
        assert!(old.restored);
        assert_eq!(old.recovery, None, "{id}");
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_stopped_mode_lists_what_it_did_not_start() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let live = create(&client, spec(&["/bin/cat"])).await;
    drop(client);
    std::fs::write(
        &t.paths.config_file,
        "[daemon]\nrestore_sessions = \"stopped\"\n",
    )
    .unwrap();
    let t = t.restart().await;
    let client = t.connect().await;
    let after = session(&client, live.id).await;
    assert_eq!(after.state, SessionState::Stopped);
    let RecoveryKind::NotRestarted { reason } = &recovery_of(&after).kind else {
        panic!("{:?}", after.recovery);
    };
    assert!(reason.contains("restore_sessions"), "{reason}");

    // The user restarts it: nothing is left to say.
    let restarted = client
        .call::<SessionResult>(Request::SessionRestart(SessionRef {
            session_id: live.id,
        }))
        .await
        .unwrap()
        .session;
    assert_eq!(restarted.recovery, None);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_program_or_an_unusable_row_is_not_restarted_with_the_reason() {
    let t = TestDaemon::start().await;
    let (dir, paths) = stop_keeping_home(t).await;
    let storage = Storage::open(&paths.database).unwrap();
    let home = storage
        .workspaces()
        .list()
        .unwrap()
        .into_iter()
        .find(|w| w.kind == hutan_storage::WorkspaceKind::Home)
        .unwrap()
        .id;
    let row = |title: &str, spec_json: String| SessionRow {
        id: SessionId::new(),
        workspace_id: home,
        kind: hutan_core::SessionKind::Terminal,
        title: title.to_owned(),
        title_locked: true,
        spec_json,
        cwd: None,
        state: SessionState::Running,
        created_at_ms: 1_000,
        ended_at_ms: None,
        resume_ref_json: None,
    };
    let missing = row(
        "missing",
        serde_json::to_string(&TerminalSpec {
            command: vec!["/definitely/not/a/binary".into()],
            ..TerminalSpec::default()
        })
        .unwrap(),
    );
    let broken = row("broken", "this is not json".to_owned());
    for r in [&missing, &broken] {
        storage.sessions().upsert(r.clone()).unwrap();
    }
    storage.close();

    let t = start_on(dir, paths).await;
    let client = t.connect().await;
    let missing2 = session(&client, missing.id).await;
    assert!(matches!(missing2.state, SessionState::Failed { .. }));
    let RecoveryKind::NotRestarted { reason } = &recovery_of(&missing2).kind else {
        panic!("{:?}", missing2.recovery);
    };
    assert!(reason.contains("/definitely/not/a/binary"), "{reason}");
    let broken2 = session(&client, broken.id).await;
    let RecoveryKind::NotRestarted { reason } = &recovery_of(&broken2).kind else {
        panic!("{:?}", broken2.recovery);
    };
    assert!(reason.contains("unreadable"), "{reason}");
    t.stop().await;
}

/// A fake agent on `PATH`: records its argv, then waits.
fn install_fake_agent(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("fake-agent");
    std::fs::write(
        &script,
        "#!/bin/sh\n{ for a in \"$@\"; do echo \"$a\"; done; } > \"$FAKE_AGENT_OUT\"\nexec sleep 600\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_agent_with_a_resume_command_comes_back_as_resumed() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let bin = TempDir::new().unwrap();
    install_fake_agent(bin.path());
    let out_dir = TempDir::new().unwrap();
    let out = out_dir.path().join("argv");
    let mut agent_spec = sh("exec sleep 600");
    agent_spec.env.insert(
        "PATH".into(),
        format!(
            "{}:{}",
            bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    agent_spec
        .env
        .insert("FAKE_AGENT_OUT".into(), out.to_string_lossy().into_owned());
    let created = create(&client, agent_spec).await;
    let argv = vec![
        "fake-agent".to_owned(),
        "--resume".to_owned(),
        "conv 42".to_owned(),
    ];
    client
        .call::<hutan_protocol::messages::AgentApplied>(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: created.id,
            source: "fake-hooks".into(),
            agent: "fake".into(),
            state: Some(AgentState::Working),
            message: None,
            seq: 5,
            agent_session_id: Some("conv 42".into()),
            resume_argv: Some(argv.clone()),
            node: None,
        }))
        .await
        .unwrap();
    eventually_stored(&t.paths, "the resume reference", |rows| {
        rows.iter().any(|r| r.resume_ref_json.is_some())
    })
    .await;
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    let after = wait_session(&client, created.id, "the respawn", |s| {
        s.state == SessionState::Running
    })
    .await;
    assert_eq!(
        recovery_of(&after).kind,
        RecoveryKind::AgentResumed { resume_argv: argv }
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_respawned_session_that_exits_by_itself_is_no_longer_said_to_run() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    // Lives long enough to be stored as live, then ends right after the restart.
    let marker_dir = TempDir::new().unwrap();
    let marker = marker_dir.path().join("second-run");
    let created = create(
        &client,
        sh(&format!(
            "if [ -e '{0}' ]; then exit 7; else touch '{0}'; exec sleep 600; fi",
            marker.display()
        )),
    )
    .await;
    wait_for_file(&marker).await;
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    let after = wait_session(&client, created.id, "the exit", |s| {
        matches!(s.state, SessionState::Exited { .. })
    })
    .await;
    let RecoveryKind::NotRestarted { reason } = &recovery_of(&after).kind else {
        panic!("{:?}", after.recovery);
    };
    assert!(reason.contains("exited with code 7"), "{reason}");
    t.stop().await;
}

async fn wait_for_file(path: &Path) {
    timeout(WAIT, async {
        while !path.exists() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the marker file");
}

#[tokio::test(flavor = "multi_thread")]
async fn how_the_previous_daemon_ended_is_told_only_as_far_as_it_is_known() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let live = create(&client, spec(&["/bin/cat"])).await;
    drop(client);

    // Clean: the stop was recorded.
    let (dir, paths) = stop_keeping_home(t).await;
    let t = start_on(dir, paths).await;
    let client = t.connect().await;
    assert!(matches!(
        recovery_of(&session(&client, live.id).await).daemon_stop,
        DaemonStop::Clean { .. }
    ));
    drop(client);

    // Crashed: a run that never recorded its stop (killed, or the power went).
    let (dir, paths) = stop_keeping_home(t).await;
    set_last_run_end(&paths, Some(serde_json::json!({"state": "open"})));
    let t = start_on(dir, paths).await;
    let client = t.connect().await;
    let crashed = session(&client, live.id).await;
    assert_eq!(recovery_of(&crashed).daemon_stop, DaemonStop::Crashed);
    assert!(
        matches!(
            recovery_of(&crashed).kind,
            RecoveryKind::ShellRespawned { .. }
        ),
        "a crash is no reason to say less: the session is still respawned"
    );
    drop(client);

    // Unknown: the record comes from a build that did not track it.
    let (dir, paths) = stop_keeping_home(t).await;
    set_last_run_end(&paths, None);
    let t = start_on(dir, paths).await;
    let client = t.connect().await;
    assert_eq!(
        recovery_of(&session(&client, live.id).await).daemon_stop,
        DaemonStop::Unknown
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn acknowledging_clears_one_session_or_all_and_tells_the_clients() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let a = create(&client, spec(&["/bin/cat"])).await;
    let b = create(&client, spec(&["/bin/cat"])).await;
    let c = create(&client, spec(&["/bin/cat"])).await;
    drop(client);

    let t = t.restart().await;
    let (client, mut events) = t.connect_subscribed().await;
    for id in [a.id, b.id, c.id] {
        recovery_of(&session(&client, id).await);
    }

    assert_eq!(ack(&client, Some(a.id)).await, [a.id]);
    wait_event(
        &mut events,
        "the update of the acknowledged session",
        |e| match e {
            Event::SessionUpdated { session }
                if session.id == a.id && session.recovery.is_none() =>
            {
                Some(())
            }
            _ => None,
        },
    )
    .await;
    assert_eq!(session(&client, a.id).await.recovery, None);
    assert!(session(&client, b.id).await.recovery.is_some());
    // Nothing to clear: not an error, and nothing is listed.
    assert!(ack(&client, Some(a.id)).await.is_empty());
    assert_eq!(
        remote_code(
            client
                .call::<RecoveryAcked>(Request::SessionRecoveryAck(RecoveryAck {
                    session_id: Some(SessionId::new()),
                }))
                .await
        ),
        ErrorCode::NotFound
    );

    let mut all = ack(&client, None).await;
    all.sort();
    let mut expected = vec![b.id, c.id];
    expected.sort();
    assert_eq!(all, expected);
    assert!(
        snapshot(&client)
            .await
            .sessions
            .iter()
            .all(|s| s.recovery.is_none())
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_next_user_action_in_a_session_clears_what_the_restart_did() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let typed = create(&client, spec(&["/bin/cat"])).await;
    let stopped = create(&client, spec(&["/bin/cat"])).await;
    let untouched = create(&client, spec(&["/bin/cat"])).await;
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    // Output of the new process (a prompt, a banner) is not the user's action.
    let typed2 = session(&client, typed.id).await;
    let mut view = View::attach(&client, terminal_of(&typed2), None).await;
    sleep(Duration::from_millis(300)).await;
    assert!(session(&client, typed.id).await.recovery.is_some());

    client
        .send_input(terminal_of(&typed2), b"hello\r")
        .await
        .unwrap();
    wait_session(&client, typed.id, "the recovery cleared by typing", |s| {
        s.recovery.is_none()
    })
    .await;
    stop(&client, stopped.id).await.unwrap();
    assert_eq!(session(&client, stopped.id).await.recovery, None);
    assert!(session(&client, untouched.id).await.recovery.is_some());
    view.wait_for("the echo", |s| s.contains("hello")).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_restart_reports_the_second_restart_not_the_first() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let live = create(&client, spec(&["/bin/cat"])).await;
    drop(client);
    let first = t.daemon.epoch();
    let t = t.restart().await;
    let second = t.daemon.epoch();
    let client = t.connect().await;
    assert_eq!(
        recovery_of(&session(&client, live.id).await).previous_epoch,
        Some(first)
    );
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    let again = session(&client, live.id).await;
    assert_eq!(recovery_of(&again).previous_epoch, Some(second));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hutand_that_was_killed_is_told_from_one_that_was_stopped() {
    use std::process::{Command, Stdio};

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let paths = Paths::in_home(&home);
    // Stops the process below if the test fails before it is killed (dropped once it is).
    let reaper = hutan_daemon::testing::DaemonReaper::new(&home);
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_hutand"))
        .env("HUTAN_HOME", &home)
        .env_remove("HUTAN_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let options = hutan_client::ConnectOptions {
        socket: Some(paths.socket.clone()),
        autostart: None,
        client: hutan_protocol::messages::ClientInfo {
            kind: hutan_protocol::messages::ClientKind::Cli,
            version: "test".into(),
            origin: None,
        },
    };
    let client = timeout(WAIT, async {
        loop {
            match Client::connect(options.clone()).await {
                Ok(client) => return client,
                Err(_) => sleep(Duration::from_millis(25)).await,
            }
        }
    })
    .await
    .expect("hutand started");
    let created = create(&client, spec(&["/bin/cat"])).await;
    eventually_stored(&paths, "the session", |rows| {
        rows.iter().any(|r| r.id == created.id)
    })
    .await;
    drop(client);

    // SIGKILL: no shutdown path runs, so no stop is noted.
    let killed = Command::new("kill")
        .args(["-KILL", &daemon.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    timeout(WAIT, async {
        loop {
            if daemon.try_wait().unwrap().is_some() {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("hutand died");
    // The home's next daemon runs in this process.
    drop(reaper);

    let t = start_on(dir, paths).await;
    let client = t.connect().await;
    let after = wait_session(&client, created.id, "the respawn", |s| {
        s.state == SessionState::Running
    })
    .await;
    let recovery = recovery_of(&after);
    assert_eq!(recovery.daemon_stop, DaemonStop::Crashed);
    assert!(matches!(recovery.kind, RecoveryKind::ShellRespawned { .. }));
    assert!(recovery.previous_epoch.is_some());
    t.stop().await;
}
