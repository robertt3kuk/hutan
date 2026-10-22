//! `LspHub`: the daemon's language servers (protocol 0.39, `docs/lsp.md`).
//!
//! One server per (server, root), shared by every client — the desktop, the CLI, agents. A
//! **slot** is one such pair: it exists from the first `doc.open` that needs it and says why it
//! has no process when it has none (`untrusted`, `not_installed`, `stopped`). Documents are
//! mirrored per path (bounded: [`DOCS_PER_CONNECTION_MAX`] per connection,
//! [`DOC_MAX_BYTES`] per document, [`MAX_MIRROR_BYTES`] in all) and fed to the slot's server
//! through `hutan-lsp-client`, which coalesces a burst of edits into one `didChange`.
//!
//! Nothing here is unbounded: every channel is bounded (the server's event queue is
//! [`hutan_lsp_client::EVENT_QUEUE`]; diagnostics events to clients are dropped for a full
//! connection queue and re-asked with `lsp.diagnostics`), slots are capped at [`MAX_SLOTS`] and
//! servers at `[lsp] max_servers`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use hutan_buffer::{Buffer, Change, HistoryLimits, Text, Utf16Point};
use hutan_core::WorkspaceId;
use hutan_lsp_client::ls_types::request::{
    Completion, GotoDefinition, HoverRequest, ResolveCompletionItem, SignatureHelpRequest,
};
use hutan_lsp_client::ls_types::{
    CompletionContext, CompletionItem, CompletionParams, CompletionResponse, CompletionTriggerKind,
    GotoDefinitionParams, GotoDefinitionResponse, HoverParams, PartialResultParams,
    SignatureHelpContext, SignatureHelpParams, SignatureHelpTriggerKind, TextDocumentIdentifier,
    TextDocumentPositionParams, Uri, WorkDoneProgressParams,
};
use hutan_lsp_client::{
    FileDiagnostics, Launch, LspError, PositionEncoding, RestartPolicy, Server, ServerConfig,
    ServerEvent, ServerState, Subscription, Timeouts,
};
use hutan_protocol::control::{ControlMessage, ErrorBody, ErrorCode};
use hutan_protocol::lsp::{
    COMPLETION_DOC_MAX_BYTES, COMPLETION_ITEMS_MAX, DIAGNOSTICS_EVENT_INTERVAL_MS,
    DIAGNOSTICS_PER_FILE_MAX, DIAGNOSTICS_TOTAL_MAX, DOC_CHANGE_MAX_BYTES, DOC_EDITS_MAX,
    DOC_MAX_BYTES, DOC_OPEN_TEXT_MAX_BYTES, DOCS_PER_CONNECTION_MAX, DocChange, DocOpen, DocOpened,
    DocServer, HOVER_MAX_BYTES, LOCATIONS_MAX, LspCompletionItem, LspCompletionList,
    LspCompletionRequest, LspCompletionResolve, LspDiagnostic, LspDiagnosticsRequest,
    LspDiagnosticsResult, LspFileDiagnostics, LspHover, LspLocation, LspLocations,
    LspPositionRequest, LspServerBrief, LspServerInfo, LspServerState, LspSignatureHelp,
    LspSignatureHelpRequest, LspStatus, LspStatusRequest, LspTrust, LspTrustEntry, LspTrustPolicy,
    LspTrustRequest, LspTrustResult, TRIGGERS_MAX,
};
use hutan_protocol::messages::Event;
use hutan_protocol::{Frame, MAX_PAYLOAD};
use hutan_storage::{LspTrustRow, Storage};
use parking_lot::Mutex;
use tokio::sync::OnceCell;
use tokio::time::{interval, sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::catalog::{Catalog, ServerSpec, language_of};
use super::convert::{self, Texts};
use super::roots;
use super::rustup;
use super::trust::{Standing, TrustBook};
use crate::bus::EventBus;
use crate::config::{LspSettings, LspTrustMode};
use crate::conn::{ConnHandle, ConnId, LocalSend};
use crate::harness::{Harnesses, find_in, find_program, program_dirs, with_program_dir};
use crate::registry::{RegistryHandle, now_ms};
use crate::resources::{ProcessSource, ServiceTarget, ServiceTargets};

/// Slots (server, root) the hub remembers at most; an idle one without documents makes room.
pub const MAX_SLOTS: usize = 64;
/// Text the mirrors hold in all, in bytes.
pub const MAX_MIRROR_BYTES: usize = 256 << 20;
/// Files read from disk to convert one answer's UTF-8 columns.
const DISK_READS_MAX: usize = 512;
/// Largest file read for that.
const DISK_READ_MAX_BYTES: u64 = 4 << 20;
/// How long shutdown waits for the servers to leave before their groups are killed.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);
/// How long a request at a position waits for a server that is still starting.
const START_WAIT: Duration = Duration::from_secs(10);
/// How often idle servers are looked for when no tuning says otherwise.
const IDLE_TICK: Duration = Duration::from_secs(5);
/// Completion lists kept for `lsp.completion_resolve` (the newest ones, of every connection).
const COMPLETION_LISTS_KEPT: usize = 16;

/// What tests change; the defaults are what runs.
#[derive(Debug, Clone, Default)]
pub struct LspTuning {
    /// Replaces every idle-stop setting (`[lsp] idle_stop_secs` and the servers' own).
    pub idle_stop: Option<Duration>,
    /// How long document changes are collected before one `didChange` (default 50 ms).
    pub coalesce: Option<Duration>,
    /// Replaces the restart policy, the servers' `max_failures` included.
    pub restart: Option<RestartPolicy>,
    pub timeouts: Option<Timeouts>,
}

#[derive(Debug, Clone)]
struct Down {
    state: LspServerState,
    reason: String,
    pending_trust: bool,
}

#[derive(Debug)]
struct Slot {
    id: String,
    spec: Arc<ServerSpec>,
    root: PathBuf,
    server: Option<Arc<Server>>,
    /// Why there is no server (`None` while there is one).
    down: Option<Down>,
    /// Documents (paths) served.
    docs: BTreeSet<PathBuf>,
    last_used: Instant,
    /// Since when no document is open in a running server.
    idle_since: Option<Instant>,
    /// Bumped at each start; events of an older server are ignored.
    generation: u64,
    /// A start is in progress.
    starting: bool,
    pending_diagnostics: HashSet<PathBuf>,
    diagnostics_scheduled: bool,
}

#[derive(Debug)]
struct Mirror {
    buffer: Buffer,
    language: Option<String>,
    version: i32,
    uri: Uri,
    slot: Option<String>,
}

#[derive(Debug)]
struct DocHandle {
    path: PathBuf,
    conn: ConnId,
    owner: bool,
}

#[derive(Debug)]
struct ConnDocs {
    handle: ConnHandle,
    docs: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct State {
    slots: BTreeMap<String, Slot>,
    mirrors: HashMap<PathBuf, Mirror>,
    docs: HashMap<String, DocHandle>,
    conns: HashMap<ConnId, ConnDocs>,
    trust: TrustBook,
    next_doc: u64,
    mirror_bytes: usize,
    stopping: bool,
}

/// What a `doc.open` found out about where the document goes.
enum Plan {
    /// No server will get it.
    None(String),
    Server {
        spec: Arc<ServerSpec>,
        root: PathBuf,
        program: Result<PathBuf, String>,
    },
}

/// The workspace a path belongs to.
#[derive(Debug, Clone)]
struct Project {
    id: String,
    name: String,
    root: PathBuf,
}

/// What the hub is built from.
#[derive(Debug)]
pub struct Inputs {
    pub settings: LspSettings,
    pub registry: RegistryHandle,
    pub bus: Arc<EventBus>,
    pub storage: Storage,
    pub source: Arc<dyn ProcessSource>,
    pub harnesses: Arc<Harnesses>,
    pub tuning: LspTuning,
    pub shutdown: CancellationToken,
}

#[derive(Debug)]
pub struct LspHub {
    settings: LspSettings,
    catalog: Catalog,
    registry: RegistryHandle,
    bus: Arc<EventBus>,
    storage: Storage,
    source: Arc<dyn ProcessSource>,
    harnesses: Arc<Harnesses>,
    environment: OnceCell<Arc<BTreeMap<String, String>>>,
    tuning: LspTuning,
    state: Mutex<State>,
    shutdown: CancellationToken,
    /// The completion and signature help requests waiting, one per connection and kind.
    inflight: Mutex<Inflight>,
    /// The newest completion lists, as the servers sent them, for `lsp.completion_resolve`.
    completions: Mutex<CompletionLists>,
    me: Weak<LspHub>,
}

/// The requests a newer one of the same connection and kind supersedes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Superseded {
    Completion,
    Resolve,
    SignatureHelp,
}

#[derive(Debug, Default)]
struct Inflight {
    next: u64,
    running: HashMap<(ConnId, Superseded), (u64, CancellationToken)>,
}

/// A completion list as the server sent it (only the items that went on the wire).
#[derive(Debug)]
struct KeptList {
    id: u64,
    path: PathBuf,
    items: Vec<CompletionItem>,
}

#[derive(Debug, Default)]
struct CompletionLists {
    next: u64,
    lists: std::collections::VecDeque<KeptList>,
}

fn bad(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::BadRequest, message)
}

fn invalid_state(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidState, message)
}

fn not_found(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::NotFound, message)
}

fn internal(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, message)
}

fn unsupported(message: impl Into<String>, reason: &str) -> ErrorBody {
    let mut error = ErrorBody::new(ErrorCode::Unsupported, message);
    error.data = Some(serde_json::json!({ "reason": reason }));
    error
}

/// A failed request to a server as an error of the wire.
fn request_error(error: LspError) -> ErrorBody {
    let message = error.to_string();
    match error {
        LspError::NotRunning { .. } | LspError::Document(_) | LspError::Limit(_) => {
            invalid_state(message)
        }
        LspError::Unsupported { .. } => unsupported(message, "server"),
        LspError::Position(_) => bad(message),
        LspError::Spawn { .. } => internal(message),
        _ => ErrorBody::new(ErrorCode::ProviderError, message),
    }
}

/// `path` made canonical when it exists, else its folder canonical with the file name below it.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

fn check_absolute(path: &Path) -> Result<(), ErrorBody> {
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err(bad(format!(
            "`{}` is not an absolute path without `..`",
            path.display()
        )));
    }
    Ok(())
}

async fn canonical(path: &Path) -> Result<PathBuf, ErrorBody> {
    check_absolute(path)?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || canonicalize_lenient(&path))
        .await
        .map_err(|e| internal(format!("looking up the path failed: {e}")))
}

fn server_id(name: &str, root: &Path) -> String {
    format!("{name}:{}", root.display())
}

fn state_of(state: ServerState) -> LspServerState {
    match state {
        ServerState::Starting => LspServerState::Starting,
        ServerState::Indexing => LspServerState::Indexing,
        ServerState::Ready => LspServerState::Ready,
        ServerState::Failed => LspServerState::Failed,
        ServerState::Stopped => LspServerState::Stopped,
    }
}

/// The trigger characters of a server's completion and signature help, bounded.
fn triggers_of(server: &Server) -> (Vec<String>, Vec<String>) {
    let Some(caps) = server.capabilities() else {
        return (Vec::new(), Vec::new());
    };
    let keep = |chars: Vec<String>| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in chars {
            if !c.is_empty() && c.len() <= 8 && !out.contains(&c) && out.len() < TRIGGERS_MAX {
                out.push(c);
            }
        }
        out
    };
    let completion = caps
        .completion_provider
        .as_ref()
        .and_then(|c| c.trigger_characters.clone())
        .unwrap_or_default();
    let signature = caps
        .signature_help_provider
        .as_ref()
        .map(|s| {
            let mut all = s.trigger_characters.clone().unwrap_or_default();
            all.extend(s.retrigger_characters.clone().unwrap_or_default());
            all
        })
        .unwrap_or_default();
    (keep(completion), keep(signature))
}

fn capabilities_of(server: &Server) -> Vec<String> {
    use hutan_lsp_client::ls_types::{HoverProviderCapability, OneOf};
    let Some(caps) = server.capabilities() else {
        return Vec::new();
    };
    fn on<T>(one: Option<&OneOf<bool, T>>) -> bool {
        matches!(one, Some(OneOf::Left(true) | OneOf::Right(_)))
    }
    let mut out = Vec::new();
    let mut add = |name: &str, yes: bool| {
        if yes {
            out.push(name.to_owned());
        }
    };
    add(
        "hover",
        matches!(
            caps.hover_provider,
            Some(HoverProviderCapability::Simple(true) | HoverProviderCapability::Options(_))
        ),
    );
    add("definition", on(caps.definition_provider.as_ref()));
    add("references", on(caps.references_provider.as_ref()));
    add("completion", caps.completion_provider.is_some());
    add("signature_help", caps.signature_help_provider.is_some());
    add(
        "document_symbols",
        on(caps.document_symbol_provider.as_ref()),
    );
    add("formatting", on(caps.document_formatting_provider.as_ref()));
    add("rename", caps.rename_provider.is_some());
    add("code_actions", caps.code_action_provider.is_some());
    out
}

fn is_inside(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

impl Slot {
    fn info(&self) -> LspServerInfo {
        let mut info = LspServerInfo {
            server_id: self.id.clone(),
            name: self.spec.name.clone(),
            root: self.root.clone(),
            languages: self.spec.languages.clone(),
            state: LspServerState::Stopped,
            reason: None,
            pid: None,
            restarts: 0,
            rss_bytes: None,
            documents: u32::try_from(self.docs.len()).unwrap_or(u32::MAX),
            install: None,
            setting: None,
            trust_pending: false,
            capabilities: Vec::new(),
            completion_triggers: Vec::new(),
            signature_triggers: Vec::new(),
        };
        match (&self.server, &self.down) {
            (Some(server), _) => {
                let status = server.status();
                info.state = state_of(status.state);
                info.pid = status.pid;
                info.restarts = status.restarts;
                info.reason = status.last_error.as_deref().map(summarize);
                info.capabilities = capabilities_of(server);
                (info.completion_triggers, info.signature_triggers) = triggers_of(server);
            }
            (None, Some(down)) => {
                info.state = down.state;
                info.reason = Some(down.reason.clone());
                info.trust_pending = down.pending_trust;
                if down.state == LspServerState::NotInstalled {
                    info.install = self.spec.install.clone();
                    info.setting = Some(self.spec.command_setting());
                }
            }
            (None, None) => {}
        }
        info
    }
}

/// A server's last error as one line: what a crashed server wrote to stderr runs over many lines
/// (a rustup proxy repeats itself), and the last one is what says why.
fn summarize(error: &str) -> String {
    let last = error
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    convert::clip(last, 300)
}

fn doc_server(info: &LspServerInfo) -> DocServer {
    DocServer {
        server_id: info.server_id.clone(),
        name: info.name.clone(),
        state: info.state,
        reason: info.reason.clone(),
        install: info.install.clone(),
        setting: info.setting.clone(),
        trust_pending: info.trust_pending,
    }
}

impl LspHub {
    pub fn new(inputs: Inputs) -> Result<Arc<Self>, String> {
        let Inputs {
            settings,
            registry,
            bus,
            storage,
            source,
            harnesses,
            tuning,
            shutdown,
        } = inputs;
        let catalog = Catalog::new(&settings)?;
        let trust = match storage.lsp_trust().list() {
            Ok(rows) => TrustBook::new(rows.into_iter().map(|row| LspTrustEntry {
                folder: row.folder,
                decision: if row.trusted {
                    LspTrust::Trusted
                } else {
                    LspTrust::Denied
                },
                decided_at_ms: row.decided_at_ms,
                decided_by: row.decided_by,
            })),
            Err(error) => {
                warn!(%error, "could not read the language server trust decisions; every folder is asked about again");
                TrustBook::default()
            }
        };
        let hub = Arc::new_cyclic(|me| Self {
            settings,
            catalog,
            registry,
            bus,
            storage,
            source,
            harnesses,
            environment: OnceCell::new(),
            tuning,
            state: Mutex::new(State {
                trust,
                ..State::default()
            }),
            shutdown,
            inflight: Mutex::new(Inflight::default()),
            completions: Mutex::new(CompletionLists::default()),
            me: me.clone(),
        });
        if hub.settings.enabled {
            tokio::spawn(hub.clone().idle_loop());
        }
        Ok(hub)
    }

    // Environment and binaries -----------------------------------------------------------------

    /// The login environment servers start from, captured once.
    async fn environment(&self) -> Arc<BTreeMap<String, String>> {
        self.environment
            .get_or_init(|| async {
                let harnesses = self.harnesses.clone();
                Arc::new(
                    tokio::task::spawn_blocking(move || harnesses.environment())
                        .await
                        .unwrap_or_default(),
                )
            })
            .await
            .clone()
    }

    /// The server's program: a path as it is, a name found on the login `PATH`, the daemon's,
    /// or in the folders installers use. A rustup proxy whose toolchain for `dir` lacks the tool
    /// is no program ([`rustup::missing_component`]).
    fn locate(
        spec: &ServerSpec,
        env: &BTreeMap<String, String>,
        dir: &Path,
    ) -> Result<PathBuf, String> {
        let (path, fallback) = program_dirs(env);
        let program = find_program(&spec.command, path.as_deref())
            .or_else(|| find_in(&spec.command, &fallback))
            .ok_or_else(|| {
                if spec.command.contains('/') {
                    format!("`{}` is not an executable file", spec.command)
                } else {
                    format!(
                        "`{}` was not found on the login PATH or in the usual install folders",
                        spec.command
                    )
                }
            })?;
        match rustup::missing_component(&program, env, dir) {
            Some(reason) => Err(reason),
            None => Ok(program),
        }
    }

    // Planning ---------------------------------------------------------------------------------

    fn project_of(
        path: &Path,
        workspaces: &[hutan_protocol::messages::WorkspaceInfo],
    ) -> Option<Project> {
        workspaces
            .iter()
            .flat_map(|ws| ws.roots.iter().map(move |root| (ws, root)))
            .filter(|(_, root)| path.starts_with(root))
            .max_by_key(|(_, root)| root.components().count())
            .map(|(ws, root)| Project {
                id: ws.id.to_string(),
                name: ws.name.clone(),
                root: root.clone(),
            })
    }

    /// Where a document goes: which server, which root, whether the binary exists. Reads the
    /// file system.
    fn plan_blocking(
        catalog: &Catalog,
        settings: &LspSettings,
        env: &BTreeMap<String, String>,
        path: &Path,
        language: &str,
        project: Option<&Project>,
    ) -> Plan {
        let over = project.and_then(|p| {
            settings
                .projects
                .get(&p.id)
                .or_else(|| settings.projects.get(&p.name))
        });
        if over.is_some_and(|o| o.enabled == Some(false)) {
            return Plan::None(format!(
                "language servers are switched off for the project `{}` ([lsp.projects] in daemon.toml)",
                project.map_or("", |p| p.name.as_str())
            ));
        }
        let candidates: Vec<&ServerSpec> = catalog.for_language(language).collect();
        let Some(first) = candidates.first() else {
            return Plan::None(format!("no language server is configured for `{language}`"));
        };
        // rustup reads the toolchain from the file's folder up.
        let dir = path.parent().unwrap_or(path);
        let mut chosen = None;
        let mut first_error = None;
        for spec in &candidates {
            match Self::locate(spec, env, dir) {
                Ok(program) => {
                    chosen = Some((*spec, Ok(program)));
                    break;
                }
                Err(reason) => {
                    first_error.get_or_insert(reason);
                }
            }
        }
        // None found: the first one is the one to install.
        let (spec, program) =
            chosen.unwrap_or_else(|| (*first, Err(first_error.unwrap_or_default())));
        let project_root = project.map(|p| p.root.as_path());
        let root = over
            .and_then(|o| o.roots.get(&spec.name))
            .and_then(|configured| roots::overridden(path, configured, project_root))
            .unwrap_or_else(|| {
                let boundary = roots::boundary(path, project_root);
                roots::detect(path, &spec.root_markers, boundary.as_deref())
            });
        Plan::Server {
            spec: Arc::new(spec.clone()),
            root,
            program,
        }
    }

    // Documents --------------------------------------------------------------------------------

    /// `doc.open`.
    pub async fn doc_open(
        self: &Arc<Self>,
        conn: &ConnHandle,
        agent: bool,
        req: DocOpen,
    ) -> Result<DocOpened, ErrorBody> {
        if req.text.len() > DOC_OPEN_TEXT_MAX_BYTES {
            return Err(unsupported(
                format!(
                    "the document is {} bytes; a language server gets at most {DOC_OPEN_TEXT_MAX_BYTES}",
                    req.text.len()
                ),
                "too_large",
            ));
        }
        let path = canonical(&req.path).await?;
        let workspaces = self
            .registry
            .list_workspaces()
            .await
            .map(|list| list.workspaces)
            .unwrap_or_default();
        if agent {
            let roots: Vec<PathBuf> = workspaces.iter().flat_map(|w| w.roots.clone()).collect();
            if !is_inside(&path, &roots) {
                return Err(ErrorBody::new(
                    ErrorCode::Forbidden,
                    format!(
                        "`{}` is outside the workspace folders: an agent may open documents only inside them",
                        path.display()
                    ),
                ));
            }
        }
        let language = req.language.clone().or_else(|| language_of(&path));
        let (doc_id, owner) = self.register(conn, &path, language.clone(), &req)?;
        if !owner {
            let servers = self.servers_of_mirror(&path);
            return Ok(DocOpened {
                doc_id,
                owner,
                language,
                reason: servers
                    .is_empty()
                    .then(|| "no language server serves this document".to_owned()),
                servers,
            });
        }
        match self.attach(&path, language.as_deref(), &workspaces).await {
            Ok(Attached::None(reason)) => Ok(DocOpened {
                doc_id,
                owner,
                language,
                servers: Vec::new(),
                reason: Some(reason),
            }),
            Ok(Attached::Slot(slot_id)) => {
                self.ensure_running(&slot_id).await;
                let info = self.state.lock().slots.get(&slot_id).map(Slot::info);
                Ok(DocOpened {
                    doc_id,
                    owner,
                    language,
                    servers: info.iter().map(doc_server).collect(),
                    reason: None,
                })
            }
            Err(error) => {
                // Nothing keeps a document the client was told is not open.
                self.drop_doc(&doc_id);
                Err(error)
            }
        }
    }

    /// Registers the document of `conn` and, for the first connection that opens `path`, its
    /// mirror.
    fn register(
        &self,
        conn: &ConnHandle,
        path: &Path,
        language: Option<String>,
        req: &DocOpen,
    ) -> Result<(String, bool), ErrorBody> {
        let mut state = self.state.lock();
        if state.stopping {
            return Err(invalid_state("the daemon is shutting down"));
        }
        let entry = state.conns.entry(conn.id()).or_insert_with(|| ConnDocs {
            handle: conn.clone(),
            docs: BTreeSet::new(),
        });
        if entry.docs.len() >= DOCS_PER_CONNECTION_MAX {
            return Err(invalid_state(format!(
                "this connection has {DOCS_PER_CONNECTION_MAX} documents open already"
            )));
        }
        let mine = entry.docs.clone();
        if mine
            .iter()
            .any(|id| state.docs.get(id).is_some_and(|d| d.path == path))
        {
            return Err(invalid_state(format!(
                "`{}` is already open in this connection",
                path.display()
            )));
        }
        let owner = !state.mirrors.contains_key(path);
        if owner && state.mirror_bytes + req.text.len() > MAX_MIRROR_BYTES {
            return Err(invalid_state(format!(
                "the daemon mirrors at most {MAX_MIRROR_BYTES} bytes of open documents"
            )));
        }
        let uri = convert::uri_of(path)
            .ok_or_else(|| bad(format!("`{}` has no file URI", path.display())))?;
        state.next_doc += 1;
        let doc_id = format!("d{}", state.next_doc);
        if owner {
            let buffer = mirror_buffer(&req.text)?;
            state.mirror_bytes += req.text.len();
            state.mirrors.insert(
                path.to_path_buf(),
                Mirror {
                    buffer,
                    language,
                    version: req.version,
                    uri,
                    slot: None,
                },
            );
        }
        state.docs.insert(
            doc_id.clone(),
            DocHandle {
                path: path.to_path_buf(),
                conn: conn.id(),
                owner,
            },
        );
        if let Some(entry) = state.conns.get_mut(&conn.id()) {
            entry.docs.insert(doc_id.clone());
        }
        Ok((doc_id, owner))
    }

    fn servers_of_mirror(&self, path: &Path) -> Vec<DocServer> {
        let state = self.state.lock();
        state
            .mirrors
            .get(path)
            .and_then(|mirror| mirror.slot.as_ref())
            .and_then(|id| state.slots.get(id))
            .map(|slot| doc_server(&slot.info()))
            .into_iter()
            .collect()
    }

    /// Finds the server for the owner's document and puts the document in its slot.
    async fn attach(
        self: &Arc<Self>,
        path: &Path,
        language: Option<&str>,
        workspaces: &[hutan_protocol::messages::WorkspaceInfo],
    ) -> Result<Attached, ErrorBody> {
        if !self.settings.enabled {
            return Ok(Attached::None(
                "language servers are off ([lsp] enabled = false in daemon.toml)".to_owned(),
            ));
        }
        let Some(language) = language else {
            return Ok(Attached::None(format!(
                "no language is known for `{}`",
                path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned()
                )
            )));
        };
        let env = self.environment().await;
        let project = Self::project_of(path, workspaces);
        let plan = {
            let hub = self.clone();
            let (path, language) = (path.to_path_buf(), language.to_owned());
            tokio::task::spawn_blocking(move || {
                Self::plan_blocking(
                    &hub.catalog,
                    &hub.settings,
                    &env,
                    &path,
                    &language,
                    project.as_ref(),
                )
            })
            .await
            .map_err(|e| internal(format!("planning the server failed: {e}")))?
        };
        let Plan::Server {
            spec,
            root,
            program,
        } = plan
        else {
            let Plan::None(reason) = plan else {
                return Err(internal("unreachable plan"));
            };
            return Ok(Attached::None(reason));
        };
        let (slot_id, infos) = {
            let mut state = self.state.lock();
            let slot_id = Self::slot_for(&mut state, &spec, &root)?;
            let mut infos = Vec::new();
            let mode = self.settings.trust;
            let standing = state.trust.standing(&root, mode);
            let State { slots, mirrors, .. } = &mut *state;
            let Some(slot) = slots.get_mut(&slot_id) else {
                return Err(internal("the server slot vanished"));
            };
            slot.last_used = Instant::now();
            slot.docs.insert(path.to_path_buf());
            slot.idle_since = None;
            if let Some(mirror) = mirrors.get_mut(path) {
                mirror.slot = Some(slot_id.clone());
                if let Some(server) = &slot.server {
                    let text = mirror.buffer.snapshot();
                    if let Err(error) = server.open(
                        mirror.uri.clone(),
                        mirror.language.as_deref().unwrap_or(language),
                        mirror.version,
                        text,
                    ) {
                        debug!(%error, path = %path.display(), "the server did not take the document");
                    }
                }
            }
            if slot.server.is_none() {
                let before = slot.info();
                Self::settle(slot, standing, &program);
                let after = slot.info();
                if (before.state, &before.reason) != (after.state, &after.reason) {
                    infos.push(after);
                }
            }
            (slot_id, infos)
        };
        self.emit(infos);
        Ok(Attached::Slot(slot_id))
    }

    /// The slot of `spec` that covers `root`: an existing one whose root contains it (a server
    /// is shared only when the root is inside its workspace folders), else a new one.
    fn slot_for(
        state: &mut State,
        spec: &Arc<ServerSpec>,
        root: &Path,
    ) -> Result<String, ErrorBody> {
        if let Some(existing) = state
            .slots
            .values()
            .filter(|s| s.spec.name == spec.name && root.starts_with(&s.root))
            .min_by_key(|s| s.root.components().count())
        {
            return Ok(existing.id.clone());
        }
        if state.slots.len() >= MAX_SLOTS {
            let victim = state
                .slots
                .values()
                .filter(|s| s.docs.is_empty() && s.server.is_none())
                .min_by_key(|s| s.last_used)
                .map(|s| s.id.clone());
            match victim {
                Some(id) => {
                    state.slots.remove(&id);
                }
                None => {
                    return Err(invalid_state(format!(
                        "{MAX_SLOTS} language server roots are in use; close some documents"
                    )));
                }
            }
        }
        let id = server_id(&spec.name, root);
        state.slots.insert(
            id.clone(),
            Slot {
                id: id.clone(),
                spec: spec.clone(),
                root: root.to_path_buf(),
                server: None,
                down: Some(Down {
                    state: LspServerState::Stopped,
                    reason: "not started".to_owned(),
                    pending_trust: false,
                }),
                docs: BTreeSet::new(),
                last_used: Instant::now(),
                idle_since: None,
                generation: 0,
                starting: false,
                pending_diagnostics: HashSet::new(),
                diagnostics_scheduled: false,
            },
        );
        Ok(id)
    }

    /// Says why a slot without a server has none: the folder is not trusted, the binary is
    /// missing, or nothing stands in the way (`stopped`, about to start).
    fn settle(slot: &mut Slot, standing: Standing, program: &Result<PathBuf, String>) {
        let root = slot.root.display();
        slot.down = Some(match (standing, program) {
            (Standing::Denied, _) => Down {
                state: LspServerState::Untrusted,
                reason: format!(
                    "{root} is not trusted: a language server runs the project's build scripts and is never started here (`hutan lsp trust {root}` changes that)"
                ),
                pending_trust: false,
            },
            (Standing::Ask, _) => Down {
                state: LspServerState::Untrusted,
                reason: format!(
                    "{root} is not trusted yet: a language server runs the project's build scripts; `hutan lsp trust {root}` allows it"
                ),
                pending_trust: true,
            },
            (Standing::Trusted, Err(reason)) => Down {
                state: LspServerState::NotInstalled,
                reason: reason.clone(),
                pending_trust: false,
            },
            (Standing::Trusted, Ok(_)) => Down {
                state: LspServerState::Stopped,
                reason: "not started".to_owned(),
                pending_trust: false,
            },
        });
    }

    // Starting and stopping --------------------------------------------------------------------

    /// Starts the slot's server when the folder is trusted, the binary exists and there is room;
    /// the documents of the slot are sent to it. A no-op while one runs.
    async fn ensure_running(self: &Arc<Self>, slot_id: &str) {
        let (spec, root) = {
            let mut state = self.state.lock();
            if state.stopping {
                return;
            }
            let standing = {
                let Some(slot) = state.slots.get(slot_id) else {
                    return;
                };
                state.trust.standing(&slot.root, self.settings.trust)
            };
            let Some(slot) = state.slots.get_mut(slot_id) else {
                return;
            };
            if slot.server.is_some() || slot.starting || standing != Standing::Trusted {
                return;
            }
            slot.starting = true;
            (slot.spec.clone(), slot.root.clone())
        };
        let env = self.environment().await;
        let program = {
            let (spec, env) = (spec.clone(), env.clone());
            tokio::task::spawn_blocking(move || Self::locate(&spec, &env, &root))
                .await
                .unwrap_or_else(|e| Err(format!("looking for the server failed: {e}")))
        };
        let mut evicted: Vec<(Arc<Server>, String)> = Vec::new();
        let infos = {
            let mut state = self.state.lock();
            let infos = self.start_locked(&mut state, slot_id, program, &env, &mut evicted);
            if let Some(slot) = state.slots.get_mut(slot_id) {
                slot.starting = false;
            }
            infos
        };
        self.emit(infos);
        for (server, _) in evicted {
            server.stop().await;
        }
    }

    /// The part of [`ensure_running`](Self::ensure_running) under the lock.
    fn start_locked(
        self: &Arc<Self>,
        state: &mut State,
        slot_id: &str,
        program: Result<PathBuf, String>,
        env: &BTreeMap<String, String>,
        evicted: &mut Vec<(Arc<Server>, String)>,
    ) -> Vec<LspServerInfo> {
        let mut infos = Vec::new();
        let Some(slot) = state.slots.get(slot_id) else {
            return infos;
        };
        let program = match program {
            Ok(program) => program,
            Err(reason) => {
                if let Some(slot) = state.slots.get_mut(slot_id) {
                    slot.down = Some(Down {
                        state: LspServerState::NotInstalled,
                        reason,
                        pending_trust: false,
                    });
                    infos.push(slot.info());
                }
                return infos;
            }
        };
        if slot.server.is_some() {
            return infos;
        }
        // Room: the least recently used server without documents makes way.
        // A server that ended (`failed`, `stopped`) holds no process and no room.
        let running = state
            .slots
            .values()
            .filter(|s| {
                s.server
                    .as_ref()
                    .is_some_and(|server| !server.status().state.is_final())
            })
            .count();
        if running >= self.settings.max_servers as usize {
            let victim = state
                .slots
                .values()
                .filter(|s| s.server.is_some() && s.docs.is_empty())
                .min_by_key(|s| s.last_used)
                .map(|s| s.id.clone());
            match victim {
                Some(victim) => {
                    let reason = format!(
                        "stopped to make room for {} (`[lsp] max_servers` is {})",
                        slot_id, self.settings.max_servers
                    );
                    if let Some(server) = Self::take_server(state, &victim, &reason, &mut infos) {
                        evicted.push((server, reason));
                    }
                }
                None => {
                    if let Some(slot) = state.slots.get_mut(slot_id) {
                        slot.down = Some(Down {
                            state: LspServerState::Stopped,
                            reason: format!(
                                "`[lsp] max_servers` ({}) servers run and each has documents open: close some or stop one with `hutan lsp stop`",
                                self.settings.max_servers
                            ),
                            pending_trust: false,
                        });
                        infos.push(slot.info());
                    }
                    return infos;
                }
            }
        }
        let Some(slot) = state.slots.get(slot_id) else {
            return infos;
        };
        let mut launch_env = env.clone();
        with_program_dir(&mut launch_env, &program);
        launch_env.extend(slot.spec.env.clone());
        let launch = Launch {
            program,
            args: slot.spec.args.clone(),
            cwd: Some(slot.root.clone()),
            env: launch_env.into_iter().collect(),
        };
        let mut config = ServerConfig::new(slot.spec.name.clone(), launch, slot.root.clone());
        config.initialization_options = slot.spec.initialization_options.clone();
        config.settings = slot
            .spec
            .settings
            .clone()
            .unwrap_or(serde_json::Value::Null);
        let mut restart = RestartPolicy::default();
        if let Some(max) = slot.spec.max_failures {
            restart.max_failures = max;
        }
        if let Some(window) = slot.spec.failure_window {
            restart.window = window;
        }
        config.restart = self.tuning.restart.unwrap_or(restart);
        if let Some(timeouts) = self.tuning.timeouts {
            config.timeouts = timeouts;
        }
        if let Some(coalesce) = self.tuning.coalesce {
            config.coalesce = coalesce;
        }
        let server = Arc::new(Server::start(config));
        let subscription = match server.subscribe() {
            Ok(subscription) => subscription,
            Err(error) => {
                warn!(%error, "could not follow the language server");
                return infos;
            }
        };
        let State { slots, mirrors, .. } = state;
        let Some(slot) = slots.get_mut(slot_id) else {
            return infos;
        };
        slot.generation += 1;
        slot.down = None;
        slot.last_used = Instant::now();
        for path in &slot.docs {
            if let Some(mirror) = mirrors.get(path) {
                let language = mirror
                    .language
                    .clone()
                    .or_else(|| slot.spec.languages.first().cloned())
                    .unwrap_or_default();
                if let Err(error) = server.open(
                    mirror.uri.clone(),
                    &language,
                    mirror.version,
                    mirror.buffer.snapshot(),
                ) {
                    debug!(%error, path = %path.display(), "the server did not take the document");
                }
            }
        }
        if slot.docs.is_empty() {
            slot.idle_since = Some(Instant::now());
        }
        slot.server = Some(server);
        info!(server = %slot.id, "language server starting");
        tokio::spawn(follow(
            self.me.clone(),
            slot.id.clone(),
            slot.generation,
            subscription,
        ));
        infos.push(slot.info());
        infos
    }

    /// Takes the server out of the slot, which becomes `stopped` for `reason`; the caller stops
    /// it.
    fn take_server(
        state: &mut State,
        slot_id: &str,
        reason: &str,
        infos: &mut Vec<LspServerInfo>,
    ) -> Option<Arc<Server>> {
        let slot = state.slots.get_mut(slot_id)?;
        let server = slot.server.take()?;
        slot.down = Some(Down {
            state: LspServerState::Stopped,
            reason: reason.to_owned(),
            pending_trust: false,
        });
        slot.idle_since = None;
        slot.generation += 1;
        slot.pending_diagnostics.clear();
        infos.push(slot.info());
        Some(server)
    }

    async fn stop_slot(&self, slot_id: &str, reason: &str) {
        let (server, infos) = {
            let mut state = self.state.lock();
            let mut infos = Vec::new();
            let server = Self::take_server(&mut state, slot_id, reason, &mut infos);
            (server, infos)
        };
        self.emit(infos);
        if let Some(server) = server {
            server.stop().await;
        }
    }

    fn emit(&self, infos: Vec<LspServerInfo>) {
        for info in infos {
            if let Err(error) = self.bus.publish(&Event::LspState {
                server: Box::new(info),
            }) {
                debug!(%error, "an lsp.state event was not published");
            }
        }
    }

    fn idle_limit(&self, spec: &ServerSpec) -> Option<Duration> {
        if let Some(idle) = self.tuning.idle_stop {
            return Some(idle);
        }
        match spec.idle_stop_secs.unwrap_or(self.settings.idle_stop_secs) {
            0 => None,
            secs => Some(Duration::from_secs(secs)),
        }
    }

    /// Stops the servers that had no document for their idle limit.
    async fn idle_loop(self: Arc<Self>) {
        let period = self
            .tuning
            .idle_stop
            .map_or(IDLE_TICK, |idle| (idle / 4).max(Duration::from_millis(5)));
        let mut tick = interval(period);
        loop {
            tokio::select! {
                () = self.shutdown.cancelled() => return,
                _ = tick.tick() => {}
            }
            let now = Instant::now();
            let due: Vec<String> = {
                let state = self.state.lock();
                state
                    .slots
                    .values()
                    .filter(|slot| slot.server.is_some() && slot.docs.is_empty())
                    .filter(|slot| {
                        let (Some(since), Some(limit)) =
                            (slot.idle_since, self.idle_limit(&slot.spec))
                        else {
                            return false;
                        };
                        now.saturating_duration_since(since) >= limit
                    })
                    .map(|slot| slot.id.clone())
                    .collect()
            };
            for id in due {
                self.stop_slot(&id, "idle: no document was open for a while")
                    .await;
            }
        }
    }

    // Edits ------------------------------------------------------------------------------------

    /// `doc.change`.
    pub fn doc_change(&self, conn: ConnId, req: DocChange) -> Result<(), ErrorBody> {
        if req.text.is_some() == !req.edits.is_empty() {
            return Err(bad("`doc.change` takes `edits` or `text`, one of them"));
        }
        if req.edits.len() > DOC_EDITS_MAX {
            return Err(bad(format!(
                "at most {DOC_EDITS_MAX} edits fit one `doc.change`"
            )));
        }
        let bytes: usize = req.edits.iter().map(|e| e.text.len()).sum::<usize>()
            + req.text.as_ref().map_or(0, String::len);
        let limit = if req.text.is_some() {
            DOC_OPEN_TEXT_MAX_BYTES
        } else {
            DOC_CHANGE_MAX_BYTES
        };
        if bytes > limit {
            return Err(bad(format!(
                "a `doc.change` carries at most {limit} bytes of text, not {bytes}"
            )));
        }
        let mut state = self.state.lock();
        let handle = Self::handle_of(&state, conn, &req.doc_id)?;
        if !handle.owner {
            return Err(invalid_state(
                "another connection owns this document: only its owner sends changes",
            ));
        }
        let path = handle.path.clone();
        let State {
            mirrors,
            slots,
            mirror_bytes,
            ..
        } = &mut *state;
        let mirror = mirrors
            .get_mut(&path)
            .ok_or_else(|| invalid_state("the document is closed: open it again"))?;
        let server = mirror
            .slot
            .as_ref()
            .and_then(|id| slots.get(id))
            .and_then(|slot| slot.server.clone());
        let before = mirror.buffer.snapshot();
        let mut steps: Vec<(Text, Vec<Change>)> = Vec::new();
        let applied = apply_change(
            &mut mirror.buffer,
            &req,
            *mirror_bytes - before.len(),
            &mut steps,
        );
        let size = match applied {
            Ok(size) => size,
            Err(error) => {
                // All of the request or none of it.
                if let Ok(buffer) = mirror_buffer(&before.to_string()) {
                    mirror.buffer = buffer;
                }
                return Err(error);
            }
        };
        *mirror_bytes = *mirror_bytes - before.len() + size;
        mirror.version = req.version;
        if let Some(server) = server {
            // The server's client coalesces these: one `didChange` per window, or before the
            // next request.
            let result = if req.text.is_some() {
                server.replace(&mirror.uri, req.version, mirror.buffer.snapshot())
            } else {
                steps.iter().try_for_each(|(text, changes)| {
                    server.change(&mirror.uri, req.version, text.clone(), changes)
                })
            };
            if let Err(error) = result {
                debug!(%error, "the server did not take the change");
            }
        }
        Ok(())
    }

    fn handle_of<'a>(
        state: &'a State,
        conn: ConnId,
        doc_id: &str,
    ) -> Result<&'a DocHandle, ErrorBody> {
        state
            .docs
            .get(doc_id)
            .filter(|handle| handle.conn == conn)
            .ok_or_else(|| {
                not_found(format!(
                    "document `{doc_id}` is not open in this connection"
                ))
            })
    }

    /// `doc.save`.
    pub fn doc_save(&self, conn: ConnId, doc_id: &str) -> Result<(), ErrorBody> {
        let state = self.state.lock();
        let handle = Self::handle_of(&state, conn, doc_id)?;
        if !handle.owner {
            return Ok(());
        }
        if let Some(mirror) = state.mirrors.get(&handle.path)
            && let Some(server) = mirror
                .slot
                .as_ref()
                .and_then(|id| state.slots.get(id))
                .and_then(|slot| slot.server.as_ref())
            && let Err(error) = server.save(&mirror.uri)
        {
            debug!(%error, "the server did not take the save");
        }
        Ok(())
    }

    /// `doc.close`.
    pub fn doc_close(&self, conn: ConnId, doc_id: &str) -> Result<(), ErrorBody> {
        {
            let state = self.state.lock();
            Self::handle_of(&state, conn, doc_id)?;
        }
        self.drop_doc(doc_id);
        Ok(())
    }

    /// Forgets one document of a connection: its handle, and when it owned the mirror, the
    /// mirror and the server's copy.
    fn drop_doc(&self, doc_id: &str) {
        let mut infos = Vec::new();
        {
            let mut state = self.state.lock();
            let Some(handle) = state.docs.remove(doc_id) else {
                return;
            };
            if let Some(entry) = state.conns.get_mut(&handle.conn) {
                entry.docs.remove(doc_id);
                if entry.docs.is_empty() {
                    state.conns.remove(&handle.conn);
                }
            }
            if !handle.owner {
                return;
            }
            let Some(mirror) = state.mirrors.remove(&handle.path) else {
                return;
            };
            state.mirror_bytes = state
                .mirror_bytes
                .saturating_sub(mirror.buffer.text().len());
            let Some(slot_id) = mirror.slot else {
                return;
            };
            let Some(slot) = state.slots.get_mut(&slot_id) else {
                return;
            };
            slot.docs.remove(&handle.path);
            slot.last_used = Instant::now();
            slot.pending_diagnostics.remove(&handle.path);
            if let Some(server) = &slot.server
                && let Err(error) = server.close(&mirror.uri)
            {
                debug!(%error, "the server did not take the close");
            }
            if slot.docs.is_empty() {
                slot.idle_since = Some(Instant::now());
                // A request to trust, or a missing binary, that nobody waits on any more.
                if slot.server.is_none()
                    && slot.down.as_ref().is_some_and(|d| {
                        matches!(
                            d.state,
                            LspServerState::Untrusted | LspServerState::NotInstalled
                        )
                    })
                {
                    let mut gone = slot.info();
                    gone.state = LspServerState::Stopped;
                    gone.trust_pending = false;
                    gone.reason = Some("no document is open any more".to_owned());
                    gone.install = None;
                    gone.setting = None;
                    infos.push(gone);
                    state.slots.remove(&slot_id);
                }
            }
        }
        self.emit(infos);
    }

    /// A connection went away: its documents are closed.
    pub fn connection_closed(&self, conn: ConnId) {
        let docs: Vec<String> = {
            let state = self.state.lock();
            state
                .conns
                .get(&conn)
                .map(|entry| entry.docs.iter().cloned().collect())
                .unwrap_or_default()
        };
        for doc in docs {
            self.drop_doc(&doc);
        }
    }

    // Servers' events --------------------------------------------------------------------------

    fn on_event(self: &Arc<Self>, slot_id: &str, generation: u64, event: ServerEvent) {
        let mut infos = Vec::new();
        let mut schedule = false;
        {
            let mut state = self.state.lock();
            let State { slots, mirrors, .. } = &mut *state;
            let Some(slot) = slots.get_mut(slot_id) else {
                return;
            };
            if slot.generation != generation {
                return;
            }
            match event {
                ServerEvent::State(_) | ServerEvent::Restarting { .. } => infos.push(slot.info()),
                ServerEvent::Diagnostics { uri, .. } => {
                    if let Some(path) = convert::path_of(&uri)
                        && mirrors.contains_key(&path)
                    {
                        slot.pending_diagnostics.insert(path);
                    }
                }
                ServerEvent::Lagged(_) => {
                    // Events were lost: every open document gets its diagnostics again.
                    slot.pending_diagnostics.extend(slot.docs.iter().cloned());
                }
                ServerEvent::Progress { .. } | ServerEvent::Log(_) => {}
            }
            if !slot.pending_diagnostics.is_empty() && !slot.diagnostics_scheduled {
                slot.diagnostics_scheduled = true;
                schedule = true;
            }
        }
        self.emit(infos);
        if schedule {
            let (hub, slot_id) = (self.clone(), slot_id.to_owned());
            tokio::spawn(async move {
                sleep(Duration::from_millis(DIAGNOSTICS_EVENT_INTERVAL_MS)).await;
                hub.flush_diagnostics(&slot_id).await;
            });
        }
    }

    /// Sends the diagnostics that changed to the connections that have the files open: at most
    /// one event per file per [`DIAGNOSTICS_EVENT_INTERVAL_MS`].
    async fn flush_diagnostics(self: &Arc<Self>, slot_id: &str) {
        let (server, paths) = {
            let mut state = self.state.lock();
            let Some(slot) = state.slots.get_mut(slot_id) else {
                return;
            };
            slot.diagnostics_scheduled = false;
            let paths: Vec<PathBuf> = slot.pending_diagnostics.drain().collect();
            (slot.server.clone(), paths)
        };
        let Some(server) = server else {
            return;
        };
        let Some(encoding) = server.encoding() else {
            return;
        };
        for path in paths {
            let (texts, targets, uri) = {
                let state = self.state.lock();
                let Some(mirror) = state.mirrors.get(&path) else {
                    continue;
                };
                let mut texts = Texts::new();
                texts.insert(path.clone(), mirror.buffer.snapshot());
                let targets: Vec<ConnHandle> = state
                    .docs
                    .values()
                    .filter(|handle| handle.path == path)
                    .filter_map(|handle| state.conns.get(&handle.conn))
                    .map(|entry| entry.handle.clone())
                    .collect();
                (texts, targets, mirror.uri.clone())
            };
            if targets.is_empty() {
                continue;
            }
            let file = server.diagnostics(&uri).unwrap_or_default();
            let mut diagnostics: Vec<LspDiagnostic> = file
                .diagnostics
                .iter()
                .map(|d| convert::diagnostic(d, &path, &texts, encoding))
                .collect();
            let mut truncated = file.truncated;
            let frame = loop {
                let event = Event::LspDiagnosticsChanged {
                    path: path.clone(),
                    version: file.version,
                    diagnostics: diagnostics.clone(),
                    truncated,
                };
                match self.frame(&event) {
                    Some(frame) => break Some(frame),
                    None if diagnostics.len() > 1 => {
                        diagnostics.truncate(diagnostics.len() / 2);
                        truncated = true;
                    }
                    None => break None,
                }
            };
            let Some(frame) = frame else {
                warn!(path = %path.display(), "an lsp.diagnostics event does not fit a frame; skipped");
                continue;
            };
            for handle in targets {
                // A lost event is replaced by the next one, or asked for with `lsp.diagnostics`.
                if handle.enqueue_local(frame.clone()) == LocalSend::Dropped {
                    debug!(conn = %handle.id(), "an lsp.diagnostics event was dropped: the queue is full");
                }
            }
        }
    }

    fn frame(&self, event: &Event) -> Option<Frame> {
        let frame = ControlMessage::event(self.bus.epoch(), self.bus.last_seq(), event)
            .ok()?
            .to_frame()
            .ok()?;
        (frame.payload.len() <= MAX_PAYLOAD).then_some(frame)
    }

    // Queries ----------------------------------------------------------------------------------

    /// `lsp.status`.
    pub async fn status(&self, req: LspStatusRequest) -> Result<LspStatus, ErrorBody> {
        let roots = match req.workspace_id {
            Some(id) => Some(self.workspace_roots(id).await?),
            None => None,
        };
        let (mut servers, trust) = {
            let state = self.state.lock();
            (
                state
                    .slots
                    .values()
                    .filter(|slot| {
                        roots.as_ref().is_none_or(|roots| {
                            roots
                                .iter()
                                .any(|r| slot.root.starts_with(r) || r.starts_with(&slot.root))
                        })
                    })
                    .map(Slot::info)
                    .collect::<Vec<_>>(),
                state.trust.entries(),
            )
        };
        let source = self.source.clone();
        let pids: Vec<Option<u32>> = servers.iter().map(|s| s.pid).collect();
        let rss = tokio::task::spawn_blocking(move || {
            pids.into_iter()
                .map(|pid| pid.map(|pid| tree_rss(&*source, pid)))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for (info, rss) in servers.iter_mut().zip(rss) {
            info.rss_bytes = rss;
        }
        Ok(LspStatus {
            enabled: self.settings.enabled,
            trust_policy: match self.settings.trust {
                LspTrustMode::Ask => LspTrustPolicy::Ask,
                LspTrustMode::Always => LspTrustPolicy::Always,
                LspTrustMode::Never => LspTrustPolicy::Never,
            },
            max_servers: self.settings.max_servers,
            servers,
            trust,
        })
    }

    async fn workspace_roots(&self, id: WorkspaceId) -> Result<Vec<PathBuf>, ErrorBody> {
        let list = self.registry.list_workspaces().await?;
        let ws = list
            .workspaces
            .into_iter()
            .find(|w| w.id == id)
            .ok_or_else(|| not_found(format!("workspace {id} not found")))?;
        let roots = tokio::task::spawn_blocking(move || {
            ws.roots
                .iter()
                .map(|r| canonicalize_lenient(r))
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|e| internal(format!("looking up the folders failed: {e}")))?;
        Ok(roots)
    }

    /// `lsp.diagnostics`.
    pub async fn diagnostics(
        &self,
        req: LspDiagnosticsRequest,
    ) -> Result<LspDiagnosticsResult, ErrorBody> {
        let path = match &req.path {
            Some(path) => Some(canonical(path).await?),
            None => None,
        };
        let roots = match req.workspace_id {
            Some(id) => Some(self.workspace_roots(id).await?),
            None => None,
        };
        let mut servers = Vec::new();
        let mut stale = false;
        let mut sources: Vec<(String, Arc<Server>)> = Vec::new();
        {
            let state = self.state.lock();
            for slot in state.slots.values() {
                let covers = match (&path, &roots) {
                    (Some(path), _) => path.starts_with(&slot.root),
                    (None, Some(roots)) => roots
                        .iter()
                        .any(|r| slot.root.starts_with(r) || r.starts_with(&slot.root)),
                    (None, None) => true,
                };
                if !covers {
                    continue;
                }
                let info = slot.info();
                stale |= info.state != LspServerState::Ready;
                servers.push(LspServerBrief {
                    server_id: info.server_id.clone(),
                    name: info.name,
                    state: info.state,
                });
                if let Some(server) = &slot.server {
                    sources.push((info.server_id, server.clone()));
                }
            }
        }
        let mut truncated = false;
        let mut entries: Vec<(String, PathBuf, FileDiagnostics, PositionEncoding)> = Vec::new();
        for (server_id, server) in &sources {
            let encoding = server.encoding().unwrap_or(PositionEncoding::Utf16);
            match &path {
                Some(path) => {
                    if let Some(file) =
                        convert::uri_of(path).and_then(|uri| server.diagnostics(&uri))
                    {
                        entries.push((server_id.clone(), path.clone(), file, encoding));
                    }
                }
                None => {
                    let (files, cut) = server.all_diagnostics();
                    truncated |= cut;
                    for (uri, file) in files {
                        let Some(file_path) = convert::path_of(&uri) else {
                            continue;
                        };
                        if roots.as_ref().is_some_and(|r| !is_inside(&file_path, r)) {
                            continue;
                        }
                        entries.push((server_id.clone(), file_path, file, encoding));
                    }
                }
            }
        }
        entries.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
        let wanted: Vec<PathBuf> = entries
            .iter()
            .filter(|(_, _, file, encoding)| {
                *encoding == PositionEncoding::Utf8 && !file.diagnostics.is_empty()
            })
            .map(|(_, path, _, _)| path.clone())
            .collect();
        let texts = self.texts_of(&wanted).await;
        let mut files = Vec::new();
        let mut total = 0usize;
        for (server_id, file_path, file, encoding) in entries {
            let diagnostics: Vec<LspDiagnostic> = file
                .diagnostics
                .iter()
                .map(|d| convert::diagnostic(d, &file_path, &texts, encoding))
                .filter(|d| {
                    req.min_severity
                        .is_none_or(|min| d.severity.is_none_or(|s| s <= min))
                })
                .collect();
            if diagnostics.is_empty() {
                continue;
            }
            if total + diagnostics.len() > DIAGNOSTICS_TOTAL_MAX {
                truncated = true;
                break;
            }
            total += diagnostics.len();
            files.push(LspFileDiagnostics {
                path: file_path,
                version: file.version,
                server_id,
                diagnostics,
                truncated: file.truncated || file.diagnostics.len() > DIAGNOSTICS_PER_FILE_MAX,
            });
        }
        Ok(LspDiagnosticsResult {
            files,
            truncated,
            stale,
            servers,
        })
    }

    /// The text of each path: the mirror of an open document, else the file read from disk (at
    /// most [`DISK_READS_MAX`] files of at most [`DISK_READ_MAX_BYTES`]). A path without a text
    /// is left out: the server's columns stand.
    async fn texts_of(&self, paths: &[PathBuf]) -> Texts {
        let mut texts = Texts::new();
        let mut missing = Vec::new();
        {
            let state = self.state.lock();
            for path in paths {
                if texts.contains_key(path) {
                    continue;
                }
                match state.mirrors.get(path) {
                    Some(mirror) => {
                        texts.insert(path.clone(), mirror.buffer.snapshot());
                    }
                    None => missing.push(path.clone()),
                }
            }
        }
        missing.sort();
        missing.dedup();
        missing.truncate(DISK_READS_MAX);
        if missing.is_empty() {
            return texts;
        }
        let read = tokio::task::spawn_blocking(move || {
            missing
                .into_iter()
                .filter_map(|path| {
                    let meta = std::fs::metadata(&path).ok()?;
                    if meta.len() > DISK_READ_MAX_BYTES {
                        return None;
                    }
                    let text = std::fs::read_to_string(&path).ok()?;
                    Some((path, Text::from(text.as_str())))
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        texts.extend(read);
        texts
    }

    /// The server and the text of an open document, for a request at a position. A server that
    /// is still starting is waited for (at most [`START_WAIT`]).
    async fn target(
        &self,
        path: &Path,
    ) -> Result<(Arc<Server>, Text, Uri, PositionEncoding), ErrorBody> {
        let (server, info) = {
            let state = self.state.lock();
            let mirror = state.mirrors.get(path).ok_or_else(|| {
                invalid_state(format!(
                    "`{}` is not open: send `doc.open` first",
                    path.display()
                ))
            })?;
            let slot = mirror
                .slot
                .as_ref()
                .and_then(|id| state.slots.get(id))
                .ok_or_else(|| invalid_state("no language server serves this document"))?;
            (slot.server.clone(), slot.info())
        };
        let Some(server) = server else {
            return Err(invalid_state(format!(
                "the language server is {}{}",
                info.state.as_str(),
                info.reason.map(|r| format!(": {r}")).unwrap_or_default()
            )));
        };
        if info.state == LspServerState::Starting {
            let wait = self
                .tuning
                .timeouts
                .unwrap_or_default()
                .initialize
                .min(START_WAIT);
            let _ = timeout(wait, server.wait_for(|s| s.state != ServerState::Starting)).await;
        }
        let encoding = server.encoding().ok_or_else(|| {
            let status = server.status();
            invalid_state(format!(
                "the language server is {}{}",
                state_of(status.state).as_str(),
                status
                    .last_error
                    .map(|r| format!(": {r}"))
                    .unwrap_or_default()
            ))
        })?;
        let (text, uri) = {
            let state = self.state.lock();
            let mirror = state
                .mirrors
                .get(path)
                .ok_or_else(|| invalid_state("the document was closed"))?;
            (mirror.buffer.snapshot(), mirror.uri.clone())
        };
        Ok((server, text, uri, encoding))
    }

    fn position_params(
        uri: &Uri,
        text: &Text,
        encoding: PositionEncoding,
        at: hutan_protocol::lsp::LspPosition,
    ) -> TextDocumentPositionParams {
        TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: convert::to_server(text, encoding, at),
        }
    }

    /// `lsp.hover`.
    pub async fn hover(&self, req: LspPositionRequest) -> Result<LspHover, ErrorBody> {
        let path = canonical(&req.path).await?;
        let (server, text, uri, encoding) = self.target(&path).await?;
        let hover = server
            .request::<HoverRequest>(HoverParams {
                text_document_position_params: Self::position_params(
                    &uri,
                    &text,
                    encoding,
                    req.position,
                ),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .await
            .map_err(request_error)?;
        Ok(match hover {
            Some(hover) => LspHover {
                contents: convert::hover_text(&hover),
                range: hover
                    .range
                    .map(|range| convert::range_to_wire(Some(&text), encoding, range)),
            },
            None => LspHover {
                contents: None,
                range: None,
            },
        })
    }

    /// `lsp.definition`.
    pub async fn definition(&self, req: LspPositionRequest) -> Result<LspLocations, ErrorBody> {
        let path = canonical(&req.path).await?;
        let (server, text, uri, encoding) = self.target(&path).await?;
        let response = server
            .request::<GotoDefinition>(GotoDefinitionParams {
                text_document_position_params: Self::position_params(
                    &uri,
                    &text,
                    encoding,
                    req.position,
                ),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .await
            .map_err(request_error)?;
        let found: Vec<(Uri, hutan_lsp_client::ls_types::Range)> = match response {
            None => Vec::new(),
            Some(GotoDefinitionResponse::Scalar(location)) => vec![(location.uri, location.range)],
            Some(GotoDefinitionResponse::Array(locations)) => {
                locations.into_iter().map(|l| (l.uri, l.range)).collect()
            }
            Some(GotoDefinitionResponse::Link(links)) => links
                .into_iter()
                .map(|l| (l.target_uri, l.target_selection_range))
                .collect(),
        };
        let targets: Vec<(PathBuf, hutan_lsp_client::ls_types::Range)> = found
            .into_iter()
            .filter_map(|(uri, range)| Some((convert::path_of(&uri)?, range)))
            .take(LOCATIONS_MAX)
            .collect();
        let paths: Vec<PathBuf> = targets.iter().map(|(p, _)| p.clone()).collect();
        let texts = if encoding == PositionEncoding::Utf8 {
            self.texts_of(&paths).await
        } else {
            Texts::new()
        };
        Ok(LspLocations {
            locations: targets
                .into_iter()
                .map(|(path, range)| LspLocation {
                    range: convert::range_to_wire(texts.get(&path), encoding, range),
                    path,
                })
                .collect(),
        })
    }

    /// Runs `work` as the request of kind `kind` of connection `conn`: a newer one of the same
    /// kind cancels it (dropping the server's request sends `$/cancelRequest`), and it answers
    /// `conflict` with `data.reason = "superseded"`.
    async fn superseding<T>(
        &self,
        conn: ConnId,
        kind: Superseded,
        method: &str,
        work: impl Future<Output = Result<T, ErrorBody>>,
    ) -> Result<T, ErrorBody> {
        let token = CancellationToken::new();
        let id = {
            let mut inflight = self.inflight.lock();
            inflight.next += 1;
            let id = inflight.next;
            if let Some((_, older)) = inflight.running.insert((conn, kind), (id, token.clone())) {
                older.cancel();
            }
            id
        };
        let result = tokio::select! {
            biased;
            () = token.cancelled() => {
                let mut error = ErrorBody::new(
                    ErrorCode::Conflict,
                    format!("superseded by a newer `{method}`"),
                );
                error.data = Some(serde_json::json!({ "reason": "superseded" }));
                Err(error)
            }
            result = work => result,
        };
        let mut inflight = self.inflight.lock();
        if inflight
            .running
            .get(&(conn, kind))
            .is_some_and(|(running, _)| *running == id)
        {
            inflight.running.remove(&(conn, kind));
        }
        result
    }

    /// `lsp.completion`.
    pub async fn completion(
        &self,
        conn: ConnId,
        req: LspCompletionRequest,
    ) -> Result<LspCompletionList, ErrorBody> {
        self.superseding(conn, Superseded::Completion, "lsp.completion", async {
            let path = canonical(&req.path).await?;
            let (server, text, uri, encoding) = self.target(&path).await?;
            let trigger_kind = if req.incomplete {
                CompletionTriggerKind::TRIGGER_FOR_INCOMPLETE_COMPLETIONS
            } else if req.trigger.is_some() {
                CompletionTriggerKind::TRIGGER_CHARACTER
            } else {
                CompletionTriggerKind::INVOKED
            };
            let response = server
                .request::<Completion>(CompletionParams {
                    text_document_position: Self::position_params(
                        &uri,
                        &text,
                        encoding,
                        req.position,
                    ),
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                    context: Some(CompletionContext {
                        trigger_kind,
                        trigger_character: req.trigger.clone(),
                    }),
                })
                .await
                .map_err(request_error)?;
            let (items, incomplete) = match response {
                None => (Vec::new(), false),
                Some(CompletionResponse::Array(items)) => (items, false),
                Some(CompletionResponse::List(list)) => (list.items, list.is_incomplete),
            };
            let resolvable = server
                .capabilities()
                .and_then(|c| c.completion_provider)
                .and_then(|c| c.resolve_provider)
                == Some(true);
            let mut truncated = false;
            let mut kept = Vec::new();
            let mut wire = Vec::new();
            for item in items {
                if wire.len() == COMPLETION_ITEMS_MAX {
                    truncated = true;
                    break;
                }
                match convert::completion_item(&item, &text, encoding, COMPLETION_DOC_MAX_BYTES) {
                    Some(mut converted) => {
                        converted.resolvable = resolvable;
                        wire.push(converted);
                        kept.push(item);
                    }
                    None => truncated = true,
                }
            }
            let completion_id = {
                let mut lists = self.completions.lock();
                lists.next += 1;
                let id = lists.next;
                if lists.lists.len() == COMPLETION_LISTS_KEPT {
                    lists.lists.pop_front();
                }
                lists.lists.push_back(KeptList {
                    id,
                    path,
                    items: kept,
                });
                id
            };
            Ok(LspCompletionList {
                completion_id,
                items: wire,
                incomplete,
                truncated,
            })
        })
        .await
    }

    /// `lsp.completion_resolve`: a server without `resolveProvider` answers with the item as
    /// it was.
    pub async fn completion_resolve(
        &self,
        conn: ConnId,
        req: LspCompletionResolve,
    ) -> Result<LspCompletionItem, ErrorBody> {
        self.superseding(conn, Superseded::Resolve, "lsp.completion_resolve", async {
            let (path, item) = {
                let lists = self.completions.lock();
                let list = lists
                    .lists
                    .iter()
                    .find(|l| l.id == req.completion_id)
                    .ok_or_else(|| {
                        not_found(format!(
                            "completion list {} is no longer kept",
                            req.completion_id
                        ))
                    })?;
                let item = usize::try_from(req.index)
                    .ok()
                    .and_then(|i| list.items.get(i))
                    .ok_or_else(|| not_found(format!("no item {}", req.index)))?;
                (list.path.clone(), item.clone())
            };
            let (server, text, _, encoding) = self.target(&path).await?;
            let resolved = match server.request::<ResolveCompletionItem>(item.clone()).await {
                Ok(resolved) => resolved,
                Err(LspError::Unsupported { .. }) => item,
                Err(error) => return Err(request_error(error)),
            };
            let mut converted =
                convert::completion_item(&resolved, &text, encoding, HOVER_MAX_BYTES)
                    .ok_or_else(|| bad("the resolved item is over the bounds of the wire"))?;
            converted.resolvable = false;
            Ok(converted)
        })
        .await
    }

    /// `lsp.signature_help`.
    pub async fn signature_help(
        &self,
        conn: ConnId,
        req: LspSignatureHelpRequest,
    ) -> Result<LspSignatureHelp, ErrorBody> {
        self.superseding(
            conn,
            Superseded::SignatureHelp,
            "lsp.signature_help",
            async {
                let path = canonical(&req.path).await?;
                let (server, text, uri, encoding) = self.target(&path).await?;
                let trigger_kind = if req.trigger.is_some() {
                    SignatureHelpTriggerKind::TRIGGER_CHARACTER
                } else if req.retrigger {
                    SignatureHelpTriggerKind::CONTENT_CHANGE
                } else {
                    SignatureHelpTriggerKind::INVOKED
                };
                let help = server
                    .request::<SignatureHelpRequest>(SignatureHelpParams {
                        context: Some(SignatureHelpContext {
                            trigger_kind,
                            trigger_character: req.trigger.clone(),
                            is_retrigger: req.retrigger,
                            active_signature_help: None,
                        }),
                        text_document_position_params: Self::position_params(
                            &uri,
                            &text,
                            encoding,
                            req.position,
                        ),
                        work_done_progress_params: WorkDoneProgressParams::default(),
                    })
                    .await
                    .map_err(request_error)?;
                Ok(help.map_or(
                    LspSignatureHelp {
                        signatures: Vec::new(),
                        active_signature: 0,
                    },
                    |help| convert::signature_help(&help),
                ))
            },
        )
        .await
    }

    // Control ----------------------------------------------------------------------------------

    /// The slot a `server_id` names: the id itself, or a server name when only one slot has it.
    fn find_slot(&self, server_id: &str) -> Result<String, ErrorBody> {
        let state = self.state.lock();
        if state.slots.contains_key(server_id) {
            return Ok(server_id.to_owned());
        }
        let named: Vec<&String> = state
            .slots
            .values()
            .filter(|s| s.spec.name == server_id)
            .map(|s| &s.id)
            .collect();
        match named.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(not_found(format!("no language server `{server_id}`"))),
            _ => Err(bad(format!(
                "several `{server_id}` servers run: name one by its server_id (`lsp.status`)"
            ))),
        }
    }

    fn info_of(&self, slot_id: &str) -> Result<LspServerInfo, ErrorBody> {
        self.state
            .lock()
            .slots
            .get(slot_id)
            .map(Slot::info)
            .ok_or_else(|| not_found(format!("no language server `{slot_id}`")))
    }

    /// `lsp.restart`: stops the server when it runs, then starts it again (looking for the
    /// binary again when it was missing).
    pub async fn restart(self: &Arc<Self>, server_id: &str) -> Result<LspServerInfo, ErrorBody> {
        let slot_id = self.find_slot(server_id)?;
        {
            let state = self.state.lock();
            if let Some(slot) = state.slots.get(&slot_id)
                && slot.server.is_none()
                && state.trust.standing(&slot.root, self.settings.trust) != Standing::Trusted
            {
                return Err(ErrorBody::new(
                    ErrorCode::Forbidden,
                    format!(
                        "{} is not trusted: `hutan lsp trust {}` first",
                        slot.root.display(),
                        slot.root.display()
                    ),
                ));
            }
        }
        self.stop_slot(&slot_id, "restarting").await;
        self.ensure_running(&slot_id).await;
        self.info_of(&slot_id)
    }

    /// `lsp.stop`: the server stops; the next `doc.open` that needs it starts it again.
    pub async fn stop(self: &Arc<Self>, server_id: &str) -> Result<LspServerInfo, ErrorBody> {
        let slot_id = self.find_slot(server_id)?;
        self.stop_slot(&slot_id, "stopped by request").await;
        self.info_of(&slot_id)
    }

    /// `lsp.trust`: records the user's decision about a folder and applies it to the servers
    /// that live in it.
    pub async fn set_trust(
        self: &Arc<Self>,
        by: &str,
        req: LspTrustRequest,
    ) -> Result<LspTrustResult, ErrorBody> {
        let folder = canonical(&req.folder).await?;
        let entry = LspTrustEntry {
            folder: folder.clone(),
            decision: req.decision,
            decided_at_ms: now_ms(),
            decided_by: by.to_owned(),
        };
        let storage = self.storage.clone();
        let stored = entry.clone();
        tokio::task::spawn_blocking(move || match stored.decision {
            LspTrust::Ask => storage.lsp_trust().forget(stored.folder),
            decision => storage.lsp_trust().set(LspTrustRow {
                folder: stored.folder,
                trusted: decision == LspTrust::Trusted,
                decided_at_ms: stored.decided_at_ms,
                decided_by: stored.decided_by,
            }),
        })
        .await
        .map_err(|e| internal(format!("saving the decision failed: {e}")))?
        .map_err(|e| internal(format!("saving the decision failed: {e}")))?;
        // Every slot below the folder is judged again.
        let mut infos = Vec::new();
        let mut stops = Vec::new();
        let mut starts = Vec::new();
        {
            let mut state = self.state.lock();
            state.trust.set(entry);
            let mode = self.settings.trust;
            let ids: Vec<String> = state
                .slots
                .values()
                .filter(|s| s.root.starts_with(&folder))
                .map(|s| s.id.clone())
                .collect();
            for id in ids {
                let standing = match state.slots.get(&id) {
                    Some(slot) => state.trust.standing(&slot.root, mode),
                    None => continue,
                };
                match standing {
                    Standing::Trusted => {
                        // A server starts for documents that wait for it, not for a slot that
                        // went idle.
                        if state.slots.get(&id).is_some_and(|s| !s.docs.is_empty()) {
                            starts.push(id);
                        }
                    }
                    Standing::Denied | Standing::Ask => {
                        let reason = if standing == Standing::Denied {
                            "the folder is no longer trusted"
                        } else {
                            "the folder's trust was forgotten"
                        };
                        if let Some(server) = Self::take_server(&mut state, &id, reason, &mut infos)
                        {
                            stops.push(server);
                        }
                        if let Some(slot) = state.slots.get_mut(&id) {
                            Self::settle(slot, standing, &Ok(PathBuf::new()));
                            infos.push(slot.info());
                        }
                    }
                }
            }
        }
        self.emit(infos);
        for server in stops {
            server.stop().await;
        }
        for id in starts {
            let needs_check = {
                let mut state = self.state.lock();
                match state.slots.get_mut(&id) {
                    Some(slot) if slot.server.is_none() => {
                        // Not untrusted any more; `ensure_running` looks for the binary.
                        slot.down = Some(Down {
                            state: LspServerState::Stopped,
                            reason: "not started".to_owned(),
                            pending_trust: false,
                        });
                        true
                    }
                    _ => false,
                }
            };
            if needs_check {
                self.ensure_running(&id).await;
            }
        }
        let servers = {
            let state = self.state.lock();
            state
                .slots
                .values()
                .filter(|s| s.root.starts_with(&folder))
                .map(Slot::info)
                .collect()
        };
        Ok(LspTrustResult {
            folder,
            decision: req.decision,
            servers,
        })
    }

    /// Stops every server and refuses new ones: the daemon is ending.
    pub async fn shutdown(&self) {
        let servers: Vec<Arc<Server>> = {
            let mut state = self.state.lock();
            state.stopping = true;
            state
                .slots
                .values_mut()
                .filter_map(|slot| slot.server.take())
                .collect()
        };
        let stops = futures::future::join_all(servers.iter().map(|s| s.stop()));
        if timeout(SHUTDOWN_WAIT, stops).await.is_err() {
            warn!("language servers did not stop in time; their groups are killed");
        }
        // Dropping the last handle kills what lingers.
        drop(servers);
    }
}

impl ServiceTargets for LspHub {
    fn service_targets(&self) -> Vec<ServiceTarget> {
        self.state
            .lock()
            .slots
            .values()
            .filter_map(|slot| {
                let pid = slot.server.as_ref()?.pid()?;
                Some(ServiceTarget {
                    service_id: format!("lsp:{}", slot.id),
                    kind: "lsp".to_owned(),
                    label: format!("{} {}", slot.spec.name, slot.root.display()),
                    root_pid: pid,
                })
            })
            .collect()
    }
}

/// A mirror's buffer: the text as sent, with an undo history that keeps next to nothing.
fn mirror_buffer(text: &str) -> Result<Buffer, ErrorBody> {
    let mut buffer = Buffer::new();
    buffer
        .insert(0, text)
        .map_err(|e| internal(format!("mirroring the text failed: {e}")))?;
    buffer.set_history_limits(HistoryLimits {
        max_groups: 1,
        max_bytes: 4 << 10,
        max_age: Some(Duration::from_millis(1)),
    });
    Ok(buffer)
}

/// Applies the edits of `req` one after the other (LSP `contentChanges` semantics), recording
/// the text after each with its changes; returns the document's size. `others` is what the
/// mirrors hold apart from this document.
fn apply_change(
    buffer: &mut Buffer,
    req: &DocChange,
    others: usize,
    steps: &mut Vec<(Text, Vec<Change>)>,
) -> Result<usize, ErrorBody> {
    if let Some(text) = &req.text {
        if text.len() > DOC_MAX_BYTES || others + text.len() > MAX_MIRROR_BYTES {
            return Err(bad("the document would grow past what the daemon mirrors"));
        }
        let len = buffer.text().len();
        buffer
            .replace(0..len, text.as_str())
            .map_err(|e| bad(format!("the text does not apply: {e}")))?;
        return Ok(text.len());
    }
    let mut size = buffer.text().len();
    for edit in &req.edits {
        let text = buffer.text();
        let point = |p: hutan_protocol::lsp::LspPosition| Utf16Point {
            line: p.line as usize,
            character: p.character as usize,
        };
        let start = text
            .utf16_point_to_byte(point(edit.start))
            .map_err(|e| bad(format!("an edit starts outside the text: {e}")))?;
        let end = text
            .utf16_point_to_byte(point(edit.end))
            .map_err(|e| bad(format!("an edit ends outside the text: {e}")))?;
        if start > end {
            return Err(bad("an edit ends before it starts"));
        }
        size = size + edit.text.len() - (end - start);
        if size > DOC_MAX_BYTES || others + size > MAX_MIRROR_BYTES {
            return Err(bad("the document would grow past what the daemon mirrors"));
        }
        let applied = buffer
            .replace(start..end, edit.text.as_str())
            .map_err(|e| bad(format!("the edit does not apply: {e}")))?;
        steps.push((buffer.snapshot(), applied.transaction.changes()));
    }
    Ok(size)
}

enum Attached {
    None(String),
    Slot(String),
}

/// Follows one server's events until it is gone.
async fn follow(hub: Weak<LspHub>, slot_id: String, generation: u64, mut events: Subscription) {
    while let Some(event) = events.recv().await {
        let Some(hub) = hub.upgrade() else {
            return;
        };
        hub.on_event(&slot_id, generation, event);
    }
}

/// Resident memory of a process and everything below it.
fn tree_rss(source: &dyn ProcessSource, root: u32) -> u64 {
    let mut total = source.inspect(root).map_or(0, |p| p.rss_bytes);
    for pid in source.descendants(root, hutan_protocol::resources::MAX_PROCESSES_PER_SESSION) {
        total += source.inspect(pid).map_or(0, |p| p.rss_bytes);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::summarize;

    #[test]
    fn an_error_of_many_lines_is_summed_up_by_its_last() {
        let stderr = "info: falling back\ninfo: unavailable\n\nerror: infinite recursion detected (exited with code 1)\n";
        assert_eq!(
            summarize(stderr),
            "error: infinite recursion detected (exited with code 1)"
        );
        assert_eq!(summarize("exited with code 3"), "exited with code 3");
        assert_eq!(summarize(""), "");
        assert!(summarize(&"x".repeat(1000)).len() <= 300);
    }
}
