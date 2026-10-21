//! initialize → diagnostics → hover → definition, document sync and position encodings,
//! against the fake server.

use hutan_buffer::{Buffer, Text, Transaction};
use hutan_lsp_client::ls_types::request::{GotoDefinition, HoverRequest, References};
use hutan_lsp_client::ls_types::{
    GotoDefinitionParams, GotoDefinitionResponse, HoverContents, HoverParams, Location, MarkupKind,
    PartialResultParams, Position, Range, ReferenceContext, ReferenceParams,
    TextDocumentIdentifier, TextDocumentPositionParams, Uri, WorkDoneProgressParams,
};
use hutan_lsp_client::position::{self, PositionEncoding};
use hutan_lsp_client::{LspError, ServerEvent};
use serde_json::{Value, json};

use crate::common::{self, Fake};

fn at(uri: &Uri, line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: Position::new(line, character),
    }
}

fn hover_params(uri: &Uri) -> HoverParams {
    HoverParams {
        text_document_position_params: at(uri, 0, 0),
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

/// Waits for diagnostics of `uri` at `version`.
async fn diagnostics_at(fake: &mut Fake, uri: &Uri, version: i32) {
    if fake
        .server
        .diagnostics(uri)
        .is_some_and(|d| d.version == Some(version))
    {
        return;
    }
    common::event(&mut fake.events, |event| {
        matches!(event, ServerEvent::Diagnostics { uri: u, version: Some(v) } if u == uri && *v == version)
    })
    .await;
}

/// The fake's mirror of `uri`, read through its `echo` hover.
async fn mirror(fake: &Fake, uri: &Uri) -> Value {
    let hover = fake
        .server
        .request::<HoverRequest>(hover_params(uri))
        .await
        .expect("hover")
        .expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("unexpected hover {hover:?}");
    };
    serde_json::from_str(&markup.value).expect("echo JSON")
}

/// The text the diagnostics of `uri` cover, converted with the negotiated encoding.
fn diagnosed(fake: &Fake, uri: &Uri, text: &Text) -> Vec<String> {
    let encoding = fake.server.encoding().expect("running");
    let file = fake.server.diagnostics(uri).expect("diagnostics");
    file.diagnostics
        .iter()
        .map(|d| {
            let a = position::offset(text, d.range.start, encoding);
            let b = position::offset(text, d.range.end, encoding);
            text.slice(a..b).expect("range in text")
        })
        .collect()
}

#[tokio::test]
async fn initialize_diagnostics_hover_definition() {
    let mut fake = common::start(&json!({
        "capabilities": {
            "positionEncoding": "utf-8",
            "textDocumentSync": {"openClose": true, "change": 2, "save": {"includeText": false}},
            "hoverProvider": true,
            "definitionProvider": true,
        },
        "diagnostics": true,
        "hover": "**fake** hover",
    }));
    common::ready(&fake).await;
    let status = fake.server.status();
    assert_eq!(status.encoding, Some(PositionEncoding::Utf8));
    assert!(status.pid.is_some());
    assert!(fake.server.capabilities().unwrap().hover_provider.is_some());

    let uri = fake.uri("main.rs");
    let text = Text::from("fn main() {\n    let error = 1; // é error\n}\n");
    fake.server
        .open(uri.clone(), "rust", 1, text.clone())
        .unwrap();
    diagnostics_at(&mut fake, &uri, 1).await;
    assert_eq!(diagnosed(&fake, &uri, &text), ["error", "error"]);

    let hover = fake
        .server
        .request::<HoverRequest>(HoverParams {
            text_document_position_params: at(&uri, 1, 8),
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .unwrap()
        .unwrap();
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("{hover:?}");
    };
    assert_eq!(markup.kind, MarkupKind::Markdown);
    assert_eq!(markup.value, "**fake** hover");
    assert_eq!(
        hover.range,
        Some(Range::new(Position::new(1, 8), Position::new(1, 8)))
    );

    let definition = fake
        .server
        .request::<GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: at(&uri, 1, 8),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        definition,
        Some(GotoDefinitionResponse::Scalar(Location {
            uri: uri.clone(),
            range: Range::new(Position::new(0, 0), Position::new(0, 2)),
        }))
    );

    // Not announced: refused without asking the server.
    let references = fake
        .server
        .request::<References>(ReferenceParams {
            text_document_position: at(&uri, 1, 8),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        })
        .await;
    assert!(matches!(references, Err(LspError::Unsupported { .. })));

    fake.server.save(&uri).unwrap();
    common::log_line(&mut fake, &format!("saved {}", uri.as_str())).await;
    fake.server.close(&uri).unwrap();
    common::log_line(&mut fake, &format!("closed {}", uri.as_str())).await;
}

#[tokio::test]
async fn utf16_positions_convert_for_emoji_and_cjk() {
    // No `positionEncoding` in the answer: UTF-16, the protocol's default.
    let mut fake = common::start(&json!({
        "capabilities": {"textDocumentSync": 2, "hoverProvider": true},
        "diagnostics": true,
        "hover": "echo",
    }));
    common::ready(&fake).await;
    assert_eq!(fake.server.encoding(), Some(PositionEncoding::Utf16));

    let uri = fake.uri("emoji.ts");
    let mut buffer = Buffer::from_content("const s = \"😀 漢字\"; // error 🎉 error\n");
    fake.server
        .open(uri.clone(), "typescript", 0, buffer.text().clone())
        .unwrap();
    diagnostics_at(&mut fake, &uri, 0).await;
    assert_eq!(diagnosed(&fake, &uri, buffer.text()), ["error", "error"]);

    // An incremental change after astral and CJK characters, on the same line.
    let at = buffer.text().to_string().find("🎉").unwrap();
    let applied = buffer
        .apply(Transaction::replace(at..at + "🎉".len(), "字😀字").unwrap())
        .unwrap();
    fake.server
        .change(
            &uri,
            1,
            buffer.text().clone(),
            &applied.transaction.changes(),
        )
        .unwrap();
    let state = mirror(&fake, &uri).await;
    assert_eq!(state["text"], buffer.text().to_string());
    assert_eq!(state["full_changes"], 0);
    diagnostics_at(&mut fake, &uri, 1).await;
    assert_eq!(diagnosed(&fake, &uri, buffer.text()), ["error", "error"]);
}

/// Applies 100 single-character edits in one tick (no await in between).
fn burst(fake: &Fake, uri: &Uri, buffer: &mut Buffer) -> i32 {
    let mut version = 0;
    for i in 0..100 {
        let text = buffer.text();
        let at = text.floor_char_boundary(text.len() / 2);
        let tx = if i % 4 == 3 {
            let before = text.char_before(at).map_or(0, char::len_utf8);
            Transaction::delete(at - before..at).unwrap()
        } else {
            Transaction::insert(at, if i % 2 == 0 { "é" } else { "x" })
        };
        let applied = buffer.apply(tx).unwrap();
        version = applied.version as i32;
        fake.server
            .change(
                uri,
                version,
                buffer.text().clone(),
                &applied.transaction.changes(),
            )
            .unwrap();
    }
    version
}

#[tokio::test]
async fn a_burst_of_changes_in_one_tick_sends_one_did_change() {
    let mut fake = common::start(&json!({
        "capabilities": {"positionEncoding": "utf-8", "textDocumentSync": 2, "hoverProvider": true},
        "diagnostics": true,
        "hover": "echo",
    }));
    common::ready(&fake).await;
    let uri = fake.uri("burst.rs");
    let mut buffer = Buffer::from_content("fn main() {\n    let error = 1;\n}\n");
    fake.server
        .open(uri.clone(), "rust", 0, buffer.text().clone())
        .unwrap();
    diagnostics_at(&mut fake, &uri, 0).await;

    let version = burst(&fake, &uri, &mut buffer);
    // No request: the coalescing window sends it.
    diagnostics_at(&mut fake, &uri, version).await;
    let state = mirror(&fake, &uri).await;
    assert_eq!(state["changes"], 1, "{state}");
    assert_eq!(state["full_changes"], 0);
    assert_eq!(state["version"], version);
    assert_eq!(state["text"], buffer.text().to_string());
}

#[tokio::test]
async fn a_full_sync_server_gets_one_full_change() {
    let fake = common::start(&json!({
        "capabilities": {"textDocumentSync": 1, "hoverProvider": true},
        "hover": "echo",
    }));
    common::ready(&fake).await;
    let uri = fake.uri("full.rs");
    let mut buffer = Buffer::from_content("é😀\n漢字\n");
    fake.server
        .open(uri.clone(), "rust", 0, buffer.text().clone())
        .unwrap();
    fake.server.flush().await;
    burst(&fake, &uri, &mut buffer);
    // The request sends the pending changes first.
    let state = mirror(&fake, &uri).await;
    assert_eq!(state["changes"], 1, "{state}");
    assert_eq!(state["full_changes"], 1);
    assert_eq!(state["text"], buffer.text().to_string());
}
