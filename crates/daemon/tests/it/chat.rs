//! Chat sessions end to end: a real daemon on a temporary `HUTAN_HOME`, real client
//! connections, and `hutan-codex-fixture replay` standing in for `codex app-server` with
//! transcripts recorded from codex 0.162.1.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, create, remote_code, spec, stop, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::agent::{DecidedBy, PermissionOutcome};
use hutan_core::question::{QuestionResponse, Values};
use hutan_core::{
    AgentEvent, AgentEventKind, ApprovalId, ApprovalPolicy, QuestionId, QuestionOutcome,
    SandboxMode, SessionId,
};
use hutan_core::{Seq, SessionState};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{
    ApprovalInfo, ApprovalList, ApprovalListRequest, ApprovalRespond, ChatCreate, ChatFork,
    ChatForked, ChatHistory, ChatHistoryRequest, ChatModels, ChatModelsRequest, ChatSend, ChatSent,
    ForkWhere, QuestionAnswer, QuestionInfo, QuestionList, QuestionListRequest,
};
use hutan_protocol::messages::{
    ClientKind, Empty, Event, Request, SessionInfo, SessionList, SessionListFilter, SessionRef,
    SessionResult,
};
use hutan_protocol::notify::{ACTION_OPEN, IntentKind, NotifyList};
use hutan_protocol::recovery::{DaemonStop, LastTurn, Lost, RecoveryKind};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

const FIXTURE_BIN_NAME: &str = "hutan-codex-fixture";

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
        .join(FIXTURE_BIN_NAME);
    assert!(
        bin.is_file(),
        "{FIXTURE_BIN_NAME} not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

/// A codex that replays `transcript` (a file, or a directory of transcripts used one per
/// spawn), recording what the client sends into `received`.
fn replay(transcript: &Path, received: &Path) -> CodexConfig {
    CodexConfig {
        program: fixture_bin(),
        args: vec![
            "replay".to_owned(),
            "--received".to_owned(),
            received.display().to_string(),
            transcript.display().to_string(),
            "app-server".to_owned(),
        ],
        env: Vec::new(),
    }
}

struct Chat {
    daemon: TestDaemon,
    /// Where the agent works; also holds the scripted transcripts and the received log.
    work: tempfile::TempDir,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
}

impl Chat {
    /// A daemon whose codex replays the committed fixture `name`.
    async fn with_fixture(name: &str) -> Self {
        let transcript = fixtures().join(format!("{name}.jsonl"));
        Self::with_transcripts(&transcript, |_| {}).await
    }

    /// A daemon whose codex replays `transcript`; `prepare` may write files into the work dir
    /// first.
    async fn with_transcripts(transcript: &Path, prepare: impl FnOnce(&Path)) -> Self {
        let work = tempfile::tempdir().unwrap();
        prepare(work.path());
        let config = replay(transcript, &work.path().join("received.jsonl"));
        let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    fn received_path(&self) -> PathBuf {
        self.work.path().join("received.jsonl")
    }

    /// Everything the fake server received, as `(method or "response", message)`.
    fn received(&self) -> Vec<(String, Value)> {
        received_in(&self.received_path())
    }

    /// What the fake server received of one method.
    fn received_of(&self, method: &str) -> Vec<Value> {
        self.received()
            .into_iter()
            .filter(|(name, _)| name == method)
            .map(|(_, msg)| msg)
            .collect()
    }

    fn cwd(&self) -> PathBuf {
        // The agent's directory: the (canonical) work dir itself.
        self.work.path().to_owned()
    }

    async fn create(&self) -> SessionInfo {
        self.create_with(None, None, None).await
    }

    async fn create_with(
        &self,
        model: Option<&str>,
        policy: Option<ApprovalPolicy>,
        sandbox: Option<SandboxMode>,
    ) -> SessionInfo {
        self.client
            .call::<SessionResult>(Request::ChatCreate(ChatCreate {
                extra_dirs: Vec::new(),
                contain: None,
                workspace_id: None,
                cwd: self.cwd(),
                agent: "codex".to_owned(),
                model: model.map(str::to_owned),
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

    async fn send(&self, id: SessionId, text: &str) -> ChatSent {
        self.client
            .call(Request::ChatSend(ChatSend {
                session_id: id,
                text: text.to_owned(),
                attachments: vec![],
            }))
            .await
            .unwrap()
    }

    async fn history(&self, id: SessionId, before: Option<u64>, limit: u32) -> ChatHistory {
        self.client
            .call(Request::ChatHistory(ChatHistoryRequest {
                session_id: id,
                before_seq: before,
                limit,
            }))
            .await
            .unwrap()
    }

    async fn approvals(&self, client: &Client) -> Vec<ApprovalInfo> {
        client
            .call::<ApprovalList>(Request::ApprovalList(ApprovalListRequest {
                session_id: None,
            }))
            .await
            .unwrap()
            .approvals
    }

    async fn session(&self, id: SessionId) -> SessionInfo {
        self.client
            .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap()
            .sessions
            .into_iter()
            .find(|s| s.id == id)
            .expect("session listed")
    }

    /// Polls until the session is in `state`.
    async fn wait_state(&self, id: SessionId, state: SessionState) -> SessionInfo {
        timeout(WAIT, async {
            loop {
                let session = self.session(id).await;
                if session.state == state {
                    return session;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("session never reached {state:?}"))
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

fn received_in(path: &Path) -> Vec<(String, Value)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let wrapper: Value = serde_json::from_str(line).unwrap();
            let msg = wrapper["msg"].clone();
            (msg["method"].as_str().unwrap_or("response").to_owned(), msg)
        })
        .collect()
}

async fn next_chat_event(
    events: &mut broadcast::Receiver<(Seq, Event)>,
    what: &str,
    mut pick: impl FnMut(&AgentEvent) -> bool,
) -> AgentEvent {
    wait_event(events, what, |event| match event {
        Event::ChatEvent { event, .. } if pick(event) => Some((**event).clone()),
        _ => None,
    })
    .await
}

fn is_turn_end(event: &AgentEvent) -> bool {
    event.kind.ends_turn()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_turn_streams_journals_and_returns_the_session_to_idle() {
    let mut t = Chat::with_fixture("plain-turn").await;
    let created = t.create().await;
    assert_eq!(created.kind, hutan_core::SessionKind::Harness);
    assert_eq!(created.state, SessionState::Idle);
    assert_eq!(created.terminal_id, None);
    let chat = created.chat.clone().expect("chat info");
    assert_eq!(chat.agent, "codex");
    assert_eq!(
        chat.model, "gpt-6.1-sol",
        "the catalog's default, not a config default"
    );
    assert_eq!(chat.approval_policy, ApprovalPolicy::OnRequest);
    assert_eq!(chat.sandbox, SandboxMode::WorkspaceWrite);
    assert!(!chat.turn_active);

    // Workspace of the session: Home (the work dir is under no project).
    let sent = t
        .send(
            created.id,
            "Reply with the single word pong and nothing else.",
        )
        .await;

    let first = next_chat_event(&mut t.events, "turn_started", |e| {
        matches!(e.kind, AgentEventKind::TurnStarted)
    })
    .await;
    assert_eq!(first.turn_id, Some(sent.turn_id));
    assert_eq!(first.session_id, created.id);
    let mut seqs = vec![first.seq];
    let mut text = String::new();
    loop {
        let event = next_chat_event(&mut t.events, "the turn to end", |_| true).await;
        assert!(
            event.seq > *seqs.last().unwrap(),
            "seq increases per session"
        );
        seqs.push(event.seq);
        assert_eq!(event.delivery, hutan_core::Delivery::Live);
        if let AgentEventKind::MessageDelta { text: delta } = &event.kind {
            text.push_str(delta);
        }
        if is_turn_end(&event) {
            assert!(matches!(event.kind, AgentEventKind::TurnCompleted { .. }));
            break;
        }
    }
    assert_eq!(text, "pong");
    let idle = t.wait_state(created.id, SessionState::Idle).await;
    assert!(!idle.chat.unwrap().turn_active);

    // The journal has everything but the deltas, oldest first, marked as replay.
    let history = t.history(created.id, None, 200).await;
    let kinds: Vec<_> = history.events.iter().map(|e| e.kind.name()).collect();
    assert!(!kinds.contains(&"message_delta"), "{kinds:?}");
    for expected in [
        "resume_locator",
        "turn_started",
        "message_start",
        "message_end",
        "usage",
        "turn_completed",
    ] {
        assert!(kinds.contains(&expected), "{expected} in {kinds:?}");
    }
    assert!(
        history
            .events
            .iter()
            .all(|e| e.delivery == hutan_core::Delivery::Replay)
    );
    let journal_seqs: Vec<u64> = history.events.iter().map(|e| e.seq).collect();
    assert!(
        journal_seqs.windows(2).all(|w| w[0] < w[1]),
        "{journal_seqs:?}"
    );
    assert!(!history.has_more);
    assert_eq!(history.last_seq, *seqs.last().unwrap());
    assert!(
        journal_seqs.len() < seqs.len() + 1,
        "deltas used seqs the journal does not have"
    );
    let answer = history
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::MessageEnd {
                role: hutan_core::agent::MessageRole::Assistant,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .next_back();
    assert_eq!(answer, Some("pong"));

    // Paging backwards: the newest two, then the two before them.
    let newest = t.history(created.id, None, 2).await;
    assert_eq!(newest.events.len(), 2);
    assert!(newest.has_more);
    assert_eq!(
        newest.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        journal_seqs[journal_seqs.len() - 2..]
    );
    let older = t.history(created.id, Some(newest.events[0].seq), 2).await;
    assert_eq!(
        older.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        journal_seqs[journal_seqs.len() - 4..journal_seqs.len() - 2]
    );

    // What the provider was asked for.
    let received = t.received();
    let start = received.iter().find(|(m, _)| m == "thread/start").unwrap();
    assert_eq!(start.1["params"]["approvalPolicy"], "on-request");
    assert_eq!(start.1["params"]["sandbox"], "workspace-write");
    assert_eq!(start.1["params"]["approvalsReviewer"], "user");
    let cwd = std::fs::canonicalize(t.work.path()).unwrap();
    assert_eq!(start.1["params"]["cwd"], cwd.to_str().unwrap());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_is_created_with_the_requested_model_policy_and_sandbox() {
    let t = Chat::with_fixture("plain-turn").await;
    let created = t
        .create_with(
            Some("gpt-5.6-luna"),
            Some(ApprovalPolicy::Untrusted),
            Some(SandboxMode::ReadOnly),
        )
        .await;
    let chat = created.chat.unwrap();
    assert_eq!(
        (chat.model.as_str(), chat.approval_policy, chat.sandbox),
        (
            "gpt-5.6-luna",
            ApprovalPolicy::Untrusted,
            SandboxMode::ReadOnly
        )
    );
    let start = t
        .received()
        .into_iter()
        .find(|(m, _)| m == "thread/start")
        .unwrap();
    assert_eq!(start.1["params"]["model"], "gpt-5.6-luna");
    assert_eq!(start.1["params"]["approvalPolicy"], "untrusted");
    assert_eq!(start.1["params"]["sandbox"], "read-only");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_model_is_refused_with_the_catalog() {
    let t = Chat::with_fixture("plain-turn").await;
    let error = t
        .client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: None,
            cwd: t.cwd(),
            agent: "codex".to_owned(),
            model: Some("gpt-nope".to_owned()),
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        }))
        .await
        .unwrap_err();
    let hutan_client::ClientError::Remote(error) = error else {
        panic!("{error:?}")
    };
    assert_eq!(error.code, ErrorCode::ProviderError);
    assert!(
        error.message.contains("gpt-nope") && error.message.contains("gpt-6.1-sol"),
        "{}",
        error.message
    );
    // The session exists, failed, so the attempt is visible.
    let sessions = t
        .client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap()
        .sessions;
    assert!(
        matches!(sessions[0].state, SessionState::Failed { .. }),
        "{sessions:?}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn approvals_are_pending_in_the_daemon_and_answered_only_with_offered_options() {
    let mut t = Chat::with_fixture("approval-touch").await;
    let created = t
        .create_with(
            None,
            Some(ApprovalPolicy::Untrusted),
            Some(SandboxMode::ReadOnly),
        )
        .await;
    t.send(
        created.id,
        "Run `touch hi.txt` in a shell, then reply with the word done.",
    )
    .await;

    let info = wait_event(&mut t.events, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some((**approval).clone()),
        _ => None,
    })
    .await;
    assert_eq!(info.session_id, created.id);
    assert!(info.title.contains("touch hi.txt"), "{}", info.title);
    let offered: Vec<_> = info.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(
        offered,
        ["accept", "acceptWithExecpolicyAmendment", "cancel"]
    );

    // The session says it waits; the approval is listed.
    let waiting = t
        .wait_state(created.id, SessionState::WaitingApproval)
        .await;
    assert_eq!(waiting.chat.unwrap().pending_approvals, 1);
    assert_eq!(t.approvals(&t.client).await, std::slice::from_ref(&info));

    // A second client sees it too; the first one going away changes nothing.
    let other = t.daemon.connect().await;
    let (reconnected, events) = t.daemon.connect_subscribed().await;
    drop(std::mem::replace(&mut t.client, reconnected));
    t.events = events;
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        t.approvals(&other).await,
        std::slice::from_ref(&info),
        "survives a client disconnect"
    );
    assert_eq!(t.approvals(&t.client).await.len(), 1);

    // Only offered options can be chosen; nothing reaches the provider otherwise.
    for bad in ["decline", "acceptForSession", "", "ACCEPT"] {
        let result = t
            .client
            .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
                approval_id: info.id,
                option_id: bad.to_owned(),
            }))
            .await;
        assert_eq!(remote_code(result), ErrorCode::OptionNotOffered, "{bad:?}");
    }
    let unknown = t
        .client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: ApprovalId::new(),
            option_id: "accept".to_owned(),
        }))
        .await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    assert!(t.received().iter().all(|(m, _)| m != "response"));
    assert_eq!(
        t.approvals(&t.client).await.len(),
        1,
        "refusals leave it pending"
    );

    // Answered by the second client; everyone is told.
    let mut other_events = {
        let events = other.events();
        other
            .call::<hutan_protocol::messages::Subscribed>(Request::EventsSubscribe(Empty {}))
            .await
            .unwrap();
        events
    };
    other
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: info.id,
            option_id: "accept".to_owned(),
        }))
        .await
        .unwrap();
    let (outcome, decided_by) =
        wait_event(&mut t.events, "approval.resolved", |event| match event {
            Event::ApprovalResolved {
                approval_id,
                outcome,
                decided_by,
                ..
            } if *approval_id == info.id => Some((outcome.clone(), decided_by.clone())),
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
        decided_by,
        Some(DecidedBy {
            client_kind: "cli".into(),
            client_name: None
        }),
        "answered over a CLI connection"
    );
    wait_event(
        &mut other_events,
        "approval.resolved on the other client",
        |event| matches!(event, Event::ApprovalResolved { .. }).then_some(()),
    )
    .await;

    let mut done = false;
    let mut reply = String::new();
    while !done {
        let event = next_chat_event(&mut t.events, "the turn to end", |_| true).await;
        if let AgentEventKind::MessageDelta { text } = &event.kind {
            reply.push_str(text);
        }
        done = is_turn_end(&event);
    }
    assert!(reply.ends_with("done"), "{reply}");
    t.wait_state(created.id, SessionState::Idle).await;
    assert!(t.approvals(&t.client).await.is_empty());

    // The provider received exactly the chosen decision, for its own request.
    let received = t.received();
    let response = received.iter().find(|(m, _)| m == "response").unwrap();
    assert_eq!(response.1["result"]["decision"], "accept");
    assert_eq!(received.iter().filter(|(m, _)| m == "response").count(), 1);

    // Answering again is too late.
    let again = t
        .client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: info.id,
            option_id: "accept".to_owned(),
        }))
        .await;
    assert_eq!(remote_code(again), ErrorCode::NotFound);

    // The audit trail: the request and who decided what.
    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let row: (String, String, String, Option<String>, Option<String>, Option<String>) = db
        .query_row(
            "SELECT kind, title, session_id, outcome, option_id, decided_by FROM approvals WHERE id = ?1",
            [info.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();
    assert_eq!(row.0, "command");
    assert!(row.1.contains("touch hi.txt"));
    assert_eq!(row.2, created.id.to_string());
    assert_eq!(
        (row.3.as_deref(), row.4.as_deref(), row.5.as_deref()),
        (Some("selected"), Some("accept"), Some("cli"))
    );
    // The journal says it too, so that a client opened later can show it.
    assert_eq!(
        resolved_in_history(&t, created.id, info.id).await,
        Some(DecidedBy {
            client_kind: "cli".into(),
            client_name: None
        })
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_approval_expires_when_its_turn_ends() {
    let mut t = Chat::with_fixture("approval-interrupt").await;
    let created = t.create().await;
    t.send(created.id, "Run `touch hi.txt`").await;
    let info = wait_event(&mut t.events, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some((**approval).clone()),
        _ => None,
    })
    .await;
    t.wait_state(created.id, SessionState::WaitingApproval)
        .await;

    t.client.chat_interrupt(created.id).await.unwrap();
    let (outcome, decided_by) =
        wait_event(&mut t.events, "approval.resolved", |event| match event {
            Event::ApprovalResolved {
                approval_id,
                outcome,
                decided_by,
                ..
            } if *approval_id == info.id => Some((outcome.clone(), decided_by.clone())),
            _ => None,
        })
        .await;
    assert_eq!(outcome, PermissionOutcome::Expired);
    assert_eq!(decided_by, None, "nobody answered");
    next_chat_event(&mut t.events, "turn_cancelled", |e| {
        matches!(e.kind, AgentEventKind::TurnCancelled)
    })
    .await;
    t.wait_state(created.id, SessionState::Idle).await;
    assert!(t.approvals(&t.client).await.is_empty());

    let late = t
        .client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: info.id,
            option_id: "accept".to_owned(),
        }))
        .await;
    assert_eq!(remote_code(late), ErrorCode::NotFound);
    assert!(
        t.received().iter().all(|(m, _)| m != "response"),
        "nothing was answered"
    );

    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let (outcome, decided_by): (String, Option<String>) = db
        .query_row(
            "SELECT outcome, decided_by FROM approvals WHERE id = ?1",
            [info.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((outcome.as_str(), decided_by), ("expired", None));
    // Nor does the journal name anybody.
    let history = t.history(created.id, None, 200).await;
    assert!(history.events.iter().any(|e| matches!(
        &e.kind,
        AgentEventKind::PermissionResolved {
            approval_id,
            outcome: PermissionOutcome::Expired,
            decided_by: None,
        } if *approval_id == info.id
    )));
    t.stop().await;
}

/// `decided_by` of the `permission_resolved` event of `approval` as `chat.history` replays it.
async fn resolved_in_history(
    t: &Chat,
    session: SessionId,
    approval: ApprovalId,
) -> Option<DecidedBy> {
    let history = t.history(session, None, 200).await;
    let resolved: Vec<_> = history
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::PermissionResolved {
                approval_id,
                decided_by,
                ..
            } if *approval_id == approval => Some((e.delivery, decided_by.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(resolved.len(), 1, "one resolution in the journal");
    assert_eq!(resolved[0].0, hutan_core::Delivery::Replay);
    resolved[0].1.clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_from_the_desktop_is_attributed_to_it_and_replayed() {
    let mut t = Chat::with_fixture("approval-touch").await;
    let created = t
        .create_with(
            None,
            Some(ApprovalPolicy::Untrusted),
            Some(SandboxMode::ReadOnly),
        )
        .await;
    t.send(created.id, "Run `touch hi.txt`, then say done.")
        .await;
    let info = wait_event(&mut t.events, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some((**approval).clone()),
        _ => None,
    })
    .await;
    let desktop = t.daemon.connect_as(ClientKind::Desktop).await;
    // A refused attempt takes no credit.
    let refused = desktop
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: info.id,
            option_id: "decline".to_owned(),
        }))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::OptionNotOffered);
    desktop
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: info.id,
            option_id: "accept".to_owned(),
        }))
        .await
        .unwrap();
    let live = next_chat_event(&mut t.events, "permission_resolved", |e| {
        matches!(e.kind, AgentEventKind::PermissionResolved { .. })
    })
    .await;
    let expected = DecidedBy {
        client_kind: "desktop".into(),
        client_name: None,
    };
    assert!(
        matches!(
            &live.kind,
            AgentEventKind::PermissionResolved { decided_by: Some(by), .. } if *by == expected
        ),
        "{live:?}"
    );
    assert_eq!(
        resolved_in_history(&t, created.id, info.id).await,
        Some(expected.clone())
    );
    // The journal's copy is the one in the event.
    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let decided_by: String = db
        .query_row(
            "SELECT decided_by FROM approvals WHERE id = ?1",
            [info.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(decided_by, "desktop");
    t.stop().await;
}

/// A directory of transcripts for a daemon that will spawn the agent several times.
fn script(dir: &Path, names: &[&str]) -> PathBuf {
    let scripted = dir.join("script");
    std::fs::create_dir_all(&scripted).unwrap();
    for (index, name) in names.iter().enumerate() {
        std::fs::copy(
            fixtures().join(format!("{name}.jsonl")),
            scripted.join(format!("{:02}-{name}.jsonl", index + 1)),
        )
        .unwrap();
    }
    scripted
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_restart_resumes_the_agent_and_keeps_the_history() {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["plain-turn", "resume-turn"]);
    let received = work.path().join("received.jsonl");
    let config = replay(&scripted, &received);
    let daemon = TestDaemon::start_with("", {
        let config = config.clone();
        move |c| c.codex = Some(config)
    })
    .await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events: events.resubscribe(),
    };
    let created = chat.create().await;
    chat.send(created.id, "ping").await;
    next_chat_event(&mut events, "the turn to end", is_turn_end).await;
    let before = chat.history(created.id, None, 500).await;
    let thread_id = before
        .events
        .iter()
        .find_map(|e| match &e.kind {
            AgentEventKind::ResumeLocator { thread_id, .. } => Some(thread_id.clone()),
            _ => None,
        })
        .expect("a resume locator in the journal");
    assert!(!before.events.is_empty());

    // The daemon restarts: the agent comes back by resuming the stored thread.
    let Chat { daemon, work, .. } = chat;
    let daemon = daemon
        .restart_with({
            let config = config.clone();
            move |c| c.codex = Some(config)
        })
        .await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events: events.resubscribe(),
    };
    let restored = chat.wait_state(created.id, SessionState::Idle).await;
    assert_eq!(restored.id, created.id, "the same session");
    assert!(!restored.restored, "the journal has the whole conversation");
    let recovery = restored.recovery.clone().expect("the restart is reported");
    assert_eq!(
        recovery.kind,
        RecoveryKind::ChatResumed {
            thread_id: Some(thread_id.clone()),
            last_turn: LastTurn::Completed,
        }
    );
    assert!(matches!(recovery.daemon_stop, DaemonStop::Clean { .. }));
    assert!(recovery.lost.is_empty());
    assert_eq!(restored.chat.unwrap().model, "gpt-6.1-sol");

    let spawns: Vec<_> = chat
        .received()
        .into_iter()
        .filter(|(m, _)| m == "thread/resume")
        .collect();
    assert_eq!(spawns.len(), 1, "{:?}", chat.received());
    assert_eq!(spawns[0].1["params"]["threadId"], thread_id.as_str());
    assert_eq!(spawns[0].1["params"]["excludeTurns"], true);
    assert_eq!(spawns[0].1["params"]["approvalPolicy"], "on-request");

    // History is Hutan's own: unchanged by the resume, with the resumed run appended.
    let after = chat.history(created.id, None, 500).await;
    assert_eq!(&after.events[..before.events.len()], &before.events[..]);
    let last_before = before.events.last().unwrap().seq;
    chat.send(created.id, "again").await;
    next_chat_event(&mut events, "the resumed turn to end", is_turn_end).await;
    // Going on with the conversation is the user's next action: the restart is old news.
    assert_eq!(chat.session(created.id).await.recovery, None);
    let all = chat.history(created.id, None, 500).await;
    let resumed_seqs: Vec<u64> = all
        .events
        .iter()
        .map(|e| e.seq)
        .filter(|seq| *seq > last_before)
        .collect();
    assert!(resumed_seqs.len() >= 4, "{resumed_seqs:?}");
    assert!(
        all.events.windows(2).all(|w| w[0].seq < w[1].seq),
        "seq never goes back"
    );
    let answers: Vec<_> = all
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::MessageEnd {
                role: hutan_core::agent::MessageRole::Assistant,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(answers, ["pong", "again"]);
    chat.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn approvals_pending_at_a_daemon_restart_are_expired() {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["approval-touch", "resume-turn"]);
    let config = replay(&scripted, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", {
        let config = config.clone();
        move |c| c.codex = Some(config)
    })
    .await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events: events.resubscribe(),
    };
    let created = chat.create().await;
    chat.send(created.id, "go").await;
    let info = wait_event(&mut events, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some((**approval).clone()),
        _ => None,
    })
    .await;

    let Chat { daemon, work, .. } = chat;
    let daemon = daemon
        .restart_with({
            let config = config.clone();
            move |c| c.codex = Some(config)
        })
        .await;
    let (client, events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events,
    };
    chat.wait_state(created.id, SessionState::Idle).await;
    assert!(chat.approvals(&chat.client).await.is_empty());
    let late = chat
        .client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: info.id,
            option_id: "accept".to_owned(),
        }))
        .await;
    assert_eq!(remote_code(late), ErrorCode::NotFound);
    let db = rusqlite::Connection::open(&chat.daemon.paths.database).unwrap();
    let outcome: String = db
        .query_row(
            "SELECT outcome FROM approvals WHERE id = ?1",
            [info.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "expired");
    // The journal shows the interrupted turn ended (the agent went down mid-turn).
    let history = chat.history(created.id, None, 500).await;
    let kinds: Vec<_> = history.events.iter().map(|e| e.kind.name()).collect();
    assert!(kinds.contains(&"permission_requested"), "{kinds:?}");
    assert!(
        kinds.contains(&"permission_resolved")
            && (kinds.contains(&"turn_cancelled") || kinds.contains(&"turn_failed")),
        "{kinds:?}"
    );
    chat.stop().await;
}

/// Stops the chat's daemon and returns its home, so the database can be changed before the
/// next start.
async fn stop_chat_daemon(
    chat: Chat,
) -> (tempfile::TempDir, tempfile::TempDir, hutan_daemon::Paths) {
    let Chat { daemon, work, .. } = chat;
    let TestDaemon {
        _dir,
        paths,
        daemon,
    } = daemon;
    daemon.shutdown();
    timeout(WAIT, daemon.wait()).await.unwrap().unwrap();
    (_dir, work, paths)
}

async fn start_chat_daemon(
    dir: tempfile::TempDir,
    work: tempfile::TempDir,
    paths: hutan_daemon::Paths,
    config: CodexConfig,
) -> Chat {
    let mut daemon_config = hutan_daemon::DaemonConfig::new(paths.clone());
    daemon_config.codex = Some(config);
    let daemon = common::start_when_free(daemon_config).await;
    let daemon = TestDaemon {
        _dir: dir,
        paths,
        daemon,
    };
    let (client, events) = daemon.connect_subscribed().await;
    Chat {
        daemon,
        work,
        client,
        events,
    }
}

/// A chat with an approval pending in its first turn, stopped; the agent transcript for the
/// resume is the second one.
async fn chat_stopped_mid_turn() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    hutan_daemon::Paths,
    CodexConfig,
    SessionId,
) {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(
        work.path(),
        &["approval-touch", "resume-turn", "resume-turn"],
    );
    let config = replay(&scripted, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", {
        let config = config.clone();
        move |c| c.codex = Some(config)
    })
    .await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events: events.resubscribe(),
    };
    let created = chat.create().await;
    chat.send(created.id, "go").await;
    wait_event(&mut events, "approval.required", |event| match event {
        Event::ApprovalRequired { .. } => Some(()),
        _ => None,
    })
    .await;
    let (dir, work, paths) = stop_chat_daemon(chat).await;
    (dir, work, paths, config, created.id)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clean_restart_in_the_middle_of_a_turn_says_the_turn_was_interrupted() {
    let (dir, work, paths, config, id) = chat_stopped_mid_turn().await;
    let chat = start_chat_daemon(dir, work, paths, config).await;
    let restored = chat.wait_state(id, SessionState::Idle).await;
    let recovery = restored.recovery.expect("the restart is reported");
    let RecoveryKind::ChatResumed {
        thread_id,
        last_turn,
    } = &recovery.kind
    else {
        panic!("{:?}", recovery.kind);
    };
    assert!(thread_id.is_some());
    assert_eq!(*last_turn, LastTurn::Interrupted);
    assert!(matches!(recovery.daemon_stop, DaemonStop::Clean { .. }));
    assert_eq!(recovery.lost, [Lost::Approvals { count: 1 }, Lost::Turn]);
    // The user dismisses it.
    let acked = chat
        .client
        .call::<hutan_protocol::recovery::RecoveryAcked>(Request::SessionRecoveryAck(
            hutan_protocol::recovery::RecoveryAck {
                session_id: Some(id),
            },
        ))
        .await
        .unwrap();
    assert_eq!(acked.sessions, [id]);
    assert_eq!(chat.session(id).await.recovery, None);

    // A second restart with the turn long over has nothing interrupted to report.
    let (dir, work, paths) = stop_chat_daemon(chat).await;
    let config = replay(
        &work.path().join("script"),
        &work.path().join("received.jsonl"),
    );
    let chat = start_chat_daemon(dir, work, paths, config).await;
    let again = chat.wait_state(id, SessionState::Idle).await;
    let recovery = again.recovery.expect("reported again");
    assert!(
        matches!(
            recovery.kind,
            RecoveryKind::ChatResumed {
                last_turn: LastTurn::Completed,
                ..
            }
        ),
        "{:?}",
        recovery.kind
    );
    assert!(recovery.lost.is_empty(), "{:?}", recovery.lost);
    chat.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_in_the_middle_of_a_turn_is_told_apart_from_a_clean_stop() {
    let (dir, work, paths, config, id) = chat_stopped_mid_turn().await;
    // What a killed daemon leaves: the turn and the approval never ended, and no stop was
    // recorded for the run.
    let storage = hutan_storage::Storage::open(&paths.database).unwrap();
    let db = rusqlite::Connection::open(&paths.database).unwrap();
    db.execute(
        "UPDATE chat_turns SET status = 'running', ended_at_ms = NULL, error = NULL",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE approvals SET resolved_at_ms = NULL, outcome = NULL, decided_by = NULL",
        [],
    )
    .unwrap();
    drop(db);
    let mut history: Vec<serde_json::Value> =
        serde_json::from_str(&storage.app_meta().get("epoch_history").unwrap().unwrap()).unwrap();
    history.last_mut().unwrap()["end"] = serde_json::json!({"state": "open"});
    storage
        .app_meta()
        .set("epoch_history", &serde_json::to_string(&history).unwrap())
        .unwrap();
    storage.close();

    let chat = start_chat_daemon(dir, work, paths, config).await;
    let restored = chat.wait_state(id, SessionState::Idle).await;
    let recovery = restored.recovery.expect("the restart is reported");
    assert_eq!(recovery.daemon_stop, DaemonStop::Crashed);
    assert!(
        matches!(
            recovery.kind,
            RecoveryKind::ChatResumed {
                last_turn: LastTurn::Interrupted,
                ..
            }
        ),
        "{:?}",
        recovery.kind
    );
    assert!(recovery.lost.contains(&Lost::Turn), "{:?}", recovery.lost);
    assert!(
        recovery.lost.contains(&Lost::Approvals { count: 1 }),
        "{:?}",
        recovery.lost
    );
    chat.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_whose_agent_cannot_resume_is_listed_as_not_restarted_with_the_error() {
    let (dir, work, paths, _config, id) = chat_stopped_mid_turn().await;
    let missing = CodexConfig {
        program: PathBuf::from("/definitely/not/a/codex"),
        args: Vec::new(),
        env: Vec::new(),
    };
    let chat = start_chat_daemon(dir, work, paths, missing).await;
    let failed = timeout(WAIT, async {
        loop {
            let session = chat.session(id).await;
            if matches!(session.state, SessionState::Failed { .. }) {
                return session;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the chat fails to start");
    let recovery = failed.recovery.expect("the restart is reported");
    let RecoveryKind::NotRestarted { reason } = &recovery.kind else {
        panic!("said it resumed: {:?}", recovery.kind);
    };
    assert!(reason.contains("resuming failed"), "{reason}");
    assert!(reason.contains("/definitely/not/a/codex"), "{reason}");
    chat.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_stopped_mode_lists_a_chat_without_starting_its_agent() {
    let (dir, work, paths, config, id) = chat_stopped_mid_turn().await;
    std::fs::write(
        &paths.config_file,
        "[daemon]\nrestore_sessions = \"stopped\"\n",
    )
    .unwrap();
    let chat = start_chat_daemon(dir, work, paths, config).await;
    let listed = chat.session(id).await;
    assert_eq!(listed.state, SessionState::Stopped);
    let recovery = listed.recovery.expect("the restart is reported");
    assert!(
        matches!(&recovery.kind, RecoveryKind::NotRestarted { reason } if reason.contains("restore_sessions")),
        "{:?}",
        recovery.kind
    );
    chat.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_chat_ends_its_agent_and_restarting_resumes_it() {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["plain-turn", "resume-turn"]);
    let config = replay(&scripted, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let (client, events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events,
    };
    let created = chat.create().await;

    let stopped = stop(&chat.client, created.id).await.unwrap();
    assert_eq!(stopped.state, SessionState::Stopped);
    let again = stop(&chat.client, created.id).await;
    assert_eq!(remote_code(again), ErrorCode::InvalidState);
    let blocked = chat
        .client
        .call::<ChatSent>(Request::ChatSend(ChatSend {
            session_id: created.id,
            text: "hi".into(),
            attachments: vec![],
        }))
        .await;
    assert_eq!(remote_code(blocked), ErrorCode::InvalidState);

    let restarted = chat
        .client
        .call::<SessionResult>(Request::SessionRestart(SessionRef {
            session_id: created.id,
        }))
        .await
        .unwrap()
        .session;
    assert_eq!(restarted.id, created.id);
    chat.wait_state(created.id, SessionState::Idle).await;
    let resumed = chat
        .received()
        .into_iter()
        .filter(|(m, _)| m == "thread/resume")
        .count();
    assert_eq!(resumed, 1);
    chat.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sending_is_validated() {
    let mut t = Chat::with_fixture("interrupt").await;
    let created = t.create().await;
    let empty = t
        .client
        .call::<ChatSent>(Request::ChatSend(ChatSend {
            session_id: created.id,
            text: "  \n".into(),
            attachments: vec![],
        }))
        .await;
    assert_eq!(remote_code(empty), ErrorCode::BadRequest);
    let unknown = t
        .client
        .call::<ChatSent>(Request::ChatSend(ChatSend {
            session_id: SessionId::new(),
            text: "x".into(),
            attachments: vec![],
        }))
        .await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    let terminal = create(&t.client, spec(&["cat"])).await;
    let not_chat = t
        .client
        .call::<ChatSent>(Request::ChatSend(ChatSend {
            session_id: terminal.id,
            text: "x".into(),
            attachments: vec![],
        }))
        .await;
    assert_eq!(remote_code(not_chat), ErrorCode::BadRequest);
    let no_turn = t.client.chat_interrupt(created.id).await;
    assert_eq!(remote_code(no_turn), ErrorCode::InvalidState);

    // One turn at a time.
    t.send(created.id, "Write the numbers from 1 to 300").await;
    let second = t
        .client
        .call::<ChatSent>(Request::ChatSend(ChatSend {
            session_id: created.id,
            text: "more".into(),
            attachments: vec![],
        }))
        .await;
    assert_eq!(remote_code(second), ErrorCode::InvalidState);
    t.client.chat_interrupt(created.id).await.unwrap();
    next_chat_event(&mut t.events, "turn_cancelled", |e| {
        matches!(e.kind, AgentEventKind::TurnCancelled)
    })
    .await;
    t.wait_state(created.id, SessionState::Idle).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_a_chat_validates_the_request() {
    let t = Chat::with_fixture("plain-turn").await;
    let create = |agent: &str, cwd: PathBuf| {
        let client = t.client.clone();
        let agent = agent.to_owned();
        async move {
            client
                .call::<SessionResult>(Request::ChatCreate(ChatCreate {
                    extra_dirs: Vec::new(),
                    contain: None,
                    workspace_id: None,
                    cwd,
                    agent,
                    model: None,
                    approval_policy: None,
                    sandbox: None,
                    title: None,
                    mode: None,
                    provider_id: None,
                    system_prompt: None,
                    account: None,
                    safety: None,
                }))
                .await
        }
    };
    assert_eq!(
        remote_code(create("gemini", t.cwd()).await),
        ErrorCode::BadRequest
    );
    assert_eq!(
        remote_code(create("codex", t.cwd().join("missing")).await),
        ErrorCode::BadRequest
    );
    assert_eq!(
        remote_code(create("codex", "relative/dir".into()).await),
        ErrorCode::BadRequest
    );
    let history = t
        .client
        .call::<ChatHistory>(Request::ChatHistory(ChatHistoryRequest {
            session_id: SessionId::new(),
            before_seq: None,
            limit: 10,
        }))
        .await;
    assert_eq!(remote_code(history), ErrorCode::NotFound);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsupported_codex_version_fails_the_session_with_a_clear_reason() {
    let work = tempfile::tempdir().unwrap();
    let transcript = work.path().join("old.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"dir":"out","msg":{"method":"initialize","id":1,"params":{}}}"#,
            "\n",
            r#"{"dir":"in","msg":{"id":1,"result":{"userAgent":"hutan/0.144.2 (Mac OS; arm64)"}}}"#,
            "\n"
        ),
    )
    .unwrap();
    let config = replay(&transcript, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let (client, _events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events: _events,
    };
    let result = chat
        .client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: None,
            cwd: chat.cwd(),
            agent: "codex".into(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        }))
        .await;
    let hutan_client::ClientError::Remote(error) = result.unwrap_err() else {
        panic!()
    };
    assert_eq!(error.code, ErrorCode::ProviderError);
    assert!(
        error.message.contains("0.144.2") && error.message.contains(">=0.160, <0.170"),
        "{}",
        error.message
    );
    let sessions = chat
        .client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap()
        .sessions;
    let SessionState::Failed { reason } = &sessions[0].state else {
        panic!("{sessions:?}")
    };
    assert!(reason.contains("0.144.2"), "{reason}");
    chat.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_codex_says_where_to_configure_it() {
    let daemon = TestDaemon::start_with("", |c| {
        c.codex = Some(CodexConfig {
            program: "/nonexistent/hutan-test-codex".into(),
            ..CodexConfig::default()
        });
    })
    .await;
    let client = daemon.connect().await;
    let cwd = tempfile::tempdir().unwrap();
    let result = client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: None,
            cwd: cwd.path().to_owned(),
            agent: "codex".into(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        }))
        .await;
    let hutan_client::ClientError::Remote(error) = result.unwrap_err() else {
        panic!()
    };
    assert_eq!(error.code, ErrorCode::ProviderError);
    assert!(
        error.message.contains("/nonexistent/hutan-test-codex")
            && error.message.contains("daemon.toml"),
        "{}",
        error.message
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn models_come_from_the_agents_catalog() {
    let t = Chat::with_fixture("plain-turn").await;
    let models = t
        .client
        .call::<ChatModels>(Request::ChatModels(ChatModelsRequest {
            agent: "codex".into(),
        }))
        .await
        .unwrap();
    assert_eq!(models.agent, "codex");
    let default: Vec<_> = models
        .models
        .iter()
        .filter(|m| m.is_default)
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(default, ["gpt-6.1-sol"]);
    assert!(models.models.iter().any(|m| m.id == "gpt-5.6-luna"));
    let unsupported = t
        .client
        .call::<ChatModels>(Request::ChatModels(ChatModelsRequest {
            agent: "gemini".into(),
        }))
        .await;
    assert_eq!(remote_code(unsupported), ErrorCode::BadRequest);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_dies_mid_turn_fails_the_session_and_ends_the_turn() {
    let work = tempfile::tempdir().unwrap();
    let real = std::fs::read_to_string(fixtures().join("plain-turn.jsonl")).unwrap();
    let mut lines: Vec<String> = Vec::new();
    for line in real.lines() {
        lines.push(line.to_owned());
        if line.contains("\"item/agentMessage/delta\"") {
            break;
        }
    }
    lines.push(r#"{"dir":"exit","code":3}"#.to_owned());
    let transcript = work.path().join("dies.jsonl");
    std::fs::write(&transcript, lines.join("\n") + "\n").unwrap();
    let config = replay(&transcript, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let (client, events) = daemon.connect_subscribed().await;
    let mut t = Chat {
        daemon,
        work,
        client,
        events,
    };
    let created = t.create().await;
    t.send(created.id, "ping").await;
    let failed = next_chat_event(&mut t.events, "turn_failed", |e| {
        matches!(e.kind, AgentEventKind::TurnFailed { .. })
    })
    .await;
    let AgentEventKind::TurnFailed { message, .. } = failed.kind else {
        unreachable!()
    };
    assert!(message.contains("exited during the turn"), "{message}");
    let session = t
        .wait_state(
            created.id,
            SessionState::Failed {
                reason: "the agent exited with code 3".to_owned(),
            },
        )
        .await;
    assert!(!session.chat.unwrap().turn_active);
    t.stop().await;
}

/// `plain-turn` with its one `"pong"` delta replaced by a stream that stops at two gates: the
/// message is `"Hello, "`, then (after the first gate) 40 one-character deltas and `"world"`,
/// then (after the second gate) the rest. Returns the transcript and the final text.
fn gated_stream(dir: &Path, gates: [&Path; 2]) -> (PathBuf, String) {
    let real = std::fs::read_to_string(fixtures().join("plain-turn.jsonl")).unwrap();
    let lines: Vec<&str> = real.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains("\"item/agentMessage/delta\""))
        .unwrap();
    let delta = |text: &str| {
        let mut message: Value = serde_json::from_str(lines[at]).unwrap();
        message["msg"]["params"]["delta"] = Value::String(text.to_owned());
        message.to_string()
    };
    let hold = |gate: &Path| serde_json::json!({"dir": "hold", "file": gate}).to_string();
    let middle: String = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMN".to_owned();
    assert_eq!(middle.len(), 40);
    let full = format!("Hello, {middle}world");

    let mut out: Vec<String> = lines[..at].iter().map(|l| (*l).to_owned()).collect();
    out.push(delta("Hel"));
    out.push(delta("lo, "));
    out.push(hold(gates[0]));
    for c in middle.chars() {
        out.push(delta(&c.to_string()));
    }
    out.push(delta("world"));
    out.push(hold(gates[1]));
    // The completed item has the whole text.
    let mut completed: Value = serde_json::from_str(lines[at + 1]).unwrap();
    assert_eq!(completed["msg"]["method"], "item/completed");
    completed["msg"]["params"]["item"]["text"] = Value::String(full.clone());
    out.push(completed.to_string());
    out.extend(lines[at + 2..].iter().map(|l| (*l).to_owned()));
    let transcript = dir.join("stream.jsonl");
    std::fs::write(&transcript, out.join("\n") + "\n").unwrap();
    (transcript, full)
}

/// `(kind, event_json)` of the journal rows of `session`, oldest first.
fn journal_rows(t: &Chat, session: SessionId) -> Vec<(String, String)> {
    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let mut stmt = db
        .prepare("SELECT kind, event_json FROM chat_items WHERE session_id = ?1 ORDER BY seq")
        .unwrap();
    stmt.query_map([session.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn journaled_partial(rows: &[(String, String)]) -> Vec<AgentEvent> {
    rows.iter()
        .filter(|(kind, _)| kind == "message_partial")
        .map(|(_, json)| serde_json::from_str(json).unwrap())
        .collect()
}

fn partial_text(event: &AgentEvent) -> &str {
    match &event.kind {
        AgentEventKind::MessagePartial { text, .. } => text,
        other => panic!("not a partial: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_joins_mid_stream_gets_the_text_so_far_and_then_the_live_deltas() {
    let work = tempfile::tempdir().unwrap();
    let gate_one = work.path().join("gate-one");
    let gate_two = work.path().join("gate-two");
    let (transcript, full) = gated_stream(work.path(), [&gate_one, &gate_two]);
    let config = replay(&transcript, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let (client, events) = daemon.connect_subscribed().await;
    let mut t = Chat {
        daemon,
        work,
        client,
        events,
    };
    let created = t.create().await;
    t.send(created.id, "say hello").await;

    // The stream stops at the first gate with "Hello, " written.
    let item = loop {
        let event = next_chat_event(&mut t.events, "the first deltas", |e| {
            matches!(e.kind, AgentEventKind::MessageDelta { .. })
        })
        .await;
        if matches!(&event.kind, AgentEventKind::MessageDelta { text } if text == "lo, ") {
            break event.item_id.unwrap();
        }
    };
    // Within a second the daemon has journaled a snapshot of it.
    let rows = timeout(WAIT, async {
        loop {
            let rows = journal_rows(&t, created.id);
            if !journaled_partial(&rows).is_empty() {
                return rows;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("a snapshot is journaled while the message streams");
    let journaled = journaled_partial(&rows);
    assert_eq!(journaled.len(), 1);
    assert_eq!(partial_text(&journaled[0]), "Hello, ");
    assert_eq!(journaled[0].item_id, Some(item));
    let rows_at_first_gate = rows.len();

    // A second client opens the chat now, subscribed before it reads the journal.
    let (late, mut late_events) = t.daemon.connect_subscribed().await;
    let history: ChatHistory = late
        .call(Request::ChatHistory(ChatHistoryRequest {
            session_id: created.id,
            before_seq: None,
            limit: 200,
        }))
        .await
        .unwrap();
    let partials: Vec<&AgentEvent> = history
        .events
        .iter()
        .filter(|e| e.kind.name() == "message_partial")
        .collect();
    assert_eq!(partials.len(), 1, "one partial per open message");
    let snapshot = partials[0];
    assert_eq!(partial_text(snapshot), "Hello, ");
    assert_eq!(snapshot.delivery, hutan_core::Delivery::Replay);
    assert!(snapshot.seq <= history.last_seq);
    assert!(
        history.events.iter().all(|e| !matches!(
            &e.kind,
            AgentEventKind::MessageEnd {
                role: hutan_core::agent::MessageRole::Assistant,
                ..
            }
        )),
        "the message is not over"
    );

    // The rest streams: the client shows the snapshot, then appends every live delta after it.
    std::fs::write(&gate_one, "").unwrap();
    let mut shown = partial_text(snapshot).to_owned();
    let mut last = snapshot.seq;
    let mut deltas = 0;
    while shown.len() < "Hello, ".len() + 40 + "world".len() {
        let event = next_chat_event(&mut late_events, "deltas", |_| true).await;
        if let AgentEventKind::MessageDelta { text } = &event.kind {
            assert!(
                event.seq > last,
                "live deltas arrive in order after the snapshot"
            );
            assert_eq!(event.item_id, Some(item));
            last = event.seq;
            shown.push_str(text);
            deltas += 1;
        }
    }
    assert_eq!(deltas, 41);
    assert_eq!(shown, full);

    // Stopped at the second gate: another snapshot replaced the first, and 41 deltas did not
    // make the journal any longer.
    let rows = timeout(WAIT, async {
        loop {
            let rows = journal_rows(&t, created.id);
            let partial = journaled_partial(&rows);
            if partial.len() == 1 && partial_text(&partial[0]) == full {
                return rows;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the snapshot is replaced by a newer one");
    assert_eq!(
        rows.len(),
        rows_at_first_gate,
        "the journal did not grow with the deltas"
    );
    let replaced = &journaled_partial(&rows)[0];
    assert!(
        replaced.seq > snapshot.seq,
        "a newer snapshot, in place of the old one"
    );

    // The message ends: the snapshot is superseded, the journal has exactly the final text.
    std::fs::write(&gate_two, "").unwrap();
    let end = next_chat_event(&mut t.events, "message_end", |e| {
        matches!(e.kind, AgentEventKind::MessageEnd { .. })
    })
    .await;
    let AgentEventKind::MessageEnd { text, .. } = &end.kind else {
        unreachable!()
    };
    assert_eq!(*text, full);
    next_chat_event(&mut t.events, "the turn to end", is_turn_end).await;
    t.wait_state(created.id, SessionState::Idle).await;
    let rows = journal_rows(&t, created.id);
    assert!(
        journaled_partial(&rows).is_empty(),
        "superseded by message_end"
    );
    let history = t.history(created.id, None, 200).await;
    assert!(
        history
            .events
            .iter()
            .all(|e| e.kind.name() != "message_partial")
    );
    let finals: Vec<&str> = history
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::MessageEnd {
                role: hutan_core::agent::MessageRole::Assistant,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(finals, [full.as_str()]);
    drop(late);
    t.stop().await;
}

// Questions (protocol 0.24) -------------------------------------------------------------------

const SECRET: &str = "s3cr3t-token";

impl Chat {
    async fn questions(&self, client: &Client) -> Vec<QuestionInfo> {
        client
            .call::<QuestionList>(Request::QuestionList(QuestionListRequest {
                session_id: None,
            }))
            .await
            .unwrap()
            .questions
    }

    async fn asked(&mut self) -> QuestionInfo {
        wait_event(&mut self.events, "question.required", |event| match event {
            Event::QuestionRequired { question } => Some((**question).clone()),
            _ => None,
        })
        .await
    }

    async fn resolved(&mut self, question: QuestionId) -> (QuestionOutcome, Option<DecidedBy>) {
        wait_event(&mut self.events, "question.resolved", |event| match event {
            Event::QuestionResolved {
                question_id,
                outcome,
                decided_by,
                ..
            } if *question_id == question => Some((outcome.clone(), decided_by.clone())),
            _ => None,
        })
        .await
    }

    /// Every file of the daemon's home (database, its write-ahead log, logs) that holds `text`.
    fn files_holding(&self, text: &str) -> Vec<PathBuf> {
        fn walk(dir: &Path, text: &[u8], found: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, text, found);
                } else if let Ok(bytes) = std::fs::read(&path)
                    && bytes.windows(text.len()).any(|w| w == text)
                {
                    found.push(path);
                }
            }
        }
        let mut found = Vec::new();
        walk(self.daemon._dir.path(), text.as_bytes(), &mut found);
        found
    }
}

fn values(pairs: &[(&str, Value)]) -> Values {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

async fn answer(
    client: &Client,
    question: QuestionId,
    response: QuestionResponse,
) -> Result<Empty, hutan_client::ClientError> {
    client
        .call::<Empty>(Request::QuestionAnswer(QuestionAnswer {
            question_id: question,
            response,
        }))
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_question_is_pending_in_the_daemon_checked_answered_once_and_the_secret_never_kept() {
    let mut t = Chat::with_fixture("question-user-input").await;
    let created = t.create().await;
    t.send(created.id, "Ask me something.").await;
    let info = t.asked().await;
    assert_eq!(info.session_id, created.id);
    assert_eq!(info.spec.fields.len(), 3);

    // The session waits for the user; the question is listed, and survives a client leaving.
    let waiting = t
        .wait_state(created.id, SessionState::WaitingApproval)
        .await;
    let chat = waiting.chat.unwrap();
    assert_eq!((chat.pending_questions, chat.pending_approvals), (1, 0));
    assert_eq!(t.questions(&t.client).await, std::slice::from_ref(&info));
    let other = t.daemon.connect().await;
    assert_eq!(t.questions(&other).await, std::slice::from_ref(&info));
    // Its intent says so, with "Open" only: a form is not answered from a notification.
    let intents = t
        .client
        .call::<NotifyList>(Request::NotifyList(Empty {}))
        .await
        .unwrap()
        .intents;
    let intent = intents
        .iter()
        .find(|i| i.kind == IntentKind::QuestionRequired)
        .expect("a question_required intent");
    assert_eq!(intent.session_id, created.id);
    assert_eq!(
        intent
            .actions
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        [ACTION_OPEN]
    );
    assert_eq!(intent.expires_at_ms, None, "lives as long as the question");

    // What does not fit the form is refused and reaches nobody.
    for (what, wrong) in [
        (
            "an unknown field",
            values(&[("nonsense", Value::from("x"))]),
        ),
        (
            "a missing required field",
            values(&[("scope", Value::from("Only the CLI"))]),
        ),
        (
            "a value of the wrong type",
            values(&[
                ("scope", Value::from(7)),
                ("note", Value::from("n")),
                ("token", Value::from("t")),
            ]),
        ),
    ] {
        let refused = answer(
            &t.client,
            info.id,
            QuestionResponse::Submit { values: wrong },
        )
        .await;
        assert_eq!(remote_code(refused), ErrorCode::BadRequest, "{what}");
    }
    let unknown = answer(&t.client, QuestionId::new(), QuestionResponse::Decline).await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    assert!(t.received().iter().all(|(m, _)| m != "response"));
    assert_eq!(
        t.questions(&t.client).await.len(),
        1,
        "refusals leave it pending"
    );

    // The desktop answers; everyone is told, and the secret is in nobody's copy.
    let desktop = t.daemon.connect_as(ClientKind::Desktop).await;
    answer(
        &desktop,
        info.id,
        QuestionResponse::Submit {
            values: values(&[
                ("scope", Value::from("Only the CLI")),
                ("note", Value::from("none")),
                ("token", Value::from(SECRET)),
            ]),
        },
    )
    .await
    .unwrap();
    let (outcome, decided_by) = t.resolved(info.id).await;
    assert_eq!(
        outcome,
        QuestionOutcome::Answered {
            values: values(&[
                ("scope", Value::from("Only the CLI")),
                ("note", Value::from("none")),
            ]),
            withheld: vec!["token".to_owned()],
        }
    );
    assert_eq!(
        decided_by,
        Some(DecidedBy {
            client_kind: "desktop".into(),
            client_name: None
        })
    );
    // Answered once.
    let twice = answer(&t.client, info.id, QuestionResponse::Decline).await;
    assert_eq!(remote_code(twice), ErrorCode::NotFound);
    next_chat_event(&mut t.events, "the turn to end", is_turn_end).await;
    let idle = t.wait_state(created.id, SessionState::Idle).await;
    assert_eq!(idle.chat.unwrap().pending_questions, 0);
    assert!(t.questions(&t.client).await.is_empty());
    let intents = t
        .client
        .call::<NotifyList>(Request::NotifyList(Empty {}))
        .await
        .unwrap()
        .intents;
    assert!(
        intents
            .iter()
            .all(|i| i.kind != IntentKind::QuestionRequired),
        "resolving the question withdrew its intent"
    );

    // Codex got the real answer (the fake server logged it); Hutan kept none of it.
    let response = t
        .received()
        .into_iter()
        .find(|(m, _)| m == "response")
        .expect("the answer reached the agent");
    assert_eq!(
        response.1["result"]["answers"]["token"],
        serde_json::json!({"answers": [SECRET]})
    );
    let history = t.history(created.id, None, 500).await;
    let journaled: Vec<_> = history
        .events
        .iter()
        .filter(|e| e.kind.name().starts_with("question_"))
        .collect();
    assert_eq!(journaled.len(), 2, "{journaled:?}");
    assert!(matches!(
        &journaled[1].kind,
        AgentEventKind::QuestionResolved {
            outcome: QuestionOutcome::Answered { withheld, .. },
            decided_by: Some(by),
            ..
        } if withheld == &["token".to_owned()] && by.client_kind == "desktop"
    ));
    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let (outcome, decided_by): (String, Option<String>) = db
        .query_row(
            "SELECT outcome, decided_by FROM questions WHERE id = ?1",
            [info.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (outcome.as_str(), decided_by.as_deref()),
        ("answered", Some("desktop"))
    );
    drop(db);
    assert_eq!(
        t.files_holding(SECRET),
        Vec::<PathBuf>::new(),
        "the secret is in no file of the daemon's home"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn declining_an_mcp_elicitation_says_who_declined_and_the_agent_gets_decline() {
    let mut t = Chat::with_fixture("question-elicitation").await;
    let created = t.create().await;
    t.send(created.id, "Deploy.").await;
    let info = t.asked().await;
    assert_eq!(info.spec.source.as_deref(), Some("MCP server deployer"));
    answer(&t.client, info.id, QuestionResponse::Decline)
        .await
        .unwrap();
    let (outcome, decided_by) = t.resolved(info.id).await;
    assert_eq!(outcome, QuestionOutcome::Declined);
    assert_eq!(decided_by.unwrap().client_kind, "cli");
    next_chat_event(&mut t.events, "the turn to end", is_turn_end).await;
    let response = t
        .received()
        .into_iter()
        .find(|(m, _)| m == "response")
        .unwrap();
    assert_eq!(
        response.1["result"],
        serde_json::json!({"action": "decline", "content": null})
    );
    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let (outcome, decided_by): (String, Option<String>) = db
        .query_row(
            "SELECT outcome, decided_by FROM questions WHERE id = ?1",
            [info.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (outcome.as_str(), decided_by.as_deref()),
        ("declined", Some("cli"))
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_question_expires_when_its_turn_ends() {
    let mut t = Chat::with_fixture("question-interrupt").await;
    let created = t.create().await;
    t.send(created.id, "Ask.").await;
    let info = t.asked().await;
    t.wait_state(created.id, SessionState::WaitingApproval)
        .await;
    t.client.chat_interrupt(created.id).await.unwrap();
    let (outcome, decided_by) = t.resolved(info.id).await;
    assert_eq!(outcome, QuestionOutcome::Expired);
    assert_eq!(decided_by, None, "nobody answered");
    next_chat_event(&mut t.events, "turn_cancelled", |e| {
        matches!(e.kind, AgentEventKind::TurnCancelled)
    })
    .await;
    t.wait_state(created.id, SessionState::Idle).await;
    assert!(t.questions(&t.client).await.is_empty());
    let late = answer(&t.client, info.id, QuestionResponse::Decline).await;
    assert_eq!(remote_code(late), ErrorCode::NotFound);
    assert!(t.received().iter().all(|(m, _)| m != "response"));
    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let (outcome, decided_by): (String, Option<String>) = db
        .query_row(
            "SELECT outcome, decided_by FROM questions WHERE id = ?1",
            [info.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((outcome.as_str(), decided_by), ("expired", None));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn questions_pending_at_a_daemon_restart_are_expired() {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["question-user-input", "resume-turn"]);
    let config = replay(&scripted, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", {
        let config = config.clone();
        move |c| c.codex = Some(config)
    })
    .await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events: events.resubscribe(),
    };
    let created = chat.create().await;
    chat.send(created.id, "go").await;
    let info = wait_event(&mut events, "question.required", |event| match event {
        Event::QuestionRequired { question } => Some((**question).clone()),
        _ => None,
    })
    .await;

    let Chat { daemon, work, .. } = chat;
    let daemon = daemon
        .restart_with({
            let config = config.clone();
            move |c| c.codex = Some(config)
        })
        .await;
    let (client, events) = daemon.connect_subscribed().await;
    let chat = Chat {
        daemon,
        work,
        client,
        events,
    };
    let restored = chat.wait_state(created.id, SessionState::Idle).await;
    // The restart says what it cost: the question and the turn it was asked in.
    let recovery = restored.recovery.clone().expect("the restart is reported");
    assert_eq!(recovery.lost, [Lost::Questions { count: 1 }, Lost::Turn]);
    assert_eq!(restored.chat.unwrap().pending_questions, 0);
    assert!(chat.questions(&chat.client).await.is_empty());
    let late = answer(&chat.client, info.id, QuestionResponse::Decline).await;
    assert_eq!(remote_code(late), ErrorCode::NotFound);
    let db = rusqlite::Connection::open(&chat.daemon.paths.database).unwrap();
    let outcome: String = db
        .query_row(
            "SELECT outcome FROM questions WHERE id = ?1",
            [info.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "expired");
    // The journal closes the question like the turn: nothing is left asking.
    let history = chat.history(created.id, None, 500).await;
    assert!(history.events.iter().any(|e| matches!(
        &e.kind,
        AgentEventKind::QuestionResolved {
            question_id,
            outcome: QuestionOutcome::Expired,
            ..
        } if *question_id == info.id
    )));
    chat.stop().await;
}

// Usage and limits ------------------------------------------------------------------------------

/// The `usage.changed` that shows the codex turn and both windows.
async fn codex_usage(
    events: &mut broadcast::Receiver<(Seq, Event)>,
) -> hutan_protocol::usage::UsageSummary {
    wait_event(
        events,
        "usage.changed with the turn and the windows",
        |event| match event {
            Event::UsageChanged { summary }
                if summary
                    .harnesses
                    .iter()
                    .any(|h| h.harness == "codex" && h.week.turns == 1 && h.limits.len() == 2) =>
            {
                Some((**summary).clone())
            }
            _ => None,
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_rate_limit_notifications_and_a_turns_tokens_reach_the_usage_summary() {
    // `rate-limits`: the recorded plain turn with two `account/rateLimits/updated` notifications
    // (a full report, then a sparse one that moves only the 5-hour window).
    let mut t = Chat::with_fixture("rate-limits").await;
    assert!(t.client.supports_usage());
    let created = t.create().await;
    t.send(
        created.id,
        "Reply with the single word pong and nothing else.",
    )
    .await;
    let summary = codex_usage(&mut t.events).await;

    let codex = summary
        .harnesses
        .iter()
        .find(|h| h.harness == "codex")
        .unwrap();
    let windows: Vec<_> = codex
        .limits
        .iter()
        .map(|l| {
            (
                l.id.as_str(),
                l.label.as_str(),
                l.used_percent,
                l.resets_at_ms,
            )
        })
        .collect();
    assert_eq!(
        windows,
        [
            ("5h", "5-hour", 81.0, Some(4_102_444_800_000)),
            ("7d", "weekly", 31.0, Some(4_102_531_200_000)),
        ],
        "the sparse report moved the 5-hour window only"
    );
    assert!(codex.limits.iter().all(|l| l.provider == "openai"));
    // The recorded turn: 22 178 tokens, 12 288 of them cached, no cost (a subscription).
    for totals in [&codex.today, &codex.week] {
        assert_eq!(totals.turns, 1);
        assert_eq!(
            (
                totals.input_tokens,
                totals.cached_input_tokens,
                totals.output_tokens,
                totals.total_tokens
            ),
            (22_173, 12_288, 5, 22_178)
        );
        assert_eq!((totals.cost_usd, totals.cost_is_estimate), (None, false));
    }
    assert_eq!(summary.providers.len(), 1);
    assert_eq!(summary.providers[0].provider, "openai");
    assert!(summary.day_start_ms <= summary.generated_at_ms);
    assert!(summary.week_start_ms <= summary.day_start_ms);
    // The same picture on request, and after the notifications the journal holds no chatter.
    let asked = t.client.usage_summary(false).await.unwrap();
    assert_eq!(asked.harnesses, summary.harnesses);
    let history = t.history(created.id, None, 500).await;
    assert!(
        !history.events.iter().any(|e| e.kind.name() == "opaque"),
        "rate limits are not journaled"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_private_in_a_rate_limit_report_is_stored_or_sent() {
    const SECRETS: &[&str] = &[
        "me@example.com",
        "acct_SECRET_1",
        "sk-ant-oat01-SECRETSECRETSECRET",
        "Bearer-token-SECRET",
        "99.99",
        "Me Myself",
    ];
    let work = tempfile::tempdir().unwrap();
    let transcript = work.path().join("leaky.jsonl");
    let real = std::fs::read_to_string(fixtures().join("rate-limits.jsonl")).unwrap();
    let leaky: String = real
        .lines()
        .map(|line| {
            let mut value: Value = serde_json::from_str(line).unwrap();
            if let Some(limits) = value.pointer_mut("/msg/params/rateLimits") {
                let limits = limits.as_object_mut().unwrap();
                limits.insert("limitName".into(), "Me Myself <me@example.com>".into());
                limits.insert("accountId".into(), "acct_SECRET_1".into());
                limits.insert(
                    "accessToken".into(),
                    "sk-ant-oat01-SECRETSECRETSECRET".into(),
                );
                limits.insert("planType".into(), "me@example.com".into());
                limits.insert(
                    "credits".into(),
                    serde_json::json!({"hasCredits": true, "unlimited": false, "balance": "99.99"}),
                );
                if let Some(primary) = limits.get_mut("primary").and_then(Value::as_object_mut) {
                    primary.insert("label".into(), "Bearer-token-SECRET".into());
                }
            }
            format!("{value}\n")
        })
        .collect();
    std::fs::write(&transcript, leaky).unwrap();
    let mut t = Chat::with_transcripts(&transcript, |_| {}).await;
    let created = t.create().await;
    t.send(
        created.id,
        "Reply with the single word pong and nothing else.",
    )
    .await;
    let summary = codex_usage(&mut t.events).await;

    let sent = serde_json::to_string(&summary).unwrap();
    assert!(
        summary.harnesses[0].limits.iter().all(|l| l.plan.is_none()),
        "{sent}"
    );
    // What was stored: every value of both tables, and the database files themselves (the
    // write-ahead log included).
    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let mut stored = String::new();
    for table in ["usage_limits", "usage_turns"] {
        let mut stmt = db.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let columns = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for column in 0..columns {
                match row.get_ref(column).unwrap() {
                    rusqlite::types::ValueRef::Text(text) => {
                        stored.push_str(&String::from_utf8_lossy(text));
                    }
                    other => stored.push_str(&format!("{other:?}")),
                }
                stored.push(' ');
            }
        }
    }
    assert!(stored.contains("openai"), "the rows exist: {stored}");
    let mut files = String::new();
    for entry in std::fs::read_dir(t.daemon.paths.database.parent().unwrap()).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("hutan.db"))
        {
            files.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
        }
    }
    for secret in SECRETS {
        assert!(!sent.contains(secret), "{secret} in the summary: {sent}");
        assert!(!stored.contains(secret), "{secret} in the tables: {stored}");
        assert!(!files.contains(secret), "{secret} in the database files");
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_asks_a_codex_that_ran_through_hutan_for_its_rate_limits() {
    // Spawn 1 is the chat (the recorded turn with its notifications, ending on 81 % and 31 %),
    // spawn 2 the short-lived app server of the refresh (`account/rateLimits/read`: 72 %).
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["rate-limits", "rate-limits-read"]);
    let config = replay(&scripted, &work.path().join("received.jsonl"));
    // No OMP either: the daemon must not run whatever `omp` the machine has.
    let daemon = TestDaemon::start_with(
        "[harness.omp]\ncli = \"/nonexistent/hutan-test-omp\"\n",
        move |c| {
            c.codex = Some(config);
            c.usage_probes = true;
        },
    )
    .await;
    let (client, events) = daemon.connect_subscribed().await;
    let mut t = Chat {
        daemon,
        work,
        client,
        events,
    };
    // Codex was never used: a refresh must not start it (that would take the transcript of the
    // chat below).
    let idle = t.client.usage_summary(true).await.unwrap();
    assert!(idle.harnesses.is_empty(), "{idle:?}");

    let created = t.create().await;
    t.send(
        created.id,
        "Reply with the single word pong and nothing else.",
    )
    .await;
    let known = codex_usage(&mut t.events).await;
    assert_eq!(known.harnesses[0].limits[0].used_percent, 81.0);

    let refreshed = t.client.usage_summary(true).await.unwrap();
    let five_hours = &refreshed.harnesses[0].limits[0];
    assert_eq!(
        (five_hours.id.as_str(), five_hours.used_percent),
        ("5h", 72.0)
    );
    assert_eq!(
        t.received()
            .iter()
            .filter(|(m, _)| m == "account/rateLimits/read")
            .count(),
        1,
        "asked once"
    );
    t.stop().await;
}

// Fork (ADR 0023) ------------------------------------------------------------------------------

/// `fork-turn` is built from the 0.162.1 schema (`ThreadForkParams`/`ThreadForkResponse`) on the
/// shape of a recorded `thread/resume`; `two-turns` is two recorded turns of `plain-turn`.
#[tokio::test(flavor = "multi_thread")]
async fn a_codex_chat_forks_after_a_turn_with_thread_fork_and_a_copy_of_its_journal() {
    use hutan_core::agent::MessageRole;
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["two-turns", "fork-turn"]);
    let config = replay(&scripted, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let t = Chat {
        daemon,
        work,
        client,
        events: events.resubscribe(),
    };
    let created = t.create().await;
    assert_eq!(created.forked_from, None);
    for text in ["first", "second"] {
        t.send(created.id, text).await;
        next_chat_event(&mut events, "the turn to end", is_turn_end).await;
        t.wait_state(created.id, SessionState::Idle).await;
    }
    let parent = t.history(created.id, None, 500).await;
    let started: Vec<&AgentEvent> = parent
        .events
        .iter()
        .filter(|e| matches!(e.kind, AgentEventKind::TurnStarted))
        .collect();
    assert_eq!(started.len(), 2);
    let first = started[0].turn_id.unwrap();
    let provider_first = started[0]
        .provider
        .turn_id
        .clone()
        .expect("Codex's own turn id");
    let parent_thread = created
        .chat
        .as_ref()
        .unwrap()
        .agent_session_id
        .clone()
        .unwrap();
    assert_eq!(
        t.session(created.id)
            .await
            .chat
            .unwrap()
            .fork
            .map(|f| f.support),
        Some(hutan_core::ForkSupport::Exact),
        "Codex cuts at any turn"
    );

    let forked: ChatForked = t
        .client
        .call(Request::ChatFork(ChatFork {
            session_id: created.id,
            turn_id: Some(first),
            mode: hutan_core::ForkMode::Exact,
            title: None,
            where_: ForkWhere::ThisFolder,
            branch: None,
            restore_files: false,
            archive_parent: false,
        }))
        .await
        .unwrap();
    let child = forked.session;
    assert_eq!(forked.fork.ordinal, 1);
    assert_eq!(forked.fork.support, hutan_core::ForkSupport::Exact);
    let origin = child.forked_from.clone().unwrap();
    assert_eq!((origin.session_id, origin.turn_id), (created.id, first));
    // Codex was asked to fork its thread after the turn the journal names.
    let fork = &t.received_of("thread/fork")[0]["params"];
    assert_eq!(fork["threadId"], parent_thread.as_str());
    assert_eq!(fork["lastTurnId"], provider_first.as_str());
    assert_eq!(
        fork["cwd"],
        created.cwd.as_ref().unwrap().display().to_string()
    );
    assert!(
        t.received_of("thread/start").len() == 1,
        "only the parent was started"
    );
    let child_thread = child
        .chat
        .as_ref()
        .unwrap()
        .agent_session_id
        .clone()
        .unwrap();
    assert_ne!(
        child_thread, parent_thread,
        "the fork is a thread of its own"
    );

    let copied = t.history(child.id, None, 500).await;
    assert!(copied.events.iter().all(|e| e.session_id == child.id));
    let users = |history: &ChatHistory| -> Vec<String> {
        history
            .events
            .iter()
            .filter_map(|e| match &e.kind {
                AgentEventKind::MessageEnd {
                    role: MessageRole::User,
                    text,
                    ..
                } => Some(text.clone()),
                _ => None,
            })
            .collect()
    };
    let first_user = users(&parent)[0].clone();
    assert_eq!(users(&copied), [first_user.as_str()]);
    assert_eq!(users(&parent).len(), 2);
    assert!(copied.events.iter().any(|e| matches!(
        &e.kind,
        AgentEventKind::Notice { message, .. } if message.contains("after turn 1 of 2")
    )));

    // The fork takes turns on its own thread.
    t.send(child.id, "again").await;
    next_chat_event(&mut events, "the fork's turn to end", |e| {
        e.session_id == child.id && e.kind.ends_turn()
    })
    .await;
    assert_eq!(
        t.received_of("turn/start").last().unwrap()["params"]["threadId"],
        child_thread.as_str()
    );
    assert_eq!(
        users(&t.history(created.id, None, 500).await).len(),
        2,
        "the parent is untouched"
    );
    t.stop().await;
}
