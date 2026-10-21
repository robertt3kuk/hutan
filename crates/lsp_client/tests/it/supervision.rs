//! Crashes, restarts, timeouts, cancellation, stop, and the process tree, against the fake
//! server.

use std::time::Duration;

use futures::FutureExt;
use hutan_buffer::Text;
use hutan_lsp_client::ls_types::request::HoverRequest;
use hutan_lsp_client::ls_types::{
    HoverContents, HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
    WorkDoneProgressParams,
};
use hutan_lsp_client::{Launch, LspError, Server, ServerConfig, ServerEvent, ServerState};
use serde_json::json;

use crate::common::{self, Fake};

fn hover_params(uri: &Uri) -> HoverParams {
    HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position::new(0, 0),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

async fn finished(fake: &Fake) -> hutan_lsp_client::ServerStatus {
    tokio::time::timeout(
        common::PATIENCE,
        fake.server.wait_for(|s| s.state.is_final()),
    )
    .await
    .expect("final state within the patience")
}

/// The child the fake started (`child <pid>` in its log).
async fn child_pid(fake: &mut Fake) -> u32 {
    let line = common::log_line(fake, "child ").await;
    line.trim_start_matches("child ")
        .parse()
        .unwrap_or_else(|_| panic!("unexpected log line {line:?}"))
}

/// Waits until our reaper collected `pid` (not even a zombie is left).
async fn wait_reaped(pid: u32) {
    tokio::time::timeout(common::PATIENCE, async {
        while hutan_terminal::procs::inspect(pid).is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("process {pid} was not reaped"));
}

#[tokio::test]
async fn a_crash_restarts_with_backoff_and_ends_failed_after_the_limit() {
    let mut fake = common::start(&json!({"crash": {"on": "initialize", "code": 7}}));
    let mut delays = Vec::new();
    let restarting_or_failed = |e: &ServerEvent| {
        matches!(
            e,
            ServerEvent::Restarting { .. } | ServerEvent::State(ServerState::Failed)
        )
    };
    while let ServerEvent::Restarting {
        attempt,
        delay,
        reason,
    } = common::event(&mut fake.events, restarting_or_failed).await
    {
        assert_eq!(attempt as usize, delays.len() + 1);
        assert!(reason.contains("code 7"), "{reason}");
        delays.push(delay);
    }
    // Three failures (the policy's limit) within the window: two restarts, doubling delays.
    assert_eq!(
        delays,
        [Duration::from_millis(20), Duration::from_millis(40)]
    );
    let status = fake.server.status();
    assert_eq!(status.state, ServerState::Failed);
    assert_eq!(status.restarts, 2);
    assert_eq!(status.pid, None);
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("code 7"),
        "{status:?}"
    );
    let refused = fake
        .server
        .request::<HoverRequest>(hover_params(&fake.uri("a")))
        .await;
    assert!(matches!(
        refused,
        Err(LspError::NotRunning {
            state: ServerState::Failed
        })
    ));
}

#[tokio::test]
async fn a_crashed_server_comes_back_with_the_current_documents() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("crashes");
    let mut fake = common::start(&json!({
        "capabilities": {"textDocumentSync": 2, "hoverProvider": true},
        "crash": {"on": "textDocument/didChange", "times": 1, "counter": counter},
        "hover": "echo",
    }));
    common::ready(&fake).await;
    let first = fake.server.pid().unwrap();
    let uri = fake.uri("a.rs");
    fake.server
        .open(uri.clone(), "rust", 1, Text::from("one"))
        .unwrap();
    fake.server.flush().await;
    fake.server.replace(&uri, 2, Text::from("two")).unwrap();
    // The change crashes the server once; it is restarted and given the document again.
    common::event(&mut fake.events, |e| {
        matches!(e, ServerEvent::Restarting { attempt: 1, .. })
    })
    .await;
    common::event(&mut fake.events, |e| {
        matches!(e, ServerEvent::State(ServerState::Ready))
    })
    .await;
    assert_ne!(fake.server.pid(), Some(first));
    wait_reaped(first).await;
    let hover = fake
        .server
        .request::<HoverRequest>(hover_params(&uri))
        .await
        .unwrap()
        .unwrap();
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("{hover:?}");
    };
    let state: serde_json::Value = serde_json::from_str(&markup.value).unwrap();
    assert_eq!(state["text"], "two");
    assert_eq!(state["version"], 2);
    assert_eq!(state["opens"], 1);
    assert_eq!(state["changes"], 0);
    assert_eq!(fake.server.status().restarts, 1);
}

#[tokio::test]
async fn a_hung_request_times_out_and_is_cancelled() {
    let (mut config, dir) = common::config(&json!({"hang": ["textDocument/hover"], "hover": "x"}));
    config.timeouts.request = Duration::from_millis(300);
    let mut fake = common::start_with(config, dir);
    common::ready(&fake).await;
    let uri = fake.uri("a.rs");
    let result = fake
        .server
        .request::<HoverRequest>(hover_params(&uri))
        .await;
    assert!(
        matches!(&result, Err(LspError::Timeout { method }) if method == "textDocument/hover"),
        "{result:?}"
    );
    let cancelled = common::log_line(&mut fake, "cancelled ").await;
    assert_eq!(fake.server.queue_stats().pending, 0);

    // A request the caller drops once it is in flight is withdrawn as well.
    let mut dropped = Box::pin(fake.server.request::<HoverRequest>(hover_params(&uri)));
    tokio::time::timeout(common::PATIENCE, async {
        while fake.server.queue_stats().pending == 0 {
            assert!((&mut dropped).now_or_never().is_none(), "answered");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("request in flight");
    drop(dropped);
    assert_eq!(fake.server.queue_stats().pending, 0);
    let cancellations = |fake: &Fake| {
        fake.server
            .log()
            .into_iter()
            .filter(|entry| entry.message.starts_with("cancelled "))
            .map(|entry| entry.message)
            .collect::<Vec<_>>()
    };
    while cancellations(&fake).len() < 2 {
        common::event(&mut fake.events, |e| matches!(e, ServerEvent::Log(_))).await;
    }
    let lines = cancellations(&fake);
    assert_eq!(lines[0], cancelled);
    assert_ne!(lines[1], cancelled);
    assert_eq!(fake.server.status().state, ServerState::Ready);
}

#[tokio::test]
async fn dropping_the_server_kills_it_and_its_children() {
    let mut fake = common::start(&json!({"spawn_child": true}));
    common::ready(&fake).await;
    let child = child_pid(&mut fake).await;
    let pid = fake.server.pid().unwrap();
    // Resources read the tree from the server's pid: the child is below it, with its RSS.
    let tree = hutan_terminal::procs::descendants(pid, 64);
    assert!(tree.contains(&child), "{tree:?} lacks {child}");
    for p in [pid, child] {
        let info = hutan_terminal::procs::inspect(p).expect("running");
        assert!(info.rss_bytes > 0, "{info:?}");
    }
    drop(fake.server);
    wait_reaped(pid).await;
    common::wait_gone(child).await;
}

#[tokio::test]
async fn stop_shuts_down_and_kills_a_server_that_ignores_exit() {
    let fake = common::start(&json!({}));
    common::ready(&fake).await;
    let pid = fake.server.pid().unwrap();
    fake.server.stop().await;
    assert_eq!(fake.server.status().state, ServerState::Stopped);
    assert_eq!(fake.server.pid(), None);
    wait_reaped(pid).await;

    let (mut config, dir) = common::config(&json!({
        "hang": ["shutdown"],
        "ignore_exit": true,
        "spawn_child": true,
    }));
    config.timeouts.shutdown = Duration::from_millis(200);
    config.timeouts.exit = Duration::from_millis(200);
    let mut fake = common::start_with(config, dir);
    common::ready(&fake).await;
    let child = child_pid(&mut fake).await;
    let pid = fake.server.pid().unwrap();
    fake.server.stop().await;
    let status = finished(&fake).await;
    assert_eq!(status.state, ServerState::Stopped);
    wait_reaped(pid).await;
    common::wait_gone(child).await;
}

#[tokio::test]
async fn a_missing_server_fails_without_restarting() {
    let dir = tempfile::tempdir().unwrap();
    let launch = Launch {
        program: dir.path().join("no-such-language-server"),
        args: Vec::new(),
        cwd: None,
        env: Vec::new(),
    };
    let server = Server::start(ServerConfig::new("missing", launch, dir.path()));
    let status = tokio::time::timeout(common::PATIENCE, server.wait_for(|s| s.state.is_final()))
        .await
        .unwrap();
    assert_eq!(status.state, ServerState::Failed);
    assert_eq!(status.restarts, 0);
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("cannot start"),
        "{status:?}"
    );
    server.stop().await;
    assert_eq!(server.status().state, ServerState::Stopped);
}

#[tokio::test]
async fn server_requests_are_answered_and_progress_drives_indexing() {
    let (mut config, dir) = common::config(&json!({
        "progress": 3,
        "configuration": ["fake.section", "fake.missing"],
    }));
    config.settings = json!({"fake": {"section": {"answer": 42}}});
    let mut fake = common::start_with(config, dir);
    let mut states = Vec::new();
    while states.last() != Some(&ServerState::Ready) || !states.contains(&ServerState::Indexing) {
        if let ServerEvent::State(state) =
            common::event(&mut fake.events, |e| matches!(e, ServerEvent::State(_))).await
        {
            states.push(state);
        }
    }
    assert_eq!(
        states,
        [
            ServerState::Ready,
            ServerState::Indexing,
            ServerState::Ready
        ]
    );
    let line = common::log_line(&mut fake, "configuration ").await;
    assert_eq!(line, r#"configuration [{"answer":42},null]"#);
}
