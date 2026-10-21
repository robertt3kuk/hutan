//! The requests the client makes, typed with `ls-types`: hover, definition, references,
//! completion, document symbols, formatting, rename (prepare and apply) and code actions.
//!
//! Each one knows the capability it needs (the client refuses it without sending when the
//! server did not announce it) and whether it is latency-critical (completion gets the short
//! timeout). Results are returned as the server sent them; nothing here applies an edit.

use ls_types::request::{
    CodeActionRequest, Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest,
    PrepareRenameRequest, References, Rename, Request,
};
use ls_types::{CodeActionProviderCapability, HoverProviderCapability, OneOf, ServerCapabilities};

/// Which timeout applies ([`Timeouts`](crate::Timeouts)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Latency {
    /// Typing waits for it (completion).
    Interactive,
    Normal,
}

/// A request the client may send.
pub trait TypedRequest: Request {
    const LATENCY: Latency = Latency::Normal;
    /// Whether the server announced what this request needs.
    fn supported(capabilities: &ServerCapabilities) -> bool;
}

fn enabled<T>(capability: Option<&OneOf<bool, T>>) -> bool {
    matches!(capability, Some(OneOf::Left(true) | OneOf::Right(_)))
}

impl TypedRequest for HoverRequest {
    fn supported(capabilities: &ServerCapabilities) -> bool {
        matches!(
            capabilities.hover_provider,
            Some(HoverProviderCapability::Simple(true) | HoverProviderCapability::Options(_))
        )
    }
}

impl TypedRequest for GotoDefinition {
    fn supported(capabilities: &ServerCapabilities) -> bool {
        enabled(capabilities.definition_provider.as_ref())
    }
}

impl TypedRequest for References {
    fn supported(capabilities: &ServerCapabilities) -> bool {
        enabled(capabilities.references_provider.as_ref())
    }
}

impl TypedRequest for Completion {
    const LATENCY: Latency = Latency::Interactive;
    fn supported(capabilities: &ServerCapabilities) -> bool {
        capabilities.completion_provider.is_some()
    }
}

impl TypedRequest for DocumentSymbolRequest {
    fn supported(capabilities: &ServerCapabilities) -> bool {
        enabled(capabilities.document_symbol_provider.as_ref())
    }
}

impl TypedRequest for Formatting {
    fn supported(capabilities: &ServerCapabilities) -> bool {
        enabled(capabilities.document_formatting_provider.as_ref())
    }
}

impl TypedRequest for PrepareRenameRequest {
    fn supported(capabilities: &ServerCapabilities) -> bool {
        matches!(
            &capabilities.rename_provider,
            Some(OneOf::Right(options)) if options.prepare_provider == Some(true)
        )
    }
}

impl TypedRequest for Rename {
    fn supported(capabilities: &ServerCapabilities) -> bool {
        enabled(capabilities.rename_provider.as_ref())
    }
}

impl TypedRequest for CodeActionRequest {
    fn supported(capabilities: &ServerCapabilities) -> bool {
        matches!(
            capabilities.code_action_provider,
            Some(
                CodeActionProviderCapability::Simple(true)
                    | CodeActionProviderCapability::Options(_)
            )
        )
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    use super::*;

    /// `params` and `result` (wire JSON as servers send it) parse into the request's types and
    /// serialize back to the same JSON.
    fn round_trip<R: TypedRequest>(params: Value, result: Value)
    where
        R::Params: Serialize + DeserializeOwned,
        R::Result: Serialize + DeserializeOwned,
    {
        let typed: R::Params = serde_json::from_value(params.clone())
            .unwrap_or_else(|e| panic!("{} params: {e}", R::METHOD));
        assert_eq!(
            serde_json::to_value(&typed).unwrap(),
            params,
            "{}",
            R::METHOD
        );
        let typed: R::Result = serde_json::from_value(result.clone())
            .unwrap_or_else(|e| panic!("{} result: {e}", R::METHOD));
        assert_eq!(
            serde_json::to_value(&typed).unwrap(),
            result,
            "{}",
            R::METHOD
        );
    }

    fn at() -> Value {
        json!({"textDocument": {"uri": "file:///w/src/main.rs"}, "position": {"line": 3, "character": 8}})
    }

    fn range(l0: u32, c0: u32, l1: u32, c1: u32) -> Value {
        json!({"start": {"line": l0, "character": c0}, "end": {"line": l1, "character": c1}})
    }

    #[test]
    fn the_request_subset_round_trips() {
        round_trip::<HoverRequest>(
            at(),
            json!({"contents": {"kind": "markdown", "value": "```rust\nfn main()\n```"}, "range": range(3, 4, 3, 8)}),
        );
        round_trip::<GotoDefinition>(
            at(),
            json!([{"targetUri": "file:///w/src/lib.rs", "targetRange": range(0, 0, 9, 1), "targetSelectionRange": range(0, 7, 0, 10), "originSelectionRange": range(3, 4, 3, 8)}]),
        );
        round_trip::<GotoDefinition>(
            at(),
            json!({"uri": "file:///w/src/lib.rs", "range": range(1, 0, 1, 3)}),
        );
        let mut references = at();
        references["context"] = json!({"includeDeclaration": true});
        round_trip::<References>(
            references,
            json!([{"uri": "file:///w/src/a.rs", "range": range(1, 2, 1, 5)}]),
        );
        let mut completion = at();
        completion["context"] = json!({"triggerKind": 2, "triggerCharacter": "."});
        round_trip::<Completion>(
            completion,
            json!({"isIncomplete": true, "items": [{"label": "len", "kind": 2, "detail": "fn(&self) -> usize", "insertTextFormat": 1, "textEdit": {"range": range(3, 8, 3, 8), "newText": "len()"}}]}),
        );
        round_trip::<DocumentSymbolRequest>(
            json!({"textDocument": {"uri": "file:///w/src/main.rs"}}),
            json!([{"name": "main", "kind": 12, "range": range(0, 0, 5, 1), "selectionRange": range(0, 3, 0, 7), "children": []}]),
        );
        round_trip::<Formatting>(
            json!({"textDocument": {"uri": "file:///w/src/main.rs"}, "options": {"tabSize": 4, "insertSpaces": true}}),
            json!([{"range": range(0, 0, 0, 3), "newText": "fn "}]),
        );
        round_trip::<PrepareRenameRequest>(
            at(),
            json!({"range": range(3, 4, 3, 8), "placeholder": "main"}),
        );
        let mut rename = at();
        rename["newName"] = json!("start");
        round_trip::<Rename>(
            rename,
            json!({"changes": {"file:///w/src/main.rs": [{"range": range(3, 4, 3, 8), "newText": "start"}]}}),
        );
        round_trip::<CodeActionRequest>(
            json!({"textDocument": {"uri": "file:///w/src/main.rs"}, "range": range(3, 0, 3, 9), "context": {"diagnostics": [{"range": range(3, 4, 3, 8), "severity": 1, "message": "unused"}], "only": ["quickfix"]}}),
            json!([{"title": "Remove", "kind": "quickfix", "edit": {"changes": {"file:///w/src/main.rs": [{"range": range(3, 0, 4, 0), "newText": ""}]}}}, {"title": "Run", "command": "ra.run", "arguments": [1]}]),
        );
    }

    #[test]
    fn capabilities_gate_the_requests() {
        let caps: ServerCapabilities = serde_json::from_value(json!({
            "hoverProvider": true,
            "definitionProvider": {"workDoneProgress": false},
            "referencesProvider": false,
            "renameProvider": {"prepareProvider": true},
            "codeActionProvider": {"codeActionKinds": ["quickfix"]}
        }))
        .unwrap();
        assert!(HoverRequest::supported(&caps));
        assert!(GotoDefinition::supported(&caps));
        assert!(!References::supported(&caps));
        assert!(!Completion::supported(&caps));
        assert!(!DocumentSymbolRequest::supported(&caps));
        assert!(!Formatting::supported(&caps));
        assert!(PrepareRenameRequest::supported(&caps));
        assert!(Rename::supported(&caps));
        assert!(CodeActionRequest::supported(&caps));
        let plain: ServerCapabilities =
            serde_json::from_value(json!({"renameProvider": true})).unwrap();
        assert!(Rename::supported(&plain));
        assert!(!PrepareRenameRequest::supported(&plain));
    }
}
