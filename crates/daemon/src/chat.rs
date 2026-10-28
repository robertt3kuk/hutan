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

mod fork;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use hutan_agents::{
    AcpAdapter, AcpConfig, AcpSession, AdapterMessage, AgentAdapter, AgentError,
    CodexAppServerAdapter, CodexConfig, CodexSession, ContextBlock, DirsChange, ExitInfo,
    ImageBlock, MessageExtras, StartSpec, Started,
};
use hutan_core::agent::{DecidedBy, NoticeLevel, PermissionOutcome};
use hutan_core::question::{self, QuestionResponse};
use hutan_core::{
    AgentEvent, AgentEventDraft, AgentEventKind, ApprovalId, ApprovalPolicy, Contain, Containment,
    ForkSupport, ItemId, ModelInfo, QuestionId, QuestionOutcome, SafetyPreset, SandboxMode,
    SessionId, TurnId,
};
use hutan_protocol::assistant::{ATTACHMENTS_META_KEY, AttachmentMeta};
use hutan_protocol::chat::{
    ApprovalInfo, ApprovalList, ChatDirApplied, ChatHistory, ChatHistoryRequest, ChatModels,
    ChatQueue, ChatSentNow, HISTORY_MAX_LIMIT, QuestionInfo, QuestionList, QueueHold,
    QueuedMessage, SEND_MAX_BYTES,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{ClientKind, Event};
use hutan_storage::{
    ApprovalOutcome, ApprovalRow, ChatItemRow, ChatTurnRow, ChatTurnStatus,
    QuestionOutcome as StoredOutcome, QuestionRow, Storage,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::assistant_pick::{CATALOG_TIMEOUT, pick_assistant_model};
use crate::bus::EventBus;
use crate::chat_queue::{Changed, Queues};
use crate::checkpoints::turns::{Scope, TurnCheckpoints};
use crate::config::AssistantMode;
use crate::contain::{Fragment, Sandbox};
use crate::context::Attached;
use crate::direct::{DirectSession, Target};
use crate::harness::{
    AGENT_CLAUDE, AGENT_OMP, Harnesses, assistant_profile, is_harness_agent, omp_assistant_prompt,
};
use crate::insight::{InsightHub, InsightTarget};
use crate::mcp_wiring::{McpServerSpec, McpWiring};
use crate::memory::{MemoryContext, PreparedDelta, StartContext};
use crate::partial::{Flush, PARTIAL_INTERVAL, Partials};
use crate::providers::ProviderService;
use crate::registry::{self, ChatNotifier};
use crate::usage::{Attribution, UsageService};
use hutan_core::insight::TurnUsage;

use fork::fork_from;
pub use fork::{ChatForkSpec, FORK_MAX_EVENTS, ForkPending, JournalCopy};
pub(crate) use fork::{ForkParent, ForkPlan, support_note};

/// The Codex app-server agent.
pub const AGENT_CODEX: &str = "codex";
/// Names of the temporary files that hold clipboard images for Codex (`<home>/tmp/`).
const TEMP_IMAGE_PREFIX: &str = "img-";
/// How many of the newest journaled `message_end` events `chat.retry` looks through for the
/// last user message (a turn ends a few assistant messages at most after it).
const RETRY_LOOKBACK: u32 = 200;
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
/// How long `agent.measure_context` waits for the harness to answer `/context`.
const MEASURE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a harness's model list is reused (`omp models` is a subprocess of seconds).
const HARNESS_MODEL_CACHE: Duration = Duration::from_secs(5 * 60);
/// The longest the idle sleeper waits between two looks at the assistants.
const SLEEP_CHECK_MAX: Duration = Duration::from_secs(5);
/// The shortest.
const SLEEP_CHECK_MIN: Duration = Duration::from_millis(20);
/// How long a message waits for an assistant that is going to sleep to finish doing so.
const SLEEP_WAIT: Duration = Duration::from_secs(30);
/// The journal notice of a wake that could not continue the conversation. The same words as the
/// restart recovery's chip for a chat that started a new conversation (`hutan-client`).
const NEW_CONVERSATION_NOTICE: &str =
    "The agent started a new conversation; the transcript is kept.";

/// What Mawas may do right now: `assistant.mode` and `assistant.max_actions_per_turn`, both live
/// settings of the daemon (`daemon.settings.set`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssistantPolicy {
    pub mode: AssistantMode,
    /// Mutating requests Mawas may make in one turn before every further one asks.
    pub max_actions: u32,
}

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
    /// Codex and harness chats only (protocol 0.50): the managed account the agent runs as,
    /// resolved when the chat was created (a restart or restore keeps it whatever the defaults
    /// say now); `None`: the harness's own login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The account's label when the chat was created, for the chat header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    /// Codex and harness chats (protocol 0.51): the safety preset the chat was created with.
    /// Its Codex pair (`approval_policy`, `sandbox`) and its session `mode` are stored beside it
    /// when the chat is created; the rest (OMP's arguments, Claude Code's sandbox metadata) is
    /// derived from it at every start, so a restart or restore applies it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<SafetyPreset>,
    /// OMP chats (protocol 0.52): the process runs inside Hutan's Seatbelt boundary. Planned
    /// again at every start, so a restart or a restore applies it again — or fails closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contain: Option<Contain>,
    /// Protocol 0.56 (ADR 0023): where the chat was forked from, and what its first start still
    /// has to do. Absent for chats that are not forks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<ChatForkSpec>,
    /// Codex and harness chats (protocol 0.57): the extra folders of the session, canonical, in
    /// the order they were added; given to the agent again at every start, restart and restore
    /// (`session_dirs`). The session's own folder is the session's `cwd`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<PathBuf>,
    /// The chat has had extra folders at some time, so an agent that keeps a list of its own
    /// (OMP's session header) is asked for it on a resume and made to match `dirs`, also when
    /// that is empty now (a folder taken out while the agent was not running).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dirs_managed: bool,
}

/// What runs a session: an agent process, or the model gateway.
#[derive(Debug, Clone)]
enum Backend {
    Agent(Arc<dyn AgentAdapter>),
    Direct(Arc<DirectSession>),
}

/// The name a harness conversation's memory snapshot records as its backend.
fn harness_backend(agent: &str) -> &'static str {
    match agent {
        AGENT_OMP => "omp",
        AGENT_CLAUDE => "claude",
        _ => "codex",
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded data is never left half-updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A duration in whole milliseconds, saturating.
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Pending approvals that are not an agent's tool permissions (the control gate's, protocol
/// 0.21), offered through the same `approval.*` requests, notification intents and events.
pub trait ApprovalSource: Send + Sync + std::fmt::Debug {
    /// The pending approvals, optionally of one session.
    fn list(&self, session: Option<SessionId>) -> Vec<ApprovalInfo>;
    /// Answers `approval` when it is the source's: `None` when it is not. The answer may take
    /// work (a memory proposal is applied), so it is a future.
    fn respond(
        &self,
        approval: ApprovalId,
        option_id: String,
        client: Option<ClientKind>,
    ) -> Option<BoxFuture<'static, Result<(), ErrorBody>>>;
}

/// Why an agent could not be started, and whether the agent refused to continue the stored
/// conversation (a wake then starts a new one).
#[derive(Debug)]
struct StartFailure {
    error: ErrorBody,
    resume_refused: bool,
}

impl StartFailure {
    fn agent(cause: &AgentError, error: ErrorBody) -> Self {
        let resume_refused = match cause {
            AgentError::Rpc { method, .. } => {
                matches!(
                    method.as_str(),
                    "session/load" | "session/resume" | "thread/resume"
                )
            }
            AgentError::Unsupported(message) => message.contains("cannot continue a conversation"),
            _ => false,
        };
        Self {
            error,
            resume_refused,
        }
    }
}

/// Why a turn could not be started now.
#[derive(Debug)]
enum Acquire {
    /// A turn runs.
    Busy(Arc<Live>),
    Failed(ErrorBody),
}

/// What `chat.send_now` sends.
#[derive(Debug)]
enum Now {
    Text(String),
    /// A queued message.
    Item(u64),
}

fn turn_in_progress() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidState,
        "a turn is already in progress; wait for it, queue the message (`chat.queue.add`) or \
         interrupt the turn",
    )
}

/// A message's text is not empty (unless it has attachments) and not too long.
fn check_message(text: &str, attached: bool) -> Result<(), ErrorBody> {
    if text.trim().is_empty() && !attached {
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
    Ok(())
}

impl From<ErrorBody> for StartFailure {
    fn from(error: ErrorBody) -> Self {
        Self {
            error,
            resume_refused: false,
        }
    }
}

#[derive(Debug)]
struct Live {
    id: SessionId,
    backend: Backend,
    /// The next event's `seq`.
    next_seq: AtomicU64,
    /// A turn is in progress, or `chat.send` is starting one (or the idle sleeper holds the
    /// session while it stops the agent).
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
    /// An assistant: the idle sleeper may stop its agent.
    assistant: bool,
    /// The mode an assistant's agent was started in. A change of `assistant.mode` restarts the
    /// agent before the next turn (`ChatManager::awake`).
    mode: AssistantMode,
    /// The idle sleeper stops the agent; its exit puts the session to sleep, it does not end
    /// it.
    sleeping: AtomicBool,
    /// When the session last did something: started, ended a turn, had an approval or a
    /// question answered.
    active_at: Mutex<Instant>,
    /// A Mawas conversation: it is given the memory the user approved (`crate::memory`), and a
    /// delta of it when the memory moved between turns.
    mawas: bool,
    /// Memory the conversation's first turn carries because its backend has no instruction
    /// channel of its own (Claude Code): taken by the first `chat.send`, put back when that
    /// send fails.
    first_turn: Mutex<Option<String>>,
    /// A summary fork's digest, carried by the conversation's first message: taken by it, put
    /// back when that message is not sent, and reported delivered once it was.
    seed: Mutex<Option<String>>,
    /// What this session's turns used, and who they are charged to (`docs/agents.md` § Usage).
    usage: LiveUsage,
    /// `agent.measure_context` waits here for the end of its turn: the first turn that ends
    /// while it is set is the measuring one (it holds `busy`, so no other turn runs).
    measuring: Mutex<Option<oneshot::Sender<MeasureEnd>>>,
}

/// How the turn of `agent.measure_context` ended.
#[derive(Debug)]
enum MeasureEnd {
    Completed,
    Failed(String),
}

/// The usage accounting of one running session.
#[derive(Debug)]
struct LiveUsage {
    attribution: Mutex<Attribution>,
    turn: Mutex<TurnUsage>,
}

impl LiveUsage {
    /// `fresh`: the conversation began with this start (its cost, when reported, began at zero).
    fn new(attribution: Attribution, fresh: bool) -> Self {
        Self {
            attribution: Mutex::new(attribution),
            turn: Mutex::new(TurnUsage::new(fresh)),
        }
    }
}

impl Live {
    fn new(
        id: SessionId,
        backend: Backend,
        last_seq: u64,
        assistant: bool,
        mode: AssistantMode,
        mawas: bool,
        usage: LiveUsage,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            backend,
            next_seq: AtomicU64::new(last_seq + 1),
            busy: AtomicBool::new(false),
            open_turn: Mutex::new(None),
            stopping: AtomicBool::new(false),
            daemon_stop: AtomicBool::new(false),
            partials: Mutex::new(Partials::default()),
            delivery: Mutex::new(Delivery::default()),
            assistant,
            mode,
            sleeping: AtomicBool::new(false),
            active_at: Mutex::new(Instant::now()),
            mawas,
            first_turn: Mutex::new(None),
            seed: Mutex::new(None),
            usage,
            measuring: Mutex::new(None),
        })
    }

    /// The idle clock starts again.
    fn touch(&self) {
        *lock(&self.active_at) = Instant::now();
    }

    /// How long since the session last did something.
    fn idle_for(&self) -> Duration {
        lock(&self.active_at).elapsed()
    }
}

/// Attachments of the message a turn started with.
#[derive(Debug, Default)]
struct Delivery {
    /// Their meta, for the journaled user message (`attachments_meta`); taken by the first user
    /// `message_end` of the turn.
    meta: Option<Vec<AttachmentMeta>>,
    /// Temporary files holding clipboard images for the agent; removed when the turn ends.
    temp_files: Vec<PathBuf>,
    /// The turn is a `chat.retry`: the user message the agent echoes is already in the journal
    /// and is dropped, not journaled again. Cleared by that echo's `message_end` and when the
    /// turn ends.
    retry: bool,
}

/// What Hutan adds to one turn of a Mawas conversation (`crate::memory`): context blocks that
/// are Hutan's, never text the user typed.
#[derive(Debug, Default)]
struct TurnMemory {
    blocks: Vec<ContextBlock>,
    /// Context the agent is given without the journal showing it as the user's words (a summary
    /// fork's digest): ACP agents take it as resources of their own.
    hidden: Vec<ContextBlock>,
    /// What the journal says about them.
    notices: Vec<String>,
    /// The delta to record once the turn is accepted.
    delta: Option<PreparedDelta>,
    /// The first-turn block taken from the session, put back when the send fails.
    first_turn: Option<String>,
    /// A summary fork's digest taken from the session, put back when the send fails.
    seed: Option<String>,
}

impl TurnMemory {
    /// `text` followed by each block, for backends that take one text (Codex, direct chats).
    fn appended_to(&self, mut text: String) -> String {
        for block in &self.blocks {
            text.push_str("\n\n");
            text.push_str(&block.text);
        }
        text
    }

    /// The send failed: nothing was delivered.
    fn undo(&self, live: &Live) {
        if let Some(text) = &self.first_turn {
            *lock(&live.first_turn) = Some(text.clone());
        }
        if let Some(text) = &self.seed {
            *lock(&live.seed) = Some(text.clone());
        }
    }
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
    /// `assistant.idle_stop_secs` in milliseconds; `0` never puts an assistant to sleep.
    idle_stop_ms: AtomicU64,
    /// `assistant.mode` and `assistant.max_actions_per_turn`, now.
    policy: Mutex<AssistantPolicy>,
    live: Mutex<HashMap<SessionId, Arc<Live>>>,
    /// Assistants whose agent was stopped for being idle (or that a daemon restart brought
    /// back without starting it): sessions that are idle for their clients and have no
    /// process. Changes together with `live`, under its lock.
    asleep: Mutex<HashSet<SessionId>>,
    /// One wake of a session at a time: a second message waits for the first one's agent.
    wakes: Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
    approvals: Mutex<HashMap<ApprovalId, Pending>>,
    questions: Mutex<HashMap<QuestionId, PendingQuestion>>,
    /// Model catalogs by agent.
    models: Mutex<HashMap<String, (Instant, Vec<ModelInfo>)>>,
    external: Mutex<Vec<Weak<dyn ApprovalSource>>>,
    /// Mawas's memory (MW-2): what a conversation is given. Weak, like `external`.
    memory: Mutex<Option<Weak<dyn MemoryContext>>>,
    mcp: OnceLock<McpWiring>,
    /// `<home>/tmp`: where clipboard images wait for an agent that reads images from disk.
    tmp_dir: PathBuf,
    /// A snapshot of the checkout when an agent chat's turn starts and when it ends.
    checkpoints: Arc<TurnCheckpoints>,
    /// The queue of each chat session (protocol 0.36).
    queues: Queues,
    /// Records what turns used and the limits harnesses report.
    usage: Arc<UsageService>,
    /// What each chat's context holds and spends (protocol 0.59): fed by the pump.
    insight: Arc<InsightHub>,
    /// Contain (protocol 0.52): wraps the process of a chat that asks for the boundary.
    sandbox: Arc<Sandbox>,
    /// The boundary each running chat's process was started in, read from what was applied.
    contained: Mutex<HashMap<SessionId, Containment>>,
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

/// The journal's words when Mawas's mode changed: the agent restarts so the change applies to
/// the turn that is about to start.
fn mode_notice(mode: AssistantMode) -> &'static str {
    match mode {
        AssistantMode::Ask => {
            "Mawas is now in “Ask before acting” mode: it can act, and every change asks you first. \
             Its agent restarted so the change applies to this turn."
        }
        AssistantMode::Advisor => {
            "Mawas is now in “Advisor” mode: it reads and advises, and cannot change anything. \
             Its agent restarted so the change applies to this turn."
        }
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
        idle_stop: Duration,
        policy: AssistantPolicy,
        tmp_dir: PathBuf,
        checkpoints: Arc<TurnCheckpoints>,
        usage: Arc<UsageService>,
        insight: Arc<InsightHub>,
        sandbox: Arc<Sandbox>,
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
        // Checkpoint events go to every client; a chat's checkpoint state to its `ChatInfo`.
        checkpoints.connect(bus.clone(), notifier.clone());
        let manager = Arc::new(Self {
            bus,
            queues: Queues::new(storage.clone()),
            storage,
            notifier,
            codex,
            harnesses,
            providers,
            max_context_chars: Arc::new(AtomicUsize::new(max_context_chars)),
            idle_stop_ms: AtomicU64::new(millis(idle_stop)),
            policy: Mutex::new(policy),
            live: Mutex::new(HashMap::new()),
            asleep: Mutex::new(HashSet::new()),
            wakes: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            questions: Mutex::new(HashMap::new()),
            models: Mutex::new(HashMap::new()),
            external: Mutex::new(Vec::new()),
            memory: Mutex::new(None),
            mcp: OnceLock::new(),
            tmp_dir,
            checkpoints,
            usage,
            insight,
            sandbox,
            contained: Mutex::new(HashMap::new()),
        });
        tokio::spawn(Self::sleeper(Arc::downgrade(&manager)));
        manager
    }

    /// The boundary the chat's process was started in (protocol 0.52): what was applied to the
    /// spawn, `None` for a chat started without one.
    pub(crate) fn containment(&self, id: SessionId) -> Option<Containment> {
        lock(&self.contained).get(&id).cloned()
    }

    /// How far the running agent has taken each extra folder of the session (protocol 0.57),
    /// one per `spec.dirs` entry in order: what the agent says it holds (`now`), how the rest
    /// reaches it. A session without a running agent says `restart`: the folders are given at
    /// the next start. Contain's profile is fixed at spawn, so a folder it does not list is
    /// `restart` whatever the agent holds.
    pub(crate) fn dir_states(&self, id: SessionId, spec: &ChatSpec) -> Vec<ChatDirApplied> {
        let adapter = lock(&self.live)
            .get(&id)
            .and_then(|live| match &live.backend {
                Backend::Agent(adapter) => Some(adapter.clone()),
                Backend::Direct(_) => None,
            });
        let held = adapter.as_ref().and_then(|adapter| adapter.dirs());
        let profile = spec.contain.as_ref().map(|_| {
            self.containment(id)
                .map(|c| c.write_roots)
                .unwrap_or_default()
        });
        spec.dirs
            .iter()
            .map(|dir| {
                let Some(held) = &held else {
                    return ChatDirApplied::Restart;
                };
                if profile.as_ref().is_some_and(|roots| !roots.contains(dir)) {
                    return ChatDirApplied::Restart;
                }
                if held.given.contains(dir) {
                    return ChatDirApplied::Now;
                }
                match held.change {
                    DirsChange::NextTurn => ChatDirApplied::NextTurn,
                    DirsChange::Restart => ChatDirApplied::Restart,
                    DirsChange::Unsupported => ChatDirApplied::Unsupported,
                }
            })
            .collect()
    }

    /// Tells the running agent which extra folders the session has now (`chat.add_dir`,
    /// `chat.remove_dir`). Nothing to do for a session without a running agent: the folders are
    /// given at its next start. A contained session is only told the folders its boundary
    /// lists: a folder it cannot write would be announced to the agent as usable.
    pub(crate) async fn apply_dirs(&self, id: SessionId, spec: &ChatSpec) -> Result<(), ErrorBody> {
        let adapter = lock(&self.live)
            .get(&id)
            .and_then(|live| match &live.backend {
                Backend::Agent(adapter) => Some(adapter.clone()),
                Backend::Direct(_) => None,
            });
        let Some(adapter) = adapter else {
            return Ok(());
        };
        let dirs = match &spec.contain {
            None => spec.dirs.clone(),
            Some(_) => {
                let profile = self
                    .containment(id)
                    .map(|c| c.write_roots)
                    .unwrap_or_default();
                spec.dirs
                    .iter()
                    .filter(|dir| profile.contains(dir))
                    .cloned()
                    .collect()
            }
        };
        adapter.set_dirs(dirs).await.map_err(|error| match error {
            AgentError::Unsupported(_) => ErrorBody::new(ErrorCode::Unsupported, error.to_string()),
            error => ErrorBody::new(ErrorCode::ProviderError, error.to_string()),
        })
    }

    /// Contain's service: it also knows the places a session folder may not be.
    pub(crate) fn sandbox(&self) -> &Arc<Sandbox> {
        &self.sandbox
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

    /// Pending approvals of another service (the control gate, protocol 0.21; Mawas's memory
    /// proposals, 0.32) that `approval.list` and `approval.respond` serve next to the agents'
    /// own. Weak: the services own the registry that owns this manager.
    pub(crate) fn add_external_approvals(&self, source: Weak<dyn ApprovalSource>) {
        lock(&self.external).push(source);
    }

    fn external_approvals(&self) -> Vec<Arc<dyn ApprovalSource>> {
        lock(&self.external)
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }

    /// Mawas's memory: what its conversations are given at their start and between turns.
    pub(crate) fn set_memory_context(&self, memory: Weak<dyn MemoryContext>) {
        *lock(&self.memory) = Some(memory);
    }

    fn memory_context(&self) -> Option<Arc<dyn MemoryContext>> {
        lock(&self.memory).as_ref().and_then(Weak::upgrade)
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
    async fn codex_for(
        &self,
        session: SessionId,
        account: Option<&str>,
    ) -> Result<CodexConfig, ErrorBody> {
        let mut codex = self.resolved_codex(account).await?;
        if let Some(server) = self.mcp_server(session) {
            codex.args.extend(server.codex_overrides());
        }
        Ok(codex)
    }

    /// `[codex]` with its program found the way the harnesses are (`PATH` of the login
    /// environment, then the usual install folders) and that environment underneath the
    /// configured one. When it is not found the configuration stays as it is and starting it
    /// reports the error.
    ///
    /// With `account` (a managed account, protocol 0.50) Codex runs with that account's
    /// `CODEX_HOME`; an account that does not exist is an error rather than a silent fall back
    /// to the user's own login.
    async fn resolved_codex(&self, account: Option<&str>) -> Result<CodexConfig, ErrorBody> {
        let account_env = account
            .map(|name| self.harnesses.accounts().launch_env(AGENT_CODEX, name))
            .transpose()?;
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
        // The account's home wins over everything, found or not.
        if let Some(account_env) = account_env {
            for (name, value) in account_env.set {
                codex.env.retain(|(k, _)| *k != name);
                codex.env.push((name, value));
            }
        }
        Ok(codex)
    }

    /// The harnesses (and the managed accounts they run as).
    pub(crate) fn harnesses(&self) -> &Arc<Harnesses> {
        &self.harnesses
    }

    /// Whether the session has a running backend (not merely a record).
    pub(crate) fn is_running(&self, id: SessionId) -> bool {
        lock(&self.live).contains_key(&id)
    }

    // Idle assistants --------------------------------------------------------------------------

    /// `assistant.idle_stop_secs` from now on (`Duration::ZERO`: never).
    pub(crate) fn set_assistant_idle_stop(&self, idle: Duration) {
        self.idle_stop_ms.store(millis(idle), Ordering::Relaxed);
    }

    /// `assistant.mode` and `assistant.max_actions_per_turn` from now on. The agent of an
    /// assistant that runs in another mode restarts before its next turn ([`Self::awake`]);
    /// the control gate reads the mode for each request.
    pub(crate) fn set_assistant_policy(&self, mode: AssistantMode, max_actions: u32) {
        *lock(&self.policy) = AssistantPolicy { mode, max_actions };
    }

    /// What Mawas may do, now.
    pub(crate) fn assistant_policy(&self) -> AssistantPolicy {
        *lock(&self.policy)
    }

    /// The turn the session's journal shows as open, if any: the control gate counts Mawas's
    /// changes per turn.
    pub(crate) fn current_turn(&self, id: SessionId) -> Option<TurnId> {
        let live = lock(&self.live).get(&id).cloned()?;
        *lock(&live.open_turn)
    }

    /// `spec` as it is started now: an assistant's session mode, approval policy and sandbox
    /// follow `assistant.mode` (`assistant_profile`), whatever was stored when it was created,
    /// so a mode change applies at the next start, wake or restore.
    pub(crate) fn launch_spec(&self, spec: &ChatSpec) -> ChatSpec {
        if !spec.assistant {
            return spec.clone();
        }
        let profile = assistant_profile(&spec.agent, self.assistant_policy().mode);
        let mut launch = spec.clone();
        if is_harness_agent(&spec.agent) {
            launch.mode = profile.session_mode.map(str::to_owned);
        } else {
            launch.approval_policy = profile.approval_policy;
            launch.sandbox = profile.sandbox;
        }
        launch
    }

    /// Whether the session is an assistant whose agent is stopped: idle for its clients, with
    /// no process until something needs the agent.
    pub(crate) fn is_asleep(&self, id: SessionId) -> bool {
        lock(&self.asleep).contains(&id)
    }

    /// The session has no agent process until it is used: an assistant a daemon restart brought
    /// back, or one whose wake failed.
    pub(crate) fn mark_asleep(&self, id: SessionId) {
        lock(&self.asleep).insert(id);
    }

    /// Takes the session out of the sleeping ones because its agent is being started; whether it
    /// was asleep.
    pub(crate) fn take_asleep(&self, id: SessionId) -> bool {
        lock(&self.asleep).remove(&id)
    }

    /// The session ended without an agent to end (it was asleep): nothing of it is kept here.
    pub(crate) fn forget_sleeper(&self, id: SessionId) {
        lock(&self.asleep).remove(&id);
        lock(&self.wakes).remove(&id);
    }

    /// Looks at the assistants every so often and stops the agent of each that has been idle
    /// for `assistant.idle_stop_secs` (read at every look, so a change applies live). Ends with
    /// the manager.
    async fn sleeper(manager: Weak<Self>) {
        loop {
            let Some(this) = manager.upgrade() else {
                return;
            };
            let idle = Duration::from_millis(this.idle_stop_ms.load(Ordering::Relaxed));
            let wait = if idle.is_zero() {
                SLEEP_CHECK_MAX
            } else {
                this.put_idle_to_sleep(idle);
                (idle / 4).clamp(SLEEP_CHECK_MIN, SLEEP_CHECK_MAX)
            };
            drop(this);
            tokio::time::sleep(wait).await;
        }
    }

    fn put_idle_to_sleep(&self, idle: Duration) {
        let due: Vec<Arc<Live>> = lock(&self.live)
            .values()
            .filter(|live| {
                live.assistant
                    && matches!(live.backend, Backend::Agent(_))
                    && !live.busy.load(Ordering::Acquire)
                    && !live.stopping.load(Ordering::Acquire)
                    && live.idle_for() >= idle
            })
            .cloned()
            .collect();
        for live in due {
            self.put_to_sleep(&live, idle);
        }
    }

    /// Stops the agent of an idle assistant. The session stays what it is for its clients: its
    /// exit is reported as sleep, not as an end ([`Self::finish`]), and the next message starts
    /// the agent again ([`Self::awake`]).
    fn put_to_sleep(&self, live: &Arc<Live>, idle: Duration) {
        let waiting = || {
            !self.approval_list(Some(live.id)).approvals.is_empty()
                || !self.question_list(Some(live.id)).questions.is_empty()
        };
        if waiting() {
            return;
        }
        // `sleeping` before the claim: a message that finds `busy` taken then knows it is the
        // sleeper's, not a turn, and waits for the wake instead of failing.
        live.sleeping.store(true, Ordering::Release);
        let claimed = live
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if !claimed || waiting() || live.idle_for() < idle {
            if claimed {
                live.busy.store(false, Ordering::Release);
            }
            live.sleeping.store(false, Ordering::Release);
            return;
        }
        // Asleep before the agent goes: the session is never neither running nor asleep.
        lock(&self.asleep).insert(live.id);
        info!(session = %live.id, "assistant {} asleep, process stopped after {} s idle", live.id, live.idle_for().as_secs());
        let live = live.clone();
        tokio::spawn(async move {
            if let Backend::Agent(adapter) = &live.backend {
                adapter.shutdown().await;
            }
        });
    }

    /// The session's running backend. A sleeping assistant is woken first (its agent starts
    /// again and continues the conversation), and one that is going to sleep is waited for.
    async fn awake(&self, id: SessionId) -> Result<Arc<Live>, ErrorBody> {
        let deadline = Instant::now() + SLEEP_WAIT;
        loop {
            let live = lock(&self.live).get(&id).cloned();
            match live {
                Some(live) if !live.sleeping.load(Ordering::Acquire) => {
                    // `assistant.mode` changed since this agent started: it starts again, in
                    // the new mode, before it takes the message.
                    if live.assistant
                        && matches!(live.backend, Backend::Agent(_))
                        && live.mode != self.assistant_policy().mode
                        && self.restart_for_mode(&live).await
                    {
                        continue;
                    }
                    return Ok(live);
                }
                Some(_) => {
                    if Instant::now() >= deadline {
                        return Err(ErrorBody::new(
                            ErrorCode::InvalidState,
                            format!("chat session {id} is going to sleep; send again"),
                        ));
                    }
                    tokio::time::sleep(SLEEP_CHECK_MIN).await;
                }
                None if self.is_asleep(id) => self.wake(id).await?,
                None => return self.live(id),
            }
        }
    }

    /// Stops the agent of an assistant that runs in another mode than `assistant.mode` says,
    /// the way the idle sleeper does: the session stays what it is for its clients, and the
    /// caller wakes it, which starts the agent in the current mode and continues the stored
    /// conversation. The journal says so. Not while a turn runs or anything waits for the
    /// user (the change applies to the next turn): `false` then.
    async fn restart_for_mode(&self, live: &Arc<Live>) -> bool {
        let waiting = || {
            !self.approval_list(Some(live.id)).approvals.is_empty()
                || !self.question_list(Some(live.id)).questions.is_empty()
        };
        if waiting() {
            return false;
        }
        live.sleeping.store(true, Ordering::Release);
        let claimed = live
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if !claimed || waiting() {
            if claimed {
                live.busy.store(false, Ordering::Release);
            }
            live.sleeping.store(false, Ordering::Release);
            return false;
        }
        lock(&self.asleep).insert(live.id);
        let mode = self.assistant_policy().mode;
        info!(session = %live.id, from = live.mode.as_str(), to = mode.as_str(), "assistant mode changed; restarting its agent");
        let notice = AgentEventDraft::new(AgentEventKind::Notice {
            level: NoticeLevel::Info,
            message: mode_notice(mode).to_owned(),
        });
        self.handle(live, notice).await;
        let live = live.clone();
        tokio::spawn(async move {
            if let Backend::Agent(adapter) = &live.backend {
                adapter.shutdown().await;
            }
        });
        true
    }

    /// Starts the agent of a sleeping assistant (the registry knows how); returns once it runs.
    async fn wake(&self, id: SessionId) -> Result<(), ErrorBody> {
        let gate = lock(&self.wakes).entry(id).or_default().clone();
        let _one_at_a_time = gate.lock().await;
        // Another message woke it while this one waited.
        if lock(&self.live).contains_key(&id) || !self.is_asleep(id) {
            return Ok(());
        }
        self.notifier.wake(id).await
    }

    /// Starts the agent of a sleeping assistant, continuing its conversation the way a restart
    /// does. An agent that refuses to continue it starts a new conversation and the journal
    /// says so. Called by the registry on behalf of [`Self::wake`].
    pub(crate) async fn start_waking(
        self: &Arc<Self>,
        id: SessionId,
        cwd: PathBuf,
        spec: &ChatSpec,
        scope: Scope,
    ) -> Result<Started, ErrorBody> {
        // The assistant's turns have checkpoints from its first one on (see `start`).
        self.checkpoints.open(id, cwd.clone(), scope);
        let started = self.wake_agent(id, cwd, spec).await;
        if started.is_err() {
            self.checkpoints.close(id);
        }
        started
    }

    async fn wake_agent(
        self: &Arc<Self>,
        id: SessionId,
        cwd: PathBuf,
        spec: &ChatSpec,
    ) -> Result<Started, ErrorBody> {
        // An assistant that was opened but never started has neither a conversation nor a
        // model yet: its model is picked now, the way `assistant.open` used to do it up front.
        let picked;
        let spec = if spec.assistant
            && spec.model.is_none()
            && spec.thread_id.is_none()
            && !is_gateway_agent(&spec.agent)
        {
            picked = ChatSpec {
                model: self.pick_first_model(&spec.agent).await,
                ..spec.clone()
            };
            &picked
        } else {
            spec
        };
        let first = self.start_agent(id, cwd.clone(), spec).await;
        let refused = match first {
            Ok(started) => return Ok(started),
            Err(failure) if failure.resume_refused && spec.thread_id.is_some() => failure,
            Err(failure) => return Err(failure.error),
        };
        warn!(session = %id, message = %refused.error.message, "the agent would not continue the conversation; starting a new one");
        let fresh = ChatSpec {
            thread_id: None,
            ..spec.clone()
        };
        let started = self
            .start_agent(id, cwd, &fresh)
            .await
            .map_err(|failure| failure.error)?;
        if let Ok(live) = self.live(id) {
            let notice = AgentEventDraft::new(AgentEventKind::Notice {
                level: NoticeLevel::Info,
                message: NEW_CONVERSATION_NOTICE.to_owned(),
            });
            self.handle(&live, notice).await;
        }
        Ok(started)
    }

    /// The model a harness assistant starts on when none was configured (AM-1): the backend's
    /// catalog through [`pick_assistant_model`]. `None` when the catalog is empty, unreadable or
    /// slow: the harness's own default then runs.
    async fn pick_first_model(&self, agent: &str) -> Option<String> {
        match tokio::time::timeout(CATALOG_TIMEOUT, self.models(agent)).await {
            Ok(Ok(catalog)) => pick_assistant_model(&catalog.models),
            Ok(Err(error)) => {
                debug!(agent, message = %error.message, "no model catalog for the assistant; the backend's default runs");
                None
            }
            Err(_) => {
                debug!(
                    agent,
                    "the model catalog was too slow for the assistant; the backend's default runs"
                );
                None
            }
        }
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
    /// journaling its events. An agent chat's turns get checkpoints, a harness assistant's
    /// (Mawas's) included — they work where the assistant works, and say why when that is not
    /// a git repository (`scope` names its project for the per-project settings); a direct
    /// chat's and the provider-backed assistant's do not.
    pub async fn start(
        self: &Arc<Self>,
        id: SessionId,
        cwd: PathBuf,
        spec: &ChatSpec,
        scope: Scope,
    ) -> Result<Started, ErrorBody> {
        // A fork's journal prefix is there before its agent (or its direct session) starts.
        if let Some(copy) = spec
            .fork
            .as_ref()
            .and_then(|fork| fork.pending.as_ref())
            .and_then(|pending| pending.copy.as_ref())
        {
            self.copy_fork_journal(id, copy).await?;
        }
        if is_gateway_agent(&spec.agent) {
            return self.start_direct(id, spec).await;
        }
        // Before the agent runs: its first turn may follow at once.
        self.checkpoints.open(id, cwd.clone(), scope);
        lock(&self.contained).remove(&id);
        let started = self.start_agent(id, cwd, spec).await;
        if started.is_err() {
            self.checkpoints.close(id);
            lock(&self.contained).remove(&id);
        }
        started.map_err(|failure| failure.error)
    }

    /// Checkpoints of chat turns (ADR 0019).
    pub(crate) fn checkpoints(&self) -> &Arc<TurnCheckpoints> {
        &self.checkpoints
    }

    /// Starts (or resumes) the agent process of a Codex or harness chat.
    async fn start_agent(
        self: &Arc<Self>,
        id: SessionId,
        cwd: PathBuf,
        spec: &ChatSpec,
    ) -> Result<Started, StartFailure> {
        let launch = self.launch_spec(spec);
        let spec = &launch;
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
            )
            .into());
        }
        let last = self.journal_end(id).await?;
        // Mawas's memory is the thread's developer instructions (`developerInstructions`),
        // next to Codex's own base instructions.
        let memory = if spec.assistant {
            self.start_memory(id, "codex", spec).await
        } else {
            None
        };
        let forking = fork_from(spec).is_some();
        let start = StartSpec {
            cwd,
            model: spec.model.clone(),
            approval_policy: spec.approval_policy,
            sandbox: spec.sandbox,
            resume_thread_id: spec.thread_id.clone(),
            fork: fork_from(spec),
            developer_instructions: memory.map(|memory| memory.text).or_else(|| {
                // A summary fork's fresh thread is given the digest as its instructions: it is
                // not the user's words, and the thread keeps it from then on.
                (spec.thread_id.is_none())
                    .then(|| spec.fork.as_ref().and_then(|fork| fork.seed.clone()))
                    .flatten()
            }),
            session_meta: None,
            dirs: crate::session_dirs::existing(&spec.dirs),
            dirs_managed: spec.dirs_managed,
        };
        let codex = self.codex_for(id, spec.account.as_deref()).await?;
        let CodexSession {
            adapter,
            started,
            events,
        } = CodexAppServerAdapter::start(&codex, &start)
            .await
            .map_err(|e| {
                let error = fork::forked_error(forking, &e, provider_error(&e, &codex.program));
                StartFailure::agent(&e, error)
            })?;
        let live = Live::new(
            id,
            Backend::Agent(adapter),
            last,
            spec.assistant,
            self.assistant_policy().mode,
            spec.assistant,
            LiveUsage::new(
                Attribution::harness(AGENT_CODEX, Some(&started.model))
                    .with_account(spec.account.clone()),
                !started.resumed,
            ),
        );
        // A summary fork's digest went in as the thread's developer instructions, above.
        self.attach_insight(&live, Some(&start.cwd), !started.resumed)
            .await;
        self.adopt(&live);
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
    ) -> Result<Started, StartFailure> {
        let harnesses = self.harnesses.clone();
        let agent = spec.agent.clone();
        let account = spec.account.clone();
        let safety = spec.safety;
        let mode = spec.assistant.then(|| self.assistant_policy().mode);
        // Mawas's memory: OMP gets it in a private prompt file named on its command line (never
        // in argv itself), Claude Code on its first turn, below.
        let memory = if spec.assistant {
            self.start_memory(id, harness_backend(&spec.agent), spec)
                .await
        } else {
            None
        };
        let prompt_file = match (&memory, mode) {
            (Some(memory), Some(mode)) if spec.agent == AGENT_OMP => {
                self.omp_prompt_file(memory, mode)
            }
            _ => None,
        };
        let config = tokio::task::spawn_blocking(move || {
            harnesses.acp_config_with(
                &agent,
                mode,
                prompt_file.as_deref(),
                account.as_deref(),
                safety,
            )
        })
        .await
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("locating the agent: {e}")))??;
        // What the errors below call the program, not the Seatbelt tool that runs it.
        let program = config.program.clone();
        // The folders the process is started with; a contained OMP can write in them too (the
        // profile is fixed now).
        let launch_dirs = crate::session_dirs::existing(&spec.dirs);
        let config = match &spec.contain {
            None => config,
            Some(request) => {
                let sandbox = self.sandbox.clone();
                let request = crate::session_dirs::contain_with(request, &launch_dirs);
                let dir = cwd.clone();
                let planned = tokio::task::spawn_blocking(move || {
                    let fragment = Fragment::omp(sandbox.user_home(), &config.env);
                    sandbox
                        .plan(&dir, &request, &fragment)
                        .map(|plan| (plan, config))
                })
                .await
                .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("planning Contain: {e}")))?
                .map_err(ErrorBody::from)?;
                let (plan, config) = planned;
                let (wrapped, args) = plan.wrap(config.program.as_os_str(), &config.args);
                lock(&self.contained).insert(id, plan.containment);
                AcpConfig {
                    program: wrapped,
                    args,
                    ..config
                }
            }
        };
        let last = self.journal_end(id).await?;
        let forking = fork_from(spec).is_some();
        let start = StartSpec {
            cwd,
            model: spec.model.clone(),
            approval_policy: spec.approval_policy,
            sandbox: spec.sandbox,
            resume_thread_id: spec.thread_id.clone(),
            developer_instructions: None,
            fork: fork_from(spec),
            session_meta: spec
                .safety
                .and_then(|preset| crate::safety::claude_session_meta(&spec.agent, preset)),
            dirs: launch_dirs,
            dirs_managed: spec.dirs_managed,
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
            .map_err(|e| {
                let error =
                    fork::forked_error(forking, &e, harness_error(&e, &spec.agent, &program));
                StartFailure::agent(&e, error)
            })?;
        if !models.is_empty() {
            lock(&self.models).insert(spec.agent.clone(), (Instant::now(), models));
        }
        let live = Live::new(
            id,
            Backend::Agent(adapter),
            last,
            spec.assistant,
            self.assistant_policy().mode,
            spec.assistant,
            LiveUsage::new(
                Attribution::harness(&spec.agent, Some(&started.model))
                    .with_account(spec.account.clone()),
                !started.resumed,
            ),
        );
        if spec.agent == AGENT_CLAUDE
            && let Some(memory) = memory
            && memory.fresh
        {
            // Claude Code has no instruction channel Hutan can rely on: the memory rides on the
            // conversation's first turn, labelled as Hutan's.
            *lock(&live.first_turn) = Some(memory.text);
        }
        *lock(&live.seed) = spec.fork.as_ref().and_then(|fork| fork.seed.clone());
        self.attach_insight(&live, Some(&start.cwd), !started.resumed)
            .await;
        self.adopt(&live);
        info!(session = %id, agent = %spec.agent, model = %started.model, resumed = started.resumed, "chat harness started");
        tokio::spawn(self.clone().pump(live, events));
        Ok(started)
    }

    /// Makes `live` the session's running backend; a sleeping session wakes.
    fn adopt(&self, live: &Arc<Live>) {
        let steer = match &live.backend {
            Backend::Agent(adapter) => adapter.can_steer(),
            Backend::Direct(_) => false,
        };
        self.queues.set_steer(live.id, steer);
        let mut running = lock(&self.live);
        running.insert(live.id, live.clone());
        lock(&self.asleep).remove(&live.id);
    }

    /// The memory a Mawas conversation starts with, and the line about the project's
    /// `AGENTS.md` (`None`: memory is off and the project has none, or there is nothing to give).
    async fn start_memory(
        &self,
        id: SessionId,
        backend: &'static str,
        spec: &ChatSpec,
    ) -> Option<StartContext> {
        let context = self.memory_context()?;
        let resumed = spec.thread_id.is_some();
        let memory = context.open(id, backend, resumed).await;
        // The project's AGENTS.md rides with it (`crate::project_instructions`), also when
        // memory is off.
        let project = context.project_instructions(id).await;
        crate::project_instructions::combine(memory, project, id, resumed)
    }

    /// The prompt file of an OMP assistant: its Mawas prompt for `mode` and the memory, private
    /// to the daemon. `None` (the plain prompt on the command line) when it cannot be written:
    /// the assistant still starts, without memory.
    fn omp_prompt_file(&self, memory: &StartContext, mode: AssistantMode) -> Option<PathBuf> {
        let context = self.memory_context()?;
        let content = format!("{}\n\n{}\n", omp_assistant_prompt(mode), memory.text);
        match context.prompt_file(&memory.snapshot_id, &content) {
            Ok(path) => Some(path),
            Err(error) => {
                warn!(%error, "could not write the memory prompt file; the assistant starts without memory");
                None
            }
        }
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
        // Mawas's memory is a system message ahead of the conversation, fixed for this start.
        let mut system_prompt = spec.system_prompt.clone();
        if spec.agent == AGENT_ASSISTANT
            && let Some(memory) = self.start_memory(id, "provider", spec).await
        {
            system_prompt = Some(match system_prompt {
                Some(prompt) => format!("{prompt}\n\n{}", memory.text),
                None => memory.text,
            });
        }
        let session = DirectSession::new(
            id,
            self.storage.clone(),
            self.providers.clone(),
            tx,
            self.max_context_chars.clone(),
            Target {
                provider_id: provider_id.clone(),
                model: model.clone(),
                system_prompt,
            },
            usage,
        );
        let live = Live::new(
            id,
            Backend::Direct(session),
            last,
            false,
            AssistantMode::default(),
            spec.agent == AGENT_ASSISTANT,
            LiveUsage::new(
                Attribution::direct(&provider_id, &model),
                spec.thread_id.is_none(),
            ),
        );
        self.attach_insight(&live, None, spec.thread_id.is_none())
            .await;
        self.adopt(&live);
        info!(session = %id, provider = %provider_id, %model, "direct chat started");
        tokio::spawn(self.clone().pump(live, events));
        Ok(Started {
            model,
            // There is no provider-side conversation: the session is its own thread.
            thread_id: id.to_string(),
            resumed: spec.thread_id.is_some(),
            // The provider's model list answers when an image is attached.
            images: None,
            mode: None,
            reported: None,
            // The journal is the conversation: any turn is a cut.
            fork: Some(ForkSupport::Exact),
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
        let live = self.awake(id).await?;
        match &live.backend {
            Backend::Direct(session) => {
                *lock(&live.usage.attribution) = Attribution::direct(&provider_id, &model);
                session.set_model(provider_id, model).await;
                Ok(())
            }
            Backend::Agent(adapter) => {
                adapter
                    .set_model(model.clone())
                    .await
                    .map_err(|error| match error {
                        AgentError::Unsupported(_) | AgentError::UnknownModel { .. } => {
                            ErrorBody::new(ErrorCode::BadRequest, error.to_string())
                        }
                        error => ErrorBody::new(ErrorCode::ProviderError, error.to_string()),
                    })?;
                let mut attribution = lock(&live.usage.attribution);
                *attribution = Attribution::harness(&attribution.harness.clone(), Some(&model))
                    .with_account(attribution.account.clone());
                Ok(())
            }
        }
    }

    /// Builds the insight of a chat whose agent just started from its journal, so that every
    /// event of the run lands in it (`fresh`: the conversation began with this start).
    async fn attach_insight(&self, live: &Live, cwd: Option<&std::path::Path>, fresh: bool) {
        let attribution = lock(&live.usage.attribution).clone();
        let target = InsightTarget {
            harness: attribution.harness,
            model: attribution.model,
            provider: Some(attribution.provider),
            cwd: cwd.map(|cwd| cwd.display().to_string()),
            assistant: live.assistant || live.mawas,
        };
        self.insight.attach(live.id, &target, fresh).await;
    }

    /// `agent.measure_context`: sends `/context` to the harness as a turn and waits for its end
    /// (the insight reads the answer from the journal's events: `hutan_core::insight`). The
    /// turn is part of the conversation, costs no tokens, and is sent only because a client
    /// asked. `unsupported` for an agent without such a command and for assistants,
    /// `invalid_state` while a turn runs or the session is not running.
    pub async fn measure_context(&self, id: SessionId) -> Result<TurnId, ErrorBody> {
        let live = match self.acquire(id).await {
            Ok(live) => live,
            Err(Acquire::Busy(_)) => return Err(turn_in_progress()),
            Err(Acquire::Failed(error)) => return Err(error),
        };
        let supported = match &live.backend {
            Backend::Agent(adapter) => {
                hutan_core::insight::harness_insight(adapter.agent()).can_measure
            }
            Backend::Direct(_) => false,
        };
        if !supported || live.assistant || live.mawas {
            live.busy.store(false, Ordering::Release);
            return Err(crate::insight::measure_unsupported(
                &lock(&live.usage.attribution).harness,
            ));
        }
        let (tx, ended) = oneshot::channel();
        *lock(&live.measuring) = Some(tx);
        let turn = match self
            .run_turn(
                &live,
                hutan_core::insight::MEASURE_COMMAND.to_owned(),
                None,
                false,
            )
            .await
        {
            Ok(turn) => turn,
            Err(error) => {
                lock(&live.measuring).take();
                return Err(error);
            }
        };
        match tokio::time::timeout(MEASURE_TIMEOUT, ended).await {
            Ok(Ok(MeasureEnd::Completed)) => Ok(turn),
            Ok(Ok(MeasureEnd::Failed(message))) => Err(ErrorBody::new(
                ErrorCode::ProviderError,
                format!("the agent did not answer /context: {message}"),
            )),
            Ok(Err(_)) => Err(ErrorBody::new(
                ErrorCode::ProviderError,
                "the agent went away before it answered /context",
            )),
            Err(_) => {
                lock(&live.measuring).take();
                Err(ErrorBody::new(
                    ErrorCode::ProviderError,
                    format!(
                        "the agent did not answer /context within {} seconds",
                        MEASURE_TIMEOUT.as_secs()
                    ),
                ))
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

    /// Sends the user's message; the turn's events follow on the bus. A queue that held starts
    /// again: the user went on with the conversation.
    pub async fn send(
        &self,
        id: SessionId,
        text: String,
        attached: Option<Attached>,
    ) -> Result<TurnId, ErrorBody> {
        let turn = self.start_turn(id, text, attached, false).await?;
        self.release_queue(id);
        Ok(turn)
    }

    /// `chat.retry`: the conversation's last user message again, as a new turn that does not
    /// journal it a second time.
    pub async fn retry(&self, id: SessionId) -> Result<TurnId, ErrorBody> {
        let storage = self.storage.clone();
        let rows = tokio::task::spawn_blocking(move || {
            storage
                .chat()
                .items_of_kind(id, "message_end", RETRY_LOOKBACK)
        })
        .await
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))?
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))?;
        // Newest first.
        let text = rows
            .iter()
            .filter_map(|row| serde_json::from_str::<AgentEvent>(&row.event_json).ok())
            .find_map(|event| match event.kind {
                AgentEventKind::MessageEnd {
                    role: hutan_core::agent::MessageRole::User,
                    text,
                    ..
                } if !text.trim().is_empty() => Some(text),
                _ => None,
            })
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::InvalidState,
                    "there is nothing to retry: the chat has no message of the user yet",
                )
            })?;
        let turn = self.start_turn(id, text, None, true).await?;
        self.release_queue(id);
        Ok(turn)
    }

    /// A turn with `text`; `retry`: the message is the journal's last user message, which the
    /// turn does not journal again.
    async fn start_turn(
        &self,
        id: SessionId,
        text: String,
        attached: Option<Attached>,
        retry: bool,
    ) -> Result<TurnId, ErrorBody> {
        check_message(&text, attached.is_some())?;
        let live = match self.acquire(id).await {
            Ok(live) => live,
            Err(Acquire::Busy(_)) => return Err(turn_in_progress()),
            Err(Acquire::Failed(error)) => return Err(error),
        };
        self.run_turn(&live, text, attached, retry).await
    }

    /// The session's backend, awake, with `busy` taken for a turn that is about to start.
    async fn acquire(&self, id: SessionId) -> Result<Arc<Live>, Acquire> {
        loop {
            let live = self.awake(id).await.map_err(Acquire::Failed)?;
            if !live.busy.swap(true, Ordering::AcqRel) {
                return Ok(live);
            }
            // Held by the idle sleeper, not by a turn: the agent is going away, wake it.
            if !live.sleeping.load(Ordering::Acquire) {
                return Err(Acquire::Busy(live));
            }
        }
    }

    /// Starts a turn with `text` on `live`, whose `busy` the caller took ([`Self::acquire`]); a
    /// turn that does not start gives it back.
    async fn run_turn(
        &self,
        live: &Arc<Live>,
        text: String,
        attached: Option<Attached>,
        retry: bool,
    ) -> Result<TurnId, ErrorBody> {
        let id = live.id;
        // The checkout as it is before the agent hears of this turn (ADR 0019): the prompt
        // waits at most the start budget for it, then goes, and the turn's checkpoint is late.
        let checkpoint = self.checkpoints.begin(id).await;
        // Mawas's memory: the first turn of a Claude Code conversation carries it, and any turn
        // carries what changed since the conversation last heard of it.
        let memory = self.turn_memory(live).await;
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
                            ..Default::default()
                        },
                        Some(attached.meta),
                    ),
                    Some(attached) => (
                        text,
                        MessageExtras {
                            context: attached.blocks,
                            images: attached.images,
                            ..Default::default()
                        },
                        Some(attached.meta),
                    ),
                    None => (text, MessageExtras::default(), None),
                };
                // What Hutan adds (Mawas's memory) is labelled as Hutan's, apart from what the
                // user typed and attached: a resource of its own for the ACP harnesses, a
                // delimited block after the message for Codex.
                let text = if codex {
                    memory.appended_to(text)
                } else {
                    extras.context.splice(0..0, memory.blocks.iter().cloned());
                    extras.unjournaled.clone_from(&memory.hidden);
                    text
                };
                let prepared = if codex {
                    self.give_images_a_file(live, &mut extras.images).await
                } else {
                    Ok(())
                };
                match prepared {
                    Ok(()) => {
                        {
                            let mut delivery = lock(&live.delivery);
                            delivery.meta = meta;
                            delivery.retry = retry;
                        }
                        let sent = adapter
                            .send_message_with(text, extras)
                            .await
                            .map(|turn| turn.turn_id)
                            .map_err(|error| match error {
                                AgentError::Unsupported(_) => {
                                    ErrorBody::new(ErrorCode::Unsupported, error.to_string())
                                }
                                AgentError::TurnInProgress => turn_in_progress(),
                                error => {
                                    ErrorBody::new(ErrorCode::ProviderError, error.to_string())
                                }
                            });
                        if sent.is_err() {
                            self.drop_temp_images(live);
                        }
                        sent
                    }
                    Err(error) => {
                        self.drop_temp_images(live);
                        Err(error)
                    }
                }
            }
            Backend::Direct(session) => {
                let text = memory.appended_to(text);
                session.send(text, attached, retry).await
            }
        };
        if let Some(checkpoint) = checkpoint {
            match &started {
                Ok(turn) => self.checkpoints.sent(id, checkpoint, *turn),
                Err(_) => self.checkpoints.abandoned(id, checkpoint),
            }
        }
        if started.is_err() {
            live.busy.store(false, Ordering::Release);
            memory.undo(live);
        } else {
            self.memory_delivered(live, memory).await;
        }
        started
    }

    /// What Hutan adds to the turn `live` is about to start.
    async fn turn_memory(&self, live: &Live) -> TurnMemory {
        let mut memory = TurnMemory::default();
        // A summary fork's digest rides with the conversation's first message, labelled as
        // Hutan's, apart from what the user typed (the same channel as Mawas's memory).
        if let Some(text) = lock(&live.seed).take() {
            memory.hidden.push(ContextBlock {
                label: "Earlier conversation (summary)".to_owned(),
                text: text.clone(),
            });
            memory.notices.push(
                "Hutan gave the agent the summary of the parent chat's turns with this first message."
                    .to_owned(),
            );
            memory.seed = Some(text);
        }
        if !live.mawas {
            return memory;
        }
        if let Some(text) = lock(&live.first_turn).take() {
            memory.blocks.push(ContextBlock {
                label: "Hutan memory".to_owned(),
                text: text.clone(),
            });
            memory.notices.push(
                "Hutan memory added to this conversation's first turn (the backend has no instruction channel of its own).".to_owned(),
            );
            memory.first_turn = Some(text);
        }
        if let Some(context) = self.memory_context()
            && let Some(delta) = context.prepare_delta(live.id).await
        {
            memory.blocks.push(ContextBlock {
                label: "Hutan memory update".to_owned(),
                text: delta.text.clone(),
            });
            memory.notices.push(delta.notice.clone());
            memory.delta = Some(delta);
        }
        memory
    }

    /// The turn that carried `memory` was accepted: the delta is now what the conversation
    /// knows, and the journal says what Hutan added.
    async fn memory_delivered(&self, live: &Live, memory: TurnMemory) {
        if let Some(delta) = memory.delta
            && let Some(context) = self.memory_context()
        {
            context.commit_delta(delta).await;
        }
        if memory.seed.is_some() {
            self.notifier.seed_delivered(live.id).await;
        }
        for message in memory.notices {
            let notice = AgentEventDraft::new(AgentEventKind::Notice {
                level: NoticeLevel::Info,
                message,
            });
            self.handle(live, notice).await;
        }
    }

    /// Stop: interrupts the turn in progress, and clears the queue (protocol 0.36) so that
    /// nothing queued starts after it, unless `keep_queue`, which keeps the queue and holds it.
    pub async fn interrupt(&self, id: SessionId, keep_queue: bool) -> Result<(), ErrorBody> {
        if !self.is_running(id) && self.is_asleep(id) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "no turn is in progress",
            ));
        }
        let live = self.live(id)?;
        // Before the turn can end: its end must find the queue cleared or holding.
        self.queues.load(id).await?;
        let changed = if keep_queue {
            self.queues.hold(id, QueueHold::Stopped)
        } else {
            self.queues.clear(id, true)
        };
        self.emit_queue(changed);
        self.interrupt_turn(&live).await
    }

    /// Asks the backend of `live` to stop its turn.
    async fn interrupt_turn(&self, live: &Live) -> Result<(), ErrorBody> {
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

    // Queue -------------------------------------------------------------------------------------

    fn emit_queue(&self, changed: Option<Changed>) {
        if let Some((queue, change)) = changed {
            self.emit(&Event::ChatQueueChanged { queue, change });
        }
    }

    /// A queue that held starts again (the user sent something).
    fn release_queue(&self, id: SessionId) {
        let released = self.queues.release(id);
        self.emit_queue(released);
    }

    /// `chat.queue.list`.
    pub async fn queue_list(&self, id: SessionId) -> Result<ChatQueue, ErrorBody> {
        self.queues.load(id).await?;
        Ok(self.queues.list(id))
    }

    /// `chat.queue.add`: at the end of the queue; sent at once when no turn runs and the queue
    /// does not hold.
    pub async fn queue_add(
        self: &Arc<Self>,
        id: SessionId,
        text: String,
    ) -> Result<QueuedMessage, ErrorBody> {
        check_message(&text, false)?;
        self.queues.load(id).await?;
        let (item, changed) = self.queues.add(id, text, false)?;
        self.emit_queue(Some(changed));
        self.deliver_soon(id);
        Ok(item)
    }

    /// `chat.queue.edit`.
    pub async fn queue_edit(
        &self,
        id: SessionId,
        item: u64,
        text: String,
    ) -> Result<QueuedMessage, ErrorBody> {
        check_message(&text, false)?;
        self.queues.load(id).await?;
        let (edited, changed) = self.queues.edit(id, item, text)?;
        self.emit_queue(Some(changed));
        Ok(edited)
    }

    /// `chat.queue.remove`.
    pub async fn queue_remove(&self, id: SessionId, item: u64) -> Result<(), ErrorBody> {
        self.queues.load(id).await?;
        let (_, queue) = self.queues.take(id, item)?;
        self.emit_queue(Some((
            queue,
            hutan_protocol::chat::QueueChange::Removed { item_id: item },
        )));
        Ok(())
    }

    /// `chat.queue.clear`.
    pub async fn queue_clear(&self, id: SessionId) -> Result<(), ErrorBody> {
        self.queues.load(id).await?;
        let changed = self.queues.clear(id, false);
        self.emit_queue(changed);
        Ok(())
    }

    /// `chat.send_now`: `text`, or the queued message `item`, without waiting for the turn.
    /// With no turn running it starts one (and the queue goes on after it); a running turn of
    /// an agent that steers takes it at its next tool boundary; any other running turn is
    /// interrupted, and the message goes first once it has ended.
    pub async fn send_now(
        self: &Arc<Self>,
        id: SessionId,
        text: Option<String>,
        item: Option<u64>,
    ) -> Result<ChatSentNow, ErrorBody> {
        let what = match (text, item) {
            (Some(text), None) => {
                check_message(&text, false)?;
                Now::Text(text)
            }
            (None, Some(item)) => Now::Item(item),
            _ => {
                return Err(ErrorBody::new(
                    ErrorCode::BadRequest,
                    "send either `text` or a queued message's `item_id`",
                ));
            }
        };
        self.queues.load(id).await?;
        if let Now::Item(item) = &what {
            // Gone already (sent, removed): say so before anything happens.
            let changed = self.queues.to_front(id, *item)?;
            self.emit_queue(Some(changed));
        }
        let live = match self.acquire(id).await {
            Ok(live) => live,
            Err(Acquire::Busy(live)) => return self.send_during_turn(&live, what).await,
            Err(Acquire::Failed(error)) => return Err(error),
        };
        // No turn runs: this is the next turn.
        let (text, taken) = match what {
            Now::Text(text) => (text, None),
            Now::Item(item) => match self.queues.take(id, item) {
                Ok((taken, _)) => (taken.text.clone(), Some(taken)),
                Err(error) => {
                    live.busy.store(false, Ordering::Release);
                    return Err(error);
                }
            },
        };
        match self.run_turn(&live, text, None, false).await {
            Ok(turn_id) => {
                if let Some(taken) = taken {
                    self.emit_queue(Some((
                        self.queues.list(id),
                        hutan_protocol::chat::QueueChange::Delivered {
                            item_id: taken.id,
                            turn_id,
                        },
                    )));
                }
                self.release_queue(id);
                Ok(ChatSentNow::Started { turn_id })
            }
            Err(error) => {
                if let Some(taken) = taken {
                    let changed = self.queues.put_back(id, taken, QueueHold::SendFailed);
                    self.emit_queue(Some(changed));
                }
                Err(error)
            }
        }
    }

    /// [`send_now`](Self::send_now) while `live` runs a turn.
    async fn send_during_turn(
        self: &Arc<Self>,
        live: &Arc<Live>,
        what: Now,
    ) -> Result<ChatSentNow, ErrorBody> {
        let id = live.id;
        if let Backend::Agent(adapter) = &live.backend
            && adapter.can_steer()
        {
            let (text, taken) = match what {
                Now::Text(text) => (text, None),
                Now::Item(item) => {
                    let (taken, _) = self.queues.take(id, item)?;
                    (taken.text.clone(), Some(taken))
                }
            };
            return match adapter.steer(text.clone()).await {
                Ok(turn_id) => {
                    if let Some(taken) = taken {
                        self.emit_queue(Some((
                            self.queues.list(id),
                            hutan_protocol::chat::QueueChange::Delivered {
                                item_id: taken.id,
                                turn_id,
                            },
                        )));
                    }
                    self.notice(
                        live,
                        NoticeLevel::Info,
                        "Sent now: the agent reads your message at its next step, without \
                         stopping the turn.",
                    )
                    .await;
                    Ok(ChatSentNow::Steered { turn_id })
                }
                // The turn ended meanwhile: the message goes first, as the next turn.
                Err(AgentError::NoTurn) => {
                    let item = match taken {
                        Some(taken) => {
                            let changed = self.queues.put_back(id, taken, QueueHold::SendFailed);
                            self.emit_queue(Some(changed));
                            None
                        }
                        None => Some(text),
                    };
                    if let Some(text) = item {
                        let (_, changed) = self.queues.add(id, text, true)?;
                        self.emit_queue(Some(changed));
                    }
                    self.release_queue(id);
                    self.deliver_soon(id);
                    Ok(ChatSentNow::Interrupting)
                }
                Err(error) => {
                    if let Some(taken) = taken {
                        let changed = self.queues.put_back(id, taken, QueueHold::SendFailed);
                        self.emit_queue(Some(changed));
                    }
                    Err(ErrorBody::new(ErrorCode::ProviderError, error.to_string()))
                }
            };
        }
        // The agent takes nothing while it works: the message goes first, the turn is stopped,
        // and its end sends it. The journal says the turn was interrupted for it.
        if let Now::Text(text) = what {
            let (_, changed) = self.queues.add(id, text, true)?;
            self.emit_queue(Some(changed));
        }
        self.release_queue(id);
        self.notice(
            live,
            NoticeLevel::Warning,
            "Send now interrupts the turn: this agent cannot take a message while it works. \
             Your message goes next.",
        )
        .await;
        match self.interrupt_turn(live).await {
            // The turn ended on its own meanwhile: its end may have found the queue holding.
            Ok(())
            | Err(ErrorBody {
                code: ErrorCode::InvalidState,
                ..
            }) => {}
            Err(error) => return Err(error),
        }
        self.deliver_soon(id);
        Ok(ChatSentNow::Interrupting)
    }

    /// Journals and shows a notice of Hutan's in the conversation.
    async fn notice(&self, live: &Live, level: NoticeLevel, message: &str) {
        let notice = AgentEventDraft::new(AgentEventKind::Notice {
            level,
            message: message.to_owned(),
        });
        self.handle(live, notice).await;
    }

    /// Sends the next queued message once nothing else runs (on a task of its own: a turn end
    /// is handled on the session's event pump, which the next turn's start must not wait on).
    fn deliver_soon(self: &Arc<Self>, id: SessionId) {
        if self.queues.has_next(id) {
            tokio::spawn(self.clone().deliver_next(id));
        }
    }

    /// Starts the next queued message as a turn, when no turn runs and the queue does not hold.
    /// A turn that runs now sends it when it ends.
    async fn deliver_next(self: Arc<Self>, id: SessionId) {
        if !self.queues.has_next(id) {
            return;
        }
        let Ok(live) = self.acquire(id).await else {
            return;
        };
        let Some((item, _)) = self.queues.take_next(id) else {
            live.busy.store(false, Ordering::Release);
            return;
        };
        match self.run_turn(&live, item.text.clone(), None, false).await {
            Ok(turn_id) => self.emit_queue(Some((
                self.queues.list(id),
                hutan_protocol::chat::QueueChange::Delivered {
                    item_id: item.id,
                    turn_id,
                },
            ))),
            Err(error) => {
                warn!(session = %id, error = %error.message, "a queued message could not be sent");
                let changed = self.queues.put_back(id, item, QueueHold::SendFailed);
                self.emit_queue(Some(changed));
                self.notice(
                    &live,
                    NoticeLevel::Warning,
                    &format!(
                        "The queued message could not be sent ({}). It waits first in the \
                         queue: send it again or edit it.",
                        error.message
                    ),
                )
                .await;
            }
        }
    }

    /// What follows a turn's end: a failed turn holds the queue, any other end sends the next
    /// queued message.
    fn after_turn(self: &Arc<Self>, live: &Live, failed: bool) {
        if live.stopping.load(Ordering::Acquire) {
            return;
        }
        if failed {
            let changed = self.queues.hold(live.id, QueueHold::TurnFailed);
            self.emit_queue(changed);
        } else {
            self.deliver_soon(live.id);
        }
    }

    /// Asks the agent to stop the running turn of one of its subagent threads.
    pub async fn interrupt_thread(
        &self,
        id: SessionId,
        thread: &str,
    ) -> Result<(), ThreadInterruptError> {
        if !self.is_running(id) && self.is_asleep(id) {
            return Err(ThreadInterruptError::NoTurn);
        }
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
        for source in self.external_approvals() {
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
        for source in self.external_approvals() {
            if let Some(answered) = source.respond(approval, option_id.clone(), client) {
                return answered.await;
            }
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
            let codex = self.resolved_codex(None).await?;
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
        let located = tokio::task::spawn_blocking(move || harnesses.acp_config(&name, None))
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
                        AdapterMessage::Event(draft) => {
                            let ended = draft.kind.ends_turn().then_some(matches!(
                                draft.kind,
                                AgentEventKind::TurnFailed { .. }
                            ));
                            self.handle(&live, *draft).await;
                            if let Some(failed) = ended {
                                self.after_turn(&live, failed);
                            }
                        }
                        AdapterMessage::Node(update) => {
                            // An ACP harness reports the session's cost on its root node.
                            if let Some(usage) = update.usage.as_ref()
                                && usage.kind == "acp.context"
                                && let Some(cost) = usage.cost_usd
                            {
                                lock(&live.usage.turn).cost_reported(cost);
                                self.insight.cost(live.id, cost);
                            }
                            self.notifier.node(live.id, *update).await;
                        }
                        AdapterMessage::Mode(mode) => self.notifier.mode(live.id, mode).await,
                        AdapterMessage::Limits(limits) => {
                            let (harness, account) = {
                                let attribution = lock(&live.usage.attribution);
                                (attribution.harness.clone(), attribution.account.clone())
                            };
                            self.usage
                                .limits_reported(&harness, account.as_deref(), &limits);
                        }
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
            delivery.retry = false;
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
        // A turn the agent never started (or never ended) still gets its end.
        self.checkpoints.close(live.id);
        self.drop_temp_images(live);
        // The idle sleeper stopped it, and nobody else asked: the session sleeps on.
        let slept = live.sleeping.load(Ordering::Acquire) && !live.stopping.load(Ordering::Acquire);
        if slept {
            debug!(session = %live.id, code = ?exit.code, signal = ?exit.signal, "assistant agent exited asleep");
            self.notifier.slept(live.id).await;
            return;
        }
        lock(&self.asleep).remove(&live.id);
        // Nothing is sent to an agent that is gone: what waits, waits for the user.
        let held = self.queues.hold(live.id, QueueHold::AgentGone);
        self.emit_queue(held);
        info!(session = %live.id, code = ?exit.code, signal = ?exit.signal, "chat agent exited");
        self.notifier.exited(live.id, exit).await;
    }

    async fn handle(&self, live: &Live, mut draft: AgentEventDraft) {
        // A retried turn: the agent's echo of the user message is in the journal already.
        if let AgentEventKind::MessageStart {
            role: hutan_core::agent::MessageRole::User,
            ..
        }
        | AgentEventKind::MessageEnd {
            role: hutan_core::agent::MessageRole::User,
            ..
        } = &draft.kind
        {
            let mut delivery = lock(&live.delivery);
            if delivery.retry {
                if matches!(draft.kind, AgentEventKind::MessageEnd { .. }) {
                    delivery.retry = false;
                }
                return;
            }
        }
        let ts_ms = registry::now_ms();
        let ends_turn = draft.kind.ends_turn();
        if ends_turn
            || matches!(
                draft.kind,
                AgentEventKind::TurnStarted
                    | AgentEventKind::PermissionRequested { .. }
                    | AgentEventKind::PermissionResolved { .. }
                    | AgentEventKind::QuestionRequested { .. }
                    | AgentEventKind::QuestionResolved { .. }
            )
        {
            live.touch();
        }
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
                lock(&live.usage.turn).turn_started();
                *lock(&live.open_turn) = event.turn_id;
                self.write_turn(&event, ChatTurnStatus::Running, None);
                turn_flag = Some(true);
            }
            AgentEventKind::Usage { total, last, .. } => {
                lock(&live.usage.turn).observe(*total, *last);
            }
            kind if kind.ends_turn() => {
                self.record_turn_usage(live, &event);
                *lock(&live.open_turn) = None;
                // Before the next turn may start: its start waits for this end.
                self.checkpoints.turn_ended(live.id, event.turn_id);
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
            // After the journal write is queued: a reducer built from the journal meanwhile
            // has this event, and one that is not yet in place buffers it (`InsightHub`).
            self.insight.observe(&event);
        }
        if ends_turn && let Some(waiter) = lock(&live.measuring).take() {
            let end = match &event.kind {
                AgentEventKind::TurnFailed { message, .. } => MeasureEnd::Failed(message.clone()),
                AgentEventKind::TurnCancelled => {
                    MeasureEnd::Failed("the turn was cancelled".to_owned())
                }
                _ => MeasureEnd::Completed,
            };
            let _ = waiter.send(end);
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

    /// A turn ended, however it ended: what it used is recorded (`docs/agents.md` § Usage).
    fn record_turn_usage(&self, live: &Live, event: &AgentEvent) {
        let (tokens, cost) = lock(&live.usage.turn).turn_ended();
        let attribution = lock(&live.usage.attribution).clone();
        if let Some(turn) = event.turn_id {
            self.usage.turn_ended(
                live.id,
                turn.to_string(),
                &attribution,
                tokens,
                cost,
                event.ts_ms,
            );
        }
        self.usage.harness_turn_ended(&attribution.harness);
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
