//! Notification intents (protocol 0.13) end to end: a real daemon on a temporary `HUTAN_HOME`,
//! real client connections to its socket, real PTY sessions, and `hutan-codex-fixture replay`
//! standing in for `codex app-server`. Every trigger, the gate (dedup, finished bursts, settings,
//! expiry), focus suppression and the routing of `notify.act` are exercised over the wire.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, create, remote_code, sh, stop, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::Seq;
use hutan_core::agent::PermissionOutcome;
use hutan_core::{AgentEventKind, ApprovalPolicy, NodeKind, NodeOutcome, SandboxMode, SessionId};
use hutan_daemon::notify_native::fake::{Authorization, Call, FakePoster};
use hutan_daemon::notify_native::{OPEN_ACTION_ID, ResponseAction};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ApprovalRespond, ChatCreate};
use hutan_protocol::messages::{
    AgentApplied, AgentReleaseRequest, AgentReportRequest, AgentState, ClientKind, Empty, Event,
    Request, SessionInfo, SessionList, SessionListFilter, SessionResult,
};
use hutan_protocol::notify::{
    ACTION_OPEN, ActRoute, BODY_MAX_CHARS, IntentKind, NotificationIntent,
};
use hutan_protocol::tree::NodeReport;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/codex-0.162")
}

/// The fake app server, next to the test binaries; `just test` builds the workspace binaries
/// first.
fn fixture_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-codex-fixture");
    assert!(
        bin.is_file(),
        "hutan-codex-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

fn replay(name: &str, received: &Path) -> CodexConfig {
    CodexConfig {
        program: fixture_bin(),
        args: vec![
            "replay".to_owned(),
            "--received".to_owned(),
            received.display().to_string(),
            fixtures()
                .join(format!("{name}.jsonl"))
                .display()
                .to_string(),
            "app-server".to_owned(),
        ],
        env: Vec::new(),
    }
}

struct Env {
    daemon: TestDaemon,
    /// Where the fake agent works; also holds its `received.jsonl` log.
    work: tempfile::TempDir,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
}

impl Env {
    /// A daemon with `toml` as `daemon.toml`; its codex replays the committed fixture `name`
    /// (when given).
    async fn start(toml: &str, fixture: Option<&str>) -> Self {
        Self::start_with_poster(toml, fixture, None).await
    }

    /// As [`start`](Self::start), with `poster` standing in for the system's notification
    /// centre.
    async fn start_with_poster(
        toml: &str,
        fixture: Option<&str>,
        poster: Option<std::sync::Arc<FakePoster>>,
    ) -> Self {
        let work = tempfile::tempdir().unwrap();
        let config = fixture.map(|name| replay(name, &work.path().join("received.jsonl")));
        let daemon = TestDaemon::start_with(toml, move |c| {
            c.codex = config;
            c.native =
                poster.map(|p| p as std::sync::Arc<dyn hutan_daemon::notify_native::NativePoster>);
        })
        .await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    async fn plain() -> Self {
        Self::start("", None).await
    }

    async fn chat(
        &self,
        policy: Option<ApprovalPolicy>,
        sandbox: Option<SandboxMode>,
    ) -> SessionInfo {
        self.client
            .call::<SessionResult>(Request::ChatCreate(ChatCreate {
                contain: None,
                workspace_id: None,
                cwd: self.work.path().to_owned(),
                agent: "codex".to_owned(),
                model: None,
                approval_policy: policy,
                sandbox,
                title: None,
                mode: None,
                provider_id: None,
                system_prompt: None,
                account: None,
                safety: None,
            }))
            .await
            .unwrap()
            .session
    }

    async fn intents(&self) -> Vec<NotificationIntent> {
        self.client.notify_list().await.unwrap()
    }

    /// The next `notify.intent` event satisfying `pick`.
    async fn next_intent(
        &mut self,
        what: &str,
        mut pick: impl FnMut(&NotificationIntent) -> bool,
    ) -> NotificationIntent {
        wait_event(&mut self.events, what, |event| match event {
            Event::NotifyIntent { intent } if pick(intent) => Some(intent.clone()),
            _ => None,
        })
        .await
    }

    /// Asserts that no `notify.intent` event arrives within `within`.
    async fn no_intent(&mut self, within: Duration) {
        let end = tokio::time::Instant::now() + within;
        while let Ok(received) =
            timeout(end - tokio::time::Instant::now(), self.events.recv()).await
        {
            if let (_, Event::NotifyIntent { intent }) = received.expect("event stream closed") {
                panic!("unexpected intent: {intent:?}");
            }
        }
    }

    /// Waits until the chat turn of `session` ended.
    async fn turn_end(&mut self, session: SessionId) -> AgentEventKind {
        wait_event(&mut self.events, "the turn to end", |event| match event {
            Event::ChatEvent {
                session_id, event, ..
            } if *session_id == session && event.kind.ends_turn() => Some(event.kind.clone()),
            _ => None,
        })
        .await
    }

    async fn send(&self, session: SessionId, text: &str) {
        self.client.chat_send(session, text).await.unwrap();
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

async fn report(client: &Client, request: AgentReportRequest) {
    let applied: AgentApplied = client.call(Request::AgentReport(request)).await.unwrap();
    assert!(applied.applied, "report ignored");
}

fn main_report(session: SessionId, state: AgentState, seq: u64) -> AgentReportRequest {
    AgentReportRequest {
        pid: None,
        session_id: session,
        source: "test-agent".into(),
        agent: "fake".into(),
        state: Some(state),
        message: None,
        seq,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }
}

fn sub_report(
    session: SessionId,
    state: AgentState,
    seq: u64,
    activity: Option<&str>,
) -> AgentReportRequest {
    AgentReportRequest {
        pid: None,
        node: Some(NodeReport {
            id: "explore-1".into(),
            parent: None,
            kind: NodeKind::Sub,
            name: Some("Explore".into()),
            agent_type: None,
            outcome: None,
            model: None,
            activity: activity.map(str::to_owned),
            usage: None,
            transcript_path: None,
        }),
        ..main_report(session, state, seq)
    }
}

async fn release(client: &Client, session: SessionId, seq: u64, outcome: Option<NodeOutcome>) {
    let applied: AgentApplied = client
        .call(Request::AgentRelease(AgentReleaseRequest {
            session_id: session,
            source: "test-agent".into(),
            seq,
            node: None,
            outcome,
        }))
        .await
        .unwrap();
    assert!(applied.applied, "release ignored");
}

fn idle_session() -> hutan_protocol::messages::TerminalSpec {
    sh("exec sleep 600")
}

// ---- approval_required + notify.act routing ---------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_approval_raises_an_intent_that_notify_act_answers() {
    let mut t = Env::start("", Some("approval-touch")).await;
    let created = t
        .chat(Some(ApprovalPolicy::Untrusted), Some(SandboxMode::ReadOnly))
        .await;
    t.send(
        created.id,
        "Run `touch hi.txt` in a shell, then reply with the word done.",
    )
    .await;

    let intent = t
        .next_intent("the approval intent", |i| {
            i.kind == IntentKind::ApprovalRequired
        })
        .await;
    assert_eq!(intent.session_id, created.id);
    assert_eq!(intent.workspace_id, Some(created.workspace_id));
    assert!(
        intent.title.ends_with(": approval needed"),
        "{}",
        intent.title
    );
    assert!(intent.body.contains("touch hi.txt"), "{}", intent.body);
    assert!(intent.body.chars().count() <= BODY_MAX_CHARS);
    let actions: Vec<_> = intent.actions.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(
        actions,
        ["accept", "acceptWithExecpolicyAmendment", "cancel"],
        "the approval's own options"
    );
    assert_eq!(intent.expires_at_ms, None);
    assert_eq!(t.intents().await, vec![intent.clone()]);

    // A second client, which joined late, sees it pending too.
    let late = t.daemon.connect().await;
    assert_eq!(late.notify_list().await.unwrap(), vec![intent.clone()]);

    // Only what the intent offers; nothing reaches the agent otherwise.
    let wrong = t.client.notify_act(&intent.id, "decline").await;
    assert_eq!(remote_code(wrong), ErrorCode::OptionNotOffered);
    let open = t.client.notify_act(&intent.id, ACTION_OPEN).await;
    assert_eq!(
        remote_code(open),
        ErrorCode::OptionNotOffered,
        "an approval is answered, not opened"
    );
    let unknown = t.client.notify_act("no-such-intent", "accept").await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    assert_eq!(t.intents().await.len(), 1, "refusals leave it pending");
    let received =
        std::fs::read_to_string(t.work.path().join("received.jsonl")).unwrap_or_default();
    assert!(!received.contains("\"decision\""), "nothing answered yet");

    // Acting routes to approval.respond.
    let acted = late.notify_act(&intent.id, "accept").await.unwrap();
    assert_eq!(acted.route, ActRoute::Approval);
    assert_eq!(acted.desktop_clients, 0);
    let outcome = wait_event(&mut t.events, "approval.resolved", |event| match event {
        Event::ApprovalResolved { outcome, .. } => Some(outcome.clone()),
        _ => None,
    })
    .await;
    assert_eq!(
        outcome,
        PermissionOutcome::Selected {
            option_id: "accept".into()
        }
    );
    t.turn_end(created.id).await;
    // The turn's own end raises its intent after the approval's was withdrawn (events are
    // handled in order), so the list holds that one alone.
    let finished = t
        .next_intent("the turn's intent", |i| {
            i.kind == IntentKind::ChatTurnFinished
        })
        .await;
    assert_eq!(
        t.intents().await,
        vec![finished],
        "answered intents are gone"
    );
    let again = t.client.notify_act(&intent.id, "accept").await;
    assert_eq!(remote_code(again), ErrorCode::NotFound);

    // The agent received exactly the chosen decision, and the audit trail says who decided.
    let received: Vec<serde_json::Value> =
        std::fs::read_to_string(t.work.path().join("received.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["msg"].clone())
            .filter(|msg| msg["method"].is_null())
            .collect();
    assert_eq!(received.len(), 1, "{received:?}");
    assert_eq!(received[0]["result"]["decision"], "accept");
    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let decided_by: Option<String> = db
        .query_row(
            "SELECT decided_by FROM approvals WHERE session_id = ?1",
            [created.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(decided_by.as_deref(), Some("cli"));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_answered_elsewhere_withdraws_its_intent() {
    let mut t = Env::start("", Some("approval-touch")).await;
    let created = t.chat(Some(ApprovalPolicy::Untrusted), None).await;
    t.send(created.id, "Run `touch hi.txt`").await;
    let intent = t
        .next_intent("the approval intent", |i| {
            i.kind == IntentKind::ApprovalRequired
        })
        .await;
    let approval = t.client.approvals(None).await.unwrap().remove(0);

    t.client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval.id,
            option_id: "accept".to_owned(),
        }))
        .await
        .unwrap();
    wait_event(&mut t.events, "approval.resolved", |event| {
        matches!(event, Event::ApprovalResolved { .. }).then_some(())
    })
    .await;
    t.turn_end(created.id).await;
    let finished = t
        .next_intent("the turn's intent", |i| {
            i.kind == IntentKind::ChatTurnFinished
        })
        .await;
    assert_eq!(t.intents().await, vec![finished]);
    let late = t.client.notify_act(&intent.id, "accept").await;
    assert_eq!(remote_code(late), ErrorCode::NotFound);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_that_expires_with_its_turn_withdraws_its_intent() {
    let mut t = Env::start("", Some("approval-interrupt")).await;
    let created = t.chat(None, None).await;
    t.send(created.id, "Run `touch hi.txt`").await;
    t.next_intent("the approval intent", |i| {
        i.kind == IntentKind::ApprovalRequired
    })
    .await;
    t.client.chat_interrupt(created.id).await.unwrap();
    let outcome = wait_event(&mut t.events, "approval.resolved", |event| match event {
        Event::ApprovalResolved { outcome, .. } => Some(outcome.clone()),
        _ => None,
    })
    .await;
    assert_eq!(outcome, PermissionOutcome::Expired);
    t.turn_end(created.id).await;
    assert!(t.intents().await.is_empty());
    t.stop().await;
}

// ---- chat_turn_finished + focus ---------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_chat_turn_raises_an_intent_unless_a_client_has_the_session_focused() {
    let mut t = Env::start("", Some("plain-turn")).await;
    let focused = t.chat(None, None).await;
    let unfocused = t.chat(None, None).await;
    let released = t.chat(None, None).await;
    let dropped = t.chat(None, None).await;

    // Three connections report focus: one keeps it, one replaces it with nothing, one goes away.
    let keeper = t.daemon.connect().await;
    keeper.client_focus(Some(focused.id)).await.unwrap();
    let changer = t.daemon.connect().await;
    changer.client_focus(Some(released.id)).await.unwrap();
    changer.client_focus(None).await.unwrap();
    let leaver = t.daemon.connect().await;
    leaver.client_focus(Some(dropped.id)).await.unwrap();
    drop(leaver);
    // The daemon forgets a closed connection's focus.
    timeout(WAIT, async {
        while t.daemon_clients().await > 3 {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the daemon never noticed the closed connection");

    // Intents are watched on a subscription of their own: waiting for a turn's end on the other
    // one skips whatever arrives before it, an intent of the previous turn included.
    let (_watcher, mut intents) = t.daemon.connect_subscribed().await;
    // Turns finish in order: the focused session first, then the others.
    t.send(focused.id, "Say hi").await;
    t.turn_end(focused.id).await;
    for session in [&unfocused, &released, &dropped] {
        t.send(session.id, "Say hi").await;
        t.turn_end(session.id).await;
    }
    let mut raised = Vec::new();
    for _ in 0..3 {
        raised.push(
            wait_event(
                &mut intents,
                "a chat_turn_finished intent",
                |event| match event {
                    Event::NotifyIntent { intent }
                        if intent.kind == IntentKind::ChatTurnFinished =>
                    {
                        Some(intent.clone())
                    }
                    _ => None,
                },
            )
            .await,
        );
    }
    let mut sessions: Vec<_> = raised.iter().map(|i| i.session_id).collect();
    sessions.sort();
    let mut expected = vec![unfocused.id, released.id, dropped.id];
    expected.sort();
    assert_eq!(sessions, expected, "everything but the focused session");
    // Nothing more comes (the focused session's turn raised nothing).
    let quiet = tokio::time::Instant::now() + Duration::from_millis(300);
    while let Ok(received) = timeout(quiet - tokio::time::Instant::now(), intents.recv()).await {
        if let (_, Event::NotifyIntent { intent }) = received.expect("event stream closed") {
            panic!("unexpected intent: {intent:?}");
        }
    }
    let pending = t.intents().await;
    assert_eq!(pending.len(), 3);
    assert!(pending.iter().all(|i| i.session_id != focused.id));

    // The body is the assistant's reply, bounded and on one line.
    let intent = &raised[0];
    assert!(intent.title.ends_with(": reply ready"), "{}", intent.title);
    assert_ne!(intent.body, "The turn finished.", "the reply is quoted");
    assert!(!intent.body.is_empty() && intent.body.chars().count() <= BODY_MAX_CHARS);
    assert!(!intent.body.contains('\n'));
    assert_eq!(intent.actions.len(), 1);
    assert_eq!(intent.actions[0].id, ACTION_OPEN);
    assert!(intent.expires_at_ms.is_some());
    drop(keeper);
    t.stop().await;
}

impl Env {
    async fn daemon_clients(&self) -> u32 {
        let status: hutan_protocol::messages::DaemonStatusInfo = self
            .client
            .call(Request::DaemonStatus(Empty {}))
            .await
            .unwrap();
        status.clients
    }
}

// ---- session_exited_with_error + ack ----------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn only_a_non_zero_exit_raises_an_intent_and_ack_removes_it() {
    let mut t = Env::plain().await;
    // None of these is an error: a stopped session (signal) and a clean exit.
    let sleeper = create(&t.client, idle_session()).await;
    create(&t.client, sh("exit 0")).await;
    stop(&t.client, sleeper.id).await.unwrap();
    let failing = create(&t.client, sh("exit 3")).await;

    let intent = t.next_intent("the exit intent", |_| true).await;
    assert_eq!(intent.kind, IntentKind::SessionExitedWithError);
    assert_eq!(intent.session_id, failing.id);
    assert_eq!(intent.workspace_id, Some(failing.workspace_id));
    // Sessions of one folder are told apart by number: `sh 3`, not just `sh`.
    assert_eq!(
        intent.title,
        format!("{}: exited with an error", failing.title)
    );
    assert_eq!(intent.body, "The process exited with code 3.");
    assert_eq!(intent.actions.len(), 1);
    assert_eq!(t.intents().await, vec![intent.clone()]);
    t.no_intent(Duration::from_millis(300)).await;

    t.client.notify_ack(&intent.id).await.unwrap();
    assert!(t.intents().await.is_empty());
    assert_eq!(
        remote_code(t.client.notify_ack(&intent.id).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(t.client.notify_act(&intent.id, ACTION_OPEN).await),
        ErrorCode::NotFound,
        "an acknowledged intent cannot be acted on"
    );
    t.stop().await;
}

// ---- agent_blocked, agent_finished: dedup and bursts -------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_node_becoming_blocked_raises_one_bounded_redacted_intent_until_it_is_unblocked() {
    let mut t = Env::plain().await;
    let session = create(&t.client, idle_session()).await;
    report(&t.client, main_report(session.id, AgentState::Working, 1)).await;
    let long = format!(
        "wants to run `npm install` with token=abcdef0123456789 {}",
        "and a very long explanation ".repeat(20)
    );
    report(
        &t.client,
        sub_report(session.id, AgentState::Blocked, 2, Some(&long)),
    )
    .await;

    let intent = t
        .next_intent("the blocked intent", |i| i.kind == IntentKind::AgentBlocked)
        .await;
    assert_eq!(intent.session_id, session.id);
    assert_eq!(intent.title, "sh: Explore is waiting for you");
    assert!(intent.body.chars().count() <= BODY_MAX_CHARS);
    assert!(
        intent
            .body
            .starts_with("wants to run `npm install` with token=[REDACTED]")
    );
    assert!(!intent.body.contains("abcdef0123456789"));

    // Dedup: still blocked, only the activity changed.
    sleep(Duration::from_millis(400)).await;
    report(
        &t.client,
        sub_report(session.id, AgentState::Blocked, 3, Some("still waiting")),
    )
    .await;
    t.no_intent(Duration::from_millis(500)).await;
    assert_eq!(t.intents().await.len(), 1);

    // Rate limit: seen, unblocked and blocked again within ten seconds of the first intent.
    t.client.notify_ack(&intent.id).await.unwrap();
    report(
        &t.client,
        sub_report(session.id, AgentState::Working, 4, None),
    )
    .await;
    sleep(Duration::from_millis(400)).await;
    report(
        &t.client,
        sub_report(session.id, AgentState::Blocked, 5, None),
    )
    .await;
    // A request is never held back: blocked again, it calls again, at once.
    let again = t
        .next_intent("the second blocked intent", |i| {
            i.kind == IntentKind::AgentBlocked && i.id != intent.id
        })
        .await;
    assert_eq!(again.session_id, session.id);

    // Another session raises its own.
    let other = create(&t.client, idle_session()).await;
    report(
        &t.client,
        sub_report(other.id, AgentState::Blocked, 1, None),
    )
    .await;
    let second = t
        .next_intent("the other session's intent", |i| i.session_id == other.id)
        .await;
    assert_eq!(second.body, "It needs a decision from you.");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_enough_turn_ending_raises_agent_finished_and_failures_do_not() {
    let mut t = Env::start("[notify]\nmin_turn_secs = 1\n", None).await;
    let finished = create(&t.client, idle_session()).await;
    let released = create(&t.client, idle_session()).await;
    let failed = create(&t.client, idle_session()).await;
    let short = create(&t.client, idle_session()).await;
    let plain = create(&t.client, idle_session()).await;
    for session in [&finished, &released, &failed, &plain] {
        report(&t.client, main_report(session.id, AgentState::Working, 1)).await;
    }
    sleep(Duration::from_millis(1300)).await;
    // The short one starts now and ends after the coalescing interval: well under a second.
    report(&t.client, main_report(short.id, AgentState::Working, 1)).await;
    sleep(Duration::from_millis(400)).await;
    report(&t.client, main_report(short.id, AgentState::Idle, 2)).await;
    // The long ones end: a main agent reporting idle with its outcome `completed`, a completed
    // release, a failed release, and a plain idle (a normal stop as the Stop hooks report it).
    let mut done = sub_report(finished.id, AgentState::Idle, 2, None);
    done.node = done.node.map(|node| NodeReport {
        id: "root".into(),
        kind: NodeKind::Main,
        name: None,
        outcome: Some(NodeOutcome::Completed),
        ..node
    });
    report(&t.client, done).await;
    report(&t.client, main_report(plain.id, AgentState::Idle, 2)).await;
    release(&t.client, released.id, 2, Some(NodeOutcome::Completed)).await;
    release(&t.client, failed.id, 2, Some(NodeOutcome::Failed)).await;

    let mut raised = Vec::new();
    for _ in 0..3 {
        raised.push(
            t.next_intent("an agent_finished intent", |i| {
                i.kind == IntentKind::AgentFinished
            })
            .await,
        );
    }
    let mut sessions: Vec<_> = raised.iter().map(|i| i.session_id).collect();
    sessions.sort();
    let mut expected = vec![finished.id, released.id, plain.id];
    expected.sort();
    assert_eq!(sessions, expected);
    for intent in &raised {
        let session = [&finished, &released, &plain]
            .into_iter()
            .find(|s| s.id == intent.session_id)
            .unwrap();
        assert_eq!(intent.title, format!("{}: fake finished", session.title));
        assert!(
            intent.body.starts_with("Finished after 1"),
            "{}",
            intent.body
        );
    }
    t.no_intent(Duration::from_millis(500)).await;
    let pending = t.intents().await;
    assert_eq!(pending.len(), 3);
    assert!(pending.iter().all(|i| i.session_id != failed.id));
    t.stop().await;
}

// ---- settings and expiry ----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn kinds_enabled_and_expiry_come_from_daemon_toml() {
    let mut t = Env::start(
        "[notify]\nkinds = [\"session_exited_with_error\"]\nexpire_secs = 1\n",
        None,
    )
    .await;
    // A blocked agent is a kind that is switched off.
    let agent = create(&t.client, idle_session()).await;
    report(
        &t.client,
        sub_report(agent.id, AgentState::Blocked, 1, None),
    )
    .await;
    let failing = create(&t.client, sh("exit 1")).await;
    let intent = t.next_intent("the exit intent", |_| true).await;
    assert_eq!(intent.session_id, failing.id, "only the enabled kind");
    let expires = intent.expires_at_ms.expect("non-approval intents expire");
    assert_eq!(expires, intent.created_at_ms + 1_000);
    assert_eq!(t.intents().await.len(), 1);

    sleep(Duration::from_millis(1200)).await;
    assert!(
        t.intents().await.is_empty(),
        "expired intents are not listed"
    );
    assert_eq!(
        remote_code(t.client.notify_ack(&intent.id).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(t.client.notify_act(&intent.id, ACTION_OPEN).await),
        ErrorCode::NotFound
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_notify_section_raises_nothing() {
    let mut t = Env::start("[notify]\nenabled = false\n", None).await;
    create(&t.client, sh("exit 5")).await;
    t.no_intent(Duration::from_millis(800)).await;
    assert!(t.intents().await.is_empty());
    // Focus reports are still accepted.
    t.client.client_focus(None).await.unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_notify_settings_refuse_to_start_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let paths = hutan_daemon::Paths::in_home(dir.path());
    std::fs::write(&paths.config_file, "[notify]\nkinds = [\"gossip\"]\n").unwrap();
    let error = hutan_daemon::start(hutan_daemon::DaemonConfig::new(paths))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("gossip"), "{error}");
}

// ---- notify.act open --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn acting_open_tells_the_desktops_to_show_the_session() {
    let mut t = Env::plain().await;
    let mut options = t.daemon.options();
    options.client.kind = ClientKind::Desktop;
    let desktop = Client::connect(options).await.unwrap();
    let mut desktop_events = desktop.events();
    desktop
        .call::<hutan_protocol::messages::Subscribed>(Request::EventsSubscribe(Empty {}))
        .await
        .unwrap();

    let failing = create(&t.client, sh("exit 2")).await;
    let intent = t.next_intent("the exit intent", |_| true).await;
    let acted = t.client.notify_act(&intent.id, ACTION_OPEN).await.unwrap();
    assert_eq!(acted.route, ActRoute::UiOpen);
    assert_eq!(acted.desktop_clients, 1, "the CLI does not count");
    let (session, workspace, at_ms) =
        wait_event(&mut desktop_events, "ui.open", |event| match event {
            Event::UiOpen {
                session_id,
                workspace_id,
                at_ms,
            } => Some((*session_id, *workspace_id, *at_ms)),
            _ => None,
        })
        .await;
    assert_eq!(session, failing.id);
    assert_eq!(workspace, Some(failing.workspace_id));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(
        now.saturating_sub(at_ms) < 10_000,
        "stamped when it was sent"
    );
    assert!(t.intents().await.is_empty(), "acting acknowledges");

    // Without a desktop the request still goes out and says nobody can show it.
    drop(desktop);
    timeout(WAIT, async {
        while t.daemon_clients().await > 1 {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // Subscription is dropped with the connection; let the daemon notice it.
    sleep(Duration::from_millis(100)).await;
    create(&t.client, sh("exit 4")).await;
    let second = t
        .next_intent("the second exit intent", |i| i.session_id != failing.id)
        .await;
    let acted = t.client.notify_act(&second.id, ACTION_OPEN).await.unwrap();
    assert_eq!(acted.desktop_clients, 0);
    t.stop().await;
}

// ---- the wire -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_advertises_the_capability_since_protocol_0_13() {
    let t = Env::plain().await;
    assert!(t.client.supports_notify());
    let raw: serde_json::Value = t
        .client
        .call(Request::DaemonStatus(Empty {}))
        .await
        .unwrap();
    // Later minors only add: the capability and the 0.13 messages stay.
    assert!(raw["protocol"]["minor"].as_u64() >= Some(13), "{raw}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_cut_short_by_the_tree_never_reaches_an_intent() {
    let mut t = Env::plain().await;
    let session = create(&t.client, idle_session()).await;
    report(&t.client, main_report(session.id, AgentState::Working, 1)).await;
    // The tree keeps 200 characters of an activity: this one loses the closing quote (and most
    // of the key) before the daemon builds the intent.
    let secret = "Zq9xK".repeat(60);
    let activity = format!("calling the API with api_key=\"{secret}\" and retrying");
    report(
        &t.client,
        sub_report(session.id, AgentState::Blocked, 2, Some(&activity)),
    )
    .await;
    let kept = {
        let tree: hutan_protocol::tree::AgentTree = t
            .client
            .call(Request::AgentTree(hutan_protocol::tree::AgentTreeRequest {
                session_id: Some(session.id),
            }))
            .await
            .unwrap();
        tree.observations[0]
            .nodes
            .values()
            .find_map(|n| n.activity.clone())
            .expect("the blocked node's activity")
    };
    assert!(
        kept.contains("Zq9xK") && !kept.contains("retrying"),
        "the fixture must be cut inside the quoted key: {kept}"
    );

    let intent = t
        .next_intent("the blocked intent", |i| i.kind == IntentKind::AgentBlocked)
        .await;
    assert!(!intent.body.contains("Zq9xK"), "{}", intent.body);
    assert!(!intent.title.contains("Zq9xK"), "{}", intent.title);
    assert!(
        intent.body.contains("api_key=[REDACTED]"),
        "{}",
        intent.body
    );
    assert!(intent.body.starts_with("calling the API with"));
    t.stop().await;
}

// ---- native notifications (M6-B): the daemon posts, clicks route like notify.act ---------------

fn decided_by(t: &Env, session: SessionId) -> Option<String> {
    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    db.query_row(
        "SELECT decided_by FROM approvals WHERE session_id = ?1",
        [session.to_string()],
        |row| row.get(0),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_is_posted_natively_and_a_click_on_an_option_answers_it_as_the_notifier() {
    let poster = FakePoster::new(Authorization::Grant);
    let mut t = Env::start_with_poster("", Some("approval-touch"), Some(poster.clone())).await;
    assert!(
        poster.calls().is_empty(),
        "nothing is asked before the first intent"
    );
    let created = t
        .chat(Some(ApprovalPolicy::Untrusted), Some(SandboxMode::ReadOnly))
        .await;
    t.send(
        created.id,
        "Run `touch hi.txt` in a shell, then reply with the word done.",
    )
    .await;
    let intent = t
        .next_intent("the approval intent", |i| {
            i.kind == IntentKind::ApprovalRequired
        })
        .await;

    poster
        .wait_for("the post", |calls| {
            calls.iter().any(|c| matches!(c, Call::Post(_)))
        })
        .await;
    assert_eq!(poster.authorize_calls(), 1);
    let posted = poster.posted();
    assert_eq!(posted.len(), 1);
    let n = &posted[0];
    assert_eq!(n.identifier, intent.id);
    assert_eq!(n.thread, created.id.to_string());
    assert_eq!(
        (n.title.as_str(), n.body.as_str()),
        (intent.title.as_str(), intent.body.as_str())
    );
    assert!(
        n.category.starts_with("hutan.approval_required."),
        "{}",
        n.category
    );
    let actions: Vec<(&str, bool, bool)> = n
        .actions
        .iter()
        .map(|a| (a.id.as_str(), a.destructive, a.foreground))
        .collect();
    assert_eq!(
        actions,
        [
            ("accept", false, false),
            ("acceptWithExecpolicyAmendment", false, false),
            ("cancel", true, false),
            (OPEN_ACTION_ID, false, true),
        ],
        "the options, the refusing one destructive, then Open"
    );

    poster.click(&intent.id, ResponseAction::Action("accept".to_owned()));
    let (outcome, who) = wait_event(&mut t.events, "approval.resolved", |event| match event {
        Event::ApprovalResolved {
            outcome,
            decided_by,
            ..
        } => Some((outcome.clone(), decided_by.clone())),
        _ => None,
    })
    .await;
    assert_eq!(
        outcome,
        PermissionOutcome::Selected {
            option_id: "accept".into()
        }
    );
    assert_eq!(
        who.map(|d| d.client_kind).as_deref(),
        Some("notifier"),
        "the event says a notification answered"
    );
    t.turn_end(created.id).await;
    assert_eq!(decided_by(&t, created.id).as_deref(), Some("notifier"));
    poster
        .wait_for("the removal", |calls| {
            calls.contains(&Call::Remove(intent.id.clone()))
        })
        .await;
    let received: Vec<serde_json::Value> =
        std::fs::read_to_string(t.work.path().join("received.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["msg"].clone())
            .filter(|msg| msg["method"].is_null())
            .collect();
    assert_eq!(received.len(), 1, "{received:?}");
    assert_eq!(
        received[0]["result"]["decision"], "accept",
        "the agent got the chosen decision"
    );
    assert_eq!(
        poster.authorize_calls(),
        1,
        "authorization is asked once per run"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_default_action_opens_the_session_and_leaves_the_approval_pending() {
    let poster = FakePoster::new(Authorization::Grant);
    let mut t = Env::start_with_poster("", Some("approval-touch"), Some(poster.clone())).await;
    let mut options = t.daemon.options();
    options.client.kind = ClientKind::Desktop;
    let desktop = Client::connect(options).await.unwrap();
    let mut desktop_events = desktop.events();
    desktop
        .call::<hutan_protocol::messages::Subscribed>(Request::EventsSubscribe(Empty {}))
        .await
        .unwrap();

    let created = t.chat(Some(ApprovalPolicy::Untrusted), None).await;
    t.send(created.id, "Run `touch hi.txt`").await;
    let intent = t
        .next_intent("the approval intent", |i| {
            i.kind == IntentKind::ApprovalRequired
        })
        .await;
    poster.click(
        &intent.id,
        hutan_daemon::notify_native::response_action(
            hutan_daemon::notify_native::SYSTEM_DEFAULT_ACTION,
        ),
    );
    let (session, workspace) = wait_event(&mut desktop_events, "ui.open", |event| match event {
        Event::UiOpen {
            session_id,
            workspace_id,
            ..
        } => Some((*session_id, *workspace_id)),
        _ => None,
    })
    .await;
    assert_eq!(session, created.id);
    assert_eq!(workspace, Some(created.workspace_id));
    assert_eq!(
        t.client.approvals(None).await.unwrap().len(),
        1,
        "still pending"
    );
    assert_eq!(
        t.intents().await,
        vec![intent.clone()],
        "to be answered where it opened"
    );
    assert!(
        !poster.removed().contains(&intent.id),
        "an unanswered approval keeps its notification"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn open_and_the_default_action_of_other_kinds_publish_ui_open_and_dismiss_acknowledges() {
    let poster = FakePoster::new(Authorization::Grant);
    let mut t = Env::start_with_poster("", None, Some(poster.clone())).await;
    let mut options = t.daemon.options();
    options.client.kind = ClientKind::Desktop;
    let desktop = Client::connect(options).await.unwrap();
    let mut desktop_events = desktop.events();
    desktop
        .call::<hutan_protocol::messages::Subscribed>(Request::EventsSubscribe(Empty {}))
        .await
        .unwrap();

    let first = create(&t.client, sh("exit 2")).await;
    let open = t
        .next_intent("the first exit intent", |i| i.session_id == first.id)
        .await;
    let calls = poster
        .wait_for("the post", |calls| {
            calls.iter().any(|c| matches!(c, Call::Post(_)))
        })
        .await;
    let Some(Call::Post(n)) = calls.iter().find(|c| matches!(c, Call::Post(_))) else {
        unreachable!()
    };
    assert_eq!(n.category, "hutan.session_exited_with_error");
    assert_eq!(n.actions.len(), 1);
    assert_eq!(n.actions[0].id, OPEN_ACTION_ID);

    poster.click(&open.id, ResponseAction::Open);
    let shown = wait_event(&mut desktop_events, "ui.open", |event| match event {
        Event::UiOpen { session_id, .. } => Some(*session_id),
        _ => None,
    })
    .await;
    assert_eq!(shown, first.id);
    poster
        .wait_for("the removal", |calls| {
            calls.contains(&Call::Remove(open.id.clone()))
        })
        .await;
    assert!(t.intents().await.is_empty(), "open acknowledges");

    // Dismissing (swiping away) is an acknowledgement; the session is not opened.
    let second = create(&t.client, sh("exit 3")).await;
    let dismissed = t
        .next_intent("the second exit intent", |i| i.session_id == second.id)
        .await;
    poster.click(&dismissed.id, ResponseAction::Dismiss);
    timeout(WAIT, async {
        while !t.intents().await.is_empty() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the dismissed intent is acknowledged");
    // A dismissal and a click on an intent that is gone are ignored, not errors.
    poster.click(&dismissed.id, ResponseAction::Open);
    poster.click(
        "no-such-intent",
        ResponseAction::Action("accept".to_owned()),
    );
    sleep(Duration::from_millis(200)).await;
    assert!(t.intents().await.is_empty());
    assert!(
        desktop_events
            .try_recv()
            .map_or(true, |(_, e)| !matches!(e, Event::UiOpen { .. })),
        "the late Open opened nothing"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_notification_is_removed_when_its_intent_is_acknowledged_expires_or_resolves() {
    let poster = FakePoster::new(Authorization::Grant);
    let mut t = Env::start_with_poster(
        "[notify]\nexpire_secs = 1\n",
        Some("approval-touch"),
        Some(poster.clone()),
    )
    .await;

    // Acknowledged: `notify.ack` (anyone, here the CLI) takes the notification down.
    let failing = create(&t.client, sh("exit 1")).await;
    let acked = t
        .next_intent("the exit intent", |i| i.session_id == failing.id)
        .await;
    t.client.notify_ack(&acked.id).await.unwrap();
    poster
        .wait_for("the removal after ack", |calls| {
            calls.contains(&Call::Remove(acked.id.clone()))
        })
        .await;

    // Expired: nobody touches the intent; the daemon sweeps it.
    let expiring = create(&t.client, sh("exit 4")).await;
    let lapsed = t
        .next_intent("the second exit intent", |i| i.session_id == expiring.id)
        .await;
    poster
        .wait_for("the removal after expiry", |calls| {
            calls.contains(&Call::Remove(lapsed.id.clone()))
        })
        .await;
    assert!(t.intents().await.is_empty());

    // Resolved elsewhere: an approval answered by `approval.respond`.
    let created = t.chat(Some(ApprovalPolicy::Untrusted), None).await;
    t.send(created.id, "Run `touch hi.txt`").await;
    let approval_intent = t
        .next_intent("the approval intent", |i| {
            i.kind == IntentKind::ApprovalRequired
        })
        .await;
    let approval = t.client.approvals(None).await.unwrap().remove(0);
    assert!(!poster.removed().contains(&approval_intent.id));
    t.client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval.id,
            option_id: "accept".to_owned(),
        }))
        .await
        .unwrap();
    poster
        .wait_for("the removal after resolution", |calls| {
            calls.contains(&Call::Remove(approval_intent.id.clone()))
        })
        .await;
    assert_eq!(
        decided_by(&t, created.id).as_deref(),
        Some("cli"),
        "not the notifier"
    );

    // Acted on through `notify.act`: the other route of an answer.
    let failing = create(&t.client, sh("exit 6")).await;
    let acted = t
        .next_intent("the third exit intent", |i| i.session_id == failing.id)
        .await;
    t.client.notify_act(&acted.id, ACTION_OPEN).await.unwrap();
    poster
        .wait_for("the removal after act", |calls| {
            calls.contains(&Call::Remove(acted.id.clone()))
        })
        .await;
    assert_eq!(poster.authorize_calls(), 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denied_permission_stops_posting_but_intents_stay_listed() {
    let poster = FakePoster::new(Authorization::Deny);
    let mut t = Env::start_with_poster("", None, Some(poster.clone())).await;
    let first = create(&t.client, sh("exit 1")).await;
    let first = t
        .next_intent("the first intent", |i| i.session_id == first.id)
        .await;
    poster
        .wait_for("the authorization request", |calls| {
            calls.contains(&Call::Authorize)
        })
        .await;
    let second = create(&t.client, sh("exit 2")).await;
    let second = t
        .next_intent("the second intent", |i| i.session_id == second.id)
        .await;
    t.client.notify_ack(&first.id).await.unwrap();
    // Nothing else reaches the poster: the intents are listed, but none is posted or removed.
    sleep(Duration::from_millis(300)).await;
    assert_eq!(
        poster.calls(),
        [Call::Authorize],
        "asked once, then silence"
    );
    assert_eq!(t.intents().await, vec![second]);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_false_posts_nothing_and_does_not_even_ask() {
    let poster = FakePoster::new(Authorization::Grant);
    let mut t =
        Env::start_with_poster("[notify]\nnative = false\n", None, Some(poster.clone())).await;
    let failing = create(&t.client, sh("exit 1")).await;
    let intent = t
        .next_intent("the intent", |i| i.session_id == failing.id)
        .await;
    t.client.notify_ack(&intent.id).await.unwrap();
    sleep(Duration::from_millis(300)).await;
    assert!(poster.calls().is_empty(), "{:?}", poster.calls());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_without_a_poster_still_raises_intents() {
    // The shape of a `cargo run` daemon or one that is not in Hutan.app.
    let mut t = Env::start_with_poster("", None, None).await;
    let failing = create(&t.client, sh("exit 1")).await;
    let intent = t
        .next_intent("the intent", |i| i.session_id == failing.id)
        .await;
    assert_eq!(t.intents().await, vec![intent]);
    t.stop().await;
}

// ---- notify.mute, focus and sound (protocol 0.34) ----------------------------------------------

impl Env {
    /// `session` as `session.list` reports it now.
    async fn session(&self, session: SessionId) -> SessionInfo {
        self.client
            .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap()
            .sessions
            .into_iter()
            .find(|s| s.id == session)
            .expect("the session is listed")
    }

    /// An idle session whose (reported) agent is now blocked on its subagent.
    async fn blocked_session(&self) -> SessionInfo {
        let session = create(&self.client, idle_session()).await;
        report(
            &self.client,
            main_report(session.id, AgentState::Working, 1),
        )
        .await;
        report(
            &self.client,
            sub_report(
                session.id,
                AgentState::Blocked,
                2,
                Some("wants to run tests"),
            ),
        )
        .await;
        session
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_muted_session_raises_nothing_keeps_needing_the_user_and_stays_muted_after_a_restart() {
    let poster = FakePoster::new(Authorization::Grant);
    let mut t = Env::start_with_poster("", None, Some(poster.clone())).await;
    assert!(t.client.supports_notify_mute());

    // A session with something pending is muted: the intent and its banner go.
    let noisy = t.blocked_session().await;
    let raised = t
        .next_intent("the blocked intent", |i| i.session_id == noisy.id)
        .await;
    let posted = poster
        .wait_for("the post", |calls| {
            calls
                .iter()
                .any(|c| matches!(c, Call::Post(n) if n.identifier == raised.id))
        })
        .await;
    assert!(
        posted.iter().any(|c| matches!(c, Call::Post(n) if n.sound)),
        "notify.sound defaults to the system's sound"
    );
    let muted = t.client.notify_mute(noisy.id, true).await.unwrap();
    assert!(muted.muted);
    wait_event(
        &mut t.events,
        "the muted session's update",
        |event| match event {
            Event::SessionUpdated { session } if session.id == noisy.id && session.muted => {
                Some(())
            }
            _ => None,
        },
    )
    .await;
    poster
        .wait_for("the removal after the mute", |calls| {
            calls.contains(&Call::Remove(raised.id.clone()))
        })
        .await;
    assert!(t.intents().await.is_empty());

    // A muted session raises nothing; the next session's intent is the first to arrive.
    let quiet = create(&t.client, idle_session()).await;
    t.client.notify_mute(quiet.id, true).await.unwrap();
    report(&t.client, main_report(quiet.id, AgentState::Working, 1)).await;
    report(&t.client, main_report(quiet.id, AgentState::Blocked, 2)).await;
    let loud = t.blocked_session().await;
    let next = t.next_intent("the next intent", |_| true).await;
    assert_eq!(
        next.session_id, loud.id,
        "the muted session raised {next:?}"
    );
    assert_eq!(
        t.intents()
            .await
            .iter()
            .map(|i| i.session_id)
            .collect::<Vec<_>>(),
        [loud.id]
    );
    // It still needs the user: its agent is reported blocked, which is what attention counts.
    let listed = t.session(quiet.id).await;
    assert!(listed.muted);
    assert_eq!(
        listed.agent.as_ref().map(|a| a.state),
        Some(AgentState::Blocked)
    );

    // Unmuting is told the same way; an unknown session is `not_found`.
    let unmuted = t.client.notify_mute(noisy.id, false).await.unwrap();
    assert!(!unmuted.muted);
    assert_eq!(
        remote_code(t.client.notify_mute(SessionId::new(), true).await),
        ErrorCode::NotFound
    );

    // The choice survives a daemon restart.
    let Env {
        daemon,
        work: _work,
        client,
        events,
    } = t;
    drop((client, events));
    let daemon = daemon.restart().await;
    let client = daemon.connect().await;
    let sessions = client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap()
        .sessions;
    let muted_now = |id| sessions.iter().find(|s| s.id == id).map(|s| s.muted);
    assert_eq!(muted_now(quiet.id), Some(true));
    assert_eq!(muted_now(noisy.id), Some(false));
    assert_eq!(muted_now(loud.id), Some(false));
    drop(client);
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn focusing_a_session_withdraws_its_intents_and_a_focused_session_raises_none() {
    let poster = FakePoster::new(Authorization::Grant);
    let mut t = Env::start_with_poster("", None, Some(poster.clone())).await;
    let looked_at = t.blocked_session().await;
    let raised = t
        .next_intent("the blocked intent", |i| i.session_id == looked_at.id)
        .await;

    // A window shows the session: its intent and its banner go.
    let window = t.daemon.connect_as(ClientKind::Desktop).await;
    window.client_focus(Some(looked_at.id)).await.unwrap();
    poster
        .wait_for("the removal after the focus", |calls| {
            calls.contains(&Call::Remove(raised.id.clone()))
        })
        .await;
    assert!(t.intents().await.is_empty());

    // While a session is focused, nothing it does raises an intent, of any kind.
    let focused = create(&t.client, idle_session()).await;
    window.client_focus(Some(focused.id)).await.unwrap();
    report(&t.client, main_report(focused.id, AgentState::Working, 1)).await;
    report(&t.client, main_report(focused.id, AgentState::Blocked, 2)).await;
    let other = t.blocked_session().await;
    let next = t.next_intent("the next intent", |_| true).await;
    assert_eq!(
        next.session_id, other.id,
        "the focused session raised {next:?}"
    );
    drop(window);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_sound_none_posts_silent_notifications() {
    let poster = FakePoster::new(Authorization::Grant);
    let mut t =
        Env::start_with_poster("[notify]\nsound = \"none\"\n", None, Some(poster.clone())).await;
    let failing = create(&t.client, sh("exit 1")).await;
    let intent = t
        .next_intent("the intent", |i| i.session_id == failing.id)
        .await;
    let calls = poster
        .wait_for("the post", |calls| {
            calls
                .iter()
                .any(|c| matches!(c, Call::Post(n) if n.identifier == intent.id))
        })
        .await;
    for call in calls {
        if let Call::Post(notification) = call {
            assert!(!notification.sound, "{notification:?}");
        }
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_or_failure_plays_the_sound_and_finished_work_is_a_silent_banner() {
    let poster = FakePoster::new(Authorization::Grant);
    let mut t = Env::start_with_poster("", Some("plain-turn"), Some(poster.clone())).await;
    let chat = t.chat(None, None).await;
    t.send(chat.id, "Say hi").await;
    let finished = t
        .next_intent("the finished intent", |i| {
            i.kind == IntentKind::ChatTurnFinished
        })
        .await;
    let failing = create(&t.client, sh("exit 1")).await;
    let failed = t
        .next_intent("the failed intent", |i| i.session_id == failing.id)
        .await;
    let calls = poster
        .wait_for("both posts", |calls| {
            [&finished.id, &failed.id].iter().all(|id| {
                calls
                    .iter()
                    .any(|c| matches!(c, Call::Post(n) if &n.identifier == *id))
            })
        })
        .await;
    for call in calls {
        if let Call::Post(notification) = call {
            if notification.identifier == finished.id {
                assert!(!notification.sound, "finished work is silent");
            } else if notification.identifier == failed.id {
                assert!(notification.sound, "a failure plays the sound");
            }
        }
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_finished_false_raises_nothing_for_finished_work_but_still_for_failures() {
    let mut t = Env::start("[notify]\nfinished = false\n", Some("plain-turn")).await;
    let chat = t.chat(None, None).await;
    t.send(chat.id, "Say hi").await;
    t.turn_end(chat.id).await;
    let failing = create(&t.client, sh("exit 1")).await;
    let failed = t
        .next_intent("the failed intent", |i| i.session_id == failing.id)
        .await;
    assert_eq!(failed.kind, IntentKind::SessionExitedWithError);
    let pending = t.intents().await;
    assert_eq!(pending.len(), 1, "{pending:?}");
    t.stop().await;
}
