//! [`Server`]: one supervised language server — lifecycle, state machine, restart policy,
//! documents, requests.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hutan_buffer::{Change, Text};
use ls_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification,
};
use ls_types::{
    ConfigurationParams, InitializeResult, LogMessageParams, ProgressParams, ProgressParamsValue,
    ProgressToken, PublishDiagnosticsParams, ServerCapabilities, Uri, WorkDoneProgress,
};
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout, timeout_at};
use tracing::{debug, info, warn};

use crate::codec::{LspCodec, MAX_FRAME_BYTES};
use crate::error::LspError;
use crate::events::{
    DiagnosticsStore, Fanout, FileDiagnostics, LogEntry, LogRing, MAX_MESSAGE_BYTES,
    MAX_PROGRESS_TOKENS, ProgressPhase, ServerEvent, Subscription, clip,
};
use crate::position::PositionEncoding;
use crate::process::{self, ExitInfo, Launch, ProcessHandle, lock};
use crate::requests::{Latency, TypedRequest};
use crate::rpc::{Connection, Handler, INVALID_PARAMS, METHOD_NOT_FOUND, QueueStats};
use crate::sync::{Documents, Outgoing, SyncOptions};

/// Items one `workspace/configuration` request may ask for.
const MAX_CONFIGURATION_ITEMS: usize = 1024;

/// Where a server is in its life.
///
/// `Starting` (process spawned, `initialize` in flight, or waiting to restart) → `Ready`, or
/// `Indexing` while the server reports work-done progress (`$/progress` begin … end) →
/// `Failed` (could not be started, or crashed too often) / `Stopped` (asked to stop).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerState {
    Starting,
    Indexing,
    Ready,
    Failed,
    Stopped,
}

impl ServerState {
    /// No process will run again without a new [`Server`].
    pub fn is_final(self) -> bool {
        matches!(self, Self::Failed | Self::Stopped)
    }
}

/// What a server is doing, as a whole.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerStatus {
    pub state: ServerState,
    /// The server process (also the id of its process group) while one runs; Resources read
    /// the process tree, RSS included, from here.
    pub pid: Option<u32>,
    /// Restarts after a crash so far.
    pub restarts: u32,
    /// Why the last instance ended or could not start.
    pub last_error: Option<String>,
    /// The negotiated position encoding while an instance runs.
    pub encoding: Option<PositionEncoding>,
}

/// How long each step may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// `initialize` (rust-analyzer answers quickly; indexing comes after, as progress).
    pub initialize: Duration,
    /// Requests ([`Latency::Normal`]).
    pub request: Duration,
    /// Requests typing waits for ([`Latency::Interactive`]: completion).
    pub interactive: Duration,
    /// `shutdown` before `exit` is sent anyway.
    pub shutdown: Duration,
    /// After `exit` (or when the client lets go of the process), before its group is killed.
    pub exit: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            initialize: Duration::from_secs(60),
            request: Duration::from_secs(5),
            interactive: Duration::from_secs(2),
            shutdown: Duration::from_secs(2),
            exit: Duration::from_secs(2),
        }
    }
}

/// When a crashed server is started again: after `initial_backoff`, doubling per failure up to
/// `max_backoff`; `max_failures` failures within `window` make it `failed` for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    pub max_failures: u32,
    pub window: Duration,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_failures: 5,
            window: Duration::from_secs(180),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }
}

impl RestartPolicy {
    /// The delay before the restart that follows the `failures`-th recent failure (from 1).
    pub fn backoff(&self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(16);
        self.initial_backoff
            .saturating_mul(1 << doublings)
            .min(self.max_backoff)
    }
}

/// How to run one language server for one workspace root.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    /// For logs (`rust-analyzer`).
    pub name: String,
    pub launch: Launch,
    /// The workspace folder (`rootUri`, `workspaceFolders`).
    pub root: PathBuf,
    /// `initializationOptions`.
    pub initialization_options: Option<Value>,
    /// What `workspace/configuration` answers: the value at each requested section's dotted
    /// path (`rust-analyzer` → `settings["rust-analyzer"]`), the whole value without a section.
    pub settings: Value,
    pub timeouts: Timeouts,
    pub restart: RestartPolicy,
    /// How long document changes are collected before one `didChange` per document is sent
    /// (requests send pending changes first, whatever is left of the window).
    pub coalesce: Duration,
    /// Largest message in either direction ([`MAX_FRAME_BYTES`]).
    pub max_frame: usize,
}

impl ServerConfig {
    pub fn new(name: impl Into<String>, launch: Launch, root: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            launch,
            root: root.into(),
            initialization_options: None,
            settings: Value::Null,
            timeouts: Timeouts::default(),
            restart: RestartPolicy::default(),
            coalesce: Duration::from_millis(50),
            max_frame: MAX_FRAME_BYTES,
        }
    }
}

/// One running server process after `initialize`.
#[derive(Debug)]
struct Instance {
    /// Which start this is (notifications and document state are per instance).
    generation: u64,
    connection: Arc<Connection>,
    process: ProcessHandle,
    capabilities: ServerCapabilities,
    sync: SyncOptions,
    encoding: PositionEncoding,
}

#[derive(Debug, Default)]
struct Observed {
    fanout: Fanout,
    diagnostics: DiagnosticsStore,
    log: LogRing,
    progress: HashSet<ProgressToken>,
    /// The instance whose notifications count; bumped before each start.
    generation: u64,
}

#[derive(Debug)]
struct Shared {
    config: ServerConfig,
    status: watch::Sender<ServerStatus>,
    observed: Mutex<Observed>,
    docs: Mutex<Documents>,
    instance: Mutex<Option<Arc<Instance>>>,
    /// The process being started or running, so that dropping the [`Server`] kills it.
    process: Mutex<Option<ProcessHandle>>,
    /// Document notifications are sent only by the flusher task, so they leave in order and a
    /// caller that gives up waiting never interrupts a flush halfway. `flush_wake` starts the
    /// coalescing window, `flush_now` skips it.
    flush_wake: Notify,
    flush_now: Notify,
    /// Flushes asked for so far, and the last one the flusher covered.
    flush_requested: AtomicU64,
    flushed: watch::Sender<u64>,
}

/// A supervised language server: started, restarted with backoff, stopped; fed documents;
/// asked typed requests. Dropping it kills the server's process group at once.
///
/// Must be created inside a Tokio runtime.
#[derive(Debug)]
pub struct Server {
    shared: Arc<Shared>,
    stop: mpsc::Sender<()>,
    supervisor: JoinHandle<()>,
    flusher: JoinHandle<()>,
}

impl Server {
    /// Starts the server in the background; watch [`status`](Self::status) or
    /// [`subscribe`](Self::subscribe) to follow it.
    pub fn start(config: ServerConfig) -> Self {
        let shared = Arc::new(Shared::new(config));
        let (stop, stop_rx) = mpsc::channel(1);
        let supervisor = tokio::spawn(supervise(shared.clone(), stop_rx));
        let flusher = tokio::spawn(flush_loop(shared.clone()));
        Self {
            shared,
            stop,
            supervisor,
            flusher,
        }
    }

    pub fn config(&self) -> &ServerConfig {
        &self.shared.config
    }

    /// A new queue of [`ServerEvent`]s from now on.
    pub fn subscribe(&self) -> Result<Subscription, LspError> {
        lock(&self.shared.observed)
            .fanout
            .subscribe()
            .ok_or(LspError::Limit("event subscribers"))
    }

    pub fn status(&self) -> ServerStatus {
        self.shared.status.borrow().clone()
    }

    pub fn watch_status(&self) -> watch::Receiver<ServerStatus> {
        self.shared.status.subscribe()
    }

    /// Waits until the state satisfies `done` and returns that status.
    pub async fn wait_for(&self, done: impl FnMut(&ServerStatus) -> bool) -> ServerStatus {
        let mut status = self.shared.status.subscribe();
        // The sender lives in `self`, so the watch cannot close while we wait.
        let result = status.wait_for(done).await.map(|s| s.clone());
        result.unwrap_or_else(|_| self.status())
    }

    /// The server's process id while one runs.
    pub fn pid(&self) -> Option<u32> {
        lock(&self.shared.process)
            .as_ref()
            .and_then(ProcessHandle::pid)
    }

    /// What the running instance announced at `initialize`.
    pub fn capabilities(&self) -> Option<ServerCapabilities> {
        self.shared.current().map(|i| i.capabilities.clone())
    }

    pub fn encoding(&self) -> Option<PositionEncoding> {
        self.shared.current().map(|i| i.encoding)
    }

    pub fn diagnostics(&self, uri: &Uri) -> Option<FileDiagnostics> {
        lock(&self.shared.observed).diagnostics.get(uri).cloned()
    }

    /// Every file's diagnostics, and whether files were left out ([`MAX_DIAGNOSTIC_FILES`]).
    ///
    /// [`MAX_DIAGNOSTIC_FILES`]: crate::events::MAX_DIAGNOSTIC_FILES
    pub fn all_diagnostics(&self) -> (Vec<(Uri, FileDiagnostics)>, bool) {
        let observed = lock(&self.shared.observed);
        let files = observed
            .diagnostics
            .iter()
            .map(|(uri, file)| (uri.clone(), file.clone()))
            .collect();
        (files, observed.diagnostics.truncated())
    }

    /// The server's last log and show messages, oldest first.
    pub fn log(&self) -> Vec<LogEntry> {
        lock(&self.shared.observed).log.snapshot()
    }

    /// The last lines the running instance wrote to stderr.
    pub fn stderr_tail(&self) -> String {
        self.shared
            .current()
            .map(|i| i.connection.stderr_tail())
            .unwrap_or_default()
    }

    /// How full the running instance's queues are.
    pub fn queue_stats(&self) -> QueueStats {
        self.shared
            .current()
            .map(|i| i.connection.stats())
            .unwrap_or_default()
    }

    /// Opens a document (sent with the next flush, and again after every restart).
    pub fn open(
        &self,
        uri: Uri,
        language_id: &str,
        version: i32,
        text: Text,
    ) -> Result<(), LspError> {
        lock(&self.shared.docs).open(uri, language_id, version, text)?;
        self.shared.flush_wake.notify_one();
        Ok(())
    }

    /// Records an applied transaction of an open document: `text` is the text after it and
    /// `changes` its [`Transaction::changes`](hutan_buffer::Transaction::changes). Changes are
    /// coalesced until the next flush.
    pub fn change(
        &self,
        uri: &Uri,
        version: i32,
        text: Text,
        changes: &[Change],
    ) -> Result<(), LspError> {
        lock(&self.shared.docs).change(uri, version, text, changes)?;
        self.shared.flush_wake.notify_one();
        Ok(())
    }

    /// Replaces an open document's text without saying what changed (sent in full).
    pub fn replace(&self, uri: &Uri, version: i32, text: Text) -> Result<(), LspError> {
        lock(&self.shared.docs).replace(uri, version, text)?;
        self.shared.flush_wake.notify_one();
        Ok(())
    }

    /// The document was written to disk (`didSave` when the server wants it).
    pub fn save(&self, uri: &Uri) -> Result<(), LspError> {
        lock(&self.shared.docs).save(uri)?;
        self.shared.flush_wake.notify_one();
        Ok(())
    }

    pub fn close(&self, uri: &Uri) -> Result<(), LspError> {
        lock(&self.shared.docs).close(uri)?;
        self.shared.flush_wake.notify_one();
        Ok(())
    }

    /// Sends pending document changes now and waits until they are queued for the server.
    /// Cancel-safe: giving up waiting does not interrupt the flush.
    pub async fn flush(&self) {
        self.shared.flush_and_wait().await;
    }

    /// Sends a request after the pending document changes, within its timeout
    /// ([`Timeouts::request`] or [`Timeouts::interactive`]); on timeout it is cancelled.
    pub async fn request<R: TypedRequest>(&self, params: R::Params) -> Result<R::Result, LspError>
    where
        R::Result: DeserializeOwned,
    {
        let Some(instance) = self.shared.current() else {
            return Err(LspError::NotRunning {
                state: self.status().state,
            });
        };
        if !R::supported(&instance.capabilities) {
            return Err(LspError::Unsupported {
                method: R::METHOD.to_owned(),
            });
        }
        let timeouts = &self.shared.config.timeouts;
        let deadline = match R::LATENCY {
            Latency::Interactive => timeouts.interactive,
            Latency::Normal => timeouts.request,
        };
        let until = Instant::now() + deadline;
        timeout_at(until, self.shared.flush_and_wait())
            .await
            .map_err(|_| LspError::Timeout {
                method: R::METHOD.to_owned(),
            })?;
        instance
            .connection
            .request(
                R::METHOD,
                Some(&params),
                until.saturating_duration_since(Instant::now()),
            )
            .await
    }

    /// Stops the server (`shutdown`, `exit`, then its group is killed if it lingers) and
    /// returns once it is `stopped`.
    pub async fn stop(&self) {
        if self.stop.try_send(()).is_err() && self.supervisor.is_finished() {
            // The supervisor is done (`failed`): nothing runs; record the user's decision.
            self.shared.set_state(ServerState::Stopped);
        }
        self.wait_for(|s| s.state.is_final()).await;
        if self.status().state == ServerState::Failed && self.supervisor.is_finished() {
            self.shared.set_state(ServerState::Stopped);
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(process) = lock(&self.shared.process).take() {
            process.kill_now();
        }
        self.supervisor.abort();
        self.flusher.abort();
    }
}

/// How an instance's run ended.
enum Ended {
    Crashed(String),
    Stopped,
}

impl Shared {
    fn new(config: ServerConfig) -> Self {
        let (status, _) = watch::channel(ServerStatus {
            state: ServerState::Starting,
            pid: None,
            restarts: 0,
            last_error: None,
            encoding: None,
        });
        Self {
            config,
            status,
            observed: Mutex::default(),
            docs: Mutex::default(),
            instance: Mutex::default(),
            process: Mutex::default(),
            flush_wake: Notify::new(),
            flush_now: Notify::new(),
            flush_requested: AtomicU64::new(0),
            flushed: watch::channel(0).0,
        }
    }

    /// Asks the flusher to flush without waiting for the coalescing window; returns the
    /// number of this request.
    fn request_flush(&self) -> u64 {
        let ticket = self.flush_requested.fetch_add(1, Ordering::AcqRel) + 1;
        self.flush_now.notify_one();
        ticket
    }

    /// Flushes now and waits until everything changed before the call is queued.
    async fn flush_and_wait(&self) {
        let ticket = self.request_flush();
        let mut flushed = self.flushed.subscribe();
        // The sender lives in `self`, so the watch cannot close while we wait.
        let _ = flushed.wait_for(|done| *done >= ticket).await;
    }

    fn current(&self) -> Option<Arc<Instance>> {
        lock(&self.instance).clone()
    }

    fn emit(&self, event: ServerEvent) {
        lock(&self.observed).fanout.send(&event);
    }

    fn set_state(&self, state: ServerState) {
        let changed = self.status.send_if_modified(|status| {
            let changed = status.state != state;
            status.state = state;
            changed
        });
        if changed {
            self.emit(ServerEvent::State(state));
        }
    }

    /// `Ready` or `Indexing`, from the work-done progress in flight, while an instance runs.
    fn settle(&self) {
        let busy = !lock(&self.observed).progress.is_empty();
        let running = matches!(
            self.status.borrow().state,
            ServerState::Ready | ServerState::Indexing
        );
        if running {
            self.set_state(if busy {
                ServerState::Indexing
            } else {
                ServerState::Ready
            });
        }
    }

    fn initialize_params(&self) -> Value {
        let root = ls_types::Uri::from_file_path(&self.config.root);
        let folders = root.as_ref().map(|uri| {
            let name = self
                .config
                .root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.config.root.display().to_string());
            json!([{ "uri": uri, "name": name }])
        });
        let encodings: Vec<_> = PositionEncoding::OFFERED
            .iter()
            .map(|e| e.kind().as_str().to_owned())
            .collect();
        json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "Hutan", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root,
            "workspaceFolders": folders,
            "initializationOptions": self.config.initialization_options,
            "capabilities": {
                "general": { "positionEncodings": encodings },
                "window": { "workDoneProgress": true, "showDocument": { "support": false } },
                "workspace": {
                    "configuration": true,
                    "workspaceFolders": true,
                    "applyEdit": false,
                    "workspaceEdit": { "documentChanges": true },
                    "didChangeConfiguration": { "dynamicRegistration": false },
                },
                "textDocument": {
                    "synchronization": { "didSave": true, "willSave": false, "dynamicRegistration": false },
                    "publishDiagnostics": { "relatedInformation": true, "versionSupport": true, "codeDescriptionSupport": true },
                    "hover": { "contentFormat": ["markdown", "plaintext"] },
                    "definition": { "linkSupport": true },
                    "references": {},
                    "completion": {
                        "completionItem": { "snippetSupport": false, "documentationFormat": ["markdown", "plaintext"] },
                        "contextSupport": true,
                    },
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "formatting": {},
                    "rename": { "prepareSupport": true },
                    "codeAction": {
                        "codeActionLiteralSupport": {
                            "codeActionKind": { "valueSet": ["", "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source", "source.organizeImports"] }
                        },
                    },
                },
            },
        })
    }

    /// Spawns and initializes a new instance. On failure the process is killed and the error
    /// carries how it ended.
    async fn start_instance(
        self: &Arc<Self>,
    ) -> Result<(Arc<Instance>, oneshot::Receiver<ExitInfo>), (LspError, String)> {
        let generation = {
            let mut observed = lock(&self.observed);
            observed.generation += 1;
            observed.progress.clear();
            observed.generation
        };
        let spawned =
            process::spawn(&self.config.launch, self.config.timeouts.exit).map_err(|e| {
                let reason = e.to_string();
                (e, reason)
            })?;
        let handle = spawned.handle;
        let mut exit = spawned.exit;
        *lock(&self.process) = Some(handle.clone());
        self.status.send_modify(|status| status.pid = handle.pid());
        let connection = Connection::start(
            spawned.stdin,
            spawned.stdout,
            spawned.stderr,
            LspCodec::new(self.config.max_frame),
            self.clone(),
            generation,
        );
        let initialized = async {
            let result: InitializeResult = connection
                .request(
                    "initialize",
                    Some(&self.initialize_params()),
                    self.config.timeouts.initialize,
                )
                .await?;
            let encoding =
                PositionEncoding::negotiate(result.capabilities.position_encoding.as_ref())
                    .map_err(LspError::Encoding)?;
            connection.notify("initialized", Some(&json!({}))).await?;
            Ok::<_, LspError>((result.capabilities, encoding))
        };
        match initialized.await {
            Ok((capabilities, encoding)) => Ok((
                Arc::new(Instance {
                    generation,
                    connection,
                    process: handle,
                    sync: SyncOptions::from_capabilities(&capabilities),
                    capabilities,
                    encoding,
                }),
                exit,
            )),
            Err(error) => {
                handle.kill_now();
                let ended = timeout(self.config.timeouts.exit, &mut exit).await;
                *lock(&self.process) = None;
                let reason = match ended {
                    Ok(Ok(info)) => format!("{error} ({})", info.describe()),
                    _ => error.to_string(),
                };
                Err((error, reason))
            }
        }
    }

    /// Makes `instance` the one requests and documents go to and opens every document in it.
    fn install(&self, instance: Arc<Instance>) {
        lock(&self.docs).reset(instance.generation);
        let encoding = instance.encoding;
        *lock(&self.instance) = Some(instance);
        self.status.send_modify(|status| {
            status.encoding = Some(encoding);
            // `settle` only moves between the running states.
            status.state = ServerState::Ready;
        });
        self.emit(ServerEvent::State(ServerState::Ready));
        self.settle();
        // Open every document in the new instance.
        self.request_flush();
    }

    /// Forgets the instance that ended: its diagnostics and progress go with it.
    fn uninstall(&self) {
        *lock(&self.instance) = None;
        *lock(&self.process) = None;
        let cleared = {
            let mut observed = lock(&self.observed);
            observed.progress.clear();
            observed.diagnostics.clear()
        };
        for uri in cleared {
            self.emit(ServerEvent::Diagnostics { uri, version: None });
        }
        self.status.send_modify(|status| {
            status.pid = None;
            status.encoding = None;
        });
    }

    /// Sends what changed in the documents to the running instance. Only the flusher task
    /// calls this.
    async fn flush(&self) -> Result<(), LspError> {
        let Some(instance) = self.current() else {
            // Nothing runs: documents are opened with their current text at the next start.
            return Ok(());
        };
        // Empty when the instance was replaced since `current` (the documents were reset for
        // the new one).
        let messages =
            lock(&self.docs).flush(instance.generation, instance.sync, instance.encoding);
        let connection = &instance.connection;
        for message in messages {
            match message {
                Outgoing::Open(p) => {
                    connection
                        .notify(DidOpenTextDocument::METHOD, Some(&p))
                        .await?
                }
                Outgoing::Change(p) => {
                    connection
                        .notify(DidChangeTextDocument::METHOD, Some(&p))
                        .await?
                }
                Outgoing::Save(p) => {
                    connection
                        .notify(DidSaveTextDocument::METHOD, Some(&p))
                        .await?
                }
                Outgoing::Close(p) => {
                    connection
                        .notify(DidCloseTextDocument::METHOD, Some(&p))
                        .await?
                }
            }
        }
        Ok(())
    }

    /// `shutdown`, `exit`, stdin closed; the reaper kills the group if it is still there after
    /// the exit grace.
    async fn shut_down(&self, instance: &Instance, exit: oneshot::Receiver<ExitInfo>) {
        let connection = &instance.connection;
        let timeouts = self.config.timeouts;
        if let Err(error) = connection
            .request::<(), Value>("shutdown", None, timeouts.shutdown)
            .await
        {
            debug!(%error, "shutdown was not answered; exiting anyway");
        }
        if timeout(timeouts.shutdown, connection.notify::<()>("exit", None))
            .await
            .is_err()
        {
            debug!("exit could not be sent");
        }
        let _ = timeout(timeouts.shutdown, connection.close_input()).await;
        instance.process.end();
        // The reaper gives up waiting after the exit grace and kills the group.
        let _ = exit.await;
    }

    fn settings_for(&self, section: Option<&str>) -> Value {
        let mut value = &self.config.settings;
        if let Some(section) = section {
            for key in section.split('.') {
                match value.get(key) {
                    Some(inner) => value = inner,
                    None => return Value::Null,
                }
            }
        }
        value.clone()
    }
}

fn parse<T: DeserializeOwned>(method: &str, params: Option<&RawValue>) -> Result<T, LspError> {
    let raw = params.map_or("null", RawValue::get);
    serde_json::from_str(raw).map_err(|source| LspError::Malformed {
        method: method.to_owned(),
        source,
    })
}

impl Handler for Shared {
    fn notification(&self, generation: u64, method: &str, params: Option<&RawValue>) {
        if lock(&self.observed).generation != generation {
            return;
        }
        let handled = match method {
            "textDocument/publishDiagnostics" => parse::<PublishDiagnosticsParams>(method, params)
                .map(|p| {
                    let stored = lock(&self.observed).diagnostics.publish(
                        p.uri.clone(),
                        p.version,
                        p.diagnostics,
                    );
                    if stored {
                        self.emit(ServerEvent::Diagnostics {
                            uri: p.uri,
                            version: p.version,
                        });
                    }
                }),
            "$/progress" => parse::<ProgressParams>(method, params).map(|p| self.progress(p)),
            "window/logMessage" | "window/showMessage" => parse::<LogMessageParams>(method, params)
                .map(|p| {
                    let entry = lock(&self.observed).log.push(p.typ, p.message);
                    self.emit(ServerEvent::Log(entry));
                }),
            _ => {
                debug!(method, "notification ignored");
                Ok(())
            }
        };
        if let Err(error) = handled {
            warn!(%error, "language server notification dropped");
        }
    }

    fn request(
        &self,
        _generation: u64,
        method: &str,
        params: Option<&RawValue>,
    ) -> Result<Value, (i64, String)> {
        let invalid = |e: LspError| (INVALID_PARAMS, e.to_string());
        match method {
            "workspace/configuration" => {
                let params: ConfigurationParams = parse(method, params).map_err(invalid)?;
                if params.items.len() > MAX_CONFIGURATION_ITEMS {
                    return Err((INVALID_PARAMS, "too many configuration items".to_owned()));
                }
                Ok(Value::Array(
                    params
                        .items
                        .iter()
                        .map(|item| self.settings_for(item.section.as_deref()))
                        .collect(),
                ))
            }
            // Progress is shown from `$/progress`; registrations are accepted and not acted on
            // (the client announced no dynamic registration).
            "window/workDoneProgress/create"
            | "client/registerCapability"
            | "client/unregisterCapability"
            | "window/showMessageRequest" => Ok(Value::Null),
            "workspace/workspaceFolders" => {
                let root = ls_types::Uri::from_file_path(&self.config.root);
                Ok(json!(root.map(|uri| vec![json!({
                    "uri": uri,
                    "name": self.config.name,
                })])))
            }
            // The client never writes files on a server's behalf.
            "workspace/applyEdit" => Ok(json!({
                "applied": false,
                "failureReason": "Hutan does not apply edits requested by a language server",
            })),
            "window/showDocument" => Ok(json!({ "success": false })),
            m if m.starts_with("workspace/") && m.ends_with("/refresh") => Ok(Value::Null),
            _ => Err((METHOD_NOT_FOUND, format!("unhandled method {method}"))),
        }
    }
}

impl Shared {
    fn progress(&self, params: ProgressParams) {
        let ProgressParamsValue::WorkDone(progress) = params.value;
        let token = params.token;
        let (phase, title, message, percentage) = match progress {
            WorkDoneProgress::Begin(begin) => {
                let mut observed = lock(&self.observed);
                if observed.progress.len() < MAX_PROGRESS_TOKENS {
                    observed.progress.insert(token.clone());
                }
                (
                    ProgressPhase::Begin,
                    Some(begin.title),
                    begin.message,
                    begin.percentage,
                )
            }
            WorkDoneProgress::Report(report) => (
                ProgressPhase::Report,
                None,
                report.message,
                report.percentage,
            ),
            WorkDoneProgress::End(end) => {
                lock(&self.observed).progress.remove(&token);
                (ProgressPhase::End, None, end.message, None)
            }
        };
        self.emit(ServerEvent::Progress {
            token,
            phase,
            title: title.map(|t| clip(t, MAX_MESSAGE_BYTES)),
            message: message.map(|m| clip(m, MAX_MESSAGE_BYTES)),
            percentage,
        });
        self.settle();
    }
}

/// Sends document notifications: after the coalescing window that the first change opens, or
/// at once when a flush is requested (a request, [`Server::flush`], a new instance).
async fn flush_loop(shared: Arc<Shared>) {
    loop {
        tokio::select! {
            () = shared.flush_wake.notified() => {
                tokio::select! {
                    () = sleep(shared.config.coalesce) => {}
                    () = shared.flush_now.notified() => {}
                }
            }
            () = shared.flush_now.notified() => {}
        }
        // Every flush asked for until now is covered by this one.
        let covered = shared.flush_requested.load(Ordering::Acquire);
        if let Err(error) = shared.flush().await {
            debug!(%error, "document changes not sent");
        }
        shared
            .flushed
            .send_modify(|done| *done = (*done).max(covered));
    }
}

async fn supervise(shared: Arc<Shared>, mut stop: mpsc::Receiver<()>) {
    let policy = shared.config.restart;
    let mut failures: VecDeque<Instant> = VecDeque::new();
    let mut restarts = 0u32;
    loop {
        shared.set_state(ServerState::Starting);
        let started = tokio::select! {
            started = shared.start_instance() => started,
            _ = stop.recv() => {
                // Dropping the start kills the half-started process.
                if let Some(process) = lock(&shared.process).take() {
                    process.kill_now();
                }
                shared.set_state(ServerState::Stopped);
                return;
            }
        };
        let reason = match started {
            Ok((instance, mut exit)) => {
                info!(server = %shared.config.name, pid = ?instance.process.pid(), "language server ready");
                shared.install(instance.clone());
                let ended = tokio::select! {
                    info = &mut exit => Ended::Crashed(info.map(|i| i.describe()).unwrap_or_default()),
                    () = instance.connection.closed() => {
                        instance.process.end();
                        let info = exit.await.map(|i| i.describe()).unwrap_or_default();
                        Ended::Crashed(format!("closed its output, {info}"))
                    }
                    _ = stop.recv() => {
                        shared.shut_down(&instance, exit).await;
                        Ended::Stopped
                    }
                };
                shared.uninstall();
                match ended {
                    Ended::Stopped => {
                        shared.set_state(ServerState::Stopped);
                        return;
                    }
                    Ended::Crashed(reason) => reason,
                }
            }
            Err((LspError::Spawn { .. }, reason)) => {
                warn!(server = %shared.config.name, %reason, "language server cannot start");
                shared
                    .status
                    .send_modify(|status| status.last_error = Some(reason));
                shared.set_state(ServerState::Failed);
                return;
            }
            Err((_, reason)) => {
                shared.uninstall();
                reason
            }
        };
        warn!(server = %shared.config.name, %reason, "language server ended");
        let now = Instant::now();
        failures.push_back(now);
        while failures
            .front()
            .is_some_and(|t| now.duration_since(*t) > policy.window)
        {
            failures.pop_front();
        }
        let recent = u32::try_from(failures.len()).unwrap_or(u32::MAX);
        shared
            .status
            .send_modify(|status| status.last_error = Some(reason.clone()));
        if recent >= policy.max_failures {
            shared.set_state(ServerState::Failed);
            return;
        }
        restarts += 1;
        let delay = policy.backoff(recent);
        shared
            .status
            .send_modify(|status| status.restarts = restarts);
        shared.set_state(ServerState::Starting);
        shared.emit(ServerEvent::Restarting {
            attempt: restarts,
            delay,
            reason,
        });
        tokio::select! {
            () = sleep(delay) => {}
            _ = stop.recv() => {
                shared.set_state(ServerState::Stopped);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let policy = RestartPolicy {
            max_failures: 10,
            window: Duration::from_secs(60),
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(500),
        };
        let delays: Vec<_> = (1..=5).map(|n| policy.backoff(n).as_millis()).collect();
        assert_eq!(delays, [100, 200, 400, 500, 500]);
    }

    fn shared() -> Shared {
        let mut config = ServerConfig::new(
            "fake",
            Launch {
                program: "fake".into(),
                args: Vec::new(),
                cwd: None,
                env: Vec::new(),
            },
            "/tmp",
        );
        config.settings = json!({"rust-analyzer": {"cargo": {"features": "all"}}});
        Shared::new(config)
    }

    #[test]
    fn initialize_params_offer_utf8_first_and_parse_as_lsp() {
        let shared = shared();
        let params: ls_types::InitializeParams =
            serde_json::from_value(shared.initialize_params()).unwrap();
        let general = params.capabilities.general.unwrap();
        assert_eq!(
            general.position_encodings.unwrap(),
            [
                ls_types::PositionEncodingKind::UTF8,
                ls_types::PositionEncodingKind::UTF16
            ]
        );
        assert_eq!(
            params.workspace_folders.unwrap()[0].uri.as_str(),
            "file:///tmp"
        );
    }

    #[test]
    fn server_requests_get_minimal_answers() {
        let shared = shared();
        let ask = |method: &str, params: Value| {
            let raw = serde_json::value::to_raw_value(&params).unwrap();
            Handler::request(&shared, 1, method, Some(&raw))
        };
        assert_eq!(
            ask(
                "workspace/configuration",
                json!({"items": [{"section": "rust-analyzer.cargo"}, {"section": "rust-analyzer.missing"}, {}]})
            ),
            Ok(json!([
                {"features": "all"},
                null,
                {"rust-analyzer": {"cargo": {"features": "all"}}}
            ]))
        );
        assert_eq!(
            ask(
                "client/registerCapability",
                json!({"registrations": [{"id": "1", "method": "workspace/didChangeWatchedFiles"}]})
            ),
            Ok(Value::Null)
        );
        assert_eq!(
            ask("window/workDoneProgress/create", json!({"token": 1})),
            Ok(Value::Null)
        );
        assert_eq!(
            ask("workspace/applyEdit", json!({"edit": {}}))
                .unwrap()
                .get("applied"),
            Some(&json!(false))
        );
        assert_eq!(
            ask("workspace/inlayHint/refresh", Value::Null),
            Ok(Value::Null)
        );
        assert!(matches!(
            ask("workspace/configuration", json!({"items": 3})),
            Err((INVALID_PARAMS, _))
        ));
        assert!(matches!(
            ask("custom/thing", json!({})),
            Err((METHOD_NOT_FOUND, _))
        ));
    }
}
