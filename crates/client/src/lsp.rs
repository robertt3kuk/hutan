//! Typed calls of the daemon's language servers (protocol 0.39, capabilities `doc.sync`,
//! `lsp`; `docs/protocol.md` § LSP, `docs/lsp.md`). The `lsp.state` and `lsp.diagnostics`
//! events arrive on [`Client::events`](Client::events).

use hutan_protocol::lsp::{
    DocChange, DocOpen, DocOpened, DocRef, LspCompletionItem, LspCompletionList,
    LspCompletionRequest, LspCompletionResolve, LspDiagnosticsRequest, LspDiagnosticsResult,
    LspHover, LspLocations, LspPositionRequest, LspServerInfo, LspServerRef,
    LspSignatureHelp, LspSignatureHelpRequest, LspStatus, LspStatusRequest, LspTrustRequest,
    LspTrustResult,
};
use hutan_protocol::messages::{Empty, Request};

use crate::client::{Client, DOC_SYNC_CAPABILITY, LSP_CAPABILITY};
use crate::error::ClientError;

/// Capability of protocol 0.41: completion and signature help.
pub const LSP_COMPLETION_CAPABILITY: &str = "lsp.completion";

impl Client {
    /// True when the daemon advertised capability `doc.sync` (protocol 0.39):
    /// `doc.open|change|save|close`.
    pub fn supports_doc_sync(&self) -> bool {
        self.has_capability(DOC_SYNC_CAPABILITY)
    }

    /// True when the daemon advertised capability `lsp` (protocol 0.39): `lsp.*`, the
    /// `lsp.state` and `lsp.diagnostics` events and the language servers in Resources.
    pub fn supports_lsp(&self) -> bool {
        self.has_capability(LSP_CAPABILITY)
    }

    /// True when the daemon advertised capability `lsp.completion` (protocol 0.41):
    /// `lsp.completion|completion_resolve|signature_help` and the servers' trigger characters.
    pub fn supports_lsp_completion(&self) -> bool {
        self.has_capability(LSP_COMPLETION_CAPABILITY)
    }

    /// `doc.open`: this connection has the document open with this text; a server starts for it
    /// in a trusted folder.
    pub async fn doc_open(&self, req: DocOpen) -> Result<DocOpened, ClientError> {
        self.call(Request::DocOpen(req)).await
    }

    /// `doc.change`: the owner's edits of an open document.
    pub async fn doc_change(&self, req: DocChange) -> Result<(), ClientError> {
        self.call::<Empty>(Request::DocChange(req))
            .await
            .map(|_| ())
    }

    /// `doc.save`: the document was written to disk.
    pub async fn doc_save(&self, doc_id: &str) -> Result<(), ClientError> {
        self.call::<Empty>(Request::DocSave(DocRef {
            doc_id: doc_id.to_owned(),
        }))
        .await
        .map(|_| ())
    }

    /// `doc.close`.
    pub async fn doc_close(&self, doc_id: &str) -> Result<(), ClientError> {
        self.call::<Empty>(Request::DocClose(DocRef {
            doc_id: doc_id.to_owned(),
        }))
        .await
        .map(|_| ())
    }

    /// `lsp.status`: the servers and the trust decisions.
    pub async fn lsp_status(&self, req: LspStatusRequest) -> Result<LspStatus, ClientError> {
        self.call(Request::LspStatus(req)).await
    }

    /// `lsp.diagnostics`: what the servers published.
    pub async fn lsp_diagnostics(
        &self,
        req: LspDiagnosticsRequest,
    ) -> Result<LspDiagnosticsResult, ClientError> {
        self.call(Request::LspDiagnostics(req)).await
    }

    /// `lsp.hover` at a position of an open document.
    pub async fn lsp_hover(&self, req: LspPositionRequest) -> Result<LspHover, ClientError> {
        self.call(Request::LspHover(req)).await
    }

    /// `lsp.definition` of the symbol at a position of an open document.
    pub async fn lsp_definition(
        &self,
        req: LspPositionRequest,
    ) -> Result<LspLocations, ClientError> {
        self.call(Request::LspDefinition(req)).await
    }

    /// `lsp.completion` at a position of an open document; a newer one of this connection
    /// supersedes it (`conflict`).
    pub async fn lsp_completion(
        &self,
        req: LspCompletionRequest,
    ) -> Result<LspCompletionList, ClientError> {
        self.call(Request::LspCompletion(req)).await
    }

    /// `lsp.completion_resolve`: more about one item of a completion list.
    pub async fn lsp_completion_resolve(
        &self,
        req: LspCompletionResolve,
    ) -> Result<LspCompletionItem, ClientError> {
        self.call(Request::LspCompletionResolve(req)).await
    }

    /// `lsp.signature_help` at a position of an open document.
    pub async fn lsp_signature_help(
        &self,
        req: LspSignatureHelpRequest,
    ) -> Result<LspSignatureHelp, ClientError> {
        self.call(Request::LspSignatureHelp(req)).await
    }

    /// `lsp.restart`: starts a server again, or looks for its binary again.
    pub async fn lsp_restart(&self, server_id: &str) -> Result<LspServerInfo, ClientError> {
        self.call(Request::LspRestart(LspServerRef {
            server_id: server_id.to_owned(),
        }))
        .await
    }

    /// `lsp.stop`.
    pub async fn lsp_stop(&self, server_id: &str) -> Result<LspServerInfo, ClientError> {
        self.call(Request::LspStop(LspServerRef {
            server_id: server_id.to_owned(),
        }))
        .await
    }

    /// `lsp.trust`: the user's decision about a folder. Refused with `forbidden` from an agent.
    pub async fn lsp_trust(&self, req: LspTrustRequest) -> Result<LspTrustResult, ClientError> {
        self.call(Request::LspTrust(req)).await
    }
}
