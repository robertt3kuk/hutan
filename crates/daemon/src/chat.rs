//! Chat sessions (protocol 0.9 agents, 0.11 direct model chat, 0.17 ACP harnesses): one adapter
//! process per Codex or harness (`omp`, `claude`) session or one [`DirectSession`] per direct
//! chat, the approval engine, and the journal.
//!
//! The registry owns the sessions' records and lifecycle; [`ChatManager`] owns what runs: the
//! adapters, a pump task per session that numbers, journals and publishes every event, and the
//! pending approvals. Hutan's journal (`chat_items`) is the source of a conversation's
//! history; the provider's own copy is only used to continue the agent (ADR 0005).
//!
//! Approvals live here, in daemon state: they survive client disconnects, are answered only
//! with an option the agent offered, expire when their turn ends (or the agent goes away),
//! and every request and decision is written to the `approvals` table. A decision says who made
//! it (`decided_by`, protocol 0.15): the answering client's kind, journaled with the event.
//!
//! Streaming text is journaled as coalesced snapshots of the open message ([`crate::partial`]),
//! so a client that joins mid-stream starts from the text so far.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use hutan_agents::{
    AcpAdapter, AcpSession, AdapterMessage, AgentAdapter, AgentError, CodexAppServerAdapter,
    CodexConfig, CodexSession, ExitInfo, ImageBlock, MessageExtras, StartSpec, Started,
};
use hutan_core::agent::{DecidedBy, PermissionOutcome};
use hutan_core::question::{self, QuestionResponse};
use hutan_core::{
    AgentEvent, AgentEventDraft, AgentEventKind, ApprovalId, ApprovalPolicy, ItemId, ModelInfo,
    QuestionId, QuestionOutcome, SandboxMode, SessionId, TurnId,
};
use hutan_protocol::assistant::{ATTACHMENTS_META_KEY, AttachmentMeta};
use hutan_protocol::chat::{
    ApprovalInfo, ApprovalList, ChatHistory, ChatHistoryRequest, ChatModels, HISTORY_MAX_LIMIT,
    QuestionInfo, QuestionList, SEND_MAX_BYTES,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{ClientKind, Event};
use hutan_storage::{
    ApprovalOutcome, ApprovalRow, ChatItemRow, ChatTurnRow, ChatTurnStatus,
    QuestionOutcome as StoredOutcome, QuestionRow, Storage,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::bus::EventBus;
use crate::context::Attached;
use crate::direct::{DirectSession, Target};
use crate::harness::{Harnesses, is_harness_agent};
use crate::mcp_wiring::{McpServerSpec, McpWiring};
use crate::partial::{Flush, PARTIAL_INTERVAL, Partials};
use crate::providers::ProviderService;
use crate::registry::{self, ChatNotifier};

/// The Codex app-server agent.
pub const AGENT_CODEX: &str = "codex";
/// Names of the temporary files that hold clipboard images for Codex (`<home>/tmp/`).
const TEMP_IMAGE_PREFIX: &str = "img-";
/// The `error` of a turn row that a daemon shutdown cancelled; the next start tells such a turn
/// from one the user cancelled (`session.recovery`, `last_turn: interrupted`).
pub const DAEMON_STOP_TURN_ERROR: &str = "hutand stopped during the turn";
/// The `decided_by` of an approval row that a daemon shutdown expired.
pub const DAEMON_STOP_DECIDER: &str = "daemon_stop";
/// The model gateway: no agent, a provider's chat API.
pub use hutan_protocol::assistant::AGENT_ASSISTANT;
pub use hutan_protocol::provider::AGENT_DIRECT;

/// Whether sessions of this agent are model gateway chats: a provider's API, no process.
pub fn is_gateway_agent(agent: &str) -> bool {
    agent == AGENT_DIRECT || agent == AGENT_ASSISTANT
}

/// How long a fetched model catalog is reused.
const MODEL_CACHE: Duration = Duration::from_secs(60);
/// How long a harness's model list is reused (`omp models` is a subprocess of seconds).
const HARNESS_MODEL_CACHE: Duration = Duration::from_secs(5 * 60);

/// What a chat session is, as stored in the session row's `spec_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSpec {
    pub agent: String,
    /// The model asked for; `None` takes the agent's catalog default.
    #[serde(default)]
    pub model: Option<String>,
    pub approval_policy: ApprovalPolicy,
    pub sandbox: SandboxMode,
    /// The provider's conversation, once there is one: what a restart or a daemon restart
    /// resumes.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// `direct` only: the provider the conversation talks to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// `direct` only: sent to the model ahead of the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Harness chats only: the session mode asked for at creation (applied again on restore).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Harness and Codex chats only (protocol 0.20): the session is an assistant, started with
    /// its tools restricted (`Harnesses::acp_config`, `AssistantService`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub assistant: bool,
}

/// What runs a session: an agent process, or the model gateway.
#[derive(Debug, Clone)]
enum Backend {
    Agent(Arc<dyn AgentAdapter>),
    Direct(Arc<DirectSession>),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded data is never left half-updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Pending approvals that are not an agent's tool permissions (the control gate's, protocol
/// 0.21), offered through the same `approval.*` requests, notification intents and events.
pub trait ApprovalSource: Send + Sync + std::fmt::Debug {
    /// The pending approvals, optionally of one session.
    fn list(&self, session: Option<SessionId>) -> Vec<ApprovalInfo>;
    /// Answers `approval` when it is the source's: `None` when it is not.
    fn respond(
        &self,
        approval: ApprovalId,
        option_id: String,
        client: Option<ClientKind>,
    ) -> Option<Result<(), ErrorBody>>;
}

#[derive(Debug)]
struct Live {
    id: SessionId,
    backend: Backend,
    /// The next event's `seq`.
    next_seq: AtomicU64,
    /// A turn is in progress, or `chat.send` is starting one.
    busy: AtomicBool,
    /// The turn the journal shows as open.
    open_turn: Mutex<Option<TurnId>>,
    /// The user (or a daemon shutdown) asked to end the session.
    stopping: AtomicBool,
    /// The stop is the daemon's own shutdown, not the user's: the session is meant to come
    /// back with the next daemon, and what the shutdown cut off is marked as such (see
    /// [`DAEMON_STOP_TURN_ERROR`]).
    daemon_stop: AtomicBool,
    /// The open messages and reasoning blocks of the running turn. Numbering an event and
    /// folding it in happen under this lock, so a snapshot is exact (see [`crate::partial`]).
    partials: Mutex<Partials>,
    /// What the message being sent carries, until its turn ends.
    delivery: Mutex<Delivery>,
}

/// Attachments of the message a turn started with.
#[derive(Debug, Default)]
struct Delivery {
    /// Their meta, for the journaled user message (`attachments_meta`); taken by the first user
    /// `message_end` of the turn.
    meta: Option<Vec<AttachmentMeta>>,
    /// Temporary files holding clipboard images for the agent; removed when the turn ends.
    temp_files: Vec<PathBuf>,
}

#[derive(Debug)]
struct Pending {
    info: ApprovalInfo,
    decided_by: Option<&'static str>,
}

#[derive(Debug)]
struct PendingQuestion {
    info: QuestionInfo,
    decided_by: Option<&'static str>,
}

/// Runs the chat sessions of one daemon.
#[derive(Debug)]
pub struct ChatManager {
    bus: Arc<EventBus>,
    storage: Storage,
    notifier: ChatNotifier,
    codex: CodexConfig,
    harnesses: Arc<Harnesses>,
    providers: Arc<ProviderService>,
    max_context_chars: Arc<AtomicUsize>,
    live: Mutex<HashMap<SessionId, Arc<Live>>>,
    approvals: Mutex<HashMap<ApprovalId, Pending>>,
    questions: Mutex<HashMap<QuestionId, PendingQuestion>>,
    /// Model catalogs by agent.
    models: Mutex<HashMap<String, (Instant, Vec<ModelInfo>)>>,
    external: Mutex<Option<Weak<dyn ApprovalSource>>>,
    mcp: OnceLock<McpWiring>,
    /// `<home>/tmp`: where clipboard images wait for an agent that reads images from disk.
    tmp_dir: PathBuf,
}

/// Why a subagent thread could not be interrupted.
#[derive(Debug)]
pub enum ThreadInterruptError {
    /// The harness cannot stop a single subagent (direct chats, and agents whose protocol has
    /// no per-thread interrupt).
    Unsupported,
    /// The thread has no turn in progress.
    NoTurn,
    Failed(ErrorBody),
}

fn provider_error(error: &AgentError, program: &std::path::Path) -> ErrorBody {
    let hint = match error {
        AgentError::Spawn { .. } => format!(
            "; install Codex or set `bin` under `[codex]` in daemon.toml (looked for `{}`)",
            program.display()
        ),
        _ => String::new(),
    };
    ErrorBody::new(ErrorCode::ProviderError, format!("{error}{hint}"))
}

/// What a failed harness start or call says to the client.
fn harness_error(error: &AgentError, agent: &str, program: &std::path::Path) -> ErrorBody {
    match error {
        AgentError::Spawn { .. } => ErrorBody::new(
            ErrorCode::ProviderError,
            format!(
                "{error}; check `bin` under `[harness.{agent}]` in daemon.toml (looked for `{}`)",
                program.display()
            ),
        ),
        AgentError::AuthRequired { .. } => ErrorBody::new(
            ErrorCode::ProviderError,
            format!("{error}: run `hutan account login {agent}`"),
        ),
        AgentError::UnknownModel { .. } | AgentError::Unsupported(_) => {
            ErrorBody::new(ErrorCode::BadRequest, error.to_string())
        }
        _ => ErrorBody::new(ErrorCode::ProviderError, error.to_string()),
    }
}

fn client_name(kind: Option<ClientKind>) -> &'static str {
    match kind {
        Some(ClientKind::Desktop) => "desktop",
        Some(ClientKind::Cli) => "cli",
        Some(ClientKind::Notifier) => "notifier",
        Some(ClientKind::Other) | None => "other",
    }
}

fn enum_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

impl ChatManager {
    #[allow(clippy::too_many_arguments)] // The wiring of one manager, built once at start.
    pub(crate) fn new(
        bus: Arc<EventBus>,
        storage: Storage,
        notifier: ChatNotifier,
        codex: CodexConfig,
        harnesses: Arc<Harnesses>,
        providers: Arc<ProviderService>,
        max_context_chars: usize,
        tmp_dir: PathBuf,
    ) -> Arc<Self> {
        // A daemon that was killed mid-turn left its images behind: nothing is in flight yet.
        let stale = tmp_dir.clone();
        tokio::task::spawn_blocking(move || {
            let Ok(entries) = std::fs::read_dir(&stale) else {
                return;
            };
            for entry in entries.filter_map(Result::ok) {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(TEMP_IMAGE_PREFIX)
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        });
        Arc::new(Self {
            bus,
            storage,
            notifier,
            codex,
            harnesses,
            providers,
            max_context_chars: Arc::new(AtomicUsize::new(max_context_chars)),
            live: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            questions: Mutex::new(HashMap::new()),
            models: Mutex::new(HashMap::new()),
            external: Mutex::new(None),
            mcp: OnceLock::new(),
            tmp_dir,
        })
    }

    /// The model providers of this daemon.
    pub(crate) fn providers(&self) -> &Arc<ProviderService> {
        &self.providers
    }

    /// `gateway.max_context_chars`, now.
    pub(crate) fn max_context_chars(&self) -> usize {
        self.max_context_chars.load(Ordering::Relaxed)
    }

    /// `gateway.max_context_chars` from now on, for direct chats that already run too.
    pub(crate) fn set_max_context_chars(&self, chars: usize) {
        self.max_context_chars.store(chars, Ordering::Relaxed);
    }

    /// The pending approvals of another service (the control gate, protocol 0.21) that
    /// `approval.list` and `approval.respond` serve next to the agents' own. Weak: the service
    /// owns the registry that owns this manager.
    pub(crate) fn set_external_approvals(&self, source: Weak<dyn ApprovalSource>) {
        *lock(&self.external) = Some(source);
    }

    fn external_approvals(&self) -> Option<Arc<dyn ApprovalSource>> {
        lock(&self.external).as_ref().and_then(Weak::upgrade)
    }

    /// How `hutan mcp` is described to the agents of this daemon (protocol 0.21): set once at
    /// start from `[control] mcp`.
    pub fn set_mcp_wiring(&self, wiring: McpWiring) {
        let _ = self.mcp.set(wiring);
    }

    /// The MCP server to give the agent of `session`: `None` when `[control] mcp = false`.
    /// Codex gets it through [`Self::codex_for`], the ACP harnesses (OMP, Claude Code) through
    /// [`Self::start_harness`] as [`McpServerSpec::acp_server`]. The session id is passed
    /// explicitly so the server's requests are attributed to that session (a chat session is
    /// always an agent's, `control::is_agent_session`).
    pub fn mcp_server(&self, session: SessionId) -> Option<McpServerSpec> {
        self.mcp.get().and_then(|wiring| wiring.server_for(session))
    }

    /// The Codex command line for the agent of `session`, with `hutan mcp` registered.
    async fn codex_for(&self, session: SessionId) -> CodexConfig {
        let mut codex = self.resolved_codex().await;
        if let Some(server) = self.mcp_server(session) {
            codex.args.extend(server.codex_overrides());
        }
        codex
    }

    /// `[codex]` with its program found the way the harnesses are (`PATH` of the login
    /// environment, then the usual install folders) and that environment underneath the
    /// configured one. When it is not found the configuration stays as it is and starting it
    /// reports the error.
    async fn resolved_codex(&self) -> CodexConfig {
        let mut codex = self.codex.clone();
        let harnesses = self.harnesses.clone();
        let program = codex.program.clone();
        let found = tokio::task::spawn_blocking(move || harnesses.cli(AGENT_CODEX, &program))
            .await
            .ok()
            .and_then(Result::ok);
        if let Some((program, env)) = found {
            codex.program = program;
            let configured: std::collections::HashSet<String> =
                codex.env.iter().map(|(k, _)| k.clone()).collect();
            let mut merged: Vec<(String, String)> = env
                .into_iter()
                .filter(|(k, _)| !configured.contains(k))
                .collect();
            merged.append(&mut codex.env);
            codex.env = merged;
        }
        codex
    }

    /// Whether the session has a running backend (not merely a record).
    pub(crate) fn is_running(&self, id: SessionId) -> bool {
        lock(&self.live).contains_key(&id)
    }

    /// The highest journaled `seq` of a session.
    async fn journal_end(&self, id: SessionId) -> Result<u64, ErrorBody> {
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || storage.chat().last_seq(id))
            .await
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))?
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))
    }

    /// Starts (or resumes, when `spec.thread_id` is set) the agent of a session and begins
    /// journaling its events.
    pub async fn start(
        self: &Arc<Self>,
        id: SessionId,
        cwd: PathBuf,
        spec: &ChatSpec,
    ) -> Result<Started, ErrorBody> {
        if is_gateway_agent(&spec.agent) {
            return self.start_direct(id, spec).await;
        }
        if is_harness_agent(&spec.agent) {
            return self.start_harness(id, cwd, spec).await;
        }
        if spec.agent != AGENT_CODEX {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "unsupported agent `{}` (supported: {AGENT_CODEX}, omp, claude, {AGENT_DIRECT}, {AGENT_ASSISTANT})",
                    spec.agent
                ),
            ));
        }
        let last = self.journal_end(id).await?;
        let start = StartSpec {
            cwd,
            model: spec.model.clone(),
            approval_policy: spec.approval_policy,
            sandbox: spec.sandbox,
            resume_thread_id: spec.thread_id.clone(),
        };
        let codex = self.codex_for(id).await;
        let CodexSession {
            adapter,
            started,
            events,
        } = CodexAppServerAdapter::start(&codex, &start)
            .await
            .map_err(|e| provider_error(&e, &codex.program))?;
        let live = Arc::new(Live {
            id,
            backend: Backend::Agent(adapter),
            next_seq: AtomicU64::new(last + 1),
            busy: AtomicBool::new(false),
            open_turn: Mutex::new(None),
            stopping: AtomicBool::new(false),
            daemon_stop: AtomicBool::new(false),
            partials: Mutex::new(Partials::default()),
            delivery: Mutex::new(Delivery::default()),
        });
        lock(&self.live).insert(id, live.clone());
        info!(session = %id, model = %started.model, resumed = started.resumed, "chat agent started");
        tokio::spawn(self.clone().pump(live, events));
        Ok(started)
    }

    /// Starts (or, with `spec.thread_id`, continues with `session/load`) the ACP process of a
    /// harness chat and begins journaling its events.
    async fn start_harness(
        self: &Arc<Self>,
        id: SessionId,
        cwd: PathBuf,
        spec: &ChatSpec,
    ) -> Result<Started, ErrorBody> {
        let harnesses = self.harnesses.clone();
        let agent = spec.agent.clone();
        let assistant = spec.assistant;
        let config = tokio::task::spawn_blocking(move || harnesses.acp_config(&agent, assistant))
            .await
            .map_err(|e| {
                ErrorBody::new(ErrorCode::Internal, format!("locating the agent: {e}"))
            })??;
        let last = self.journal_end(id).await?;
        let start = StartSpec {
            cwd,
            model: spec.model.clone(),
            approval_policy: spec.approval_policy,
            sandbox: spec.sandbox,
            resume_thread_id: spec.thread_id.clone(),
        };
        // The user's `[mcp.*]` servers, and Hutan's own (`[control] mcp`), which carries this
        // session's id so that its requests are attributed to it.
        let mut servers = self.harnesses.mcp_servers().to_vec();
        servers.extend(self.mcp_server(id).map(|server| server.acp_server()));
        let AcpSession {
            adapter,
            started,
            events,
            models,
        } = AcpAdapter::start(&config, &start, spec.mode.as_deref(), &servers)
            .await
            .map_err(|e| harness_error(&e, &spec.agent, &config.program))?;
        if !models.is_empty() {
            lock(&self.models).insert(spec.agent.clone(), (Instant::now(), models));
        }
        let live = Arc::new(Live {
            id,
            backend: Backend::Agent(adapter),
            next_seq: AtomicU64::new(last + 1),
            busy: AtomicBool::new(false),
            open_turn: Mutex::new(None),
            stopping: AtomicBool::new(false),
            daemon_stop: AtomicBool::new(false),
            partials: Mutex::new(Partials::default()),
            delivery: Mutex::new(Delivery::default()),
        });
        lock(&self.live).insert(id, live.clone());
        info!(session = %id, agent = %spec.agent, model = %started.model, resumed = started.resumed, "chat harness started");
        tokio::spawn(self.clone().pump(live, events));
        Ok(started)
    }

    /// Opens a direct chat: no process, just the session's target and its journal.
    async fn start_direct(
        self: &Arc<Self>,
        id: SessionId,
        spec: &ChatSpec,
    ) -> Result<Started, ErrorBody> {
        let (Some(provider_id), Some(model)) = (spec.provider_id.clone(), spec.model.clone())
        else {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "a direct chat needs a provider and a model",
            ));
        };
        let last = self.journal_end(id).await?;
        // The conversation's running token total continues from the newest `usage` event.
        let storage = self.storage.clone();
        let usage =
            tokio::task::spawn_blocking(move || storage.chat().items_of_kind(id, "usage", 1))
                .await
                .map_err(|e| {
                    ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}"))
                })?
                .map_err(|e| {
                    ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}"))
                })?
                .last()
                .and_then(|row| serde_json::from_str::<AgentEvent>(&row.event_json).ok())
                .and_then(|event| match event.kind {
                    AgentEventKind::Usage { total, .. } => total,
                    _ => None,
                })
                .unwrap_or_default();
        let (tx, events) = mpsc::channel(256);
        let session = DirectSession::new(
            id,
            self.storage.clone(),
            self.providers.clone(),
            tx,
            self.max_context_chars.clone(),
            Target {
                provider_id: provider_id.clone(),
                model: model.clone(),
                system_prompt: spec.system_prompt.clone(),
            },
            usage,
        );
        let live = Arc::new(Live {
            id,
            backend: Backend::Direct(session),
            next_seq: AtomicU64::new(last + 1),
            busy: AtomicBool::new(false),
            open_turn: Mutex::new(None),
            stopping: AtomicBool::new(false),
            daemon_stop: AtomicBool::new(false),
            partials: Mutex::new(Partials::default()),
            delivery: Mutex::new(Delivery::default()),
        });
        lock(&self.live).insert(id, live.clone());
        info!(session = %id, provider = %provider_id, %model, "direct chat started");
        tokio::spawn(self.clone().pump(live, events));
        Ok(Started {
            model,
            // There is no provider-side conversation: the session is its own thread.
            thread_id: id.to_string(),
            resumed: spec.thread_id.is_some(),
            // The provider's model list answers when an image is attached.
            images: None,
        })
    }

    /// Switches the model of a running chat; its next turn uses it. A direct chat takes a
    /// provider too; a harness chat takes one of the models its agent offers.
    pub(crate) async fn set_model(
        &self,
        id: SessionId,
        provider_id: String,
        model: String,
    ) -> Result<(), ErrorBody> {
        let live = self.live(id)?;
        match &live.backend {
            Backend::Direct(session) => {
                session.set_model(provider_id, model).await;
                Ok(())
            }
            Backend::Agent(adapter) => {
                adapter.set_model(model).await.map_err(|error| match error {
                    AgentError::Unsupported(_) | AgentError::UnknownModel { .. } => {
                        ErrorBody::new(ErrorCode::BadRequest, error.to_string())
                    }
                    error => ErrorBody::new(ErrorCode::ProviderError, error.to_string()),
                })
            }
        }
    }

    fn live(&self, id: SessionId) -> Result<Arc<Live>, ErrorBody> {
        lock(&self.live).get(&id).cloned().ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::InvalidState,
                format!("chat session {id} is not running"),
            )
        })
    }

    /// Asks the session's agent to end; the pump reports the exit.
    pub fn stop(&self, id: SessionId) {
        let Some(live) = lock(&self.live).get(&id).cloned() else {
            return;
        };
        live.stopping.store(true, Ordering::Release);
        tokio::spawn(async move {
            match &live.backend {
                Backend::Agent(adapter) => adapter.shutdown().await,
                Backend::Direct(session) => session.shutdown().await,
            }
        });
    }

    /// [`stop`](Self::stop) for the daemon's own shutdown: the session comes back with the next
    /// daemon, so a turn and approvals it cuts off are marked as the daemon's doing.
    pub fn stop_for_daemon(&self, id: SessionId) {
        if let Some(live) = lock(&self.live).get(&id) {
            live.daemon_stop.store(true, Ordering::Release);
        }
        self.stop(id);
    }

    /// Ends the session's agent now: SIGKILL for the harness process and everything below it
    /// (a harness shares the daemon's process group, so there is no group to kill); a direct
    /// chat has no process and just ends. The pump reports the exit.
    pub fn kill(&self, id: SessionId) {
        let Some(live) = lock(&self.live).get(&id).cloned() else {
            return;
        };
        live.stopping.store(true, Ordering::Release);
        match &live.backend {
            Backend::Agent(adapter) => match adapter.pid() {
                Some(pid) => {
                    // Process listing is a burst of system calls: off the async threads.
                    tokio::task::spawn_blocking(move || crate::resources::kill_tree(pid));
                }
                None => {
                    let adapter = adapter.clone();
                    tokio::spawn(async move { adapter.shutdown().await });
                }
            },
            Backend::Direct(session) => {
                let session = session.clone();
                tokio::spawn(async move { session.shutdown().await });
            }
        }
    }

    /// Sends the user's message; the turn's events follow on the bus.
    pub async fn send(
        &self,
        id: SessionId,
        text: String,
        attached: Option<Attached>,
    ) -> Result<TurnId, ErrorBody> {
        if text.trim().is_empty() && attached.is_none() {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "the message is empty",
            ));
        }
        if text.len() > SEND_MAX_BYTES {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "the message is {} bytes; at most {SEND_MAX_BYTES} are allowed",
                    text.len()
                ),
            ));
        }
        let live = self.live(id)?;
        if live.busy.swap(true, Ordering::AcqRel) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "a turn is already in progress; wait for it or interrupt it",
            ));
        }
        let started = match &live.backend {
            Backend::Agent(adapter) => {
                // ACP harnesses take each text attachment as an embedded resource and each
                // image as an image block. Codex has no resource input: it gets the same text
                // quoted after the message, and images as files it reads (`localImage`).
                let codex = adapter.agent() == AGENT_CODEX;
                let (text, mut extras, meta) = match attached {
                    Some(attached) if codex => (
                        format!("{text}\n\n{}", attached.appendix),
                        MessageExtras {
                            context: Vec::new(),
                            images: attached.images,
                        },
                        Some(attached.meta),
                    ),
                    Some(attached) => (
                        text,
                        MessageExtras {
                            context: attached.blocks,
                            images: attached.images,
                        },
                        Some(attached.meta),
                    ),
                    None => (text, MessageExtras::default(), None),
                };
                let prepared = if codex {
                    self.give_images_a_file(&live, &mut extras.images).await
                } else {
                    Ok(())
                };
                match prepared {
                    Ok(()) => {
                        lock(&live.delivery).meta = meta;
                        let sent = adapter
                            .send_message_with(text, extras)
                            .await
                            .map(|turn| turn.turn_id)
                            .map_err(|error| match error {
                                AgentError::Unsupported(_) => {
                                    ErrorBody::new(ErrorCode::Unsupported, error.to_string())
                                }
                                AgentError::TurnInProgress => ErrorBody::new(
                                    ErrorCode::InvalidState,
                                    "a turn is already in progress; wait for it or interrupt it",
                                ),
                                error => {
                                    ErrorBody::new(ErrorCode::ProviderError, error.to_string())
                                }
                            });
                        if sent.is_err() {
                            self.drop_temp_images(&live);
                        }
                        sent
                    }
                    Err(error) => {
                        self.drop_temp_images(&live);
                        Err(error)
                    }
                }
            }
            Backend::Direct(session) => session.send(text, attached).await,
        };
        if started.is_err() {
            live.busy.store(false, Ordering::Release);
        }
        started
    }

    pub async fn interrupt(&self, id: SessionId) -> Result<(), ErrorBody> {
        let live = self.live(id)?;
        let adapter = match &live.backend {
            Backend::Agent(adapter) => adapter,
            Backend::Direct(session) => {
                return if session.interrupt() {
                    Ok(())
                } else {
                    Err(ErrorBody::new(
                        ErrorCode::InvalidState,
                        "no turn is in progress",
                    ))
                };
            }
        };
        match adapter.interrupt().await {
            Ok(()) => Ok(()),
            Err(AgentError::NoTurn) => Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "no turn is in progress",
            )),
            Err(error) => Err(ErrorBody::new(ErrorCode::ProviderError, error.to_string())),
        }
    }

    /// Asks the agent to stop the running turn of one of its subagent threads.
    pub async fn interrupt_thread(
        &self,
        id: SessionId,
        thread: &str,
    ) -> Result<(), ThreadInterruptError> {
        let live = self.live(id).map_err(ThreadInterruptError::Failed)?;
        let Backend::Agent(adapter) = &live.backend else {
            return Err(ThreadInterruptError::Unsupported);
        };
        match adapter.interrupt_thread(thread).await {
            Ok(()) => Ok(()),
            Err(AgentError::NoTurn) => Err(ThreadInterruptError::NoTurn),
            Err(AgentError::Unsupported(_)) => Err(ThreadInterruptError::Unsupported),
            Err(error) => Err(ThreadInterruptError::Failed(ErrorBody::new(
                ErrorCode::ProviderError,
                error.to_string(),
            ))),
        }
    }

    /// The process id of a running chat session's harness (the process whose descendants are
    /// the session's processes); `None` for a direct chat, a session that is not running, and
    /// an agent that has exited.
    pub fn harness_pid(&self, id: SessionId) -> Option<u32> {
        match &lock(&self.live).get(&id)?.backend {
            Backend::Agent(adapter) => adapter.pid(),
            Backend::Direct(_) => None,
        }
    }

    // Approvals --------------------------------------------------------------------------------

    /// Permission requests waiting for an answer, oldest first.
    pub fn approval_list(&self, session: Option<SessionId>) -> ApprovalList {
        let mut approvals: Vec<ApprovalInfo> = lock(&self.approvals)
            .values()
            .map(|p| p.info.clone())
            .filter(|info| session.is_none_or(|s| s == info.session_id))
            .collect();
        if let Some(source) = self.external_approvals() {
            approvals.extend(source.list(session));
        }
        approvals.sort_by(|a, b| {
            a.requested_at_ms
                .cmp(&b.requested_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        ApprovalList { approvals }
    }

    /// Answers a pending request with one of the options the agent offered.
    pub async fn respond(
        &self,
        approval: ApprovalId,
        option_id: String,
        client: Option<ClientKind>,
    ) -> Result<(), ErrorBody> {
        if let Some(source) = self.external_approvals()
            && let Some(answered) = source.respond(approval, option_id.clone(), client)
        {
            return answered;
        }
        let session = {
            let mut approvals = lock(&self.approvals);
            let Some(pending) = approvals.get_mut(&approval) else {
                return Err(ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("approval {approval} is not pending (answered, or its turn ended)"),
                ));
            };
            if !pending.info.options.iter().any(|o| o.id == option_id) {
                let offered: Vec<&str> =
                    pending.info.options.iter().map(|o| o.id.as_str()).collect();
                let mut error = ErrorBody::new(
                    ErrorCode::OptionNotOffered,
                    format!(
                        "option `{option_id}` was not offered; offered: {}",
                        offered.join(", ")
                    ),
                );
                error.data = Some(serde_json::json!({ "offered": offered }));
                return Err(error);
            }
            // The first answer to reach the agent is the one that counts: a second client's
            // is refused below, and must not take the credit.
            if pending.decided_by.is_none() {
                pending.decided_by = Some(client_name(client));
            }
            pending.info.session_id
        };
        let live = self.live(session)?;
        let Backend::Agent(adapter) = &live.backend else {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("approval {approval} is not pending"),
            ));
        };
        match adapter.respond(approval, option_id).await {
            Ok(()) => Ok(()),
            Err(AgentError::UnknownApproval) => Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("approval {approval} was already answered"),
            )),
            Err(error) => {
                // Nobody answered: the claim goes with the failed attempt.
                if let Some(pending) = lock(&self.approvals).get_mut(&approval) {
                    pending.decided_by = None;
                }
                Err(match error {
                    AgentError::OptionNotOffered { option } => ErrorBody::new(
                        ErrorCode::OptionNotOffered,
                        format!("option `{option}` was not offered"),
                    ),
                    error => ErrorBody::new(ErrorCode::ProviderError, error.to_string()),
                })
            }
        }
    }

    // Questions --------------------------------------------------------------------------------

    /// Questions waiting for an answer, oldest first.
    pub fn question_list(&self, session: Option<SessionId>) -> QuestionList {
        let mut questions: Vec<QuestionInfo> = lock(&self.questions)
            .values()
            .map(|p| p.info.clone())
            .filter(|info| session.is_none_or(|s| s == info.session_id))
            .collect();
        questions.sort_by(|a, b| {
            a.requested_at_ms
                .cmp(&b.requested_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        QuestionList { questions }
    }

    /// Answers a pending question: values (checked against its fields), a decline, or a cancel.
    /// The values go to the agent in the shape it asked for and are not kept anywhere.
    pub async fn answer_question(
        &self,
        question: QuestionId,
        response: QuestionResponse,
        client: Option<ClientKind>,
    ) -> Result<(), ErrorBody> {
        let (session, response) = {
            let mut questions = lock(&self.questions);
            let Some(pending) = questions.get_mut(&question) else {
                return Err(ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("question {question} is not pending (answered, or its turn ended)"),
                ));
            };
            let response = match response {
                QuestionResponse::Submit { values } => QuestionResponse::Submit {
                    values: question::validate(&pending.info.spec.fields, &values)
                        .map_err(|e| ErrorBody::new(ErrorCode::BadRequest, e.to_string()))?,
                },
                other => other,
            };
            // The first answer to reach the agent is the one that counts, as for approvals.
            if pending.decided_by.is_none() {
                pending.decided_by = Some(client_name(client));
            }
            (pending.info.session_id, response)
        };
        let released = |this: &Self| {
            if let Some(pending) = lock(&this.questions).get_mut(&question) {
                pending.decided_by = None;
            }
        };
        let live = self.live(session)?;
        let Backend::Agent(adapter) = &live.backend else {
            released(self);
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("question {question} is not pending"),
            ));
        };
        match adapter.answer_question(question, response).await {
            Ok(()) => Ok(()),
            Err(AgentError::UnknownQuestion) => Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("question {question} was already answered"),
            )),
            Err(error) => {
                // Nobody answered: the claim goes with the failed attempt.
                released(self);
                Err(match error {
                    AgentError::BadAnswer(problem) => {
                        ErrorBody::new(ErrorCode::BadRequest, problem)
                    }
                    error => ErrorBody::new(ErrorCode::ProviderError, error.to_string()),
                })
            }
        }
    }

    // History and models -----------------------------------------------------------------------

    pub async fn history(&self, req: ChatHistoryRequest) -> Result<ChatHistory, ErrorBody> {
        let limit = req.limit.clamp(1, HISTORY_MAX_LIMIT);
        // What is still streaming is answered from memory, as it is right now: the journal's
        // snapshot of it is up to 500 ms old. Taken together with `last_seq`, so that every
        // live delta after this reply has a greater `seq` than the text it carries.
        let live = lock(&self.live).get(&req.session_id).cloned();
        let (open, fresh, live_last) = match &live {
            Some(live) => {
                let partials = lock(&live.partials);
                let open: HashSet<ItemId> = partials.open_items().collect();
                let fresh = if req.before_seq.is_none() {
                    partials.view(live.id, registry::now_ms(), || {
                        live.next_seq.fetch_add(1, Ordering::AcqRel)
                    })
                } else {
                    Vec::new()
                };
                let last = live.next_seq.load(Ordering::Acquire).saturating_sub(1);
                (open, fresh, Some(last))
            }
            None => (HashSet::new(), Vec::new(), None),
        };
        let storage = self.storage.clone();
        let (mut rows, journaled) = tokio::task::spawn_blocking(move || {
            let rows =
                storage
                    .chat()
                    .items(req.session_id, req.before_seq, limit.saturating_add(1))?;
            let last = storage.chat().last_seq(req.session_id)?;
            Ok::<_, hutan_storage::StorageError>((rows, last))
        })
        .await
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))?
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))?;
        let has_more = rows.len() > limit as usize;
        if has_more {
            rows.remove(0);
        }
        let mut events: Vec<AgentEvent> = rows
            .iter()
            .filter(|row| {
                // The journal's snapshot of an item that is still streaming is stale.
                let snapshot = row.kind == "message_partial" || row.kind == "reasoning_partial";
                !(snapshot
                    && row
                        .item_id
                        .as_deref()
                        .and_then(|id| id.parse::<ItemId>().ok())
                        .is_some_and(|id| open.contains(&id)))
            })
            .filter_map(|row| match serde_json::from_str::<AgentEvent>(&row.event_json) {
                Ok(event) => Some(event.replayed()),
                Err(error) => {
                    warn!(session = %row.session_id, seq = row.seq, %error, "unreadable journal row skipped");
                    None
                }
            })
            .collect();
        events.extend(fresh.into_iter().map(AgentEvent::replayed));
        Ok(ChatHistory {
            events,
            has_more,
            last_seq: live_last.unwrap_or(journaled).max(journaled),
        })
    }

    pub async fn models(&self, agent: &str) -> Result<ChatModels, ErrorBody> {
        if is_gateway_agent(agent) {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "a direct chat's models come from its provider: use `provider.models`",
            ));
        }
        let harness = is_harness_agent(agent);
        if agent != AGENT_CODEX && !harness {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("unsupported agent `{agent}` (supported: {AGENT_CODEX}, omp, claude)"),
            ));
        }
        let ttl = if harness {
            HARNESS_MODEL_CACHE
        } else {
            MODEL_CACHE
        };
        if let Some((at, models)) = lock(&self.models).get(agent)
            && at.elapsed() < ttl
        {
            return Ok(ChatModels {
                agent: agent.to_owned(),
                models: models.clone(),
            });
        }
        let models = if harness {
            self.harness_models(agent).await?
        } else {
            let codex = self.resolved_codex().await;
            hutan_agents::codex::list_models(&codex)
                .await
                .map_err(|e| provider_error(&e, &codex.program))?
        };
        lock(&self.models).insert(agent.to_owned(), (Instant::now(), models.clone()));
        Ok(ChatModels {
            agent: agent.to_owned(),
            models,
        })
    }

    /// The models a harness offers: the model options of an ACP session of its own (a throwaway
    /// one in the system temp directory, asked and closed; sessions that run already refreshed
    /// the cache when they started). OMP, when that fails or offers none, falls back to
    /// `omp models --json`, which needs no session.
    async fn harness_models(&self, agent: &str) -> Result<Vec<ModelInfo>, ErrorBody> {
        let harnesses = self.harnesses.clone();
        let name = agent.to_owned();
        let located = tokio::task::spawn_blocking(move || harnesses.acp_config(&name, false))
            .await
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("locating the agent: {e}")))?;
        let session_error = match located {
            Ok(config) => {
                match hutan_agents::acp::list_models(&config, std::env::temp_dir()).await {
                    Ok(models) if !models.is_empty() => return Ok(models),
                    Ok(_) => None,
                    Err(error) => Some(harness_error(&error, agent, &config.program)),
                }
            }
            Err(error) => Some(error),
        };
        if agent != crate::harness::AGENT_OMP {
            return Err(session_error.unwrap_or_else(|| {
                ErrorBody::new(
                    ErrorCode::ProviderError,
                    format!("{agent} offers no choice of models"),
                )
            }));
        }
        if let Some(error) = &session_error {
            debug!(message = %error.message, "no model options from an omp session; asking `omp models`");
        }
        let harnesses = self.harnesses.clone();
        let (program, env) = tokio::task::spawn_blocking(move || {
            harnesses.cli(crate::harness::AGENT_OMP, std::path::Path::new("codex"))
        })
        .await
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("locating omp: {e}")))??;
        crate::accounts::omp_models(&program, &env).await
    }

    // Events -----------------------------------------------------------------------------------

    /// Numbers, journals and publishes the events of one session until its agent is gone.
    async fn pump(self: Arc<Self>, live: Arc<Live>, mut events: mpsc::Receiver<AdapterMessage>) {
        let mut exit = ExitInfo::default();
        // When the snapshots of streaming text are next journaled: set when text first arrives
        // that no snapshot has, not moved by further deltas (so a steady stream is still
        // written every interval). No timer runs while nothing streams.
        let mut snapshot_at: Option<tokio::time::Instant> = None;
        loop {
            let due = async {
                match snapshot_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                message = events.recv() => {
                    let Some(message) = message else { break };
                    match message {
                        AdapterMessage::Event(draft) => self.handle(&live, *draft).await,
                        AdapterMessage::Node(update) => self.notifier.node(live.id, *update).await,
                        AdapterMessage::Exited(info) => {
                            exit = info;
                            break;
                        }
                    }
                    if snapshot_at.is_none() && lock(&live.partials).has_dirty() {
                        snapshot_at = Some(tokio::time::Instant::now() + PARTIAL_INTERVAL);
                    }
                }
                () = due => {
                    snapshot_at = None;
                    self.flush_partials(&live, Flush::Dirty);
                }
            }
        }
        self.finish(&live, exit).await;
    }

    /// Journals snapshots of the open streaming items (see [`Partials::flush`]).
    fn flush_partials(&self, live: &Live, flush: Flush) {
        let mut partials = lock(&live.partials);
        self.flush_locked(live, &mut partials, flush);
    }

    fn flush_locked(&self, live: &Live, partials: &mut Partials, flush: Flush) {
        let written = partials.flush(flush, live.id, registry::now_ms(), || {
            live.next_seq.fetch_add(1, Ordering::AcqRel)
        });
        // Queued while the lock is held: two snapshots of one item reach the writer in the
        // order they were taken.
        for snapshot in written {
            self.journal_replacing(&snapshot.event, snapshot.replaces);
        }
    }

    /// Deletes the temporary copies of clipboard images a turn gave to its agent, and forgets
    /// the attachments' meta of a turn that never journaled a user message.
    fn drop_temp_images(&self, live: &Live) {
        let files = {
            let mut delivery = lock(&live.delivery);
            delivery.meta = None;
            std::mem::take(&mut delivery.temp_files)
        };
        for file in files {
            if let Err(error) = std::fs::remove_file(&file)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                warn!(file = %file.display(), %error, "could not remove a temporary image");
            }
        }
    }

    /// Writes the clipboard images of `images` that have no file to `<home>/tmp/` for an agent
    /// that reads images from disk; the files are removed when the turn ends.
    async fn give_images_a_file(
        &self,
        live: &Live,
        images: &mut [ImageBlock],
    ) -> Result<(), ErrorBody> {
        use std::os::unix::fs::PermissionsExt as _;
        let failed = |error: std::io::Error| {
            ErrorBody::new(
                ErrorCode::Internal,
                format!("could not write a temporary image for the agent: {error}"),
            )
        };
        for image in images.iter_mut().filter(|image| image.path.is_none()) {
            let extension = match image.media_type.as_str() {
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/webp" => "webp",
                _ => "png",
            };
            tokio::fs::create_dir_all(&self.tmp_dir)
                .await
                .map_err(failed)?;
            let file = self.tmp_dir.join(format!(
                "{TEMP_IMAGE_PREFIX}{}.{extension}",
                ulid::Ulid::generate()
            ));
            lock(&live.delivery).temp_files.push(file.clone());
            tokio::fs::write(&file, image.data.as_slice())
                .await
                .map_err(failed)?;
            tokio::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(failed)?;
            image.path = Some(file);
        }
        Ok(())
    }

    /// The agent is gone: nothing pending can be answered, an open turn is over.
    async fn finish(&self, live: &Live, exit: ExitInfo) {
        let leftover: Vec<ApprovalId> = lock(&self.approvals)
            .values()
            .filter(|p| p.info.session_id == live.id)
            .map(|p| p.info.id)
            .collect();
        for approval in leftover {
            let draft = AgentEventDraft::new(AgentEventKind::PermissionResolved {
                approval_id: approval,
                outcome: PermissionOutcome::Expired,
                decided_by: None,
            });
            self.handle(live, draft).await;
        }
        let asked: Vec<QuestionId> = lock(&self.questions)
            .values()
            .filter(|p| p.info.session_id == live.id)
            .map(|p| p.info.id)
            .collect();
        for question in asked {
            let draft = AgentEventDraft::new(AgentEventKind::QuestionResolved {
                question_id: question,
                outcome: QuestionOutcome::Expired,
                decided_by: None,
            });
            self.handle(live, draft).await;
        }
        let open_turn = lock(&live.open_turn).take();
        if let Some(turn) = open_turn {
            let kind = if live.stopping.load(Ordering::Acquire) {
                AgentEventKind::TurnCancelled
            } else {
                AgentEventKind::TurnFailed {
                    message: "the agent exited during the turn".to_owned(),
                    details: (!exit.stderr_tail.is_empty()).then(|| exit.stderr_tail.clone()),
                }
            };
            let mut draft = AgentEventDraft::new(kind);
            draft.turn_id = Some(turn);
            // `handle` clears `busy` and tells the registry the turn ended.
            self.handle(live, draft).await;
        }
        // Text no turn end claimed (the agent died between messages) is kept as it is.
        self.flush_partials(live, Flush::Final);
        lock(&self.live).remove(&live.id);
        self.drop_temp_images(live);
        info!(session = %live.id, code = ?exit.code, signal = ?exit.signal, "chat agent exited");
        self.notifier.exited(live.id, exit).await;
    }

    async fn handle(&self, live: &Live, mut draft: AgentEventDraft) {
        let ts_ms = registry::now_ms();
        let ends_turn = draft.kind.ends_turn();
        // The user's message of a turn that carried attachments says what they were, whichever
        // adapter journals the message.
        if matches!(
            draft.kind,
            AgentEventKind::MessageEnd {
                role: hutan_core::agent::MessageRole::User,
                ..
            }
        ) && draft.payload.is_none()
            && let Some(meta) = lock(&live.delivery).meta.take()
        {
            draft.payload = Some(serde_json::json!({ ATTACHMENTS_META_KEY: meta }));
        }
        if ends_turn {
            self.drop_temp_images(live);
        }
        let mut event = {
            // Numbered and folded into the open items in one step: see `Live::partials`.
            let mut partials = lock(&live.partials);
            if ends_turn {
                // Text of a turn that ended without its `*_end` events is all there is of it.
                self.flush_locked(live, &mut partials, Flush::Final);
            }
            let seq = live.next_seq.fetch_add(1, Ordering::AcqRel);
            let event = draft.into_event(live.id, seq, ts_ms);
            if let Some(snapshot) = partials.observe(&event) {
                registry::log_write(
                    self.storage.chat().delete(live.id, snapshot),
                    "a superseded chat snapshot",
                );
            }
            event
        };
        self.attribute(&mut event);

        let mut turn_flag = None;
        let mut required: Option<ApprovalInfo> = None;
        let mut resolved: Option<(ApprovalId, PermissionOutcome, Option<DecidedBy>)> = None;
        let mut asked: Option<QuestionInfo> = None;
        let mut settled: Option<(QuestionId, QuestionOutcome, Option<DecidedBy>)> = None;
        match &event.kind {
            AgentEventKind::TurnStarted => {
                *lock(&live.open_turn) = event.turn_id;
                self.write_turn(&event, ChatTurnStatus::Running, None);
                turn_flag = Some(true);
            }
            kind if kind.ends_turn() => {
                *lock(&live.open_turn) = None;
                live.busy.store(false, Ordering::Release);
                let (status, error) = match kind {
                    AgentEventKind::TurnCompleted { .. } => (ChatTurnStatus::Completed, None),
                    AgentEventKind::TurnFailed { message, .. } => {
                        (ChatTurnStatus::Failed, Some(message.clone()))
                    }
                    _ => (
                        ChatTurnStatus::Cancelled,
                        live.daemon_stop
                            .load(Ordering::Acquire)
                            .then(|| DAEMON_STOP_TURN_ERROR.to_owned()),
                    ),
                };
                self.write_turn(&event, status, error);
                turn_flag = Some(false);
            }
            AgentEventKind::PermissionRequested {
                approval_id,
                kind,
                title,
                detail,
                options,
            } => {
                let info = ApprovalInfo {
                    id: *approval_id,
                    session_id: live.id,
                    turn_id: event.turn_id,
                    kind: *kind,
                    title: title.clone(),
                    detail: detail.clone(),
                    options: options.clone(),
                    requested_at_ms: ts_ms,
                };
                self.write_approval_request(&info);
                lock(&self.approvals).insert(
                    *approval_id,
                    Pending {
                        info: info.clone(),
                        decided_by: None,
                    },
                );
                required = Some(info);
            }
            AgentEventKind::PermissionResolved {
                approval_id,
                outcome,
                decided_by,
            } => {
                let pending = lock(&self.approvals).remove(approval_id);
                if let Some(pending) = pending {
                    self.write_approval_outcome(
                        &pending,
                        outcome,
                        ts_ms,
                        live.daemon_stop.load(Ordering::Acquire),
                    );
                    resolved = Some((*approval_id, outcome.clone(), decided_by.clone()));
                }
            }
            AgentEventKind::QuestionRequested { question_id, spec } => {
                let info = QuestionInfo {
                    id: *question_id,
                    session_id: live.id,
                    turn_id: event.turn_id,
                    spec: spec.clone(),
                    requested_at_ms: ts_ms,
                };
                self.write_question_request(&info);
                lock(&self.questions).insert(
                    *question_id,
                    PendingQuestion {
                        info: info.clone(),
                        decided_by: None,
                    },
                );
                asked = Some(info);
            }
            AgentEventKind::QuestionResolved {
                question_id,
                outcome,
                decided_by,
            } => {
                let pending = lock(&self.questions).remove(question_id);
                if let Some(pending) = pending {
                    self.write_question_outcome(
                        &pending,
                        outcome,
                        ts_ms,
                        live.daemon_stop.load(Ordering::Acquire),
                    );
                    settled = Some((*question_id, outcome.clone(), decided_by.clone()));
                }
            }
            _ => {}
        }

        let approvals_changed =
            required.is_some() || resolved.is_some() || asked.is_some() || settled.is_some();
        if !event.kind.is_streaming() {
            self.journal(&event);
        }
        self.emit(&Event::ChatEvent {
            session_id: live.id,
            event: Box::new(event),
        });
        if let Some(info) = required {
            self.emit(&Event::ApprovalRequired {
                approval: Box::new(info),
            });
        }
        if let Some((approval_id, outcome, decided_by)) = resolved {
            self.emit(&Event::ApprovalResolved {
                approval_id,
                session_id: live.id,
                outcome,
                decided_by,
            });
        }
        if let Some(info) = asked {
            self.emit(&Event::QuestionRequired {
                question: Box::new(info),
            });
        }
        if let Some((question_id, outcome, decided_by)) = settled {
            self.emit(&Event::QuestionResolved {
                question_id,
                session_id: live.id,
                outcome,
                decided_by,
            });
        }
        // The session's state follows what happened: the registry is told after the events
        // are out, so a client that sees the state change has the events already.
        let pending = u32::try_from(
            lock(&self.approvals)
                .values()
                .filter(|p| p.info.session_id == live.id)
                .count(),
        )
        .unwrap_or(u32::MAX);
        let questions = u32::try_from(
            lock(&self.questions)
                .values()
                .filter(|p| p.info.session_id == live.id)
                .count(),
        )
        .unwrap_or(u32::MAX);
        if turn_flag.is_some() || approvals_changed {
            self.notifier
                .state(live.id, turn_flag, pending, questions)
                .await;
        }
    }

    /// Says on a `permission_resolved` event who answered: the client that reached the agent
    /// first (see [`respond`](Self::respond)); nobody for an expiry.
    fn attribute(&self, event: &mut AgentEvent) {
        if let AgentEventKind::QuestionResolved {
            question_id,
            outcome,
            decided_by,
        } = &mut event.kind
        {
            if !matches!(outcome, QuestionOutcome::Expired)
                && let Some(pending) = lock(&self.questions).get(question_id)
            {
                *decided_by = Some(DecidedBy {
                    client_kind: pending.decided_by.unwrap_or("other").to_owned(),
                    client_name: None,
                });
            }
            return;
        }
        let AgentEventKind::PermissionResolved {
            approval_id,
            outcome: PermissionOutcome::Selected { .. },
            decided_by,
        } = &mut event.kind
        else {
            return;
        };
        if let Some(pending) = lock(&self.approvals).get(approval_id) {
            *decided_by = Some(DecidedBy {
                client_kind: pending.decided_by.unwrap_or("other").to_owned(),
                client_name: None,
            });
        }
    }

    fn emit(&self, event: &Event) {
        if let Err(error) = self.bus.publish(event) {
            error!(%error, "failed to publish a chat event");
        }
    }

    fn journal(&self, event: &AgentEvent) {
        self.journal_replacing(event, None);
    }

    /// Journals `event`; with `replaces` the journal row at that `seq` goes in the same
    /// transaction (a snapshot that is kept up to date).
    fn journal_replacing(&self, event: &AgentEvent, replaces: Option<u64>) {
        let event_json = match serde_json::to_string(event) {
            Ok(json) => json,
            Err(error) => {
                error!(session = %event.session_id, %error, "could not encode a chat event");
                return;
            }
        };
        let row = ChatItemRow {
            session_id: event.session_id,
            seq: event.seq,
            ts_ms: event.ts_ms,
            turn_id: event.turn_id.map(|t| t.to_string()),
            item_id: event.item_id.map(|i| i.to_string()),
            kind: event.kind.name().to_owned(),
            event_json,
        };
        let chat = self.storage.chat();
        let result = match replaces {
            Some(old) => chat.replace(old, row),
            None => chat.append(row),
        };
        registry::log_write(result, "a chat event");
    }

    fn write_turn(&self, event: &AgentEvent, status: ChatTurnStatus, error: Option<String>) {
        let Some(turn) = event.turn_id else { return };
        let running = status == ChatTurnStatus::Running;
        let row = ChatTurnRow {
            session_id: event.session_id,
            turn_id: turn.to_string(),
            provider_turn_id: event.provider.turn_id.clone(),
            status,
            started_at_ms: event.ts_ms,
            ended_at_ms: (!running).then_some(event.ts_ms),
            error,
        };
        registry::log_write(self.storage.chat().upsert_turn(row), "a chat turn");
    }

    fn write_approval_request(&self, info: &ApprovalInfo) {
        let options_json = serde_json::to_string(&info.options).unwrap_or_else(|error| {
            error!(approval = %info.id, %error, "could not encode approval options");
            "[]".to_owned()
        });
        let row = ApprovalRow {
            id: info.id,
            session_id: info.session_id,
            turn_id: info.turn_id.map(|t| t.to_string()),
            kind: enum_name(&info.kind),
            title: info.title.clone(),
            options_json,
            requested_at_ms: info.requested_at_ms,
            resolved_at_ms: None,
            outcome: None,
            option_id: None,
            decided_by: None,
        };
        registry::log_write(self.storage.approvals().upsert(row), "an approval request");
    }

    fn write_approval_outcome(
        &self,
        pending: &Pending,
        outcome: &PermissionOutcome,
        at_ms: u64,
        daemon_stop: bool,
    ) {
        let info = &pending.info;
        let options_json = serde_json::to_string(&info.options).unwrap_or_else(|_| "[]".to_owned());
        let (outcome, option_id, decided_by) = match outcome {
            PermissionOutcome::Selected { option_id } => (
                ApprovalOutcome::Selected,
                Some(option_id.clone()),
                Some(pending.decided_by.unwrap_or("other").to_owned()),
            ),
            PermissionOutcome::Expired => (
                ApprovalOutcome::Expired,
                None,
                daemon_stop.then(|| DAEMON_STOP_DECIDER.to_owned()),
            ),
        };
        let row = ApprovalRow {
            id: info.id,
            session_id: info.session_id,
            turn_id: info.turn_id.map(|t| t.to_string()),
            kind: enum_name(&info.kind),
            title: info.title.clone(),
            options_json,
            requested_at_ms: info.requested_at_ms,
            resolved_at_ms: Some(at_ms),
            outcome: Some(outcome),
            option_id,
            decided_by,
        };
        registry::log_write(self.storage.approvals().upsert(row), "an approval decision");
    }
}

impl ChatManager {
    fn write_question_request(&self, info: &QuestionInfo) {
        let row = QuestionRow {
            id: info.id,
            session_id: info.session_id,
            turn_id: info.turn_id.map(|t| t.to_string()),
            title: info.spec.title.clone(),
            requested_at_ms: info.requested_at_ms,
            resolved_at_ms: None,
            outcome: None,
            decided_by: None,
        };
        registry::log_write(self.storage.questions().upsert(row), "a question");
    }

    /// How a question ended and who decided: never the values.
    fn write_question_outcome(
        &self,
        pending: &PendingQuestion,
        outcome: &QuestionOutcome,
        at: u64,
        daemon_stop: bool,
    ) {
        let info = &pending.info;
        let (stored, decided_by) = match outcome {
            QuestionOutcome::Answered { .. } => (StoredOutcome::Answered, true),
            QuestionOutcome::Declined => (StoredOutcome::Declined, true),
            QuestionOutcome::Cancelled => (StoredOutcome::Cancelled, true),
            QuestionOutcome::Expired => (StoredOutcome::Expired, false),
        };
        // An expiry the daemon's own shutdown caused says so (`session.recovery`).
        let stopped_by_daemon = daemon_stop && matches!(outcome, QuestionOutcome::Expired);
        let row = QuestionRow {
            id: info.id,
            session_id: info.session_id,
            turn_id: info.turn_id.map(|t| t.to_string()),
            title: info.spec.title.clone(),
            requested_at_ms: info.requested_at_ms,
            resolved_at_ms: Some(at),
            outcome: Some(stored),
            decided_by: if stopped_by_daemon {
                Some(DAEMON_STOP_DECIDER.to_owned())
            } else {
                decided_by.then(|| pending.decided_by.unwrap_or("other").to_owned())
            },
        };
        registry::log_write(self.storage.questions().upsert(row), "a question outcome");
    }
}

/// Marks approvals and questions a crashed daemon left open as expired (their agents are gone)
/// and returns them as they were.
pub fn expire_dangling_approvals(
    storage: &Storage,
) -> Result<(Vec<ApprovalRow>, Vec<QuestionRow>), hutan_storage::StorageError> {
    let now = registry::now_ms();
    let open = storage.approvals().unresolved()?;
    for row in &open {
        let mut expired = row.clone();
        expired.resolved_at_ms = Some(now);
        expired.outcome = Some(ApprovalOutcome::Expired);
        storage.approvals().upsert(expired)?;
    }
    debug!(count = open.len(), "dangling approvals expired");
    let open_questions = storage.questions().unresolved()?;
    for row in &open_questions {
        let mut expired = row.clone();
        expired.resolved_at_ms = Some(now);
        expired.outcome = Some(StoredOutcome::Expired);
        storage.questions().upsert(expired)?;
    }
    debug!(count = open_questions.len(), "dangling questions expired");
    Ok((open, open_questions))
}
