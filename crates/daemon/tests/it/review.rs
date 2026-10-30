//! `agents.review` (protocol 0.66) end to end: a real daemon whose Codex is `hutan-codex-fixture
//! replay` working in a git repository the test creates (it edits the files while the replay
//! holds the turn open, as the agent would), and a terminal agent that reports through a hook.
//! Nothing here starts a real harness or calls a model; the rules of the packet itself
//! (signals, bounds, redaction) are unit tests of `agents_review`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::checkpoint_turns::{codex, git_env, held_fixture, repository};
use crate::common;

use std::fs;
use std::time::Duration;

use common::{TestDaemon, WAIT, create_in, remote_code, sh, wait_event};
use hutan_core::{ApprovalPolicy, SandboxMode, SessionId, TurnId};
use hutan_protocol::ErrorCode;
use hutan_protocol::board::BoardSource;
use hutan_protocol::chat::{ApprovalRespond, ChatCreate, ChatSend, ChatSent};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Empty, Event, Request, SessionResult,
};
use hutan_protocol::review::{
    AgentsReviewRequest, ReviewFilesSource, ReviewToolStatus, ReviewTurnStatus, TurnReview,
};
use tokio::time::{sleep, timeout};

async fn review(
    client: &hutan_client::Client,
    session: SessionId,
    turn: Option<TurnId>,
) -> Result<TurnReview, hutan_client::ClientError> {
    client
        .call(Request::AgentsReview(AgentsReviewRequest {
            session_id: session,
            turn_id: turn,
        }))
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_turn_is_reviewed_from_its_journal_and_its_checkpoint() {
    let work = tempfile::tempdir().unwrap();
    let root = repository(work.path());
    let gate = work.path().join("gate");
    let transcript = held_fixture(work.path(), "approval-touch", &gate);
    let config = codex(&transcript, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with("", move |c| {
        c.codex = Some(config);
        c.checkpoints.git_env = git_env();
    })
    .await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let chat = client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: None,
            cwd: root.clone(),
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

    // Before any turn there is nothing to review.
    assert_eq!(
        remote_code(review(&client, chat.id, None).await),
        ErrorCode::NotFound
    );

    let turn = client
        .call::<ChatSent>(Request::ChatSend(ChatSend {
            session_id: chat.id,
            text: "Run `touch hi.txt` in a shell, then reply with the word done.".into(),
            attachments: vec![],
        }))
        .await
        .unwrap()
        .turn_id;
    // The agent edits a manifest and adds a file while the turn is held open.
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/new.rs"), "fn main() {}\n").unwrap();
    fs::write(&gate, "").unwrap();
    let approval = wait_event(&mut events, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some(approval.id),
        _ => None,
    })
    .await;

    // While it waits the turn can be reviewed by its id: it is running, and says what waits.
    let running = review(&client, chat.id, Some(turn)).await.unwrap();
    assert_eq!(
        running.turn.as_ref().unwrap().status,
        ReviewTurnStatus::Running
    );
    assert_eq!(running.requests.len(), 1);
    assert!(running.requests[0].title.contains("touch hi.txt"));
    assert!(
        running
            .not_recorded
            .iter()
            .any(|n| n.what == "the end of the turn")
    );
    // The default is the last *finished* turn: there is none yet.
    assert_eq!(
        remote_code(review(&client, chat.id, None).await),
        ErrorCode::NotFound
    );

    client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval,
            option_id: "accept".into(),
        }))
        .await
        .unwrap();
    wait_event(&mut events, "the turn's end", |event| match event {
        Event::ChatEvent { event, .. } if event.kind.ends_turn() => Some(()),
        _ => None,
    })
    .await;

    // The checkpoint settles a moment after the turn: wait for the files, not for time.
    let packet = timeout(WAIT, async {
        loop {
            let packet = review(&client, chat.id, None).await.unwrap();
            if packet
                .files
                .as_ref()
                .is_some_and(|f| f.source == ReviewFilesSource::Checkpoint)
            {
                return packet;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the checkpoint never showed in the review");

    assert_eq!(packet.session_id, chat.id);
    assert_eq!(packet.source, BoardSource::Journal);
    assert_eq!(packet.harness.as_deref(), Some("codex"));
    let reviewed = packet.turn.as_ref().unwrap();
    assert_eq!(reviewed.turn_id, turn);
    assert_eq!(reviewed.number, 1);
    assert_eq!(reviewed.status, ReviewTurnStatus::Completed);
    assert!(reviewed.wall_ms.is_some());
    assert!(
        packet
            .prompt
            .as_ref()
            .unwrap()
            .text
            .contains("touch hi.txt"),
        "{:?}",
        packet.prompt
    );
    assert_eq!(packet.final_message.as_ref().unwrap().text, "done");

    // The command, as the journal has it: its words and what the harness said of its end.
    let commands = packet.commands.as_ref().unwrap();
    assert_eq!(commands.total, 1);
    let command = &commands.items[0];
    assert!(command.text.contains("touch hi.txt"), "{command:?}");
    assert_eq!(command.status, ReviewToolStatus::Completed);
    if command.exit_code.is_none() {
        // A harness that reports no exit code leaves it absent and the packet says so.
        assert!(
            packet
                .not_recorded
                .iter()
                .any(|n| n.what.contains("exit codes")),
            "{:?}",
            packet.not_recorded
        );
    }

    // The files the checkpoint recorded: exact, with lines.
    let files = packet.files.as_ref().unwrap();
    let paths: Vec<&str> = files.items.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["Cargo.toml", "src/new.rs"]);
    assert_eq!(files.total, 2);
    assert!(files.checkpoint_id.is_some());
    assert!(files.added.unwrap_or(0) > 0);
    let new = files.items.iter().find(|f| f.path == "src/new.rs").unwrap();
    assert_eq!((new.added, new.removed), (Some(1), Some(0)));

    // Signals point at that evidence, by name.
    let ids: Vec<&str> = packet.signals.iter().map(|s| s.id.as_str()).collect();
    assert!(ids.contains(&"dependency_file_changed"), "{ids:?}");
    assert!(ids.contains(&"no_test_command"), "{ids:?}");
    assert!(
        !ids.contains(&"failed_command_then_done"),
        "no command failed: {ids:?}"
    );
    let manifest = packet
        .signals
        .iter()
        .find(|s| s.id.as_str() == "dependency_file_changed")
        .unwrap();
    assert_eq!(manifest.evidence, ["Cargo.toml"]);
    assert!(packet.requests.is_empty());
    assert!(serde_json::to_vec(&packet).unwrap().len() <= 24 * 1024);

    // By id it is the same turn; an id the journal does not have is `not_found`.
    let by_id = review(&client, chat.id, Some(turn)).await.unwrap();
    assert_eq!(by_id.turn.as_ref().unwrap().turn_id, turn);
    assert_eq!(
        remote_code(review(&client, chat.id, Some(TurnId::new())).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(review(&client, SessionId::new(), None).await),
        ErrorCode::NotFound
    );

    // A read: an agent-originated connection may ask too.
    let agent = daemon
        .connect_as(hutan_protocol::messages::ClientKind::Cli)
        .await;
    assert_eq!(
        review(&agent, chat.id, None)
            .await
            .unwrap()
            .turn
            .unwrap()
            .turn_id,
        turn
    );
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_agent_has_a_report_and_the_packet_says_what_it_cannot_know() {
    let daemon = TestDaemon::start().await;
    let (client, _events) = daemon.connect_subscribed().await;
    let hooked = create_in(&client, None, sh("exec sleep 600")).await;
    client
        .call::<AgentApplied>(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: hooked.id,
            source: "fake-hook".into(),
            agent: "claude".into(),
            state: Some(AgentState::Blocked),
            message: Some("Waiting for permission".into()),
            seq: 1,
            agent_session_id: None,
            resume_argv: None,
            node: None,
        }))
        .await
        .unwrap();
    let packet = review(&client, hooked.id, None).await.unwrap();
    assert_eq!(packet.source, BoardSource::Hook);
    assert!(packet.turn.is_none() && packet.commands.is_none() && packet.files.is_none());
    let report = packet.report.as_ref().unwrap();
    assert_eq!(report.state, "blocked");
    assert_eq!(report.message.as_deref(), Some("Waiting for permission"));
    let what: Vec<&str> = packet
        .not_recorded
        .iter()
        .map(|n| n.what.as_str())
        .collect();
    for expected in [
        "turns and their outcome",
        "tools and commands, with exit codes",
        "files changed and lines",
    ] {
        assert!(what.contains(&expected), "{what:?}");
    }
    assert!(packet.text().contains("a terminal agent has no journal"));

    // Recognised only (OSC 7501): state and title, and it says so.
    let recognised = create_in(&client, None, sh("exec cat")).await;
    client
        .send_input(
            common::terminal_of(&recognised),
            b"\x1b]7501;state=working:app=omp\x1b\\\r",
        )
        .await
        .unwrap();
    let packet = timeout(WAIT, async {
        loop {
            if let Ok(packet) = review(&client, recognised.id, None).await
                && packet.source == BoardSource::Recognised
            {
                return packet;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the recognised agent was never reviewable");
    assert_eq!(packet.report.as_ref().unwrap().state, "working");
    assert!(packet.not_recorded[0].why.contains("recognised"));

    // A plain shell is no agent: there is nothing to review.
    let shell = create_in(&client, None, sh("exec sleep 600")).await;
    assert_eq!(
        remote_code(review(&client, shell.id, None).await),
        ErrorCode::BadRequest
    );
    daemon.stop().await;
}
