//! Language servers for file tabs (slice E-12a; protocol 0.39 `doc.*` / `lsp.*`,
//! `docs/lsp.md`, `docs/editor.md` § Language servers).
//!
//! [`LspStore`] is the window's side of the daemon's `LspHub`: one per workbench, an entity the
//! file tabs observe.
//!
//! - **Document sync.** A file tab's buffer is opened as a daemon document (`doc.open`) once its
//!   text is in, but only while the daemon offers `doc.sync`. A file whose language has no
//!   server is answered with no servers: it is closed again at once and no file of that
//!   language (by extension) is offered again on this connection, so a window without language
//!   servers sends nothing after the first try. Edits are gathered for
//!   [`CHANGE_DEBOUNCE`] and sent as one `doc.change` replacing the one span they touched
//!   (the whole text when that is simpler), in order, one call at a time; a save is
//!   `doc.save` after them, a closed buffer `doc.close`. The daemon closes a connection's
//!   documents when it goes away, so after a reconnect every document is opened again.
//! - **Diagnostics** arrive as `lsp.diagnostics` notices for the documents of this connection
//!   and replace the buffer's set ([`FileBuffer::set_diagnostics`]); the set already published
//!   is read once after `doc.open` (`lsp.diagnostics {path}`).
//! - **Servers.** `lsp.state` notices and `lsp.status` keep what the file tabs show: the trust
//!   bar for a folder that waits for the user (*Trust* is `lsp.trust`, user-only), the install
//!   command of a missing server, and the state the status item shows.
//! - **Requests.** Hover and go to definition wait for the edits on their way (`doc.change`),
//!   then ask `lsp.hover` / `lsp.definition`.
//! - **Jumps.** Where go to definition jumped from, for *Go Back* (`⌃O`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, EventEmitter, Task, WeakEntity};
use hutan_buffer::{Change, Text, Utf16Point};
use hutan_client::{ClientError, Connection, Notice};
use hutan_code_view::{BufferEdited, CompletionTriggers, DiagnosticInput, FileBuffer, Severity};
use hutan_protocol::lsp::{
    DOC_CHANGE_MAX_BYTES, DOC_OPEN_TEXT_MAX_BYTES, DocChange, DocEdit, DocOpen, DocOpened,
    DocServer, LspCompletionItem, LspCompletionList, LspCompletionRequest, LspCompletionResolve,
    LspDiagnostic, LspDiagnosticsRequest, LspHover, LspLocations, LspPosition, LspPositionRequest,
    LspServerInfo, LspServerState, LspSeverity, LspSignatureHelp, LspSignatureHelpRequest,
    LspStatusRequest, LspTrust, LspTrustRequest,
};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::oneshot;

/// A burst of edits goes to the daemon as one `doc.change` this long after the last of them.
pub const CHANGE_DEBOUNCE: Duration = Duration::from_millis(50);
/// Entries kept by *Go Back*.
const MAX_JUMPS: usize = 100;
/// Requests queued for one document before its edits are sent as the whole text instead.
const MAX_OUTBOX: usize = 16;

/// What edits since the last `doc.change` touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dirty {
    Clean,
    /// One span: `start..sent_end` of the text the daemon has became `start..end` of the
    /// buffer's.
    Range {
        start: usize,
        sent_end: usize,
        end: usize,
    },
    /// Send the whole text.
    Full,
}

/// What goes to the daemon for a document, in order.
#[derive(Debug)]
enum Outgoing {
    Change(DocChange),
    Save,
}

/// A document as the daemon knows it.
#[derive(Debug, Clone)]
struct OpenDoc {
    doc_id: String,
    /// The connection it was opened on ([`Connection::generation`]).
    generation: u64,
    /// This connection's edits are mirrored (another connection may own the path).
    owner: bool,
    servers: Vec<DocServer>,
}

struct Doc {
    buffer: WeakEntity<FileBuffer>,
    /// `None` while `doc.open` is on its way (or failed).
    open: Option<OpenDoc>,
    opening: Option<Task<()>>,
    /// The text the daemon's mirror has once what was sent lands.
    sent: Text,
    version: i32,
    dirty: Dirty,
    flush: Option<Task<()>>,
    outbox: VecDeque<Outgoing>,
    /// A call of the outbox is on its way.
    sending: Option<Task<()>>,
    /// Requests that wait for the outbox to drain.
    waiters: Vec<oneshot::Sender<()>>,
    /// Bumped by every diagnostics notice: an older `lsp.diagnostics` answer is dropped.
    diagnostics_seq: u64,
    seed: Option<Task<()>>,
}

/// A server of a document, as the file tab shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerView {
    pub server_id: String,
    pub name: String,
    pub state: LspServerState,
    pub reason: Option<String>,
    pub install: Option<String>,
    pub setting: Option<String>,
    pub trust_pending: bool,
    /// Its folder (known once `lsp.status` or an `lsp.state` named it).
    pub root: Option<PathBuf>,
}

/// The bar over a file tab about its language server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspBar {
    /// The folder is not trusted yet: *Trust* / *Not now*. Trust is per folder: every server
    /// waiting below it starts once it is trusted, so the bar names them all.
    Trust {
        folder: PathBuf,
        /// The languages of the servers that wait for the folder, each once, the file's own
        /// first ([`language_label_of`]).
        languages: Vec<String>,
        /// How many servers wait for it.
        servers: usize,
    },
    /// The server is not installed: the command that installs it (never run by Hutan).
    Install {
        server_id: String,
        name: String,
        command: String,
        setting: Option<String>,
    },
}

/// What the store tells the workbench.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspStoreEvent {
    /// Servers, documents or diagnostics changed (the status item, the bars).
    Changed,
    /// A line for the notice area (a refused trust, a failed request).
    Notice(String),
}

/// A place go to definition left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jump {
    pub path: String,
    pub position: LspPosition,
}

/// The window's documents and servers (see the module docs).
pub struct LspStore {
    connection: Connection,
    docs: HashMap<String, Doc>,
    servers: HashMap<String, LspServerInfo>,
    /// Languages (by file extension) the daemon has no server for.
    no_server: HashSet<String>,
    /// Install bars the user closed (the install hint's close, its *Copy Command*).
    dismissed: HashSet<String>,
    /// Folders whose trust bar the user closed (*Not now*): no bar asks again for a server
    /// below them in this window.
    not_now: HashSet<PathBuf>,
    /// Folders whose `lsp.trust` is on its way.
    trusting: HashSet<PathBuf>,
    jumps: Vec<Jump>,
    listener: Option<Task<()>>,
    status: Option<Task<()>>,
    /// `doc.change` calls sent (tests: debouncing).
    changes_sent: u64,
    /// Completion and signature help requests asked so far: one that waited for the edits
    /// goes only if no newer one was asked meanwhile.
    completion_asked: Arc<AtomicU64>,
    signature_asked: Arc<AtomicU64>,
    /// Diagnostics notices applied (tests).
    diagnostics_seen: u64,
}

impl EventEmitter<LspStoreEvent> for LspStore {}

impl std::fmt::Debug for LspStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspStore")
            .field("docs", &self.docs.keys().collect::<Vec<_>>())
            .field("servers", &self.servers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// The language key of `path` for "no server for it": its extension, else its name.
fn language_key(path: &str) -> String {
    let path = Path::new(path);
    path.extension()
        .or_else(|| path.file_name())
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// The LSP position of byte `offset` of `text` (UTF-16 columns).
pub fn position_of(text: &Text, offset: usize) -> LspPosition {
    let offset = text.floor_char_boundary(offset.min(text.len()));
    let point = text.byte_to_utf16_point(offset).unwrap_or(Utf16Point {
        line: 0,
        character: 0,
    });
    LspPosition {
        line: u32::try_from(point.line).unwrap_or(u32::MAX),
        character: u32::try_from(point.character).unwrap_or(u32::MAX),
    }
}

/// A protocol severity as the editor's (an unspecified one counts as an error, as LSP clients
/// commonly treat it).
pub fn severity_of(severity: Option<LspSeverity>) -> Severity {
    match severity {
        Some(LspSeverity::Error) | None => Severity::Error,
        Some(LspSeverity::Warning) => Severity::Warning,
        Some(LspSeverity::Information) => Severity::Info,
        Some(LspSeverity::Hint) => Severity::Hint,
    }
}

/// The name of LSP language `id` the way the editor names languages (`Rust`, `TSX`); an id the
/// editor has no language for reads `Plain Text`, as the tab of such a file does.
pub fn language_label_of(id: &str) -> &'static str {
    match id {
        "rust" => "Rust",
        "typescript" => "TypeScript",
        "typescriptreact" => "TSX",
        "javascript" | "javascriptreact" => "JavaScript",
        "python" => "Python",
        "go" | "gomod" => "Go",
        "json" | "jsonc" => "JSON",
        "toml" => "TOML",
        "yaml" => "YAML",
        "markdown" => "Markdown",
        "shellscript" => "Shell",
        "html" => "HTML",
        "css" => "CSS",
        "c" => "C",
        "cpp" => "C++",
        "objective-c" => "Objective-C",
        "objective-cpp" => "Objective-C++",
        _ => "Plain Text",
    }
}

/// The languages a server serves, by name, each once, in its order (`C, C++`).
pub fn languages_of(languages: &[String]) -> Vec<&'static str> {
    let mut labels: Vec<&'static str> = Vec::new();
    for id in languages {
        let label = language_label_of(id);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}

/// `A`, `A and B`, `A, B and C`.
pub fn spoken_list<S: AsRef<str>>(items: &[S]) -> String {
    match items {
        [] => String::new(),
        [one] => one.as_ref().to_owned(),
        [rest @ .., last] => format!(
            "{} and {}",
            rest.iter().map(AsRef::as_ref).collect::<Vec<_>>().join(", "),
            last.as_ref()
        ),
    }
}

fn inputs_of(diagnostics: Vec<LspDiagnostic>) -> Vec<DiagnosticInput> {
    let point = |p: LspPosition| Utf16Point {
        line: p.line as usize,
        character: p.character as usize,
    };
    diagnostics
        .into_iter()
        .map(|d| DiagnosticInput {
            start: point(d.range.start),
            end: point(d.range.end),
            severity: severity_of(d.severity),
            message: d.message,
            source: d.source,
            code: d.code,
        })
        .collect()
}

/// The span `dirty` grows to with one more transaction (`changes`, old ranges in the text
/// before it, which was `before` bytes long; `after` bytes after it).
fn merge(dirty: Dirty, changes: &[Change], before: usize, after: usize) -> Dirty {
    let (Some(first), Some(last)) = (changes.first(), changes.last()) else {
        return dirty;
    };
    let (s, e, new_e) = (first.old.start, last.old.end, last.new.end);
    let consistent = s <= e
        && e <= before
        && first.new.start == s
        && new_e >= s
        && before - (e - s) + (new_e - s) == after;
    match dirty {
        _ if !consistent => Dirty::Full,
        Dirty::Full => Dirty::Full,
        Dirty::Clean => Dirty::Range {
            start: s,
            sent_end: e,
            end: new_e,
        },
        Dirty::Range {
            start,
            sent_end,
            end,
        } => {
            // `end` is in the coordinates of the text before this transaction.
            let hi = end.max(e);
            Dirty::Range {
                start: start.min(s),
                sent_end: sent_end + (hi - end),
                end: hi - e + new_e,
            }
        }
    }
}

/// The `doc.change` that takes the daemon's `sent` to `current`.
fn change_of(doc_id: &str, version: i32, sent: &Text, current: &Text, dirty: Dirty) -> DocChange {
    let whole = || DocChange {
        doc_id: doc_id.to_owned(),
        version,
        edits: Vec::new(),
        text: Some(current.to_string()),
    };
    let Dirty::Range {
        start,
        sent_end,
        end,
    } = dirty
    else {
        return whole();
    };
    let edit = (|| {
        let text = current.slice(start..end).ok()?;
        if text.len() > DOC_CHANGE_MAX_BYTES {
            return None;
        }
        let from = sent.byte_to_utf16_point(start).ok()?;
        let to = sent.byte_to_utf16_point(sent_end).ok()?;
        let position = |p: Utf16Point| LspPosition {
            line: u32::try_from(p.line).unwrap_or(u32::MAX),
            character: u32::try_from(p.character).unwrap_or(u32::MAX),
        };
        Some(DocEdit {
            start: position(from),
            end: position(to),
            text,
        })
    })();
    match edit {
        Some(edit) => DocChange {
            doc_id: doc_id.to_owned(),
            version,
            edits: vec![edit],
            text: None,
        },
        None => whole(),
    }
}

impl LspStore {
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            docs: HashMap::new(),
            servers: HashMap::new(),
            no_server: HashSet::new(),
            dismissed: HashSet::new(),
            not_now: HashSet::new(),
            trusting: HashSet::new(),
            jumps: Vec::new(),
            listener: None,
            status: None,
            changes_sent: 0,
            completion_asked: Arc::new(AtomicU64::new(0)),
            signature_asked: Arc::new(AtomicU64::new(0)),
            diagnostics_seen: 0,
        }
    }

    // ---- documents ---------------------------------------------------------------------------

    /// The buffer of `path` is in: open it as a daemon document (once) when the daemon syncs
    /// documents and its language may have a server.
    pub fn open(&mut self, path: &str, buffer: &Entity<FileBuffer>, cx: &mut Context<Self>) {
        if self.docs.contains_key(path) || !self.connection.supports_doc_sync() {
            return;
        }
        let text = {
            let b = buffer.read(cx);
            if !b.is_ready() || b.is_large() {
                return;
            }
            b.text().clone()
        };
        if text.len() > DOC_OPEN_TEXT_MAX_BYTES || self.no_server.contains(&language_key(path)) {
            return;
        }
        self.listen(cx);
        self.docs.insert(
            path.to_owned(),
            Doc {
                buffer: buffer.downgrade(),
                open: None,
                opening: None,
                sent: text,
                version: 1,
                dirty: Dirty::Clean,
                flush: None,
                outbox: VecDeque::new(),
                sending: None,
                waiters: Vec::new(),
                diagnostics_seq: 0,
                seed: None,
            },
        );
        self.send_open(path, cx);
    }

    /// (Re)opens `path` with the buffer's text now.
    fn send_open(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        let Some(buffer) = doc.buffer.upgrade() else {
            return;
        };
        let text = buffer.read(cx).text().clone();
        doc.sent = text.clone();
        doc.version = 1;
        doc.dirty = Dirty::Clean;
        doc.open = None;
        doc.flush = None;
        doc.outbox.clear();
        doc.sending = None;
        let connection = self.connection.clone();
        let request = DocOpen {
            path: PathBuf::from(path),
            language: None,
            version: 1,
            text: text.to_string(),
        };
        let call = cx.background_spawn(async move { connection.doc_open(request).await });
        let key = path.to_owned();
        doc.opening = Some(cx.spawn(async move |this, cx| {
            let opened = call.await;
            this.update(cx, |this, cx| this.opened(&key, opened, cx))
                .ok();
        }));
    }

    fn opened(
        &mut self,
        path: &str,
        result: Result<(u64, DocOpened), ClientError>,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.docs.get_mut(path) else {
            // The tab closed meanwhile: the document goes too.
            if let Ok((_, opened)) = result {
                self.close_on_daemon(opened.doc_id, cx);
            }
            return;
        };
        doc.opening = None;
        match result {
            Ok((_, opened)) if opened.servers.is_empty() => {
                // No server for this language: nothing more is sent for it.
                tracing::debug!(path, reason = ?opened.reason, "no language server");
                self.no_server.insert(language_key(path));
                self.docs.remove(path);
                self.close_on_daemon(opened.doc_id, cx);
            }
            Ok((generation, opened)) => {
                let waits = opened.servers.iter().any(|s| {
                    matches!(
                        s.state,
                        LspServerState::Untrusted | LspServerState::NotInstalled
                    ) || !self.servers.contains_key(&s.server_id)
                });
                doc.open = Some(OpenDoc {
                    doc_id: opened.doc_id,
                    generation,
                    owner: opened.owner,
                    servers: opened.servers,
                });
                if doc.dirty != Dirty::Clean {
                    self.schedule_flush(path, Duration::ZERO, cx);
                }
                if waits {
                    self.refresh_status(cx);
                }
                self.seed_diagnostics(path, cx);
                // Requests that waited for the document go now.
                self.pump(path, cx);
                cx.emit(LspStoreEvent::Changed);
                cx.notify();
            }
            Err(error) => {
                // Left without a document; a reconnect or the next load tries again.
                tracing::debug!(path, %error, "doc.open failed");
                self.docs.remove(path);
            }
        }
    }

    fn close_on_daemon(&self, doc_id: String, cx: &mut Context<Self>) {
        let connection = self.connection.clone();
        cx.background_spawn(async move {
            let _ = connection.doc_close(&doc_id).await;
        })
        .detach();
    }

    /// The buffer of `path` changed: the edit joins the span to send, after a pause.
    pub fn edited(&mut self, path: &str, event: &BufferEdited, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        let Some(buffer) = doc.buffer.upgrade() else {
            return;
        };
        let after = buffer.read(cx).text().len();
        let delta = event.transaction.len_delta();
        let before = usize::try_from(after as isize - delta).unwrap_or(0);
        doc.dirty = merge(doc.dirty, &event.transaction.changes(), before, after);
        if doc.open.as_ref().is_some_and(|o| o.owner) {
            self.schedule_flush(path, CHANGE_DEBOUNCE, cx);
        }
    }

    fn schedule_flush(&mut self, path: &str, after: Duration, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        let key = path.to_owned();
        doc.flush = Some(cx.spawn(async move |this, cx| {
            if !after.is_zero() {
                cx.background_executor().timer(after).await;
            }
            this.update(cx, |this, cx| this.flush(&key, cx)).ok();
        }));
    }

    /// Queues the pending edits of `path` as one `doc.change` and sends what is queued.
    fn flush(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        doc.flush = None;
        let Some(open) = doc.open.as_ref() else {
            return;
        };
        // A follower's edits are not mirrored; its requests still wait for the queue.
        if open.owner && doc.dirty != Dirty::Clean {
            let Some(buffer) = doc.buffer.upgrade() else {
                return;
            };
            let current = buffer.read(cx).text().clone();
            if current.len() > DOC_OPEN_TEXT_MAX_BYTES {
                // Too large to send whole: the document goes; reopened if it shrinks on load.
                let doc_id = open.doc_id.clone();
                self.docs.remove(path);
                self.close_on_daemon(doc_id, cx);
                return;
            }
            doc.version += 1;
            let dirty = if doc.outbox.len() >= MAX_OUTBOX {
                Dirty::Full
            } else {
                doc.dirty
            };
            let change = change_of(&open.doc_id, doc.version, &doc.sent, &current, dirty);
            doc.sent = current;
            doc.dirty = Dirty::Clean;
            doc.outbox.push_back(Outgoing::Change(change));
        }
        self.pump(path, cx);
    }

    /// Sends the next queued call of `path` when none is on its way; wakes the requests that
    /// waited once the queue is empty.
    fn pump(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        if doc.sending.is_some() {
            return;
        }
        let Some(open) = doc.open.clone() else {
            return;
        };
        let Some(next) = doc.outbox.pop_front() else {
            for waiter in doc.waiters.drain(..) {
                let _ = waiter.send(());
            }
            return;
        };
        let connection = self.connection.clone();
        let is_change = matches!(next, Outgoing::Change(_));
        if is_change {
            self.changes_sent += 1;
        }
        let call = cx.background_spawn(async move {
            match next {
                Outgoing::Change(change) => connection.doc_change(change).await,
                Outgoing::Save => connection.doc_save(&open.doc_id).await,
            }
        });
        let key = path.to_owned();
        doc.sending = Some(cx.spawn(async move |this, cx| {
            let result = call.await;
            this.update(cx, |this, cx| this.sent(&key, is_change, result, cx))
                .ok();
        }));
    }

    fn sent(
        &mut self,
        path: &str,
        is_change: bool,
        result: Result<(), ClientError>,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        doc.sending = None;
        match result {
            Ok(()) => self.pump(path, cx),
            // Gone with the connection: the reconnect opens it again.
            Err(ClientError::Disconnected) => {}
            Err(error) if is_change => {
                // The daemon's mirror and ours disagree: start over from the whole text.
                tracing::debug!(path, %error, "doc.change refused; opening again");
                self.send_open(path, cx);
            }
            Err(error) => {
                tracing::debug!(path, %error, "doc.save failed");
                self.pump(path, cx);
            }
        }
    }

    /// The file of `path` was written: the edits first, then `doc.save`.
    pub fn saved(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        if doc.open.as_ref().is_none_or(|o| !o.owner) {
            return;
        }
        doc.flush = None;
        self.flush(path, cx);
        if let Some(doc) = self.docs.get_mut(path) {
            doc.outbox.push_back(Outgoing::Save);
        }
        self.pump(path, cx);
    }

    /// The last tab of `path` closed: the document is closed.
    pub fn close(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(mut doc) = self.docs.remove(path) else {
            return;
        };
        if let Some(open) = doc.open.take() {
            self.close_on_daemon(open.doc_id, cx);
        } else if let Some(opening) = doc.opening.take() {
            // Its answer closes the document (see `opened`).
            opening.detach();
        }
        cx.emit(LspStoreEvent::Changed);
    }

    /// The connection was made again (or may have been): documents of an older connection are
    /// opened again.
    fn reconnected(&mut self, cx: &mut Context<Self>) {
        let generation = self.connection.generation();
        let stale: Vec<String> = self
            .docs
            .iter()
            .filter(|(_, d)| {
                d.opening.is_none() && d.open.as_ref().is_none_or(|o| o.generation != generation)
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in stale {
            self.send_open(&path, cx);
        }
    }

    // ---- notices -----------------------------------------------------------------------------

    fn listen(&mut self, cx: &mut Context<Self>) {
        if self.listener.is_some() {
            return;
        }
        let mut notices = self.connection.notices();
        self.listener = Some(cx.spawn(async move |this, cx| {
            loop {
                let notice = match notices.recv().await {
                    Ok(notice) => Some(notice),
                    Err(RecvError::Lagged(_)) => None,
                    Err(RecvError::Closed) => break,
                };
                let alive = this.update(cx, |this, cx| match notice {
                    Some(Notice::LspState { server }) => this.server_changed(*server, cx),
                    Some(Notice::LspDiagnostics {
                        path, diagnostics, ..
                    }) => this.diagnostics_arrived(&path, diagnostics, cx),
                    // After a reconnect the connection announces an overflow of its watches.
                    Some(Notice::FsChanged { overflow: true, .. })
                    | Some(Notice::DaemonRestarted { .. }) => this.reconnected(cx),
                    Some(_) => {}
                    // Missed some: read again what the daemon knows.
                    None => this.resync(cx),
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    fn resync(&mut self, cx: &mut Context<Self>) {
        self.reconnected(cx);
        self.refresh_status(cx);
        let paths: Vec<String> = self.docs.keys().cloned().collect();
        for path in paths {
            self.seed_diagnostics(&path, cx);
        }
    }

    fn server_changed(&mut self, server: LspServerInfo, cx: &mut Context<Self>) {
        self.servers.insert(server.server_id.clone(), server);
        cx.emit(LspStoreEvent::Changed);
        cx.notify();
    }

    fn diagnostics_arrived(
        &mut self,
        path: &Path,
        diagnostics: Vec<LspDiagnostic>,
        cx: &mut Context<Self>,
    ) {
        let key = path.display().to_string();
        let Some(doc) = self.docs.get_mut(&key) else {
            return;
        };
        doc.diagnostics_seq += 1;
        doc.seed = None;
        let Some(buffer) = doc.buffer.upgrade() else {
            return;
        };
        self.diagnostics_seen += 1;
        buffer.update(cx, |b, cx| b.set_diagnostics(inputs_of(diagnostics), cx));
        cx.emit(LspStoreEvent::Changed);
        cx.notify();
    }

    /// Reads what the servers already published for `path` (once, after `doc.open`).
    fn seed_diagnostics(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get_mut(path) else {
            return;
        };
        let seq = doc.diagnostics_seq;
        let connection = self.connection.clone();
        let request = LspDiagnosticsRequest {
            path: Some(PathBuf::from(path)),
            ..LspDiagnosticsRequest::default()
        };
        let call = cx.background_spawn(async move { connection.lsp_diagnostics(request).await });
        let key = path.to_owned();
        doc.seed = Some(cx.spawn(async move |this, cx| {
            let Ok(answer) = call.await else {
                return;
            };
            this.update(cx, |this, cx| {
                let Some(doc) = this.docs.get_mut(&key) else {
                    return;
                };
                if doc.diagnostics_seq != seq {
                    // A notice was newer.
                    return;
                }
                doc.seed = None;
                let diagnostics: Vec<LspDiagnostic> = answer
                    .files
                    .into_iter()
                    .filter(|f| f.path.display().to_string() == key)
                    .flat_map(|f| f.diagnostics)
                    .collect();
                if let Some(buffer) = doc.buffer.upgrade() {
                    buffer.update(cx, |b, cx| b.set_diagnostics(inputs_of(diagnostics), cx));
                }
                cx.emit(LspStoreEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Reads every server's state (`lsp.status`): roots, reasons, install commands.
    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        if !self.connection.supports_lsp() {
            return;
        }
        let connection = self.connection.clone();
        let call =
            cx.background_spawn(
                async move { connection.lsp_status(LspStatusRequest::default()).await },
            );
        self.status = Some(cx.spawn(async move |this, cx| {
            let Ok(status) = call.await else {
                return;
            };
            this.update(cx, |this, cx| {
                this.status = None;
                for server in status.servers {
                    this.servers.insert(server.server_id.clone(), server);
                }
                cx.emit(LspStoreEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
    }

    // ---- what the tabs show ------------------------------------------------------------------

    /// Whether `path` is a daemon document now (tests).
    pub fn is_open(&self, path: &str) -> bool {
        self.docs.get(path).is_some_and(|d| d.open.is_some())
    }

    /// Whether `path` has a document, open or on its way.
    pub fn has_doc(&self, path: &str) -> bool {
        self.docs.contains_key(path)
    }

    /// `doc.change` calls sent so far (tests).
    pub fn changes_sent(&self) -> u64 {
        self.changes_sent
    }

    /// Diagnostics notices applied so far (tests).
    pub fn diagnostics_seen(&self) -> u64 {
        self.diagnostics_seen
    }

    /// Whether nothing of `path` waits to be sent (tests).
    pub fn is_synced(&self, path: &str) -> bool {
        self.docs.get(path).is_some_and(|d| {
            d.open.is_some()
                && d.dirty == Dirty::Clean
                && d.flush.is_none()
                && d.outbox.is_empty()
                && d.sending.is_none()
        })
    }

    /// The servers of `path`'s document, as known now.
    pub fn servers_of(&self, path: &str) -> Vec<ServerView> {
        let Some(open) = self.docs.get(path).and_then(|d| d.open.as_ref()) else {
            return Vec::new();
        };
        open.servers
            .iter()
            .map(|s| match self.servers.get(&s.server_id) {
                Some(info) => ServerView {
                    server_id: info.server_id.clone(),
                    name: info.name.clone(),
                    state: info.state,
                    reason: info.reason.clone(),
                    install: info.install.clone(),
                    setting: info.setting.clone(),
                    trust_pending: info.trust_pending,
                    root: Some(info.root.clone()),
                },
                None => ServerView {
                    server_id: s.server_id.clone(),
                    name: s.name.clone(),
                    state: s.state,
                    reason: s.reason.clone(),
                    install: s.install.clone(),
                    setting: s.setting.clone(),
                    trust_pending: s.trust_pending,
                    root: None,
                },
            })
            .collect()
    }

    /// The bar the file tab of `path` shows, if any: a folder to trust (until the user says
    /// *Not now* to it or a folder above it), or a server to install (until the user closes it).
    pub fn bar(&self, path: &str) -> Option<LspBar> {
        self.servers_of(path).into_iter().find_map(|s| match s.state {
            LspServerState::Untrusted if s.trust_pending => {
                let folder = s.root?;
                if self.not_now.iter().any(|closed| folder.starts_with(closed)) {
                    return None;
                }
                Some(self.trust_bar(folder, &s.server_id))
            }
            LspServerState::NotInstalled if !self.dismissed.contains(&s.server_id) => {
                Some(LspBar::Install {
                    command: s.install.clone()?,
                    server_id: s.server_id,
                    name: s.name,
                    setting: s.setting,
                })
            }
            _ => None,
        })
    }

    /// The trust bar of `folder`: every server known to wait for it (its root at or below the
    /// folder), `first` (the file's own) leading.
    fn trust_bar(&self, folder: PathBuf, first: &str) -> LspBar {
        let mut waiting: Vec<&LspServerInfo> = self
            .servers
            .values()
            .filter(|s| {
                s.state == LspServerState::Untrusted
                    && s.trust_pending
                    && s.root.starts_with(&folder)
            })
            .collect();
        waiting.sort_by(|a, b| {
            (a.server_id != first, &a.name, &a.server_id).cmp(&(
                b.server_id != first,
                &b.name,
                &b.server_id,
            ))
        });
        let mut languages: Vec<String> = Vec::new();
        for server in &waiting {
            for label in languages_of(&server.languages) {
                if !languages.iter().any(|l| l == label) {
                    languages.push(label.to_owned());
                }
            }
        }
        LspBar::Trust {
            folder,
            languages,
            servers: waiting.len().max(1),
        }
    }

    /// Whether the trust of `folder` is on its way.
    pub fn is_trusting(&self, folder: &Path) -> bool {
        self.trusting.contains(folder)
    }

    /// The server the status item shows for `path`: the first one of its document.
    pub fn server_of(&self, path: &str) -> Option<ServerView> {
        self.servers_of(path).into_iter().next()
    }

    // ---- what the user answers ---------------------------------------------------------------

    /// *Trust*: `lsp.trust {folder, trusted}`; the daemon starts the servers that waited.
    pub fn trust(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        if !self.trusting.insert(folder.clone()) {
            return;
        }
        let connection = self.connection.clone();
        let request = LspTrustRequest {
            folder: folder.clone(),
            decision: LspTrust::Trusted,
        };
        let call = cx.background_spawn(async move { connection.lsp_trust(request).await });
        cx.spawn(async move |this, cx| {
            let result = call.await;
            this.update(cx, |this, cx| {
                this.trusting.remove(&folder);
                match result {
                    Ok(answer) => {
                        for server in answer.servers {
                            this.servers.insert(server.server_id.clone(), server);
                        }
                    }
                    Err(error) => cx.emit(LspStoreEvent::Notice(format!(
                        "Could not trust {}: {}",
                        folder.display(),
                        super::files::error_text(&error)
                    ))),
                }
                cx.emit(LspStoreEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The install hint's close: the install bar of `server_id` stays closed in this window.
    pub fn dismiss(&mut self, server_id: &str, cx: &mut Context<Self>) {
        self.dismissed.insert(server_id.to_owned());
        cx.emit(LspStoreEvent::Changed);
        cx.notify();
    }

    /// *Not now* on a trust bar: nothing is decided, and no bar asks about `folder` (or a
    /// folder below it) again in this window, whatever language the next file has.
    pub fn not_now(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.not_now.insert(folder);
        cx.emit(LspStoreEvent::Changed);
        cx.notify();
    }

    // ---- requests ----------------------------------------------------------------------------

    /// A future that resolves once the edits of `path` on their way have reached the daemon.
    fn after_edits(&mut self, path: &str, cx: &mut Context<Self>) -> Option<oneshot::Receiver<()>> {
        let doc = self.docs.get_mut(path)?;
        doc.open.as_ref()?;
        let (sender, receiver) = oneshot::channel();
        doc.waiters.push(sender);
        doc.flush = None;
        self.flush(path, cx);
        Some(receiver)
    }

    /// `lsp.hover` at `position` of `path`, after its pending edits; `None` when the file has
    /// no document.
    pub fn hover(
        &mut self,
        path: &str,
        position: LspPosition,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<LspHover, ClientError>>> {
        let ready = self.after_edits(path, cx)?;
        let connection = self.connection.clone();
        let request = LspPositionRequest {
            path: PathBuf::from(path),
            position,
        };
        Some(cx.background_spawn(async move {
            let _ = ready.await;
            connection.lsp_hover(request).await
        }))
    }

    /// `lsp.definition` at `position` of `path`, after its pending edits.
    pub fn definition(
        &mut self,
        path: &str,
        position: LspPosition,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<LspLocations, ClientError>>> {
        let ready = self.after_edits(path, cx)?;
        let connection = self.connection.clone();
        let request = LspPositionRequest {
            path: PathBuf::from(path),
            position,
        };
        Some(cx.background_spawn(async move {
            let _ = ready.await;
            connection.lsp_definition(request).await
        }))
    }

    /// What makes the code view of `path` ask for completion and signature help: on while
    /// the daemon has `lsp.completion` and a server of the document announced completion; the
    /// servers' single-character triggers.
    pub fn completion_triggers(&self, path: &str) -> CompletionTriggers {
        let Some(open) = self.docs.get(path).and_then(|d| d.open.as_ref()) else {
            return CompletionTriggers::default();
        };
        if !self.connection.supports_lsp_completion() {
            return CompletionTriggers::default();
        }
        let servers: Vec<&LspServerInfo> = open
            .servers
            .iter()
            .filter_map(|s| self.servers.get(&s.server_id))
            .collect();
        let chars = |of: fn(&LspServerInfo) -> &Vec<String>| -> Vec<char> {
            let mut out: Vec<char> = servers
                .iter()
                .flat_map(|s| of(s).iter())
                .filter_map(|t| {
                    let mut chars = t.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => Some(c),
                        _ => None,
                    }
                })
                .collect();
            out.sort_unstable();
            out.dedup();
            out
        };
        CompletionTriggers {
            enabled: servers
                .iter()
                .any(|s| s.capabilities.iter().any(|c| c == "completion")),
            completion: chars(|s| &s.completion_triggers),
            signature: chars(|s| &s.signature_triggers),
        }
    }

    /// `lsp.completion` at `position` of `path`, after its pending edits. `Ok(None)`: a newer
    /// one was asked while this one waited for the edits, so it was not sent (requests that
    /// wait together must not reach the daemon out of order: a newer one supersedes the older
    /// there).
    pub fn completion(
        &mut self,
        path: &str,
        position: LspPosition,
        trigger: Option<char>,
        incomplete: bool,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Option<LspCompletionList>, ClientError>>> {
        let ready = self.after_edits(path, cx)?;
        let connection = self.connection.clone();
        let request = LspCompletionRequest {
            path: PathBuf::from(path),
            position,
            trigger: trigger.map(String::from),
            incomplete,
        };
        let latest = self.completion_asked.clone();
        let mine = latest.fetch_add(1, Ordering::SeqCst) + 1;
        Some(cx.background_spawn(async move {
            let _ = ready.await;
            if latest.load(Ordering::SeqCst) != mine {
                return Ok(None);
            }
            connection.lsp_completion(request).await.map(Some)
        }))
    }

    /// `lsp.completion_resolve` of item `index` of list `completion_id`.
    pub fn completion_resolve(
        &self,
        completion_id: u64,
        index: u32,
        cx: &mut Context<Self>,
    ) -> Task<Result<LspCompletionItem, ClientError>> {
        let connection = self.connection.clone();
        cx.background_spawn(async move {
            connection
                .lsp_completion_resolve(LspCompletionResolve {
                    completion_id,
                    index,
                })
                .await
        })
    }

    /// `lsp.signature_help` at `position` of `path`, after its pending edits; `Ok(None)` as
    /// for [`completion`](Self::completion).
    pub fn signature_help(
        &mut self,
        path: &str,
        position: LspPosition,
        trigger: Option<char>,
        retrigger: bool,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Option<LspSignatureHelp>, ClientError>>> {
        let ready = self.after_edits(path, cx)?;
        let connection = self.connection.clone();
        let request = LspSignatureHelpRequest {
            path: PathBuf::from(path),
            position,
            trigger: trigger.map(String::from),
            retrigger,
        };
        let latest = self.signature_asked.clone();
        let mine = latest.fetch_add(1, Ordering::SeqCst) + 1;
        Some(cx.background_spawn(async move {
            let _ = ready.await;
            if latest.load(Ordering::SeqCst) != mine {
                return Ok(None);
            }
            connection.lsp_signature_help(request).await.map(Some)
        }))
    }

    // ---- jumps -------------------------------------------------------------------------------

    /// Go to definition is leaving `jump`.
    pub fn push_jump(&mut self, jump: Jump) {
        if self.jumps.last() == Some(&jump) {
            return;
        }
        self.jumps.push(jump);
        if self.jumps.len() > MAX_JUMPS {
            self.jumps.remove(0);
        }
    }

    /// *Go Back*: the place the last jump left.
    pub fn pop_jump(&mut self) -> Option<Jump> {
        self.jumps.pop()
    }

    /// The places *Go Back* returns to, oldest first (tests).
    pub fn jumps(&self) -> &[Jump] {
        &self.jumps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_buffer::Transaction;

    fn apply(text: &Text, tx: &Transaction) -> Text {
        Text::from(tx.apply_to_string(&text.to_string()).unwrap().as_str())
    }

    /// Edits merged into one span, sent as one `doc.change`, give the daemon the buffer's text.
    #[test]
    fn a_burst_of_edits_is_one_span_that_rebuilds_the_text() {
        let sent = Text::from("fn main() {\n    let é = 1;\n}\n");
        let mut current = sent.clone();
        let mut dirty = Dirty::Clean;
        for tx in [
            Transaction::insert(16, "x"),
            Transaction::delete(3..7).unwrap(),
            Transaction::insert(0, "pub "),
            Transaction::replace(4..6, "😀").unwrap(),
        ] {
            let before = current.len();
            current = apply(&current, &tx);
            dirty = merge(dirty, &tx.changes(), before, current.len());
        }
        let change = change_of("d", 2, &sent, &current, dirty);
        assert_eq!(change.edits.len(), 1, "{change:?}");
        // Apply the edit with UTF-16 positions to the sent text.
        let edit = &change.edits[0];
        let start = sent.utf16_point_to_byte(Utf16Point {
            line: edit.start.line as usize,
            character: edit.start.character as usize,
        });
        let end = sent.utf16_point_to_byte(Utf16Point {
            line: edit.end.line as usize,
            character: edit.end.character as usize,
        });
        let mut rebuilt = sent.to_string();
        rebuilt.replace_range(start.unwrap()..end.unwrap(), &edit.text);
        assert_eq!(rebuilt, current.to_string());
        // Nothing to say about a clean document: the whole text is the fallback.
        assert_eq!(
            change_of("d", 3, &current, &current, Dirty::Full).text,
            Some(current.to_string())
        );
    }

    #[test]
    fn languages_are_keyed_by_extension_and_severities_map() {
        assert_eq!(language_key("/a/b/Main.RS"), "rs");
        assert_eq!(language_key("/a/Makefile"), "makefile");
        assert_eq!(severity_of(None), Severity::Error);
        assert_eq!(severity_of(Some(LspSeverity::Hint)), Severity::Hint);
        let text = Text::from("é😀x\n");
        assert_eq!(
            position_of(&text, 6),
            LspPosition {
                line: 0,
                character: 3
            }
        );
    }
}
