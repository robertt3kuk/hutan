//! The ACP adapter against the fake agent (`hutan-acp-fixture replay`). `handshake.jsonl` was
//! recorded from `omp acp` 18.11.1 (initialize, session/new, session/close; no login, no model
//! call); the other transcripts are built from the ACP v1 spec and what OMP's ACP server sends
//! (read from its source), with the same message shapes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_agents::{
    AcpAdapter, AcpConfig, AcpSession, AdapterMessage, AgentAdapter, AgentError, ContextBlock,
    ForkFrom, ForkMessage, ForkPoint, ImageBlock, MessageExtras, StartSpec,
};
use hutan_core::agent::{
    FileChangeKind, MessageRole, NoticeLevel, PermissionKind, PermissionOptionKind,
    PermissionOutcome, PlanStepStatus, ToolKind, ToolStatus,
};
use hutan_core::question::{FieldKind, QuestionExpiry, Values};
use hutan_core::{
    AgentEventKind, ApprovalId, ApprovalPolicy, ForkSupport, NodeKind, NodeOutcome, NodeState,
    QuestionId, QuestionOutcome, QuestionResponse, SandboxMode,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::mpsc::Receiver;
use tokio::time::timeout;

const FIXTURE_BIN: &str = env!("CARGO_BIN_EXE_hutan-acp-fixture");
const WAIT: Duration = Duration::from_secs(20);
const SESSION: &str = "sess_fixture_1";
const OPUS: &str = "gitlab-duo/duo-chat-opus-4-6";
const HAIKU: &str = "gitlab-duo/claude-haiku-4-5-20251001";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"))
}

struct Harness {
    dir: TempDir,
    received: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let received = dir.path().join("received.jsonl");
        Self { dir, received }
    }

    fn config(&self, name: &str) -> AcpConfig {
        AcpConfig {
            agent: "omp".to_owned(),
            program: PathBuf::from(FIXTURE_BIN),
            args: vec![
                "replay".to_owned(),
                "--received".to_owned(),
                self.received.display().to_string(),
                fixture(name).display().to_string(),
                "acp".to_owned(),
            ],
            env: Vec::new(),
        }
    }

    fn spec(&self, model: Option<&str>, resume: Option<&str>) -> StartSpec {
        StartSpec {
            fork: None,
            cwd: self.dir.path().to_owned(),
            model: model.map(str::to_owned),
            approval_policy: ApprovalPolicy::default(),
            sandbox: SandboxMode::default(),
            resume_thread_id: resume.map(str::to_owned),
            developer_instructions: None,
            session_meta: None,
        }
    }

    async fn try_start(
        &self,
        name: &str,
        model: Option<&str>,
        mode: Option<&str>,
        resume: Option<&str>,
    ) -> Result<AcpSession, AgentError> {
        timeout(
            WAIT,
            AcpAdapter::start(&self.config(name), &self.spec(model, resume), mode, &[]),
        )
        .await
        .expect("start timed out")
    }

    async fn start(&self, name: &str) -> AcpSession {
        self.try_start(name, None, None, None).await.unwrap()
    }

    /// What the fake agent received, as `(method or "response", message)`.
    fn received(&self) -> Vec<(String, Value)> {
        std::fs::read_to_string(&self.received)
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let wrapper: Value = serde_json::from_str(line).unwrap();
                let msg = wrapper["msg"].clone();
                (msg["method"].as_str().unwrap_or("response").to_owned(), msg)
            })
            .collect()
    }

    fn received_of(&self, method: &str) -> Vec<Value> {
        self.received()
            .into_iter()
            .filter(|(name, _)| name == method)
            .map(|(_, msg)| msg)
            .collect()
    }
}

async fn collect_until(
    events: &mut Receiver<AdapterMessage>,
    mut stop: impl FnMut(&AdapterMessage) -> bool,
) -> Vec<AdapterMessage> {
    let mut seen = Vec::new();
    loop {
        let message = timeout(WAIT, events.recv())
            .await
            .expect("timed out waiting for an event")
            .expect("event stream ended");
        let done = stop(&message);
        seen.push(message);
        if done {
            return seen;
        }
    }
}

fn kinds(messages: &[AdapterMessage]) -> Vec<&AgentEventKind> {
    messages
        .iter()
        .filter_map(|m| match m {
            AdapterMessage::Event(draft) => Some(&draft.kind),
            AdapterMessage::Exited(_)
            | AdapterMessage::Node(_)
            | AdapterMessage::Mode(_)
            | AdapterMessage::Limits(_) => None,
        })
        .collect()
}

fn names(messages: &[AdapterMessage]) -> Vec<&'static str> {
    kinds(messages)
        .into_iter()
        .map(AgentEventKind::name)
        .collect()
}

fn ends_turn(message: &AdapterMessage) -> bool {
    matches!(message, AdapterMessage::Event(d) if d.kind.ends_turn())
}

fn is_permission_request(message: &AdapterMessage) -> bool {
    matches!(message, AdapterMessage::Event(d) if matches!(d.kind, AgentEventKind::PermissionRequested { .. }))
}

fn approval_of(messages: &[AdapterMessage]) -> ApprovalId {
    kinds(messages)
        .into_iter()
        .find_map(|k| match k {
            AgentEventKind::PermissionRequested { approval_id, .. } => Some(*approval_id),
            _ => None,
        })
        .expect("a permission request")
}

/// The start events every session begins with: the resume locator and the root node.
async fn initial(events: &mut Receiver<AdapterMessage>) -> Vec<AdapterMessage> {
    let mut seen = Vec::new();
    for _ in 0..2 {
        seen.push(timeout(WAIT, events.recv()).await.unwrap().unwrap());
    }
    seen
}

#[tokio::test]
async fn the_handshake_with_a_recorded_omp_names_the_session_and_its_models() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        started,
        mut events,
        models,
    } = h.start("handshake").await;
    assert_eq!(adapter.agent(), "omp");
    assert_eq!(started.thread_id, SESSION);
    assert_eq!(started.model, OPUS, "the model the session starts with");
    assert!(!started.resumed);
    assert_eq!(models.len(), 17);
    assert!(models.iter().any(|m| m.id == HAIKU && !m.is_default));
    assert_eq!(models.iter().filter(|m| m.is_default).count(), 1);

    let first = initial(&mut events).await;
    assert!(matches!(
        &first[0],
        AdapterMessage::Event(d) if matches!(&d.kind, AgentEventKind::ResumeLocator { agent, thread_id, model }
            if agent == "omp" && thread_id == SESSION && model.as_deref() == Some(OPUS))
    ));
    let AdapterMessage::Node(root) = &first[1] else {
        panic!("{:?}", first[1])
    };
    assert_eq!(
        (root.kind, root.state),
        (NodeKind::Main, Some(NodeState::Idle))
    );
    assert_eq!(root.id.as_deref(), Some(SESSION));

    let init = &h.received_of("initialize")[0];
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(init["params"]["protocolVersion"], 1);
    assert_eq!(
        init["params"]["clientCapabilities"]["fs"]["readTextFile"],
        false
    );
    assert_eq!(
        init["params"]["clientCapabilities"]["fs"]["writeTextFile"],
        false
    );
    assert_eq!(init["params"]["clientCapabilities"]["terminal"], false);
    // Form elicitations are served (OMP's `ask`, Claude Code's `AskUserQuestion`); urls are not.
    assert_eq!(
        init["params"]["clientCapabilities"]["elicitation"],
        json!({"form": {}})
    );
    assert_eq!(init["params"]["clientInfo"]["name"], "hutan");
    let new = &h.received_of("session/new")[0];
    assert_eq!(new["params"]["cwd"], h.dir.path().display().to_string());
    assert_eq!(new["params"]["mcpServers"], json!([]));
    adapter.shutdown().await;
    // The agent advertises session/close: it is asked before the process ends.
    assert_eq!(h.received_of("session/close").len(), 1);
}

#[tokio::test]
async fn a_plain_turn_streams_thoughts_and_messages_then_completes() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("plain-turn").await;
    initial(&mut events).await;
    let turn = adapter.send_message("Say hello.".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    assert_eq!(
        names(&seen),
        [
            "turn_started",
            "message_start",
            "message_end",
            "reasoning_start",
            "reasoning_delta",
            "reasoning_delta",
            "reasoning_end",
            "message_start",
            "message_delta",
            "message_delta",
            "usage",
            "message_end",
            "usage",
            "turn_completed",
        ]
    );
    let all = kinds(&seen);
    assert!(matches!(
        all[2],
        AgentEventKind::MessageEnd { role: MessageRole::User, text, .. } if text == "Say hello."
    ));
    assert!(matches!(
        all[6],
        AgentEventKind::ReasoningEnd { summary, .. } if summary == &["Let me think. Reply with a greeting."]
    ));
    assert!(matches!(
        all[11],
        AgentEventKind::MessageEnd { role: MessageRole::Assistant, text, .. } if text == "Hello, world."
    ));
    let AgentEventKind::Usage {
        total,
        context_window,
        ..
    } = all[10]
    else {
        panic!()
    };
    assert_eq!(total.and_then(|t| t.total_tokens), Some(1234));
    assert_eq!(*context_window, Some(200_000));
    let AgentEventKind::Usage { last, .. } = all[12] else {
        panic!()
    };
    let last = last.unwrap();
    assert_eq!(
        (
            last.input_tokens,
            last.output_tokens,
            last.cached_input_tokens
        ),
        (Some(1200), Some(34), Some(900))
    );
    // Every event of the turn carries the local turn id the call returned.
    for message in &seen {
        if let AdapterMessage::Event(draft) = message {
            assert_eq!(draft.turn_id, Some(turn.turn_id), "{:?}", draft.kind);
        }
    }
    let prompt = &h.received_of("session/prompt")[0];
    assert_eq!(prompt["params"]["sessionId"], SESSION);
    assert_eq!(
        prompt["params"]["prompt"],
        json!([{"type": "text", "text": "Say hello."}])
    );
    adapter.shutdown().await;
}

#[tokio::test]
async fn tool_calls_map_to_commands_and_file_diffs() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("tool-diff").await;
    initial(&mut events).await;
    adapter
        .send_message("run the tests and fix b".into())
        .await
        .unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    let all = kinds(&seen);

    let start = all
        .iter()
        .find(|k| {
            matches!(
                k,
                AgentEventKind::ToolStart {
                    tool: ToolKind::Command,
                    ..
                }
            )
        })
        .unwrap();
    let AgentEventKind::ToolStart { title, input, .. } = start else {
        panic!()
    };
    assert_eq!(title, "$ cargo test");
    assert_eq!(input.as_ref().unwrap()["command"], "cargo test");
    assert!(all.iter().any(|k| matches!(k, AgentEventKind::ToolUpdate { message } if message.contains("running 1 test"))));
    let end = all
        .iter()
        .find_map(|k| match k {
            AgentEventKind::ToolEnd {
                tool: ToolKind::Command,
                status,
                output,
                exit_code,
                ..
            } => Some((*status, output.clone(), *exit_code)),
            _ => None,
        })
        .unwrap();
    assert_eq!(end.0, ToolStatus::Completed);
    assert!(end.1.unwrap().contains("1 passed"));
    assert_eq!(end.2, Some(0));

    let diff = all
        .iter()
        .find_map(|k| match k {
            AgentEventKind::FileDiff { changes, .. } => Some(changes),
            _ => None,
        })
        .unwrap();
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].kind, FileChangeKind::Update);
    assert!(diff[0].path.ends_with("src/lib.rs"));
    let patch = diff[0].patch.as_deref().unwrap();
    assert!(patch.contains("-fn b() {}\n+fn b() { 1; }\n"), "{patch}");
    assert!(
        patch.starts_with("--- /tmp/hutan-fixture/src/lib.rs\n"),
        "{patch}"
    );

    let plan = all
        .iter()
        .find_map(|k| match k {
            AgentEventKind::Plan { steps, .. } => Some(steps),
            _ => None,
        })
        .unwrap();
    assert_eq!(plan.len(), 3);
    assert_eq!(plan[0].status, PlanStepStatus::Completed);
    assert_eq!(plan[1].status, PlanStepStatus::InProgress);
    assert_eq!(plan[2].text, "Ship");

    // The edit tool call is a file-change tool whose end follows the diff.
    let names = names(&seen);
    let diff_at = names.iter().position(|n| *n == "file_diff").unwrap();
    assert_eq!(names[diff_at + 1], "tool_end");
    adapter.shutdown().await;
}

#[tokio::test]
async fn an_allowed_permission_request_reaches_the_agent_with_the_chosen_option() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("permission-allow").await;
    initial(&mut events).await;
    adapter.send_message("edit lib.rs".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let approval = approval_of(&first);
    let Some(AgentEventKind::PermissionRequested {
        kind,
        title,
        detail,
        options,
        ..
    }) = kinds(&first).into_iter().last()
    else {
        panic!()
    };
    assert_eq!(*kind, PermissionKind::FileChange);
    assert_eq!(title, "edit: src/lib.rs");
    // Where it would act, then every file it names.
    let detail = detail.as_deref().unwrap();
    assert_eq!(
        detail.lines().next(),
        Some(format!("in {}", h.dir.path().display()).as_str())
    );
    assert!(detail.ends_with("src/lib.rs"));
    // Exactly what the agent offered, in its order.
    let offered: Vec<_> = options.iter().map(|o| (o.id.as_str(), o.kind)).collect();
    assert_eq!(
        offered,
        [
            ("allow_once", PermissionOptionKind::AllowOnce),
            ("allow_always", PermissionOptionKind::AllowAlways),
            ("reject_once", PermissionOptionKind::RejectOnce),
            ("reject_always", PermissionOptionKind::RejectAlways),
        ]
    );

    // An option that was not offered never reaches the agent.
    let refused = adapter.respond(approval, "approve_everything".into()).await;
    assert!(
        matches!(refused, Err(AgentError::OptionNotOffered { option }) if option == "approve_everything")
    );
    assert!(h.received_of("response").is_empty());

    adapter
        .respond(approval, "allow_once".into())
        .await
        .unwrap();
    let again = adapter.respond(approval, "allow_once".into()).await;
    assert!(
        matches!(again, Err(AgentError::UnknownApproval)),
        "answered once"
    );

    let rest = collect_until(&mut events, ends_turn).await;
    let resolved = kinds(&rest)
        .into_iter()
        .find_map(|k| match k {
            AgentEventKind::PermissionResolved {
                approval_id,
                outcome,
                ..
            } => Some((*approval_id, outcome.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(resolved.0, approval);
    assert_eq!(
        resolved.1,
        PermissionOutcome::Selected {
            option_id: "allow_once".into()
        }
    );
    assert!(matches!(
        kinds(&rest).last().unwrap(),
        AgentEventKind::TurnCompleted { .. }
    ));
    let response = &h.received_of("response")[0];
    assert_eq!(response["id"], 900);
    assert_eq!(
        response["result"],
        json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}})
    );
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_denied_request_goes_back_to_the_agent_and_the_tool_fails() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("permission-deny").await;
    initial(&mut events).await;
    adapter.send_message("edit lib.rs".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    adapter
        .respond(approval_of(&first), "reject_once".into())
        .await
        .unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert!(kinds(&rest).iter().any(|k| matches!(
        k,
        AgentEventKind::ToolEnd {
            status: ToolStatus::Failed,
            ..
        }
    )));
    assert_eq!(
        h.received_of("response")[0]["result"]["outcome"]["optionId"],
        "reject_once"
    );
    adapter.shutdown().await;
}

#[tokio::test]
async fn interrupting_cancels_the_turn_and_the_waiting_request() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("permission-cancel").await;
    initial(&mut events).await;
    adapter.send_message("edit lib.rs".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let approval = approval_of(&first);
    adapter.interrupt().await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    let order: Vec<_> = kinds(&rest)
        .into_iter()
        .map(|k| match k {
            AgentEventKind::PermissionResolved {
                approval_id,
                outcome,
                ..
            } => {
                assert_eq!(*approval_id, approval);
                assert_eq!(*outcome, PermissionOutcome::Expired);
                "expired"
            }
            other => other.name(),
        })
        .collect();
    assert_eq!(order, ["expired", "tool_end", "turn_cancelled"]);
    // The agent was told (ACP: a client that cancels must answer what it holds).
    let received = h.received();
    let names: Vec<_> = received.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(&names[names.len() - 2..], ["session/cancel", "response"]);
    assert_eq!(
        received[received.len() - 1].1["result"],
        json!({"outcome": {"outcome": "cancelled"}})
    );
    // Nothing can be chosen any more.
    assert!(matches!(
        adapter.respond(approval, "allow_once".into()).await,
        Err(AgentError::UnknownApproval)
    ));
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_request_the_agent_gave_up_on_expires_when_the_turn_ends() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("permission-expires").await;
    initial(&mut events).await;
    adapter.send_message("edit lib.rs".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    let all = kinds(&seen);
    let expired = all.iter().position(|k| {
        matches!(
            k,
            AgentEventKind::PermissionResolved {
                outcome: PermissionOutcome::Expired,
                ..
            }
        )
    });
    let completed = all
        .iter()
        .position(|k| matches!(k, AgentEventKind::TurnCompleted { .. }));
    assert!(expired.unwrap() < completed.unwrap(), "{:?}", names(&seen));
    // Leave time for the agent to read the answer it is owed.
    adapter.shutdown().await;
    let response = &h.received_of("response")[0];
    assert_eq!(
        response["result"],
        json!({"outcome": {"outcome": "cancelled"}})
    );
}

#[tokio::test]
async fn interrupting_a_streaming_turn_keeps_the_text_so_far() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("cancel").await;
    initial(&mut events).await;
    adapter.send_message("long task".into()).await.unwrap();
    // A second message while the turn runs is refused by the adapter itself.
    assert!(matches!(
        adapter.send_message("again".into()).await,
        Err(AgentError::TurnInProgress)
    ));
    collect_until(&mut events, |m| {
        matches!(m, AdapterMessage::Event(d) if matches!(d.kind, AgentEventKind::MessageDelta { .. }))
    })
    .await;
    adapter.interrupt().await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert_eq!(names(&rest), ["message_end", "turn_cancelled"]);
    assert!(matches!(
        kinds(&rest)[0],
        AgentEventKind::MessageEnd { text, .. } if text == "Working on it"
    ));
    assert!(matches!(adapter.interrupt().await, Err(AgentError::NoTurn)));
    adapter.shutdown().await;
}

#[tokio::test]
async fn updates_hutan_has_no_model_for_become_opaque_events() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        started,
        ..
    } = h.start("unknown-update").await;
    // The mode the session starts in, as `session/new` reported it (protocol 0.34).
    assert_eq!(
        started.mode,
        Some(hutan_agents::SessionMode {
            id: "default".into(),
            name: Some("Default".into()),
        })
    );
    initial(&mut events).await;
    adapter.send_message("go".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    let all = kinds(&seen);
    let opaque: Vec<_> = all
        .iter()
        .filter_map(|k| match k {
            AgentEventKind::Opaque { method } => Some(method.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(opaque, ["session/update:novel_update", "_omp/novel"]);
    // The raw payload is kept for the first.
    let payload = seen.iter().find_map(|m| match m {
        AdapterMessage::Event(d) if matches!(&d.kind, AgentEventKind::Opaque { method } if method.ends_with("novel_update")) => d.payload.clone(),
        _ => None,
    });
    assert_eq!(payload.unwrap()["update"]["payload"], json!({"x": 1}));
    // Mode and model changes of the agent are announced.
    let notices: Vec<_> = all
        .iter()
        .filter_map(|k| match k {
            AgentEventKind::Notice { level, message } => Some((*level, message.as_str())),
            _ => None,
        })
        .collect();
    assert!(
        notices.contains(&(NoticeLevel::Info, "mode: plan")),
        "{notices:?}"
    );
    // ...and reported as the session's mode, with the name the agent gave it.
    let modes: Vec<_> = seen
        .iter()
        .filter_map(|m| match m {
            AdapterMessage::Mode(mode) => Some(mode.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        modes,
        [hutan_agents::SessionMode {
            id: "plan".into(),
            name: Some("Plan".into()),
        }]
    );
    assert!(
        notices.contains(&(
            NoticeLevel::Info,
            "model: gitlab-duo/claude-opus-4-5-20251101"
        )),
        "{notices:?}"
    );
    // A request Hutan does not serve (file system) is declined, and said.
    assert!(
        notices
            .iter()
            .any(|(level, message)| *level == NoticeLevel::Warning
                && message.contains("fs/read_text_file"))
    );
    let declined = h.received_of("response");
    assert_eq!(declined[0]["error"]["code"], -32601);
    adapter.shutdown().await;
}

#[tokio::test]
async fn task_tool_calls_feed_the_agent_tree_with_subagents() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("subagents").await;
    initial(&mut events).await;
    adapter
        .send_message("explore and fix".into())
        .await
        .unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    // The node updates after the turn end (root idle) arrive right behind it.
    let tail = collect_until(&mut events, |m| matches!(m, AdapterMessage::Node(n) if n.state == Some(NodeState::Idle) && n.kind == NodeKind::Main)).await;
    let nodes: Vec<_> = seen
        .iter()
        .chain(&tail)
        .filter_map(|m| match m {
            AdapterMessage::Node(n) => Some(n.as_ref()),
            _ => None,
        })
        .collect();
    let subs: Vec<_> = nodes.iter().filter(|n| n.kind == NodeKind::Sub).collect();
    let started: Vec<_> = subs
        .iter()
        .filter(|n| n.state == Some(NodeState::Working))
        .map(|n| {
            (
                n.id.clone().unwrap(),
                n.name.clone(),
                n.agent_type.clone(),
                n.parent.clone(),
            )
        })
        .collect();
    assert_eq!(
        started,
        [
            (
                "call_task_1#0".to_owned(),
                Some("Mapper".to_owned()),
                Some("explore".to_owned()),
                Some(SESSION.to_owned())
            ),
            (
                "call_task_1#1".to_owned(),
                Some("Fixer".to_owned()),
                Some("task".to_owned()),
                Some(SESSION.to_owned())
            ),
        ]
    );
    let finished: Vec<_> = subs.iter().filter_map(|n| n.outcome).collect();
    assert_eq!(finished, [NodeOutcome::Completed, NodeOutcome::Completed]);
    adapter.shutdown().await;
}

#[tokio::test]
async fn authentication_uses_the_agents_own_method_when_the_session_asks_for_it() {
    let h = Harness::new();
    let session = h.start("auth-required").await;
    assert_eq!(session.started.thread_id, SESSION);
    let auth = h.received_of("authenticate");
    assert_eq!(auth.len(), 1);
    assert_eq!(auth[0]["params"]["methodId"], "agent");
    assert_eq!(
        h.received_of("session/new").len(),
        2,
        "asked again after authenticating"
    );
    session.adapter.shutdown().await;
}

#[tokio::test]
async fn a_login_hutan_cannot_do_for_the_agent_is_an_error_that_says_so() {
    let h = Harness::new();
    let error = h
        .try_start("auth-terminal-only", None, None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, AgentError::AuthRequired { detail } if detail.contains("hutan account login")),
        "{error}"
    );
    assert!(
        h.received_of("authenticate").is_empty(),
        "a terminal method is not Hutan's to run"
    );

    let h = Harness::new();
    let error = h
        .try_start("auth-rejected", None, None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, AgentError::AuthRequired { detail } if detail.contains("no credentials")),
        "{error}"
    );
}

#[tokio::test]
async fn an_agent_of_another_protocol_version_is_refused_with_both_versions() {
    let h = Harness::new();
    let error = h
        .try_start("old-protocol", None, None, None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AgentError::UnsupportedAcp {
            found: 2,
            supported: 1
        }
    ));
    let text = error.to_string();
    assert!(
        text.contains("version 2") && text.contains("version 1"),
        "{text}"
    );
}

#[tokio::test]
async fn a_conversation_is_continued_with_session_load_and_its_replay_is_dropped() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        started,
        mut events,
        ..
    } = h
        .try_start("load", None, None, Some(SESSION))
        .await
        .unwrap();
    assert!(started.resumed);
    assert_eq!(started.thread_id, SESSION);
    let load = &h.received_of("session/load")[0];
    assert_eq!(load["params"]["sessionId"], SESSION);
    assert_eq!(load["params"]["cwd"], h.dir.path().display().to_string());
    assert!(h.received_of("session/new").is_empty());

    adapter.send_message("again".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    // The journal is the history: the agent's replay of earlier messages is not in the stream.
    let texts: Vec<_> = kinds(&seen)
        .into_iter()
        .filter_map(|k| match k {
            AgentEventKind::MessageEnd { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["again", "Welcome back."]);
    assert!(matches!(
        kinds(&seen)[0],
        AgentEventKind::ResumeLocator { thread_id, .. } if thread_id == SESSION
    ));
    adapter.shutdown().await;
}

#[tokio::test]
async fn an_agent_without_session_load_is_resumed_with_session_resume() {
    let h = Harness::new();
    let session = h
        .try_start("resume-only", None, None, Some(SESSION))
        .await
        .unwrap();
    assert!(session.started.resumed);
    assert_eq!(
        h.received_of("session/resume")[0]["params"]["sessionId"],
        SESSION
    );
    assert!(h.received_of("session/load").is_empty());
    session.adapter.shutdown().await;
}

#[tokio::test]
async fn an_agent_that_cannot_continue_conversations_says_so() {
    let h = Harness::new();
    let error = h
        .try_start("no-resume", None, None, Some(SESSION))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, AgentError::Unsupported(m) if m.contains("session/load")),
        "{error}"
    );

    let h = Harness::new();
    let error = h
        .try_start("load-fails", None, None, Some(SESSION))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, AgentError::Rpc { method, message, .. } if method == "session/load" && message.contains("not found")),
        "{error}"
    );
}

#[tokio::test]
async fn models_are_chosen_from_the_sessions_offer_and_switched_through_config_options() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        started,
        mut events,
        ..
    } = h
        .try_start("model-switch", Some(HAIKU), None, None)
        .await
        .unwrap();
    assert_eq!(started.model, HAIKU);
    let set = &h.received_of("session/set_config_option")[0];
    assert_eq!(
        set["params"],
        json!({"sessionId": SESSION, "configId": "model", "value": HAIKU})
    );

    // A later switch, and the notice that records it.
    adapter.set_model(OPUS.into()).await.unwrap();
    let notice = collect_until(&mut events, |m| {
        matches!(m, AdapterMessage::Event(d) if matches!(&d.kind, AgentEventKind::Notice { message, .. } if message.starts_with("model:")))
    })
    .await;
    assert!(matches!(
        kinds(&notice).last().unwrap(),
        AgentEventKind::Notice { message, .. } if message == &format!("model: {OPUS}")
    ));
    assert_eq!(h.received_of("session/set_config_option").len(), 2);
    let unknown = adapter.set_model("nobody/nothing".into()).await;
    assert!(
        matches!(unknown, Err(AgentError::UnknownModel { model, available }) if model == "nobody/nothing" && available.contains(HAIKU))
    );
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_model_the_session_does_not_offer_fails_the_start_naming_the_choices() {
    let h = Harness::new();
    let error = h
        .try_start("plain-turn", Some("nobody/nothing"), None, None)
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains("nobody/nothing") && text.contains(HAIKU),
        "{text}"
    );
    assert!(h.received_of("session/set_config_option").is_empty());
}

#[tokio::test]
async fn a_mode_is_set_from_the_modes_the_session_offers() {
    let h = Harness::new();
    let session = h
        .try_start("mode-plan", None, Some("plan"), None)
        .await
        .unwrap();
    assert_eq!(
        h.received_of("session/set_mode")[0]["params"],
        json!({"sessionId": SESSION, "modeId": "plan"})
    );
    session.adapter.shutdown().await;

    let h = Harness::new();
    let error = h
        .try_start("plain-turn", None, Some("yolo"), None)
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains("yolo") && text.contains("default, plan"),
        "{text}"
    );
}

#[tokio::test]
async fn session_meta_rides_on_session_new_and_on_the_load_that_continues_it() {
    let meta = json!({"claudeCode": {"options": {"sandbox": {"enabled": true}}}});
    let h = Harness::new();
    let mut spec = h.spec(None, None);
    spec.session_meta = Some(meta.clone());
    let session = timeout(
        WAIT,
        AcpAdapter::start(&h.config("plain-turn"), &spec, None, &[]),
    )
    .await
    .expect("start timed out")
    .unwrap();
    assert_eq!(h.received_of("session/new")[0]["params"]["_meta"], meta);
    session.adapter.shutdown().await;

    let h = Harness::new();
    let mut spec = h.spec(None, Some(SESSION));
    spec.session_meta = Some(meta.clone());
    let session = timeout(WAIT, AcpAdapter::start(&h.config("load"), &spec, None, &[]))
        .await
        .expect("start timed out")
        .unwrap();
    assert_eq!(h.received_of("session/load")[0]["params"]["_meta"], meta);
    session.adapter.shutdown().await;

    // Without meta the request carries none: agents never see a `_meta` Hutan did not mean.
    let h = Harness::new();
    let session = h.start("plain-turn").await;
    assert!(
        h.received_of("session/new")[0]["params"]
            .get("_meta")
            .is_none()
    );
    session.adapter.shutdown().await;
}

#[tokio::test]
async fn a_dying_agent_ends_the_stream_with_its_exit() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("exit-mid-turn").await;
    initial(&mut events).await;
    adapter.send_message("go".into()).await.unwrap();
    let seen = collect_until(&mut events, |m| matches!(m, AdapterMessage::Exited(_))).await;
    let AdapterMessage::Exited(info) = seen.last().unwrap() else {
        panic!()
    };
    assert_eq!(info.code, Some(1));
    // The turn was not completed by the adapter: the daemon closes it.
    assert!(!kinds(&seen).iter().any(|k| k.ends_turn()));
    assert!(adapter.send_message("more".into()).await.is_err());
}

#[tokio::test]
async fn failures_of_the_prompt_are_failed_turns() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("turn-error").await;
    initial(&mut events).await;
    adapter.send_message("go".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    assert!(matches!(
        kinds(&seen).last().unwrap(),
        AgentEventKind::TurnFailed { message, .. } if message.contains("model overloaded")
    ));
    adapter.shutdown().await;

    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("refusal").await;
    initial(&mut events).await;
    adapter.send_message("go".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    assert!(matches!(
        kinds(&seen).last().unwrap(),
        AgentEventKind::TurnFailed { message, .. } if message.contains("refused")
    ));
    adapter.shutdown().await;

    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("max-tokens").await;
    initial(&mut events).await;
    adapter.send_message("go".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    let all = kinds(&seen);
    assert!(all.iter().any(|k| matches!(k, AgentEventKind::Notice { level: NoticeLevel::Warning, message } if message.contains("output limit"))));
    assert!(matches!(
        all.last().unwrap(),
        AgentEventKind::TurnCompleted { .. }
    ));
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_second_turn_follows_the_first() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("two-turns").await;
    initial(&mut events).await;
    let first = adapter.send_message("one".into()).await.unwrap();
    collect_until(&mut events, ends_turn).await;
    let second = adapter.send_message("two".into()).await.unwrap();
    assert_ne!(first.turn_id, second.turn_id);
    let seen = collect_until(&mut events, ends_turn).await;
    assert!(
        kinds(&seen)
            .iter()
            .any(|k| matches!(k, AgentEventKind::MessageEnd { text, .. } if text == "two"))
    );
    adapter.shutdown().await;
}

#[tokio::test]
async fn attached_context_is_sent_as_embedded_resources() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("plain-turn").await;
    initial(&mut events).await;
    adapter
        .send_message_with(
            "explain".into(),
            MessageExtras {
                context: vec![ContextBlock {
                    label: "shell: cargo test".into(),
                    text: "[shell: cargo test · exit 1]\n1 failed".into(),
                }],
                images: Vec::new(),
            },
        )
        .await
        .unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    // The journal shows what the agent was given.
    assert!(kinds(&seen).iter().any(|k| matches!(k, AgentEventKind::MessageEnd { role: MessageRole::User, text, .. } if text.contains("explain") && text.contains("1 failed"))));
    let prompt = &h.received_of("session/prompt")[0]["params"]["prompt"];
    assert_eq!(prompt[0], json!({"type": "text", "text": "explain"}));
    assert_eq!(prompt[1]["type"], "resource");
    assert_eq!(
        prompt[1]["resource"]["uri"],
        "hutan://context/1/shell--cargo-test"
    );
    assert!(
        prompt[1]["resource"]["text"]
            .as_str()
            .unwrap()
            .contains("1 failed")
    );
    adapter.shutdown().await;
}

fn png() -> ImageBlock {
    ImageBlock {
        header: "[image: shot.png · image/png · 8 B]".into(),
        media_type: "image/png".into(),
        data: std::sync::Arc::new(b"\x89PNG\r\n\x1a\nxyz".to_vec()),
        path: None,
    }
}

#[tokio::test]
async fn images_are_sent_as_image_blocks_when_the_agent_takes_them() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        started,
        mut events,
        ..
    } = h.start("plain-turn").await;
    // The recorded handshake advertises `promptCapabilities.image`.
    assert_eq!(started.images, Some(true));
    initial(&mut events).await;
    adapter
        .send_message_with(
            "what is this?".into(),
            MessageExtras {
                context: vec![ContextBlock {
                    label: "pasted text".into(),
                    text: "[pasted text: 1 line]\nhello".into(),
                }],
                images: vec![png()],
            },
        )
        .await
        .unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    // The journal names the image without its bytes.
    assert!(kinds(&seen).iter().any(|k| matches!(k, AgentEventKind::MessageEnd { role: MessageRole::User, text, .. } if text.contains("what is this?") && text.contains("[image: shot.png · image/png · 8 B]") && !text.contains("PNG"))));
    let prompt = &h.received_of("session/prompt")[0]["params"]["prompt"];
    assert_eq!(prompt[0], json!({"type": "text", "text": "what is this?"}));
    assert_eq!(prompt[1]["type"], "resource");
    assert_eq!(
        prompt[2],
        json!({
            "type": "image",
            "mimeType": "image/png",
            "data": "iVBORw0KGgp4eXo=",
        })
    );
    adapter.shutdown().await;
}

#[tokio::test]
async fn images_are_refused_when_the_agent_does_not_take_them() {
    let h = Harness::new();
    let text = std::fs::read_to_string(fixture("plain-turn")).unwrap();
    assert!(text.contains("\"image\":true"));
    let without = h.dir.path().join("no-images");
    std::fs::write(
        without.with_extension("jsonl"),
        text.replace("\"image\":true", "\"image\":false"),
    )
    .unwrap();
    let AcpSession {
        adapter,
        started,
        mut events,
        ..
    } = h.start(&without.display().to_string()).await;
    assert_eq!(started.images, Some(false));
    initial(&mut events).await;
    let refused = adapter
        .send_message_with(
            "look".into(),
            MessageExtras {
                context: Vec::new(),
                images: vec![png()],
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, AgentError::Unsupported(message) if message.contains("does not accept images")),
        "{refused:?}"
    );
    // Nothing was sent, no turn began, and the agent is still usable.
    assert!(h.received_of("session/prompt").is_empty());
    adapter.send_message("plain".into()).await.unwrap();
    collect_until(&mut events, ends_turn).await;
    adapter.shutdown().await;
}

// Questions: `elicitation/create` ----------------------------------------------------------------
//
// OMP 18.11.2 (`src/modes/acp/acp-agent.ts`) and `@agentclientprotocol/claude-agent-acp` 0.67
// (`dist/elicitation.js`) send these when the client advertises `elicitation.form`; the
// transcripts are built from their sources and the ACP schema (1.3.0), not recorded, because a
// recording needs a model turn.

fn is_question_request(message: &AdapterMessage) -> bool {
    matches!(message, AdapterMessage::Event(d) if matches!(d.kind, AgentEventKind::QuestionRequested { .. }))
}

fn question_of(messages: &[AdapterMessage]) -> QuestionId {
    kinds(messages)
        .into_iter()
        .find_map(|k| match k {
            AgentEventKind::QuestionRequested { question_id, .. } => Some(*question_id),
            _ => None,
        })
        .expect("a question")
}

fn values(pairs: &[(&str, Value)]) -> Values {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

#[tokio::test]
async fn an_omp_ask_dialog_becomes_a_form_and_the_answer_reaches_omp_as_accept_with_content() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("question-ask").await;
    initial(&mut events).await;
    adapter.send_message("decide".into()).await.unwrap();
    let first = collect_until(&mut events, is_question_request).await;
    let question = question_of(&first);
    // The open message ends before the question, like before a permission request.
    assert!(names(&first).contains(&"message_end"));
    let Some(AgentEventKind::QuestionRequested { spec, .. }) = kinds(&first).into_iter().last()
    else {
        panic!()
    };
    assert_eq!(spec.title, "Which database should the migration target?");
    assert_eq!(spec.expires, QuestionExpiry::Turn);
    assert_eq!(
        spec.fields
            .iter()
            .map(|f| f.id.as_str())
            .collect::<Vec<_>>(),
        ["q0", "q0__other"]
    );
    assert_eq!(spec.fields[0].description.as_deref(), Some("Database"));
    assert_eq!(
        spec.fields[0].default,
        Some(Value::from("PostgreSQL (Recommended)"))
    );
    assert!(matches!(
        &spec.fields[0].kind,
        FieldKind::Choice { options, multi: false, .. }
            if options[0].description.as_deref() == Some("Production default")
    ));
    assert!(matches!(spec.fields[1].kind, FieldKind::Text));

    adapter
        .answer_question(
            question,
            QuestionResponse::Submit {
                values: values(&[("q0", Value::from("SQLite"))]),
            },
        )
        .await
        .unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert!(kinds(&rest).iter().any(|k| matches!(
        k,
        AgentEventKind::QuestionResolved {
            question_id,
            outcome: QuestionOutcome::Answered { values, withheld },
            ..
        } if *question_id == question && values["q0"] == "SQLite" && withheld.is_empty()
    )));
    adapter.shutdown().await;
    assert_eq!(
        h.received_of("response")[0]["result"],
        json!({"action": "accept", "content": {"q0": "SQLite"}})
    );
}

#[tokio::test]
async fn claude_codes_ask_user_question_maps_choices_multi_selects_and_its_custom_boxes() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("question-claude-ask").await;
    initial(&mut events).await;
    adapter.send_message("ask".into()).await.unwrap();
    let first = collect_until(&mut events, is_question_request).await;
    let question = question_of(&first);
    let Some(AgentEventKind::QuestionRequested { spec, .. }) = kinds(&first).into_iter().last()
    else {
        panic!()
    };
    assert_eq!(spec.title, "Please answer the following questions.");
    let ids: Vec<&str> = spec.fields.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "question_0",
            "question_0_custom",
            "question_1",
            "question_1_custom"
        ]
    );
    assert_eq!(spec.fields[0].label, "Scope");
    assert_eq!(
        spec.fields[0].description.as_deref(),
        Some("Which part should change?")
    );
    assert!(matches!(
        &spec.fields[2].kind,
        FieldKind::Choice { options, multi: true, .. } if options.len() == 3
    ));
    // The question hangs off the tool call it came from.
    let tool_item = kinds(&first)
        .iter()
        .position(|k| matches!(k, AgentEventKind::ToolStart { .. }))
        .unwrap();
    let AdapterMessage::Event(request) =
        first.iter().rev().find(|m| is_question_request(m)).unwrap()
    else {
        panic!()
    };
    let AdapterMessage::Event(tool) = first
        .iter()
        .filter(|m| matches!(m, AdapterMessage::Event(_)))
        .nth(tool_item)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(request.item_id, tool.item_id);

    // Decline: Claude tells the model the user skipped.
    adapter
        .answer_question(question, QuestionResponse::Decline)
        .await
        .unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert!(kinds(&rest).iter().any(|k| matches!(
        k,
        AgentEventKind::QuestionResolved {
            outcome: QuestionOutcome::Declined,
            ..
        }
    )));
    adapter.shutdown().await;
    assert_eq!(
        h.received_of("response")[0]["result"],
        json!({"action": "decline"})
    );
}

#[tokio::test]
async fn omps_plan_approval_is_a_choice_question() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("question-plan-approval").await;
    initial(&mut events).await;
    adapter.send_message("plan".into()).await.unwrap();
    let first = collect_until(&mut events, is_question_request).await;
    let question = question_of(&first);
    let Some(AgentEventKind::QuestionRequested { spec, .. }) = kinds(&first).into_iter().last()
    else {
        panic!()
    };
    // The first line titles the card, the rest (the plan's preview) is its body.
    assert!(
        spec.title
            .starts_with("Approve plan \"Add the questions card\""),
        "{}",
        spec.title
    );
    assert!(
        spec.prompt.starts_with("1. Map the schema"),
        "{}",
        spec.prompt
    );
    let FieldKind::Choice { options, .. } = &spec.fields[0].kind else {
        panic!("{:?}", spec.fields[0].kind)
    };
    assert_eq!(options.len(), 2);
    assert!(spec.fields[0].required);
    adapter
        .answer_question(
            question,
            QuestionResponse::Submit {
                values: values(&[("value", Value::from("Approve and execute"))]),
            },
        )
        .await
        .unwrap();
    collect_until(&mut events, ends_turn).await;
    adapter.shutdown().await;
    assert_eq!(
        h.received_of("response")[0]["result"],
        json!({"action": "accept", "content": {"value": "Approve and execute"}})
    );
}

#[tokio::test]
async fn an_unanswered_question_expires_with_its_turn_and_the_agent_is_told_cancel() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("question-expires").await;
    initial(&mut events).await;
    adapter.send_message("ask".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    let all = kinds(&seen);
    let expired = all.iter().position(|k| {
        matches!(
            k,
            AgentEventKind::QuestionResolved {
                outcome: QuestionOutcome::Expired,
                ..
            }
        )
    });
    let completed = all
        .iter()
        .position(|k| matches!(k, AgentEventKind::TurnCompleted { .. }));
    assert!(expired.unwrap() < completed.unwrap(), "{:?}", names(&seen));
    adapter.shutdown().await;
    assert_eq!(
        h.received_of("response")[0]["result"],
        json!({"action": "cancel"})
    );
    // Too late to answer.
    let error = adapter
        .answer_question(question_of(&seen), QuestionResponse::Decline)
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::UnknownQuestion), "{error}");
}

#[tokio::test]
async fn interrupting_cancels_a_waiting_question_before_the_turn_ends() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("question-cancel").await;
    initial(&mut events).await;
    adapter.send_message("ask".into()).await.unwrap();
    let first = collect_until(&mut events, is_question_request).await;
    let question = question_of(&first);
    adapter.interrupt().await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert!(kinds(&rest).iter().any(|k| matches!(
        k,
        AgentEventKind::QuestionResolved {
            outcome: QuestionOutcome::Expired,
            ..
        }
    )));
    // The agent was told to cancel: the user cannot answer any more.
    let error = adapter
        .answer_question(question, QuestionResponse::Decline)
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::UnknownQuestion), "{error}");
    let received = h.received();
    let names: Vec<_> = received.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(&names[names.len() - 2..], ["session/cancel", "response"]);
    assert_eq!(
        received[received.len() - 1].1["result"],
        json!({"action": "cancel"})
    );
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_url_elicitation_is_declined_with_a_notice_because_only_forms_are_offered() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("question-url").await;
    initial(&mut events).await;
    adapter.send_message("auth".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    assert!(names(&seen).contains(&"notice"));
    assert!(!names(&seen).contains(&"question_requested"));
    adapter.shutdown().await;
    assert_eq!(
        h.received_of("response")[0]["result"],
        json!({"action": "decline"})
    );
}

#[test]
fn committed_fixtures_are_already_scrubbed() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp-omp-18.11");
    let mut seen = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let scrubbed = hutan_agents::fixture::acp_scrub::scrub(&text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(scrubbed, text, "{} is not scrubbed", path.display());
        seen += 1;
    }
    assert!(seen >= 25, "{seen} fixtures");
}

#[tokio::test]
async fn claude_rate_limits_in_a_usage_update_become_limit_windows() {
    // Claude Code's ACP adapter forwards the SDK's `rate_limit_event` in
    // `usage_update._meta["_claude/rateLimit"]` (payload as quoted in
    // anthropics/claude-code#91857); the usage event of the turn is unchanged.
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("claude-rate-limit").await;
    initial(&mut events).await;
    adapter.send_message("Say hello.".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    let limits: Vec<_> = seen
        .iter()
        .filter_map(|m| match m {
            AdapterMessage::Limits(limits) => Some(limits),
            _ => None,
        })
        .collect();
    assert_eq!(limits.len(), 1, "{seen:?}");
    assert_eq!(limits[0].provider, "anthropic");
    let windows: Vec<_> = limits[0]
        .windows
        .iter()
        .map(|w| (w.id.as_str(), w.used_percent.round(), w.resets_at_ms))
        .collect();
    assert_eq!(
        windows,
        [
            ("5h", 24.0, Some(1_788_465_600_000)),
            ("7d", 13.0, Some(1_789_005_600_000))
        ]
    );
    assert!(names(&seen).contains(&"usage"));
}

// Fork (ADR 0023). The transcripts are built from the ACP `session/fork` schema, OMP's handshake
// (recorded) and what `claude-agent-acp` 0.98.0 answers (its source was read, no login here):
// they are scrubbed like the recordings.

fn fork_spec(h: &Harness, point: Option<ForkPoint>) -> StartSpec {
    StartSpec {
        fork: Some(ForkFrom {
            thread_id: SESSION.to_owned(),
            point,
        }),
        ..h.spec(None, None)
    }
}

fn claude(h: &Harness, name: &str) -> AcpConfig {
    AcpConfig {
        agent: "claude".to_owned(),
        ..h.config(name)
    }
}

fn message_point() -> ForkPoint {
    ForkPoint {
        provider_turn_id: None,
        message: Some(ForkMessage {
            id: "msg-1".to_owned(),
            fingerprint: "sha256:abc123".to_owned(),
            occurrence: 2,
        }),
    }
}

async fn try_fork(config: AcpConfig, spec: StartSpec) -> Result<AcpSession, AgentError> {
    timeout(WAIT, AcpAdapter::start(&config, &spec, None, &[]))
        .await
        .expect("start timed out")
}

#[tokio::test]
async fn an_omp_fork_at_the_end_is_a_new_session_that_takes_turns() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        started,
        mut events,
        ..
    } = try_fork(h.config("fork-head"), fork_spec(&h, None))
        .await
        .unwrap();
    assert_eq!(started.thread_id, "sess_fixture_2", "the new session's id");
    assert!(!started.resumed);
    assert_eq!(
        started.fork,
        Some(ForkSupport::Head),
        "OMP forks at the end"
    );

    let fork = &h.received_of("session/fork")[0]["params"];
    assert_eq!(fork["sessionId"], SESSION);
    assert_eq!(fork["cwd"], h.dir.path().display().to_string());
    assert_eq!(fork["mcpServers"], json!([]));
    assert!(fork.get("_meta").is_none(), "no fork point: {fork}");
    assert!(h.received_of("session/new").is_empty());
    assert!(h.received_of("session/load").is_empty());

    adapter.send_message("hi".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    assert!(matches!(
        kinds(&seen)[0],
        AgentEventKind::ResumeLocator { thread_id, .. } if thread_id == "sess_fixture_2"
    ));
    assert_eq!(
        h.received_of("session/prompt")[0]["params"]["sessionId"],
        "sess_fixture_2"
    );
    adapter.shutdown().await;
}

#[tokio::test]
async fn omp_cannot_cut_at_a_turn_and_says_so_before_asking() {
    let h = Harness::new();
    let error = try_fork(h.config("fork-head"), fork_spec(&h, Some(message_point())))
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(
        matches!(&error, AgentError::Unsupported(text) if text.contains("only at the end")),
        "{error}"
    );
    assert!(h.received_of("session/fork").is_empty());
}

#[tokio::test]
async fn claude_code_cuts_at_a_message_through_the_adapters_fork_point() {
    let h = Harness::new();
    let mut spec = fork_spec(&h, Some(message_point()));
    spec.session_meta = Some(json!({"claudeCode": {"options": {"sandbox": {"enabled": true}}}}));
    let session = try_fork(claude(&h, "claude-fork-exact"), spec)
        .await
        .unwrap();
    assert_eq!(session.started.fork, Some(ForkSupport::Exact));
    assert_eq!(session.started.thread_id, "sess_fixture_2");
    let meta = &h.received_of("session/fork")[0]["params"]["_meta"];
    assert_eq!(
        meta["jetbrains"]["air"]["fork"],
        json!({
            "version": 1,
            "messageId": "msg-1",
            "messageFingerprint": "sha256:abc123",
            "messageOccurrence": 2
        })
    );
    assert_eq!(
        meta["claudeCode"]["options"]["sandbox"]["enabled"], true,
        "the session's own meta rides along"
    );
    session.adapter.shutdown().await;

    // No point: no `jetbrains` key, the adapter forks the whole session.
    let h = Harness::new();
    let session = try_fork(claude(&h, "claude-fork-exact"), fork_spec(&h, None))
        .await
        .unwrap();
    assert!(
        h.received_of("session/fork")[0]["params"]
            .get("_meta")
            .is_none()
    );
    session.adapter.shutdown().await;
}

#[tokio::test]
async fn an_older_claude_adapter_forks_at_the_end_only_and_never_gets_a_fork_point_it_would_ignore()
{
    let h = Harness::new();
    let session = try_fork(claude(&h, "claude-fork-old"), fork_spec(&h, None))
        .await
        .unwrap();
    assert_eq!(session.started.fork, Some(ForkSupport::Head), "0.73.0");
    session.adapter.shutdown().await;

    let h = Harness::new();
    let error = try_fork(
        claude(&h, "claude-fork-old"),
        fork_spec(&h, Some(message_point())),
    )
    .await
    .map(|_| ())
    .unwrap_err();
    assert!(matches!(error, AgentError::Unsupported(_)), "{error}");
    assert!(
        h.received_of("session/fork").is_empty(),
        "an adapter that would ignore the point must not be asked"
    );
}

#[tokio::test]
async fn an_agent_without_session_fork_offers_a_summary_only() {
    let h = Harness::new();
    let session = h.start("no-fork").await;
    assert_eq!(session.started.fork, Some(ForkSupport::Summary));
    session.adapter.shutdown().await;

    let h = Harness::new();
    let error = try_fork(h.config("no-fork"), fork_spec(&h, None))
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(
        matches!(&error, AgentError::Unsupported(text) if text.contains("cannot fork")),
        "{error}"
    );
}

#[tokio::test]
async fn the_agents_message_id_is_journaled_with_its_assistant_messages() {
    let h = Harness::new();
    let AcpSession {
        adapter,
        mut events,
        ..
    } = h.start("plain-turn").await;
    initial(&mut events).await;
    adapter.send_message("hi".into()).await.unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    let assistant: Vec<_> = seen
        .iter()
        .filter_map(|m| match m {
            AdapterMessage::Event(draft) => Some(draft),
            _ => None,
        })
        .filter(|draft| {
            matches!(
                draft.kind,
                AgentEventKind::MessageStart {
                    role: MessageRole::Assistant,
                    ..
                } | AgentEventKind::MessageEnd {
                    role: MessageRole::Assistant,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(assistant.len(), 2, "a start and an end");
    for draft in assistant {
        assert_eq!(
            draft.provider.item_id.as_deref(),
            Some("msg-1"),
            "{:?}",
            draft.kind
        );
    }
    // Thoughts have their own ids and are not what a fork cuts at.
    assert!(seen.iter().all(|m| match m {
        AdapterMessage::Event(draft)
            if matches!(draft.kind, AgentEventKind::ReasoningEnd { .. }) =>
            draft.provider.item_id.is_none(),
        _ => true,
    }));
    adapter.shutdown().await;
}
