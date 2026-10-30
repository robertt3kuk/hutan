//! `agents.board` (protocol 0.63) end to end: a real daemon on a temporary `HUTAN_HOME`, a
//! Codex chat that asks for permission (`hutan-codex-fixture replay`), a terminal agent that
//! reports through a hook and one that is only recognised from OSC 7501, in two workspaces.
//! Nothing here starts a real harness or calls a model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, create_in, remote_code, sh, terminal_of, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::{ApprovalPolicy, SandboxMode, SessionId, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::board::{
    AgentsBoard, AgentsBoardRequest, BoardGroup, BoardKind, BoardSource, BoardState, BoardWaitKind,
};
use hutan_protocol::chat::{ApprovalInfo, ChatCreate, ChatSend};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Event, Request, SessionResult, WorkspaceCreate,
    WorkspaceInfo,
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
    assert!(
        bin.is_file(),
        "hutan-codex-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
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

async fn board(client: &Client, workspace_id: Option<WorkspaceId>, finished: bool) -> AgentsBoard {
    client
        .call(Request::AgentsBoard(AgentsBoardRequest {
            workspace_id,
            include_finished: finished,
        }))
        .await
        .unwrap()
}

/// Polls the board until `pred` holds for it.
async fn board_where(
    client: &Client,
    what: &str,
    pred: impl Fn(&AgentsBoard) -> bool,
) -> AgentsBoard {
    timeout(WAIT, async {
        loop {
            let board = board(client, None, false).await;
            if pred(&board) {
                return board;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the board never showed {what}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_board_joins_chats_hooks_and_recognised_agents_across_workspaces() {
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

    // A chat in alpha that asks before it touches a file.
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

    // In beta: one terminal agent that reports through a hook, one only recognised from OSC 7501.
    let hooked = create_in(&client, Some(beta.id), sh("exec sleep 600")).await;
    client
        .call::<AgentApplied>(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: hooked.id,
            source: "fake-hook".into(),
            agent: "omp".into(),
            state: Some(AgentState::Working),
            message: Some("Editing src/main.rs".into()),
            seq: 1,
            agent_session_id: None,
            resume_argv: None,
            node: None,
        }))
        .await
        .unwrap();
    let recognised = create_in(&client, Some(beta.id), sh("exec cat")).await;
    client
        .send_input(
            terminal_of(&recognised),
            b"\x1b]7501;state=working:app=omp\x1b\\\r",
        )
        .await
        .unwrap();
    // A plain shell is no agent.
    create_in(&client, Some(beta.id), sh("exec sleep 600")).await;

    let all = board_where(&client, "every agent", |b| {
        b.counts.agents == 3 && b.counts.working == 2 && b.counts.requests == 1
    })
    .await;
    assert_eq!(all.agents.len(), 3);

    // The one that blocks comes first, with what it asks and since when.
    let first = &all.agents[0];
    assert_eq!(first.session_id, chat.id);
    assert_eq!(first.kind, BoardKind::Chat);
    assert_eq!(first.source, BoardSource::Journal);
    // The turn is still in progress (state is the activity); the group says it needs the user.
    assert!(first.turn_active);
    assert_eq!(
        (first.state, first.group),
        (BoardState::Working, BoardGroup::Requests)
    );
    assert_eq!(first.harness, "Codex");
    assert_eq!(first.project.id, alpha.id);
    assert_eq!(first.waiting.len(), 1);
    let wait = &first.waiting[0];
    assert_eq!(wait.kind, BoardWaitKind::Approval);
    assert!(wait.title.contains("touch hi.txt"), "{}", wait.title);
    assert_eq!(wait.since_ms, Some(approval.requested_at_ms));
    assert_eq!(
        wait.ref_id.as_deref(),
        Some(approval.id.to_string().as_str())
    );
    assert_eq!(
        first.reported_ms, None,
        "a chat the daemon runs has no report time"
    );

    // Coverage says how each is seen.
    assert_eq!(
        (
            all.coverage.chats,
            all.coverage.terminal_reported,
            all.coverage.terminal_recognised
        ),
        (1, 1, 1)
    );
    let by_id = |id: SessionId| all.agents.iter().find(|a| a.session_id == id).unwrap();
    let hook_row = by_id(hooked.id);
    assert_eq!(hook_row.source, BoardSource::Hook);
    assert_eq!(hook_row.kind, BoardKind::Terminal);
    assert_eq!(hook_row.activity.as_deref(), Some("Editing src/main.rs"));
    assert_eq!(hook_row.project.id, beta.id);
    assert_eq!(by_id(recognised.id).source, BoardSource::Recognised);

    // A workspace narrows the board to its agents, counts included.
    let only_beta = board(&client, Some(beta.id), false).await;
    assert_eq!(only_beta.agents.len(), 2);
    assert!(only_beta.agents.iter().all(|a| a.project.id == beta.id));
    assert_eq!(only_beta.counts.agents, 2);
    assert_eq!(only_beta.counts.requests, 0);
    let only_alpha = board(&client, Some(alpha.id), false).await;
    assert_eq!(
        only_alpha
            .agents
            .iter()
            .map(|a| a.session_id)
            .collect::<Vec<_>>(),
        [chat.id]
    );
    let missing = client
        .call::<AgentsBoard>(Request::AgentsBoard(AgentsBoardRequest {
            workspace_id: Some(WorkspaceId::new()),
            include_finished: false,
        }))
        .await;
    assert_eq!(remote_code(missing), ErrorCode::NotFound);

    // The same request is read-only: an agent-originated connection may ask.
    let agent = daemon
        .connect_as(hutan_protocol::messages::ClientKind::Cli)
        .await;
    assert_eq!(board(&agent, None, false).await.counts.agents, 3);

    daemon.stop().await;
}
