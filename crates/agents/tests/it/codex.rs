//! The Codex adapter against the fake app server (`hutan-codex-fixture replay`) replaying
//! transcripts recorded from the real codex 0.162.1.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_agents::{
    AdapterMessage, AgentAdapter, AgentError, CodexAppServerAdapter, CodexConfig, CodexSession,
    StartSpec,
};
use hutan_core::agent::{
    FileChangeKind, MessageRole, PermissionKind, PermissionOptionKind, PermissionOutcome, ToolKind,
    ToolStatus,
};
use hutan_core::question::{FieldKind, QuestionExpiry, Values};
use hutan_core::{
    AgentEventDraft, AgentEventKind, ApprovalId, ApprovalPolicy, QuestionId, QuestionOutcome,
    QuestionResponse, SandboxMode,
};
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::timeout;

const FIXTURE_BIN: &str = env!("CARGO_BIN_EXE_hutan-codex-fixture");
const WAIT: Duration = Duration::from_secs(20);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex-0.162")
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

    fn config(&self, transcript: &Path) -> CodexConfig {
        CodexConfig {
            program: PathBuf::from(FIXTURE_BIN),
            args: vec![
                "replay".to_owned(),
                "--received".to_owned(),
                self.received.display().to_string(),
                transcript.display().to_string(),
                "app-server".to_owned(),
            ],
            env: Vec::new(),
        }
    }

    fn spec(&self, resume: Option<&str>) -> StartSpec {
        self.spec_with(resume, None)
    }

    fn spec_with(&self, resume: Option<&str>, instructions: Option<&str>) -> StartSpec {
        StartSpec {
            cwd: self.dir.path().to_owned(),
            model: None,
            approval_policy: ApprovalPolicy::Untrusted,
            sandbox: SandboxMode::ReadOnly,
            resume_thread_id: resume.map(str::to_owned),
            developer_instructions: instructions.map(str::to_owned),
        }
    }

    async fn start(&self, transcript: &str) -> CodexSession {
        self.start_with(&fixture(transcript), None).await
    }

    async fn start_with(&self, transcript: &Path, resume: Option<&str>) -> CodexSession {
        timeout(
            WAIT,
            CodexAppServerAdapter::start(&self.config(transcript), &self.spec(resume)),
        )
        .await
        .expect("start timed out")
        .expect("start failed")
    }

    /// What the fake server received, as `(method or "response", message)`.
    fn received(&self) -> Vec<(String, Value)> {
        std::fs::read_to_string(&self.received)
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let wrapper: Value = serde_json::from_str(line).unwrap();
                let msg = wrapper["msg"].clone();
                let name = msg["method"].as_str().unwrap_or("response").to_owned();
                (name, msg)
            })
            .collect()
    }
}

/// Reads messages until `stop` says so (inclusive); returns them all.
async fn collect_until(
    events: &mut tokio::sync::mpsc::Receiver<AdapterMessage>,
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

fn is_permission_request(message: &AdapterMessage) -> bool {
    matches!(message, AdapterMessage::Event(d) if matches!(d.kind, AgentEventKind::PermissionRequested { .. }))
}

fn ends_turn(message: &AdapterMessage) -> bool {
    matches!(message, AdapterMessage::Event(d) if d.kind.ends_turn())
}

fn requested(messages: &[AdapterMessage]) -> (ApprovalId, &AgentEventDraft) {
    messages
        .iter()
        .find_map(|m| match m {
            AdapterMessage::Event(draft) => match &draft.kind {
                AgentEventKind::PermissionRequested { approval_id, .. } => {
                    Some((*approval_id, draft.as_ref()))
                }
                _ => None,
            },
            AdapterMessage::Exited(_)
            | AdapterMessage::Node(_)
            | AdapterMessage::Mode(_)
            | AdapterMessage::Limits(_) => None,
        })
        .expect("a permission request")
}

#[tokio::test]
async fn a_command_turn_with_an_approval_maps_to_the_unified_events() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        started,
        mut events,
    } = h.start("approval-touch").await;
    assert_eq!(started.model, "gpt-6.1-sol", "the catalog's default model");
    assert!(!started.resumed);
    assert_eq!(adapter.agent(), "codex");

    let turn = adapter
        .send_message("Run `touch hi.txt` in a shell, then reply with the word done.".into())
        .await
        .unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, draft) = requested(&first);
    let AgentEventKind::PermissionRequested {
        kind,
        title,
        detail,
        options,
        ..
    } = &draft.kind
    else {
        unreachable!()
    };
    assert_eq!(*kind, PermissionKind::Command);
    assert!(title.contains("touch hi.txt"), "{title}");
    assert!(
        detail
            .as_deref()
            .unwrap_or("")
            .contains("/tmp/hutan-fixture")
    );
    // Exactly what the server offered, in its order.
    let offered: Vec<_> = options.iter().map(|o| (o.id.as_str(), o.kind)).collect();
    assert_eq!(
        offered,
        [
            ("accept", PermissionOptionKind::AllowOnce),
            (
                "acceptWithExecpolicyAmendment",
                PermissionOptionKind::AllowAlways
            ),
            ("cancel", PermissionOptionKind::Cancel),
        ]
    );
    assert_eq!(options[0].provider, Some(Value::String("accept".into())));
    assert!(
        options[1].label.contains("touch hi.txt"),
        "{}",
        options[1].label
    );
    assert_eq!(draft.turn_id, Some(turn.turn_id), "local turn id is stable");
    assert_eq!(
        draft.provider.turn_id.as_deref(),
        Some(turn.provider_turn_id.as_str())
    );
    assert!(draft.provider.request_id.is_some());

    // The first part of the turn: user message echo, commentary, tool start.
    let before = names(&first);
    assert_eq!(
        &before[..4],
        [
            "resume_locator",
            "turn_started",
            "message_start",
            "message_end"
        ]
    );
    assert!(
        before.contains(&"tool_start") && before.contains(&"message_delta"),
        "{before:?}"
    );

    adapter.respond(approval, "accept".into()).await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    let after = names(&rest);
    assert_eq!(after.first(), Some(&"permission_resolved"), "{after:?}");
    assert_eq!(after.last(), Some(&"turn_completed"), "{after:?}");
    for expected in ["tool_end", "message_end", "usage"] {
        assert!(after.contains(&expected), "{expected} in {after:?}");
    }
    let resolved = kinds(&rest)[0];
    assert_eq!(
        *resolved,
        AgentEventKind::PermissionResolved {
            approval_id: approval,
            outcome: PermissionOutcome::Selected {
                option_id: "accept".into()
            },
            decided_by: None,
        }
    );
    let tool_end = kinds(&rest)
        .into_iter()
        .find(|k| matches!(k, AgentEventKind::ToolEnd { .. }))
        .unwrap();
    assert!(matches!(
        tool_end,
        AgentEventKind::ToolEnd {
            tool: ToolKind::Command,
            status: ToolStatus::Completed,
            ..
        }
    ));
    let final_text = kinds(&rest)
        .into_iter()
        .filter_map(|k| match k {
            AgentEventKind::MessageEnd {
                role: MessageRole::Assistant,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .next_back();
    assert_eq!(final_text, Some("done"));

    // The provider got the decision it was offered, answering its own request id.
    let received = h.received();
    let response = received
        .iter()
        .find(|(name, _)| name == "response")
        .unwrap();
    assert_eq!(response.1["id"], 0);
    assert_eq!(response.1["result"]["decision"], "accept");
    let methods: Vec<_> = received.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "initialized",
            "model/list",
            "thread/start",
            "turn/start",
            "response"
        ]
    );
    let start = &received[3].1["params"];
    assert_eq!(start["approvalPolicy"], "untrusted");
    assert_eq!(start["sandbox"], "read-only");
    assert_eq!(start["approvalsReviewer"], "user");
    assert_eq!(start["model"], "gpt-6.1-sol");
    adapter.shutdown().await;
}

#[tokio::test]
async fn only_offered_options_are_sent_to_the_provider() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("approval-touch").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, _) = requested(&first);

    // `decline` exists in Codex's schema but this request did not offer it.
    for option in ["decline", "acceptForSession", "nonsense", ""] {
        let error = adapter.respond(approval, option.into()).await.unwrap_err();
        assert!(
            matches!(&error, AgentError::OptionNotOffered { option: o } if o == option),
            "{option}: {error}"
        );
    }
    let error = adapter
        .respond(ApprovalId::new(), "accept".into())
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::UnknownApproval), "{error}");
    assert!(
        h.received().iter().all(|(name, _)| name != "response"),
        "nothing reached the provider"
    );

    adapter.respond(approval, "accept".into()).await.unwrap();
    collect_until(&mut events, ends_turn).await;
    adapter.shutdown().await;
}

#[tokio::test]
async fn an_answered_request_cannot_be_answered_twice() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("approval-touch").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, _) = requested(&first);
    adapter.respond(approval, "accept".into()).await.unwrap();
    let error = adapter
        .respond(approval, "accept".into())
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::UnknownApproval), "{error}");
    collect_until(&mut events, ends_turn).await;
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_declined_command_ends_as_a_declined_tool() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("approval-decline").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, _) = requested(&first);
    // The recording answered `decline`; the server offered only accept/cancel, so Hutan
    // answers `cancel` and the replay (which matches responses loosely) carries on.
    adapter.respond(approval, "cancel".into()).await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert!(rest.iter().any(|m| matches!(
        m,
        AdapterMessage::Event(d) if matches!(
            d.kind,
            AgentEventKind::ToolEnd { status: ToolStatus::Declined, .. }
        )
    )));
    let response = h
        .received()
        .into_iter()
        .find(|(name, _)| name == "response")
        .unwrap();
    assert_eq!(response.1["result"]["decision"], "cancel");
    adapter.shutdown().await;
}

#[tokio::test]
async fn file_changes_carry_per_file_diffs_and_default_options() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("file-change").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, draft) = requested(&first);
    let AgentEventKind::PermissionRequested { kind, options, .. } = &draft.kind else {
        unreachable!()
    };
    assert_eq!(*kind, PermissionKind::FileChange);
    // The request listed no `availableDecisions`: the schema's decisions are the offer.
    let ids: Vec<_> = options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, ["accept", "acceptForSession", "decline", "cancel"]);
    let diff = kinds(&first)
        .into_iter()
        .find_map(|k| match k {
            AgentEventKind::FileDiff { changes, .. } => changes.first(),
            _ => None,
        })
        .expect("a file diff before the approval");
    assert_eq!(diff.kind, FileChangeKind::Add);
    assert!(diff.path.ends_with("hello.txt"), "{}", diff.path);
    assert_eq!(diff.patch.as_deref(), Some("hi\n"));

    adapter.respond(approval, "accept".into()).await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    let after = names(&rest);
    assert!(
        after.contains(&"file_diff") && after.contains(&"tool_end"),
        "{after:?}"
    );
    // The aggregate turn diff arrives as a patch.
    assert!(
        kinds(&rest).into_iter().any(|k| matches!(
            k,
            AgentEventKind::FileDiff { patch: Some(p), changes } if p.contains("+hi") && changes.is_empty()
        )),
        "{after:?}"
    );
    adapter.shutdown().await;
}

#[tokio::test]
async fn interrupting_a_streaming_turn_cancels_it() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("interrupt").await;
    adapter
        .send_message("Write the numbers from 1 to 300".into())
        .await
        .unwrap();
    let mut deltas = 0;
    let streamed = collect_until(&mut events, |m| {
        deltas += usize::from(matches!(
            m,
            AdapterMessage::Event(d) if matches!(d.kind, AgentEventKind::MessageDelta { .. })
        ));
        deltas == 4
    })
    .await;
    assert!(names(&streamed).contains(&"message_start"));
    adapter.interrupt().await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert_eq!(names(&rest).last(), Some(&"turn_cancelled"));
    let methods: Vec<_> = h.received().into_iter().map(|(m, _)| m).collect();
    assert_eq!(methods.last().map(String::as_str), Some("turn/interrupt"));
    let interrupt = h.received().pop().unwrap().1;
    assert!(interrupt["params"]["turnId"].as_str().is_some());
    adapter.shutdown().await;
}

#[tokio::test]
async fn interrupting_without_a_turn_is_an_error() {
    let h = Harness::new();
    let session = h.start("interrupt").await;
    let error = session.adapter.interrupt().await.unwrap_err();
    assert!(matches!(error, AgentError::NoTurn), "{error}");
    session.adapter.shutdown().await;
}

#[tokio::test]
async fn a_pending_approval_expires_when_the_turn_is_interrupted() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("approval-interrupt").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_permission_request).await;
    let (approval, _) = requested(&first);
    adapter.interrupt().await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    let kinds = kinds(&rest);
    // The resolution comes before the turn's end.
    assert_eq!(
        kinds[kinds.len() - 2],
        &AgentEventKind::PermissionResolved {
            approval_id: approval,
            outcome: PermissionOutcome::Expired,
            decided_by: None,
        }
    );
    assert_eq!(kinds[kinds.len() - 1], &AgentEventKind::TurnCancelled);
    // Too late to answer.
    let error = adapter
        .respond(approval, "accept".into())
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::UnknownApproval), "{error}");
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_failed_turn_reports_the_providers_message() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("turn-failed").await;
    adapter.send_message("hi".into()).await.unwrap();
    let all = collect_until(&mut events, ends_turn).await;
    let failed = kinds(&all).pop().unwrap();
    let AgentEventKind::TurnFailed { message, .. } = failed else {
        panic!("{failed:?}")
    };
    assert!(
        message.contains("is not supported when using Codex"),
        "{message}"
    );
    // The provider's own error notification is kept as a notice.
    assert!(names(&all).contains(&"notice"), "{:?}", names(&all));
    adapter.shutdown().await;
}

#[tokio::test]
async fn resuming_reopens_the_stored_thread_and_continues() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        started,
        mut events,
    } = h
        .start_with(&fixture("resume-turn"), Some("thread-from-the-journal"))
        .await;
    assert!(started.resumed);
    assert_eq!(started.thread_id, "thread-from-the-journal");

    let turn = adapter.send_message("again".into()).await.unwrap();
    let all = collect_until(&mut events, ends_turn).await;
    // Events of the resumed thread (whose recorded id differs) are not dropped as foreign.
    let completed = kinds(&all).pop().unwrap();
    assert!(
        matches!(completed, AgentEventKind::TurnCompleted { .. }),
        "{completed:?}"
    );
    let locator = kinds(&all)[0];
    assert_eq!(
        *locator,
        AgentEventKind::ResumeLocator {
            agent: "codex".into(),
            thread_id: "thread-from-the-journal".into(),
            model: Some("gpt-6.1-sol".into())
        }
    );
    assert!(turn.provider_turn_id.len() > 8);

    let received = h.received();
    let methods: Vec<_> = received.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        methods,
        ["initialize", "initialized", "thread/resume", "turn/start"]
    );
    let resume = &received[2].1["params"];
    assert_eq!(resume["threadId"], "thread-from-the-journal");
    assert_eq!(
        resume["excludeTurns"], true,
        "history comes from our journal"
    );
    assert_eq!(
        received[3].1["params"]["threadId"],
        "thread-from-the-journal"
    );
    adapter.shutdown().await;
}

fn write_transcript(h: &Harness, name: &str, lines: &[Value]) -> PathBuf {
    let path = h.dir.path().join(name);
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&path, text).unwrap();
    path
}

fn handshake_lines(user_agent: &str) -> Vec<Value> {
    use serde_json::json;
    vec![
        json!({"dir":"out","msg":{"method":"initialize","id":1,"params":{}}}),
        json!({"dir":"in","msg":{"id":1,"result":{"userAgent":user_agent}}}),
        json!({"dir":"out","msg":{"method":"initialized"}}),
    ]
}

#[tokio::test]
async fn unsupported_versions_are_refused_with_a_clear_error() {
    for (agent, found) in [
        ("hutan/0.144.2 (Mac OS; arm64)", "0.144.2"),
        ("hutan/0.170.0 (Mac OS; arm64)", "0.170.0"),
    ] {
        let h = Harness::new();
        let transcript = write_transcript(&h, "old.jsonl", &handshake_lines(agent));
        let error = timeout(
            WAIT,
            CodexAppServerAdapter::start(&h.config(&transcript), &h.spec(None)),
        )
        .await
        .unwrap()
        .unwrap_err();
        let text = error.to_string();
        assert!(
            matches!(error, AgentError::UnsupportedVersion { .. }),
            "{text}"
        );
        assert!(
            text.contains(found) && text.contains(">=0.160, <0.170"),
            "{text}"
        );
        let methods: Vec<_> = h.received().into_iter().map(|(m, _)| m).collect();
        assert_eq!(methods, ["initialize"], "nothing past the version check");
    }
}

#[tokio::test]
async fn a_missing_program_is_a_spawn_error() {
    let h = Harness::new();
    let config = CodexConfig {
        program: PathBuf::from("/nonexistent/hutan-codex"),
        ..CodexConfig::default()
    };
    let error = CodexAppServerAdapter::start(&config, &h.spec(None))
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::Spawn { .. }), "{error}");
    assert!(error.to_string().contains("/nonexistent/hutan-codex"));
}

#[tokio::test]
async fn unknown_messages_become_opaque_events_and_never_crash_the_adapter() {
    use serde_json::json;
    let h = Harness::new();
    // The real handshake and thread start, then things this build has never heard of.
    let real = std::fs::read_to_string(fixture("approval-touch")).unwrap();
    let mut lines: Vec<Value> = Vec::new();
    for line in real.lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        let is_turn_start = value["msg"]["method"] == "turn/start";
        if is_turn_start {
            break;
        }
        lines.push(value);
    }
    let thread = lines
        .iter()
        .find_map(|l| {
            l["msg"]["result"]["thread"]["id"]
                .as_str()
                .map(str::to_owned)
        })
        .unwrap();
    lines.extend([
        json!({"dir":"out","msg":{"method":"turn/start","id":4,"params":{}}}),
        json!({"dir":"in","msg":{"id":4,"result":{"turn":{"id":"turn-1"}}}}),
        json!({"dir":"in","msg":{"method":"future/thing","params":{"threadId":thread,"x":1}}}),
        json!({"dir":"in","msg":{"method":"turn/started","params":{"threadId":thread,"turn":"not an object"}}}),
        json!({"dir":"in","msg":{"method":"item/started","params":{"threadId":thread,"turnId":"turn-1","startedAtMs":1,"item":{"type":"futureItem","id":"i1","novel":true}}}}),
        json!({"dir":"in","msg":{"method":"item/started","params":{"threadId":"some-other-thread","turnId":"t9","item":{"type":"agentMessage","id":"x","text":""}}}}),
        json!({"dir":"in","msg":{"method":"item/tool/call","id":77,"params":{"threadId":thread,"turnId":"turn-1","callId":"q","tool":"t","arguments":{}}}}),
        json!({"dir":"in","msg":{"method":"account/updated","params":{}}}),
        json!({"dir":"out","msg":{"id":77,"error":{"code":-32601,"message":"declined"}}}),
        json!({"dir":"exit","code":9}),
    ]);
    let transcript = write_transcript(&h, "odd.jsonl", &lines);
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start_with(&transcript, None).await;
    adapter.send_message("go".into()).await.unwrap();
    let all = collect_until(&mut events, |m| matches!(m, AdapterMessage::Exited(_))).await;

    let opaque: Vec<_> = all
        .iter()
        .filter_map(|m| match m {
            AdapterMessage::Event(d) => match &d.kind {
                AgentEventKind::Opaque { method } => Some((method.as_str(), d)),
                _ => None,
            },
            AdapterMessage::Exited(_)
            | AdapterMessage::Node(_)
            | AdapterMessage::Mode(_)
            | AdapterMessage::Limits(_) => None,
        })
        .collect();
    let methods: Vec<_> = opaque.iter().map(|(m, _)| *m).collect();
    assert_eq!(methods, ["future/thing", "turn/started", "item/started"]);
    assert_eq!(opaque[0].1.payload.as_ref().unwrap()["x"], 1);
    assert_eq!(opaque[2].1.payload.as_ref().unwrap()["novel"], true);
    assert_eq!(opaque[2].1.provider.item_id.as_deref(), Some("i1"));
    // A request Hutan cannot serve is declined (the agent must not wait for it) and noted.
    assert!(names(&all).contains(&"notice"));
    let refusal = h
        .received()
        .into_iter()
        .find(|(name, msg)| name == "response" && msg["id"] == 77)
        .expect("the unsupported request was answered");
    assert_eq!(refusal.1["error"]["code"], -32601);
    // Other threads' events and account chatter are dropped, not turned into noise.
    assert_eq!(opaque.len(), 3);
    // The process ended: the adapter says so, with the exit code.
    let Some(AdapterMessage::Exited(info)) = all.last() else {
        panic!("{all:?}")
    };
    assert_eq!(info.code, Some(9));
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_process_that_dies_expires_what_was_pending() {
    use serde_json::json;
    let h = Harness::new();
    let real = std::fs::read_to_string(fixture("approval-touch")).unwrap();
    let mut lines: Vec<Value> = Vec::new();
    for line in real.lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        lines.push(value.clone());
        if value["msg"]["method"] == "item/commandExecution/requestApproval" {
            break;
        }
    }
    lines.push(json!({"dir":"exit","code":1}));
    let transcript = write_transcript(&h, "dies.jsonl", &lines);
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start_with(&transcript, None).await;
    adapter.send_message("go".into()).await.unwrap();
    let all = collect_until(&mut events, |m| matches!(m, AdapterMessage::Exited(_))).await;
    let names = names(&all);
    assert_eq!(names.last(), Some(&"permission_resolved"), "{names:?}");
    let Some(AdapterMessage::Exited(info)) = all.last() else {
        panic!("{all:?}")
    };
    assert_eq!(info.code, Some(1));
    let error = adapter
        .send_message("more".into())
        .await
        .expect_err("the process is gone");
    assert!(matches!(error, AgentError::Exited { .. }), "{error}");
}

// Questions -----------------------------------------------------------------------------------
//
// The `question-*` transcripts are built by hand from the schema `codex app-server
// generate-json-schema` writes for 0.162.1 (a real recording of `requestUserInput` or an MCP
// elicitation needs a paid turn), on top of the recorded handshake and turn of
// `approval-touch`, and scrubbed like the recordings.

fn is_question_request(message: &AdapterMessage) -> bool {
    matches!(message, AdapterMessage::Event(d) if matches!(d.kind, AgentEventKind::QuestionRequested { .. }))
}

fn asked(messages: &[AdapterMessage]) -> (QuestionId, &AgentEventDraft) {
    messages
        .iter()
        .find_map(|m| match m {
            AdapterMessage::Event(draft) => match &draft.kind {
                AgentEventKind::QuestionRequested { question_id, .. } => {
                    Some((*question_id, draft.as_ref()))
                }
                _ => None,
            },
            AdapterMessage::Exited(_)
            | AdapterMessage::Node(_)
            | AdapterMessage::Mode(_)
            | AdapterMessage::Limits(_) => None,
        })
        .expect("a question")
}

fn values(pairs: &[(&str, Value)]) -> Values {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn responses(h: &Harness) -> Vec<Value> {
    h.received()
        .into_iter()
        .filter(|(name, _)| name == "response")
        .map(|(_, msg)| msg)
        .collect()
}

#[tokio::test]
async fn a_user_input_request_is_answered_in_codexs_shape_and_the_secret_stays_out_of_the_events() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("question-user-input").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_question_request).await;
    let (question, draft) = asked(&first);
    let AgentEventKind::QuestionRequested { spec, .. } = &draft.kind else {
        panic!()
    };
    assert_eq!(spec.title, "Codex has 3 questions");
    assert_eq!(spec.expires, QuestionExpiry::Turn);
    assert_eq!(
        spec.fields
            .iter()
            .map(|f| f.id.as_str())
            .collect::<Vec<_>>(),
        ["scope", "note", "token"]
    );
    assert!(matches!(
        &spec.fields[0].kind,
        FieldKind::Choice { options, allow_other: true, .. } if options.len() == 2
    ));
    assert!(matches!(spec.fields[2].kind, FieldKind::Secret));
    assert_eq!(draft.provider.request_id.as_deref(), Some("0"));
    assert_eq!(draft.provider.item_id.as_deref(), Some("call_fixture_1"));

    adapter
        .answer_question(
            question,
            QuestionResponse::Submit {
                values: values(&[
                    ("scope", Value::from("Only the CLI")),
                    ("note", Value::from("none")),
                    ("token", Value::from("s3cr3t-token")),
                ]),
            },
        )
        .await
        .unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    // The replay answers `serverRequest/resolved` only after it read Codex's response, which is
    // therefore logged by now: it is the shape the schema gives.
    assert_eq!(
        responses(&h)[0]["result"],
        serde_json::json!({"answers": {
            "scope": {"answers": ["Only the CLI"]},
            "note": {"answers": ["none"]},
            "token": {"answers": ["s3cr3t-token"]}
        }})
    );
    let resolved = kinds(&rest)
        .into_iter()
        .find(|k| matches!(k, AgentEventKind::QuestionResolved { .. }))
        .expect("the question is resolved");
    assert_eq!(
        resolved,
        &AgentEventKind::QuestionResolved {
            question_id: question,
            outcome: QuestionOutcome::Answered {
                values: values(&[
                    ("scope", Value::from("Only the CLI")),
                    ("note", Value::from("none")),
                ]),
                withheld: vec!["token".to_owned()],
            },
            decided_by: None,
        }
    );
    for message in rest.iter().chain(first.iter()) {
        if let AdapterMessage::Event(draft) = message {
            let wire = serde_json::to_string(draft).unwrap();
            // The request payload holds the questions; nothing holds the answer.
            assert!(!wire.contains("s3cr3t-token"), "{wire}");
        }
    }
    // Answered once.
    let again = adapter
        .answer_question(question, QuestionResponse::Decline)
        .await
        .unwrap_err();
    assert!(matches!(again, AgentError::UnknownQuestion), "{again}");
    let unknown = adapter
        .answer_question(QuestionId::new(), QuestionResponse::Decline)
        .await
        .unwrap_err();
    assert!(matches!(unknown, AgentError::UnknownQuestion), "{unknown}");
    adapter.shutdown().await;
}

#[tokio::test]
async fn an_mcp_elicitation_is_answered_with_accept_and_its_content() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("question-elicitation").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_question_request).await;
    let (question, draft) = asked(&first);
    let AgentEventKind::QuestionRequested { spec, .. } = &draft.kind else {
        panic!()
    };
    assert_eq!(spec.title, "deployer asks");
    assert_eq!(spec.source.as_deref(), Some("MCP server deployer"));
    assert_eq!(
        spec.prompt,
        "The deployer needs a few details before it ships the build."
    );
    let by = |id: &str| spec.fields.iter().find(|f| f.id == id).unwrap();
    assert!(by("environment").required);
    assert_eq!(by("environment").default, Some(Value::from("staging")));
    assert!(matches!(
        &by("environment").kind,
        FieldKind::Choice { options, multi: false, .. } if options[1].label == "Production"
    ));
    assert!(matches!(
        by("replicas").kind,
        FieldKind::Number {
            integer: true,
            min: Some(1.0),
            max: Some(9.0)
        }
    ));
    assert_eq!(by("notify").default, Some(Value::Bool(true)));

    adapter
        .answer_question(
            question,
            QuestionResponse::Submit {
                values: values(&[
                    ("environment", Value::from("production")),
                    ("replicas", Value::from(3)),
                    ("notify", Value::Bool(false)),
                ]),
            },
        )
        .await
        .unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert_eq!(
        responses(&h)[0]["result"],
        serde_json::json!({"action": "accept", "content": {
            "environment": "production", "replicas": 3, "notify": false
        }})
    );
    assert!(names(&rest).contains(&"question_resolved"));
    adapter.shutdown().await;
}

#[tokio::test]
async fn declining_and_cancelling_use_the_providers_own_words() {
    // MCP elicitation: decline.
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("question-elicitation").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_question_request).await;
    let (question, _) = asked(&first);
    adapter
        .answer_question(question, QuestionResponse::Decline)
        .await
        .unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert_eq!(
        responses(&h)[0]["result"],
        serde_json::json!({"action": "decline", "content": null})
    );
    assert!(kinds(&rest).iter().any(|k| matches!(
        k,
        AgentEventKind::QuestionResolved {
            outcome: QuestionOutcome::Declined,
            ..
        }
    )));
    adapter.shutdown().await;

    // requestUserInput: no cancel in the schema, so an empty answer and the turn is stopped.
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("question-cancel").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_question_request).await;
    let (question, _) = asked(&first);
    adapter
        .answer_question(question, QuestionResponse::Cancel)
        .await
        .unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    assert_eq!(
        responses(&h)[0]["result"],
        serde_json::json!({"answers": {}})
    );
    let kinds = kinds(&rest);
    assert!(kinds.iter().any(|k| matches!(
        k,
        AgentEventKind::QuestionResolved {
            outcome: QuestionOutcome::Cancelled,
            ..
        }
    )));
    assert_eq!(kinds[kinds.len() - 1], &AgentEventKind::TurnCancelled);
    assert!(
        h.received()
            .iter()
            .any(|(name, _)| name == "turn/interrupt"),
        "cancelling the question stopped the turn"
    );
    adapter.shutdown().await;
}

#[tokio::test]
async fn a_pending_question_expires_when_the_turn_is_interrupted() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("question-interrupt").await;
    adapter.send_message("go".into()).await.unwrap();
    let first = collect_until(&mut events, is_question_request).await;
    let (question, _) = asked(&first);
    adapter.interrupt().await.unwrap();
    let rest = collect_until(&mut events, ends_turn).await;
    let kinds = kinds(&rest);
    assert_eq!(
        kinds[kinds.len() - 2],
        &AgentEventKind::QuestionResolved {
            question_id: question,
            outcome: QuestionOutcome::Expired,
            decided_by: None,
        }
    );
    assert_eq!(kinds[kinds.len() - 1], &AgentEventKind::TurnCancelled);
    let error = adapter
        .answer_question(question, QuestionResponse::Decline)
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::UnknownQuestion), "{error}");
    adapter.shutdown().await;
}

#[tokio::test]
async fn an_elicitation_outside_a_turn_expires_with_the_process() {
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("question-elicitation-process").await;
    adapter.send_message("go".into()).await.unwrap();
    collect_until(&mut events, ends_turn).await;
    let first = collect_until(&mut events, is_question_request).await;
    let (question, draft) = asked(&first);
    assert!(draft.turn_id.is_none());
    let AgentEventKind::QuestionRequested { spec, .. } = &draft.kind else {
        panic!()
    };
    assert_eq!(spec.expires, QuestionExpiry::Process);
    assert!(matches!(spec.fields[0].kind, FieldKind::Secret));
    adapter.shutdown().await;
    let rest = collect_until(&mut events, |m| matches!(m, AdapterMessage::Exited(_))).await;
    assert!(kinds(&rest).iter().any(|k| matches!(
        k,
        AgentEventKind::QuestionResolved {
            question_id,
            outcome: QuestionOutcome::Expired,
            ..
        } if *question_id == question
    )));
}

#[test]
fn committed_fixtures_are_already_scrubbed() {
    use hutan_agents::fixture::scrub::scrub;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex-0.162");
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let scrubbed = scrub(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            scrubbed,
            text,
            "{} changes when scrubbed again",
            path.display()
        );
        count += 1;
    }
    assert!(count >= 11, "{count} fixtures");
}

#[tokio::test]
async fn developer_instructions_ride_on_thread_start_and_resume_and_base_instructions_stay_absent()
{
    let h = Harness::new();
    let started = timeout(
        WAIT,
        CodexAppServerAdapter::start(
            &h.config(&fixture("plain-turn")),
            &h.spec_with(None, Some("Hutan memory: the user prefers short answers.")),
        ),
    )
    .await
    .expect("start timed out")
    .expect("start failed");
    started.adapter.shutdown().await;
    let received = h.received();
    let start = &received
        .iter()
        .find(|(m, _)| m == "thread/start")
        .unwrap()
        .1["params"];
    assert_eq!(
        start["developerInstructions"],
        "Hutan memory: the user prefers short answers."
    );
    assert!(
        start.get("baseInstructions").is_none(),
        "Codex keeps its own base instructions: {start}"
    );

    let h = Harness::new();
    let resumed = timeout(
        WAIT,
        CodexAppServerAdapter::start(
            &h.config(&fixture("resume-turn")),
            &h.spec_with(Some("thread-from-the-journal"), Some("remembered")),
        ),
    )
    .await
    .expect("start timed out")
    .expect("start failed");
    resumed.adapter.shutdown().await;
    let received = h.received();
    let resume = &received
        .iter()
        .find(|(m, _)| m == "thread/resume")
        .unwrap()
        .1["params"];
    assert_eq!(resume["developerInstructions"], "remembered");
    assert!(resume.get("baseInstructions").is_none());

    // Without instructions the field is not sent at all.
    let h = Harness::new();
    h.start("plain-turn").await.adapter.shutdown().await;
    let received = h.received();
    let start = &received
        .iter()
        .find(|(m, _)| m == "thread/start")
        .unwrap()
        .1["params"];
    assert!(start.get("developerInstructions").is_none(), "{start}");
}

fn limits_of(messages: &[AdapterMessage]) -> Vec<&hutan_agents::RateLimits> {
    messages
        .iter()
        .filter_map(|m| match m {
            AdapterMessage::Limits(limits) => Some(&**limits),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn rate_limit_notifications_become_limit_windows_and_never_journal_events() {
    // `account/rateLimits/updated` (built by hand from the documented schema, between the
    // messages of a recorded turn): a full report, then a sparse one with only the 5-hour
    // window.
    let h = Harness::new();
    let CodexSession {
        adapter,
        mut events,
        ..
    } = h.start("rate-limits").await;
    adapter
        .send_message("Reply with the single word pong and nothing else.".into())
        .await
        .unwrap();
    let seen = collect_until(&mut events, ends_turn).await;
    let limits = limits_of(&seen);
    assert_eq!(limits.len(), 2, "{limits:?}");
    let full = limits[0];
    assert_eq!(full.provider, "openai");
    // The scrubber nulls plan names out of recordings.
    assert_eq!(full.plan, None);
    let windows: Vec<_> = full
        .windows
        .iter()
        .map(|w| (w.id.as_str(), w.used_percent, w.resets_at_ms))
        .collect();
    assert_eq!(
        windows,
        [
            ("5h", 72.0, Some(4_102_444_800_000)),
            ("7d", 31.0, Some(4_102_531_200_000))
        ]
    );
    assert_eq!(limits[1].windows.len(), 1, "a sparse report carries one window");
    assert_eq!(limits[1].windows[0].used_percent, 81.0);
    // The conversation is what it was: no opaque event for the chatter.
    let names = names(&seen);
    assert!(!names.contains(&"opaque"), "{names:?}");
    assert_eq!(names.last(), Some(&"turn_completed"));
}

#[tokio::test]
async fn rate_limits_can_be_read_from_a_short_lived_app_server() {
    let h = Harness::new();
    let limits = timeout(
        WAIT,
        hutan_agents::codex::read_rate_limits(&h.config(&fixture("rate-limits-read"))),
    )
    .await
    .expect("read timed out")
    .expect("read failed")
    .expect("windows");
    let ids: Vec<_> = limits.windows.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(ids, ["5h", "7d"]);
    let methods: Vec<_> = h.received().into_iter().map(|(m, _)| m).collect();
    assert_eq!(
        methods,
        ["initialize", "initialized", "account/rateLimits/read"]
    );
}
