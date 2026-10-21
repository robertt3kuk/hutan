//! `session.close` / `session.reopen` (protocol 0.30, capability `session.close`): a real daemon
//! on a temporary `HUTAN_HOME`, real client connections, real shells in PTYs, and the fake ACP
//! agent for chats.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use hutan_client::{Client, ClientError};
use hutan_core::{AgentEventKind, SessionId, SessionState};
use hutan_protocol::ErrorCode;
use hutan_protocol::blocks::BlockState;
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Empty, Event, Request, SessionBusy, SessionClose,
    SessionInfo, SessionList, SessionListFilter, SessionRef, SessionResult, SessionSnapshot,
};
use hutan_protocol::recovery::RecoveryKind;
use hutan_storage::Storage;
use serde_json::Value;
use tokio::time::{sleep, timeout};

async fn list(client: &Client) -> Vec<SessionInfo> {
    client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap()
        .sessions
}

async fn listed(client: &Client, id: SessionId) -> Option<SessionInfo> {
    list(client).await.into_iter().find(|s| s.id == id)
}

async fn close(client: &Client, id: SessionId, stop_running: bool) -> Result<Empty, ClientError> {
    client
        .call::<Empty>(Request::SessionClose(SessionClose {
            session_id: id,
            stop_running,
        }))
        .await
}

async fn reopen(client: &Client, id: SessionId) -> Result<SessionInfo, ClientError> {
    client
        .call::<SessionResult>(Request::SessionReopen(SessionRef { session_id: id }))
        .await
        .map(|r| r.session)
}

fn busy_of(error: ClientError) -> SessionBusy {
    match &error {
        ClientError::Remote(body) if body.code == ErrorCode::InvalidState => error
            .busy()
            .unwrap_or_else(|| panic!("no busy data: {body:?}")),
        other => panic!("expected invalid_state, got {other:?}"),
    }
}

async fn wait_listed(
    client: &Client,
    id: SessionId,
    what: &str,
    pred: impl Fn(&SessionInfo) -> bool,
) {
    timeout(WAIT, async {
        loop {
            if listed(client, id).await.is_some_and(|s| pred(&s)) {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("session {id}: timed out waiting for {what}"));
}

async fn report_working(client: &Client, id: SessionId) {
    client
        .call::<AgentApplied>(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: id,
            source: "fake-hooks".into(),
            agent: "fake".into(),
            state: Some(AgentState::Working),
            message: None,
            seq: 1,
            agent_session_id: None,
            resume_argv: None,
            node: None,
        }))
        .await
        .unwrap();
}

fn zsh_available() -> bool {
    Path::new("/bin/zsh").exists()
}

/// The closed session rows as the database has them. Storage writes are queued behind the
/// writer thread (ADR 0006): a reply does not mean the row is on disk yet.
fn closed_rows(paths: &hutan_daemon::Paths) -> Vec<hutan_storage::ClosedSession> {
    let storage = Storage::open(&paths.database).unwrap();
    let rows = storage.sessions().list_closed().unwrap();
    storage.close();
    rows
}

/// Reads the database through a second handle until exactly `count` sessions are closed.
async fn wait_closed_rows(
    paths: &hutan_daemon::Paths,
    count: usize,
) -> Vec<hutan_storage::ClosedSession> {
    timeout(WAIT, async {
        loop {
            let rows = closed_rows(paths);
            if rows.len() == count {
                return rows;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{count} closed session rows never reached the database; found {}",
            closed_rows(paths).len()
        )
    })
}

// Terminals -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn closing_an_idle_terminal_removes_it_and_announces_it() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let work = tempfile::tempdir().unwrap();
    let mut spec = spec(&["/bin/cat"]);
    spec.cwd = Some(std::fs::canonicalize(work.path()).unwrap());
    let created = create(&client, spec).await;
    let other = create(&client, common::spec(&["/bin/cat"])).await;
    let workspace = created.workspace_id;

    close(&client, created.id, false).await.unwrap();

    assert!(
        listed(&client, created.id).await.is_none(),
        "gone from the list"
    );
    assert!(listed(&client, other.id).await.is_some(), "the others stay");
    let snapshot: SessionSnapshot = client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap();
    assert!(snapshot.sessions.iter().all(|s| s.id != created.id));
    // The stop is announced before the close; the workspace's counts follow.
    let mut saw_stopped = false;
    wait_event(&mut events, "session.closed", |event| match event {
        Event::SessionUpdated { session }
            if session.id == created.id && session.state == SessionState::Stopped =>
        {
            saw_stopped = true;
            None
        }
        Event::SessionClosed { session_id } if *session_id == created.id => Some(()),
        _ => None,
    })
    .await;
    assert!(saw_stopped, "the session was stopped before it was closed");
    let workspaces = client
        .call::<hutan_protocol::messages::WorkspaceList>(Request::WorkspaceList(Empty {}))
        .await
        .unwrap()
        .workspaces;
    let counts = workspaces.iter().find(|w| w.id == workspace).unwrap();
    assert_eq!(counts.total_sessions, 0, "{counts:?}");
    assert_eq!(counts.live_sessions, 0, "{counts:?}");

    // The row is kept, marked closed.
    let rows = wait_closed_rows(&t.paths, 1).await;
    assert_eq!(rows[0].session.id, created.id);
    assert_eq!(rows[0].session.state, SessionState::Stopped);

    // Closed means closed: it is not there to stop, rename, or close again.
    assert_eq!(
        remote_code(close(&client, created.id, false).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(stop(&client, created.id).await),
        ErrorCode::NotFound
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_that_already_ended_is_closed_at_once() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let ended = create(&client, sh("exit 3")).await;
    wait_listed(&client, ended.id, "the exit", |s| {
        matches!(s.state, SessionState::Exited { .. })
    })
    .await;
    close(&client, ended.id, false).await.unwrap();
    assert!(listed(&client, ended.id).await.is_none());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_terminal_asks_first_and_stop_running_closes_it() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let created = create(&client, spec(&["/bin/cat"])).await;
    report_working(&client, created.id).await;
    wait_listed(&client, created.id, "the agent working", |s| {
        s.agent
            .as_ref()
            .is_some_and(|agent| agent.state == AgentState::Working)
    })
    .await;

    let busy = busy_of(close(&client, created.id, false).await.unwrap_err());
    assert_eq!(busy.agent.as_deref(), Some("fake"));
    assert_eq!(busy.state, "working");
    assert_eq!(busy.command, None);
    let still = listed(&client, created.id)
        .await
        .expect("refused: still listed");
    assert_eq!(still.state, SessionState::Running, "refused: still running");
    assert!(closed_rows(&t.paths).is_empty());

    close(&client, created.id, true).await.unwrap();
    assert!(listed(&client, created.id).await.is_none());
    wait_event(&mut events, "session.closed", |event| {
        matches!(event, Event::SessionClosed { session_id } if *session_id == created.id)
            .then_some(())
    })
    .await;
    wait_closed_rows(&t.paths, 1).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreground_command_is_what_runs_in_a_busy_shell() {
    if !zsh_available() {
        eprintln!("skipping: zsh is not installed");
        return;
    }
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let work = tempfile::tempdir().unwrap();
    let zsh_home = tempfile::tempdir().unwrap();
    let mut zsh = spec(&["/bin/zsh", "-i"]);
    zsh.cwd = Some(std::fs::canonicalize(work.path()).unwrap());
    zsh.size = Some(size(200, 10));
    zsh.env.insert(
        "HOME".into(),
        zsh_home.path().to_string_lossy().into_owned(),
    );
    let created = create(&client, zsh).await;
    let mut view = View::attach(&client, terminal_of(&created), None).await;
    view.wait_for("a prompt", |s| s.contains("%")).await;

    // At its prompt the shell is idle: it would close without asking. A second shell shows it.
    let idle = create(&client, spec(&["/bin/cat"])).await;
    close(&client, idle.id, false).await.unwrap();

    client
        .send_input(terminal_of(&created), b"sleep 600\r")
        .await
        .unwrap();
    wait_event(&mut events, "the command running", |event| match event {
        Event::TerminalBlock { block, .. }
            if block.state == BlockState::Running
                && block.command.as_deref() == Some("sleep 600") =>
        {
            Some(())
        }
        _ => None,
    })
    .await;

    let busy = busy_of(close(&client, created.id, false).await.unwrap_err());
    assert_eq!(busy.command.as_deref(), Some("sleep 600"));
    assert_eq!(busy.agent, None);
    assert_eq!(busy.state, "working");
    assert_eq!(
        listed(&client, created.id).await.unwrap().state,
        SessionState::Running
    );

    close(&client, created.id, true).await.unwrap();
    assert!(listed(&client, created.id).await.is_none());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reopening_a_terminal_respawns_it_in_its_directory_with_the_same_identity() {
    let t = TestDaemon::start().await;
    let (client, _events) = t.connect_subscribed().await;
    let work = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(work.path()).unwrap();
    let mut spec = sh("pwd; exec cat");
    spec.cwd = Some(root.clone());
    spec.title = Some("my shell".into());
    let created = create(&client, spec).await;
    let mut view = View::attach(&client, terminal_of(&created), None).await;
    view.wait_for("the directory", |s| s.contains(&root.to_string_lossy()))
        .await;
    drop(view);

    // Not closed: nothing to reopen.
    assert_eq!(
        remote_code(reopen(&client, created.id).await),
        ErrorCode::InvalidState
    );
    assert_eq!(
        remote_code(reopen(&client, SessionId::new()).await),
        ErrorCode::NotFound
    );

    close(&client, created.id, false).await.unwrap();
    let back = reopen(&client, created.id).await.unwrap();
    assert_eq!(back.id, created.id, "the same session");
    assert_eq!(back.title, "my shell");
    assert_eq!(back.workspace_id, created.workspace_id);
    assert_eq!(back.cwd.as_deref(), Some(root.as_path()));
    assert_eq!(back.state, SessionState::Running);
    assert_ne!(back.terminal_id, created.terminal_id, "a new terminal");
    assert!(matches!(
        back.recovery.as_ref().map(|r| &r.kind),
        Some(RecoveryKind::ShellRespawned { cwd: Some(cwd), .. }) if *cwd == root
    ));
    let again = listed(&client, created.id).await.expect("listed again");
    assert_eq!(again.id, created.id);
    let mut view = View::attach(&client, terminal_of(&back), None).await;
    view.wait_for("the directory again", |s| {
        s.contains(&root.to_string_lossy())
    })
    .await;
    wait_closed_rows(&t.paths, 0).await;

    // Reopened twice is not possible; closing again works like the first time.
    assert_eq!(
        remote_code(reopen(&client, created.id).await),
        ErrorCode::InvalidState
    );
    close(&client, created.id, true).await.unwrap();
    wait_closed_rows(&t.paths, 1).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_session_is_not_restored_and_can_be_reopened_after_a_daemon_restart() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let work = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(work.path()).unwrap();
    let mut spec = spec(&["/bin/cat"]);
    spec.cwd = Some(root.clone());
    let closed = create(&client, spec).await;
    let kept = create(&client, common::spec(&["/bin/cat"])).await;
    close(&client, closed.id, false).await.unwrap();
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    let sessions = list(&client).await;
    assert!(sessions.iter().all(|s| s.id != closed.id), "{sessions:?}");
    assert!(
        sessions.iter().any(|s| s.id == kept.id),
        "the open one is restored"
    );

    let back = reopen(&client, closed.id).await.unwrap();
    assert_eq!(back.id, closed.id);
    assert_eq!(back.state, SessionState::Running);
    assert_eq!(back.cwd.as_deref(), Some(root.as_path()));
    assert_eq!(back.workspace_id, closed.workspace_id);
    drop(client);

    // Open again: it is restored with the daemon like any session.
    let t = t.restart().await;
    let client = t.connect().await;
    assert!(listed(&client, closed.id).await.is_some());
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
async fn a_closed_agent_terminal_comes_back_through_the_agents_resume_command() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let bin = tempfile::tempdir().unwrap();
    install_fake_agent(bin.path());
    let out_dir = tempfile::tempdir().unwrap();
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
        .call::<AgentApplied>(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: created.id,
            source: "fake-hooks".into(),
            agent: "fake".into(),
            state: Some(AgentState::Working),
            message: None,
            seq: 1,
            agent_session_id: Some("conv 42".into()),
            resume_argv: Some(argv.clone()),
            node: None,
        }))
        .await
        .unwrap();
    close(&client, created.id, true).await.unwrap();
    let back = reopen(&client, created.id).await.unwrap();
    assert_eq!(
        back.recovery.as_ref().map(|r| &r.kind),
        Some(&RecoveryKind::AgentResumed { resume_argv: argv })
    );
    timeout(WAIT, async {
        while !out.exists() {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the agent was started again");
    t.stop().await;
}

// Chats -----------------------------------------------------------------------------------------

const SESSION: &str = "sess_fixture_1";

fn acp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(
        bin.is_file(),
        "hutan-acp-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"))
}

/// A directory of transcripts for a daemon that will spawn the agent several times.
fn script(dir: &Path, names: &[&str]) -> PathBuf {
    let scripted = dir.join("script");
    std::fs::create_dir_all(&scripted).unwrap();
    for (index, name) in names.iter().enumerate() {
        std::fs::copy(
            fixture(name),
            scripted.join(format!("{:02}-{name}.jsonl", index + 1)),
        )
        .unwrap();
    }
    scripted
}

fn received_of(work: &Path, method: &str) -> Vec<Value> {
    std::fs::read_to_string(work.join("received.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap()["msg"].clone())
        .filter(|msg| msg["method"].as_str() == Some(method))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_chat_keeps_its_journal_and_reopening_resumes_the_conversation() {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["plain-turn", "load"]);
    let toml = format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {scripted:?}, \"acp\"]\n",
        bin = acp_bin(),
        received = work.path().join("received.jsonl"),
    );
    let t = TestDaemon::start_with(&toml, |_| {}).await;
    let (client, mut events) = t.connect_subscribed().await;
    let created = client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: work.path().to_owned(),
            agent: "omp".into(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: Some("my chat".into()),
            mode: None,
            provider_id: None,
            system_prompt: None,
        }))
        .await
        .unwrap()
        .session;
    client.chat_send(created.id, "Say hello.").await.unwrap();
    wait_event(&mut events, "the turn to end", |event| match event {
        Event::ChatEvent { event, .. } if event.kind.ends_turn() => Some(()),
        _ => None,
    })
    .await;
    wait_listed(&client, created.id, "idle", |s| {
        s.state == SessionState::Idle
    })
    .await;

    // Idle: closes without asking; the agent ends.
    close(&client, created.id, false).await.unwrap();
    assert!(listed(&client, created.id).await.is_none());
    let storage = Storage::open(&t.paths.database).unwrap();
    let journal = storage.chat().items(created.id, None, 500).unwrap();
    storage.close();
    assert!(
        journal
            .iter()
            .any(|row| row.event_json.contains("Hello, world.")),
        "the journal is kept"
    );
    assert!(received_of(work.path(), "session/load").is_empty());

    let back = reopen(&client, created.id).await.unwrap();
    assert_eq!(back.id, created.id);
    assert_eq!(back.title, "my chat");
    wait_listed(&client, created.id, "idle again", |s| {
        s.state == SessionState::Idle
    })
    .await;
    let loads = received_of(work.path(), "session/load");
    assert_eq!(loads.len(), 1, "the agent resumed the conversation");
    assert_eq!(loads[0]["params"]["sessionId"], SESSION);
    assert!(matches!(
        listed(&client, created.id)
            .await
            .unwrap()
            .recovery
            .map(|r| r.kind),
        Some(RecoveryKind::ChatResumed { .. })
    ));
    t.stop().await;
}

/// A daemon whose `omp` replays `transcript`, and a chat in a fresh directory.
async fn omp_chat(transcript: &str) -> (TestDaemon, Client, tempfile::TempDir, SessionInfo) {
    let work = tempfile::tempdir().unwrap();
    let toml = format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\n",
        bin = acp_bin(),
        received = work.path().join("received.jsonl"),
        transcript = fixture(transcript),
    );
    let t = TestDaemon::start_with(&toml, |_| {}).await;
    let client = t.connect().await;
    let created = client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: work.path().to_owned(),
            agent: "omp".into(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: None,
            system_prompt: None,
        }))
        .await
        .unwrap()
        .session;
    (t, client, work, created)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_in_the_middle_of_a_turn_is_busy() {
    let (t, client, _work, created) = omp_chat("cancel").await;
    let mut events = client.events();
    client
        .call::<hutan_protocol::messages::Subscribed>(Request::EventsSubscribe(Empty {}))
        .await
        .unwrap();
    client.chat_send(created.id, "long task").await.unwrap();
    wait_event(&mut events, "text", |event| match event {
        Event::ChatEvent { event, .. }
            if matches!(event.kind, AgentEventKind::MessageDelta { .. }) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    wait_listed(&client, created.id, "the turn", |s| {
        s.chat.as_ref().is_some_and(|chat| chat.turn_active)
    })
    .await;

    let busy = busy_of(close(&client, created.id, false).await.unwrap_err());
    assert_eq!(busy.state, "turn");
    assert_eq!(busy.agent.as_deref(), Some("omp"));
    assert!(listed(&client, created.id).await.is_some(), "kept");

    close(&client, created.id, true).await.unwrap();
    assert!(listed(&client, created.id).await.is_none());
    wait_closed_rows(&t.paths, 1).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_waiting_for_an_approval_is_busy() {
    let (t, client, _work, created) = omp_chat("permission-allow").await;
    client.chat_send(created.id, "edit lib.rs").await.unwrap();
    wait_listed(&client, created.id, "the approval", |s| {
        s.state == SessionState::WaitingApproval
    })
    .await;
    let busy = busy_of(close(&client, created.id, false).await.unwrap_err());
    assert_eq!(busy.state, "approval");
    close(&client, created.id, true).await.unwrap();
    assert!(listed(&client, created.id).await.is_none());
    t.stop().await;
}
