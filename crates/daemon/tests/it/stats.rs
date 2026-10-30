//! The log of agents' states and `agents.stats` (protocol 0.67) end to end: a real daemon on a
//! temporary `HUTAN_HOME`, terminals that speak OSC 7501 (`cat` echoes the bytes), a hook that
//! reports, and a Codex chat that asks and finishes (`hutan-codex-fixture replay`). Nothing
//! here starts a real harness or calls a model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use common::{TestDaemon, WAIT, create_in, remote_code, sh, terminal_of, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::{ApprovalPolicy, SandboxMode, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ApprovalInfo, ApprovalRespond, ChatCreate, ChatSend};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Empty, Event, Request, SessionInfo,
    SessionResult, WorkspaceCreate, WorkspaceInfo,
};
use hutan_protocol::stats::{AgentsStats, AgentsStatsRequest, FailureSource, StatsBy, WaitSource};
use serde_json::json;
use tokio::time::{sleep, timeout};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/codex-0.162")
        .join(format!("{name}.jsonl"))
}

fn codex_replaying(transcript: &Path, received: &Path) -> CodexConfig {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-codex-fixture");
    assert!(bin.is_file(), "hutan-codex-fixture not built");
    CodexConfig {
        program: bin,
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

async fn workspace(client: &Client, name: &str, root: &Path) -> WorkspaceInfo {
    client
        .call(Request::WorkspaceCreate(WorkspaceCreate {
            name: name.into(),
            paths: vec![root.to_path_buf()],
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap()
}

async fn status(client: &Client, session: &SessionInfo, fields: &str) {
    client
        .send_input(
            terminal_of(session),
            format!("\x1b]7501;{fields}\x1b\\\r").as_bytes(),
        )
        .await
        .unwrap();
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

async fn stats(client: &Client, req: AgentsStatsRequest) -> AgentsStats {
    client.call(Request::AgentsStats(req)).await.unwrap()
}

async fn stats_where(
    client: &Client,
    req: AgentsStatsRequest,
    what: &str,
    pred: impl Fn(&AgentsStats) -> bool,
) -> AgentsStats {
    timeout(WAIT, async {
        loop {
            let answer = stats(client, req).await;
            if pred(&answer) {
                return answer;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the statistics never showed {what}"))
}

fn since(ms: u64) -> AgentsStatsRequest {
    AgentsStatsRequest {
        since_ms: Some(ms),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_state_log_tells_how_long_terminal_agents_waited_and_what_failed() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let before = hutan_client::time::now_ms().saturating_sub(1_000);
    let asks = create_in(&client, None, sh("exec cat")).await;
    status(&client, &asks, "state=working:app=omp").await;
    status(
        &client,
        &asks,
        &format!(
            "state=blocked:kind=permission:app=omp:msg={}",
            b64("Run the suite with sk-abcdefghijklmnopqrstuvwxyz0123456789?")
        ),
    )
    .await;
    // The wait is a span: the agent has to say something else for it to end.
    sleep(Duration::from_millis(60)).await;
    status(&client, &asks, "state=working:app=omp").await;
    // Another one that fails twice the same way (the numbers differ).
    let fails = create_in(&client, None, sh("exec cat")).await;
    for n in [40, 12] {
        status(&client, &fails, "state=working:app=omp").await;
        status(
            &client,
            &fails,
            &format!(
                "state=error:app=omp:msg={}",
                b64(&format!("Rate limited: retry in {n} s"))
            ),
        )
        .await;
    }
    // A hook's report is logged too; a second `working` is not.
    let hooked = create_in(&client, None, sh("exec sleep 600")).await;
    for (seq, state) in [
        (1, AgentState::Working),
        (2, AgentState::Blocked),
        (3, AgentState::Blocked),
    ] {
        client
            .call::<AgentApplied>(Request::AgentReport(AgentReportRequest {
                pid: None,
                session_id: hooked.id,
                source: "fake-hook".into(),
                agent: "omp".into(),
                state: Some(state),
                message: None,
                seq,
                agent_session_id: None,
                resume_argv: None,
                node: None,
            }))
            .await
            .unwrap();
    }

    let answer = stats_where(&client, since(before), "the waits and the errors", |s| {
        s.total.waits.as_ref().is_some_and(|w| {
            w.by_source
                .iter()
                .any(|b| b.source == WaitSource::StateLog && b.count >= 2)
        }) && s
            .failures
            .iter()
            .any(|f| f.source == FailureSource::TerminalError)
    })
    .await;
    assert!(answer.coverage.state_log);
    assert!(
        answer
            .coverage
            .state_log_from_ms
            .is_some_and(|from| from >= before)
    );
    assert_eq!(answer.coverage.terminal_agents, 3);
    assert_eq!(answer.coverage.chats, 0);
    let waits = answer.total.waits.as_ref().unwrap();
    let log = waits
        .by_source
        .iter()
        .find(|s| s.source == WaitSource::StateLog)
        .unwrap();
    // The first agent's wait ended; the hook's still goes on at the window's end.
    assert_eq!((log.count, log.open), (2, 1), "{log:?}");
    assert!(log.longest_ms >= 50, "{log:?}");
    assert!(waits.by_kind.iter().any(|k| k.kind == "permission"));
    assert!(waits.by_kind.iter().any(|k| k.kind == "terminal_prompt"));
    // Failures that read alike are one cluster, counted exactly.
    let rate = answer
        .failures
        .iter()
        .find(|f| f.pattern == "rate limited: retry in # s")
        .expect("the cluster");
    assert_eq!((rate.count, rate.sessions), (2, 1));
    assert_eq!(rate.source, FailureSource::TerminalError);
    // No turns, no tokens: terminal agents have none, and the answer says nothing it does not know.
    assert!(answer.total.turns.is_none() && answer.total.tokens.is_none());
    assert!(
        answer
            .not_recorded
            .iter()
            .any(|w| w.contains("terminal agents"))
    );
    // The secret never reached the log.
    let json = serde_json::to_string(&answer).unwrap();
    assert!(!json.contains("sk-abcdefghij"));

    // Turning the log off deletes it and stops the writing; what is recorded says so.
    client
        .daemon_setting_set("agents.state_log", Some(json!(false)))
        .await
        .unwrap();
    let off = stats(&client, since(before)).await;
    assert!(!off.coverage.state_log);
    assert_eq!(off.coverage.state_log_from_ms, None, "the log was emptied");
    assert!(off.total.waits.is_none() && off.failures.is_empty());
    status(&client, &asks, "state=blocked:kind=question:app=omp").await;
    sleep(Duration::from_millis(100)).await;
    let still = stats(&client, since(before)).await;
    assert_eq!(
        still.coverage.state_log_from_ms, None,
        "nothing is written while it is off"
    );
    // And on again: writing resumes.
    client
        .daemon_setting_set("agents.state_log", Some(json!(true)))
        .await
        .unwrap();
    status(&client, &asks, "state=working:app=omp").await;
    stats_where(&client, since(before), "the log written again", |s| {
        s.coverage.state_log && s.coverage.state_log_from_ms.is_some()
    })
    .await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chats_have_turns_waits_and_a_project_and_unknown_ids_are_not_found() {
    let work = tempfile::tempdir().unwrap();
    let alpha_root = work.path().join("alpha");
    let beta_root = work.path().join("beta");
    std::fs::create_dir_all(&alpha_root).unwrap();
    std::fs::create_dir_all(&beta_root).unwrap();
    let config = codex_replaying(
        &fixture("approval-touch"),
        &work.path().join("received.jsonl"),
    );
    let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let alpha = workspace(&client, "alpha", &alpha_root).await;
    let beta = workspace(&client, "beta", &beta_root).await;
    let before = hutan_client::time::now_ms().saturating_sub(1_000);

    let chat = client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: Some(alpha.id),
            cwd: alpha_root.clone(),
            agent: "codex".to_owned(),
            model: None,
            approval_policy: Some(ApprovalPolicy::Untrusted),
            sandbox: Some(SandboxMode::ReadOnly),
            title: Some("Touch a file".into()),
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        }))
        .await
        .unwrap()
        .session;
    create_in(&client, Some(beta.id), sh("exec sleep 600")).await;
    client
        .call::<hutan_protocol::chat::ChatSent>(Request::ChatSend(ChatSend {
            session_id: chat.id,
            text: "Run `touch hi.txt` in a shell, then reply with the word done.".into(),
            attachments: vec![],
        }))
        .await
        .unwrap();
    let approval: ApprovalInfo =
        wait_event(&mut events, "approval.required", |event| match event {
            Event::ApprovalRequired { approval } => Some((**approval).clone()),
            _ => None,
        })
        .await;
    // Open: the wait is counted up to the end of the window and says it has not ended.
    let open = stats_where(&client, since(before), "the open wait", |s| {
        s.total.waits.as_ref().is_some_and(|w| {
            w.by_source
                .iter()
                .any(|b| b.source == WaitSource::Audit && b.open == 1)
        })
    })
    .await;
    assert_eq!(open.coverage.chats, 1);
    client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval.id,
            option_id: "accept".to_owned(),
        }))
        .await
        .unwrap();
    let done = stats_where(&client, since(before), "the finished turn", |s| {
        s.total.turns.is_some_and(|t| t.count == 1)
            && s.total.waits.as_ref().is_some_and(|w| {
                w.by_source
                    .iter()
                    .any(|b| b.source == WaitSource::Audit && b.open == 0 && b.count == 1)
            })
    })
    .await;
    // By project: the chat is in alpha, and beta has a terminal that did nothing recorded.
    let alpha_group = done
        .groups
        .iter()
        .find(|g| g.label == "alpha")
        .expect("alpha");
    assert_eq!(alpha_group.coverage.chats, 1);
    assert_eq!(alpha_group.turns.unwrap().count, 1);
    assert_eq!(alpha_group.turns.unwrap().active_ms_total, None);
    assert!(
        alpha_group
            .waits
            .as_ref()
            .unwrap()
            .by_kind
            .iter()
            .any(|k| !k.kind.is_empty())
    );
    assert!(
        done.groups.iter().all(|g| g.label != "beta"),
        "nothing recorded: no group"
    );
    // By harness and by session the same numbers are grouped otherwise.
    let by_session = stats(
        &client,
        AgentsStatsRequest {
            group_by: StatsBy::Session,
            ..since(before)
        },
    )
    .await;
    assert_eq!(by_session.groups.len(), 1);
    assert_eq!(by_session.groups[0].key, chat.id.to_string());
    // A workspace narrows it; an unknown one is not found.
    let only_beta = stats(
        &client,
        AgentsStatsRequest {
            workspace_id: Some(beta.id),
            ..since(before)
        },
    )
    .await;
    assert!(only_beta.total.turns.is_none() && only_beta.groups.is_empty());
    let missing = client
        .call::<AgentsStats>(Request::AgentsStats(AgentsStatsRequest {
            workspace_id: Some(WorkspaceId::new()),
            ..Default::default()
        }))
        .await;
    assert_eq!(remote_code(missing), ErrorCode::NotFound);
    daemon.stop().await;
}
