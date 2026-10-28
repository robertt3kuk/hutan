//! AGENT-TERM-1: the coding agent of a terminal — from OSC 7501 and from its program — is a
//! field of the session (`terminal_agent`), feeds the agent tree like a hook's report, names
//! the session, and goes with the process. Real PTYs against an in-process daemon on a
//! temporary `HUTAN_HOME`; the OSC bytes are the ones OMP 18.12 writes (`cat` echoes them).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use base64::Engine as _;
use common::*;
use hutan_client::Client;
use hutan_core::SessionId;
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Empty, Request, SessionInfo, SessionRename,
    SessionResult, SessionSnapshot, TerminalAgentSource, TerminalAgentState,
};
use hutan_protocol::tree::{AgentTree, AgentTreeRequest};
use tokio::time::{sleep, timeout};

async fn session_of(client: &Client, id: SessionId) -> SessionInfo {
    let snapshot: SessionSnapshot = client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap();
    snapshot
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("session {id} missing"))
}

/// Polls the registry until `pred` holds for the session.
async fn wait_session(
    client: &Client,
    id: SessionId,
    what: &str,
    pred: impl Fn(&SessionInfo) -> bool,
) -> SessionInfo {
    timeout(WAIT, async {
        loop {
            let session = session_of(client, id).await;
            if pred(&session) {
                return session;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("session {id}: timed out waiting for {what}"))
}

/// A terminal that echoes what it is sent, so the OSC bytes arrive as program output.
async fn echo_session(client: &Client) -> SessionInfo {
    create(client, sh("exec cat")).await
}

/// `payload` as an OSC string, ST-terminated as OMP writes it; the newline makes `cat` echo it.
async fn osc(client: &Client, session: &SessionInfo, payload: &str) {
    client
        .send_input(
            terminal_of(session),
            format!("\x1b]{payload}\x1b\\\r").as_bytes(),
        )
        .await
        .unwrap();
}

async fn status(client: &Client, session: &SessionInfo, fields: &str) {
    osc(client, session, &format!("7501;{fields}")).await;
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn in_state(state: TerminalAgentState) -> impl Fn(&SessionInfo) -> bool {
    move |s| s.terminal_agent.as_ref().is_some_and(|a| a.state == state)
}

fn summary_in(state: AgentState) -> impl Fn(&SessionInfo) -> bool {
    move |s| s.agent.as_ref().is_some_and(|a| a.state == state)
}

#[tokio::test(flavor = "multi_thread")]
async fn osc_7501_reports_become_the_terminal_agent_and_feed_the_attention_model() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = echo_session(&client).await;
    assert!(session.terminal_agent.is_none() && session.agent.is_none());

    // The agent starts: known, idle, reported by itself.
    status(&client, &session, "state=idle:app=omp").await;
    let info = wait_session(&client, session.id, "idle omp", |s| {
        s.terminal_agent.is_some()
    })
    .await;
    let agent = info.terminal_agent.unwrap();
    assert_eq!(
        (agent.app.as_str(), agent.state, agent.source),
        ("omp", TerminalAgentState::Idle, TerminalAgentSource::Osc)
    );

    // A turn: working, through the agent tree to the summary older clients read.
    status(&client, &session, "state=working:app=omp").await;
    let info = wait_session(
        &client,
        session.id,
        "working",
        in_state(TerminalAgentState::Working),
    )
    .await;
    let working_since = info.terminal_agent.as_ref().unwrap().since_ms;
    let summary = wait_session(
        &client,
        session.id,
        "the tree summary",
        summary_in(AgentState::Working),
    )
    .await
    .agent
    .unwrap();
    assert_eq!(
        (summary.agent.as_str(), summary.source.as_str()),
        ("omp", "osc7501")
    );

    // It asks: blocked is a request with the question, the secret in it redacted.
    status(
        &client,
        &session,
        &format!(
            "state=blocked:kind=permission:app=omp:msg={}",
            b64("Run the tests with API_KEY=sk-live-0123456789abcdef0123?")
        ),
    )
    .await;
    let info = wait_session(
        &client,
        session.id,
        "blocked",
        in_state(TerminalAgentState::Blocked),
    )
    .await;
    let agent = info.terminal_agent.unwrap();
    assert!(agent.since_ms >= working_since);
    assert_eq!(agent.kind.as_deref(), Some("permission"));
    let question = agent.message.unwrap();
    assert!(question.starts_with("Run the tests with"), "{question}");
    assert!(!question.contains("0123456789abcdef"), "{question}");
    let summary = wait_session(
        &client,
        session.id,
        "blocked summary",
        summary_in(AgentState::Blocked),
    )
    .await
    .agent
    .unwrap();
    assert_eq!(summary.message.as_deref(), Some(question.as_str()));

    // The turn fails: the message stays on the agent; the finished node is no summary.
    status(
        &client,
        &session,
        &format!("state=error:app=omp:msg={}", b64("Rate limited")),
    )
    .await;
    let info = wait_session(
        &client,
        session.id,
        "error",
        in_state(TerminalAgentState::Error),
    )
    .await;
    assert_eq!(
        info.terminal_agent.unwrap().message.as_deref(),
        Some("Rate limited")
    );
    wait_session(&client, session.id, "no summary after a failure", |s| {
        s.agent.is_none()
    })
    .await;

    // The next prompt works again; its end is `done`, the agent still there, idle in the tree.
    status(&client, &session, "state=working:app=omp").await;
    wait_session(
        &client,
        session.id,
        "working again",
        summary_in(AgentState::Working),
    )
    .await;
    status(&client, &session, "state=done:app=omp").await;
    let info = wait_session(&client, session.id, "done", |s| {
        in_state(TerminalAgentState::Done)(s) && summary_in(AgentState::Idle)(s)
    })
    .await;
    assert_eq!(info.terminal_agent.unwrap().message, None);

    // It exits: nothing of it is left.
    status(&client, &session, "state=clear").await;
    wait_session(&client, session.id, "cleared", |s| {
        s.terminal_agent.is_none() && s.agent.is_none()
    })
    .await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hook_that_speaks_for_the_agent_is_not_counted_twice() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = echo_session(&client).await;
    let reported = client
        .call::<AgentApplied>(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: session.id,
            source: "omp-extension".into(),
            agent: "omp".into(),
            state: Some(AgentState::Working),
            message: None,
            seq: 1,
            agent_session_id: None,
            resume_argv: None,
            node: None,
        }))
        .await
        .unwrap();
    assert!(reported.applied);

    // The agent also reports itself: the hook's tree is untouched, the terminal agent is there.
    status(
        &client,
        &session,
        &format!(
            "state=blocked:kind=question:app=omp:msg={}",
            b64("Which one?")
        ),
    )
    .await;
    let info = wait_session(
        &client,
        session.id,
        "terminal agent",
        in_state(TerminalAgentState::Blocked),
    )
    .await;
    let summary = info.agent.expect("the hook's summary stays");
    assert_eq!(
        (summary.source.as_str(), summary.state),
        ("omp-extension", AgentState::Working)
    );
    let tree: AgentTree = client
        .call(Request::AgentTree(AgentTreeRequest {
            session_id: Some(session.id),
        }))
        .await
        .unwrap();
    let sources: Vec<&str> = tree
        .observations
        .iter()
        .flat_map(|o| o.nodes.values())
        .map(|n| n.source.as_str())
        .collect();
    assert_eq!(sources, ["omp-extension"]);

    // The end of the agent clears its terminal agent and leaves the hook's node alone.
    status(&client, &session, "state=clear").await;
    wait_session(&client, session.id, "cleared", |s| {
        s.terminal_agent.is_none()
    })
    .await;
    assert_eq!(
        session_of(&client, session.id).await.agent.unwrap().source,
        "omp-extension"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_program_in_the_foreground_shows_the_agent_before_it_reports() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let dir = tempfile::tempdir().unwrap();
    // A script called `omp`: the interpreter runs it, so its command line is `sh …/omp`.
    let script = dir.path().join("omp");
    std::fs::write(&script, "#!/bin/sh\nsleep 600\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let session = create(&client, spec(&[script.to_str().unwrap()])).await;
    let info = wait_session(&client, session.id, "the program's agent", |s| {
        s.terminal_agent.is_some()
    })
    .await;
    let agent = info.terminal_agent.unwrap();
    assert_eq!(
        (agent.app.as_str(), agent.state, agent.source),
        (
            "omp",
            TerminalAgentState::Unknown,
            TerminalAgentSource::Process
        )
    );

    // The end of the process takes the agent with it.
    stop(&client, session.id).await.unwrap();
    wait_session(&client, session.id, "gone with the process", |s| {
        s.terminal_agent.is_none() && !s.state.is_live()
    })
    .await;
    t.stop().await;
}
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_names_its_session_unless_the_user_did() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = echo_session(&client).await;
    let renamed = echo_session(&client).await;
    client
        .call::<SessionResult>(Request::SessionRename(SessionRename {
            session_id: renamed.id,
            title: "my build".into(),
        }))
        .await
        .unwrap();

    for target in [&session, &renamed] {
        status(&client, target, "state=working:app=omp").await;
        // OMP's title while a turn runs: its mark, a spinner frame, then its session's name.
        osc(&client, target, "0;π ⠋ 👷 WORKER: worker").await;
    }
    let named = wait_session(&client, session.id, "named by the agent", |s| {
        s.title == "👷 WORKER: worker"
    })
    .await;
    assert_eq!(
        named.terminal_agent.unwrap().title.as_deref(),
        Some("👷 WORKER: worker")
    );
    // A spinner frame is no new name.
    osc(&client, &session, "0;π ⠙ 👷 WORKER: worker").await;
    status(&client, &session, "state=done:app=omp").await;
    let after = wait_session(
        &client,
        session.id,
        "done",
        in_state(TerminalAgentState::Done),
    )
    .await;
    assert_eq!(after.title, "👷 WORKER: worker");

    // The user's name stays; the agent's own is the subtitle.
    let kept = wait_session(&client, renamed.id, "the agent's title", |s| {
        s.terminal_agent.as_ref().is_some_and(|a| a.title.is_some())
    })
    .await;
    assert_eq!(kept.title, "my build");
    assert_eq!(
        kept.terminal_agent.unwrap().title.as_deref(),
        Some("👷 WORKER: worker")
    );

    // When the agent leaves, its title goes with it: the session is named by its place again.
    status(&client, &session, "state=clear").await;
    let left = wait_session(&client, session.id, "named by its place", |s| {
        s.terminal_agent.is_none() && s.title != "👷 WORKER: worker"
    })
    .await;
    assert!(!left.title.contains('π'), "{}", left.title);
    t.stop().await;
}
