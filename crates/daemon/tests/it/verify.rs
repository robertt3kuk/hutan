//! `agents.verify` (protocol 0.70) end to end: a real daemon on a temporary `HUTAN_HOME`, terminal
//! sessions in scratch folders, and commands that are plain `sh`. Nothing here starts a harness or
//! calls a model. What the gate does with Mawas's request is in `mawas.rs`; the rules of detection
//! and of the output are unit tests of `verify`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, create, remote_code, wait_event};
use hutan_client::{Client, ClientError};
use hutan_core::SessionId;
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Event, Request, SessionInfo, TerminalSpec,
};
use hutan_protocol::verify::{
    AgentsVerifyCancel, AgentsVerifyPlanRequest, AgentsVerifyRequest, VERIFY_TAIL_BYTES,
    VerifyPlan, VerifyRefusal, VerifyRequester, VerifyRun, VerifySource, VerifyStatus,
};
use tokio::time::{sleep, timeout};

/// `[terminal] env_inherit = "daemon"`: no login shell to capture; the runs start from the
/// daemon's own environment (the login environment has its own test below).
const PLAIN: &str = "[terminal]\nenv_inherit = \"daemon\"\n";

/// A terminal session that sits in `dir` and does nothing.
async fn in_folder(client: &Client, dir: &Path) -> SessionInfo {
    create(
        client,
        TerminalSpec {
            command: vec!["sleep".to_owned(), "600".to_owned()],
            cwd: Some(dir.to_owned()),
            ..TerminalSpec::default()
        },
    )
    .await
}

async fn verify(
    client: &Client,
    session: SessionId,
    command: Option<&str>,
    force: bool,
) -> Result<VerifyRun, ClientError> {
    client
        .call(Request::AgentsVerify(AgentsVerifyRequest {
            session_id: session,
            command: command.map(str::to_owned),
            force,
        }))
        .await
}

async fn plan(
    client: &Client,
    session: SessionId,
    command: Option<&str>,
) -> Result<VerifyPlan, ClientError> {
    client
        .call(Request::AgentsVerifyPlan(AgentsVerifyPlanRequest {
            session_id: session,
            command: command.map(str::to_owned),
        }))
        .await
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_passing_run_keeps_its_exit_code_and_output_and_is_announced_twice() {
    let t = TestDaemon::start_with(PLAIN, |_| {}).await;
    let (client, mut events) = t.connect_subscribed().await;
    let work = tempfile::tempdir().unwrap();
    let session = in_folder(&client, work.path()).await;

    let run = verify(
        &client,
        session.id,
        Some("echo 'test result: ok. 12 passed'; pwd"),
        false,
    )
    .await
    .unwrap();
    assert_eq!(run.status, VerifyStatus::Passed);
    assert_eq!(run.exit_code, Some(0));
    assert_eq!(run.signal, None);
    assert_eq!(run.source, VerifySource::Caller);
    assert_eq!(run.requested_by, VerifyRequester::User);
    assert!(!run.forced && !run.output_truncated);
    assert!(
        run.output_tail.starts_with("test result: ok. 12 passed\n"),
        "{:?}",
        run.output_tail
    );
    // It ran in the session's folder.
    let printed = run.output_tail.lines().nth(1).unwrap();
    assert_eq!(canonical(Path::new(printed)), canonical(work.path()));
    assert!(run.ended_ms.unwrap() >= run.started_ms);
    assert_eq!(
        run.duration_ms,
        Some(run.ended_ms.unwrap() - run.started_ms)
    );

    // The start and the end were both events, the second with the whole run.
    let started = wait_event(&mut events, "agents.verify.started", |e| match e {
        Event::AgentsVerifyStarted { run } => Some((**run).clone()),
        _ => None,
    })
    .await;
    assert_eq!(
        (started.id, started.status),
        (run.id, VerifyStatus::Running)
    );
    assert!(started.output_tail.is_empty());
    let ended = wait_event(&mut events, "agents.verify.ended", |e| match e {
        Event::AgentsVerifyEnded { run } => Some((**run).clone()),
        _ => None,
    })
    .await;
    assert_eq!(ended, run);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_run_keeps_the_exit_code_and_what_both_streams_wrote() {
    let t = TestDaemon::start_with(PLAIN, |_| {}).await;
    let client = t.connect().await;
    let work = tempfile::tempdir().unwrap();
    let session = in_folder(&client, work.path()).await;

    let run = verify(
        &client,
        session.id,
        Some("echo 'out line'; echo 'test health ... FAILED' >&2; exit 3"),
        false,
    )
    .await
    .unwrap();
    assert_eq!(run.status, VerifyStatus::Failed);
    assert_eq!(run.exit_code, Some(3));
    assert!(
        run.output_tail.contains("out line"),
        "{:?}",
        run.output_tail
    );
    assert!(
        run.output_tail.contains("test health ... FAILED"),
        "{:?}",
        run.output_tail
    );
    // A command that does not exist is a failure with the shell's own code, not an error.
    let missing = verify(&client, session.id, Some("no-such-test-runner-xyz"), false)
        .await
        .unwrap();
    assert_eq!(missing.status, VerifyStatus::Failed);
    assert_eq!(missing.exit_code, Some(127));
    // A signal that was not ours.
    let killed = verify(&client, session.id, Some("kill -9 $$"), false)
        .await
        .unwrap();
    assert_eq!(killed.status, VerifyStatus::Failed);
    assert_eq!((killed.exit_code, killed.signal), (None, Some(9)));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_output_is_a_redacted_tail_of_at_most_eight_kib() {
    let t = TestDaemon::start_with(PLAIN, |_| {}).await;
    let client = t.connect().await;
    let work = tempfile::tempdir().unwrap();
    let session = in_folder(&client, work.path()).await;

    let script = "echo 'sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'; \
                  i=0; while [ $i -lt 3000 ]; do echo \"line $i of the test output\"; \
                  i=$((i+1)); done; printf '\\033[31mred\\033[0m end\\n'";
    let run = verify(&client, session.id, Some(script), false)
        .await
        .unwrap();
    assert_eq!(run.status, VerifyStatus::Passed);
    assert!(run.output_truncated);
    assert!(run.output_tail.len() <= VERIFY_TAIL_BYTES);
    assert!(
        run.output_tail
            .ends_with("line 2999 of the test output\nred end\n"),
        "the end is kept, without escapes: {:?}",
        &run.output_tail[run.output_tail.len() - 60..]
    );
    assert!(!run.output_tail.contains("line 0 "), "the beginning is cut");
    // The secret was in the cut part; one that is kept is redacted too.
    let secret = verify(
        &client,
        session.id,
        Some("echo 'token sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'"),
        false,
    )
    .await
    .unwrap();
    assert!(!secret.output_tail.contains("AAAAAAAA"), "{secret:?}");
    assert!(!secret.command.contains("AAAAAAAA"), "{secret:?}");
    t.stop().await;
}

/// Whether the process `pid` exists.
fn alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// The pid the command wrote to `pidfile`, once it has (the run's start event comes before the
/// shell has started its child).
async fn pid_in(pidfile: &Path) -> i32 {
    timeout(WAIT, async {
        loop {
            if let Some(pid) = std::fs::read_to_string(pidfile)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the command never wrote its child's pid")
}

async fn gone(pid: i32) {
    timeout(WAIT, async {
        while alive(pid) {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the process a timed-out run started is still running");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_past_its_timeout_is_killed_with_everything_it_started() {
    let t = TestDaemon::start_with(
        &format!("{PLAIN}\n[checks]\nverify_timeout_secs = 1\n"),
        |_| {},
    )
    .await;
    let client = t.connect().await;
    let work = tempfile::tempdir().unwrap();
    let session = in_folder(&client, work.path()).await;
    let pidfile = work.path().join("child.pid");

    let command = format!(
        "echo before; sleep 600 & echo $! > {pidfile:?}; wait",
        pidfile = pidfile
    );
    let run = verify(&client, session.id, Some(&command), false)
        .await
        .unwrap();
    assert_eq!(run.status, VerifyStatus::TimedOut);
    assert_eq!(run.exit_code, None);
    assert!(run.output_tail.contains("before"));
    assert!(run.duration_ms.unwrap() >= 1_000, "{run:?}");
    let child: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    gone(child).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_is_cancelled_by_the_user_and_only_one_runs_per_session() {
    let t = TestDaemon::start_with(PLAIN, |_| {}).await;
    let (client, mut events) = t.connect_subscribed().await;
    let work = tempfile::tempdir().unwrap();
    let session = in_folder(&client, work.path()).await;
    let other = in_folder(&client, work.path()).await;
    let pidfile = work.path().join("child.pid");

    // Nothing runs: nothing to cancel.
    assert_eq!(
        remote_code(
            client
                .call::<hutan_protocol::messages::Empty>(Request::AgentsVerifyCancel(
                    AgentsVerifyCancel {
                        session_id: session.id
                    }
                ))
                .await
        ),
        ErrorCode::NotFound
    );

    let running = tokio::spawn({
        let (client, id) = (t.connect().await, session.id);
        let command = format!("sleep 600 & echo $! > {pidfile:?}; wait");
        async move { verify(&client, id, Some(&command), false).await }
    });
    wait_event(&mut events, "the run to start", |e| match e {
        Event::AgentsVerifyStarted { run } if run.session_id == session.id => Some(()),
        _ => None,
    })
    .await;

    // The plan says it would be refused; a second run is refused; another session is not.
    assert_eq!(
        plan(&client, session.id, Some("true"))
            .await
            .unwrap()
            .refused,
        Some(VerifyRefusal::Running)
    );
    assert_eq!(
        remote_code(verify(&client, session.id, Some("true"), false).await),
        ErrorCode::InvalidState
    );
    assert_eq!(
        verify(&client, other.id, Some("true"), false)
            .await
            .unwrap()
            .status,
        VerifyStatus::Passed
    );

    let child = pid_in(&pidfile).await;
    client
        .call::<hutan_protocol::messages::Empty>(Request::AgentsVerifyCancel(AgentsVerifyCancel {
            session_id: session.id,
        }))
        .await
        .unwrap();
    let run = running.await.unwrap().unwrap();
    assert_eq!(run.status, VerifyStatus::Cancelled);
    gone(child).await;
    // The place is free again.
    assert_eq!(
        verify(&client, session.id, Some("true"), false)
            .await
            .unwrap()
            .status,
        VerifyStatus::Passed
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_caller_that_goes_away_takes_its_run_with_it() {
    let t = TestDaemon::start_with(PLAIN, |_| {}).await;
    let (client, mut events) = t.connect_subscribed().await;
    let work = tempfile::tempdir().unwrap();
    let session = in_folder(&client, work.path()).await;

    let caller = t.connect().await;
    let waiting = tokio::spawn({
        let id = session.id;
        async move { verify(&caller, id, Some("sleep 600"), false).await }
    });
    wait_event(&mut events, "the run to start", |e| match e {
        Event::AgentsVerifyStarted { .. } => Some(()),
        _ => None,
    })
    .await;
    waiting.abort();
    let _ = waiting.await;
    let ended = wait_event(&mut events, "the run to end", |e| match e {
        Event::AgentsVerifyEnded { run } => Some((**run).clone()),
        _ => None,
    })
    .await;
    assert_eq!(ended.status, VerifyStatus::Cancelled);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_is_refused_while_the_sessions_own_turn_runs_unless_the_user_forces_it() {
    let t = TestDaemon::start_with(PLAIN, |_| {}).await;
    let client = t.connect().await;
    let work = tempfile::tempdir().unwrap();
    let session = in_folder(&client, work.path()).await;
    let report = |state, seq| {
        client.call::<AgentApplied>(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: session.id,
            source: "fake-hook".into(),
            agent: "claude".into(),
            state: Some(state),
            message: None,
            seq,
            agent_session_id: None,
            resume_argv: None,
            node: None,
        }))
    };
    report(AgentState::Working, 1).await.unwrap();

    assert_eq!(
        plan(&client, session.id, Some("true"))
            .await
            .unwrap()
            .refused,
        Some(VerifyRefusal::TurnRunning)
    );
    let refused = verify(&client, session.id, Some("touch ran"), false)
        .await
        .unwrap_err();
    let ClientError::Remote(body) = refused else {
        panic!("expected a remote error, got {refused:?}");
    };
    assert_eq!(body.code, ErrorCode::InvalidState);
    assert_eq!(
        body.data.as_ref().unwrap()["verify"]["refused"],
        "turn_running"
    );
    assert!(!work.path().join("ran").exists(), "nothing ran");

    // The user may force it: the run says so.
    let forced = verify(&client, session.id, Some("touch ran"), true)
        .await
        .unwrap();
    assert_eq!(forced.status, VerifyStatus::Passed);
    assert!(forced.forced);
    assert!(work.path().join("ran").exists());

    // Once the turn is over a run is not forced.
    report(AgentState::Idle, 2).await.unwrap();
    let idle = verify(&client, session.id, Some("true"), false)
        .await
        .unwrap();
    assert!(!idle.forced);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_comes_from_the_project_then_the_setting_then_the_files() {
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("alpha");
    std::fs::create_dir_all(&project).unwrap();
    let toml = format!(
        "{PLAIN}\n[checks]\ntest_command = \"echo global\"\n\n\
         [checks.projects.alpha]\ntest_command = \"echo alpha\"\n"
    );
    let t = TestDaemon::start_with(&toml, |_| {}).await;
    let client = t.connect().await;
    let ws = client
        .call::<hutan_protocol::messages::WorkspaceInfo>(Request::WorkspaceCreate(
            hutan_protocol::messages::WorkspaceCreate {
                name: "alpha".into(),
                paths: vec![canonical(&project)],
                project_ids: Vec::new(),
                created_by: None,
            },
        ))
        .await
        .unwrap();
    let in_alpha = common::create_in(
        &client,
        Some(ws.id),
        TerminalSpec {
            command: vec!["sleep".to_owned(), "600".to_owned()],
            cwd: Some(canonical(&project)),
            ..TerminalSpec::default()
        },
    )
    .await;
    // The project's table wins; the caller's command wins over both.
    let planned = plan(&client, in_alpha.id, None).await.unwrap();
    assert_eq!(planned.command, "echo alpha");
    assert_eq!(planned.source, VerifySource::Project);
    assert_eq!(
        planned.why,
        "`[checks.projects.alpha] test_command` of daemon.toml"
    );
    assert_eq!(planned.cwd, canonical(&project));
    assert_eq!(planned.timeout_secs, 600);
    assert_eq!(planned.refused, None);
    let given = plan(&client, in_alpha.id, Some("make check"))
        .await
        .unwrap();
    assert_eq!(
        (given.command.as_str(), given.source),
        ("make check", VerifySource::Caller)
    );
    assert_eq!(
        verify(&client, in_alpha.id, None, false)
            .await
            .unwrap()
            .output_tail,
        "alpha\n"
    );

    // A session of another workspace gets the setting.
    let elsewhere = tempfile::tempdir().unwrap();
    let other = in_folder(&client, elsewhere.path()).await;
    let planned = plan(&client, other.id, None).await.unwrap();
    assert_eq!(
        (planned.command.as_str(), planned.source),
        ("echo global", VerifySource::Setting)
    );
    t.stop().await;

    // With neither, the files decide; with no files, the error says what was looked for.
    let t = TestDaemon::start_with(PLAIN, |_| {}).await;
    let client = t.connect().await;
    let folder = tempfile::tempdir().unwrap();
    let session = in_folder(&client, folder.path()).await;
    let error = plan(&client, session.id, None).await.unwrap_err();
    let ClientError::Remote(body) = error else {
        panic!("expected a remote error, got {error:?}");
    };
    assert_eq!(body.code, ErrorCode::InvalidState);
    assert!(
        body.message.contains("Cargo.toml") && body.message.contains("[checks]"),
        "{}",
        body.message
    );
    std::fs::write(folder.path().join("go.mod"), "module x\n").unwrap();
    let planned = plan(&client, session.id, None).await.unwrap();
    assert_eq!(
        (planned.command.as_str(), planned.source),
        ("go test ./...", VerifySource::Detected)
    );
    assert!(planned.why.starts_with("go.mod in "), "{}", planned.why);
    // The same files, another ecosystem, per folder: each one a fixture folder.
    for (files, command) in [
        (vec![("Cargo.toml", "")], "cargo test"),
        (
            vec![
                ("package.json", r#"{"scripts": {"test": "vitest"}}"#),
                ("pnpm-lock.yaml", ""),
            ],
            "pnpm test",
        ),
        (
            vec![
                ("package.json", r#"{"scripts": {"test": "jest"}}"#),
                ("yarn.lock", ""),
            ],
            "yarn test",
        ),
        (vec![("pytest.ini", "")], "pytest"),
        (vec![("Makefile", "test:\n\ttrue\n")], "make test"),
    ] {
        let folder = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(folder.path().join(name), text).unwrap();
        }
        let session = in_folder(&client, folder.path()).await;
        assert_eq!(
            plan(&client, session.id, None).await.unwrap().command,
            command
        );
    }
    // Empty and overlong commands are bad requests, an unknown session is not found.
    assert_eq!(
        remote_code(plan(&client, session.id, Some("  ")).await),
        ErrorCode::BadRequest
    );
    assert_eq!(
        remote_code(plan(&client, SessionId::new(), None).await),
        ErrorCode::NotFound
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_run_starts_from_the_login_environment() {
    let dir = tempfile::tempdir().unwrap();
    let shell = dir.path().join("login-shell");
    std::fs::write(
        &shell,
        "#!/bin/sh\nexport FROM_LOGIN_SHELL=yes\nexec /usr/bin/env -0\n",
    )
    .unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    // macOS vets the first execution of a new executable; do that outside the timeouts.
    let _ = std::process::Command::new(&shell)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let t = TestDaemon::start_with("", |config| {
        config.login_shell = Some(shell.clone());
    })
    .await;
    let client = t.connect().await;
    let work = tempfile::tempdir().unwrap();
    let session = in_folder(&client, work.path()).await;
    // The first capture of a shell happens in the background; a run waits for it.
    let run = verify(
        &client,
        session.id,
        Some("echo \"login=$FROM_LOGIN_SHELL\""),
        false,
    )
    .await
    .unwrap();
    assert_eq!(run.output_tail, "login=yes\n");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_are_stored_and_survive_a_restart() {
    let t = TestDaemon::start_with(PLAIN, |_| {}).await;
    let (client, _events) = t.connect_subscribed().await;
    let work = tempfile::tempdir().unwrap();
    let session = in_folder(&client, work.path()).await;
    let passed = verify(&client, session.id, Some("echo fine"), false)
        .await
        .unwrap();
    let failed = verify(&client, session.id, Some("echo broken >&2; exit 2"), false)
        .await
        .unwrap();
    assert!(failed.id > passed.id);

    let common::TestDaemon {
        _dir,
        paths,
        daemon,
    } = t;
    drop(client);
    daemon.shutdown();
    timeout(WAIT, daemon.wait()).await.unwrap().unwrap();
    let storage = hutan_storage::Storage::open(&paths.database).unwrap();
    let rows = storage.verify_runs().of_session(session.id, 10).unwrap();
    assert_eq!(rows.len(), 2);
    let row = &rows[0];
    assert_eq!(row.id, failed.id);
    assert_eq!(row.status, "failed");
    assert_eq!(row.exit_code, Some(2));
    assert_eq!(row.command, "echo broken >&2; exit 2");
    assert_eq!(canonical(Path::new(&row.cwd)), canonical(work.path()));
    assert_eq!(row.output_tail, "broken\n");
    assert_eq!(row.requested_by, "user");
    assert_eq!(row.source, "caller");
    assert!(row.ended_ms.is_some() && row.duration_ms.is_some());
    assert_eq!(rows[1].status, "passed");
    assert_eq!(
        storage
            .verify_runs()
            .latest(session.id)
            .unwrap()
            .unwrap()
            .id,
        failed.id
    );
    storage.close();
    let _keep = _dir;
}
