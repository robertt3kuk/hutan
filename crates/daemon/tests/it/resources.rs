//! The resource manager end to end: a real daemon on a temporary `HUTAN_HOME`, real shells in
//! real PTYs, real client connections. Processes are only ever signalled when the test started
//! them (inside a session, or as a bystander it kills itself).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::{TestDaemon, WAIT, create, remote_code, sh, spec, terminal_of, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::{NodeKind, NodeOutcome, SessionId, SessionState};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ChatCreate, ChatSend, ChatSent};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Event, Request, SessionInfo, SessionList,
    SessionListFilter, SessionResult,
};
use hutan_protocol::resources::{
    InterruptVia, ProcessInfo, ResourceSample, ResourcesSubscribed, SessionResources, SignalName,
};
use hutan_protocol::tree::{AgentTree, AgentTreeRequest, NodeReport};
use tokio::time::{sleep, timeout};

async fn list(client: &Client, session: SessionId) -> Option<SessionResources> {
    client
        .resources_list(Some(session))
        .await
        .unwrap()
        .sessions
        .into_iter()
        .next()
}

/// Polls `resources.list` until the session's processes satisfy `ready`.
async fn wait_processes(
    client: &Client,
    session: SessionId,
    what: &str,
    ready: impl Fn(&[ProcessInfo]) -> bool,
) -> SessionResources {
    timeout(WAIT, async {
        loop {
            if let Some(resources) = list(client, session).await
                && ready(&resources.processes)
            {
                return resources;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for: {what}"))
}

fn sleepers(processes: &[ProcessInfo]) -> Vec<&ProcessInfo> {
    processes.iter().filter(|p| p.name == "sleep").collect()
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn wait_gone(pid: u32) {
    timeout(WAIT, async {
        while alive(pid) {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("process {pid} is still running"));
}

/// A process the test owns that belongs to no session; killed when dropped.
struct Bystander(Child);

impl Bystander {
    fn start() -> Self {
        Self(
            Command::new("sleep")
                .arg("120")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn alive(&self) -> bool {
        alive(self.pid())
    }
}

impl Drop for Bystander {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const THREE: &str = "sleep 60 & sleep 60 & wait";

async fn three_process_session(client: &Client) -> SessionInfo {
    let session = create(client, sh(THREE)).await;
    wait_processes(client, session.id, "a shell and two sleeps", |p| {
        p.len() == 3 && sleepers(p).len() == 2
    })
    .await;
    session
}

async fn session_state(client: &Client, id: SessionId) -> SessionInfo {
    client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap()
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_shows_its_process_tree_and_the_daemon_row() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    assert!(client.supports_resources());
    let session = three_process_session(&client).await;
    let sample = client.resources_list(None).await.unwrap();
    let resources = sample
        .sessions
        .iter()
        .find(|s| s.session_id == session.id)
        .unwrap();
    // The shell, then its two children, parents first.
    let shell = &resources.processes[0];
    // `/bin/sh` is bash on macOS.
    assert!(
        ["sh", "bash"].contains(&shell.name.as_str()),
        "{}",
        shell.name
    );
    assert!(shell.command.contains(THREE), "{}", shell.command);
    for child in &resources.processes[1..] {
        assert_eq!(
            (child.ppid, child.command.as_str()),
            (shell.pid, "sleep 60")
        );
        assert!(child.rss_bytes > 0 && child.threads > 0);
    }
    assert_eq!(resources.totals.processes, 3);
    assert!(!resources.truncated && !sample.truncated);
    // `list` without subscribers reads twice: CPU figures are there.
    assert!(resources.processes.iter().all(|p| p.cpu_percent.is_some()));
    // The daemon is a row of its own (in-process here, so it is this process).
    assert_eq!(sample.daemon.pid, std::process::id());
    assert!(sample.daemon.rss_bytes > 0);

    // One session's list names only it; an unknown session is refused.
    let other = create(&client, sh(THREE)).await;
    let only = client.resources_list(Some(other.id)).await.unwrap();
    assert_eq!(only.sessions.len(), 1);
    assert_eq!(only.sessions[0].session_id, other.id);
    let unknown = client.resources_list(Some(SessionId::new())).await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    client.session_kill(other.id).await.unwrap();
    client.session_kill(session.id).await.unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn signalling_a_child_updates_the_tree_and_the_subscription_stream() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = three_process_session(&client).await;
    let victim = sleepers(&list(&client, session.id).await.unwrap().processes)[0].pid;

    let mut events = client.events();
    let subscribed = client.resources_subscribe(Some(1000)).await.unwrap();
    assert_eq!(subscribed.interval_ms, 1000);
    assert!(
        subscribed
            .sample
            .sessions
            .iter()
            .any(|s| s.session_id == session.id)
    );

    let done = client
        .process_signal(session.id, victim, SignalName::Term)
        .await
        .unwrap();
    assert_eq!((done.pid, done.signal), (victim, SignalName::Term));
    wait_gone(victim).await;
    // The next samples show two processes.
    let sample: ResourceSample =
        wait_event(
            &mut events,
            "a sample without the victim",
            |event| match event {
                Event::ResourcesUpdated { sample } => sample
                    .sessions
                    .iter()
                    .find(|s| s.session_id == session.id)
                    .filter(|s| s.processes.iter().all(|p| p.pid != victim))
                    .map(|_| (**sample).clone()),
                _ => None,
            },
        )
        .await;
    let resources = &sample.sessions[0];
    assert_eq!(resources.processes.len(), 2, "{resources:?}");
    client.resources_unsubscribe().await.unwrap();
    client.session_kill(session.id).await.unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pids_outside_the_session_are_refused() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = three_process_session(&client).await;
    let other = create(&client, sh(THREE)).await;
    let other_pid = wait_processes(&client, other.id, "other shell", |p| !p.is_empty())
        .await
        .processes[0]
        .pid;
    let bystander = Bystander::start();

    for (pid, what) in [
        (bystander.pid(), "a process of nobody's session"),
        (other_pid, "another session's shell"),
        (std::process::id(), "hutand itself (in-process here)"),
        (1, "pid 1"),
        (0, "pid 0"),
    ] {
        let result = client
            .process_signal(session.id, pid, SignalName::Kill)
            .await;
        assert_eq!(remote_code(result), ErrorCode::Forbidden, "{what}");
    }
    assert!(bystander.alive());
    assert!(alive(other_pid));
    // The message says why.
    let error = client
        .process_signal(session.id, bystander.pid(), SignalName::Kill)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("descendants"), "{error}");
    // A process that left is not found, not forbidden-by-accident.
    let missing = client
        .process_signal(SessionId::new(), 4242, SignalName::Term)
        .await;
    assert_eq!(remote_code(missing), ErrorCode::NotFound);

    // An ended session has no processes to signal.
    client.session_kill(other.id).await.unwrap();
    let ended = client
        .process_signal(other.id, other_pid, SignalName::Term)
        .await;
    assert_eq!(remote_code(ended), ErrorCode::InvalidState);
    client.session_kill(session.id).await.unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_kill_ends_the_whole_tree_even_with_job_control() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    // `set -m`: every background job gets a process group of its own, which a plain group kill
    // of the shell would miss.
    let session = create(&client, sh("set -m; sleep 61 & sleep 62 & wait")).await;
    let resources = wait_processes(&client, session.id, "two sleeps", |p| {
        sleepers(p).len() == 2
    })
    .await;
    let pids: Vec<u32> = resources.processes.iter().map(|p| p.pid).collect();

    let killed = client.session_kill(session.id).await.unwrap();
    assert_eq!(killed.state, SessionState::Stopped);
    for pid in pids {
        wait_gone(pid).await;
    }
    assert_eq!(
        session_state(&client, session.id).await.state,
        SessionState::Stopped
    );
    // Not running any more: not found among the live, and a second kill is refused.
    assert!(list(&client, session.id).await.is_none());
    let again = client.session_kill(session.id).await;
    assert_eq!(remote_code(again), ErrorCode::InvalidState);
    let unknown = client.session_kill(SessionId::new()).await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sampling_runs_only_while_somebody_watches() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    // A busy process: a CPU figure above zero is easy to tell from none at all.
    let session = create(&client, sh("yes > /dev/null & wait")).await;
    wait_processes(&client, session.id, "yes", |p| {
        p.iter().any(|p| p.name == "yes")
    })
    .await;

    let mut events = client.events();
    let first = client.resources_subscribe(Some(1000)).await.unwrap();
    // Nobody sampled before: no figures in the first sample.
    assert!(
        first
            .sample
            .sessions
            .iter()
            .flat_map(|s| &s.processes)
            .all(|p| p.cpu_percent.is_none())
    );
    let busy: ProcessInfo = wait_event(&mut events, "a CPU figure for yes", |event| match event {
        Event::ResourcesUpdated { sample } => sample
            .sessions
            .iter()
            .flat_map(|s| &s.processes)
            .find(|p| p.name == "yes" && p.cpu_percent.is_some_and(|cpu| cpu > 5.0))
            .cloned(),
        _ => None,
    })
    .await;
    assert!(busy.cpu_percent.unwrap() <= 400.0);

    // The last subscriber leaves: sampling stops and forgets its baseline, so the next
    // subscription starts without figures again. A sampler that kept going would have them.
    client.resources_unsubscribe().await.unwrap();
    sleep(Duration::from_millis(1500)).await;
    let again: ResourcesSubscribed = client.resources_subscribe(Some(1000)).await.unwrap();
    assert!(
        again
            .sample
            .sessions
            .iter()
            .flat_map(|s| &s.processes)
            .all(|p| p.cpu_percent.is_none()),
        "{again:?}"
    );

    // A subscriber that disconnects ends it too; the interval has a floor.
    let other = t.connect().await;
    assert_eq!(
        remote_code(other.resources_subscribe(Some(100)).await),
        ErrorCode::BadRequest
    );
    drop(other);
    client.resources_unsubscribe().await.unwrap();
    client.session_kill(session.id).await.unwrap();
    t.stop().await;
}

fn pid_report(
    session: SessionId,
    seq: u64,
    node: Option<NodeReport>,
    pid: Option<u32>,
) -> AgentReportRequest {
    AgentReportRequest {
        session_id: session,
        source: "test-agent".into(),
        agent: "fake".into(),
        state: Some(AgentState::Working),
        message: None,
        seq,
        agent_session_id: None,
        resume_argv: None,
        node,
        pid,
    }
}

fn sub_report(id: &str) -> NodeReport {
    NodeReport {
        id: id.into(),
        parent: None,
        kind: NodeKind::Sub,
        name: Some(id.into()),
        agent_type: None,
        outcome: None,
        model: None,
        activity: None,
        usage: None,
        transcript_path: None,
    }
}

async fn report(client: &Client, request: AgentReportRequest) {
    let applied: AgentApplied = client.call(Request::AgentReport(request)).await.unwrap();
    assert!(applied.applied);
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_nodes_with_reported_pids_appear_in_the_view() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = three_process_session(&client).await;
    let processes = list(&client, session.id).await.unwrap().processes;
    let shell = processes[0].pid;
    let worker = sleepers(&processes)[0].pid;
    let bystander = Bystander::start();

    // The main agent is the shell; a subagent is a sleep; one claims a process that is not
    // below the session; one has no pid.
    report(&client, pid_report(session.id, 1, None, Some(shell))).await;
    let mut node = sub_report("worker");
    node.parent = Some("root".into());
    report(&client, pid_report(session.id, 2, Some(node), Some(worker))).await;
    report(
        &client,
        pid_report(
            session.id,
            3,
            Some(sub_report("liar")),
            Some(bystander.pid()),
        ),
    )
    .await;
    report(
        &client,
        pid_report(session.id, 4, Some(sub_report("thread")), None),
    )
    .await;

    let resources = list(&client, session.id).await.unwrap();
    let agent = |id: &str| {
        resources
            .agents
            .iter()
            .find(|a| a.node_id == id)
            .unwrap_or_else(|| panic!("no agent {id}: {:?}", resources.agents))
    };
    assert_eq!(agent("root").pid, Some(shell));
    assert_eq!(agent("worker").pid, Some(worker));
    assert_eq!(
        agent("liar").pid,
        None,
        "a pid outside the tree is not shown"
    );
    assert_eq!(agent("thread").pid, None);
    let marked = |pid: u32| {
        resources
            .processes
            .iter()
            .find(|p| p.pid == pid)
            .unwrap()
            .agent_node
            .clone()
    };
    assert_eq!(marked(shell).as_deref(), Some("root"));
    assert_eq!(marked(worker).as_deref(), Some("worker"));
    // The tree carries the pid for clients that read it.
    let tree: AgentTree = client
        .call(Request::AgentTree(AgentTreeRequest {
            session_id: Some(session.id),
        }))
        .await
        .unwrap();
    let nodes = &tree.observations[0].nodes;
    assert_eq!(nodes["worker"].pid, Some(worker));
    assert_eq!(nodes["thread"].pid, None);

    // Ending the node forgets its pid.
    let mut done = sub_report("worker");
    done.outcome = Some(NodeOutcome::Completed);
    report(&client, pid_report(session.id, 5, Some(done), None)).await;
    let resources = list(&client, session.id).await.unwrap();
    assert!(resources.agents.iter().all(|a| a.node_id != "worker"));
    client.session_kill(session.id).await.unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupting_a_subagent_process_and_refusing_the_rest() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    // A background job of a non-interactive shell ignores SIGINT: a polite interrupt does
    // nothing, `force` follows with SIGTERM.
    let session = three_process_session(&client).await;
    let processes = list(&client, session.id).await.unwrap().processes;
    let (polite, stubborn) = (sleepers(&processes)[0].pid, sleepers(&processes)[1].pid);
    let bystander = Bystander::start();
    report(
        &client,
        pid_report(session.id, 1, Some(sub_report("polite")), Some(polite)),
    )
    .await;
    report(
        &client,
        pid_report(session.id, 2, Some(sub_report("stubborn")), Some(stubborn)),
    )
    .await;
    report(
        &client,
        pid_report(session.id, 3, Some(sub_report("nopid")), None),
    )
    .await;
    report(
        &client,
        pid_report(
            session.id,
            4,
            Some(sub_report("liar")),
            Some(bystander.pid()),
        ),
    )
    .await;

    let done = client
        .agent_interrupt(session.id, Some("polite".into()), false)
        .await
        .unwrap();
    assert_eq!((done.via, done.pid), (InterruptVia::Sigint, Some(polite)));
    sleep(Duration::from_millis(300)).await;
    assert!(alive(polite), "the job ignores SIGINT");

    let done = client
        .agent_interrupt(session.id, Some("stubborn".into()), true)
        .await
        .unwrap();
    assert_eq!(
        (done.via, done.pid),
        (InterruptVia::SigintTerm, Some(stubborn))
    );
    wait_gone(stubborn).await;

    // A subagent without a process id, on a harness that cannot stop one thread.
    let error = client
        .agent_interrupt(session.id, Some("nopid".into()), false)
        .await
        .unwrap_err();
    let ClientErrorView { code, message } = view(error);
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(
        message.contains("cannot stop a single subagent"),
        "{message}"
    );
    assert!(message.contains("main agent"), "{message}");
    // A pid the session does not own is never signalled.
    let liar = client
        .agent_interrupt(session.id, Some("liar".into()), true)
        .await;
    assert_eq!(remote_code(liar), ErrorCode::Forbidden);
    assert!(bystander.alive());
    // Unknown nodes; a node that finished.
    let unknown = client
        .agent_interrupt(session.id, Some("nobody".into()), false)
        .await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    let mut over = sub_report("nopid");
    over.outcome = Some(NodeOutcome::Failed);
    report(&client, pid_report(session.id, 5, Some(over), None)).await;
    let finished = client
        .agent_interrupt(session.id, Some("nopid".into()), false)
        .await;
    assert_eq!(remote_code(finished), ErrorCode::InvalidState);
    client.session_kill(session.id).await.unwrap();
    t.stop().await;
}

struct ClientErrorView {
    code: ErrorCode,
    message: String,
}

fn view(error: hutan_client::ClientError) -> ClientErrorView {
    match error {
        hutan_client::ClientError::Remote(body) => ClientErrorView {
            code: body.code,
            message: body.message,
        },
        other => panic!("expected a remote error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupting_the_main_agent_of_a_terminal_types_ctrl_c() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    // `cat` in a PTY ends on ^C, as an agent's TUI would cancel its turn.
    let session = create(&client, spec(&["cat"])).await;
    let _ = terminal_of(&session);
    wait_processes(&client, session.id, "cat", |p| !p.is_empty()).await;
    let done = client
        .agent_interrupt(session.id, None, false)
        .await
        .unwrap();
    assert_eq!((done.via, done.pid), (InterruptVia::Keystroke, None));
    timeout(WAIT, async {
        while session_state(&client, session.id).await.state.is_live() {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("cat ended on ^C");
    let after = client.agent_interrupt(session.id, None, false).await;
    assert_eq!(remote_code(after), ErrorCode::InvalidState);
    t.stop().await;
}

// Chat sessions -------------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/codex-0.162")
}

fn codex_replaying(transcript: &str, received: &Path) -> CodexConfig {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-codex-fixture");
    assert!(
        bin.is_file(),
        "hutan-codex-fixture not built: `cargo build -p hutan-agents --bins`"
    );
    CodexConfig {
        program: bin,
        args: vec![
            "replay".to_owned(),
            "--received".to_owned(),
            received.display().to_string(),
            fixtures().join(transcript).display().to_string(),
            "app-server".to_owned(),
        ],
        env: Vec::new(),
    }
}

async fn codex_session(client: &Client, cwd: &Path) -> SessionInfo {
    client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: cwd.to_owned(),
            agent: "codex".to_owned(),
            model: None,
            mode: None,
            approval_policy: None,
            sandbox: None,
            title: None,
            provider_id: None,
            system_prompt: None,
        }))
        .await
        .unwrap()
        .session
}

fn received(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["msg"].clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_codex_subagent_thread_is_interrupted_through_the_harness() {
    let work = tempfile::tempdir().unwrap();
    let log = work.path().join("received.jsonl");
    let config = codex_replaying("subagent-interrupt.jsonl", &log);
    let t = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let client = t.connect().await;
    let session = codex_session(&client, work.path()).await;
    client
        .call::<ChatSent>(Request::ChatSend(ChatSend {
            session_id: session.id,
            text: "explore".into(),
            attachments: vec![],
        }))
        .await
        .unwrap();
    const CHILD: &str = "01a00000-0000-7000-8000-000000000010";
    // The subagent shows up in the tree, a thread without a process of its own.
    timeout(WAIT, async {
        loop {
            let tree: AgentTree = client
                .call(Request::AgentTree(AgentTreeRequest {
                    session_id: Some(session.id),
                }))
                .await
                .unwrap();
            if tree
                .observations
                .first()
                .is_some_and(|o| o.nodes.contains_key(CHILD))
            {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the subagent thread appears");
    // The harness process is the session's root, and the main agent's node carries its pid.
    let resources = list(&client, session.id).await.unwrap();
    let harness = resources.processes[0].pid;
    let main = resources
        .agents
        .iter()
        .find(|a| a.kind == NodeKind::Main)
        .unwrap();
    assert_eq!(main.pid, Some(harness));
    assert_eq!(
        resources
            .agents
            .iter()
            .find(|a| a.node_id == CHILD)
            .unwrap()
            .pid,
        None
    );

    let done = client
        .agent_interrupt(session.id, Some(CHILD.into()), false)
        .await
        .unwrap();
    assert_eq!((done.via, done.pid), (InterruptVia::Thread, None));
    // The harness got `turn/interrupt` for the child's thread and turn, not the main one.
    let sent = received(&log);
    let interrupt = sent
        .iter()
        .find(|m| m["method"] == "turn/interrupt")
        .expect("turn/interrupt was sent");
    assert_eq!(interrupt["params"]["threadId"], CHILD);
    assert_eq!(
        interrupt["params"]["turnId"],
        "01a00000-0000-7000-8000-000000000011"
    );
    // The child's own turn ended as interrupted.
    timeout(WAIT, async {
        loop {
            let tree: AgentTree = client
                .call(Request::AgentTree(AgentTreeRequest {
                    session_id: Some(session.id),
                }))
                .await
                .unwrap();
            let node = &tree.observations[0].nodes[CHILD];
            if node.outcome == Some(NodeOutcome::Interrupted) {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the subagent ended interrupted");
    // It has no turn left: refused, as is a thread the harness never heard of.
    let finished = client
        .agent_interrupt(session.id, Some(CHILD.into()), false)
        .await;
    assert_eq!(remote_code(finished), ErrorCode::InvalidState);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_main_agent_of_a_chat_is_interrupted_by_its_turn_and_killing_ends_the_harness() {
    let work = tempfile::tempdir().unwrap();
    let log = work.path().join("received.jsonl");
    let config = codex_replaying("interrupt.jsonl", &log);
    let t = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let client = t.connect().await;
    let session = codex_session(&client, work.path()).await;
    // No turn yet.
    let idle = client.agent_interrupt(session.id, None, false).await;
    assert_eq!(remote_code(idle), ErrorCode::InvalidState);
    client
        .call::<ChatSent>(Request::ChatSend(ChatSend {
            session_id: session.id,
            text: "Write the numbers from 1 to 300".into(),
            attachments: vec![],
        }))
        .await
        .unwrap();
    // The turn is running: the harness is in the list and the interrupt reaches it.
    let resources = wait_processes(&client, session.id, "the harness", |p| !p.is_empty()).await;
    let harness = resources.processes[0].pid;
    let done = client
        .agent_interrupt(session.id, None, false)
        .await
        .unwrap();
    assert_eq!((done.via, done.pid), (InterruptVia::Turn, None));
    timeout(WAIT, async {
        while !received(&log)
            .iter()
            .any(|m| m["method"] == "turn/interrupt")
        {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("turn/interrupt reached the harness");

    // `session.kill` takes the harness down at once.
    let killed = client.session_kill(session.id).await.unwrap();
    assert!(!killed.state.is_live(), "{:?}", killed.state);
    wait_gone(harness).await;
    t.stop().await;
}
