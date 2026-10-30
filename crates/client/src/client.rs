use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use hutan_core::{
    ApprovalId, Epoch, QuestionId, QuestionResponse, ReviewDecision, Seq, SessionId, Task, TaskId,
    TaskState, TerminalId, WorkspaceId,
};
use hutan_protocol::account::{
    AccountAdd, AccountDefaultSet, AccountInfo, AccountList, AccountLogin, AccountLogout,
    AccountRemove, AccountRemoved, AccountSession, ManagedAccount,
};
use hutan_protocol::assistant::{
    AssistantConfig, AssistantConfigure, AssistantEntry, AssistantFocus, AssistantList,
    AssistantListRequest, AssistantOpen, AssistantReset, AssistantScope, Attachment,
};
use hutan_protocol::blocks::{
    BlockPart, TerminalBlockText, TerminalBlockTextResult, TerminalBlocks, TerminalBlocksResult,
};
use hutan_protocol::chat::{
    ApprovalInfo, ApprovalList, ApprovalListRequest, ApprovalRespond, ChatAddDir, ChatCreate,
    ChatFork, ChatForked, ChatHistory, ChatHistoryRequest, ChatInterrupt, ChatModels,
    ChatModelsRequest, ChatQueue, ChatQueueAdd, ChatQueueEdit, ChatQueueItemRef, ChatQueued,
    ChatRemoveDir, ChatRetry, ChatSend, ChatSendNow, ChatSent, ChatSentNow, HISTORY_MAX_LIMIT,
    QuestionAnswer, QuestionInfo, QuestionList, QuestionListRequest, QueuedMessage,
};
use hutan_protocol::checkpoint::{
    CheckpointDiff, CheckpointDiffRequest, CheckpointList, CheckpointListRequest,
    CheckpointPreview, CheckpointPreviewRequest, CheckpointRemove, CheckpointRemoved,
    CheckpointRestore, CheckpointRestored,
};
use hutan_protocol::daemon_settings::{DaemonSettingSet, DaemonSettings};
use hutan_protocol::git::{
    GitBlame, GitBlameResult, GitFile, GitFileResult, GitLog, GitLogResult, GitShow, GitShowResult,
};
use hutan_protocol::grants::{ControlGrant, ControlGrantRevoke, ControlGrants, ControlGrantsList};
use hutan_protocol::hooks::{HooksList, HooksLog, HooksLogRequest, HooksTest, HooksTested};
use hutan_protocol::memory::{
    MemoryEntries, MemoryEntry, MemoryFilesStatus, MemoryFilesStatusRequest, MemoryHistory,
    MemoryHistoryRequest, MemoryImport, MemoryImported, MemoryList, MemoryProposal,
    MemoryProposalResolve, MemoryProposalResolved, MemoryProposals, MemoryProposalsList,
    MemoryPropose, MemoryProposed, MemoryReceipt, MemoryTransition, MemoryUpdate, MemoryWrite,
};
use hutan_protocol::messages::{
    AgentApplied, AgentReleaseRequest, AgentReportRequest, ClientInfo, Empty, Event, Hello,
    Request, ResumePoint, SessionInfo, SessionRef, SessionResult, TerminalHistory,
    TerminalHistoryRows, Welcome, WorkspaceCreate, WorkspaceInfo, decode_result,
};
use hutan_protocol::notify::{
    ClientFocus, NotificationIntent, NotifyAck, NotifyAct, NotifyActed, NotifyList, NotifyMute,
};
use hutan_protocol::project::{
    ProjectGitInit, ProjectGitInitialized, WorkspaceAssign, WorkspaceDelete,
};
use hutan_protocol::provider::{
    ChatSetModel, DiscoveredProvider, ProviderAdd, ProviderDiscovery, ProviderInfo, ProviderList,
    ProviderModels, ProviderRef, ProviderResult, ProviderSetCredential, ProviderUpdate,
};
use hutan_protocol::recovery::{RecoveryAck, RecoveryAcked};
use hutan_protocol::render::TerminalRender;
use hutan_protocol::resources::{
    AgentInterrupt, AgentInterrupted, ProcessSignal, ProcessSignaled, ResourceSample,
    ResourcesListRequest, ResourcesSubscribe, ResourcesSubscribed, SignalName,
};
use hutan_protocol::task::{
    TaskCreate, TaskDelete, TaskDeleted, TaskList, TaskListRequest, TaskRef, TaskResult,
    TaskReview, TaskReviewed, TaskStarted, TaskUpdate,
};
use hutan_protocol::terminal_search::{TerminalSearch, TerminalSearchResult};
use hutan_protocol::tree::{AgentTree, AgentTreeRequest};
use hutan_protocol::ui::{
    SessionSendText, SessionTextSent, UiActionList, UiCommandResult, UiLayoutResult, UiPublish,
    UiRan, UiRun, UiSetSetting, UiTarget,
};
use hutan_protocol::usage::{UsageSummary, UsageSummaryRequest};
use hutan_protocol::worktree::{
    WorktreeCleanupList, WorktreeCleanupRequest, WorktreeCommit, WorktreeCommitted, WorktreeCreate,
    WorktreeCreated, WorktreeDiff, WorktreeDiffRequest, WorktreeDiscard, WorktreeDiscarded,
    WorktreeFileDiff, WorktreeFileDiffRequest, WorktreeFinish, WorktreeFinished, WorktreeHunks,
    WorktreeHunksRequest, WorktreeIndexChange, WorktreeIndexed, WorktreeInfo, WorktreeList,
    WorktreeListRequest, WorktreeMerge, WorktreeMerged, WorktreePrune, WorktreePruned,
    WorktreeRemove, WorktreeRemoved, WorktreeSetupRequest, WorktreeSetupResult, WorktreeSetupTrust,
};
use hutan_protocol::{
    ControlMessage, ErrorBody, Frame, FrameCodec, FrameKind, MAX_PAYLOAD, PROTOCOL_VERSION,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;
use tokio::process::Child;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{sleep, timeout};
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{debug, warn};

use crate::endpoint::Endpoint;
use crate::error::ClientError;
use crate::ssh::SSH_HANDSHAKE_TIMEOUT;
use crate::start_failure::{DaemonFiles, StartFailure, StartOutcome};

/// Capability for `terminal.set_colors`; the client sends colors only to daemons advertising it.
pub(crate) const COLORS_CAPABILITY: &str = "terminal.colors";
/// Capability for `terminal.blocks` and `terminal.block_text` (protocol 0.5).
pub(crate) const BLOCKS_CAPABILITY: &str = "terminal.blocks";
/// Capability for the `history.*` requests (protocol 0.8).
pub(crate) const HISTORY_CAPABILITY: &str = "history.search";
/// Capability for the `chat.*` and `approval.*` requests (protocol 0.9).
pub(crate) const CHAT_CAPABILITY: &str = "chat.codex";
/// Capability for `agent.tree` and `agent.tree_updated` (protocol 0.10).
pub(crate) const AGENT_TREE_CAPABILITY: &str = "agent.tree";
/// Capability for `provider.*` and direct chats (`chat.create` with agent `direct`,
/// `chat.set_model`; protocol 0.11).
pub(crate) const DIRECT_CAPABILITY: &str = "chat.direct";
/// Capability for `assistant.*` and `chat.send` attachments (protocol 0.12).
pub(crate) const ASSISTANT_CAPABILITY: &str = "assistant";
/// Capability for `assistant.configure` and harness-backed assistants (protocol 0.20).
pub(crate) const ASSISTANT_BACKEND_CAPABILITY: &str = "assistant.backend";
/// Capability for `notify.*`, `client.focus` and the `notify.intent` / `ui.open` events
/// (protocol 0.13).
pub(crate) const NOTIFY_CAPABILITY: &str = "notify.intents";
/// Capability for `worktree.*`, the `worktree.changed` event and `SessionInfo.placement`
/// (protocol 0.14).
pub(crate) const WORKTREE_CAPABILITY: &str = "worktree";
/// Capability for `task.*` and the `task.updated` / `task.removed` events (protocol 0.16).
pub(crate) const TASK_CAPABILITY: &str = "task";
/// Capability for `chat.create` with the ACP harnesses `omp` and `claude`, and `chat.set_model`
/// on them (protocol 0.17).
pub(crate) const HARNESS_CAPABILITY: &str = "harness.acp";
/// Capability for `account.*` and the `account.changed` event (protocol 0.17).
pub(crate) const ACCOUNTS_CAPABILITY: &str = "accounts";
/// Capability for `account.add|remove|default`, accounts in `account.list`, `chat.create`'s
/// `account` and `ChatInfo.account` (protocol 0.50).
pub(crate) const ACCOUNTS_MULTI_CAPABILITY: &str = "accounts.multi";
/// Capability for `resources.*`, `session.kill`, `process.signal`, `agent.interrupt` and the
/// `resources.updated` event (protocol 0.18).
pub(crate) const RESOURCES_CAPABILITY: &str = "resources";
/// Capability for automatic projects, `workspace.assign` / `workspace.delete`,
/// `WorkspaceInfo.parent_id` / `project` and `SessionInfo.folder` (protocol 0.19).
pub(crate) const PROJECTS_CAPABILITY: &str = "projects.auto";
/// Capability for `ui.*`, `session.send_text`, the `ui.command` event and `ClientInfo.origin`
/// (protocol 0.21).
pub(crate) const CONTROL_CAPABILITY: &str = "control";
/// Capability for `worktree.diff`, `worktree.file_diff` and `worktree.merge` (protocol 0.22).
pub(crate) const WORKTREE_REVIEW_CAPABILITY: &str = "worktree.review";
/// Capability for `image` and `pasted_text` attachments, `file` attachments in every chat and
/// `ChatInfo.images` (protocol 0.23).
pub(crate) const RICH_INPUT_CAPABILITY: &str = "chat.rich_input";
/// Capability for `question.*`, the `question.*` events and `ChatInfo.pending_questions`
/// (protocol 0.24).
pub(crate) const QUESTIONS_CAPABILITY: &str = "chat.questions";
/// Capability for `task.review`, `worktree.commit`, the delivery facet of tasks and the scope
/// of `worktree.diff` (protocol 0.25).
pub(crate) const TASK_REVIEW_CAPABILITY: &str = "task.review";
/// Capability for `SessionInfo.recovery` and `session.recovery_ack` (protocol 0.26).
pub(crate) const RECOVERY_CAPABILITY: &str = "session.recovery";
/// Capability for `TaskRun::Chat.model` (protocol 0.27).
pub(crate) const TASK_CHAT_MODEL_CAPABILITY: &str = "task.chat_model";
/// Capability for `daemon.settings.get` / `.set` and `assistant.reset` (protocol 0.28).
pub(crate) const DAEMON_SETTINGS_CAPABILITY: &str = "daemon.settings";
/// Capability for `history.sessions`, the session catalog (protocol 0.29).
pub(crate) const HISTORY_SESSIONS_CAPABILITY: &str = "history.sessions";
/// Capability for `session.close` / `session.reopen`, the `session.closed` event and
/// `HistorySessionRow.closed` (protocol 0.30).
pub(crate) const SESSION_CLOSE_CAPABILITY: &str = "session.close";
/// Capability for `control.grants.list` / `.revoke` (protocol 0.31).
pub(crate) const CONTROL_GRANTS_CAPABILITY: &str = "control.grants";
/// Capability for `memory.*` and the `memory.changed` event (protocol 0.32).
pub(crate) const MEMORY_CAPABILITY: &str = "memory";
/// Capability for `memory.import` and `memory.files.status` (protocol 0.46).
pub(crate) const MEMORY_FILES_CAPABILITY: &str = "memory.files";
/// Capability for the `fs.*` requests and the `fs.search.progress` event (protocol 0.33).
pub(crate) const FS_CAPABILITY: &str = "fs";
/// Capability for `fs.watch` / `fs.unwatch` and the `fs.changed` and `fs.generation` events
/// (protocol 0.33).
pub(crate) const FS_WATCH_CAPABILITY: &str = "fs.watch";
/// Capability for `notify.mute`, `SessionInfo.muted`, intents withheld for a focused session and
/// withdrawn when one is focused (protocol 0.34).
pub(crate) const NOTIFY_MUTE_CAPABILITY: &str = "notify.mute";
/// Capability for `chat.retry` (protocol 0.34).
pub(crate) const CHAT_RETRY_CAPABILITY: &str = "chat.retry";
/// Capability for `DiffFile.staged` / `.unstaged` of `worktree.diff` (protocol 0.34).
pub(crate) const WORKTREE_INDEX_CAPABILITY: &str = "worktree.index";
/// Capability for `ChatInfo.mode`: the session mode a harness reports (protocol 0.34).
pub(crate) const CHAT_MODE_CAPABILITY: &str = "chat.mode";
/// Capability for `worktree.finish` and `PaneItem.path` (protocol 0.35).
pub(crate) const FINISH_CAPABILITY: &str = "finish";
/// Capability for the chat queue: `chat.queue.*`, `chat.send_now`, the `chat.queue` event and
/// `chat.interrupt`'s `keep_queue` (protocol 0.36).
pub(crate) const CHAT_QUEUE_CAPABILITY: &str = "chat.queue";
/// Capability for `checkpoint.list|diff|remove`, the `checkpoint.*` events and
/// `ChatInfo.checkpoints` (protocol 0.37).
pub(crate) const CHECKPOINT_CAPABILITY: &str = "checkpoint";
/// Capability for `checkpoint.preview|restore` and `fs.changed` origin `restore` (protocol
/// 0.38).
pub(crate) const CHECKPOINT_RESTORE_CAPABILITY: &str = "checkpoint.restore";
/// Capability for `doc.open|change|save|close` (protocol 0.39).
pub(crate) const DOC_SYNC_CAPABILITY: &str = "doc.sync";
/// Capability for `lsp.*`, the `lsp.state` and `lsp.diagnostics` events and
/// `ResourceSample.services` (protocol 0.39).
pub(crate) const LSP_CAPABILITY: &str = "lsp";
/// Capability for `Welcome.daemon_build` (protocol 0.42).
pub(crate) const IDENTITY_CAPABILITY: &str = "daemon.identity";
/// Capability for the `daemon.stopping` event and `daemon.shutdown`'s `restart` (protocol 0.42).
pub(crate) const STOPPING_CAPABILITY: &str = "daemon.stopping";
/// Capability for `terminal.search` (protocol 0.43).
pub(crate) const TERMINAL_SEARCH_CAPABILITY: &str = "terminal.search";
/// Capability for `project.git_init` (protocol 0.44).
pub(crate) const PROJECT_GIT_INIT_CAPABILITY: &str = "project.git_init";
/// Capability for `usage.summary` and the `usage.changed` event (protocol 0.45).
pub(crate) const USAGE_CAPABILITY: &str = "usage";
/// Capability for `git.log|blame|show|file` (protocol 0.47).
pub(crate) const GIT_HISTORY_CAPABILITY: &str = "git.history";
/// Capability for `hooks.list|log|test` (protocol 0.48).
pub(crate) const HOOKS_CAPABILITY: &str = "hooks";
/// Capability for worktree setup, teardown and retention (protocol 0.49).
pub(crate) const WORKTREE_SETUP_CAPABILITY: &str = "worktree.setup";
/// Capability for `chat.create`'s `safety`, `ChatInfo.safety` and `TaskRun::Chat.safety`
/// (protocol 0.51).
pub(crate) const CHAT_SAFETY_CAPABILITY: &str = "chat.safety";
/// Capability for Contain: `contain` on terminal specs, chats and task runs, and
/// `SessionInfo.containment` (protocol 0.52).
pub(crate) const SANDBOX_CAPABILITY: &str = "sandbox";
/// Capability for `worktree.hunks|stage|unstage|discard` and `staged_only` (protocol 0.53).
pub(crate) const WORKTREE_STAGE_CAPABILITY: &str = "worktree.stage";
/// Capability for `fs.create|rename|move|copy|trash|restore` (protocol 0.54).
pub(crate) const FS_OPS_CAPABILITY: &str = "fs.ops";
/// Capability for `chat.fork` (protocol 0.56).
pub(crate) const CHAT_FORK_CAPABILITY: &str = "chat.fork";
/// Capability for `chat.fork`'s `where: new_worktree`, `restore_files` and `archive_parent`
/// (protocol 0.58).
pub(crate) const CHAT_FORK_WORKTREE_CAPABILITY: &str = "chat.fork.worktree";
/// Capability for session folders: `extra_dirs`, `chat.add_dir|remove_dir`, `ChatInfo.dirs`
/// (protocol 0.57).
pub(crate) const SESSION_DIRS_CAPABILITY: &str = "session.dirs";
/// Capability for `agent.insight`, `agent.measure_context` and the `agent.insight_updated`
/// event (protocol 0.59).
pub(crate) const AGENT_INSIGHT_CAPABILITY: &str = "agent.insight";
/// Capability for `update.status|check|install` and the `update.available` and `update.install`
/// events (protocol 0.60).
pub(crate) const UPDATE_CAPABILITY: &str = "update";
/// Capability for `assistant.focus` and `ChatInfo.focus` (protocol 0.61).
pub(crate) const ASSISTANT_FOCUS_CAPABILITY: &str = "assistant.focus";
/// Capability for `agents.board` (protocol 0.63).
pub(crate) const AGENTS_BOARD_CAPABILITY: &str = "agents.board";
/// Capability for `agents.activity` (protocol 0.65).
pub(crate) const AGENTS_ACTIVITY_CAPABILITY: &str = "agents.activity";
/// Capability for `agents.stats` (protocol 0.66).
pub(crate) const AGENTS_STATS_CAPABILITY: &str = "agents.stats";
/// Capability for `agents.review` and the `turn_review` attachment (protocol 0.67).
pub(crate) const AGENTS_REVIEW_CAPABILITY: &str = "agents.review";
/// Capability for `ClientFocus.window_active`, `IntentKind::Digest` and `ui.open`'s `view`
/// (protocol 0.68).
pub(crate) const NOTIFY_DIGEST_CAPABILITY: &str = "notify.digest";
/// Capabilities this client understands, offered in `hello`.
const CAPABILITIES: &[&str] = &[
    "terminal.render.v1",
    COLORS_CAPABILITY,
    "session.restore",
    "agent.report",
    BLOCKS_CAPABILITY,
    HISTORY_CAPABILITY,
    CHAT_CAPABILITY,
    AGENT_TREE_CAPABILITY,
    DIRECT_CAPABILITY,
    ASSISTANT_CAPABILITY,
    NOTIFY_CAPABILITY,
    WORKTREE_CAPABILITY,
    TASK_CAPABILITY,
    HARNESS_CAPABILITY,
    ACCOUNTS_CAPABILITY,
    ACCOUNTS_MULTI_CAPABILITY,
    CHAT_SAFETY_CAPABILITY,
    SANDBOX_CAPABILITY,
    RESOURCES_CAPABILITY,
    PROJECTS_CAPABILITY,
    ASSISTANT_BACKEND_CAPABILITY,
    CONTROL_CAPABILITY,
    WORKTREE_REVIEW_CAPABILITY,
    RICH_INPUT_CAPABILITY,
    QUESTIONS_CAPABILITY,
    TASK_REVIEW_CAPABILITY,
    RECOVERY_CAPABILITY,
    TASK_CHAT_MODEL_CAPABILITY,
    DAEMON_SETTINGS_CAPABILITY,
    HISTORY_SESSIONS_CAPABILITY,
    SESSION_CLOSE_CAPABILITY,
    CONTROL_GRANTS_CAPABILITY,
    MEMORY_CAPABILITY,
    FS_CAPABILITY,
    FS_WATCH_CAPABILITY,
    NOTIFY_MUTE_CAPABILITY,
    CHAT_RETRY_CAPABILITY,
    WORKTREE_INDEX_CAPABILITY,
    CHAT_MODE_CAPABILITY,
    FINISH_CAPABILITY,
    CHAT_QUEUE_CAPABILITY,
    CHECKPOINT_CAPABILITY,
    CHECKPOINT_RESTORE_CAPABILITY,
    DOC_SYNC_CAPABILITY,
    LSP_CAPABILITY,
    IDENTITY_CAPABILITY,
    STOPPING_CAPABILITY,
    TERMINAL_SEARCH_CAPABILITY,
    PROJECT_GIT_INIT_CAPABILITY,
    USAGE_CAPABILITY,
    MEMORY_FILES_CAPABILITY,
    GIT_HISTORY_CAPABILITY,
    HOOKS_CAPABILITY,
    WORKTREE_SETUP_CAPABILITY,
    WORKTREE_STAGE_CAPABILITY,
    FS_OPS_CAPABILITY,
    CHAT_FORK_CAPABILITY,
    SESSION_DIRS_CAPABILITY,
    AGENT_INSIGHT_CAPABILITY,
    CHAT_FORK_WORKTREE_CAPABILITY,
    UPDATE_CAPABILITY,
    ASSISTANT_FOCUS_CAPABILITY,
    AGENTS_BOARD_CAPABILITY,
    AGENTS_ACTIVITY_CAPABILITY,
    AGENTS_STATS_CAPABILITY,
    AGENTS_REVIEW_CAPABILITY,
    NOTIFY_DIGEST_CAPABILITY,
];
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Generous on purpose: the first launch of a freshly built or updated `hutand` can take
/// several seconds on macOS while the system scans the new binary (observed 3–11 s), and a
/// timeout here leaves a daemon that comes up anyway behind an error. A warm start takes
/// well under a second, so the wait only costs time when the daemon really is slow or broken.
const AUTOSTART_WAIT: Duration = Duration::from_secs(15);
const AUTOSTART_POLL: Duration = Duration::from_millis(25);
/// The first wait after a failed autostart; it doubles with each failure in a row.
const AUTOSTART_BACKOFF_START: Duration = Duration::from_secs(1);
/// The longest wait between two autostarts that keep failing.
const AUTOSTART_BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Autostarts that failed the same way in a row before a reconnecting connection stops starting
/// the daemon by itself ([`StoreState::autostart_halted`](crate::StoreState::autostart_halted)).
pub const AUTOSTART_GIVE_UP: u32 = 3;
/// Events buffered per [`Client::events`] receiver before it starts lagging.
const EVENT_BUFFER: usize = 1024;
/// Terminal render messages buffered per [`Client::terminal_frames`] receiver.
const TERMINAL_BUFFER: usize = 64;
/// Outgoing frames queued before `call`/`send_input` wait for the socket.
const OUTBOX: usize = 256;
/// Terminal input is split so every frame stays under the protocol's payload limit.
const MAX_INPUT_CHUNK: usize = MAX_PAYLOAD - 8;

#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// Daemon to reach; default the local daemon at its default socket (`HUTAN_SOCKET` /
    /// `HUTAN_HOME` / the standard location).
    pub endpoint: Option<Endpoint>,
    /// `hutand` binary to spawn (detached) when nothing accepts connections on the local
    /// socket. Used only for [`Endpoint::Local`].
    pub autostart: Option<PathBuf>,
    pub client: ClientInfo,
}

/// Result of [`Client::connect_resuming`]. `events` was subscribed before the connection started
/// reading, so it also yields the events the daemon replays right after the welcome.
#[derive(Debug)]
pub struct Resumed {
    pub client: Client,
    pub events: broadcast::Receiver<(Seq, Event)>,
}

/// Handle to one daemon connection. Clones share it; the connection closes when the last
/// clone is dropped or the daemon goes away.
#[derive(Debug, Clone)]
pub struct Client {
    shared: Arc<Shared>,
    outbox: mpsc::Sender<Frame>,
    next_id: Arc<AtomicU64>,
    welcome: Arc<Welcome>,
}

#[derive(Debug)]
struct Shared {
    pending: Mutex<Pending>,
    /// `None` once the connection is gone, so receivers observe `Closed`.
    events: Mutex<Option<broadcast::Sender<(Seq, Event)>>>,
    terminals: Mutex<HashMap<TerminalId, mpsc::Sender<TerminalRender>>>,
    last_seq: AtomicU64,
}

type CallOutcome = Result<Value, ErrorBody>;

#[derive(Debug, Default)]
struct Pending {
    closed: bool,
    calls: HashMap<u64, oneshot::Sender<CallOutcome>>,
}

impl Client {
    /// Connects and handshakes. If the endpoint is the local socket and it is absent (or stale)
    /// and `opts.autostart` is set, spawns `hutand` and waits up to 15 s for it; a daemon that
    /// does not come up fails with [`ClientError::DaemonStart`].
    pub async fn connect(opts: ConnectOptions) -> Result<Self, ClientError> {
        Ok(Self::connect_resuming(opts, None).await?.client)
    }

    /// Like [`connect`](Self::connect), asking the daemon to replay events after `resume`.
    /// Check [`welcome`](Self::welcome)`.resume`: on `Replayed` the connection is already
    /// subscribed; on `SnapshotRequired` fetch `session.snapshot` and call `events.subscribe`.
    pub async fn connect_resuming(
        opts: ConnectOptions,
        resume: Option<ResumePoint>,
    ) -> Result<Resumed, ClientError> {
        Self::connect_gated(opts, resume, || true).await
    }

    /// [`connect_resuming`](Self::connect_resuming) where `permit` decides whether a missing
    /// socket may trigger an autostart (consulted only when `opts.autostart` is set).
    pub(crate) async fn connect_gated(
        opts: ConnectOptions,
        resume: Option<ResumePoint>,
        permit: impl FnOnce() -> bool,
    ) -> Result<Resumed, ClientError> {
        let endpoint = opts
            .endpoint
            .clone()
            .or_else(Endpoint::default_local)
            .ok_or(ClientError::NoSocketPath)?;
        match &endpoint {
            Endpoint::Local(socket) => {
                let stream = dial(
                    socket,
                    opts.autostart
                        .as_deref()
                        .filter(|_| endpoint.may_autostart()),
                    opts.endpoint.is_some(),
                    permit,
                )
                .await?;
                Self::establish(stream.into_split(), opts.client, resume, HANDSHAKE_TIMEOUT).await
            }
            Endpoint::Ssh(host) => {
                // Nothing here starts a process on this machine but `ssh`; the proxy on the
                // host starts that host's daemon when it is not running.
                let (stream, diagnostics) = host.open()?;
                let established = Self::establish(
                    tokio::io::split(stream),
                    opts.client,
                    resume,
                    SSH_HANDSHAKE_TIMEOUT,
                )
                .await;
                match established {
                    Ok(resumed) => Ok(resumed),
                    Err(error) => Err(diagnostics.explain(host, error).await),
                }
            }
        }
    }

    /// Handshakes over a byte stream the caller opened, and starts the reader and writer
    /// tasks. The stream is anything that reads and writes bytes in order: an in-memory
    /// duplex, a socket pair, the stdio of a supervised `ssh` (slice R-2). The protocol on it
    /// is exactly the local one: the first frame sent is `hello`, no peer is assumed trusted
    /// by this call; authenticating the other side is the stream opener's job. No autostart
    /// and no reconnect: both belong to the endpoint that produced the stream.
    pub async fn connect_stream<S>(
        stream: S,
        client: ClientInfo,
        resume: Option<ResumePoint>,
    ) -> Result<Resumed, ClientError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self::establish(tokio::io::split(stream), client, resume, HANDSHAKE_TIMEOUT).await
    }

    /// The handshake and tasks over the two halves of one stream. A Unix socket's own
    /// `into_split` halves are lock-free, so the local path passes those.
    async fn establish<R, W>(
        (read_half, write_half): (R, W),
        client: ClientInfo,
        resume: Option<ResumePoint>,
        handshake_timeout: Duration,
    ) -> Result<Resumed, ClientError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let mut reader = FramedRead::new(read_half, FrameCodec);
        let mut writer = FramedWrite::new(write_half, FrameCodec);
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client,
            capabilities: CAPABILITIES.iter().map(|c| (*c).to_owned()).collect(),
            resume,
        };
        let welcome = timeout(
            handshake_timeout,
            handshake(&mut reader, &mut writer, hello),
        )
        .await
        .map_err(|_| ClientError::Handshake("timed out waiting for welcome".into()))??;

        let replayed = resume
            .filter(|r| r.epoch == welcome.epoch)
            .filter(|_| welcome.resume == hutan_protocol::messages::ResumeOutcome::Replayed);
        let (events_tx, events_rx) = broadcast::channel(EVENT_BUFFER);
        let shared = Arc::new(Shared {
            pending: Mutex::new(Pending::default()),
            events: Mutex::new(Some(events_tx)),
            terminals: Mutex::new(HashMap::new()),
            last_seq: AtomicU64::new(replayed.map_or(0, |r| r.seq.0)),
        });
        let (outbox, outbox_rx) = mpsc::channel(OUTBOX);
        tokio::spawn(read_loop(reader, shared.clone()));
        tokio::spawn(write_loop(writer, outbox_rx));

        let client = Client {
            shared,
            outbox,
            // Id 0 was the hello.
            next_id: Arc::new(AtomicU64::new(1)),
            welcome: Arc::new(welcome),
        };
        Ok(Resumed {
            client,
            events: events_rx,
        })
    }

    /// Sends `req` and waits for its response. Error responses become
    /// [`ClientError::Remote`]; a dropped connection fails the call with
    /// [`ClientError::Disconnected`].
    pub async fn call<T: DeserializeOwned>(&self, req: Request) -> Result<T, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = ControlMessage::request(id, &req)?.to_frame()?;
        let (reply, response) = oneshot::channel();
        {
            let mut pending = self.shared.pending();
            if pending.closed {
                return Err(ClientError::Disconnected);
            }
            pending.calls.insert(id, reply);
        }
        // Removes the entry if this future is dropped before the response arrives.
        let _entry = PendingEntry {
            shared: &self.shared,
            id,
        };
        self.outbox
            .send(frame)
            .await
            .map_err(|_| ClientError::Disconnected)?;
        let outcome = response.await.map_err(|_| ClientError::Disconnected)?;
        match outcome {
            Ok(value) => Ok(decode_result(value)?),
            Err(error) => Err(ClientError::Remote(error)),
        }
    }

    /// True when the daemon advertised capability `terminal.blocks` (`terminal.blocks`,
    /// `terminal.block_text` and `terminal.block` events; protocol 0.5).
    pub fn supports_blocks(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == BLOCKS_CAPABILITY)
    }

    /// True when the daemon advertised capability `terminal.search` (`terminal.search`; protocol
    /// 0.43).
    pub fn supports_terminal_search(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == TERMINAL_SEARCH_CAPABILITY)
    }

    /// True when the daemon advertised capability `history.search` (the `history.*` requests,
    /// protocol 0.8).
    pub fn supports_history(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == HISTORY_CAPABILITY)
    }

    /// True when the daemon advertised capability `chat.codex` (agent chat sessions and
    /// approvals; protocol 0.9).
    pub fn supports_chat(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == CHAT_CAPABILITY)
    }

    /// True when the daemon advertised capability `chat.questions` (questions of agents;
    /// protocol 0.24).
    pub fn supports_questions(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == QUESTIONS_CAPABILITY)
    }

    /// True when the daemon advertised capability `harness.acp` (chats through the `omp` and
    /// `claude` harnesses; protocol 0.17).
    pub fn supports_harness_chat(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == HARNESS_CAPABILITY)
    }

    /// True when the daemon advertised capability `accounts` (protocol 0.17).
    pub fn supports_accounts(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == ACCOUNTS_CAPABILITY)
    }

    /// True when the daemon advertised capability `usage` (protocol 0.45).
    pub fn supports_usage(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == USAGE_CAPABILITY)
    }

    /// The limit windows the harnesses report and what the turns Hutan ran used
    /// (`usage.summary`); `refresh` asks the harnesses first, which takes seconds.
    pub async fn usage_summary(&self, refresh: bool) -> Result<UsageSummary, ClientError> {
        self.call(Request::UsageSummary(UsageSummaryRequest { refresh }))
            .await
    }

    /// True when the daemon advertised capability `agents.board` (protocol 0.63).
    pub fn supports_agents_board(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == AGENTS_BOARD_CAPABILITY)
    }

    /// True when the daemon advertised capability `agents.activity` (protocol 0.65).
    pub fn supports_agents_activity(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == AGENTS_ACTIVITY_CAPABILITY)
    }

    /// True when the daemon advertised capability `agents.stats` (protocol 0.66).
    pub fn supports_agents_stats(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == AGENTS_STATS_CAPABILITY)
    }

    /// `agents.stats`: where the time goes across agents, what they wait on and what fails
    /// again, with the coverage of every number.
    pub async fn agents_stats(
        &self,
        request: hutan_protocol::stats::AgentsStatsRequest,
    ) -> Result<hutan_protocol::stats::AgentsStats, ClientError> {
        self.call(Request::AgentsStats(request)).await
    }

    /// `agents.activity`: what happened across agents in a window, bounded and read-only.
    pub async fn agents_activity(
        &self,
        request: hutan_protocol::activity::AgentsActivityRequest,
    ) -> Result<hutan_protocol::activity::AgentsActivity, ClientError> {
        self.call(Request::AgentsActivity(request)).await
    }

    /// `agents.board`: every agent across workspaces (or of one), bounded and read-only.
    pub async fn agents_board(
        &self,
        request: hutan_protocol::board::AgentsBoardRequest,
    ) -> Result<hutan_protocol::board::AgentsBoard, ClientError> {
        self.call(Request::AgentsBoard(request)).await
    }

    /// True when the daemon advertised capability `agents.review` (protocol 0.67).
    pub fn supports_agents_review(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == AGENTS_REVIEW_CAPABILITY)
    }

    /// `agents.review`: what an agent did in one turn (the last finished one by default).
    pub async fn agents_review(
        &self,
        request: hutan_protocol::review::AgentsReviewRequest,
    ) -> Result<hutan_protocol::review::TurnReview, ClientError> {
        self.call(Request::AgentsReview(request)).await
    }

    /// True when the daemon advertised capability `agent.insight` (protocol 0.59).
    pub fn supports_agent_insight(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == AGENT_INSIGHT_CAPABILITY)
    }

    /// `agent.insight`: what a chat's context holds, what each turn used, its tools, files and
    /// time. `node_id` must be the session's main agent when given.
    pub async fn agent_insight(
        &self,
        session_id: SessionId,
        node_id: Option<String>,
    ) -> Result<hutan_protocol::insight::AgentInsightResult, ClientError> {
        self.call(Request::AgentInsight(
            hutan_protocol::insight::AgentInsightRequest {
                session_id,
                node_id,
            },
        ))
        .await
    }

    /// `agent.measure_context`: has the harness print its context by category (`/context`),
    /// reads the answer, and returns the insight after it. A turn of the conversation that
    /// costs no tokens; only call it because the user asked.
    pub async fn agent_measure_context(
        &self,
        session_id: SessionId,
    ) -> Result<hutan_protocol::insight::AgentContextMeasured, ClientError> {
        self.call(Request::AgentMeasureContext(
            hutan_protocol::insight::AgentMeasureContext { session_id },
        ))
        .await
    }

    /// True when the daemon advertised capability `chat.safety` (protocol 0.51).
    pub fn supports_chat_safety(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == CHAT_SAFETY_CAPABILITY)
    }

    /// True when the daemon advertised capability `sandbox` (protocol 0.52): it can run a
    /// session inside Hutan's Seatbelt boundary.
    pub fn supports_sandbox(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == SANDBOX_CAPABILITY)
    }

    /// True when the daemon advertised capability `session.dirs` (protocol 0.57): a session can
    /// hold extra folders (`chat.add_dir`, `chat.remove_dir`, `extra_dirs`).
    pub fn supports_session_dirs(&self) -> bool {
        self.has_capability(SESSION_DIRS_CAPABILITY)
    }

    /// Adds a folder to a chat or terminal session (`chat.add_dir`, user-only); answers with the
    /// session as it is now (`ChatInfo.dirs`, or `SessionInfo.extra_dirs` for a terminal).
    pub async fn chat_add_dir(
        &self,
        session_id: SessionId,
        path: impl Into<PathBuf>,
    ) -> Result<SessionInfo, ClientError> {
        let result: SessionResult = self
            .call(Request::ChatAddDir(ChatAddDir {
                session_id,
                path: path.into(),
            }))
            .await?;
        Ok(result.session)
    }

    /// Takes an extra folder out of a session (`chat.remove_dir`, user-only).
    pub async fn chat_remove_dir(
        &self,
        session_id: SessionId,
        path: impl Into<PathBuf>,
    ) -> Result<SessionInfo, ClientError> {
        let result: SessionResult = self
            .call(Request::ChatRemoveDir(ChatRemoveDir {
                session_id,
                path: path.into(),
            }))
            .await?;
        Ok(result.session)
    }

    /// The logins of the harnesses (`account.list`): names and plan only.
    pub async fn account_list(&self) -> Result<Vec<AccountInfo>, ClientError> {
        let list: AccountList = self.call(Request::AccountList(Empty {})).await?;
        Ok(list.accounts)
    }

    /// True when the daemon advertised capability `accounts.multi` (protocol 0.50).
    pub fn supports_accounts_multi(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == ACCOUNTS_MULTI_CAPABILITY)
    }

    /// Everything `account.list` says: the harnesses' own logins, the managed accounts with
    /// what each reports, and the defaults (protocol 0.50; an older daemon leaves the last
    /// two empty).
    pub async fn account_overview(&self) -> Result<AccountList, ClientError> {
        self.call(Request::AccountList(Empty {})).await
    }

    /// Starts the harness's own login in a terminal session (`account.login`); the user
    /// completes it there, and `account.changed` follows when the session ends.
    pub async fn account_login(
        &self,
        harness: impl Into<String>,
        provider: Option<String>,
    ) -> Result<AccountSession, ClientError> {
        self.account_login_as(harness, provider, None).await
    }

    /// [`account_login`](Self::account_login) for a managed account (`None`: the harness's
    /// own login).
    pub async fn account_login_as(
        &self,
        harness: impl Into<String>,
        provider: Option<String>,
        account: Option<String>,
    ) -> Result<AccountSession, ClientError> {
        self.call(Request::AccountLogin(AccountLogin {
            harness: harness.into(),
            provider,
            account,
        }))
        .await
    }

    /// Starts the harness's own logout in a terminal session (`account.logout`).
    pub async fn account_logout(
        &self,
        harness: impl Into<String>,
        provider: Option<String>,
    ) -> Result<AccountSession, ClientError> {
        self.account_logout_as(harness, provider, None).await
    }

    /// [`account_logout`](Self::account_logout) for a managed account.
    pub async fn account_logout_as(
        &self,
        harness: impl Into<String>,
        provider: Option<String>,
        account: Option<String>,
    ) -> Result<AccountSession, ClientError> {
        self.call(Request::AccountLogout(AccountLogout {
            harness: harness.into(),
            provider,
            account,
        }))
        .await
    }

    /// Creates a managed account (`account.add`): an empty home, not yet logged in.
    pub async fn account_add(
        &self,
        harness: impl Into<String>,
        name: impl Into<String>,
        label: Option<String>,
    ) -> Result<ManagedAccount, ClientError> {
        self.call(Request::AccountAdd(AccountAdd {
            harness: harness.into(),
            name: name.into(),
            label,
        }))
        .await
    }

    /// Logs the account out, deletes its home and forgets it (`account.remove`).
    pub async fn account_remove(
        &self,
        harness: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<AccountRemoved, ClientError> {
        self.call(Request::AccountRemove(AccountRemove {
            harness: harness.into(),
            name: name.into(),
        }))
        .await
    }

    /// Sets (`Some`) or clears (`None`) the default account of a harness, or of one project
    /// (`account.default`).
    pub async fn account_default(
        &self,
        harness: impl Into<String>,
        account: Option<String>,
        project: Option<String>,
    ) -> Result<(), ClientError> {
        let _: Empty = self
            .call(Request::AccountDefault(AccountDefaultSet {
                harness: harness.into(),
                account,
                project,
            }))
            .await?;
        Ok(())
    }

    /// True when the daemon advertised capability `chat.direct` (model providers and direct
    /// chats; protocol 0.11).
    pub fn supports_direct_chat(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == DIRECT_CAPABILITY)
    }

    /// True when the daemon advertised capability `agent.tree` (protocol 0.10).
    pub fn supports_agent_tree(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == AGENT_TREE_CAPABILITY)
    }

    /// The observation trees (main agent and subagents) of one session, or of all that have one.
    pub async fn agent_tree(
        &self,
        session_id: Option<SessionId>,
    ) -> Result<Vec<hutan_core::AgentObservation>, ClientError> {
        let tree: AgentTree = self
            .call(Request::AgentTree(AgentTreeRequest { session_id }))
            .await?;
        Ok(tree.observations)
    }

    /// `agent.report`: what an agent of a session is doing. `true` when the daemon applied it,
    /// `false` when it ignored it (`seq` not newer, session ended).
    pub async fn agent_report(&self, req: AgentReportRequest) -> Result<bool, ClientError> {
        let applied: AgentApplied = self.call(Request::AgentReport(req)).await?;
        Ok(applied.applied)
    }

    /// `agent.release`: the agent behind a report source is gone (or, with a node, that one
    /// subagent ended). `true` when the daemon applied it.
    pub async fn agent_release(&self, req: AgentReleaseRequest) -> Result<bool, ClientError> {
        let applied: AgentApplied = self.call(Request::AgentRelease(req)).await?;
        Ok(applied.applied)
    }

    /// Opens an agent conversation as a new session; resolves once the agent is ready.
    pub async fn chat_create(&self, req: ChatCreate) -> Result<SessionInfo, ClientError> {
        let result: SessionResult = self.call(Request::ChatCreate(req)).await?;
        Ok(result.session)
    }

    /// Sends the user's message; the turn's events follow as `chat.event` ([`events`](Self::events)).
    pub async fn chat_send(
        &self,
        session_id: SessionId,
        text: impl Into<String>,
    ) -> Result<ChatSent, ClientError> {
        self.chat_send_with(session_id, text, Vec::new()).await
    }

    /// [`chat_send`](Self::chat_send) with read-only context attached (protocol 0.12,
    /// capability `assistant`): the daemon resolves, redacts, bounds and journals it with the
    /// message. `forbidden` when the daemon's `[assistant.context]` does not allow one of them.
    pub async fn chat_send_with(
        &self,
        session_id: SessionId,
        text: impl Into<String>,
        attachments: Vec<Attachment>,
    ) -> Result<ChatSent, ClientError> {
        self.call(Request::ChatSend(ChatSend {
            session_id,
            text: text.into(),
            attachments,
        }))
        .await
    }

    /// True when the daemon advertised capability `chat.retry` (protocol 0.34).
    pub fn supports_chat_retry(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == CHAT_RETRY_CAPABILITY)
    }

    /// True when the daemon advertised capability `chat.queue` (protocol 0.36).
    pub fn supports_chat_queue(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == CHAT_QUEUE_CAPABILITY)
    }

    /// True when the daemon advertised capability `checkpoint` (protocol 0.37): checkpoints of
    /// agent turns can be listed, diffed and removed, and `ChatInfo.checkpoints` says whether a
    /// chat gets them.
    pub fn supports_checkpoints(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == CHECKPOINT_CAPABILITY)
    }

    /// True when the daemon advertised capability `checkpoint.restore` (protocol 0.38): files
    /// can be restored to before a checkpoint's turn (`checkpoint.preview|restore`), and a
    /// restore's writes show in `fs.changed` with origin `restore`.
    pub fn supports_checkpoint_restore(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == CHECKPOINT_RESTORE_CAPABILITY)
    }

    /// True when the daemon advertised capability `project.git_init` (protocol 0.44): a
    /// folder that is not a repository can be put under version control
    /// ([`Client::project_git_init`]).
    pub fn supports_project_git_init(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == PROJECT_GIT_INIT_CAPABILITY)
    }

    /// True when the daemon advertised capability `hooks` (protocol 0.48): `hooks.list`,
    /// `hooks.log` and `hooks.test`.
    pub fn supports_hooks(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == HOOKS_CAPABILITY)
    }

    /// True when the daemon advertised capability `update` (protocol 0.60): `update.status`,
    /// `update.check` and `update.install`.
    pub fn supports_update(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == UPDATE_CAPABILITY)
    }

    /// `update.status`: the running version, how Hutan was installed and the newest release
    /// above it that the daemon knows of. Contacts nothing.
    pub async fn update_status(&self) -> Result<hutan_protocol::update::UpdateStatus, ClientError> {
        self.call(Request::UpdateStatus(Empty {})).await
    }

    /// `update.check`: asks GitHub's releases now, whatever `updates.check` says; a check that
    /// could not finish is in `last_check` of the answer.
    pub async fn update_check(&self) -> Result<hutan_protocol::update::UpdateStatus, ClientError> {
        self.call(Request::UpdateCheck(Empty {})).await
    }

    /// `update.install`: has the daemon update Hutan the way it was installed. A Homebrew
    /// install runs in a visible terminal session; a bundle copied by hand is downloaded,
    /// verified and unpacked in the background (watch `update.install` events for its stage)
    /// and waits for [`Client::update_install_apply`]. Refused with `forbidden` for a client
    /// that runs inside an agent's session.
    pub async fn update_install(
        &self,
        method: hutan_protocol::update::InstallMethod,
    ) -> Result<hutan_protocol::update::UpdateInstalling, ClientError> {
        self.call(Request::UpdateInstall(
            hutan_protocol::update::UpdateInstallRequest {
                method,
                apply: false,
                after_pid: None,
            },
        ))
        .await
    }

    /// `update.install` with `apply` for a bundle whose update is `ready`: the daemon waits
    /// until the process `after_pid` (the app that is about to quit) has exited, moves the old
    /// bundle to the Trash, puts the new one in place and opens it.
    pub async fn update_install_apply(
        &self,
        after_pid: Option<u32>,
    ) -> Result<hutan_protocol::update::UpdateInstalling, ClientError> {
        self.call(Request::UpdateInstall(
            hutan_protocol::update::UpdateInstallRequest {
                method: hutan_protocol::update::InstallMethod::Bundle,
                apply: true,
                after_pid,
            },
        ))
        .await
    }

    /// `hooks.list`: the user's `[[hooks]]` with their counters and last runs.
    pub async fn hooks_list(&self) -> Result<HooksList, ClientError> {
        self.call(Request::HooksList(Empty {})).await
    }

    /// `hooks.log`: the finished runs, newest first.
    pub async fn hooks_log(&self, req: HooksLogRequest) -> Result<HooksLog, ClientError> {
        self.call(Request::HooksLog(req)).await
    }

    /// `hooks.test`: runs one hook with a sample payload and waits for it.
    pub async fn hooks_test(&self, hook: u32) -> Result<HooksTested, ClientError> {
        self.call(Request::HooksTest(HooksTest { hook })).await
    }

    /// True when the daemon advertised capability `git.history` (protocol 0.47): `git.log`,
    /// `git.blame`, `git.show` and `git.file`.
    pub fn supports_git_history(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == GIT_HISTORY_CAPABILITY)
    }

    /// True when the daemon advertised capability `worktree.setup` (protocol 0.49): worktree
    /// setup, teardown and retention.
    pub fn supports_worktree_setup(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == WORKTREE_SETUP_CAPABILITY)
    }

    /// True when the daemon advertised capability `worktree.index` (protocol 0.34): the files
    /// of `worktree.diff` say whether they are staged, unstaged or both.
    pub fn supports_worktree_index(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == WORKTREE_INDEX_CAPABILITY)
    }

    /// True when the daemon advertised capability `chat.mode` (protocol 0.34): a harness chat
    /// without `ChatInfo.mode` reports no mode (rather than an older daemon not saying).
    pub fn supports_chat_mode(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == CHAT_MODE_CAPABILITY)
    }

    /// True when the daemon advertised capability `finish` (protocol 0.35): `worktree.finish`.
    pub fn supports_finish(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == FINISH_CAPABILITY)
    }

    /// `chat.retry`: sends the chat's last user message again; the journal keeps it once.
    pub async fn chat_retry(&self, session_id: SessionId) -> Result<ChatSent, ClientError> {
        self.call(Request::ChatRetry(ChatRetry { session_id }))
            .await
    }

    /// True when the daemon advertised capability `chat.fork` (protocol 0.56).
    pub fn supports_chat_fork(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == CHAT_FORK_CAPABILITY)
    }

    /// True when the daemon advertised capability `chat.fork.worktree` (protocol 0.58): a fork
    /// can work in a new worktree with the files of the turn, restore the folder's files, and
    /// close the original.
    pub fn supports_chat_fork_worktree(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == CHAT_FORK_WORKTREE_CAPABILITY)
    }

    /// `chat.fork`: a new chat that starts with the chat's conversation up to a turn (ADR
    /// 0023). The parent is untouched; answered once the new chat's agent is up.
    pub async fn chat_fork(&self, fork: ChatFork) -> Result<ChatForked, ClientError> {
        self.call(Request::ChatFork(fork)).await
    }

    /// True when the daemon advertised capability `assistant` (protocol 0.12).
    pub fn supports_assistant(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == ASSISTANT_CAPABILITY)
    }

    /// The default assistant session of a scope (created on first use), or with `new` one more.
    pub async fn assistant_open(&self, req: AssistantOpen) -> Result<SessionInfo, ClientError> {
        let result: SessionResult = self.call(Request::AssistantOpen(req)).await?;
        Ok(result.session)
    }

    /// True when the daemon advertised capability `assistant.focus` (protocol 0.61): a Global
    /// Mawas chat can have a Focus.
    pub fn supports_assistant_focus(&self) -> bool {
        self.has_capability(ASSISTANT_FOCUS_CAPABILITY)
    }

    /// `assistant.focus` (user-only): sets, or with `None` clears, the Focus of a Global Mawas
    /// chat; answers with the session as it is now (`ChatInfo.focus`).
    pub async fn assistant_focus(
        &self,
        session_id: SessionId,
        workspace_id: Option<WorkspaceId>,
    ) -> Result<SessionInfo, ClientError> {
        let result: SessionResult = self
            .call(Request::AssistantFocus(AssistantFocus {
                session_id,
                workspace_id,
            }))
            .await?;
        Ok(result.session)
    }

    /// The assistant sessions, oldest first.
    pub async fn assistant_list(
        &self,
        req: AssistantListRequest,
    ) -> Result<Vec<AssistantEntry>, ClientError> {
        let list: AssistantList = self.call(Request::AssistantList(req)).await?;
        Ok(list.assistants)
    }

    /// True when the daemon advertised capability `assistant.backend` (protocol 0.20): an
    /// assistant can run on a harness and `assistant.configure` chooses the backend.
    pub fn supports_assistant_backend(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == ASSISTANT_BACKEND_CAPABILITY)
    }

    /// Chooses the backend (and model) assistants of a scope are created with from now on.
    pub async fn assistant_configure(
        &self,
        req: AssistantConfigure,
    ) -> Result<AssistantConfig, ClientError> {
        self.call(Request::AssistantConfigure(req)).await
    }

    /// True when the daemon advertised capability `notify.intents` (protocol 0.13).
    pub fn supports_notify(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == NOTIFY_CAPABILITY)
    }

    /// The pending notification intents, oldest first.
    pub async fn notify_list(&self) -> Result<Vec<NotificationIntent>, ClientError> {
        let list: NotifyList = self.call(Request::NotifyList(Empty {})).await?;
        Ok(list.intents)
    }

    /// Marks an intent as seen; `not_found` when it is no longer pending.
    pub async fn notify_ack(&self, id: impl Into<String>) -> Result<(), ClientError> {
        self.call::<Empty>(Request::NotifyAck(NotifyAck { id: id.into() }))
            .await
            .map(|_| ())
    }

    /// Chooses an action of an intent: an approval's option answers the approval, `open` makes
    /// the daemon tell the desktops to show the session.
    pub async fn notify_act(
        &self,
        id: impl Into<String>,
        action_id: impl Into<String>,
    ) -> Result<NotifyActed, ClientError> {
        self.call(Request::NotifyAct(NotifyAct {
            id: id.into(),
            action_id: action_id.into(),
        }))
        .await
    }

    /// Reports the session this connection's user is looking at (none: nothing).
    pub async fn client_focus(&self, session_id: Option<SessionId>) -> Result<(), ClientError> {
        self.call::<Empty>(Request::ClientFocus(ClientFocus {
            session_id,
            window_active: None,
        }))
        .await
        .map(|_| ())
    }

    /// [`client_focus`](Self::client_focus) that also says whether this window is the active one
    /// (protocol 0.68, capability `notify.digest`): what the daemon's digest counts as the user
    /// being here.
    pub async fn client_focus_in_window(
        &self,
        session_id: Option<SessionId>,
        window_active: bool,
    ) -> Result<(), ClientError> {
        self.call::<Empty>(Request::ClientFocus(ClientFocus {
            session_id,
            window_active: Some(window_active),
        }))
        .await
        .map(|_| ())
    }

    /// True when the daemon advertised capability `notify.digest` (protocol 0.68).
    pub fn supports_notify_digest(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == NOTIFY_DIGEST_CAPABILITY)
    }

    /// True when the daemon advertised capability `notify.mute` (protocol 0.34).
    pub fn supports_notify_mute(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == NOTIFY_MUTE_CAPABILITY)
    }

    /// `notify.mute`: silences the session's notifications (`muted`), or lets them through
    /// again; the session as it is now.
    pub async fn notify_mute(
        &self,
        session_id: SessionId,
        muted: bool,
    ) -> Result<SessionInfo, ClientError> {
        let result: SessionResult = self
            .call(Request::NotifyMute(NotifyMute { session_id, muted }))
            .await?;
        Ok(result.session)
    }

    /// True when the daemon advertised capability `control` (protocol 0.21).
    pub fn supports_control(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == CONTROL_CAPABILITY)
    }

    /// Publishes this window's action catalog, settings catalog and layout (`ui.publish`).
    pub async fn ui_publish(&self, publish: UiPublish) -> Result<(), ClientError> {
        self.call::<Empty>(Request::UiPublish(publish))
            .await
            .map(|_| ())
    }

    /// Answers a `ui.command` event (`ui.command_result`).
    pub async fn ui_command_result(&self, result: UiCommandResult) -> Result<(), ClientError> {
        self.call::<Empty>(Request::UiCommandResult(result))
            .await
            .map(|_| ())
    }

    /// The action and settings catalog of a window (`ui.actions`).
    pub async fn ui_actions(&self, window_id: Option<String>) -> Result<UiActionList, ClientError> {
        self.call(Request::UiActions(UiTarget { window_id })).await
    }

    /// The layout of a window (`ui.layout`).
    pub async fn ui_layout(
        &self,
        window_id: Option<String>,
    ) -> Result<UiLayoutResult, ClientError> {
        self.call(Request::UiLayout(UiTarget { window_id })).await
    }

    /// Runs an action of a window's catalog (`ui.run`).
    pub async fn ui_run(&self, req: UiRun) -> Result<UiRan, ClientError> {
        self.call(Request::UiRun(req)).await
    }

    /// Writes a setting layer through a window (`ui.set_setting`).
    pub async fn ui_set_setting(&self, req: UiSetSetting) -> Result<UiRan, ClientError> {
        self.call(Request::UiSetSetting(req)).await
    }

    /// Types text into a terminal session (`session.send_text`).
    pub async fn session_send_text(
        &self,
        req: SessionSendText,
    ) -> Result<SessionTextSent, ClientError> {
        self.call(Request::SessionSendText(req)).await
    }

    /// True when the daemon advertised capability `worktree` (protocol 0.14).
    pub fn supports_worktrees(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == WORKTREE_CAPABILITY)
    }

    /// Creates a worktree for a branch (`worktree.create`).
    pub async fn worktree_create(&self, req: WorktreeCreate) -> Result<WorktreeInfo, ClientError> {
        let created: WorktreeCreated = self.call(Request::WorktreeCreate(req)).await?;
        Ok(created.worktree)
    }

    /// The worktrees of one workspace's repositories, or of every workspace.
    pub async fn worktree_list(
        &self,
        workspace_id: Option<WorkspaceId>,
    ) -> Result<Vec<WorktreeInfo>, ClientError> {
        let list: WorktreeList = self
            .call(Request::WorktreeList(WorktreeListRequest { workspace_id }))
            .await?;
        Ok(list.worktrees)
    }

    /// Removes a worktree (`worktree.remove`); the answer says when `delete_branch` left the
    /// branch in place.
    pub async fn worktree_remove(
        &self,
        req: WorktreeRemove,
    ) -> Result<WorktreeRemoved, ClientError> {
        self.call(Request::WorktreeRemove(req)).await
    }

    /// True when the daemon advertised capability `projects.auto` (protocol 0.19).
    pub fn supports_projects(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == PROJECTS_CAPABILITY)
    }

    /// Creates a workspace with the projects of `req.paths` and `req.project_ids`
    /// (`workspace.create`).
    pub async fn workspace_create(
        &self,
        req: WorkspaceCreate,
    ) -> Result<WorkspaceInfo, ClientError> {
        self.call(Request::WorkspaceCreate(req)).await
    }

    /// Puts a project into a workspace, or out of its workspace (`workspace.assign`).
    pub async fn workspace_assign(
        &self,
        project_id: WorkspaceId,
        workspace_id: Option<WorkspaceId>,
    ) -> Result<WorkspaceInfo, ClientError> {
        self.call(Request::WorkspaceAssign(WorkspaceAssign {
            project_id,
            workspace_id,
        }))
        .await
    }

    /// `project.git_init` (protocol 0.44): `git init` and a first commit in a project's folder,
    /// or in the Home folder (the Home workspace's id). User-only; a refusal (the home
    /// directory, a repository already, a folder inside another repository, too many files)
    /// is `invalid_state` with the reason.
    pub async fn project_git_init(
        &self,
        project_id: WorkspaceId,
    ) -> Result<ProjectGitInitialized, ClientError> {
        self.call(Request::ProjectGitInit(ProjectGitInit { project_id }))
            .await
    }

    /// `git.log`: the commits of a checkout, or of a file or folder in it, newest first; page
    /// with the answer's `next_before`.
    pub async fn git_log(&self, req: GitLog) -> Result<GitLogResult, ClientError> {
        self.call(Request::GitLog(req)).await
    }

    /// `git.blame`: which commit last changed each line of a file.
    pub async fn git_blame(&self, req: GitBlame) -> Result<GitBlameResult, ClientError> {
        self.call(Request::GitBlame(req)).await
    }

    /// `git.show`: one commit, its message and its diff.
    pub async fn git_show(&self, req: GitShow) -> Result<GitShowResult, ClientError> {
        self.call(Request::GitShow(req)).await
    }

    /// `git.file`: a file as it was at a revision.
    pub async fn git_file(&self, req: GitFile) -> Result<GitFileResult, ClientError> {
        self.call(Request::GitFile(req)).await
    }

    /// Removes a workspace; its projects stay (`workspace.delete`).
    pub async fn workspace_delete(&self, workspace_id: WorkspaceId) -> Result<(), ClientError> {
        let _: Empty = self
            .call(Request::WorkspaceDelete(WorkspaceDelete { workspace_id }))
            .await?;
        Ok(())
    }

    /// True when the daemon advertised capability `worktree.review` (protocol 0.22).
    pub fn supports_worktree_review(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == WORKTREE_REVIEW_CAPABILITY)
    }

    /// True when the daemon advertised capability `chat.rich_input` (protocol 0.23): every chat
    /// takes attachments, including `image` and `pasted_text`.
    pub fn supports_rich_input(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == RICH_INPUT_CAPABILITY)
    }

    /// True when the daemon advertised capability `task.review` (protocol 0.25): tasks carry a
    /// delivery state, `task.review` and `worktree.commit` exist.
    pub fn supports_task_review(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == TASK_REVIEW_CAPABILITY)
    }

    /// True when the daemon advertised capability `session.recovery` (protocol 0.26): sessions
    /// say what a daemon restart did to them.
    pub fn supports_recovery(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == RECOVERY_CAPABILITY)
    }

    /// Dismisses what a restart did to one session, or to every session (`session.recovery_ack`).
    pub async fn session_recovery_ack(
        &self,
        session_id: Option<SessionId>,
    ) -> Result<RecoveryAcked, ClientError> {
        self.call(Request::SessionRecoveryAck(RecoveryAck { session_id }))
            .await
    }

    /// True when the daemon advertised capability `task.chat_model` (protocol 0.27): a chat task
    /// starts with the model it names.
    pub fn supports_task_chat_model(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == TASK_CHAT_MODEL_CAPABILITY)
    }

    /// True when the daemon advertised capability `daemon.settings` (protocol 0.28):
    /// `daemon.settings.get` / `.set` and `assistant.reset` exist.
    pub fn supports_daemon_settings(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == DAEMON_SETTINGS_CAPABILITY)
    }

    /// True when the daemon advertised capability `history.sessions` (protocol 0.29): the
    /// session catalog exists.
    pub fn supports_history_sessions(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == HISTORY_SESSIONS_CAPABILITY)
    }

    /// `history.sessions`: the session catalog, one row per agent session.
    pub async fn history_sessions(
        &self,
        request: hutan_protocol::history::HistorySessionsRequest,
    ) -> Result<hutan_protocol::history::HistorySessions, ClientError> {
        self.call(Request::HistorySessions(request)).await
    }

    /// `daemon.settings.get`: every option of `daemon.toml` with its value, default and whether
    /// the running daemon uses it.
    pub async fn daemon_settings(&self) -> Result<DaemonSettings, ClientError> {
        self.call(Request::DaemonSettingsGet(Empty {})).await
    }

    /// `daemon.settings.set`: changes one option of `daemon.toml`; `None` removes it (back to
    /// the default). Answers the settings as they are afterwards. Refused with `forbidden` for
    /// a client that runs inside an agent's session.
    pub async fn daemon_setting_set(
        &self,
        key: &str,
        value: Option<Value>,
    ) -> Result<DaemonSettings, ClientError> {
        self.call(Request::DaemonSettingsSet(DaemonSettingSet {
            key: key.to_owned(),
            value,
        }))
        .await
    }

    /// True when the daemon advertised capability `ask.suggest` (protocol 0.64).
    pub fn supports_ask_suggest(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == "ask.suggest")
    }

    /// `ask.suggest`: from a Mawas chat, attaches an advisory answer to a pending approval or
    /// question. `forbidden` for any other client. Answers nothing.
    pub async fn ask_suggest(
        &self,
        request: hutan_protocol::suggest::AskSuggest,
    ) -> Result<hutan_protocol::suggest::AskSuggested, ClientError> {
        self.call(Request::AskSuggest(request)).await
    }
    /// True when the daemon advertised capability `control.grants` (protocol 0.31): the grants
    /// Mawas's approvals store can be listed and revoked.
    pub fn supports_control_grants(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == CONTROL_GRANTS_CAPABILITY)
    }

    /// `control.grants.list`: the grants of the control gate, newest first; with `workspace_id`
    /// only those that apply to that workspace's chats. Refused with `forbidden` for a client
    /// that runs inside an agent's session.
    pub async fn control_grants(
        &self,
        workspace_id: Option<WorkspaceId>,
    ) -> Result<Vec<ControlGrant>, ClientError> {
        let list: ControlGrants = self
            .call(Request::ControlGrantsList(ControlGrantsList {
                workspace_id,
            }))
            .await?;
        Ok(list.grants)
    }

    /// `control.grants.revoke`: removes one grant (`not_found` when it is gone already).
    pub async fn control_grant_revoke(&self, id: u64) -> Result<(), ClientError> {
        self.call::<Empty>(Request::ControlGrantRevoke(ControlGrantRevoke { id }))
            .await
            .map(|_| ())
    }

    /// Whether the daemon advertised `capability` in its welcome.
    pub(crate) fn has_capability(&self, capability: &str) -> bool {
        self.welcome.capabilities.iter().any(|c| c == capability)
    }

    /// True when the daemon advertised capability `memory` (protocol 0.32): Mawas's curated
    /// memory, its proposals and the `memory.changed` event.
    pub fn supports_memory(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == MEMORY_CAPABILITY)
    }

    /// True when the daemon advertised capability `memory.files` (protocol 0.46): Mawas's
    /// memory as Markdown files, `memory.import` and `memory.files.status`.
    pub fn supports_memory_files(&self) -> bool {
        self.has_capability(MEMORY_FILES_CAPABILITY)
    }

    /// `memory.import`: entries of a file or text become pending proposals.
    pub async fn memory_import(&self, req: MemoryImport) -> Result<MemoryImported, ClientError> {
        self.call(Request::MemoryImport(req)).await
    }

    /// `memory.files.status`: where a scope's memory files are and what went wrong with them.
    pub async fn memory_files_status(
        &self,
        req: MemoryFilesStatusRequest,
    ) -> Result<MemoryFilesStatus, ClientError> {
        self.call(Request::MemoryFilesStatus(req)).await
    }

    /// `memory.list`: entries of Mawas's memory.
    pub async fn memory_list(&self, req: MemoryList) -> Result<Vec<MemoryEntry>, ClientError> {
        let entries: MemoryEntries = self.call(Request::MemoryList(req)).await?;
        Ok(entries.entries)
    }

    /// `memory.history`: an entry with every revision of it, newest first.
    pub async fn memory_history(&self, entry_id: &str) -> Result<MemoryHistory, ClientError> {
        self.call(Request::MemoryHistory(MemoryHistoryRequest {
            entry_id: entry_id.to_owned(),
        }))
        .await
    }

    /// `memory.write`: the user enters an entry (`forbidden` for a client inside an agent's
    /// session). The receipt says it was applied.
    pub async fn memory_write(&self, req: MemoryWrite) -> Result<MemoryReceipt, ClientError> {
        self.call(Request::MemoryWrite(req)).await
    }

    /// `memory.update`: replaces an entry's body as it was read at `expected_revision`
    /// (`conflict` when it moved on).
    pub async fn memory_update(&self, req: MemoryUpdate) -> Result<MemoryReceipt, ClientError> {
        self.call(Request::MemoryUpdate(req)).await
    }

    /// `memory.archive`.
    pub async fn memory_archive(
        &self,
        req: MemoryTransition,
    ) -> Result<MemoryReceipt, ClientError> {
        self.call(Request::MemoryArchive(req)).await
    }

    /// `memory.restore`.
    pub async fn memory_restore(
        &self,
        req: MemoryTransition,
    ) -> Result<MemoryReceipt, ClientError> {
        self.call(Request::MemoryRestore(req)).await
    }

    /// `memory.propose`: what an agent does. Answers `proposed`; nothing is applied until the
    /// user resolves it.
    pub async fn memory_propose(&self, req: MemoryPropose) -> Result<MemoryProposed, ClientError> {
        self.call(Request::MemoryPropose(req)).await
    }

    /// `memory.proposals.list`: the proposals waiting for the user, and with
    /// `include_resolved` the latest answered ones.
    pub async fn memory_proposals(
        &self,
        include_resolved: bool,
    ) -> Result<Vec<MemoryProposal>, ClientError> {
        let list: MemoryProposals = self
            .call(Request::MemoryProposalsList(MemoryProposalsList {
                include_resolved,
            }))
            .await?;
        Ok(list.proposals)
    }

    /// `memory.proposals.resolve`: the user approves, edits and approves, or rejects.
    pub async fn memory_resolve(
        &self,
        req: MemoryProposalResolve,
    ) -> Result<MemoryProposalResolved, ClientError> {
        self.call(Request::MemoryProposalsResolve(req)).await
    }

    /// `assistant.reset`: the scope inherits its backend again.
    pub async fn assistant_reset(
        &self,
        scope: AssistantScope,
        workspace_id: Option<WorkspaceId>,
    ) -> Result<(), ClientError> {
        self.call::<Empty>(Request::AssistantReset(AssistantReset {
            scope,
            workspace_id,
        }))
        .await
        .map(|_| ())
    }

    /// The files a checkout changed against its base (`worktree.diff`).
    pub async fn worktree_diff(
        &self,
        req: WorktreeDiffRequest,
    ) -> Result<WorktreeDiff, ClientError> {
        self.call(Request::WorktreeDiff(req)).await
    }

    /// The unified diff of one changed file (`worktree.file_diff`).
    pub async fn worktree_file_diff(
        &self,
        req: WorktreeFileDiffRequest,
    ) -> Result<WorktreeFileDiff, ClientError> {
        self.call(Request::WorktreeFileDiff(req)).await
    }

    /// Merges a worktree's branch into the main checkout's branch (`worktree.merge`).
    pub async fn worktree_merge(&self, req: WorktreeMerge) -> Result<WorktreeMerged, ClientError> {
        self.call(Request::WorktreeMerge(req)).await
    }

    /// Runs a worktree's setup again (`worktree.setup`).
    pub async fn worktree_setup(
        &self,
        req: WorktreeSetupRequest,
    ) -> Result<WorktreeSetupResult, ClientError> {
        self.call(Request::WorktreeSetup(req)).await
    }

    /// Confirms the repository's setup commands and runs the setup (`worktree.setup_trust`,
    /// user-only).
    pub async fn worktree_setup_trust(
        &self,
        req: WorktreeSetupTrust,
    ) -> Result<WorktreeSetupResult, ClientError> {
        self.call(Request::WorktreeSetupTrust(req)).await
    }

    /// The worktrees retention would remove now (`worktree.cleanup`).
    pub async fn worktree_cleanup(
        &self,
        req: WorktreeCleanupRequest,
    ) -> Result<WorktreeCleanupList, ClientError> {
        self.call(Request::WorktreeCleanup(req)).await
    }

    /// Removes the worktrees retention lists, teardown first (`worktree.prune`, user-only).
    pub async fn worktree_prune(&self, req: WorktreePrune) -> Result<WorktreePruned, ClientError> {
        self.call(Request::WorktreePrune(req)).await
    }

    /// Commits everything in a worktree (`worktree.commit`).
    pub async fn worktree_commit(
        &self,
        req: WorktreeCommit,
    ) -> Result<WorktreeCommitted, ClientError> {
        self.call(Request::WorktreeCommit(req)).await
    }

    /// The user's Finish of a checkout: commit, merge a worktree, push (`worktree.finish`).
    pub async fn worktree_finish(
        &self,
        req: WorktreeFinish,
    ) -> Result<WorktreeFinished, ClientError> {
        self.call(Request::WorktreeFinish(req)).await
    }

    /// The hunks of a checkout's files, with ids (`worktree.hunks`).
    pub async fn worktree_hunks(
        &self,
        req: WorktreeHunksRequest,
    ) -> Result<WorktreeHunks, ClientError> {
        self.call(Request::WorktreeHunks(req)).await
    }

    /// Copies files or hunks into git's index (`worktree.stage`).
    pub async fn worktree_stage(
        &self,
        req: WorktreeIndexChange,
    ) -> Result<WorktreeIndexed, ClientError> {
        self.call(Request::WorktreeStage(req)).await
    }

    /// Takes files or hunks out of git's index (`worktree.unstage`).
    pub async fn worktree_unstage(
        &self,
        req: WorktreeIndexChange,
    ) -> Result<WorktreeIndexed, ClientError> {
        self.call(Request::WorktreeUnstage(req)).await
    }

    /// Throws working-tree changes away after a checkpoint (`worktree.discard`).
    pub async fn worktree_discard(
        &self,
        req: WorktreeDiscard,
    ) -> Result<WorktreeDiscarded, ClientError> {
        self.call(Request::WorktreeDiscard(req)).await
    }

    /// True when the daemon advertised capability `worktree.stage` (protocol 0.53): staging,
    /// unstaging, discarding and committing only what is staged.
    pub fn supports_worktree_stage(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == WORKTREE_STAGE_CAPABILITY)
    }

    /// True when the daemon advertised capability `task` (protocol 0.16).
    pub fn supports_tasks(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == TASK_CAPABILITY)
    }

    /// Creates a task (`task.create`).
    pub async fn task_create(&self, req: TaskCreate) -> Result<Task, ClientError> {
        let created: TaskResult = self.call(Request::TaskCreate(req)).await?;
        Ok(created.task)
    }

    /// The tasks of one workspace, or of every workspace, in dependency order; `state` keeps
    /// only those shown in it.
    pub async fn task_list(
        &self,
        workspace_id: Option<WorkspaceId>,
        state: Option<TaskState>,
    ) -> Result<Vec<Task>, ClientError> {
        let list: TaskList = self
            .call(Request::TaskList(TaskListRequest {
                workspace_id,
                state,
            }))
            .await?;
        Ok(list.tasks)
    }

    /// Changes a task or its state (`task.update`).
    pub async fn task_update(&self, req: TaskUpdate) -> Result<Task, ClientError> {
        let updated: TaskResult = self.call(Request::TaskUpdate(req)).await?;
        Ok(updated.task)
    }

    /// Deletes a task, and with `cascade` the tasks that depend on it; the ids that went.
    pub async fn task_delete(&self, id: TaskId, cascade: bool) -> Result<Vec<TaskId>, ClientError> {
        let deleted: TaskDeleted = self
            .call(Request::TaskDelete(TaskDelete { id, cascade }))
            .await?;
        Ok(deleted.removed)
    }

    /// Runs a ready task now (`task.start`): the task, its session and any warnings.
    pub async fn task_start(&self, id: TaskId) -> Result<TaskStarted, ClientError> {
        self.call(Request::TaskStart(TaskRef { id })).await
    }

    /// A person's verdict on a done task (`task.review`): accept it, or ask for changes with a
    /// note (sent to the task's chat when it still lives).
    pub async fn task_review(
        &self,
        id: TaskId,
        decision: ReviewDecision,
        note: Option<String>,
    ) -> Result<TaskReviewed, ClientError> {
        self.call(Request::TaskReview(TaskReview { id, decision, note }))
            .await
    }

    /// True when the daemon advertised capability `resources` (protocol 0.18).
    pub fn supports_resources(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == RESOURCES_CAPABILITY)
    }

    /// One fresh sample of the processes of the live sessions (of one, or of all) and of
    /// `hutand` (`resources.list`).
    pub async fn resources_list(
        &self,
        session_id: Option<SessionId>,
    ) -> Result<ResourceSample, ClientError> {
        self.call(Request::ResourcesList(ResourcesListRequest { session_id }))
            .await
    }

    /// Starts `resources.updated` events for this connection (`resources.subscribe`); read them
    /// from [`Client::events`]. The answer carries the first sample.
    pub async fn resources_subscribe(
        &self,
        interval_ms: Option<u32>,
    ) -> Result<ResourcesSubscribed, ClientError> {
        self.call(Request::ResourcesSubscribe(ResourcesSubscribe {
            interval_ms,
        }))
        .await
    }

    /// Stops this connection's `resources.updated` events (`resources.unsubscribe`).
    pub async fn resources_unsubscribe(&self) -> Result<(), ClientError> {
        self.call::<Empty>(Request::ResourcesUnsubscribe(Empty {}))
            .await
            .map(|_| ())
    }

    /// SIGKILLs a session's whole process tree now (`session.kill`).
    pub async fn session_kill(&self, session_id: SessionId) -> Result<SessionInfo, ClientError> {
        let result: SessionResult = self
            .call(Request::SessionKill(SessionRef { session_id }))
            .await?;
        Ok(result.session)
    }

    /// True when the daemon advertised capability `session.close` (protocol 0.30): sessions can
    /// be closed and reopened.
    pub fn supports_session_close(&self) -> bool {
        self.welcome
            .capabilities
            .iter()
            .any(|capability| capability == SESSION_CLOSE_CAPABILITY)
    }

    /// `session.close`: removes the session from the lists. A session something runs in is
    /// refused with `invalid_state` (see [`ClientError::busy`]) unless `stop_running`.
    pub async fn session_close(
        &self,
        session_id: SessionId,
        stop_running: bool,
    ) -> Result<(), ClientError> {
        self.call::<Empty>(Request::SessionClose(
            hutan_protocol::messages::SessionClose {
                session_id,
                stop_running,
            },
        ))
        .await
        .map(|_| ())
    }

    /// `session.reopen`: brings a closed session back; answers with the session.
    pub async fn session_reopen(&self, session_id: SessionId) -> Result<SessionInfo, ClientError> {
        let result: SessionResult = self
            .call(Request::SessionReopen(SessionRef { session_id }))
            .await?;
        Ok(result.session)
    }

    /// Signals one process of a session (`process.signal`).
    pub async fn process_signal(
        &self,
        session_id: SessionId,
        pid: u32,
        signal: SignalName,
    ) -> Result<ProcessSignaled, ClientError> {
        self.call(Request::ProcessSignal(ProcessSignal {
            session_id,
            pid,
            signal,
        }))
        .await
    }

    /// Interrupts the main agent of a session, or one subagent (`agent.interrupt`).
    pub async fn agent_interrupt(
        &self,
        session_id: SessionId,
        node_id: Option<String>,
        force: bool,
    ) -> Result<AgentInterrupted, ClientError> {
        self.call(Request::AgentInterrupt(AgentInterrupt {
            session_id,
            node_id,
            force,
        }))
        .await
    }

    /// Stops the turn in progress (and, since protocol 0.36, clears the chat's queue).
    pub async fn chat_interrupt(&self, session_id: SessionId) -> Result<(), ClientError> {
        self.chat_stop(session_id, false).await
    }

    /// `chat.interrupt` with `keep_queue` (protocol 0.36): stops the turn; the queue is cleared,
    /// or kept and held.
    pub async fn chat_stop(
        &self,
        session_id: SessionId,
        keep_queue: bool,
    ) -> Result<(), ClientError> {
        self.call::<Empty>(Request::ChatInterrupt(ChatInterrupt {
            session_id,
            keep_queue,
        }))
        .await
        .map(|_| ())
    }

    /// `chat.queue.list`.
    pub async fn chat_queue(&self, session_id: SessionId) -> Result<ChatQueue, ClientError> {
        self.call(Request::ChatQueueList(SessionRef { session_id }))
            .await
    }

    /// `chat.queue.add`: queues `text` after the turn (sent at once when no turn runs).
    pub async fn chat_queue_add(
        &self,
        session_id: SessionId,
        text: impl Into<String>,
    ) -> Result<QueuedMessage, ClientError> {
        self.call::<ChatQueued>(Request::ChatQueueAdd(ChatQueueAdd {
            session_id,
            text: text.into(),
        }))
        .await
        .map(|queued| queued.item)
    }

    /// `chat.queue.edit`.
    pub async fn chat_queue_edit(
        &self,
        session_id: SessionId,
        item_id: u64,
        text: impl Into<String>,
    ) -> Result<QueuedMessage, ClientError> {
        self.call::<ChatQueued>(Request::ChatQueueEdit(ChatQueueEdit {
            session_id,
            item_id,
            text: text.into(),
        }))
        .await
        .map(|queued| queued.item)
    }

    /// `chat.queue.remove`.
    pub async fn chat_queue_remove(
        &self,
        session_id: SessionId,
        item_id: u64,
    ) -> Result<(), ClientError> {
        self.call::<Empty>(Request::ChatQueueRemove(ChatQueueItemRef {
            session_id,
            item_id,
        }))
        .await
        .map(|_| ())
    }

    /// `chat.queue.clear`.
    pub async fn chat_queue_clear(&self, session_id: SessionId) -> Result<(), ClientError> {
        self.call::<Empty>(Request::ChatQueueClear(SessionRef { session_id }))
            .await
            .map(|_| ())
    }

    /// `chat.send_now`: `text`, or the queued message `item_id`, without waiting for the turn.
    pub async fn chat_send_now(
        &self,
        session_id: SessionId,
        text: Option<String>,
        item_id: Option<u64>,
    ) -> Result<ChatSentNow, ClientError> {
        self.call(Request::ChatSendNow(ChatSendNow {
            session_id,
            text,
            item_id,
        }))
        .await
    }

    /// `checkpoint.list` (protocol 0.37): one page of checkpoints, newest first; `next` asks
    /// for the page after it.
    pub async fn checkpoint_list(
        &self,
        request: CheckpointListRequest,
    ) -> Result<CheckpointList, ClientError> {
        self.call(Request::CheckpointList(request)).await
    }

    /// `checkpoint.diff` (protocol 0.37): what changed from a checkpoint's start to its end, to
    /// the checkout now, or to another checkpoint's end; bounded like `worktree.diff`.
    pub async fn checkpoint_diff(
        &self,
        request: CheckpointDiffRequest,
    ) -> Result<CheckpointDiff, ClientError> {
        self.call(Request::CheckpointDiff(request)).await
    }

    /// `checkpoint.remove` (protocol 0.37): one checkpoint, or every checkpoint of a session;
    /// answers with the ones that went.
    pub async fn checkpoint_remove(
        &self,
        request: CheckpointRemove,
    ) -> Result<CheckpointRemoved, ClientError> {
        self.call(Request::CheckpointRemove(request)).await
    }

    /// `checkpoint.preview` (protocol 0.38): what restoring the checkout to before a
    /// checkpoint's turn would do, file by file, with the digest a restore confirms.
    pub async fn checkpoint_preview(
        &self,
        request: CheckpointPreviewRequest,
    ) -> Result<CheckpointPreview, ClientError> {
        self.call(Request::CheckpointPreview(request)).await
    }

    /// `checkpoint.restore` (protocol 0.38): restores what the preview of `plan_digest` listed
    /// (and the conflicts named); a changed checkout answers `conflict` with the new preview as
    /// the error's `data`. User-only.
    pub async fn checkpoint_restore(
        &self,
        request: CheckpointRestore,
    ) -> Result<CheckpointRestored, ClientError> {
        self.call(Request::CheckpointRestore(request)).await
    }

    /// One page of a chat's journal, oldest first (see [`ChatHistoryRequest`]).
    pub async fn chat_history(
        &self,
        session_id: SessionId,
        before_seq: Option<u64>,
        limit: u32,
    ) -> Result<ChatHistory, ClientError> {
        self.call(Request::ChatHistory(ChatHistoryRequest {
            session_id,
            before_seq,
            limit,
        }))
        .await
    }

    /// The whole journal of a chat, oldest first, fetched page by page.
    pub async fn chat_history_all(
        &self,
        session_id: SessionId,
    ) -> Result<ChatHistory, ClientError> {
        let mut all = self
            .chat_history(session_id, None, HISTORY_MAX_LIMIT)
            .await?;
        while all.has_more {
            let Some(first) = all.events.first().map(|e| e.seq) else {
                break;
            };
            let mut page = self
                .chat_history(session_id, Some(first), HISTORY_MAX_LIMIT)
                .await?;
            all.has_more = page.has_more;
            page.events.append(&mut all.events);
            all.events = page.events;
        }
        Ok(all)
    }

    /// The models an agent offers.
    pub async fn chat_models(&self, agent: &str) -> Result<ChatModels, ClientError> {
        self.call(Request::ChatModels(ChatModelsRequest {
            agent: agent.to_owned(),
        }))
        .await
    }

    /// Switches the model (and optionally the provider) of a running direct chat.
    pub async fn chat_set_model(
        &self,
        session_id: SessionId,
        provider_id: Option<String>,
        model: impl Into<String>,
    ) -> Result<SessionInfo, ClientError> {
        let result: SessionResult = self
            .call(Request::ChatSetModel(ChatSetModel {
                session_id,
                provider_id,
                model: model.into(),
            }))
            .await?;
        Ok(result.session)
    }

    /// The configured model providers, oldest first.
    pub async fn provider_list(&self) -> Result<Vec<ProviderInfo>, ClientError> {
        let list: ProviderList = self.call(Request::ProviderList(Empty {})).await?;
        Ok(list.providers)
    }

    pub async fn provider_add(&self, req: ProviderAdd) -> Result<ProviderInfo, ClientError> {
        let result: ProviderResult = self.call(Request::ProviderAdd(req)).await?;
        Ok(result.provider)
    }

    pub async fn provider_update(&self, req: ProviderUpdate) -> Result<ProviderInfo, ClientError> {
        let result: ProviderResult = self.call(Request::ProviderUpdate(req)).await?;
        Ok(result.provider)
    }

    /// Forgets a provider and its stored API key.
    pub async fn provider_remove(&self, provider_id: impl Into<String>) -> Result<(), ClientError> {
        self.call::<Empty>(Request::ProviderRemove(ProviderRef {
            provider_id: provider_id.into(),
        }))
        .await
        .map(|_| ())
    }

    /// Hands the daemon an API key to keep in the Keychain; it is never sent back.
    pub async fn provider_set_credential(
        &self,
        provider_id: impl Into<String>,
        secret: impl Into<String>,
    ) -> Result<ProviderInfo, ClientError> {
        let result: ProviderResult = self
            .call(Request::ProviderSetCredential(ProviderSetCredential {
                provider_id: provider_id.into(),
                secret: secret.into(),
            }))
            .await?;
        Ok(result.provider)
    }

    pub async fn provider_clear_credential(
        &self,
        provider_id: impl Into<String>,
    ) -> Result<ProviderInfo, ClientError> {
        let result: ProviderResult = self
            .call(Request::ProviderClearCredential(ProviderRef {
                provider_id: provider_id.into(),
            }))
            .await?;
        Ok(result.provider)
    }

    /// The models a provider offers right now.
    pub async fn provider_models(
        &self,
        provider_id: impl Into<String>,
    ) -> Result<ProviderModels, ClientError> {
        self.call(Request::ProviderModels(ProviderRef {
            provider_id: provider_id.into(),
        }))
        .await
    }

    /// Asks the daemon to look for Ollama and LM Studio on this machine.
    pub async fn provider_discover(&self) -> Result<Vec<DiscoveredProvider>, ClientError> {
        let found: ProviderDiscovery = self.call(Request::ProviderDiscover(Empty {})).await?;
        Ok(found.found)
    }

    /// Permission requests waiting for an answer (of one session, or of all).
    pub async fn approvals(
        &self,
        session_id: Option<SessionId>,
    ) -> Result<Vec<ApprovalInfo>, ClientError> {
        let list: ApprovalList = self
            .call(Request::ApprovalList(ApprovalListRequest { session_id }))
            .await?;
        Ok(list.approvals)
    }

    /// Answers a permission request with one of the options it offered.
    pub async fn approve(
        &self,
        approval_id: ApprovalId,
        option_id: impl Into<String>,
    ) -> Result<(), ClientError> {
        self.call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id,
            option_id: option_id.into(),
        }))
        .await
        .map(|_| ())
    }

    /// Questions of agents waiting for an answer (of one session, or of all).
    pub async fn questions(
        &self,
        session_id: Option<SessionId>,
    ) -> Result<Vec<QuestionInfo>, ClientError> {
        let list: QuestionList = self
            .call(Request::QuestionList(QuestionListRequest { session_id }))
            .await?;
        Ok(list.questions)
    }

    /// Answers a question: values by field id, a decline, or a cancel.
    pub async fn answer_question(
        &self,
        question_id: QuestionId,
        response: QuestionResponse,
    ) -> Result<(), ClientError> {
        self.call::<Empty>(Request::QuestionAnswer(QuestionAnswer {
            question_id,
            response,
        }))
        .await
        .map(|_| ())
    }

    /// One page of the terminal's blocks, newest first: at most `limit` (the daemon caps it),
    /// all with an id below `before_id` when given. Page by passing the last block's id.
    pub async fn terminal_blocks(
        &self,
        terminal_id: TerminalId,
        before_id: Option<u64>,
        limit: u32,
    ) -> Result<TerminalBlocksResult, ClientError> {
        self.call(Request::TerminalBlocks(TerminalBlocks {
            terminal_id,
            before_id,
            limit,
        }))
        .await
    }

    /// One page of `terminal.history` with the absolute line number of the viewport's first row
    /// it was counted from (FX-UI-B). `from_line` counts from the viewport as the daemon has it
    /// *when it answers*, and a program that keeps drawing (a coding agent's TUI) moves it while
    /// the request is on its way: a client that numbers the rows by the viewport it has itself
    /// applied puts them on the wrong lines. The page is bracketed by two `terminal.blocks`
    /// probes (`limit` 0, which answer `viewport_top` and nothing else); the page counts from
    /// that top only when both probes agree, so the viewport did not move while it was read, and
    /// is asked for again otherwise (a few times). `None` for the top: the daemon does not
    /// advertise `terminal.blocks`, or the viewport kept moving; the page is then what plain
    /// `terminal.history` gives and the caller numbers it by its own view of the viewport.
    pub async fn terminal_history_anchored(
        &self,
        terminal_id: TerminalId,
        from_line: u32,
        count: u32,
    ) -> Result<(Option<u64>, TerminalHistoryRows), ClientError> {
        const ATTEMPTS: usize = 4;
        let history = || {
            self.call::<TerminalHistoryRows>(Request::TerminalHistory(TerminalHistory {
                terminal_id,
                from_line,
                count,
            }))
        };
        if !self.supports_blocks() {
            return Ok((None, history().await?));
        }
        for _ in 0..ATTEMPTS {
            let before = self
                .terminal_blocks(terminal_id, None, 0)
                .await?
                .viewport_top;
            let page = history().await?;
            let after = self
                .terminal_blocks(terminal_id, None, 0)
                .await?
                .viewport_top;
            if before == after {
                return Ok((Some(before), page));
            }
        }
        Ok((None, history().await?))
    }

    /// Plain text of a block's command or output, at most `max_bytes` (the daemon caps it).
    pub async fn terminal_block_text(
        &self,
        terminal_id: TerminalId,
        block_id: u64,
        part: BlockPart,
        max_bytes: u32,
    ) -> Result<TerminalBlockTextResult, ClientError> {
        self.call(Request::TerminalBlockText(TerminalBlockText {
            terminal_id,
            block_id,
            part,
            max_bytes,
        }))
        .await
    }

    /// `terminal.search`: where `req.query` matches in the terminal's scrollback and screen, oldest
    /// first; page with the answer's `next_from`. A newer search for the same terminal on this
    /// connection cancels an unfinished one, which fails with a `conflict` remote error
    /// (`data.reason = "superseded"`).
    pub async fn terminal_search(
        &self,
        req: TerminalSearch,
    ) -> Result<TerminalSearchResult, ClientError> {
        self.call(Request::TerminalSearch(req)).await
    }

    /// Daemon events, each with its seq. A receiver sees events delivered after it was created
    /// and lags (`RecvError::Lagged`) if it falls more than 1024 events behind; on lag, resync
    /// through a snapshot. It yields `RecvError::Closed` once the connection is gone.
    pub fn events(&self) -> broadcast::Receiver<(Seq, Event)> {
        match &*self.shared.events() {
            Some(sender) => sender.subscribe(),
            // Already disconnected: hand out a receiver that is closed from the start.
            None => broadcast::channel(1).1,
        }
    }

    /// Terminal render messages (snapshots and frames) for `id`. Bounded; a consumer that does
    /// not keep up loses messages, which the render protocol's snapshot/ack flow recovers.
    /// Replaces any earlier receiver for the same terminal.
    pub fn terminal_frames(&self, id: TerminalId) -> mpsc::Receiver<TerminalRender> {
        let (tx, rx) = mpsc::channel(TERMINAL_BUFFER);
        // On a closed connection the sender is dropped at once: the receiver ends immediately.
        if !self.shared.pending().closed {
            self.shared.terminals().insert(id, tx);
        }
        rx
    }

    /// Sends raw input bytes to a terminal (binary frame, no response).
    pub async fn send_input(&self, id: TerminalId, bytes: &[u8]) -> Result<(), ClientError> {
        for chunk in bytes.chunks(MAX_INPUT_CHUNK) {
            self.outbox
                .send(Frame::terminal_input(id, chunk))
                .await
                .map_err(|_| ClientError::Disconnected)?;
        }
        Ok(())
    }

    /// Sends `req` without waiting for (or keeping) its response; the daemon's answer is
    /// discarded by the read loop. Never blocks: a full outbox drops the request.
    pub(crate) fn notify(&self, req: &Request) -> Result<(), ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = ControlMessage::request(id, req)?.to_frame()?;
        match self.outbox.try_send(frame) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!("outbox full, dropping a fire-and-forget request");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(ClientError::Disconnected),
        }
    }

    pub fn epoch(&self) -> Epoch {
        self.welcome.epoch
    }

    /// The daemon's `welcome`: capabilities and whether a resume was replayed.
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// Seq of the last event received on this connection (the resume point for a reconnect).
    pub fn last_seq(&self) -> Seq {
        Seq(self.shared.last_seq.load(Ordering::Acquire))
    }
}

impl Shared {
    fn pending(&self) -> MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn events(&self) -> MutexGuard<'_, Option<broadcast::Sender<(Seq, Event)>>> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn terminals(&self) -> MutexGuard<'_, HashMap<TerminalId, mpsc::Sender<TerminalRender>>> {
        self.terminals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn complete(&self, id: u64, outcome: CallOutcome) {
        if let Some(reply) = self.pending().calls.remove(&id) {
            let _ = reply.send(outcome);
        } else {
            debug!(id, "response for an unknown or abandoned call");
        }
    }

    fn on_event(&self, seq: Seq, event: &str, data: Value) {
        // Recorded first so a consumer woken by the broadcast sees `last_seq >= seq`.
        self.last_seq.fetch_max(seq.0, Ordering::AcqRel);
        match Event::decode(event, data) {
            Ok(Some(event)) => {
                // No receivers is fine.
                if let Some(sender) = &*self.events() {
                    let _ = sender.send((seq, event));
                }
            }
            Ok(None) => debug!(event, "ignoring unknown event"),
            Err(e) => warn!(event, error = %e, "undecodable event"),
        }
    }

    fn on_terminal(&self, id: TerminalId, render: TerminalRender) {
        let mut terminals = self.terminals();
        let Some(sink) = terminals.get(&id) else {
            return;
        };
        match sink.try_send(render) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                debug!(terminal = %id, "render queue full, dropping")
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                terminals.remove(&id);
            }
        }
    }

    /// Fails every pending and future call.
    fn close(&self) {
        let mut pending = self.pending();
        pending.closed = true;
        // Dropping the senders wakes the callers with `Disconnected`.
        pending.calls.clear();
        drop(pending);
        self.terminals().clear();
        // Dropping the sender closes every events receiver.
        self.events().take();
    }
}

struct PendingEntry<'a> {
    shared: &'a Shared,
    id: u64,
}

impl Drop for PendingEntry<'_> {
    fn drop(&mut self) {
        self.shared.pending().calls.remove(&self.id);
    }
}

async fn handshake<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut FramedRead<R, FrameCodec>,
    writer: &mut FramedWrite<W, FrameCodec>,
    hello: Hello,
) -> Result<Welcome, ClientError> {
    let frame = ControlMessage::request(0, &Request::Hello(hello))?.to_frame()?;
    writer.send(frame).await?;
    let frame = match reader.next().await {
        Some(frame) => frame?,
        None => return Err(ClientError::Disconnected),
    };
    if frame.kind != FrameKind::Control {
        return Err(ClientError::Handshake(
            "first frame was not a control frame".into(),
        ));
    }
    match ControlMessage::from_frame(&frame)? {
        ControlMessage::Response { id: 0, outcome } => match outcome {
            Ok(value) => Ok(decode_result(value)?),
            Err(error) => Err(ClientError::Remote(error)),
        },
        _ => Err(ClientError::Handshake(
            "first message was not the hello response".into(),
        )),
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: FramedRead<R, FrameCodec>,
    shared: Arc<Shared>,
) {
    while let Some(item) = reader.next().await {
        let frame = match item {
            Ok(frame) => frame,
            Err(e) => {
                debug!(error = %e, "connection read failed");
                break;
            }
        };
        match frame.kind {
            FrameKind::Control => match ControlMessage::from_frame(&frame) {
                Ok(ControlMessage::Response { id, outcome }) => shared.complete(id, outcome),
                Ok(ControlMessage::Event {
                    seq, event, data, ..
                }) => shared.on_event(seq, &event, data),
                Ok(ControlMessage::Request { method, .. }) => {
                    debug!(method, "ignoring request from daemon");
                }
                Err(e) => warn!(error = %e, "malformed control frame from daemon"),
            },
            FrameKind::TerminalRender => match frame.decode_terminal_render() {
                Ok((id, render)) => shared.on_terminal(id, render),
                Err(e) => warn!(error = %e, "undecodable terminal render frame"),
            },
            FrameKind::TerminalInput => debug!("ignoring terminal input frame from daemon"),
        }
    }
    shared.close();
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: FramedWrite<W, FrameCodec>,
    mut outbox: mpsc::Receiver<Frame>,
) {
    while let Some(frame) = outbox.recv().await {
        if let Err(e) = writer.send(frame).await {
            debug!(error = %e, "connection write failed");
            break;
        }
    }
    // Ends the daemon's reading side; see the same step in the daemon's connection writer.
    let _ = writer.close().await;
}

pub(crate) fn is_unreachable(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// Paces the autostarts of a connection that dials repeatedly: after a start that failed, the
/// next waits 1 s, doubling to 60 s while starts keep failing; after [`AUTOSTART_GIVE_UP`]
/// failures of the same kind in a row it allows none (halted) until [`reset`](Self::reset). A
/// start that brought a daemon up resets it too.
#[derive(Debug, Default)]
pub(crate) struct AutostartThrottle {
    /// No autostart before this.
    next: Option<Instant>,
    /// Failed autostarts in a row, whatever their kind.
    failures: u32,
    /// The kind of the last failure and how often in a row it happened.
    kind: Option<(String, u32)>,
    /// A permit was handed out for the dial in progress.
    pending: bool,
    halted: bool,
}

impl AutostartThrottle {
    /// `true` (and the dial in progress counts as an autostart) if one is allowed now. Two
    /// autostarts are at least 1 s apart even when the first one worked (a daemon that dies
    /// right after it answered is not started in a tight loop).
    pub(crate) fn permit(&mut self) -> bool {
        let now = Instant::now();
        if self.halted || self.next.is_some_and(|next| now < next) {
            return false;
        }
        self.pending = true;
        self.next = Some(now + AUTOSTART_BACKOFF_START);
        true
    }

    /// The dial ended with `error`; counts only when it started the daemon.
    pub(crate) fn failed(&mut self, error: &ClientError) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        self.failures = self.failures.saturating_add(1);
        let kind = failure_kind(error);
        let repeats = match &self.kind {
            Some((last, repeats)) if *last == kind => repeats.saturating_add(1),
            _ => 1,
        };
        self.halted = repeats >= AUTOSTART_GIVE_UP;
        self.kind = Some((kind, repeats));
        let delay = AUTOSTART_BACKOFF_START
            .saturating_mul(1 << self.failures.saturating_sub(1).min(16))
            .min(AUTOSTART_BACKOFF_MAX);
        self.next = Some(Instant::now() + delay);
    }

    /// Whether autostarts stopped after failing the same way [`AUTOSTART_GIVE_UP`] times.
    pub(crate) fn halted(&self) -> bool {
        self.halted
    }

    /// A daemon answered: failures so far are forgotten (the spacing of [`permit`] stays).
    ///
    /// [`permit`]: Self::permit
    pub(crate) fn succeeded(&mut self) {
        *self = Self {
            next: self.next,
            ..Self::default()
        };
    }

    /// Starts afresh, now: the user asked for a start.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

/// What kind of failure `error` is, for "failed the same way": the start-error record's kind,
/// else how the started process ended, else the error's kind.
fn failure_kind(error: &ClientError) -> String {
    use crate::start_failure::StartErrorKind;
    match error {
        ClientError::DaemonStart(failure) => match (failure.kind(), failure.outcome) {
            (Some(StartErrorKind::SchemaTooNew { .. }), _) => "schema_too_new".to_owned(),
            (Some(StartErrorKind::AlreadyRunning { .. }), _) => "already_running".to_owned(),
            (Some(StartErrorKind::Other), _) => "other".to_owned(),
            (None, StartOutcome::Exited { code }) => format!("exited {code:?}"),
            (None, outcome) => format!("{outcome:?}"),
        },
        ClientError::Io(error) => format!("io {:?}", error.kind()),
        other => format!("{:?}", std::mem::discriminant(other)),
    }
}

/// Dials `socket`. When it is unreachable and `autostart` is set, spawns `hutand` if `permit`
/// agrees and waits up to [`AUTOSTART_WAIT`] for it; otherwise returns the dial error. A
/// started daemon that exits first, or does not answer in time, fails with
/// [`ClientError::DaemonStart`]: its start-error record, else the end of its stderr.
async fn dial(
    socket: &Path,
    autostart: Option<&Path>,
    socket_explicit: bool,
    permit: impl FnOnce() -> bool,
) -> Result<UnixStream, ClientError> {
    let first = match UnixStream::connect(socket).await {
        Ok(stream) => return Ok(stream),
        Err(e) => e,
    };
    let Some(binary) = autostart.filter(|_| is_unreachable(&first) && permit()) else {
        return Err(first.into());
    };
    let files = DaemonFiles::for_socket(socket);
    let spawned_ms = crate::time::now_ms();
    let spawn = || spawn_daemon(binary, socket_explicit.then_some(socket), &files);
    let child = spawn()?;
    let outcome = match timeout(AUTOSTART_WAIT, wait_for_socket(socket, child, spawn)).await {
        Ok(Ok(stream)) => return Ok(stream),
        Ok(Err(Waited::Failed(e))) => return Err(e),
        Ok(Err(Waited::Exited(code))) => StartOutcome::Exited { code },
        Err(_) => StartOutcome::TimedOut,
    };
    let failure = StartFailure::collect(&files, outcome, spawned_ms);
    debug!(%failure, "autostarted hutand did not come up");
    Err(ClientError::DaemonStart(Box::new(failure)))
}

/// Opens the Unix socket of the local daemon as a raw stream, for a byte bridge (`hutan proxy`)
/// that must not interpret the protocol. Like [`Client::connect`] it starts `hutand`
/// (`autostart`) when nothing answers on `socket` and waits for it; `socket_explicit` says the
/// socket was named by the caller (the daemon is then told to listen there).
pub async fn dial_local(
    socket: &Path,
    autostart: Option<&Path>,
    socket_explicit: bool,
) -> Result<UnixStream, ClientError> {
    dial(socket, autostart, socket_explicit, || true).await
}

/// `hutand`'s exit code when another daemon holds the lock (`crates/daemon/src/main.rs`).
const EXIT_ALREADY_RUNNING: i32 = 2;
/// Starts within one autostart after the daemon that held the lock went away.
const AUTOSTART_RESPAWNS: u32 = 5;

enum Waited {
    Failed(ClientError),
    Exited(Option<i32>),
}

/// Polls `socket` until it answers. A `child` that fails ends the wait at once; one that exits
/// cleanly or because another daemon holds the lock does not, since that other daemon may
/// still bring the socket up. When that other daemon was on its way out instead (its socket
/// is gone, a stop that had not finished), `respawn` starts ours again (a few times at most).
async fn wait_for_socket(
    socket: &Path,
    mut child: Child,
    mut respawn: impl FnMut() -> Result<Child, ClientError>,
) -> Result<UnixStream, Waited> {
    let mut running = true;
    let mut respawns = 0;
    loop {
        tokio::select! {
            status = child.wait(), if running => {
                let status = status.map_err(|e| Waited::Failed(e.into()))?;
                match status.code() {
                    Some(0 | EXIT_ALREADY_RUNNING) => running = false,
                    code => return Err(Waited::Exited(code)),
                }
            }
            () = sleep(AUTOSTART_POLL) => {}
        }
        match UnixStream::connect(socket).await {
            Ok(stream) => return Ok(stream),
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    && !running
                    && respawns < AUTOSTART_RESPAWNS =>
            {
                debug!("the daemon that held the lock is gone; starting hutand again");
                respawns += 1;
                child = respawn().map_err(Waited::Failed)?;
                running = true;
            }
            Err(e) if is_unreachable(&e) => {}
            Err(e) => return Err(Waited::Failed(e.into())),
        }
    }
}

/// Starts `hutand` in its own process group, so it outlives this process and terminal
/// signals. Its stderr goes to `hutand-start.log` (truncated; the daemon writes there only
/// while it starts), so a start that fails can say why.
fn spawn_daemon(
    binary: &Path,
    socket: Option<&Path>,
    files: &DaemonFiles,
) -> Result<Child, ClientError> {
    let stderr = match files.open_start_log() {
        Ok(file) => Stdio::from(file),
        Err(error) => {
            warn!(
                path = %files.start_log().display(),
                %error,
                "cannot open the start log; hutand's stderr is discarded"
            );
            Stdio::null()
        }
    };
    let mut command = tokio::process::Command::new(binary);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .process_group(0)
        .kill_on_drop(false);
    if let Some(socket) = socket {
        command.env("HUTAN_SOCKET", socket);
    }
    // Dropping the child does not stop it; tokio reaps it in the background once it exits.
    Ok(command.spawn()?)
}
