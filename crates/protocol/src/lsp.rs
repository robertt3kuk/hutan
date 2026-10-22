//! Language servers in the daemon (protocol 0.39, capabilities `doc.sync` and `lsp`; ADR 0012
//! amendment 2026-10-21 decisions 4–5, `docs/protocol.md` § LSP, `docs/lsp.md`).
//!
//! The daemon's `LspHub` runs one language server per (server binary, root) and shares it
//! between every client: the desktop, the CLI and agents. Clients tell the hub which documents
//! they have open and what they typed (`doc.open|change|save|close`); the hub keeps a bounded
//! text mirror per document and feeds the servers, coalescing a burst of edits into one
//! `didChange`. What the servers know comes back as `lsp.diagnostics` and answers to
//! `lsp.hover` / `lsp.definition`; completion (`lsp.completion`, `lsp.completion_resolve`)
//! and signature help (`lsp.signature_help`) since protocol 0.41 (capability
//! `lsp.completion`).
//!
//! **Positions** on this wire are LSP's `line` / `character` with `character` in UTF-16 code
//! units, whatever the server negotiated; the hub converts. Paths are absolute.
//!
//! **Trust.** A folder must be trusted before a server starts in it (`lsp.trust`, user-only):
//! a language server runs the project's build scripts and procedural macros, and rust-analyzer
//! does not sandbox them. A server whose folder is not decided yet is `untrusted` with
//! `trust_pending` set: that is the request the user answers.
//!
//! **Errors**, in short: `bad_request` (a relative path, an edit outside the text, a bound
//! exceeded), `not_found` (an unknown document or server), `invalid_state` (the document is
//! not open, the server is not running), `forbidden` (`lsp.trust` from an agent; a document
//! outside the workspace roots for an agent), `unsupported` (`data.reason`: `too_large` for a
//! document over [`DOC_OPEN_TEXT_MAX_BYTES`], `server` when the server lacks the request),
//! `provider_error` (the server failed or timed out answering), `conflict` with
//! `data.reason = "superseded"` (a newer `lsp.completion`, `lsp.completion_resolve` or
//! `lsp.signature_help` of the same connection replaced this one; the server got
//! `$/cancelRequest`).

use std::path::PathBuf;

use hutan_core::WorkspaceId;
use serde::{Deserialize, Serialize};

/// Most bytes of `text` in one `doc.open`: a control frame holds at most 4 MiB, JSON escapes
/// included. A larger file opens in the editor without a language server.
pub const DOC_OPEN_TEXT_MAX_BYTES: usize = 3 << 20;
/// Largest a mirrored document may grow to by `doc.change`.
pub const DOC_MAX_BYTES: usize = 16 << 20;
/// Documents one connection may have open.
pub const DOCS_PER_CONNECTION_MAX: usize = 128;
/// Most bytes of replacement text in one `doc.change`.
pub const DOC_CHANGE_MAX_BYTES: usize = 256 << 10;
/// Most edits in one `doc.change`.
pub const DOC_EDITS_MAX: usize = 1_000;
/// Diagnostics of one file an answer or event carries (the server's own bound is the same).
pub const DIAGNOSTICS_PER_FILE_MAX: usize = 500;
/// Diagnostics one `lsp.diagnostics` answer carries; more set `truncated`.
pub const DIAGNOSTICS_TOTAL_MAX: usize = 5_000;
/// Longest [`LspDiagnostic::message`], in bytes.
pub const DIAGNOSTIC_MESSAGE_MAX_BYTES: usize = 4 << 10;
/// Related locations kept per diagnostic.
pub const RELATED_MAX: usize = 8;
/// Longest [`LspHover::contents`], in bytes.
pub const HOVER_MAX_BYTES: usize = 64 << 10;
/// Locations one `lsp.definition` answer carries.
pub const LOCATIONS_MAX: usize = 100;
/// `lsp.diagnostics` events of one file are at most this far apart.
pub const DIAGNOSTICS_EVENT_INTERVAL_MS: u64 = 250;
/// Items one `lsp.completion` answer carries; more set `truncated`.
pub const COMPLETION_ITEMS_MAX: usize = 200;
/// Longest [`LspCompletionItem::label`], `filter_text` and `sort_text`, in bytes.
pub const COMPLETION_LABEL_MAX_BYTES: usize = 256;
/// Longest [`LspCompletionItem::detail`], in bytes.
pub const COMPLETION_DETAIL_MAX_BYTES: usize = 1 << 10;
/// Longest [`LspCompletionItem::documentation`] in a list; a resolved item may carry up to
/// [`HOVER_MAX_BYTES`].
pub const COMPLETION_DOC_MAX_BYTES: usize = 4 << 10;
/// Longest text an item inserts (`insert_text`, an edit's `new_text`); an item with a longer
/// one is left out of the answer (cutting code would insert something else).
pub const COMPLETION_TEXT_MAX_BYTES: usize = 16 << 10;
/// Additional edits of one item (an auto-import); an item with more is left out.
pub const COMPLETION_EDITS_MAX: usize = 32;
/// Signatures one `lsp.signature_help` answer carries.
pub const SIGNATURES_MAX: usize = 16;
/// Parameters kept per signature.
pub const PARAMETERS_MAX: usize = 64;
/// Trigger characters kept per server and kind.
pub const TRIGGERS_MAX: usize = 32;

/// A position in a document: zero-based line, and `character` in UTF-16 code units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

/// A range of a document; `end` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

/// Where a language server is in its life. `not_installed` and `untrusted` mean no process
/// exists: the binary was not found, or the folder is not trusted (yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspServerState {
    Starting,
    Indexing,
    Ready,
    Failed,
    Stopped,
    NotInstalled,
    Untrusted,
}

impl LspServerState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Indexing => "indexing",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::NotInstalled => "not_installed",
            Self::Untrusted => "untrusted",
        }
    }
}

/// What the user decided about a folder. As a request it also takes `ask`: forget the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspTrust {
    Trusted,
    Denied,
    Ask,
}

impl LspTrust {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Denied => "denied",
            Self::Ask => "ask",
        }
    }
}

/// One language server of the hub, as it is now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspServerInfo {
    /// Opaque and stable while the hub runs; what `lsp.restart` and `lsp.stop` take.
    pub server_id: String,
    /// The server's id in `daemon.toml` (`rust-analyzer`).
    pub name: String,
    /// Its workspace folder: the topmost root marker above the files it serves.
    pub root: PathBuf,
    /// The languages it serves.
    pub languages: Vec<String>,
    pub state: LspServerState,
    /// Why the server is in this state, when that is more than the state says: the last
    /// error, `idle`, which setting to change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The server process, while one runs: the root of the tree whose memory Resources show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Starts after a crash so far.
    pub restarts: u32,
    /// Resident memory of the server and its children, in bytes, while a process runs; filled
    /// by `lsp.status` (the `lsp.state` events leave it out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
    /// Documents open in it.
    pub documents: u32,
    /// For `not_installed`: the exact command that installs the server. Hutan never runs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<String>,
    /// For `not_installed`: the setting of `daemon.toml` that points at the binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<String>,
    /// For `untrusted`: the folder's trust is asked for and not decided yet; the user answers
    /// with `lsp.trust` (`hutan lsp trust FOLDER`). Absent when the user denied it (or
    /// `[lsp] trust = "never"`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trust_pending: bool,
    /// What the running server announced it can do: `hover`, `definition`, `references`, …
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// The characters after which the server offers completion unasked (`.`, `::`'s `:`):
    /// its `completionProvider.triggerCharacters`, while it runs (protocol 0.41).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completion_triggers: Vec<String>,
    /// The characters that open (or move) signature help (`(`, `,`): its
    /// `signatureHelpProvider.triggerCharacters` and `retriggerCharacters` (protocol 0.41).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signature_triggers: Vec<String>,
}

/// A decision about a folder, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspTrustEntry {
    pub folder: PathBuf,
    /// `trusted` or `denied`.
    pub decision: LspTrust,
    pub decided_at_ms: u64,
    /// The client that decided (`desktop`, `cli`, …).
    pub decided_by: String,
}

/// Which default decides a folder without an entry (`[lsp] trust` of `daemon.toml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspTrustPolicy {
    /// Ask the user (the default).
    Ask,
    /// Every folder is trusted.
    Always,
    /// No folder is trusted.
    Never,
}

/// `doc.open`: tells the hub the client has `path` open with this text. Answered with
/// [`DocOpened`].
///
/// A server starts on the first `doc.open` of a language in a trusted folder. Opening a path
/// another connection has open is allowed: the second connection is not the `owner`, its edits
/// are refused (`invalid_state`) and it only follows diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocOpen {
    pub path: PathBuf,
    /// The LSP language id (`rust`, `typescriptreact`); absent: from the file name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The client's version of the text; later `doc.change`s count up from it.
    pub version: i32,
    /// At most [`DOC_OPEN_TEXT_MAX_BYTES`].
    pub text: String,
}

/// One server a document is sent to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocServer {
    pub server_id: String,
    pub name: String,
    pub state: LspServerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trust_pending: bool,
}

/// Result of `doc.open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocOpened {
    pub doc_id: String,
    /// This connection's edits are the ones mirrored. `false`: another connection owns the
    /// document.
    pub owner: bool,
    /// The language the document was opened as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The servers that get the document; empty with a `reason` when none can.
    pub servers: Vec<DocServer>,
    /// Why there is no server: `lsp is disabled`, no server for the language, the project
    /// switched it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One replacement of a range of the text (UTF-16 positions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocEdit {
    pub start: LspPosition,
    pub end: LspPosition,
    pub text: String,
}

/// `doc.change`: the owner's edits, applied one after the other, each in the coordinates of the
/// text the previous one left (LSP's `contentChanges` semantics). `text` instead replaces the
/// whole document (the fallback when the client does not know what changed). Answered with
/// `Empty`; the servers get one `didChange` per coalescing window (50 ms) or before the next
/// request, whichever comes first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocChange {
    pub doc_id: String,
    pub version: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<DocEdit>,
    /// The whole new text, instead of `edits`; at most [`DOC_OPEN_TEXT_MAX_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// `doc.save` and `doc.close`: the document was written to disk / is closed. Answered with
/// `Empty`. A connection's documents are closed when it goes away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocRef {
    pub doc_id: String,
}

/// `lsp.status`: the servers, the folder decisions and what waits for the user. Answered with
/// [`LspStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LspStatusRequest {
    /// Only servers whose root is inside one of this workspace's folders (or contains one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// Result of `lsp.status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspStatus {
    /// `[lsp] enabled`.
    pub enabled: bool,
    /// `[lsp] trust`.
    pub trust_policy: LspTrustPolicy,
    /// `[lsp] max_servers`.
    pub max_servers: u32,
    pub servers: Vec<LspServerInfo>,
    /// The decisions of the user, folders sorted.
    pub trust: Vec<LspTrustEntry>,
}

/// `lsp.trust`: records what the user decides about `folder` (and everything below it that has
/// no decision of its own). `ask` forgets the decision. A server that runs in a folder that
/// becomes `denied` is stopped. Answered with [`LspTrustResult`]. **User-only**: an
/// agent-originated connection gets `forbidden`, whatever `[control]` says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspTrustRequest {
    pub folder: PathBuf,
    pub decision: LspTrust,
}

/// Result of `lsp.trust`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LspTrustResult {
    pub folder: PathBuf,
    pub decision: LspTrust,
    /// The servers of the hub afterwards that live in the folder (started by this decision,
    /// stopped by it, or waiting).
    pub servers: Vec<LspServerInfo>,
}

/// `lsp.restart` and `lsp.stop`: one server by its `server_id`. Restarting also retries a server
/// that is `failed`, `stopped` or `not_installed` (the binary is looked for again); an
/// `untrusted` one is refused with `forbidden`. Answered with [`LspServerInfo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspServerRef {
    pub server_id: String,
}

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

impl LspSeverity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Information => "information",
            Self::Hint => "hint",
        }
    }
}

/// A place a diagnostic points at besides its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspRelated {
    pub path: PathBuf,
    pub range: LspRange,
    pub message: String,
}

/// One problem a server found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspDiagnostic {
    pub range: LspRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<LspSeverity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// At most [`DIAGNOSTIC_MESSAGE_MAX_BYTES`].
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<LspRelated>,
}

/// The diagnostics of one file. The set replaces the previous one (`publishDiagnostics`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspFileDiagnostics {
    pub path: PathBuf,
    /// The document version the server saw, when it said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
    /// The server that published them.
    pub server_id: String,
    pub diagnostics: Vec<LspDiagnostic>,
    /// The server sent more than [`DIAGNOSTICS_PER_FILE_MAX`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// `lsp.diagnostics`: what the servers published, of one file (`path`), of the servers that
/// serve a workspace (`workspace_id`), or of every server. Answered with [`LspDiagnosticsResult`].
/// Files without diagnostics are not listed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LspDiagnosticsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// Only diagnostics at least this serious (`error` is the most).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_severity: Option<LspSeverity>,
}

/// A server a diagnostics answer depends on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspServerBrief {
    pub server_id: String,
    pub name: String,
    pub state: LspServerState,
}

/// Result of `lsp.diagnostics`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspDiagnosticsResult {
    /// Files sorted by path.
    pub files: Vec<LspFileDiagnostics>,
    /// More than [`DIAGNOSTICS_TOTAL_MAX`] diagnostics (or files the server left out).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Some server this answer comes from is not `ready` (still starting or indexing, or not
    /// running): the answer may be out of date or incomplete.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    /// The servers consulted.
    pub servers: Vec<LspServerBrief>,
}

/// `lsp.hover` and `lsp.definition`: what is at `position` of a document some connection has
/// open (`invalid_state` when none does). Answered with [`LspHover`] / [`LspLocations`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspPositionRequest {
    pub path: PathBuf,
    pub position: LspPosition,
}

/// Result of `lsp.hover`: the text the server shows (Markdown or plain text, parts joined by
/// a blank line), at most [`HOVER_MAX_BYTES`]; none where the server has nothing to say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspHover {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<LspRange>,
}

/// A place in a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspLocation {
    pub path: PathBuf,
    pub range: LspRange,
}

/// Result of `lsp.definition`: at most [`LOCATIONS_MAX`] places in files (other schemes are
/// left out).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspLocations {
    pub locations: Vec<LspLocation>,
}

/// What a completion item is, as the popup draws it (LSP's `CompletionItemKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspCompletionKind {
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Property,
    Unit,
    Value,
    Enum,
    Keyword,
    Snippet,
    Color,
    File,
    Reference,
    Folder,
    EnumMember,
    Constant,
    Struct,
    Event,
    Operator,
    TypeParameter,
    /// A kind of a newer peer.
    #[serde(other)]
    Other,
}

/// `lsp.completion`: what may be typed at `position` of an open document. Answered with
/// [`LspCompletionList`]. A newer `lsp.completion` of the same connection cancels this one
/// (`conflict`, `data.reason = "superseded"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspCompletionRequest {
    pub path: PathBuf,
    pub position: LspPosition,
    /// The character just typed that asked for it (one of the server's
    /// [`LspServerInfo::completion_triggers`]); absent when the user asked (⌃Space) or typed a
    /// word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Asked again while typing because the previous answer was `incomplete`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incomplete: bool,
}

/// One replacement of a range of the document (UTF-16 positions of the text the request was
/// answered for).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspTextEdit {
    pub range: LspRange,
    pub new_text: String,
}

/// One completion item, bounded ([`COMPLETION_LABEL_MAX_BYTES`] …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspCompletionItem {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<LspCompletionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Markdown or plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    /// The server's order (absent: the label).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_text: Option<String>,
    /// What typing is matched against (absent: the label).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_text: Option<String>,
    /// What accepting inserts when there is no `edit` (absent: the label); it replaces the
    /// word before the caret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert_text: Option<String>,
    /// The text inserted is a snippet (`$1`, `${1:name}`, `$0`, …).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub snippet: bool,
    /// The replacement accepting makes (its range ends at the request's position).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit: Option<LspTextEdit>,
    /// Edits elsewhere that come with it (an import), at most [`COMPLETION_EDITS_MAX`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_edits: Vec<LspTextEdit>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preselect: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
    /// The server tells more on `lsp.completion_resolve` (documentation, detail, the edits of
    /// an import).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resolvable: bool,
}

/// Result of `lsp.completion`: at most [`COMPLETION_ITEMS_MAX`] items, in the server's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspCompletionList {
    /// Names this answer for `lsp.completion_resolve`; the daemon keeps the last few lists of
    /// each connection.
    pub completion_id: u64,
    pub items: Vec<LspCompletionItem>,
    /// The server will answer differently as more is typed: ask again instead of filtering.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incomplete: bool,
    /// Items were left out (more than [`COMPLETION_ITEMS_MAX`], or one too large).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// `lsp.completion_resolve`: the item `index` of the list `completion_id`, with what the
/// server adds (`completionItem/resolve`). Answered with [`LspCompletionItem`]; `not_found`
/// when the list is no longer kept. A newer resolve of the same connection cancels this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspCompletionResolve {
    pub completion_id: u64,
    pub index: u32,
}

/// `lsp.signature_help`: the call around `position`. Answered with [`LspSignatureHelp`]; a
/// newer one of the same connection cancels this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspSignatureHelpRequest {
    pub path: PathBuf,
    pub position: LspPosition,
    /// The character just typed that asked for it (`(`, `,`); absent when the user asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Signature help shows already (LSP's `isRetrigger`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retrigger: bool,
}

/// One parameter of a signature: where it is in the signature's label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspParameter {
    /// Byte offsets into [`LspSignature::label`] (`start..end`; empty when the server named a
    /// parameter that is not in the label).
    pub start: u32,
    pub end: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
}

/// One signature of a call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspSignature {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<LspParameter>,
    /// The parameter being typed, an index into `parameters`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_parameter: Option<u32>,
}

/// Result of `lsp.signature_help`: no signatures when the position is in no call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspSignatureHelp {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<LspSignature>,
    /// The signature to show, an index into `signatures`.
    #[serde(default)]
    pub active_signature: u32,
}
