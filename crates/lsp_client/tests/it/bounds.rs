//! Nothing grows with what a server does: floods, a server that stops reading, and every
//! channel of the crate is bounded.

use std::time::Duration;

use hutan_buffer::{Buffer, Text, Transaction};
use hutan_lsp_client::events::{MAX_DIAGNOSTIC_FILES, MAX_LOG_LINES};
use hutan_lsp_client::ls_types::request::HoverRequest;
use hutan_lsp_client::ls_types::{
    HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams,
    WorkDoneProgressParams,
};
use hutan_lsp_client::{
    EVENT_QUEUE, LspError, OUTGOING_BYTES, OUTGOING_QUEUE, ServerEvent, ServerState,
};
use serde_json::json;

use crate::common;

#[tokio::test]
async fn a_flood_of_notifications_stays_within_the_bounds() {
    const LOGS: usize = 20_000;
    let mut fake = common::start(&json!({
        "flood": {"on": "initialized", "count": LOGS, "method": "window/logMessage"},
    }));
    // A second subscriber that never reads.
    let idle = fake.server.subscribe().unwrap();
    common::log_line(&mut fake, "flood done").await;
    assert!(idle.len() <= EVENT_QUEUE, "{}", idle.len());
    let log = fake.server.log();
    assert_eq!(log.len(), MAX_LOG_LINES);
    assert_eq!(log.last().unwrap().message, "flood done");
    assert_eq!(
        log[0].message,
        format!("flood {}", LOGS + 1 - MAX_LOG_LINES)
    );
    let stats = fake.server.queue_stats();
    assert!(stats.outgoing <= OUTGOING_QUEUE && stats.outgoing_bytes <= OUTGOING_BYTES);

    // The idle subscriber gets what fit, then learns how much it missed.
    let mut idle = idle;
    let mut kept = 0;
    let missed = loop {
        match idle.recv().await.unwrap() {
            ServerEvent::Lagged(missed) => break missed,
            _ => kept += 1,
        }
    };
    assert_eq!(kept, EVENT_QUEUE);
    assert!(missed as usize >= LOGS - EVENT_QUEUE, "{missed}");
    assert_eq!(fake.server.status().state, ServerState::Ready);
}

#[tokio::test]
async fn a_flood_of_diagnostics_keeps_a_bounded_store() {
    let files = MAX_DIAGNOSTIC_FILES + 500;
    let mut fake = common::start(&json!({
        "flood": {"on": "initialized", "count": files, "method": "textDocument/publishDiagnostics"},
    }));
    common::log_line(&mut fake, "flood done").await;
    let (stored, truncated) = fake.server.all_diagnostics();
    assert_eq!(stored.len(), MAX_DIAGNOSTIC_FILES);
    assert!(truncated);
    assert!(fake.events.len() <= EVENT_QUEUE);
}

#[tokio::test]
async fn a_server_that_stops_reading_never_grows_the_outgoing_queue() {
    let (mut config, dir) = common::config(&json!({"stall": true, "hover": "x"}));
    config.timeouts.request = Duration::from_millis(200);
    let fake = common::start_with(config, dir);
    common::ready(&fake).await;
    let uri = fake.uri("big.rs");
    // More than a pipe holds, so the writer blocks at once.
    let mut buffer = Buffer::from_content(&"let x = 1;\n".repeat(30_000));
    fake.server
        .open(uri.clone(), "rust", 0, buffer.text().clone())
        .unwrap();
    // Flush after every change until a flush can no longer complete: the queue is full.
    let mut version = 0;
    let mut flushes = 0;
    loop {
        let applied = buffer.apply(Transaction::insert(0, "x")).unwrap();
        version += 1;
        fake.server
            .change(
                &uri,
                version,
                buffer.text().clone(),
                &applied.transaction.changes(),
            )
            .unwrap();
        if tokio::time::timeout(Duration::from_secs(1), fake.server.flush())
            .await
            .is_err()
        {
            break;
        }
        flushes += 1;
        assert!(flushes <= OUTGOING_QUEUE + 2, "the queue never filled");
    }
    // Thousands more changes only move the pending range of the one document.
    for _ in 0..5_000 {
        let applied = buffer.apply(Transaction::insert(0, "y")).unwrap();
        version += 1;
        fake.server
            .change(
                &uri,
                version,
                buffer.text().clone(),
                &applied.transaction.changes(),
            )
            .unwrap();
        let stats = fake.server.queue_stats();
        assert!(stats.outgoing <= OUTGOING_QUEUE, "{stats:?}");
        assert!(stats.outgoing_bytes <= OUTGOING_BYTES, "{stats:?}");
    }
    let stats = fake.server.queue_stats();
    assert_eq!(stats.outgoing, OUTGOING_QUEUE, "{stats:?}");
    // A request cannot get through and gives up in time.
    let hover = fake
        .server
        .request::<HoverRequest>(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position::new(0, 0),
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await;
    assert!(matches!(hover, Err(LspError::Timeout { .. })), "{hover:?}");
    assert_eq!(fake.server.queue_stats().pending, 0);
    // Opening more documents is still instant.
    fake.server
        .open(fake.uri("other.rs"), "rust", 0, Text::from("x"))
        .unwrap();
}

/// `AGENTS.md`: bounded everything. The word for the opposite appears nowhere in the crate,
/// so no such channel can slip in.
#[test]
fn every_channel_of_the_crate_is_bounded() {
    let needle = ["un", "bounded"].concat();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut checked = 0;
    let mut dirs = vec![root.join("src"), root.join("tests")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if path.extension().is_some_and(|e| e == "rs") {
                let source = std::fs::read_to_string(&path).unwrap().to_lowercase();
                assert!(!source.contains(&needle), "{} mentions it", path.display());
                checked += 1;
            }
        }
    }
    assert!(checked >= 12, "only {checked} files checked");
}
