//! `agents.activity` (protocol 0.65) end to end: a real daemon on a temporary `HUTAN_HOME`, a
//! Codex chat that asks for permission and finishes its turn (`hutan-codex-fixture replay`), and
//! a terminal in another workspace. Nothing here starts a real harness or calls a model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, create_in, remote_code, sh, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::{ApprovalPolicy, SandboxMode, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::activity::{
    ActivityDetail, AgentsActivity, AgentsActivityRequest, RequestKind, TurnOutcome,
};
use hutan_protocol::chat::{ApprovalInfo, ApprovalRespond, ChatCreate, ChatSend};
use hutan_protocol::messages::{
    Empty, Event, Request, SessionResult, WorkspaceCreate, WorkspaceInfo,
};
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

async fn activity(client: &Client, req: AgentsActivityRequest) -> AgentsActivity {
    client.call(Request::AgentsActivity(req)).await.unwrap()
}

/// Polls the catch-up until `pred` holds for it.
async fn activity_where(
    client: &Client,
    req: AgentsActivityRequest,
    what: &str,
    pred: impl Fn(&AgentsActivity) -> bool,
) -> AgentsActivity {
    timeout(WAIT, async {
        loop {
            let answer = activity(client, req).await;
            if pred(&answer) {
                return answer;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the activity never showed {what}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_activity_joins_turns_requests_and_sessions_across_workspaces() {
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

    // A chat in alpha that asks before it touches a file, and a terminal in beta.
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
    let shell = create_in(&client, Some(beta.id), sh("exec sleep 600")).await;
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

    // Asked and not answered: an open request, a running turn, two sessions that began.
    let since = AgentsActivityRequest {
        since_ms: Some(before),
        ..Default::default()
    };
    let waiting = activity_where(&client, since, "the open request", |a| {
        a.summary.requests_open == 1 && a.summary.turns_running == 1
    })
    .await;
    assert_eq!(waiting.summary.sessions_started, 2);
    assert_eq!(waiting.summary.requests_asked, 1);
    let open = waiting
        .events
        .iter()
        .find_map(|e| match &e.detail {
            ActivityDetail::Request {
                request,
                text,
                resolved_ms,
                ..
            } => Some((e, *request, text.clone(), *resolved_ms)),
            _ => None,
        })
        .expect("the request is listed");
    assert_eq!(open.0.session_id, Some(chat.id));
    assert_eq!(open.0.project, "alpha");
    assert_eq!(open.1, RequestKind::Approval);
    assert!(open.2.contains("touch hi.txt"), "{}", open.2);
    assert_eq!(open.3, None);
    assert!(waiting.omitted == 0 && !waiting.not_recorded.is_empty());

    // Answered: the wait is measured and says who decided; the turn ends.
    client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval.id,
            option_id: "accept".to_owned(),
        }))
        .await
        .unwrap();
    let done = activity_where(&client, since, "the finished turn", |a| {
        a.summary.turns_finished == 1 && a.summary.requests_resolved == 1
    })
    .await;
    assert_eq!(done.summary.requests_open, 0);
    assert!(done.summary.wait_longest_ms.is_some());
    assert_eq!(done.summary.wait_total_ms, done.summary.wait_longest_ms);
    assert!(done.events.iter().any(|e| matches!(
        &e.detail,
        ActivityDetail::Request { resolved_ms: Some(_), outcome: Some(outcome), decided_by: Some(_), .. }
            if outcome == "selected"
    )));
    assert!(done.events.iter().any(|e| matches!(
        &e.detail,
        ActivityDetail::TurnEnded {
            outcome: TurnOutcome::Completed,
            duration_ms: Some(_),
            ..
        }
    )));
    let times: Vec<u64> = done.events.iter().map(|e| e.at_ms).collect();
    assert!(times.windows(2).all(|w| w[0] >= w[1]), "newest first");

    // A workspace narrows it, with its counts.
    let only_beta = activity(
        &client,
        AgentsActivityRequest {
            workspace_id: Some(beta.id),
            ..since
        },
    )
    .await;
    assert_eq!(only_beta.events.len(), 1);
    assert_eq!(only_beta.events[0].session_id, Some(shell.id));
    assert_eq!(only_beta.summary.turns_finished, 0);
    let only_alpha = activity(
        &client,
        AgentsActivityRequest {
            workspace_id: Some(alpha.id),
            ..since
        },
    )
    .await;
    assert_eq!(only_alpha.summary.sessions_started, 1);
    assert_eq!(only_alpha.summary.turns_finished, 1);

    // A window after all of it is empty; one that ends before it, too.
    let now = hutan_client::time::now_ms();
    let after = activity(
        &client,
        AgentsActivityRequest {
            since_ms: Some(now + 1),
            until_ms: Some(now + 2),
            ..Default::default()
        },
    )
    .await;
    assert!(after.events.is_empty() && after.summary.sessions_started == 0);
    let early = activity(
        &client,
        AgentsActivityRequest {
            since_ms: Some(1),
            until_ms: Some(before),
            ..Default::default()
        },
    )
    .await;
    assert!(early.events.is_empty());
    assert!(
        early.coverage_from_ms.is_some_and(|from| from >= before),
        "the records begin with the sessions"
    );

    // A limit lists fewer and says how many it left out.
    let one = activity(
        &client,
        AgentsActivityRequest {
            limit: Some(1),
            ..since
        },
    )
    .await;
    assert_eq!(one.events.len(), 1);
    assert_eq!(one.omitted, done.events.len() - 1);
    assert_eq!(
        one.summary, done.summary,
        "the counts cover the whole window"
    );

    // Unknown ids are not found.
    for req in [
        AgentsActivityRequest {
            workspace_id: Some(WorkspaceId::new()),
            ..Default::default()
        },
        AgentsActivityRequest {
            session_id: Some(hutan_core::SessionId::new()),
            ..Default::default()
        },
    ] {
        let result = client
            .call::<AgentsActivity>(Request::AgentsActivity(req))
            .await;
        assert_eq!(remote_code(result), ErrorCode::NotFound);
    }
    daemon.stop().await;
}
