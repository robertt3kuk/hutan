//! `SessionRegistry` actor: owns workspaces and terminal sessions and publishes their events.
//! Everything reaches it through one bounded mailbox; replies travel over `oneshot`s.
//!
//! The actor never waits on a child process. Stopping a session signals its terminal and parks
//! the reply until the terminal's watcher task reports the exit back through the same mailbox.
//!
//! Every change is written through to `hutan-storage` (fire-and-forget into the writer thread,
//! so request handling never waits for the disk), and [`spawn`] rebuilds the state from what was
//! stored (ADR 0009 step 1): live sessions come back as new terminals with the same ids.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::{Instant, sleep_until};

use hutan_agents::{CodexConfig, ExitInfo, SessionMode, Started};
use hutan_core::{
    AgentNode, NodeOutcome, NodeUpdate, ObservationTree, Patch, SessionEvent, SessionId,
    SessionKind, SessionState, TerminalId, WorkspaceId,
};
use hutan_protocol::chat::{ChatCreate, ChatInfo, ChatMode};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{
    AgentApplied, AgentReleaseRequest, AgentReportRequest, AgentState, CreatedBy, Event,
    RECENT_PROJECTS_DEFAULT_LIMIT, RECENT_PROJECTS_MAX_LIMIT, RecentProject, RecentProjects,
    SessionBusy, SessionClose, SessionCreate, SessionInfo, SessionList, SessionListFilter,
    SessionRename, SessionSnapshot, SessionSpec, TerminalSpec, WorkspaceCreate, WorkspaceInfo,
    WorkspaceKind, WorkspaceList, WorkspaceMove, WorkspaceOpen, WorkspaceOpened, WorkspacePin,
    WorkspaceRename,
};
use hutan_protocol::notify::NotifyMute;
use hutan_protocol::project::{ProjectInfo, WorkspaceAssign, WorkspaceDelete};
use hutan_protocol::provider::ChatSetModel;
use hutan_protocol::recovery::{Lost, Recovery, RecoveryAck, RecoveryAcked, RecoveryKind};
use hutan_protocol::tree::{AgentTree, NodeReport};
use hutan_protocol::worktree::Placement;
use hutan_storage::{
    self as stored_rows, ClosedSession, SessionRow, Storage, StorageError, WorkspaceRow, Write,
};
use hutan_terminal::{
    EnvInherit, ExitStatus, PromptMark, RenderTracker, Terminal, TerminalConfig, TerminalMeta,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::agent;
use crate::bus::{EventBus, PublishError};
use crate::chat::{
    AGENT_ASSISTANT, AGENT_CODEX, AGENT_DIRECT, AssistantPolicy, ChatManager, ChatSpec,
    is_gateway_agent,
};
use crate::config::RestoreMode;
use crate::direct::MAX_SYSTEM_PROMPT_BYTES;
use crate::harness::{Harnesses, is_harness_agent};
use crate::placement::{Occupant, PlacementCache, shared_with};
use crate::projects::{self, Assignment, Folder, HomeRules, Ident};
use crate::providers::ProviderService;
use crate::recovery;
use crate::restore::{self, Stored};
use crate::session_names::{self, Facts, Naming};
use crate::terminals::{TerminalEntry, TerminalHub};
use crate::tree::{self, TreeState};

/// Commands waiting in the mailbox before senders wait their turn.
const MAILBOX_CAPACITY: usize = 256;

/// `app_meta` key of the muted sessions (`notify.mute`, protocol 0.34): a JSON list of ids.
pub const MUTED_KEY: &str = "notify.muted";

/// Why a session that was alive is listed, not started, under `restore_sessions = "stopped"`.
const STOPPED_MODE_REASON: &str = "restore_sessions is \"stopped\"";

/// Time a terminal gets to react to SIGHUP before it is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(3);

/// Name of the Home workspace when the daemon creates it.
pub const HOME_WORKSPACE_NAME: &str = "Home";

/// A workspace's `last_active_ms` changes at most this often (session creation excepted).
pub const ACTIVITY_INTERVAL: Duration = Duration::from_secs(10);

/// Terminals tell the registry about input and output at most this often each; the registry
/// applies [`ACTIVITY_INTERVAL`] per workspace.
pub const ACTIVITY_NOTIFY_INTERVAL: Duration = Duration::from_secs(1);

/// Title and cwd changes are coalesced: a session is written at most this often because of them.
const DIRTY_WRITE_INTERVAL: Duration = Duration::from_secs(1);

/// Longest accepted `source`, `agent`, `agent_session_id`, node id, or parent id.
const MAX_AGENT_FIELD_BYTES: usize = 256;
/// Longer agent messages are cut to this many characters.
const MAX_AGENT_MESSAGE_CHARS: usize = 512;
/// Longer node names, types and models are cut to this many characters.
const MAX_NODE_TEXT_CHARS: usize = 128;
/// Longest accepted `usage.kind`.
const MAX_USAGE_KIND_BYTES: usize = 64;
/// A transcript path longer than this is not kept.
const MAX_TRANSCRIPT_PATH_BYTES: usize = 1024;

type Reply<T> = oneshot::Sender<Result<T, ErrorBody>>;

#[derive(Debug)]
enum Command {
    /// The folders of the request are already resolved: the projects they name.
    CreateWorkspace(WorkspaceCreate, Vec<Ident>, Reply<WorkspaceInfo>),
    ListWorkspaces(Reply<WorkspaceList>),
    RenameWorkspace(WorkspaceRename, Reply<WorkspaceInfo>),
    CloseWorkspace(WorkspaceId, Reply<()>),
    /// The folder is already resolved: this is where it goes.
    OpenWorkspace(Assignment, Option<CreatedBy>, Reply<WorkspaceOpened>),
    AddRoot(WorkspaceId, Assignment, Reply<WorkspaceInfo>),
    RemoveRoot(WorkspaceId, PathBuf, Reply<WorkspaceInfo>),
    RecentProjects(Option<u32>, Reply<RecentProjects>),
    MoveWorkspace(WorkspaceMove, Reply<WorkspaceList>),
    PinWorkspace(WorkspacePin, Reply<WorkspaceInfo>),
    AssignWorkspace(WorkspaceAssign, Reply<WorkspaceInfo>),
    DeleteWorkspace(WorkspaceDelete, Reply<()>),
    /// Input or output of a terminal; from its watcher task and the input path.
    Activity(TerminalId),
    /// Input reached a terminal; the flag says it was the user's, not an agent's.
    Input(TerminalId, bool),
    /// The user acted in the session (`chat.send`): what a restart did to it is old news.
    UserAction(SessionId),
    /// `session.recovery_ack`.
    RecoveryAck(RecoveryAck, Reply<RecoveryAcked>),
    /// Something a restart cost the session, found after the registry restored it (a task
    /// that failed with it).
    RecoveryLost(SessionId, Lost),
    Snapshot(Reply<SessionSnapshot>),
    SessionCounts(Reply<SessionCounts>),
    /// The assignment is present when the request names no workspace.
    CreateSession(SessionCreate, Option<Assignment>, Reply<SessionInfo>),
    ListSessions(SessionListFilter, Reply<SessionList>),
    /// `session.close`: answered once the session is closed (after it stopped, when it ran).
    CloseSession(SessionClose, Reply<()>),
    /// `session.reopen`.
    ReopenSession(SessionId, Reply<SessionInfo>),
    /// The closed sessions, most recently closed first.
    ClosedSessions(Reply<Vec<ClosedInfo>>),
    /// Answered once the session has actually stopped.
    StopSession(SessionId, Reply<SessionInfo>),
    RestartSession(SessionId, Reply<SessionInfo>),
    RenameSession(SessionRename, Reply<SessionInfo>),
    /// `notify.mute` (protocol 0.34).
    SetMuted(NotifyMute, Reply<SessionInfo>),
    /// Answered once no session is live any more. The flag removes the sessions from the
    /// restore set (`--forget`).
    StopAll {
        forget: bool,
        reply: Reply<()>,
    },
    /// From a terminal's watcher task.
    TerminalMeta(TerminalId, TerminalMeta),
    TerminalExited(TerminalId, ExitStatus),
    AgentReport(Box<AgentReportRequest>, Reply<AgentApplied>),
    AgentRelease(AgentReleaseRequest, Reply<AgentApplied>),
    AgentTree(Option<SessionId>, Reply<AgentTree>),
    /// `session.kill`: SIGKILL now; answered once the session has actually ended.
    KillSession(SessionId, Reply<SessionInfo>),
    /// The live sessions' root processes and unfinished agents (`resources.*`).
    ProcessTargets(Option<SessionId>, Reply<Vec<ProcessTarget>>),
    /// `chat.create` with a canonical `cwd` and, when it names no workspace, where the `cwd`
    /// goes; answered once the agent is up (or failed).
    /// `bool`s: an assistant; one that is created asleep, its agent started by the first
    /// message ([`RegistryHandle::create_assistant_chat`]).
    CreateChat(
        ChatCreate,
        Option<Assignment>,
        bool,
        bool,
        Reply<SessionInfo>,
    ),
    /// From the task that started a chat agent (`wake`: the agent of a sleeping assistant,
    /// started to be used; the session was idle all along).
    ChatStarted {
        session: SessionId,
        outcome: Result<Started, ErrorBody>,
        reply: Option<Reply<SessionInfo>>,
        wake: bool,
    },
    /// A message (or a model switch) needs the agent of a sleeping assistant: start it, and
    /// answer once it runs.
    WakeChat(SessionId, Reply<SessionInfo>),
    /// From a chat session's pump: the idle sleeper stopped the agent and the session sleeps.
    ChatSlept {
        session: SessionId,
    },
    /// From a chat session's pump: a turn started or ended (`turn`), and how many approvals
    /// and questions are pending now.
    ChatState {
        session: SessionId,
        turn: Option<bool>,
        pending: u32,
        questions: u32,
    },
    /// From a chat session's pump: the agent process is gone.
    ChatExited {
        session: SessionId,
        exit: ExitInfo,
    },
    /// From a chat session's pump: the adapter reports on an agent of the session's tree.
    ChatNode {
        session: SessionId,
        update: Box<NodeUpdate>,
    },
    /// From a chat session's pump: the harness reports its session mode changed.
    ChatMode {
        session: SessionId,
        mode: SessionMode,
    },
    /// From the checkpoint service: whether a chat's turns get checkpoints may have changed.
    ChatCheckpoints {
        session: SessionId,
    },
    /// Whether the session is a chat session, and live.
    ChatTarget(SessionId, Reply<bool>),
    /// `chat.set_model`: answered once the switch is journaled.
    SetChatModel(ChatSetModel, Reply<SessionInfo>),
    /// From the task that switched the model of a harness chat: the agent's answer.
    ChatModelSwitched {
        session: SessionId,
        model: String,
        outcome: Result<(), ErrorBody>,
        reply: Reply<SessionInfo>,
    },
    /// A worktree was created or removed: the remembered checkouts are stale. Answered once
    /// every session's placement is derived again.
    InvalidatePlacements(Reply<()>),
}

/// How a chat session's pump reaches the registry.
#[derive(Debug, Clone)]
pub(crate) struct ChatNotifier {
    tx: mpsc::WeakSender<Command>,
}

impl ChatNotifier {
    async fn send(&self, command: Command) {
        if let Some(tx) = self.tx.upgrade() {
            // A closed mailbox means the daemon is going down.
            let _ = tx.send(command).await;
        }
    }

    pub(crate) async fn state(
        &self,
        session: SessionId,
        turn: Option<bool>,
        pending: u32,
        questions: u32,
    ) {
        self.send(Command::ChatState {
            session,
            turn,
            pending,
            questions,
        })
        .await;
    }

    pub(crate) async fn exited(&self, session: SessionId, exit: ExitInfo) {
        self.send(Command::ChatExited { session, exit }).await;
    }

    pub(crate) async fn slept(&self, session: SessionId) {
        self.send(Command::ChatSlept { session }).await;
    }

    /// Asks the registry to start the agent of a sleeping assistant; returns once it runs, or
    /// with the reason it did not.
    pub(crate) async fn wake(&self, session: SessionId) -> Result<(), ErrorBody> {
        let unavailable = || ErrorBody::new(ErrorCode::Internal, "session registry unavailable");
        let tx = self.tx.upgrade().ok_or_else(unavailable)?;
        let (reply, response) = oneshot::channel();
        tx.send(Command::WakeChat(session, reply))
            .await
            .map_err(|_| unavailable())?;
        response.await.map_err(|_| unavailable())?.map(|_| ())
    }

    /// The adapter saw something about an agent of the session (the main one or a subagent).
    pub(crate) async fn node(&self, session: SessionId, update: NodeUpdate) {
        self.send(Command::ChatNode {
            session,
            update: Box::new(update),
        })
        .await;
    }

    /// The harness reports that its session mode changed (`ChatInfo.mode`).
    pub(crate) async fn mode(&self, session: SessionId, mode: SessionMode) {
        self.send(Command::ChatMode { session, mode }).await;
    }

    /// Whether the session's turns get checkpoints may have changed (`ChatInfo.checkpoints`).
    pub(crate) async fn checkpoints(&self, session: SessionId) {
        self.send(Command::ChatCheckpoints { session }).await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionCounts {
    pub total: u32,
    pub live: u32,
}

/// A closed session, as the session catalog needs to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedInfo {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub cwd: Option<PathBuf>,
}

/// What the resource manager needs to know about one live session.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessTarget {
    pub session_id: SessionId,
    /// The session's terminal, when it has one.
    pub terminal: Option<TerminalId>,
    /// The terminal's child, or a chat session's harness process; `None` for a direct chat and
    /// while the process is not up (or already reaped).
    pub root_pid: Option<u32>,
    /// A chat session (it has no terminal).
    pub chat: bool,
    /// The id of the main agent's node.
    pub root_node: Option<String>,
    /// The nodes of the session's agent tree that have not finished.
    pub agents: Vec<AgentNode>,
}

/// Handle to the registry actor; cheap to clone.
#[derive(Debug, Clone)]
pub struct RegistryHandle {
    tx: mpsc::Sender<Command>,
    chats: Arc<ChatManager>,
    /// Which folders are Home; decided before a request reaches the actor.
    rules: Arc<HomeRules>,
    /// `[projects] auto`.
    auto: bool,
}

impl RegistryHandle {
    async fn ask<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, ErrorBody> {
        let (reply, response) = oneshot::channel();
        let unavailable = || ErrorBody::new(ErrorCode::Internal, "session registry unavailable");
        self.tx
            .send(command(reply))
            .await
            .map_err(|_| unavailable())?;
        response.await.map_err(|_| unavailable())?
    }

    /// A workspace with the projects of `req.paths` (found or made) and `req.project_ids`.
    pub async fn create_workspace(&self, req: WorkspaceCreate) -> Result<WorkspaceInfo, ErrorBody> {
        let paths = req.paths.clone();
        let idents = blocking(move || {
            paths
                .iter()
                .map(|path| projects::resolve_folder(path).map(|folder| folder.ident()))
                .collect::<Result<Vec<_>, _>>()
        })
        .await??;
        self.ask(|r| Command::CreateWorkspace(req, idents, r)).await
    }

    /// Where a session that starts in `dir` goes when its request names no workspace: the
    /// project of the folder with `[projects] auto`, else Home (Home too when there is no usable
    /// directory).
    async fn assign(&self, dir: Option<PathBuf>) -> Result<Assignment, ErrorBody> {
        if !self.auto {
            return Ok(Assignment::Home);
        }
        let rules = Arc::clone(&self.rules);
        blocking(move || rules.assign_path(dir.as_deref())).await
    }

    /// `[projects] auto`: whether the daemon makes projects from the folders sessions start in.
    pub fn auto_projects(&self) -> bool {
        self.auto
    }

    pub async fn list_workspaces(&self) -> Result<WorkspaceList, ErrorBody> {
        self.ask(Command::ListWorkspaces).await
    }

    pub async fn rename_workspace(&self, req: WorkspaceRename) -> Result<WorkspaceInfo, ErrorBody> {
        self.ask(|r| Command::RenameWorkspace(req, r)).await
    }

    pub async fn close_workspace(&self, id: WorkspaceId) -> Result<(), ErrorBody> {
        self.ask(|r| Command::CloseWorkspace(id, r)).await
    }

    /// The project of `req.path` (Home for the home directory and the Home folder), created
    /// when the daemon does not know it yet.
    pub async fn open_workspace(&self, req: WorkspaceOpen) -> Result<WorkspaceOpened, ErrorBody> {
        let folder = resolve_folder(req.path).await?;
        let rules = Arc::clone(&self.rules);
        let assignment = blocking(move || rules.assign(&folder.path)).await?;
        self.ask(|r| Command::OpenWorkspace(assignment, req.created_by, r))
            .await
    }

    pub async fn add_root(
        &self,
        workspace_id: WorkspaceId,
        path: PathBuf,
    ) -> Result<WorkspaceInfo, ErrorBody> {
        let folder = resolve_folder(path).await?;
        let rules = Arc::clone(&self.rules);
        let assignment = blocking(move || rules.assign(&folder.path)).await?;
        self.ask(|r| Command::AddRoot(workspace_id, assignment, r))
            .await
    }

    pub async fn remove_root(
        &self,
        workspace_id: WorkspaceId,
        path: PathBuf,
    ) -> Result<WorkspaceInfo, ErrorBody> {
        self.ask(|r| Command::RemoveRoot(workspace_id, path, r))
            .await
    }

    pub async fn recent_projects(&self, limit: Option<u32>) -> Result<RecentProjects, ErrorBody> {
        self.ask(|r| Command::RecentProjects(limit, r)).await
    }

    pub async fn move_workspace(&self, req: WorkspaceMove) -> Result<WorkspaceList, ErrorBody> {
        self.ask(|r| Command::MoveWorkspace(req, r)).await
    }

    pub async fn pin_workspace(&self, req: WorkspacePin) -> Result<WorkspaceInfo, ErrorBody> {
        self.ask(|r| Command::PinWorkspace(req, r)).await
    }

    /// Puts a project into a workspace, or out of its workspace.
    pub async fn assign_workspace(&self, req: WorkspaceAssign) -> Result<WorkspaceInfo, ErrorBody> {
        self.ask(|r| Command::AssignWorkspace(req, r)).await
    }

    /// Removes a workspace; its projects stay.
    pub async fn delete_workspace(&self, req: WorkspaceDelete) -> Result<(), ErrorBody> {
        self.ask(|r| Command::DeleteWorkspace(req, r)).await
    }

    /// Input reached `terminal`; `by_user` unless an agent typed it. Never waits: a full
    /// mailbox just misses one note (the next keystroke makes it again).
    pub fn note_input(&self, terminal: TerminalId, by_user: bool) {
        let _ = self.tx.try_send(Command::Input(terminal, by_user));
    }

    /// The user acted in a session that is not a terminal (`chat.send`).
    pub fn note_user_action(&self, session: SessionId) {
        let _ = self.tx.try_send(Command::UserAction(session));
    }

    /// `session.recovery_ack`: dismisses what the last restart did to one session, or to all.
    pub async fn ack_recovery(&self, req: RecoveryAck) -> Result<RecoveryAcked, ErrorBody> {
        self.ask(|r| Command::RecoveryAck(req, r)).await
    }

    /// A restart cost the session something the registry could not know when it restored it.
    pub async fn note_recovery_loss(&self, session: SessionId, lost: Lost) {
        let _ = self.tx.send(Command::RecoveryLost(session, lost)).await;
    }

    pub async fn snapshot(&self) -> Result<SessionSnapshot, ErrorBody> {
        self.ask(Command::Snapshot).await
    }

    pub async fn session_counts(&self) -> Result<SessionCounts, ErrorBody> {
        self.ask(Command::SessionCounts).await
    }

    /// Without `req.workspace_id` the session goes to the project of its `cwd`, which is
    /// created when it is new.
    pub async fn create_session(&self, req: SessionCreate) -> Result<SessionInfo, ErrorBody> {
        let assignment = match req.workspace_id {
            Some(_) => None,
            None => {
                let SessionSpec::Terminal(spec) = &req.spec;
                Some(self.assign(spec.cwd.clone()).await?)
            }
        };
        self.ask(|r| Command::CreateSession(req, assignment, r))
            .await
    }

    pub async fn list_sessions(&self, filter: SessionListFilter) -> Result<SessionList, ErrorBody> {
        self.ask(|r| Command::ListSessions(filter, r)).await
    }

    /// Hangs up the session's process (killing it after [`STOP_GRACE`]) and returns the
    /// session once it is `stopped`.
    pub async fn stop_session(&self, id: SessionId) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::StopSession(id, r)).await
    }

    /// `session.close`: removes the session from the lists. A live session is stopped first
    /// (`req.stop_running`, else `invalid_state` with the busy data when something runs in it);
    /// answered once it is closed.
    pub async fn close_session(&self, req: SessionClose) -> Result<(), ErrorBody> {
        self.ask(|r| Command::CloseSession(req, r)).await
    }

    /// `session.reopen`: brings a closed session back the way a daemon restart does.
    pub async fn reopen_session(&self, id: SessionId) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::ReopenSession(id, r)).await
    }

    /// The closed sessions, most recently closed first.
    pub async fn closed_sessions(&self) -> Result<Vec<ClosedInfo>, ErrorBody> {
        self.ask(Command::ClosedSessions).await
    }

    /// SIGKILLs the session's whole process group (a chat session's harness and everything
    /// below it) and returns the session once it is `stopped`.
    pub async fn kill_session(&self, id: SessionId) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::KillSession(id, r)).await
    }

    /// The live sessions' root processes and unfinished agents; see [`ProcessTarget`].
    pub async fn process_targets(
        &self,
        session: Option<SessionId>,
    ) -> Result<Vec<ProcessTarget>, ErrorBody> {
        self.ask(|r| Command::ProcessTargets(session, r)).await
    }

    pub async fn restart_session(&self, id: SessionId) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::RestartSession(id, r)).await
    }

    pub async fn rename_session(&self, req: SessionRename) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::RenameSession(req, r)).await
    }

    /// `notify.mute`: sets `SessionInfo.muted` and remembers it across restarts.
    pub async fn set_muted(&self, req: NotifyMute) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::SetMuted(req, r)).await
    }

    /// Terminates every live session concurrently and returns once all have ended.
    ///
    /// The stopped sessions stay in the restore set (the next daemon respawns them) unless
    /// `forget`, which records them as stopped.
    pub async fn stop_all(&self, forget: bool) -> Result<(), ErrorBody> {
        self.ask(|reply| Command::StopAll { forget, reply }).await
    }

    /// A coding agent in the session reports its state and how to resume it.
    pub async fn report_agent(&self, req: AgentReportRequest) -> Result<AgentApplied, ErrorBody> {
        self.ask(|r| Command::AgentReport(Box::new(req), r)).await
    }

    /// The agent behind `req.source` is gone.
    pub async fn release_agent(&self, req: AgentReleaseRequest) -> Result<AgentApplied, ErrorBody> {
        self.ask(|r| Command::AgentRelease(req, r)).await
    }

    /// The agent trees of one session, or of every session that has one.
    pub async fn agent_tree(&self, session: Option<SessionId>) -> Result<AgentTree, ErrorBody> {
        self.ask(|r| Command::AgentTree(session, r)).await
    }

    /// The chat sessions' runtime: sending, interrupting, approvals, history.
    pub fn chats(&self) -> &Arc<ChatManager> {
        &self.chats
    }

    /// Opens an agent conversation as a session; returns once the agent is ready (state
    /// `idle`) or fails with `provider_error` when it could not start.
    pub async fn create_chat(&self, req: ChatCreate) -> Result<SessionInfo, ErrorBody> {
        self.create_chat_as(req, false, false).await
    }

    /// [`create_chat`](Self::create_chat) for an assistant (protocol 0.20): a harness session
    /// that is marked as one, so that it starts with its tools restricted and
    /// `assistant.list` finds it. Only the daemon's assistant service calls this. The session
    /// is created **asleep** (state `idle`, no process, nothing journaled): its agent starts
    /// with the first message (`chat.send`), so that opening Mawas costs nothing.
    pub async fn create_assistant_chat(&self, req: ChatCreate) -> Result<SessionInfo, ErrorBody> {
        self.create_chat_as(req, true, true).await
    }

    async fn create_chat_as(
        &self,
        mut req: ChatCreate,
        assistant: bool,
        asleep: bool,
    ) -> Result<SessionInfo, ErrorBody> {
        req.cwd = canonical_dir(req.cwd).await?;
        let assignment = match req.workspace_id {
            Some(_) => None,
            None => Some(self.assign(Some(req.cwd.clone())).await?),
        };
        self.ask(|r| Command::CreateChat(req, assignment, assistant, asleep, r))
            .await
    }

    /// Whether `id` is a chat session (else an error), and whether it is live.
    pub async fn chat_target(&self, id: SessionId) -> Result<bool, ErrorBody> {
        self.ask(|r| Command::ChatTarget(id, r)).await
    }

    /// Switches the model (and optionally the provider) of a running direct chat; its next
    /// turn uses it and the journal records the switch.
    pub async fn set_chat_model(&self, req: ChatSetModel) -> Result<SessionInfo, ErrorBody> {
        self.ask(|r| Command::SetChatModel(req, r)).await
    }

    /// `worktree.*` changed the checkouts on disk: forgets what is known of directories and
    /// derives every session's placement again (announcing the ones that changed).
    pub async fn invalidate_placements(&self) -> Result<(), ErrorBody> {
        self.ask(Command::InvalidatePlacements).await
    }
}

/// One terminal session and the daemon-side state around it.
#[derive(Debug)]
struct SessionEntry {
    info: SessionInfo,
    /// What a restart spawns again.
    spec: TerminalSpec,
    default_title: String,
    /// The user named the session (at creation or by rename); terminal titles no longer rename it.
    title_locked: bool,
    /// Last title the program set through OSC 0/2.
    osc_title: Option<String>,
    /// Kept after the process ends so clients can still read the final screen.
    terminal: Option<Arc<TerminalEntry>>,
    /// A stop was requested; the next exit counts as `stopped`.
    stopping: bool,
    stop_waiters: Vec<Reply<SessionInfo>>,
    ended_at_ms: Option<u64>,
    /// Native resume reference (JSON) recorded by an agent integration; kept as stored.
    resume_ref_json: Option<String>,
    /// Command to run instead of the spec's at the next spawn (a resume reference); used once.
    restore_argv: Option<Vec<String>>,
    /// The daemon is shutting down and takes this session with it: the database keeps this
    /// live state so the next daemon respawns it.
    keep_live: Option<SessionState>,
    /// Title or cwd changed since the last write.
    dirty: bool,
    /// The main agent's last `message` and conversation id (what `SessionInfo.agent` carries
    /// besides the state).
    agent_message: Option<String>,
    agent_session_id: Option<String>,
    /// The agents of the session: the main agent and its subagents, with the sequence numbers
    /// of their reporters (`agent.report` / `agent.release`).
    tree: TreeState,
    /// What an agent chat session is (`kind` harness); such a session has no terminal.
    chat: Option<ChatSpec>,
    /// The command line running in the foreground of the terminal (the last running block,
    /// secrets removed), for the next start's `session.recovery`.
    running_command: Option<String>,
    /// `session.close` is waiting for the session to end: it is removed once it has.
    close: Option<CloseRequest>,
}

/// A `session.close` that stopped a live session and waits for its exit.
#[derive(Debug)]
struct CloseRequest {
    /// Answered once the session is closed.
    waiters: Vec<Reply<()>>,
    /// The agent resume reference the session had when it was closed: the exit forgets it, the
    /// closed row keeps it so that `session.reopen` resumes the agent.
    resume_ref_json: Option<String>,
}

impl SessionEntry {
    fn advance(&mut self, event: SessionEvent) {
        match self.info.state.transition(event) {
            Ok(next) => {
                self.info.state = next;
                self.ended_at_ms = (!self.info.state.is_live()).then(now_ms);
            }
            Err(error) => error!(session = %self.info.id, %error, "invalid session transition"),
        }
    }

    /// Signals the process once; the exit arrives later through the watcher.
    fn request_stop(&mut self, chats: &ChatManager) {
        if self.stopping {
            return;
        }
        self.stopping = true;
        if let Some(entry) = &self.terminal
            && let Err(error) = entry.terminal().terminate(STOP_GRACE)
        {
            warn!(session = %self.info.id, %error, "could not signal the terminal");
        }
        if self.chat.is_some() {
            if self.keep_live.is_some() {
                chats.stop_for_daemon(self.info.id);
            } else {
                chats.stop(self.info.id);
            }
        }
    }

    /// SIGKILLs the session's process group (or the harness of a chat session) now; the exit
    /// arrives later through the watcher. Unlike [`Self::request_stop`] it acts again when a
    /// stop is already under way: that is how a stuck stop is escalated.
    fn request_kill(&mut self, chats: &ChatManager) {
        self.stopping = true;
        if let Some(entry) = &self.terminal
            && let Err(error) = entry.terminal().kill()
        {
            warn!(session = %self.info.id, %error, "could not kill the terminal");
        }
        if self.chat.is_some() {
            chats.kill(self.info.id);
        }
    }

    /// The agent is gone — its process ended, or the shell prompts again — without having said
    /// so: every node of its tree has lost contact, and its message, conversation id and
    /// resume reference are forgotten. Returns whether a resume reference was dropped.
    fn forget_agent(&mut self, now_ms: u64) -> bool {
        self.tree.session_ended(now_ms);
        self.agent_message = None;
        self.agent_session_id = None;
        self.resume_ref_json.take().is_some()
    }
}

/// One workspace and the bookkeeping around it.
#[derive(Debug)]
struct WorkspaceEntry {
    /// What clients know: the last published state.
    info: WorkspaceInfo,
    /// Session activity happened that `info.last_active_ms` does not show yet (throttled).
    activity_pending: bool,
}

impl WorkspaceEntry {
    fn new(info: WorkspaceInfo) -> Self {
        Self {
            info,
            activity_pending: false,
        }
    }
}

#[derive(Debug)]
struct SessionRegistry {
    bus: Arc<EventBus>,
    hub: Arc<TerminalHub>,
    /// Exported to children as `HUTAN_SOCKET`.
    socket: PathBuf,
    /// Installed shell-integration scripts handed to every new terminal; `None` = no injection.
    shell_integration_dir: Option<PathBuf>,
    /// Where new terminals' base environment comes from (`[terminal] env_inherit`).
    env_inherit: EnvInherit,
    /// `SHELL` of sessions whose spec names none; `None` = the account's login shell.
    login_shell: Option<String>,
    /// Exported to children as `HUTAN_BIN`.
    hutan_bin: String,
    /// Weak so the actor stops once every [`RegistryHandle`] is gone.
    mailbox: mpsc::WeakSender<Command>,
    /// The Home workspace: where sessions that belong to no project go.
    home: WorkspaceId,
    /// Which folders are Home (restore re-derives a stale project with it).
    rules: Arc<HomeRules>,
    /// In `position` order.
    workspaces: Vec<WorkspaceEntry>,
    /// Closed projects, newest first.
    recent: Vec<stored_rows::RecentProject>,
    /// `position` for the next workspace row.
    next_position: i64,
    activity_interval: Duration,
    sessions: Vec<SessionEntry>,
    /// The sessions `session.close` removed, most recently closed first; their rows are kept in
    /// the database for `session.reopen`.
    closed: Vec<ClosedSession>,
    /// The sessions the user muted (`notify.mute`), open and closed ones: a reopened session
    /// is still muted. Stored whole under [`MUTED_KEY`] on every change.
    muted: HashSet<SessionId>,
    stop_all_waiters: Vec<Reply<()>>,
    chats: Arc<ChatManager>,
    storage: Storage,
    /// When sessions with unwritten title/cwd changes are written next.
    flush_at: Option<Instant>,
    /// When the earliest throttled workspace activity is applied.
    activity_flush_at: Option<Instant>,
    /// When the earliest held-back `agent.tree_updated` may go out.
    tree_flush_at: Option<Instant>,
    /// The commands running in terminals changed since they were stored.
    commands_dirty: bool,
    /// The git checkout of the directories sessions work in (`SessionInfo.placement`).
    placements: PlacementCache,
}

/// What [`spawn`] needs besides the shared services.
#[derive(Debug)]
pub struct RegistryConfig {
    /// Exported to children as `HUTAN_SOCKET`.
    pub socket: PathBuf,
    /// Installed shell-integration scripts handed to every new terminal; `None` = no injection.
    pub shell_integration_dir: Option<PathBuf>,
    /// `[terminal] env_inherit`: where new terminals' base environment comes from.
    pub env_inherit: EnvInherit,
    /// `SHELL` of sessions whose spec names none; `None` = the account's login shell.
    pub login_shell: Option<String>,
    /// The `hutan` CLI exported to sessions as `HUTAN_BIN`.
    pub hutan_bin: PathBuf,
    pub storage: Storage,
    pub restore: RestoreMode,
    /// The folder of the Home workspace (existing, canonical).
    pub home_folder: PathBuf,
    /// The user's home directory: it is Home, not a project.
    pub user_home: Option<PathBuf>,
    /// `[projects] auto`: sessions without a workspace go to the project of their folder.
    pub auto_projects: bool,
    /// Shortest time between two `last_active_ms` updates of one workspace.
    pub activity_interval: Duration,
    /// How to start `codex app-server` for chat sessions.
    pub codex: CodexConfig,
    /// The ACP harnesses (`omp`, `claude`) chat sessions can run through.
    pub harnesses: Arc<Harnesses>,
    /// The model providers of direct chats.
    pub providers: Arc<ProviderService>,
    /// `gateway.max_context_chars`.
    pub max_context_chars: usize,
    /// How long an idle harness assistant keeps its agent process (`Duration::ZERO`: forever).
    pub idle_stop: Duration,
    /// `assistant.mode` and `assistant.max_actions_per_turn` at start.
    pub assistant_policy: AssistantPolicy,
    /// `<home>/tmp`: scratch files of chat deliveries (clipboard images for Codex).
    pub tmp_dir: PathBuf,
    /// Checkpoints of chat turns (ADR 0019).
    pub checkpoints: Arc<crate::checkpoints::turns::TurnCheckpoints>,
    /// Usage and limits records: what the turns of chats used.
    pub usage: Arc<crate::usage::UsageService>,
}

/// Starts the actor from the stored state. It stops when every [`RegistryHandle`] is dropped.
///
/// Sessions that were live when the previous daemon stopped are spawned again here (same id,
/// title, and workspace; last known cwd) unless `config.restore` is `Stopped`. Must run inside the
/// Tokio runtime.
pub fn spawn(
    bus: Arc<EventBus>,
    hub: Arc<TerminalHub>,
    config: RegistryConfig,
    stored: Stored,
) -> RegistryHandle {
    let (tx, rx) = mpsc::channel(MAILBOX_CAPACITY);
    let chats = ChatManager::new(
        bus.clone(),
        config.storage.clone(),
        ChatNotifier { tx: tx.downgrade() },
        config.codex,
        config.harnesses,
        config.providers,
        config.max_context_chars,
        config.idle_stop,
        config.assistant_policy,
        config.tmp_dir,
        config.checkpoints,
        config.usage,
    );
    let (workspaces, home, next_position) =
        restored_workspaces(&stored, &config.storage, &config.home_folder);
    let rules = Arc::new(HomeRules::new(config.user_home, config.home_folder));
    let mut registry = SessionRegistry {
        bus,
        hub,
        socket: config.socket,
        shell_integration_dir: config.shell_integration_dir,
        env_inherit: config.env_inherit,
        login_shell: config.login_shell,
        hutan_bin: config.hutan_bin.to_string_lossy().into_owned(),
        mailbox: tx.downgrade(),
        home,
        rules: Arc::clone(&rules),
        workspaces,
        recent: stored.recent_projects,
        next_position,
        activity_interval: config.activity_interval,
        sessions: Vec::new(),
        closed: stored.closed_sessions,
        muted: HashSet::new(),
        stop_all_waiters: Vec::new(),
        chats: chats.clone(),
        storage: config.storage,
        flush_at: None,
        activity_flush_at: None,
        tree_flush_at: None,
        commands_dirty: false,
        placements: PlacementCache::default(),
    };
    registry.restore_sessions(
        stored.sessions,
        config.restore,
        stored.agent_trees,
        stored.facts,
    );
    tokio::spawn(registry.run(rx));
    RegistryHandle {
        tx,
        chats,
        rules,
        auto: config.auto_projects,
    }
}

/// The stored workspaces, Home among them (created and stored when missing, bound to
/// `home_folder`), and the next free position.
fn restored_workspaces(
    stored: &Stored,
    storage: &Storage,
    home_folder: &Path,
) -> (Vec<WorkspaceEntry>, WorkspaceId, i64) {
    let mut workspaces: Vec<WorkspaceEntry> = stored
        .workspaces
        .iter()
        .map(|w| {
            WorkspaceEntry::new(WorkspaceInfo {
                id: w.id,
                name: w.name.clone(),
                roots: w.roots.clone(),
                kind: kind_from_row(w.kind),
                pinned: w.pinned,
                position: w.position,
                created_by: created_by_from_row(w.created_by),
                last_active_ms: w.last_active_ms,
                live_sessions: 0,
                total_sessions: 0,
                parent_id: w.parent_id,
                project: (w.kind == stored_rows::WorkspaceKind::Project)
                    .then(|| project_info(&w.roots, w.common_dir.clone())),
            })
        })
        .collect();
    // A workspace shows the folders of its projects as its roots.
    let derived: Vec<(usize, Vec<PathBuf>)> = workspaces
        .iter()
        .enumerate()
        .filter(|(_, w)| w.info.kind == WorkspaceKind::Workspace)
        .map(|(index, w)| (index, member_roots(&workspaces, w.info.id)))
        .collect();
    for (index, roots) in derived {
        workspaces[index].info.roots = roots;
    }
    let mut next_position = stored
        .workspaces
        .iter()
        .map(|w| w.position)
        .max()
        .map_or(0, |last| last + 1);
    let existing = workspaces
        .iter_mut()
        .find(|w| w.info.kind == WorkspaceKind::Home);
    let home = match existing {
        Some(home) => {
            // The configured folder is the truth; the user may have changed it.
            if home.info.roots != [home_folder] {
                home.info.roots = vec![home_folder.to_owned()];
                log_write(
                    storage
                        .workspaces()
                        .set_roots(home.info.id, home.info.roots.clone()),
                    "the Home folder",
                );
            }
            home.info.id
        }
        None => {
            let now = now_ms();
            // First in the list, ahead of whatever was stored.
            let position = stored
                .workspaces
                .iter()
                .map(|w| w.position)
                .min()
                .map_or(0, |first| first - 1);
            let info = WorkspaceInfo {
                id: WorkspaceId::new(),
                name: HOME_WORKSPACE_NAME.to_owned(),
                roots: vec![home_folder.to_owned()],
                kind: WorkspaceKind::Home,
                pinned: false,
                position,
                created_by: CreatedBy::User,
                last_active_ms: now,
                live_sessions: 0,
                total_sessions: 0,
                parent_id: None,
                project: None,
            };
            log_write(
                storage.workspaces().insert(row_of(&info, now)),
                "the Home workspace",
            );
            let id = info.id;
            workspaces.insert(0, WorkspaceEntry::new(info));
            id
        }
    };
    next_position = next_position.max(1);
    (workspaces, home, next_position)
}

fn kind_from_row(kind: stored_rows::WorkspaceKind) -> WorkspaceKind {
    match kind {
        stored_rows::WorkspaceKind::Home => WorkspaceKind::Home,
        stored_rows::WorkspaceKind::Project => WorkspaceKind::Project,
        stored_rows::WorkspaceKind::Workspace => WorkspaceKind::Workspace,
    }
}

fn kind_to_row(kind: WorkspaceKind) -> stored_rows::WorkspaceKind {
    match kind {
        WorkspaceKind::Home => stored_rows::WorkspaceKind::Home,
        WorkspaceKind::Project => stored_rows::WorkspaceKind::Project,
        WorkspaceKind::Workspace => stored_rows::WorkspaceKind::Workspace,
    }
}

fn created_by_from_row(created_by: stored_rows::CreatedBy) -> CreatedBy {
    match created_by {
        stored_rows::CreatedBy::User => CreatedBy::User,
        stored_rows::CreatedBy::Cli => CreatedBy::Cli,
        stored_rows::CreatedBy::Agent => CreatedBy::Agent,
    }
}

fn created_by_to_row(created_by: CreatedBy) -> stored_rows::CreatedBy {
    match created_by {
        CreatedBy::User => stored_rows::CreatedBy::User,
        CreatedBy::Cli => stored_rows::CreatedBy::Cli,
        CreatedBy::Agent => stored_rows::CreatedBy::Agent,
    }
}

/// The row of a workspace that is being created.
fn row_of(info: &WorkspaceInfo, created_at_ms: u64) -> WorkspaceRow {
    WorkspaceRow {
        id: info.id,
        name: info.name.clone(),
        // A workspace's roots are those of its projects: they are not stored twice.
        roots: if info.kind == WorkspaceKind::Workspace {
            Vec::new()
        } else {
            info.roots.clone()
        },
        kind: kind_to_row(info.kind),
        pinned: info.pinned,
        created_by: created_by_to_row(info.created_by),
        created_at_ms,
        last_active_ms: info.last_active_ms,
        position: info.position,
        common_dir: info
            .project
            .as_ref()
            .and_then(|project| project.common_dir.clone()),
        parent_id: info.parent_id,
    }
}

fn not_a_project(id: WorkspaceId) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidState,
        format!("{id} is not a project; Home and workspaces cannot be put into a workspace"),
    )
}

/// The identity of a project whose folder is the first of `roots` (empty when it has none).
fn project_info(roots: &[PathBuf], common_dir: Option<PathBuf>) -> ProjectInfo {
    ProjectInfo {
        common_dir,
        root: roots.first().cloned().unwrap_or_default(),
    }
}

/// The folders of the projects in workspace `id`, in list order.
fn member_roots(workspaces: &[WorkspaceEntry], id: WorkspaceId) -> Vec<PathBuf> {
    workspaces
        .iter()
        .filter(|w| w.info.parent_id == Some(id))
        .filter_map(|w| w.info.project.as_ref().map(|project| project.root.clone()))
        .collect()
}

/// The repository identity of a workspace.
fn entry_common_dir(entry: &WorkspaceEntry) -> Option<&PathBuf> {
    entry
        .info
        .project
        .as_ref()
        .and_then(|project| project.common_dir.as_ref())
}

/// Runs filesystem work on the blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ErrorBody> {
    tokio::task::spawn_blocking(work).await.map_err(|error| {
        error!(%error, "filesystem work failed");
        ErrorBody::new(ErrorCode::Internal, "could not resolve the folder")
    })
}

/// Canonicalises a folder on the blocking pool.
async fn resolve_folder(path: PathBuf) -> Result<Folder, ErrorBody> {
    blocking(move || projects::resolve_folder(&path)).await?
}

/// The canonical form of an existing directory, resolved on the blocking pool.
async fn canonical_dir(path: PathBuf) -> Result<PathBuf, ErrorBody> {
    if !path.is_absolute() {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("`{}` is not an absolute path", path.display()),
        ));
    }
    let shown = path.display().to_string();
    tokio::task::spawn_blocking(move || std::fs::canonicalize(&path).ok().filter(|p| p.is_dir()))
        .await
        .map_err(|error| {
            error!(%error, "resolving a directory failed");
            ErrorBody::new(ErrorCode::Internal, "could not resolve the directory")
        })?
        .ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::BadRequest,
                format!("`{shown}` is not an existing directory"),
            )
        })
}

/// Storage failures never take request handling down: the in-memory state stays authoritative
/// and the next write may succeed.
pub(crate) fn log_write(result: Result<(), StorageError>, what: &str) {
    match result {
        Ok(()) => {}
        // The writer is closed once the daemon has finished shutting down; nothing is lost then.
        Err(StorageError::Closed) => debug!("storage is closed; dropped a write of {what}"),
        Err(error) => error!(%error, "could not store {what}"),
    }
}

impl SessionRegistry {
    async fn run(mut self, mut mailbox: mpsc::Receiver<Command>) {
        loop {
            let deadline = [self.flush_at, self.activity_flush_at, self.tree_flush_at]
                .into_iter()
                .flatten()
                .min();
            let command = match deadline {
                Some(deadline) => tokio::select! {
                    command = mailbox.recv() => command,
                    () = sleep_until(deadline) => {
                        self.on_timer();
                        continue;
                    }
                },
                None => mailbox.recv().await,
            };
            let Some(command) = command else { break };
            // A dropped reply means the asking connection went away; nothing to do.
            match command {
                Command::CreateWorkspace(req, idents, reply) => {
                    let _ = reply.send(self.create_workspace(req, &idents));
                }
                Command::ListWorkspaces(reply) => {
                    let _ = reply.send(Ok(self.workspace_list()));
                }
                Command::RenameWorkspace(req, reply) => {
                    let _ = reply.send(self.rename_workspace(req));
                }
                Command::CloseWorkspace(id, reply) => {
                    let _ = reply.send(self.close_workspace(id));
                }
                Command::OpenWorkspace(assignment, created_by, reply) => {
                    let _ = reply.send(self.open_workspace(assignment, created_by));
                }
                Command::AddRoot(id, assignment, reply) => {
                    let _ = reply.send(self.add_root(id, assignment));
                }
                Command::RemoveRoot(id, path, reply) => {
                    let _ = reply.send(self.remove_root(id, &path));
                }
                Command::RecentProjects(limit, reply) => {
                    let _ = reply.send(Ok(self.recent_projects(limit)));
                }
                Command::MoveWorkspace(req, reply) => {
                    let _ = reply.send(self.move_workspace(req));
                }
                Command::PinWorkspace(req, reply) => {
                    let _ = reply.send(self.pin_workspace(req));
                }
                Command::AssignWorkspace(req, reply) => {
                    let _ = reply.send(self.assign_workspace(req));
                }
                Command::DeleteWorkspace(req, reply) => {
                    let _ = reply.send(self.delete_workspace(req));
                }
                Command::Activity(terminal) => self.on_activity(terminal),
                Command::Input(terminal, by_user) => {
                    self.on_activity(terminal);
                    if by_user && let Some(index) = self.terminal_index(terminal) {
                        self.clear_recovery(index);
                    }
                }
                Command::UserAction(session) => {
                    if let Ok(index) = self.session_index(session) {
                        self.clear_recovery(index);
                    }
                }
                Command::RecoveryAck(req, reply) => {
                    let _ = reply.send(self.ack_recovery(req));
                }
                Command::RecoveryLost(session, lost) => {
                    if let Ok(index) = self.session_index(session) {
                        self.add_recovery_loss(index, lost);
                    }
                }
                Command::Snapshot(reply) => {
                    // The actor is the only publisher of the state it snapshots, so the bus's
                    // last seq is exactly the watermark of this state.
                    let _ = reply.send(Ok(SessionSnapshot {
                        workspaces: self.workspace_list().workspaces,
                        sessions: self.sessions.iter().map(|s| s.info.clone()).collect(),
                        seq: self.bus.last_seq(),
                    }));
                }
                Command::SessionCounts(reply) => {
                    let _ = reply.send(Ok(SessionCounts {
                        total: self.sessions.len() as u32,
                        live: self.live_count() as u32,
                    }));
                }
                Command::CreateSession(req, assignment, reply) => {
                    let _ = reply.send(self.create_session(req, assignment));
                }
                Command::ListSessions(filter, reply) => {
                    let _ = reply.send(Ok(SessionList {
                        sessions: self
                            .sessions
                            .iter()
                            .filter(|s| {
                                filter
                                    .workspace_id
                                    .is_none_or(|id| id == s.info.workspace_id)
                            })
                            .map(|s| s.info.clone())
                            .collect(),
                        seq: self.bus.last_seq(),
                    }));
                }
                Command::StopSession(id, reply) => self.stop_session(id, reply),
                Command::CloseSession(req, reply) => self.close_session(req, reply),
                Command::ReopenSession(id, reply) => {
                    let _ = reply.send(self.reopen_session(id));
                }
                Command::ClosedSessions(reply) => {
                    let _ = reply.send(Ok(self
                        .closed
                        .iter()
                        .map(|closed| ClosedInfo {
                            id: closed.session.id,
                            workspace_id: closed.session.workspace_id,
                            title: closed.session.title.clone(),
                            cwd: closed.session.cwd.clone(),
                        })
                        .collect()));
                }
                Command::RestartSession(id, reply) => {
                    let _ = reply.send(self.restart_session(id));
                }
                Command::RenameSession(req, reply) => {
                    let _ = reply.send(self.rename_session(req));
                }
                Command::SetMuted(req, reply) => {
                    let _ = reply.send(self.set_muted(req));
                }
                Command::StopAll { forget, reply } => self.stop_all(forget, reply),
                Command::TerminalMeta(terminal, meta) => self.on_meta(terminal, meta),
                Command::TerminalExited(terminal, status) => self.on_exited(terminal, &status),
                Command::AgentReport(req, reply) => {
                    let _ = reply.send(self.report_agent(*req));
                }
                Command::AgentRelease(req, reply) => {
                    let _ = reply.send(self.release_agent(req));
                }
                Command::AgentTree(session, reply) => {
                    let _ = reply.send(self.agent_tree(session));
                }
                Command::KillSession(id, reply) => self.kill_session(id, reply),
                Command::ProcessTargets(session, reply) => {
                    let _ = reply.send(self.process_targets(session));
                }
                Command::CreateChat(req, assignment, assistant, asleep, reply) => {
                    self.create_chat(req, assignment, assistant, asleep, reply);
                }
                Command::ChatStarted {
                    session,
                    outcome,
                    reply,
                    wake,
                } => self.on_chat_started(session, outcome, reply, wake),
                Command::WakeChat(session, reply) => self.wake_chat(session, reply),
                Command::ChatSlept { session } => self.on_chat_slept(session),
                Command::ChatState {
                    session,
                    turn,
                    pending,
                    questions,
                } => self.on_chat_state(session, turn, pending, questions),
                Command::ChatExited { session, exit } => self.on_chat_exited(session, &exit),
                Command::ChatNode { session, update } => self.on_chat_node(session, *update),
                Command::ChatMode { session, mode } => self.on_chat_mode(session, mode),
                Command::ChatCheckpoints { session } => self.on_chat_checkpoints(session),
                Command::InvalidatePlacements(reply) => {
                    self.invalidate_placements();
                    let _ = reply.send(Ok(()));
                }
                Command::ChatTarget(id, reply) => {
                    let _ = reply.send(self.chat_target(id));
                }
                Command::SetChatModel(req, reply) => self.set_chat_model(req, reply),
                Command::ChatModelSwitched {
                    session,
                    model,
                    outcome,
                    reply,
                } => self.on_chat_model_switched(session, model, outcome, reply),
            }
        }
        self.flush_dirty();
    }

    fn live_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|s| s.info.state.is_live())
            .count()
    }

    // Workspaces ------------------------------------------------------------------------------

    fn workspace_list(&self) -> WorkspaceList {
        WorkspaceList {
            workspaces: self.workspaces.iter().map(|w| w.info.clone()).collect(),
        }
    }

    /// The workspace a project is in.
    fn parent_of(&self, id: WorkspaceId) -> Option<WorkspaceId> {
        self.workspaces
            .iter()
            .find(|w| w.info.id == id)
            .and_then(|w| w.info.parent_id)
    }

    /// `(live, total)` sessions of a workspace; a workspace counts those of its projects too.
    fn session_counts_of(&self, id: WorkspaceId) -> (u32, u32) {
        let mut counts = (0, 0);
        for session in self.sessions.iter().filter(|s| {
            s.info.workspace_id == id || self.parent_of(s.info.workspace_id) == Some(id)
        }) {
            counts.1 += 1;
            counts.0 += u32::from(session.info.state.is_live());
        }
        counts
    }

    /// Fills in the session counts after the sessions were restored; nothing is announced
    /// (clients take a snapshot after a daemon start).
    fn recount_sessions(&mut self) {
        for index in 0..self.workspaces.len() {
            let (live, total) = self.session_counts_of(self.workspaces[index].info.id);
            let info = &mut self.workspaces[index].info;
            info.live_sessions = live;
            info.total_sessions = total;
        }
    }

    /// Applies `change`, refreshes the session counts, and announces the result when anything
    /// differs from what clients know. The event is published before the state changes, so a
    /// rejected event leaves everything as it was. Storage is the caller's business.
    fn update_workspace(
        &mut self,
        id: WorkspaceId,
        change: impl FnOnce(&mut WorkspaceInfo),
    ) -> Result<WorkspaceInfo, ErrorBody> {
        let index = self.index_of(id)?;
        let mut next = self.workspaces[index].info.clone();
        change(&mut next);
        if next.kind == WorkspaceKind::Workspace {
            next.roots = member_roots(&self.workspaces, id);
        }
        (next.live_sessions, next.total_sessions) = self.session_counts_of(id);
        if next != self.workspaces[index].info {
            self.publish(&Event::WorkspaceUpdated {
                workspace: next.clone(),
            })?;
            self.workspaces[index].info = next.clone();
        }
        Ok(next)
    }

    /// Announces new session counts of a workspace (a session changed state), and of the
    /// workspace its project is in.
    fn sync_workspace(&mut self, id: WorkspaceId) {
        let parent = self.parent_of(id);
        for id in std::iter::once(id).chain(parent) {
            if let Err(error) = self.update_workspace(id, |_| {}) {
                warn!(workspace = %id, message = %error.message, "could not announce a workspace change");
            }
        }
    }

    /// A workspace (the user's container) with the projects of `idents` (found or made) and
    /// the existing projects `req.project_ids`.
    fn create_workspace(
        &mut self,
        req: WorkspaceCreate,
        idents: &[Ident],
    ) -> Result<WorkspaceInfo, ErrorBody> {
        let name = validated_name(req.name, "workspace")?;
        let created_by = req.created_by.unwrap_or_default();
        for id in &req.project_ids {
            if self.workspaces[self.index_of(*id)?].info.kind != WorkspaceKind::Project {
                return Err(not_a_project(*id));
            }
        }
        let now = now_ms();
        let workspace = WorkspaceInfo {
            id: WorkspaceId::new(),
            name,
            roots: Vec::new(),
            kind: WorkspaceKind::Workspace,
            pinned: false,
            position: self.next_position,
            created_by,
            last_active_ms: now,
            live_sessions: 0,
            total_sessions: 0,
            parent_id: None,
            project: None,
        };
        self.publish(&Event::WorkspaceUpdated {
            workspace: workspace.clone(),
        })?;
        self.next_position += 1;
        log_write(
            self.storage.workspaces().insert(row_of(&workspace, now)),
            "a new workspace",
        );
        let id = workspace.id;
        self.workspaces.push(WorkspaceEntry::new(workspace));
        // Members: found or made, then moved in. A folder that is Home's has no project.
        let mut members = req.project_ids;
        for ident in idents {
            let (project, _) = self.project_of(ident, created_by, false)?;
            members.push(project.id);
        }
        for project in members {
            self.set_parent(project, Some(id))?;
        }
        Ok(self.workspaces[self.index_of(id)?].info.clone())
    }

    /// Creates, announces, and stores a project at the end of the list.
    fn add_project(
        &mut self,
        name: String,
        ident: &Ident,
        created_by: CreatedBy,
    ) -> Result<WorkspaceInfo, ErrorBody> {
        let now = now_ms();
        let roots = vec![ident.root.clone()];
        let project = project_info(&roots, ident.common_dir.clone());
        let workspace = WorkspaceInfo {
            id: WorkspaceId::new(),
            name,
            roots,
            kind: WorkspaceKind::Project,
            pinned: false,
            position: self.next_position,
            created_by,
            last_active_ms: now,
            live_sessions: 0,
            total_sessions: 0,
            parent_id: None,
            project: Some(project),
        };
        self.publish(&Event::WorkspaceUpdated {
            workspace: workspace.clone(),
        })?;
        self.next_position += 1;
        log_write(
            self.storage.workspaces().insert(row_of(&workspace, now)),
            "a new project",
        );
        self.workspaces.push(WorkspaceEntry::new(workspace.clone()));
        self.remember_project(&workspace, now);
        Ok(workspace)
    }

    /// Puts the project into the workspace `parent` (a workspace), or out of its workspace, and
    /// announces the project and the workspaces whose members changed. Storage included.
    fn set_parent(
        &mut self,
        project: WorkspaceId,
        parent: Option<WorkspaceId>,
    ) -> Result<WorkspaceInfo, ErrorBody> {
        let before = self.parent_of(project);
        let updated = self.update_workspace(project, |w| w.parent_id = parent)?;
        if before != parent {
            log_write(
                self.storage.workspaces().set_parent(project, parent),
                "a project's workspace",
            );
            for container in before.into_iter().chain(parent) {
                self.sync_workspace(container);
            }
        }
        Ok(updated)
    }

    /// Notes a project in the recent list (by its primary root); Home is not a project.
    fn remember_project(&mut self, workspace: &WorkspaceInfo, opened_ms: u64) {
        if workspace.kind != WorkspaceKind::Project {
            return;
        }
        let Some(path) = workspace.roots.first() else {
            return;
        };
        let project = stored_rows::RecentProject {
            path: path.clone(),
            name: workspace.name.clone(),
            last_opened_ms: opened_ms,
        };
        match self.recent.iter_mut().find(|r| r.path == project.path) {
            Some(known) => {
                known.name.clone_from(&project.name);
                known.last_opened_ms = known.last_opened_ms.max(opened_ms);
            }
            None => self.recent.push(project.clone()),
        }
        self.recent.sort_by(|a, b| {
            b.last_opened_ms
                .cmp(&a.last_opened_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        self.recent.truncate(stored_rows::RECENT_PROJECTS_KEPT);
        log_write(
            self.storage.recent_projects().upsert(project),
            "a recent project",
        );
    }

    fn rename_workspace(&mut self, req: WorkspaceRename) -> Result<WorkspaceInfo, ErrorBody> {
        let name = validated_name(req.name, "workspace")?;
        let workspace = self.update_workspace(req.workspace_id, |w| w.name = name)?;
        log_write(
            self.storage
                .workspaces()
                .rename(workspace.id, workspace.name.clone()),
            "a workspace rename",
        );
        Ok(workspace)
    }

    /// Closes a project (it stays in the recent list) or Home's neighbours; closing a workspace
    /// deletes it.
    fn close_workspace(&mut self, id: WorkspaceId) -> Result<(), ErrorBody> {
        let index = self.index_of(id)?;
        if id == self.home {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "the Home workspace cannot be closed",
            ));
        }
        self.remove_workspace(index)
    }

    /// `workspace.delete`: a workspace (never a project or Home) goes, its projects stay.
    fn delete_workspace(&mut self, req: WorkspaceDelete) -> Result<(), ErrorBody> {
        let index = self.index_of(req.workspace_id)?;
        if self.workspaces[index].info.kind != WorkspaceKind::Workspace {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!(
                    "{} is not a workspace; a project is closed with `workspace.close`",
                    req.workspace_id
                ),
            ));
        }
        self.remove_workspace(index)
    }

    /// Removes the workspace at `index` (a project or a workspace) unless it has live sessions
    /// of its own. Its ended sessions and tasks go with it; the projects of a workspace stay,
    /// ungrouped, and the workspace of a project loses a member.
    fn remove_workspace(&mut self, index: usize) -> Result<(), ErrorBody> {
        let id = self.workspaces[index].info.id;
        let live = self
            .sessions
            .iter()
            .filter(|s| s.info.workspace_id == id && s.info.state.is_live())
            .count();
        if live > 0 {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("workspace has {live} live session(s); stop them first"),
            ));
        }
        let members: Vec<WorkspaceId> = self
            .workspaces
            .iter()
            .filter(|w| w.info.parent_id == Some(id))
            .map(|w| w.info.id)
            .collect();
        for member in members {
            self.set_parent(member, None)?;
        }
        let index = self.index_of(id)?;
        self.publish(&Event::WorkspaceClosed { workspace_id: id })?;
        let closed = self.workspaces.remove(index).info;
        // A project stays in the recent list.
        self.remember_project(&closed, closed.last_active_ms);
        // The sessions' rows go with the workspace's.
        log_write(self.storage.workspaces().delete(id), "a workspace removal");
        // Ended sessions go with their workspace, and so do their terminals.
        let hub = &self.hub;
        self.closed.retain(|c| c.session.workspace_id != id);
        self.sessions.retain(|s| {
            let keep = s.info.workspace_id != id;
            if !keep && let Some(terminal) = &s.terminal {
                hub.remove(terminal.id());
            }
            keep
        });
        if let Some(parent) = closed.parent_id {
            self.sync_workspace(parent);
        }
        Ok(())
    }

    /// `workspace.assign`: a project into a workspace, or out of its workspace.
    fn assign_workspace(&mut self, req: WorkspaceAssign) -> Result<WorkspaceInfo, ErrorBody> {
        let index = self.index_of(req.project_id)?;
        if self.workspaces[index].info.kind != WorkspaceKind::Project {
            return Err(not_a_project(req.project_id));
        }
        if let Some(parent) = req.workspace_id
            && self.workspaces[self.index_of(parent)?].info.kind != WorkspaceKind::Workspace
        {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("{parent} is not a workspace; only a workspace holds projects"),
            ));
        }
        self.set_parent(req.project_id, req.workspace_id)
    }

    /// The project `ident` names: the one with the same repository, else the one whose folder it
    /// is, else — with `containing`, for a folder outside git — the deepest folder project
    /// containing it. The first in list order wins a tie.
    fn find_project(&self, ident: &Ident, containing: bool) -> Option<usize> {
        let projects = || {
            self.workspaces
                .iter()
                .enumerate()
                .filter(|(_, w)| w.info.kind == WorkspaceKind::Project)
        };
        if let Some(wanted) = &ident.common_dir
            && let Some((index, _)) = projects().find(|(_, w)| entry_common_dir(w) == Some(wanted))
        {
            return Some(index);
        }
        let exact = projects().find(|(_, w)| {
            (entry_common_dir(w).is_none() || ident.common_dir.is_none())
                && w.info
                    .roots
                    .first()
                    .is_some_and(|root| projects::same_place(root, &ident.root))
        });
        if let Some((index, _)) = exact {
            return Some(index);
        }
        // A folder outside git belongs to the deepest folder project that contains it, so
        // the folders below a project's folder do not each become a project.
        if !containing || ident.common_dir.is_some() {
            return None;
        }
        projects()
            .filter(|(_, w)| entry_common_dir(w).is_none())
            .filter_map(|(index, w)| {
                let root = w.info.roots.first()?;
                projects::contains(root, &ident.root).then(|| (index, root.components().count()))
            })
            .max_by_key(|(_, depth)| *depth)
            .map(|(index, _)| index)
    }

    /// The project of `ident`, created when there is none (a closed project comes back under the
    /// name it had); whether it was created. A project that was found without a repository
    /// identity learns it now (a folder that became a repository).
    fn project_of(
        &mut self,
        ident: &Ident,
        created_by: CreatedBy,
        containing: bool,
    ) -> Result<(WorkspaceInfo, bool), ErrorBody> {
        if let Some(index) = self.find_project(ident, containing) {
            let known = self.workspaces[index].info.clone();
            let learned = ident.common_dir.clone().filter(|_| {
                known
                    .project
                    .as_ref()
                    .is_some_and(|p| p.common_dir.is_none())
            });
            let workspace = match learned {
                Some(common_dir) => {
                    let workspace = self.update_workspace(known.id, |w| {
                        if let Some(project) = &mut w.project {
                            project.common_dir = Some(common_dir.clone());
                        }
                    })?;
                    log_write(
                        self.storage
                            .workspaces()
                            .set_common_dir(workspace.id, Some(common_dir)),
                        "a project identity",
                    );
                    workspace
                }
                None => known,
            };
            return Ok((workspace, false));
        }
        let name = self
            .recent
            .iter()
            .find(|r| r.path == ident.root)
            .map_or_else(|| ident.name(), |r| r.name.clone());
        let workspace = self.add_project(name, ident, created_by)?;
        Ok((workspace, true))
    }

    /// The workspace a session that starts in a folder goes to.
    fn workspace_for(&mut self, assignment: Assignment) -> Result<WorkspaceId, ErrorBody> {
        match assignment {
            Assignment::Home => Ok(self.home),
            Assignment::Project(ident) => self
                .project_of(&ident, CreatedBy::User, true)
                .map(|(workspace, _)| workspace.id),
        }
    }

    fn open_workspace(
        &mut self,
        assignment: Assignment,
        created_by: Option<CreatedBy>,
    ) -> Result<WorkspaceOpened, ErrorBody> {
        let (workspace, created) = match assignment {
            Assignment::Home => (
                self.workspaces[self.index_of(self.home)?].info.clone(),
                false,
            ),
            Assignment::Project(ident) => {
                self.project_of(&ident, created_by.unwrap_or_default(), true)?
            }
        };
        if !created {
            self.remember_project(&workspace, now_ms());
        }
        Ok(WorkspaceOpened { workspace, created })
    }

    /// The index of workspace `id` for `add_root` and `remove_root`: only a workspace has
    /// folders to add and remove (a project is one folder, Home is bound to its own).
    fn workspace_for_roots(&self, id: WorkspaceId) -> Result<usize, ErrorBody> {
        let index = self.index_of(id)?;
        match self.workspaces[index].info.kind {
            WorkspaceKind::Workspace => Ok(index),
            WorkspaceKind::Home => Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "the Home workspace is bound to its folder; set `home_folder` in daemon.toml",
            )),
            WorkspaceKind::Project => Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "a project is one folder; add folders to a workspace",
            )),
        }
    }

    /// Adds the project of a folder (made when it has none) to a workspace; one that is in
    /// another workspace moves.
    fn add_root(
        &mut self,
        id: WorkspaceId,
        assignment: Assignment,
    ) -> Result<WorkspaceInfo, ErrorBody> {
        self.workspace_for_roots(id)?;
        let Assignment::Project(ident) = assignment else {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "that folder is Home, which is no project",
            ));
        };
        let (project, _) = self.project_of(&ident, CreatedBy::User, false)?;
        self.set_parent(project.id, Some(id))?;
        self.remember_project(&project, now_ms());
        Ok(self.workspaces[self.index_of(id)?].info.clone())
    }

    /// Takes the project whose folder is `path` out of the workspace; it stays a project.
    fn remove_root(&mut self, id: WorkspaceId, path: &Path) -> Result<WorkspaceInfo, ErrorBody> {
        self.workspace_for_roots(id)?;
        let member = self
            .workspaces
            .iter()
            .find(|w| {
                w.info.parent_id == Some(id)
                    && w.info
                        .project
                        .as_ref()
                        .is_some_and(|project| projects::same_place(&project.root, path))
            })
            .map(|w| w.info.id)
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("{} is not a root of workspace {id}", path.display()),
                )
            })?;
        self.set_parent(member, None)?;
        Ok(self.workspaces[self.index_of(id)?].info.clone())
    }

    /// Open projects and closed ones still remembered, newest first. An open project's time is
    /// its latest activity when that is newer than when it was last opened.
    fn recent_projects(&self, limit: Option<u32>) -> RecentProjects {
        let limit = limit
            .unwrap_or(RECENT_PROJECTS_DEFAULT_LIMIT)
            .min(RECENT_PROJECTS_MAX_LIMIT);
        let mut projects: Vec<RecentProject> = Vec::new();
        for workspace in self.workspaces.iter().map(|w| &w.info) {
            let Some(path) = workspace.roots.first() else {
                continue;
            };
            if workspace.kind != WorkspaceKind::Project {
                continue;
            }
            let opened = self
                .recent
                .iter()
                .find(|r| &r.path == path)
                .map_or(0, |r| r.last_opened_ms);
            projects.push(RecentProject {
                path: path.clone(),
                name: workspace.name.clone(),
                last_opened_ms: opened.max(workspace.last_active_ms),
                workspace_id: Some(workspace.id),
            });
        }
        for closed in &self.recent {
            let open = self
                .workspaces
                .iter()
                .any(|w| w.info.roots.contains(&closed.path));
            if !open {
                projects.push(RecentProject {
                    path: closed.path.clone(),
                    name: closed.name.clone(),
                    last_opened_ms: closed.last_opened_ms,
                    workspace_id: None,
                });
            }
        }
        projects.sort_by(|a, b| {
            b.last_opened_ms
                .cmp(&a.last_opened_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        projects.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        RecentProjects { projects }
    }

    /// Moves a workspace before another (or to the end) and renumbers the positions `0..n`;
    /// every workspace whose position changed is announced.
    fn move_workspace(&mut self, req: WorkspaceMove) -> Result<WorkspaceList, ErrorBody> {
        let from = self.index_of(req.workspace_id)?;
        if let Some(before) = req.before {
            self.index_of(before)?;
            if before == req.workspace_id {
                return Ok(self.workspace_list());
            }
        }
        let moved = self.workspaces.remove(from);
        let at = req
            .before
            .and_then(|before| self.workspaces.iter().position(|w| w.info.id == before))
            .unwrap_or(self.workspaces.len());
        self.workspaces.insert(at, moved);

        let mut writes = Vec::new();
        let mut changed = Vec::new();
        for (index, entry) in self.workspaces.iter_mut().enumerate() {
            let position = i64::try_from(index).unwrap_or(i64::MAX);
            if entry.info.position != position {
                entry.info.position = position;
                writes.push(Write::SetWorkspacePosition {
                    id: entry.info.id,
                    position,
                });
                changed.push(entry.info.clone());
            }
        }
        self.next_position = i64::try_from(self.workspaces.len()).unwrap_or(i64::MAX);
        if !writes.is_empty() {
            log_write(self.storage.batch(writes), "the workspace order");
            for workspace in changed {
                self.emit(&Event::WorkspaceUpdated { workspace });
            }
        }
        Ok(self.workspace_list())
    }

    fn pin_workspace(&mut self, req: WorkspacePin) -> Result<WorkspaceInfo, ErrorBody> {
        let workspace = self.update_workspace(req.workspace_id, |w| w.pinned = req.pinned)?;
        log_write(
            self.storage
                .workspaces()
                .set_pinned(workspace.id, workspace.pinned),
            "a workspace pin",
        );
        Ok(workspace)
    }

    // Workspace activity ----------------------------------------------------------------------

    fn activity_interval_ms(&self) -> u64 {
        u64::try_from(self.activity_interval.as_millis()).unwrap_or(u64::MAX)
    }

    /// Input or output in a terminal.
    fn on_activity(&mut self, terminal: TerminalId) {
        let Some(index) = self.terminal_index(terminal) else {
            return;
        };
        let session = &self.sessions[index];
        if session.info.state.is_live() {
            self.note_activity(session.info.workspace_id, now_ms());
        }
    }

    /// Applies the activity now when the workspace was last updated at least one interval ago;
    /// otherwise remembers it for [`flush_activity`](Self::flush_activity), so a busy workspace
    /// is announced once per interval with the newest time.
    fn note_activity(&mut self, workspace: WorkspaceId, at_ms: u64) {
        let interval = self.activity_interval_ms();
        let Some(entry) = self.workspaces.iter_mut().find(|w| w.info.id == workspace) else {
            return;
        };
        let due = entry.info.last_active_ms.saturating_add(interval);
        if due <= at_ms {
            self.apply_activity(workspace, at_ms);
            return;
        }
        entry.activity_pending = true;
        let at = Instant::now() + Duration::from_millis(due - at_ms);
        self.activity_flush_at = Some(self.activity_flush_at.map_or(at, |first| first.min(at)));
    }

    fn apply_activity(&mut self, workspace: WorkspaceId, at_ms: u64) {
        if let Some(entry) = self.workspaces.iter_mut().find(|w| w.info.id == workspace) {
            entry.activity_pending = false;
        }
        // The workspace a project is in is as active as its projects.
        let parent = self.parent_of(workspace);
        for id in std::iter::once(workspace).chain(parent) {
            match self.update_workspace(id, |w| {
                w.last_active_ms = w.last_active_ms.max(at_ms);
            }) {
                Ok(info) => log_write(
                    self.storage
                        .workspaces()
                        .set_last_active(id, info.last_active_ms),
                    "workspace activity",
                ),
                Err(error) => {
                    warn!(workspace = %id, message = %error.message, "could not announce workspace activity");
                }
            }
        }
    }

    fn on_timer(&mut self) {
        let now = Instant::now();
        if self.flush_at.is_some_and(|at| at <= now) {
            self.flush_dirty();
        }
        if self.activity_flush_at.is_some_and(|at| at <= now) {
            self.flush_activity();
        }
        if self.tree_flush_at.is_some_and(|at| at <= now) {
            self.flush_tree_events();
        }
    }

    /// Announces the remembered activity of every workspace whose interval has passed and
    /// schedules the rest.
    fn flush_activity(&mut self) {
        self.activity_flush_at = None;
        let now = now_ms();
        let interval = self.activity_interval_ms();
        let mut ready = Vec::new();
        let mut wait: Option<u64> = None;
        for entry in &self.workspaces {
            if !entry.activity_pending {
                continue;
            }
            let due = entry.info.last_active_ms.saturating_add(interval);
            if due <= now {
                ready.push(entry.info.id);
            } else {
                wait = Some(wait.map_or(due - now, |w| w.min(due - now)));
            }
        }
        // Stamped with the time of the announcement: the activity happened within the last
        // interval, and the next announcement is a full interval away.
        for id in ready {
            self.apply_activity(id, now);
        }
        if let Some(wait) = wait {
            self.activity_flush_at = Some(Instant::now() + Duration::from_millis(wait));
        }
    }

    fn index_of(&self, id: WorkspaceId) -> Result<usize, ErrorBody> {
        self.workspaces
            .iter()
            .position(|w| w.info.id == id)
            .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("workspace {id} not found")))
    }

    // Sessions --------------------------------------------------------------------------------

    fn session_index(&self, id: SessionId) -> Result<usize, ErrorBody> {
        self.sessions
            .iter()
            .position(|s| s.info.id == id)
            .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("session {id} not found")))
    }

    fn terminal_index(&self, terminal: TerminalId) -> Option<usize> {
        self.sessions
            .iter()
            .position(|s| s.terminal.as_ref().is_some_and(|t| t.id() == terminal))
    }

    fn create_session(
        &mut self,
        req: SessionCreate,
        assignment: Option<Assignment>,
    ) -> Result<SessionInfo, ErrorBody> {
        if let Some(id) = req.workspace_id {
            self.index_of(id)?;
        }
        let SessionSpec::Terminal(spec) = req.spec;
        if let Some(size) = spec.size
            && (size.cols == 0 || size.rows == 0)
        {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("invalid terminal size {}x{}", size.cols, size.rows),
            ));
        }
        // An explicit workspace wins; else the project of the folder (Home without one).
        let workspace_id = match req.workspace_id {
            Some(id) => id,
            None => self.workspace_for(assignment.unwrap_or(Assignment::Home))?,
        };

        let default_title = default_title(&spec);
        let chosen = spec
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        let placement = self.placement_of(spec.cwd.as_deref());
        let title = match &chosen {
            Some(chosen) => chosen.clone(),
            None => {
                let facts = Facts {
                    folder: spec.cwd.clone(),
                    branch: placement.as_ref().and_then(|p| p.branch.clone()),
                    osc_title: None,
                    default_title: default_title.clone(),
                    running_command: None,
                };
                self.derive_titles(workspace_id, Some((now_ms(), facts)))
                    .1
                    .pop()
                    .flatten()
                    .unwrap_or_else(|| default_title.clone())
            }
        };
        let info = SessionInfo {
            id: SessionId::new(),
            workspace_id,
            kind: SessionKind::Terminal,
            title,
            state: SessionState::Starting,
            command: spec.command.clone(),
            cwd: spec.cwd.clone(),
            terminal_id: None,
            agent: None,
            created_at_ms: now_ms(),
            restored: false,
            chat: None,
            placement,
            folder: spec.cwd.clone(),
            recovery: None,
            muted: false,
        };
        self.publish(&Event::SessionUpdated {
            session: info.clone(),
        })?;
        let session_id = info.id;
        self.sessions.push(SessionEntry {
            info,
            spec,
            default_title,
            title_locked: chosen.is_some(),
            osc_title: None,
            terminal: None,
            stopping: false,
            stop_waiters: Vec::new(),
            ended_at_ms: None,
            resume_ref_json: None,
            restore_argv: None,
            keep_live: None,
            dirty: false,
            agent_message: None,
            agent_session_id: None,
            tree: TreeState::new(session_id),
            chat: None,
            running_command: None,
            close: None,
        });
        let index = self.sessions.len() - 1;
        // The new name may change what its namesakes are called (a named session counts too).
        self.retitle_workspace(workspace_id, None);
        // A new session is activity; the workspace announcement that follows the session's
        // first update carries the new time together with the new counts.
        self.touch_workspace(workspace_id);
        self.start_terminal(index);
        Ok(self.sessions[index].info.clone())
    }

    /// Records activity now without announcing it.
    fn touch_workspace(&mut self, id: WorkspaceId) {
        let now = now_ms();
        // The workspace a project is in was active too.
        let parent = self.parent_of(id);
        for id in std::iter::once(id).chain(parent) {
            let Some(entry) = self.workspaces.iter_mut().find(|w| w.info.id == id) else {
                continue;
            };
            entry.activity_pending = false;
            entry.info.last_active_ms = entry.info.last_active_ms.max(now);
            let last_active_ms = entry.info.last_active_ms;
            log_write(
                self.storage
                    .workspaces()
                    .set_last_active(id, last_active_ms),
                "workspace activity",
            );
        }
    }

    /// Spawns the process for a `Starting` session and moves it to `Running` or `Failed`.
    fn start_terminal(&mut self, index: usize) {
        let restore_argv = self.sessions[index].restore_argv.take();
        let session = &self.sessions[index];
        let mut env: BTreeMap<String, String> = session.spec.env.clone();
        if let Some(shell) = &self.login_shell {
            env.entry("SHELL".to_owned())
                .or_insert_with(|| shell.clone());
        }
        // Daemon-managed variables win over whatever the client asked for.
        env.insert(
            "HUTAN_SOCKET".to_owned(),
            self.socket.to_string_lossy().into_owned(),
        );
        env.insert("HUTAN_SESSION_ID".to_owned(), session.info.id.to_string());
        env.insert("HUTAN_ENV".to_owned(), "1".to_owned());
        env.insert("HUTAN_BIN".to_owned(), self.hutan_bin.clone());
        env.insert(
            "HUTAN_WORKSPACE_ID".to_owned(),
            session.info.workspace_id.to_string(),
        );
        let mut config = TerminalConfig {
            argv: restore_argv.unwrap_or_else(|| session.spec.command.clone()),
            cwd: session.info.cwd.clone(),
            env,
            env_inherit: self.env_inherit,
            shell_integration_dir: self.shell_integration_dir.clone(),
            ..TerminalConfig::default()
        };
        if let Some(size) = session.spec.size {
            config.size = size;
        }

        match Terminal::spawn(config) {
            Ok(terminal) => {
                // Subscribed immediately so output the child produces right away is not lost.
                let meta = terminal.metadata();
                let entry = self.hub.register(terminal.clone());
                tokio::spawn(watch_terminal(
                    entry.id(),
                    terminal,
                    meta,
                    self.mailbox.clone(),
                ));
                let session = &mut self.sessions[index];
                session.info.terminal_id = Some(entry.id());
                session.terminal = Some(entry);
                session.advance(SessionEvent::Spawned);
            }
            Err(error) => {
                warn!(session = %session.info.id, %error, "terminal spawn failed");
                self.sessions[index].advance(SessionEvent::Failed {
                    reason: error.to_string(),
                });
                self.recovery_failed(index, error.to_string());
            }
        }
        self.emit_session(index);
        self.persist_session(index);
    }

    fn stop_session(&mut self, id: SessionId, reply: Reply<SessionInfo>) {
        let index = match self.session_index(id) {
            Ok(index) => index,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let session = &mut self.sessions[index];
        if !session.info.state.is_live() {
            let _ = reply.send(Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("session {id} is not running"),
            )));
            return;
        }
        session.request_stop(&self.chats);
        session.stop_waiters.push(reply);
        self.clear_recovery(index);
        self.end_if_asleep(index);
    }

    fn restart_session(&mut self, id: SessionId) -> Result<SessionInfo, ErrorBody> {
        let index = self.session_index(id)?;
        let session = &mut self.sessions[index];
        if session.info.state.is_live() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("session {id} is still running; stop it first"),
            ));
        }
        session.info.recovery = None;
        if session.chat.is_some() {
            return Ok(self.restart_chat(index));
        }
        session.advance(SessionEvent::Restart);
        if let Some(old) = session.terminal.take() {
            // Attached clients are dropped; they re-attach to the new terminal id.
            self.hub.remove(old.id());
        }
        session.stopping = false;
        session.keep_live = None;
        session.info.restored = false;
        // A new process is a new reporting epoch: nothing the old one said applies.
        session.forget_agent(now_ms());
        session.tree.reset_seqs();
        session.osc_title = None;
        session.info.terminal_id = None;
        // Restart where the program last said it was (OSC 7) when that directory still exists.
        session.info.cwd = session
            .info
            .cwd
            .take()
            .filter(|cwd| cwd.is_dir())
            .or_else(|| session.spec.cwd.clone());
        // The title the old process set is gone; `emit_session` names the session again.
        self.emit_session(index);
        self.start_terminal(index);
        Ok(self.sessions[index].info.clone())
    }

    // Closing and reopening ---------------------------------------------------------------------

    /// What runs in the session that closing it would cut off, if anything: a command in the
    /// foreground of a terminal (OSC 133, see `note_running_command`), an agent reporting
    /// `working` or `blocked`, a chat turn, a pending approval or question.
    fn busy_of(&self, index: usize) -> Option<SessionBusy> {
        let session = &self.sessions[index];
        if let (Some(info), Some(spec)) = (&session.info.chat, &session.chat) {
            let state = if info.pending_approvals > 0 || info.pending_questions > 0 {
                "approval"
            } else if info.turn_active {
                "turn"
            } else {
                return None;
            };
            return Some(SessionBusy {
                command: None,
                agent: Some(spec.agent.clone()),
                state: state.to_owned(),
            });
        }
        let command = session.running_command.clone();
        let agent = session
            .info
            .agent
            .as_ref()
            .filter(|agent| agent.state != AgentState::Idle);
        if command.is_none() && agent.is_none() {
            return None;
        }
        let state = match agent.map(|agent| agent.state) {
            Some(AgentState::Blocked) => "blocked",
            _ => "working",
        };
        Some(SessionBusy {
            command,
            agent: agent.map(|agent| agent.agent.clone()),
            state: state.to_owned(),
        })
    }

    /// `session.close`. A session that is not live is closed at once; an idle live one is
    /// stopped (as `session.stop`) and closed when its process has ended; a busy one is
    /// refused (`invalid_state`, `data.busy`) unless `stop_running`.
    fn close_session(&mut self, req: SessionClose, reply: Reply<()>) {
        let index = match self.session_index(req.session_id) {
            Ok(index) => index,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        if let Some(close) = &mut self.sessions[index].close {
            // Already on its way out: this caller waits for the same end.
            close.waiters.push(reply);
            return;
        }
        let live = self.sessions[index].info.state.is_live();
        if live
            && !req.stop_running
            && let Some(busy) = self.busy_of(index)
        {
            let _ = reply.send(Err(busy_error(&self.sessions[index].info.title, &busy)));
            return;
        }
        let session = &mut self.sessions[index];
        session.close = Some(CloseRequest {
            waiters: vec![reply],
            resume_ref_json: session.resume_ref_json.clone(),
        });
        if !live {
            self.complete_close(index);
            return;
        }
        session.request_stop(&self.chats);
        self.clear_recovery(index);
        self.end_if_asleep(index);
    }

    /// The session has ended and `session.close` asked for it to go: its row stays (marked
    /// closed) with everything that belongs to it, the registry forgets it, and clients and
    /// the attention state hear of it.
    fn complete_close(&mut self, index: usize) {
        if self.sessions[index].tree.take_dirty() {
            self.persist_tree(index);
        }
        let mut row = self.session_row(index);
        let session = &mut self.sessions[index];
        let close = session.close.take();
        // The exit forgot the agent's resume reference; the closed row keeps the one the
        // session had, so that reopening resumes the agent.
        if let Some(close) = &close {
            row.resume_ref_json.clone_from(&close.resume_ref_json);
        }
        row.state = session.info.state.clone();
        row.ended_at_ms = session.ended_at_ms;
        let id = session.info.id;
        let closed_at_ms = now_ms();
        log_write(
            self.storage.sessions().upsert(row.clone()),
            "a closed session",
        );
        log_write(
            self.storage.sessions().set_closed(id, Some(closed_at_ms)),
            "closing a session",
        );
        let entry = self.sessions.remove(index);
        if let Some(terminal) = &entry.terminal {
            self.hub.remove(terminal.id());
        }
        self.closed.insert(
            0,
            ClosedSession {
                session: row,
                closed_at_ms,
            },
        );
        // The recorded foreground commands no longer include this session's.
        self.commands_dirty = true;
        self.flush_at
            .get_or_insert_with(|| Instant::now() + DIRTY_WRITE_INTERVAL);
        self.emit(&Event::SessionClosed { session_id: id });
        self.retitle_workspace(entry.info.workspace_id, None);
        self.sync_workspace(entry.info.workspace_id);
        // A checkout it shared is not shared any more.
        for other in self.sync_shared() {
            self.emit(&Event::SessionUpdated {
                session: self.sessions[other].info.clone(),
            });
        }
        self.schedule_tree_events();
        for waiter in close.into_iter().flat_map(|close| close.waiters) {
            let _ = waiter.send(Ok(()));
        }
    }

    /// `session.reopen`: a closed session comes back as a restart recovers one. A terminal
    /// spawns its saved command (or its agent's resume command) in the last known directory,
    /// a chat resumes its conversation, an assistant comes back asleep; same id, title and
    /// workspace.
    fn reopen_session(&mut self, id: SessionId) -> Result<SessionInfo, ErrorBody> {
        if self.sessions.iter().any(|s| s.info.id == id) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("session {id} is not closed"),
            ));
        }
        let position = self
            .closed
            .iter()
            .position(|closed| closed.session.id == id)
            .ok_or_else(|| {
                ErrorBody::new(ErrorCode::NotFound, format!("session {id} not found"))
            })?;
        let mut row = self.closed[position].session.clone();
        // Brought back like a session that was live when the daemon stopped.
        row.state = SessionState::Running;
        let context = recovery::Context::new(None, now_ms());
        let facts = recovery::Facts::default();
        let mut pending = Restoring::default();
        let before = self.sessions.len();
        self.restore_row(row, RestoreMode::Respawn, &context, &facts, &mut pending);
        if self.sessions.len() == before {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("the workspace of session {id} is gone"),
            ));
        }
        self.closed.remove(position);
        log_write(
            self.storage.sessions().set_closed(id, None),
            "reopening a session",
        );
        let index = self.sessions.len() - 1;
        if self.muted.contains(&id) {
            self.sessions[index].info.muted = true;
        }
        self.touch_workspace(self.sessions[index].info.workspace_id);
        // Clients know the session before it starts; `start_restored` announces the rest.
        self.emit_session(index);
        self.start_restored(pending);
        Ok(self.sessions[index].info.clone())
    }

    // Recovery --------------------------------------------------------------------------------

    /// Remembers the command a terminal runs in the foreground, until it finishes: what the
    /// next start can say was running when the daemon ended. A stop in progress freezes it
    /// (the hang-up makes the shell prompt again, which is not the command finishing).
    fn note_running_command(&mut self, index: usize, block: &hutan_protocol::blocks::Block) {
        use hutan_protocol::blocks::BlockState;
        let session = &mut self.sessions[index];
        if session.stopping || session.keep_live.is_some() {
            return;
        }
        let command = match block.state {
            BlockState::Running => block.command.as_deref().and_then(recovery::bounded_command),
            BlockState::Finished | BlockState::Prompt => None,
            BlockState::Truncated => return,
        };
        if session.running_command != command {
            session.running_command = command;
            self.commands_dirty = true;
            self.flush_at
                .get_or_insert_with(|| Instant::now() + DIRTY_WRITE_INTERVAL);
            if self.retitle(index) {
                self.emit_session(index);
                self.mark_dirty(index);
            }
        }
    }

    /// Tells the clients what changed in the session's `recovery` (it is not stored).
    fn announce_recovery(&mut self, index: usize) {
        let session = self.sessions[index].info.clone();
        self.emit(&Event::SessionUpdated { session });
    }

    /// What a restart did to the session no longer needs to be said: the user dismissed it or
    /// acted in the session. Returns whether there was something.
    fn clear_recovery(&mut self, index: usize) -> bool {
        if self.sessions[index].info.recovery.take().is_none() {
            return false;
        }
        self.announce_recovery(index);
        true
    }

    /// `session.recovery_ack`.
    fn ack_recovery(&mut self, req: RecoveryAck) -> Result<RecoveryAcked, ErrorBody> {
        if let Some(id) = req.session_id {
            self.session_index(id)?;
        }
        let mut sessions = Vec::new();
        for index in 0..self.sessions.len() {
            if req
                .session_id
                .is_some_and(|id| id != self.sessions[index].info.id)
            {
                continue;
            }
            if self.clear_recovery(index) {
                sessions.push(self.sessions[index].info.id);
            }
        }
        Ok(RecoveryAcked { sessions })
    }

    /// Adds what a restart cost the session; only a session that has a recovery has one.
    fn add_recovery_loss(&mut self, index: usize, lost: Lost) {
        let Some(recovery) = &mut self.sessions[index].info.recovery else {
            return;
        };
        if recovery.lost.contains(&lost) {
            return;
        }
        recovery.lost.push(lost);
        self.announce_recovery(index);
    }

    /// The session came back as asked but then did not stay: a recovery that says it runs
    /// is replaced by what happened (`reason`).
    fn recovery_failed(&mut self, index: usize, reason: String) {
        let Some(recovery) = &mut self.sessions[index].info.recovery else {
            return;
        };
        if matches!(
            recovery.kind,
            RecoveryKind::ShellRespawned { .. }
                | RecoveryKind::AgentResumed { .. }
                | RecoveryKind::ChatResumed { .. }
        ) {
            recovery.kind = RecoveryKind::NotRestarted { reason };
        }
    }

    fn rename_session(&mut self, req: SessionRename) -> Result<SessionInfo, ErrorBody> {
        let title = validated_name(req.title, "session")?;
        let index = self.session_index(req.session_id)?;
        let mut info = self.sessions[index].info.clone();
        info.title = title;
        self.publish(&Event::SessionUpdated {
            session: info.clone(),
        })?;
        let session = &mut self.sessions[index];
        session.info = info.clone();
        session.title_locked = true;
        self.persist_session(index);
        self.retitle_workspace(info.workspace_id, None);
        Ok(info)
    }

    /// `notify.mute`.
    fn set_muted(&mut self, req: NotifyMute) -> Result<SessionInfo, ErrorBody> {
        let index = self.session_index(req.session_id)?;
        if self.sessions[index].info.muted == req.muted {
            return Ok(self.sessions[index].info.clone());
        }
        let mut info = self.sessions[index].info.clone();
        info.muted = req.muted;
        self.publish(&Event::SessionUpdated {
            session: info.clone(),
        })?;
        self.sessions[index].info = info.clone();
        if req.muted {
            self.muted.insert(info.id);
        } else {
            self.muted.remove(&info.id);
        }
        self.store_mutes();
        Ok(info)
    }

    /// Writes [`Self::muted`], without the sessions that are gone for good.
    fn store_mutes(&mut self) {
        let known: HashSet<SessionId> = self
            .sessions
            .iter()
            .map(|s| s.info.id)
            .chain(self.closed.iter().map(|c| c.session.id))
            .collect();
        self.muted.retain(|id| known.contains(id));
        let mut muted: Vec<SessionId> = self.muted.iter().copied().collect();
        muted.sort();
        match serde_json::to_string(&muted) {
            Ok(json) => log_write(
                self.storage.app_meta().set(MUTED_KEY, &json),
                "the muted sessions",
            ),
            Err(error) => warn!(%error, "could not encode the muted sessions"),
        }
    }

    /// Reads what [`store_mutes`](Self::store_mutes) wrote and marks those sessions muted.
    fn restore_mutes(&mut self) {
        let json = match self.storage.app_meta().get(MUTED_KEY) {
            Ok(Some(json)) => json,
            Ok(None) => return,
            Err(error) => {
                warn!(%error, "could not read the muted sessions");
                return;
            }
        };
        self.muted = match serde_json::from_str(&json) {
            Ok(muted) => muted,
            Err(error) => {
                warn!(%error, "the muted sessions are unreadable; none is muted");
                return;
            }
        };
        for session in &mut self.sessions {
            session.info.muted = self.muted.contains(&session.info.id);
        }
    }

    // Agent reports ---------------------------------------------------------------------------

    fn report_agent(&mut self, req: AgentReportRequest) -> Result<AgentApplied, ErrorBody> {
        let index = self.session_index(req.session_id)?;
        agent_field("source", &req.source)?;
        agent_field("agent", &req.agent)?;
        if let Some(id) = &req.agent_session_id {
            agent_field("agent_session_id", id)?;
        }
        if let Some(node) = &req.node {
            check_node_report(node)?;
        }
        let resume = match &req.resume_argv {
            Some(argv) => {
                agent::validate_resume_argv(argv)
                    .map_err(|message| ErrorBody::new(ErrorCode::BadRequest, message))?;
                let json = agent::resume_ref_json(
                    &req.source,
                    &req.agent,
                    req.agent_session_id.as_deref(),
                    argv.clone(),
                )
                .map_err(|error| {
                    error!(%error, "could not encode a resume reference");
                    ErrorBody::new(ErrorCode::Internal, "could not encode the resume reference")
                })?;
                Some(json)
            }
            None => None,
        };

        let session = &mut self.sessions[index];
        let node_id = req.node.as_ref().map(|node| node.id.clone());
        // A hook can fire just as the process ends; there is nothing left to describe.
        if !session.info.state.is_live()
            || !session
                .tree
                .accept_seq(&req.source, node_id.as_deref(), req.seq)
        {
            return Ok(AgentApplied { applied: false });
        }
        let message = req
            .message
            .map(|message| clipped(&message, MAX_AGENT_MESSAGE_CHARS))
            .filter(|message| !message.is_empty());
        let same_agent = session
            .tree
            .tree()
            .root()
            .is_some_and(|root| root.agent == req.agent);
        let now = now_ms();
        let mut update = node_update(
            &req.source,
            &req.agent,
            req.state,
            req.node,
            message.as_deref(),
        );
        // A pid below 2 is no process of an agent (0, init) and above `i32::MAX` is no pid.
        update.pid = req.pid.filter(|pid| (2..=i32::MAX as u32).contains(pid));
        session.tree.apply(update, now);
        // The message, conversation id and resume command belong to the main agent only.
        let about_root = node_id
            .as_deref()
            .is_none_or(|id| session.tree.tree().root_id() == Some(id));
        let mut resume_changed = false;
        if about_root {
            // The conversation id sticks to the agent until it says otherwise.
            session.agent_session_id = req
                .agent_session_id
                .or_else(|| session.agent_session_id.take().filter(|_| same_agent));
            session.agent_message = message;
            resume_changed = resume.is_some() && session.resume_ref_json != resume;
            if resume_changed {
                session.resume_ref_json = resume;
            }
        }
        self.tree_changed(index);
        if resume_changed {
            self.persist_session(index);
        }
        Ok(AgentApplied { applied: true })
    }

    fn release_agent(&mut self, req: AgentReleaseRequest) -> Result<AgentApplied, ErrorBody> {
        let index = self.session_index(req.session_id)?;
        agent_field("source", &req.source)?;
        if let Some(node) = &req.node {
            agent_field("node", node)?;
        }
        let session = &mut self.sessions[index];
        if !session
            .tree
            .accept_seq(&req.source, req.node.as_deref(), req.seq)
        {
            return Ok(AgentApplied { applied: false });
        }
        let about_root = req
            .node
            .as_deref()
            .is_none_or(|id| session.tree.tree().root_id() == Some(id));
        let outcome = req.outcome.unwrap_or(NodeOutcome::Completed);
        let node_released =
            session
                .tree
                .release(&req.source, req.node.as_deref(), outcome, now_ms());
        let mut resume_released = false;
        if about_root {
            resume_released = session
                .resume_ref_json
                .as_deref()
                .and_then(restore::resume_source)
                .is_some_and(|source| source == req.source);
            if resume_released {
                session.resume_ref_json = None;
            }
            if node_released {
                session.agent_message = None;
                session.agent_session_id = None;
            }
        }
        self.tree_changed(index);
        if resume_released {
            self.persist_session(index);
        }
        Ok(AgentApplied {
            applied: node_released || resume_released,
        })
    }

    fn stop_all(&mut self, forget: bool, reply: Reply<()>) {
        let mut asleep = Vec::new();
        for session in self.sessions.iter_mut().filter(|s| s.info.state.is_live()) {
            // A session the user is already stopping stays stopped; the others die with the
            // daemon but remain in the restore set unless the user asked to forget them.
            if !session.stopping && !forget {
                session.keep_live = Some(session.info.state.clone());
            }
            session.request_stop(&self.chats);
            asleep.push(session.info.id);
        }
        // Nothing to stop in a sleeping assistant: it ends here.
        for id in asleep {
            if let Ok(index) = self.session_index(id) {
                self.end_if_asleep(index);
            }
        }
        self.flush_dirty();
        self.stop_all_waiters.push(reply);
        self.settle_stop_all();
    }

    /// A chat session whose agent was stopped for being idle has no process to report its end:
    /// a stop or kill of it completes here.
    fn end_if_asleep(&mut self, index: usize) {
        let session = &self.sessions[index];
        let id = session.info.id;
        if session.chat.is_none() || self.chats.is_running(id) || !self.chats.is_asleep(id) {
            return;
        }
        self.chats.forget_sleeper(id);
        self.on_chat_exited(id, &ExitInfo::default());
    }

    fn settle_stop_all(&mut self) {
        if self.live_count() == 0 {
            for waiter in self.stop_all_waiters.drain(..) {
                let _ = waiter.send(Ok(()));
            }
        }
    }

    // Terminal notifications ------------------------------------------------------------------

    fn on_meta(&mut self, terminal: TerminalId, meta: TerminalMeta) {
        // Stale notifications of a replaced terminal find no session and are dropped.
        let Some(index) = self.terminal_index(terminal) else {
            return;
        };
        match meta {
            TerminalMeta::Title(title) => {
                let title = (!title.is_empty()).then_some(title);
                let session = &mut self.sessions[index];
                if session.osc_title == title {
                    return;
                }
                session.osc_title.clone_from(&title);
                self.emit(&Event::TerminalTitle {
                    terminal_id: terminal,
                    title,
                });
                if self.retitle(index) {
                    self.emit_session(index);
                    self.mark_dirty(index);
                }
            }
            TerminalMeta::Bell => self.emit(&Event::TerminalBell {
                terminal_id: terminal,
            }),
            TerminalMeta::Notification { title, body } => self.emit(&Event::TerminalNotification {
                terminal_id: terminal,
                title: (!title.is_empty()).then_some(title),
                body,
            }),
            TerminalMeta::Cwd(cwd) => {
                let session = &mut self.sessions[index];
                if session.info.cwd.as_deref() != Some(cwd.as_path()) {
                    session.info.folder = Some(cwd.clone());
                    session.info.cwd = Some(cwd);
                    self.emit_session(index);
                    self.mark_dirty(index);
                }
            }
            TerminalMeta::PromptMark(PromptMark::A) => {
                // The shell is prompting again: whatever agent was running in the foreground is
                // gone, even if it never got to say so.
                let resumable = self.sessions[index].forget_agent(now_ms());
                self.tree_changed(index);
                if resumable {
                    self.persist_session(index);
                }
            }
            TerminalMeta::Block(block) => {
                self.note_running_command(index, &block);
                self.emit(&Event::TerminalBlock {
                    terminal_id: terminal,
                    block,
                });
            }
            TerminalMeta::PromptMark(_) => {}
        }
    }

    fn on_exited(&mut self, terminal: TerminalId, status: &ExitStatus) {
        let Some(index) = self.terminal_index(terminal) else {
            return;
        };
        self.hub.mark_ended(terminal);
        let code = status.code;
        let signal = status.signal;
        let session = &mut self.sessions[index];
        let on_its_own = !session.stopping && session.keep_live.is_none();
        session.advance(if session.stopping {
            SessionEvent::StopCompleted
        } else {
            SessionEvent::ProcessExited { code, signal }
        });
        if on_its_own {
            // Kept while the daemon shuts down: that exit is ours; the program still has no
            // command running that the next start should remember either way.
            session.running_command = None;
            self.commands_dirty = true;
            self.recovery_failed(index, terminal_exit_reason(code, signal));
        }
        let session = &mut self.sessions[index];
        // The agent died with the process. A daemon shutdown keeps the resume reference and
        // the tree as it was: the next daemon starts the agent again and marks the tree
        // restored.
        let keep = session.keep_live.is_some();
        let resume = session.resume_ref_json.take().filter(|_| keep);
        if keep {
            session.agent_message = None;
            session.agent_session_id = None;
        } else {
            session.forget_agent(now_ms());
        }
        session.resume_ref_json = resume;
        session.stopping = false;
        self.sync_agent_summary(index);
        // The waiters and the event below must not show it still sharing a checkout.
        self.refresh_placement(index);
        let session = &mut self.sessions[index];
        let info = session.info.clone();
        let waiters = std::mem::take(&mut session.stop_waiters);
        self.emit_session(index);
        self.persist_session(index);
        self.tree_changed(index);
        self.emit(&Event::SessionExited {
            session_id: info.id,
            code,
            signal,
        });
        for waiter in waiters {
            let _ = waiter.send(Ok(info.clone()));
        }
        self.settle_stop_all();
        if self.sessions[index].close.is_some() {
            self.complete_close(index);
        }
    }

    // Agent trees -----------------------------------------------------------------------------

    /// The trees of one session (unknown id: `not_found`) or of every session that has one.
    /// Finished subtrees past their time are dropped first.
    fn agent_tree(&mut self, session: Option<SessionId>) -> Result<AgentTree, ErrorBody> {
        let indices: Vec<usize> = match session {
            Some(id) => vec![self.session_index(id)?],
            None => (0..self.sessions.len()).collect(),
        };
        let now = now_ms();
        let mut observations = Vec::new();
        for index in indices {
            self.sessions[index].tree.prune(now);
            if self.sessions[index].tree.event_due().is_some() {
                self.tree_changed(index);
            }
            if let Some(observation) = self.sessions[index].tree.tree().observation() {
                observations.push(observation);
            }
        }
        Ok(AgentTree { observations })
    }

    // Resource manager --------------------------------------------------------------------------

    /// The live sessions that have a process, with the agents of their trees that have not
    /// finished (`resources.*`): of one session (unknown id: `not_found`; a session that is not
    /// running yields nothing) or of all, in creation order.
    fn process_targets(&self, session: Option<SessionId>) -> Result<Vec<ProcessTarget>, ErrorBody> {
        let indices: Vec<usize> = match session {
            Some(id) => vec![self.session_index(id)?],
            None => (0..self.sessions.len()).collect(),
        };
        let mut targets = Vec::new();
        for index in indices {
            let session = &self.sessions[index];
            if !session.info.state.is_live() {
                continue;
            }
            let root_pid = match &session.terminal {
                Some(entry) => entry.terminal().pid(),
                None => self.chats.harness_pid(session.info.id),
            };
            let tree = session.tree.tree();
            targets.push(ProcessTarget {
                session_id: session.info.id,
                terminal: session.info.terminal_id,
                root_pid,
                chat: session.chat.is_some(),
                root_node: tree.root_id().map(str::to_owned),
                agents: tree
                    .nodes()
                    .filter(|node| !node.is_finished())
                    .cloned()
                    .collect(),
            });
        }
        Ok(targets)
    }

    /// SIGKILL now (`session.kill`); answered once the session has ended.
    fn kill_session(&mut self, id: SessionId, reply: Reply<SessionInfo>) {
        let index = match self.session_index(id) {
            Ok(index) => index,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let session = &mut self.sessions[index];
        if !session.info.state.is_live() {
            let _ = reply.send(Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("session {id} is not running"),
            )));
            return;
        }
        session.request_kill(&self.chats);
        session.stop_waiters.push(reply);
        self.end_if_asleep(index);
    }

    // Chat sessions ---------------------------------------------------------------------------

    fn chat_target(&self, id: SessionId) -> Result<bool, ErrorBody> {
        let session = &self.sessions[self.session_index(id)?];
        if session.chat.is_none() {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("session {id} is not a chat session"),
            ));
        }
        Ok(session.info.state.is_live())
    }

    fn create_chat(
        &mut self,
        req: ChatCreate,
        assignment: Option<Assignment>,
        assistant: bool,
        asleep: bool,
        reply: Reply<SessionInfo>,
    ) {
        match self.begin_chat(req, assignment, assistant) {
            Ok(index) if asleep => {
                let session = &mut self.sessions[index];
                session.advance(SessionEvent::Ready);
                let id = session.info.id;
                self.sync_chat_state(index);
                // Asleep before anyone can see it: the session is never neither running nor
                // asleep, and the first message starts the agent.
                self.chats.mark_asleep(id);
                self.emit_session(index);
                self.persist_session(index);
                let _ = reply.send(Ok(self.sessions[index].info.clone()));
            }
            Ok(index) => self.spawn_chat_start(index, Some(reply), false),
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    /// Adds the session (state `starting`) and stores it; the agent is started by the caller.
    fn begin_chat(
        &mut self,
        req: ChatCreate,
        assignment: Option<Assignment>,
        assistant: bool,
    ) -> Result<usize, ErrorBody> {
        let harness = is_harness_agent(&req.agent);
        if req.agent != AGENT_CODEX && !harness && !is_gateway_agent(&req.agent) {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "unsupported agent `{}` (supported: {AGENT_CODEX}, omp, claude, {AGENT_DIRECT}, {AGENT_ASSISTANT})",
                    req.agent
                ),
            ));
        }
        if req.mode.is_some() && !harness {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "`mode` is a session mode of a harness chat (omp, claude)",
            ));
        }
        if harness && (req.approval_policy.is_some() || req.sandbox.is_some()) {
            // Not faked: the harness decides what asks and what is sandboxed.
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "the approval policy and sandbox of a {} chat belong to the harness's own settings; \
                     Hutan answers the requests it makes",
                    req.agent
                ),
            ));
        }
        let direct = if is_gateway_agent(&req.agent) {
            Some(self.direct_target(&req)?)
        } else {
            None
        };
        let workspace_id = match req.workspace_id {
            Some(id) => {
                self.index_of(id)?;
                id
            }
            None => self.workspace_for(assignment.unwrap_or(Assignment::Home))?,
        };
        let spec = match direct {
            Some((provider_id, model, system_prompt)) => ChatSpec {
                agent: req.agent,
                model: Some(model),
                approval_policy: Default::default(),
                sandbox: Default::default(),
                thread_id: None,
                provider_id: Some(provider_id),
                system_prompt,
                mode: None,
                assistant: false,
            },
            None => ChatSpec {
                agent: req.agent,
                model: req.model.filter(|m| !m.trim().is_empty()),
                approval_policy: req.approval_policy.unwrap_or_default(),
                sandbox: req.sandbox.unwrap_or_default(),
                thread_id: None,
                provider_id: None,
                system_prompt: None,
                mode: req.mode.filter(|m| !m.trim().is_empty()),
                assistant,
            },
        };
        let folder = req
            .cwd
            .file_name()
            .map_or_else(|| "chat".to_owned(), |n| n.to_string_lossy().into_owned());
        let default_title = match (&spec.provider_id, &spec.model) {
            (Some(provider), Some(model)) => format!("{provider} · {model}"),
            _ => format!("{} · {folder}", spec.agent),
        };
        let chosen = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        let info = SessionInfo {
            id: SessionId::new(),
            workspace_id,
            kind: SessionKind::Harness,
            title: chosen.clone().unwrap_or_else(|| default_title.clone()),
            state: SessionState::Starting,
            command: vec![spec.agent.clone()],
            cwd: Some(req.cwd.clone()),
            terminal_id: None,
            agent: None,
            created_at_ms: now_ms(),
            restored: false,
            chat: Some(chat_info(&spec)),
            placement: self.placement_of(Some(&req.cwd)),
            folder: Some(req.cwd.clone()),
            recovery: None,
            muted: false,
        };
        self.publish(&Event::SessionUpdated {
            session: info.clone(),
        })?;
        let session_id = info.id;
        self.sessions.push(SessionEntry {
            info,
            spec: TerminalSpec {
                command: vec![spec.agent.clone()],
                cwd: Some(req.cwd),
                ..TerminalSpec::default()
            },
            default_title,
            title_locked: chosen.is_some(),
            osc_title: None,
            terminal: None,
            stopping: false,
            stop_waiters: Vec::new(),
            ended_at_ms: None,
            resume_ref_json: None,
            restore_argv: None,
            keep_live: None,
            dirty: false,
            agent_message: None,
            agent_session_id: None,
            tree: TreeState::new(session_id),
            chat: Some(spec),
            running_command: None,
            close: None,
        });
        let index = self.sessions.len() - 1;
        let workspace = self.sessions[index].info.workspace_id;
        self.touch_workspace(workspace);
        // Stored before the agent runs: the journal's rows refer to this one.
        self.persist_session(index);
        self.sync_workspace(workspace);
        Ok(index)
    }

    /// What a direct chat talks to: its provider, the model (the request's, else the provider's
    /// default) and the system prompt.
    fn direct_target(
        &self,
        req: &ChatCreate,
    ) -> Result<(String, String, Option<String>), ErrorBody> {
        let bad = |message: String| ErrorBody::new(ErrorCode::BadRequest, message);
        let Some(provider_id) = req.provider_id.as_deref().filter(|p| !p.is_empty()) else {
            return Err(bad(
                "a direct chat needs a provider (`provider_id`, see `hutan provider ls`)".into(),
            ));
        };
        let provider = self.chats.providers().require(provider_id)?;
        let model = req
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
            .or(provider.default_model)
            .ok_or_else(|| {
                bad(format!(
                    "no model: name one, or give provider `{provider_id}` a default model"
                ))
            })?;
        let system_prompt = req
            .system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_owned);
        if system_prompt
            .as_ref()
            .is_some_and(|p| p.len() > MAX_SYSTEM_PROMPT_BYTES)
        {
            return Err(bad(format!(
                "the system prompt is longer than {MAX_SYSTEM_PROMPT_BYTES} bytes"
            )));
        }
        Ok((provider.id, model, system_prompt))
    }

    /// `chat.set_model`: the session's spec and info change now; the running chat is told, and
    /// the reply waits until its journal has the notice.
    fn set_chat_model(&mut self, req: ChatSetModel, reply: Reply<SessionInfo>) {
        let harness = self
            .session_index(req.session_id)
            .ok()
            .and_then(|index| self.sessions[index].chat.as_ref())
            .is_some_and(|spec| is_harness_agent(&spec.agent));
        if harness {
            self.switch_harness_model(req, reply);
            return;
        }
        match self.switch_chat_model(req) {
            Ok((index, switch)) => {
                let info = self.sessions[index].info.clone();
                let chats = self.chats.clone();
                let id = info.id;
                tokio::spawn(async move {
                    let result = match switch {
                        Some((provider_id, model)) => {
                            chats.set_model(id, provider_id, model).await.map(|()| info)
                        }
                        None => Ok(info),
                    };
                    let _ = reply.send(result);
                });
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    /// `chat.set_model` of a harness chat: the agent has to accept the model, so the session's
    /// record changes only when it did (`on_chat_model_switched`).
    fn switch_harness_model(&mut self, req: ChatSetModel, reply: Reply<SessionInfo>) {
        let checked = (|| {
            let index = self.session_index(req.session_id)?;
            let session = &self.sessions[index];
            let startable =
                self.chats.is_running(req.session_id) || self.chats.is_asleep(req.session_id);
            if !session.info.state.is_live() || !startable {
                return Err(ErrorBody::new(
                    ErrorCode::InvalidState,
                    format!(
                        "chat session {} is not running (`hutan restart` it first)",
                        req.session_id
                    ),
                ));
            }
            let model = req.model.trim();
            if model.is_empty() {
                return Err(ErrorBody::new(ErrorCode::BadRequest, "the model is empty"));
            }
            let unchanged = session
                .chat
                .as_ref()
                .is_some_and(|spec| spec.model.as_deref() == Some(model));
            Ok((index, model.to_owned(), unchanged))
        })();
        let (index, model, unchanged) = match checked {
            Ok(checked) => checked,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        if unchanged {
            let _ = reply.send(Ok(self.sessions[index].info.clone()));
            return;
        }
        let id = req.session_id;
        let chats = self.chats.clone();
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move {
            let outcome = chats.set_model(id, String::new(), model.clone()).await;
            let command = Command::ChatModelSwitched {
                session: id,
                model,
                outcome,
                reply,
            };
            if let Some(tx) = mailbox.upgrade() {
                // A closed mailbox means the daemon is going down; the reply is dropped.
                let _ = tx.send(command).await;
            }
        });
    }

    fn on_chat_model_switched(
        &mut self,
        id: SessionId,
        model: String,
        outcome: Result<(), ErrorBody>,
        reply: Reply<SessionInfo>,
    ) {
        let result = match (outcome, self.session_index(id)) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(index)) => {
                let session = &mut self.sessions[index];
                if let Some(spec) = &mut session.chat {
                    spec.model = Some(model.clone());
                }
                if let Some(chat) = &mut session.info.chat {
                    chat.model = model;
                }
                self.emit_session(index);
                self.persist_session(index);
                Ok(self.sessions[index].info.clone())
            }
        };
        let _ = reply.send(result);
    }

    /// Validates and applies the switch; `None` means nothing changed.
    #[allow(clippy::type_complexity)]
    fn switch_chat_model(
        &mut self,
        req: ChatSetModel,
    ) -> Result<(usize, Option<(String, String)>), ErrorBody> {
        let index = self.session_index(req.session_id)?;
        let session = &self.sessions[index];
        let Some(spec) = &session.chat else {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("session {} is not a chat session", req.session_id),
            ));
        };
        if !is_gateway_agent(&spec.agent) {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "only a direct or harness chat has a switchable model",
            ));
        }
        if !session.info.state.is_live() || !self.chats.is_running(req.session_id) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!(
                    "chat session {} is not running (`hutan restart` it first)",
                    req.session_id
                ),
            ));
        }
        let model = req.model.trim();
        if model.is_empty() {
            return Err(ErrorBody::new(ErrorCode::BadRequest, "the model is empty"));
        }
        let current_provider = spec.provider_id.clone().unwrap_or_default();
        let provider_id = match req.provider_id.as_deref().filter(|p| !p.is_empty()) {
            Some(provider) => self.chats.providers().require(provider)?.id,
            None => current_provider.clone(),
        };
        if provider_id == current_provider && spec.model.as_deref() == Some(model) {
            return Ok((index, None));
        }
        let model = model.to_owned();
        let session = &mut self.sessions[index];
        if let Some(spec) = &mut session.chat {
            spec.provider_id = Some(provider_id.clone());
            spec.model = Some(model.clone());
        }
        if let Some(chat) = &mut session.info.chat {
            chat.provider_id = Some(provider_id.clone());
            chat.model.clone_from(&model);
        }
        self.emit_session(index);
        self.persist_session(index);
        Ok((index, Some((provider_id, model))))
    }

    /// Starts the agent of a `Starting` chat session on a task; the outcome comes back as
    /// `ChatStarted`.
    fn spawn_chat_start(&mut self, index: usize, reply: Option<Reply<SessionInfo>>, wake: bool) {
        let session = &self.sessions[index];
        let Some(spec) = session.chat.clone() else {
            return;
        };
        let id = session.info.id;
        let cwd = session.info.cwd.clone().unwrap_or_default();
        // The project whose `[checkpoints.projects.*]` apply to the chat's turns.
        let scope = self
            .workspaces
            .iter()
            .find(|w| w.info.id == session.info.workspace_id)
            .map(|w| crate::checkpoints::turns::Scope {
                project_id: w.info.id.to_string(),
                project_name: w.info.name.clone(),
            })
            .unwrap_or_default();
        let chats = self.chats.clone();
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move {
            let outcome = if wake {
                chats.start_waking(id, cwd, &spec, scope).await
            } else {
                chats.start(id, cwd, &spec, scope).await
            };
            let command = Command::ChatStarted {
                session: id,
                outcome,
                reply,
                wake,
            };
            match mailbox.upgrade() {
                Some(tx) => {
                    // A closed mailbox means the daemon is going down.
                    let _ = tx.send(command).await;
                }
                None => {
                    // Nobody left to adopt the agent.
                    chats.stop(id);
                }
            }
        });
    }

    fn on_chat_started(
        &mut self,
        id: SessionId,
        outcome: Result<Started, ErrorBody>,
        reply: Option<Reply<SessionInfo>>,
        wake: bool,
    ) {
        let respond = |reply: Option<Reply<SessionInfo>>,
                       result: Result<SessionInfo, ErrorBody>| {
            if let Some(reply) = reply {
                let _ = reply.send(result);
            }
        };
        let Ok(index) = self.session_index(id) else {
            // The workspace was closed meanwhile; there is nothing to attach the agent to.
            self.chats.stop(id);
            respond(
                reply,
                Err(ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("session {id} not found"),
                )),
            );
            return;
        };
        let session = &mut self.sessions[index];
        match outcome {
            Ok(started) => {
                let before = wake.then(|| session.info.clone());
                if !started.resumed
                    && let Some(Recovery {
                        kind: RecoveryKind::ChatResumed { thread_id, .. },
                        ..
                    }) = &mut session.info.recovery
                {
                    // Asked to continue a conversation, got a new one: the journal still has
                    // the old one.
                    *thread_id = None;
                }
                if let Some(spec) = &mut session.chat {
                    spec.thread_id = Some(started.thread_id.clone());
                    spec.model = Some(started.model.clone());
                }
                if let Some(chat) = &mut session.info.chat {
                    chat.model = started.model;
                    chat.images = started.images;
                    chat.mode = started.mode.map(|mode| ChatMode::new(mode.id, mode.name));
                    if !is_gateway_agent(&chat.agent) {
                        chat.agent_session_id = Some(started.thread_id);
                    }
                }
                if !wake {
                    session.advance(SessionEvent::Ready);
                }
                if session.stopping {
                    // A stop arrived while the agent was starting; it goes down right away
                    // and the exit finishes the session.
                    self.chats.stop(id);
                    respond(
                        reply,
                        Err(ErrorBody::new(
                            ErrorCode::InvalidState,
                            "the session was stopped while it started",
                        )),
                    );
                } else {
                    if !wake {
                        self.sync_chat_state(index);
                    }
                    respond(reply, Ok(self.sessions[index].info.clone()));
                }
                // A woken session was idle all along: clients hear of it only when what they
                // know of it changed (the model the agent came back with, a new conversation).
                if before.is_none_or(|before| before != self.sessions[index].info) {
                    self.emit_session(index);
                }
                self.persist_session(index);
            }
            Err(error) if wake && !session.stopping => {
                // The session stays asleep and usable for a retry: nothing about it changed.
                warn!(session = %id, message = %error.message, "the agent of a sleeping assistant failed to start");
                self.chats.mark_asleep(id);
                respond(reply, Err(error));
            }
            Err(error) => {
                warn!(session = %id, message = %error.message, "chat agent failed to start");
                let stopping = session.stopping;
                self.recovery_failed(index, format!("resuming failed: {}", error.message));
                let session = &mut self.sessions[index];
                session.advance(if stopping {
                    SessionEvent::StopCompleted
                } else {
                    SessionEvent::Failed {
                        reason: error.message.clone(),
                    }
                });
                session.stopping = false;
                let waiters = std::mem::take(&mut session.stop_waiters);
                let info = session.info.clone();
                self.emit_session(index);
                self.persist_session(index);
                respond(reply, Err(error));
                for waiter in waiters {
                    let _ = waiter.send(Ok(info.clone()));
                }
                self.settle_stop_all();
                if self.sessions[index].close.is_some() {
                    self.complete_close(index);
                }
            }
        }
    }

    /// Records what the pump reports and moves the session's state to match.
    fn on_chat_state(&mut self, id: SessionId, turn: Option<bool>, pending: u32, questions: u32) {
        let Ok(index) = self.session_index(id) else {
            return;
        };
        let before = self.sessions[index].info.clone();
        if let Some(chat) = &mut self.sessions[index].info.chat {
            if let Some(active) = turn {
                chat.turn_active = active;
            }
            chat.pending_approvals = pending;
            chat.pending_questions = questions;
        }
        self.sync_chat_state(index);
        if self.sessions[index].info != before {
            self.emit_session(index);
            self.persist_session(index);
        }
    }

    /// The harness's session mode changed: `ChatInfo.mode` says so (read only, protocol 0.34).
    fn on_chat_mode(&mut self, id: SessionId, mode: SessionMode) {
        let Ok(index) = self.session_index(id) else {
            return;
        };
        let mode = ChatMode::new(mode.id, mode.name);
        let Some(chat) = &mut self.sessions[index].info.chat else {
            return;
        };
        if chat.mode.as_ref() == Some(&mode) {
            return;
        }
        chat.mode = Some(mode);
        let session = self.sessions[index].info.clone();
        self.emit(&Event::SessionUpdated { session });
    }

    /// Whether the chat's turns get checkpoints may have changed: `ChatInfo.checkpoints`
    /// follows what the checkpoint service says now (protocol 0.37), and `checkpoint.state`
    /// tells when it changed. A chat the service forgot (its agent stopped) keeps the last one.
    fn on_chat_checkpoints(&mut self, id: SessionId) {
        let Some(status) = self.chats.checkpoints().status(id) else {
            return;
        };
        let Ok(index) = self.session_index(id) else {
            return;
        };
        let Some(chat) = &mut self.sessions[index].info.chat else {
            return;
        };
        if chat.checkpoints.as_ref() == Some(&status) {
            return;
        }
        chat.checkpoints = Some(status.clone());
        let session = self.sessions[index].info.clone();
        self.emit(&Event::CheckpointStateChanged {
            session_id: id,
            state: status.state,
            reason: status.reason,
        });
        self.emit(&Event::SessionUpdated { session });
    }

    /// An update of the session's agent tree from its adapter (the main agent's thread and the
    /// subagents' threads).
    fn on_chat_node(&mut self, id: SessionId, update: NodeUpdate) {
        let Ok(index) = self.session_index(id) else {
            return;
        };
        let session = &mut self.sessions[index];
        // The agent's last words can arrive after the session was stopped.
        if !session.info.state.is_live() {
            return;
        }
        session.tree.apply(update, now_ms());
        self.tree_changed(index);
    }

    /// Moves a live chat session to the state its turn and approval counts call for:
    /// `waiting_approval` while an approval or a question (protocol 0.24) is pending (the state
    /// means "waiting for the user"), else `running` during a turn, else `idle`.
    /// A session still starting keeps its state until it is ready.
    fn sync_chat_state(&mut self, index: usize) {
        let session = &mut self.sessions[index];
        let Some(chat) = &session.info.chat else {
            return;
        };
        let target = if chat.pending_approvals > 0 || chat.pending_questions > 0 {
            SessionState::WaitingApproval
        } else if chat.turn_active {
            SessionState::Running
        } else {
            SessionState::Idle
        };
        // At most three steps separate any two of the states.
        for _ in 0..3 {
            let event = match (&session.info.state, &target) {
                (SessionState::Idle, SessionState::Running | SessionState::WaitingApproval) => {
                    SessionEvent::TurnStarted
                }
                (SessionState::Running, SessionState::WaitingApproval) => {
                    SessionEvent::ApprovalRequested
                }
                (SessionState::WaitingApproval, SessionState::Running | SessionState::Idle) => {
                    SessionEvent::ApprovalResolved
                }
                (SessionState::Running, SessionState::Idle) => SessionEvent::TurnFinished,
                _ => return,
            };
            session.advance(event);
        }
    }

    /// A message or a model switch needs the agent of a sleeping assistant: it starts again
    /// and continues the conversation; the reply comes when it runs.
    fn wake_chat(&mut self, id: SessionId, reply: Reply<SessionInfo>) {
        let index = match self.session_index(id) {
            Ok(index) => index,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let session = &self.sessions[index];
        if session.chat.is_none()
            || !session.info.state.is_live()
            || session.stopping
            || !self.chats.take_asleep(id)
        {
            let _ = reply.send(Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("chat session {id} is not asleep"),
            )));
            return;
        }
        // The agent starts in the mode `assistant.mode` says now: what the session shows (a
        // Codex assistant's sandbox and approval policy) follows.
        let launched = self.sessions[index]
            .chat
            .as_ref()
            .map(|chat| self.chats.launch_spec(chat));
        if let Some(launched) = launched {
            let session = &mut self.sessions[index];
            let changed = session.chat.as_ref().is_some_and(|chat| {
                chat.approval_policy != launched.approval_policy
                    || chat.sandbox != launched.sandbox
                    || chat.mode != launched.mode
            });
            if changed {
                if let Some(info) = &mut session.info.chat {
                    info.approval_policy = launched.approval_policy;
                    info.sandbox = launched.sandbox;
                }
                session.chat = Some(launched);
                self.emit_session(index);
                self.persist_session(index);
            }
        }
        self.spawn_chat_start(index, Some(reply), true);
    }

    /// The idle sleeper stopped the agent of an assistant. For its clients the session is what
    /// it was, idle; only what the dead process knew (its agents) is let go.
    fn on_chat_slept(&mut self, id: SessionId) {
        let Ok(index) = self.session_index(id) else {
            return;
        };
        let session = &mut self.sessions[index];
        if !session.info.state.is_live() {
            return;
        }
        if let Some(chat) = &mut session.info.chat {
            chat.turn_active = false;
            chat.pending_approvals = 0;
            chat.pending_questions = 0;
        }
        session.tree.session_ended(now_ms());
        self.tree_changed(index);
    }

    fn on_chat_exited(&mut self, id: SessionId, exit: &ExitInfo) {
        let Ok(index) = self.session_index(id) else {
            return;
        };
        let session = &mut self.sessions[index];
        if !session.info.state.is_live() {
            return;
        }
        if !session.stopping && session.keep_live.is_none() {
            self.recovery_failed(index, exit_reason(exit));
        }
        let session = &mut self.sessions[index];
        session.advance(if session.stopping {
            SessionEvent::StopCompleted
        } else if exit.code == Some(0) && exit.signal.is_none() {
            SessionEvent::ProcessExited {
                code: exit.code,
                signal: exit.signal,
            }
        } else {
            SessionEvent::Failed {
                reason: exit_reason(exit),
            }
        });
        if let Some(chat) = &mut session.info.chat {
            chat.turn_active = false;
            chat.pending_approvals = 0;
            chat.pending_questions = 0;
        }
        session.stopping = false;
        // A daemon shutdown keeps the tree as it was: the next daemon marks it restored.
        if session.keep_live.is_none() {
            session.tree.session_ended(now_ms());
        }
        self.sync_agent_summary(index);
        let session = &mut self.sessions[index];
        let info = session.info.clone();
        let waiters = std::mem::take(&mut session.stop_waiters);
        self.emit_session(index);
        self.persist_session(index);
        self.tree_changed(index);
        self.emit(&Event::SessionExited {
            session_id: id,
            code: exit.code,
            signal: exit.signal,
        });
        for waiter in waiters {
            let _ = waiter.send(Ok(info.clone()));
        }
        self.settle_stop_all();
        if self.sessions[index].close.is_some() {
            self.complete_close(index);
        }
    }

    /// A stopped or failed chat session comes back: the agent resumes the stored conversation.
    fn restart_chat(&mut self, index: usize) -> SessionInfo {
        let session = &mut self.sessions[index];
        session.advance(SessionEvent::Restart);
        session.stopping = false;
        session.keep_live = None;
        session.info.restored = false;
        if let Some(chat) = &mut session.info.chat {
            chat.turn_active = false;
            chat.pending_approvals = 0;
            chat.pending_questions = 0;
        }
        self.emit_session(index);
        self.persist_session(index);
        self.spawn_chat_start(index, None, false);
        self.sessions[index].info.clone()
    }

    /// Rebuilds a stored chat session. A live one is started again (the agent resumes its
    /// conversation, the journal keeps the history) unless `mode` says to list it as stopped.
    fn restore_chat(
        &mut self,
        row: SessionRow,
        mode: RestoreMode,
        context: &recovery::Context,
        facts: &recovery::Facts,
    ) -> ChatRestore {
        let was_live = row.state.is_live();
        let (spec, unusable) = match serde_json::from_str::<ChatSpec>(&row.spec_json) {
            Ok(spec) => (spec, None),
            Err(error) => {
                warn!(session = %row.id, %error, "stored chat spec is unreadable");
                (
                    ChatSpec {
                        agent: AGENT_CODEX.to_owned(),
                        model: None,
                        approval_policy: Default::default(),
                        sandbox: Default::default(),
                        thread_id: None,
                        provider_id: None,
                        system_prompt: None,
                        mode: None,
                        assistant: false,
                    },
                    Some(format!("stored spec unreadable: {error}")),
                )
            }
        };
        let mut state = row.state.clone();
        let mut ended_at_ms = row.ended_at_ms;
        let mut action = ChatRestoreKind::Keep;
        let cut_off = facts.interrupted_turns.contains(&row.id);
        let mut recovery = context.ended(row.ended_at_ms);
        if was_live {
            ended_at_ms = Some(now_ms());
            let kind = match (unusable, mode) {
                (Some(reason), _) => {
                    state = SessionState::Failed {
                        reason: reason.clone(),
                    };
                    action = ChatRestoreKind::Rewrite;
                    RecoveryKind::NotRestarted { reason }
                }
                (None, RestoreMode::Stopped) => {
                    state = SessionState::Stopped;
                    action = ChatRestoreKind::Rewrite;
                    RecoveryKind::NotRestarted {
                        reason: STOPPED_MODE_REASON.to_owned(),
                    }
                }
                (None, RestoreMode::Respawn) if spec.assistant => {
                    // An assistant is a helper for occasional questions: it comes back asleep
                    // (idle, same conversation) and its agent starts on first use.
                    state = SessionState::Idle;
                    ended_at_ms = None;
                    action = ChatRestoreKind::Rewrite;
                    self.chats.mark_asleep(row.id);
                    RecoveryKind::ChatResumed {
                        thread_id: spec.thread_id.clone(),
                        last_turn: recovery::last_turn(cut_off),
                    }
                }
                (None, RestoreMode::Respawn) => {
                    state = SessionState::Starting;
                    ended_at_ms = None;
                    action = ChatRestoreKind::Start;
                    RecoveryKind::ChatResumed {
                        thread_id: spec.thread_id.clone(),
                        last_turn: recovery::last_turn(cut_off),
                    }
                }
            };
            recovery = Some(context.recovery(kind, recovery::lost(facts, row.id, cut_off)));
        }
        let default_title = format!("{} · chat", spec.agent);
        self.sessions.push(SessionEntry {
            info: SessionInfo {
                id: row.id,
                workspace_id: row.workspace_id,
                kind: SessionKind::Harness,
                title: row.title,
                state,
                command: vec![spec.agent.clone()],
                cwd: row.cwd.clone(),
                terminal_id: None,
                agent: None,
                created_at_ms: row.created_at_ms,
                // The journal has the whole conversation: nothing is gone.
                restored: false,
                chat: Some(chat_info(&spec)),
                placement: None,
                folder: row.cwd.clone(),
                recovery,
                // `restore_mutes` / `reopen_session` set it.
                muted: false,
            },
            spec: TerminalSpec {
                command: vec![spec.agent.clone()],
                cwd: row.cwd,
                ..TerminalSpec::default()
            },
            default_title,
            title_locked: row.title_locked,
            osc_title: None,
            terminal: None,
            stopping: false,
            stop_waiters: Vec::new(),
            ended_at_ms,
            resume_ref_json: None,
            restore_argv: None,
            keep_live: None,
            dirty: false,
            agent_message: None,
            agent_session_id: None,
            tree: TreeState::new(row.id),
            chat: Some(spec),
            running_command: None,
            close: None,
        });
        let index = self.sessions.len() - 1;
        match action {
            ChatRestoreKind::Start => ChatRestore::Start(index),
            ChatRestoreKind::Rewrite => ChatRestore::Rewrite(index),
            ChatRestoreKind::Keep => ChatRestore::Keep,
        }
    }

    // Storage ---------------------------------------------------------------------------------

    /// A session keeps the project it started in. Only when that project's folder is gone (the
    /// repository moved) and the folder the session started in still exists does the session
    /// follow its folder to the project that is there now. Returns whether it moved.
    fn follow_moved_project(&mut self, row: &mut SessionRow) -> bool {
        let Some(entry) = self
            .workspaces
            .iter()
            .find(|w| w.info.id == row.workspace_id)
        else {
            return false;
        };
        let stale = entry.info.kind == WorkspaceKind::Project
            && entry.info.roots.first().is_some_and(|root| !root.exists());
        if !stale {
            return false;
        }
        let Some(start) = restore::start_cwd(row).filter(|dir| dir.is_dir()) else {
            return false;
        };
        let assignment = self.rules.assign_path(Some(&start));
        match self.workspace_for(assignment) {
            Ok(id) if id != row.workspace_id => {
                info!(session = %row.id, from = %row.workspace_id, to = %id, "session follows its folder to another project");
                row.workspace_id = id;
                true
            }
            Ok(_) => false,
            Err(error) => {
                warn!(session = %row.id, message = %error.message, "could not re-derive a session's project");
                false
            }
        }
    }

    /// Rebuilds the sessions from their rows. Live ones are spawned again (`Respawn`) or listed
    /// as stopped; ended ones stay as they were, marked `restored` because their screen is gone.
    fn restore_sessions(
        &mut self,
        rows: Vec<SessionRow>,
        mode: RestoreMode,
        trees: HashMap<SessionId, ObservationTree>,
        facts: recovery::Facts,
    ) {
        let context = recovery::Context::new(facts.previous.as_ref(), now_ms());
        let mut pending = Restoring::default();
        for row in rows {
            self.restore_row(row, mode, &context, &facts, &mut pending);
        }
        self.restore_mutes();
        // What the sessions ran when the previous daemon ended is not running now.
        recovery::store_running_commands(&self.storage, std::iter::empty());
        // Before the respawns announce anything: the counts of a restored workspace are right
        // from the start.
        self.recount_sessions();
        self.restore_trees(trees);
        self.place_restored_sessions();
        self.start_restored(pending);
    }

    /// Starts what [`Self::restore_row`] decided to start and writes the rows it changed.
    fn start_restored(&mut self, pending: Restoring) {
        // `start_terminal` stores the session itself once it is running.
        for index in pending.respawn {
            self.start_terminal(index);
        }
        for index in pending.chat_starts {
            self.spawn_chat_start(index, None, false);
        }
        for index in pending.rewrite {
            self.persist_session(index);
        }
    }

    /// Adds the session of one stored row to `self.sessions` as the restore `mode` says: a live
    /// row comes back (or is listed as stopped), an ended one stays as it was. What still has
    /// to be started or written is noted in `pending` (indices into `self.sessions`).
    fn restore_row(
        &mut self,
        mut row: SessionRow,
        mode: RestoreMode,
        context: &recovery::Context,
        facts: &recovery::Facts,
        pending: &mut Restoring,
    ) {
        {
            if !self
                .workspaces
                .iter()
                .any(|w| w.info.id == row.workspace_id)
            {
                warn!(session = %row.id, "stored session has no workspace; skipping it");
                return;
            }
            let moved = self.follow_moved_project(&mut row);
            if row.kind == SessionKind::Harness {
                match self.restore_chat(row, mode, context, facts) {
                    ChatRestore::Start(index) => pending.chat_starts.push(index),
                    ChatRestore::Rewrite(index) => pending.rewrite.push(index),
                    ChatRestore::Keep => {}
                }
                if moved && let Some(index) = self.sessions.len().checked_sub(1) {
                    // The new project is stored with the session.
                    self.persist_session(index);
                }
                return;
            }
            let was_live = row.state.is_live();
            let (spec, unusable) = match serde_json::from_str::<TerminalSpec>(&row.spec_json) {
                Ok(spec) if row.kind == SessionKind::Terminal => (spec, None),
                Ok(spec) => (
                    spec,
                    Some(format!("unsupported session kind {:?}", row.kind)),
                ),
                Err(error) => {
                    warn!(session = %row.id, %error, "stored session spec is unreadable");
                    (
                        TerminalSpec::default(),
                        Some(format!("stored spec unreadable: {error}")),
                    )
                }
            };
            let mut state = row.state.clone();
            let mut ended_at_ms = row.ended_at_ms;
            let mut restored = true;
            let mut restore_argv = None;
            let mut cwd = row.cwd.clone();
            let mut action = None;
            let mut recovery = context.ended(row.ended_at_ms);
            if was_live {
                ended_at_ms = Some(now_ms());
                let kind = match (unusable, mode) {
                    (Some(reason), _) => {
                        state = SessionState::Failed {
                            reason: reason.clone(),
                        };
                        action = Some(false);
                        RecoveryKind::NotRestarted { reason }
                    }
                    (None, RestoreMode::Stopped) => {
                        state = SessionState::Stopped;
                        action = Some(false);
                        RecoveryKind::NotRestarted {
                            reason: STOPPED_MODE_REASON.to_owned(),
                        }
                    }
                    (None, RestoreMode::Respawn) => {
                        state = SessionState::Starting;
                        ended_at_ms = None;
                        restored = false;
                        restore_argv = row
                            .resume_ref_json
                            .as_deref()
                            .and_then(restore::resume_argv);
                        // A directory that vanished meanwhile falls back to the spec's, then to
                        // the shell's default.
                        cwd = cwd
                            .filter(|dir| dir.is_dir())
                            .or_else(|| spec.cwd.clone().filter(|dir| dir.is_dir()));
                        action = Some(true);
                        match &restore_argv {
                            Some(argv) => RecoveryKind::AgentResumed {
                                resume_argv: argv.clone(),
                            },
                            None => RecoveryKind::ShellRespawned {
                                cwd: cwd.clone(),
                                was_running: facts.running_commands.get(&row.id).cloned(),
                            },
                        }
                    }
                };
                recovery = Some(context.recovery(kind, recovery::lost(facts, row.id, false)));
            }
            let default_title = default_title(&spec);
            let state_is_live = state.is_live();
            self.sessions.push(SessionEntry {
                info: SessionInfo {
                    id: row.id,
                    workspace_id: row.workspace_id,
                    kind: row.kind,
                    title: row.title,
                    state,
                    command: spec.command.clone(),
                    cwd: cwd.clone(),
                    terminal_id: None,
                    created_at_ms: row.created_at_ms,
                    agent: None,
                    restored,
                    chat: None,
                    placement: None,
                    folder: cwd,
                    recovery,
                    muted: false,
                },
                spec,
                default_title,
                title_locked: row.title_locked,
                osc_title: None,
                terminal: None,
                stopping: false,
                stop_waiters: Vec::new(),
                ended_at_ms,
                // Only a session that comes back to life resumes; the others forgot their agent.
                resume_ref_json: row.resume_ref_json.filter(|_| state_is_live),
                restore_argv,
                keep_live: None,
                dirty: false,
                agent_message: None,
                agent_session_id: None,
                tree: TreeState::new(row.id),
                chat: None,
                running_command: None,
                close: None,
            });
            let index = self.sessions.len() - 1;
            match action {
                Some(true) => pending.respawn.push(index),
                Some(false) => pending.rewrite.push(index),
                // A respawned session is stored by `start_terminal`.
                None if moved => pending.rewrite.push(index),
                None => {}
            }
        }
    }

    /// Gives every session the tree stored for it, brought up to date: a session that comes
    /// back may have its main agent running again (unknown until it says so), one that does
    /// not has lost every agent. Finished subtrees past their time are dropped.
    fn restore_trees(&mut self, mut trees: HashMap<SessionId, ObservationTree>) {
        let now = now_ms();
        for session in &mut self.sessions {
            if let Some(tree) = trees.remove(&session.info.id) {
                session.tree = TreeState::from_tree(tree);
            }
            if session.info.state.is_live() {
                session.tree.restored(now);
            } else {
                session.tree.ended_silently(now);
            }
            session.tree.prune_silently(now);
            if session.tree.is_dirty() {
                self.flush_at
                    .get_or_insert_with(|| Instant::now() + DIRTY_WRITE_INTERVAL);
            }
        }
    }

    fn session_row(&self, index: usize) -> SessionRow {
        let session = &self.sessions[index];
        // While the daemon takes a session down with itself the row keeps saying it was alive.
        let (state, ended_at_ms) = match &session.keep_live {
            Some(live) => (live.clone(), None),
            None => (session.info.state.clone(), session.ended_at_ms),
        };
        let spec = restore::persistable_spec(&session.spec);
        let spec_json = match &session.chat {
            Some(chat) => serde_json::to_string(chat),
            None => serde_json::to_string(&spec),
        };
        SessionRow {
            id: session.info.id,
            workspace_id: session.info.workspace_id,
            kind: session.info.kind,
            title: session.info.title.clone(),
            title_locked: session.title_locked,
            spec_json: spec_json.unwrap_or_else(|error| {
                error!(session = %session.info.id, %error, "could not encode a session spec");
                "{}".to_owned()
            }),
            cwd: session.info.cwd.clone(),
            state,
            created_at_ms: session.info.created_at_ms,
            ended_at_ms,
            resume_ref_json: session.resume_ref_json.clone(),
        }
    }

    /// Writes the whole session row now.
    fn persist_session(&mut self, index: usize) {
        self.sessions[index].dirty = false;
        let row = self.session_row(index);
        log_write(self.storage.sessions().upsert(row), "a session");
    }

    /// Title and cwd change with every prompt; they are written together at most once per
    /// [`DIRTY_WRITE_INTERVAL`].
    fn mark_dirty(&mut self, index: usize) {
        self.sessions[index].dirty = true;
        self.flush_at
            .get_or_insert_with(|| Instant::now() + DIRTY_WRITE_INTERVAL);
    }

    fn flush_dirty(&mut self) {
        self.flush_at = None;
        if std::mem::take(&mut self.commands_dirty) {
            recovery::store_running_commands(
                &self.storage,
                self.sessions
                    .iter()
                    .filter_map(|s| Some((s.info.id, s.running_command.clone()?))),
            );
        }
        for index in 0..self.sessions.len() {
            if self.sessions[index].dirty {
                self.persist_session(index);
            }
            if self.sessions[index].tree.take_dirty() {
                self.persist_tree(index);
            }
        }
    }

    // Agent trees -----------------------------------------------------------------------------

    /// Writes the session's tree now (or removes its row once the tree is empty).
    fn persist_tree(&self, index: usize) {
        let session = &self.sessions[index];
        let id = session.info.id;
        let result = match session.tree.stored() {
            Some(json) => self.storage.agent_trees().upsert(id, json, now_ms()),
            None => self.storage.agent_trees().delete(id),
        };
        log_write(result, "an agent tree");
    }

    /// Recomputes the session's `agent` summary from its tree. Returns whether clients would
    /// see a difference (the timestamp aside).
    fn sync_agent_summary(&mut self, index: usize) -> bool {
        let session = &mut self.sessions[index];
        let next = tree::summary(
            session.info.state.is_live(),
            session.tree.tree(),
            session.agent_message.as_deref(),
            session.agent_session_id.as_deref(),
        );
        let changed = !tree::same_summary(session.info.agent.as_ref(), next.as_ref());
        session.info.agent = next;
        changed
    }

    /// The agents of a session changed (its tree took a report, a release, an exit): the
    /// summary older clients read is refreshed and announced when it differs, the tree's own
    /// change is announced as far as the rate limit allows, and the stored copy is queued.
    fn tree_changed(&mut self, index: usize) {
        if self.sync_agent_summary(index) {
            self.emit_session(index);
        }
        self.publish_tree_event(index, Instant::now());
        if self.sessions[index].tree.is_dirty() {
            self.flush_at
                .get_or_insert_with(|| Instant::now() + DIRTY_WRITE_INTERVAL);
        }
        self.schedule_tree_events();
    }

    /// Announces what changed in the session's tree, unless the last announcement was too
    /// recent (then the timer does it).
    fn publish_tree_event(&mut self, index: usize, now: Instant) {
        if let Some(event) = self.sessions[index].tree.take_event(now) {
            self.emit(&event);
        }
    }

    fn schedule_tree_events(&mut self) {
        self.tree_flush_at = self
            .sessions
            .iter()
            .filter_map(|s| s.tree.event_due())
            .min();
    }

    /// The timer for held-back tree announcements went off.
    fn flush_tree_events(&mut self) {
        let now = Instant::now();
        for index in 0..self.sessions.len() {
            self.publish_tree_event(index, now);
        }
        self.schedule_tree_events();
    }

    // Events ----------------------------------------------------------------------------------

    /// Announces the session and, when the change moved its workspace's counts, the workspace.
    fn emit_session(&mut self, index: usize) {
        self.refresh_placement(index);
        if self.retitle(index) {
            self.mark_dirty(index);
        }
        self.emit(&Event::SessionUpdated {
            session: self.sessions[index].info.clone(),
        });
        self.sync_workspace(self.sessions[index].info.workspace_id);
    }

    // Placement -------------------------------------------------------------------------------

    /// The placement of a session working in `dir`, before anyone shares it.
    fn placement_of(&mut self, dir: Option<&Path>) -> Option<Placement> {
        dir.and_then(|dir| self.placements.placement(dir))
    }

    /// Derives the placement of session `index` from its directory again (its checkout may
    /// have changed: it moved, a branch was switched) and who shares it, and announces the
    /// other sessions whose placement changed. The session itself is the caller's to announce.
    /// Returns whether its own placement changed.
    fn refresh_placement(&mut self, index: usize) -> bool {
        let before = self.sessions[index].info.placement.clone();
        let cwd = self.sessions[index].info.cwd.clone();
        let mut next = self.placement_of(cwd.as_deref());
        if let (Some(next), Some(before)) = (&mut next, &before) {
            // Kept until the sharing is worked out below; one change, one announcement.
            next.shared_with.clone_from(&before.shared_with);
        }
        self.sessions[index].info.placement = next;
        for other in self.sync_shared() {
            if other != index {
                self.emit(&Event::SessionUpdated {
                    session: self.sessions[other].info.clone(),
                });
            }
        }
        self.sessions[index].info.placement != before
    }

    /// Works out which sessions share a checkout and stores it; returns the sessions whose
    /// `shared_with` changed.
    fn sync_shared(&mut self) -> Vec<usize> {
        let occupants: Vec<Occupant<'_>> = self
            .sessions
            .iter()
            .map(|session| Occupant {
                id: session.info.id,
                live: session.info.state.is_live(),
                agent: session.info.agent.is_some(),
                root: session.info.placement.as_ref().map(|p| p.root.as_path()),
            })
            .collect();
        let shared = shared_with(&occupants);
        drop(occupants);
        let mut changed = Vec::new();
        for (index, shared) in shared.into_iter().enumerate() {
            if let Some(placement) = &mut self.sessions[index].info.placement
                && placement.shared_with != shared
            {
                placement.shared_with = shared;
                changed.push(index);
            }
        }
        changed
    }

    /// `worktree.*` created or removed checkouts: what was learned of directories is stale.
    fn invalidate_placements(&mut self) {
        self.placements.clear();
        for index in 0..self.sessions.len() {
            if self.refresh_placement(index) {
                self.emit_session(index);
            }
        }
    }

    /// Derives the placement of every restored session; nobody listens yet, so nothing is
    /// announced.
    fn place_restored_sessions(&mut self) {
        for index in 0..self.sessions.len() {
            let cwd = self.sessions[index].info.cwd.clone();
            self.sessions[index].info.placement = self.placement_of(cwd.as_deref());
        }
        self.sync_shared();
        let workspaces: Vec<WorkspaceId> = self.workspaces.iter().map(|w| w.info.id).collect();
        for workspace in workspaces {
            self.retitle_workspace(workspace, None);
        }
    }

    // Names -----------------------------------------------------------------------------------

    /// What the naming policy (`session_names`) sees of a session.
    fn name_entry(session: &SessionEntry) -> session_names::Entry {
        let auto = session.chat.is_none()
            && session.info.kind == SessionKind::Terminal
            && !session.title_locked;
        session_names::Entry {
            created_at_ms: session.info.created_at_ms,
            naming: if auto {
                Naming::Auto(Facts {
                    folder: session.info.cwd.clone(),
                    branch: session
                        .info
                        .placement
                        .as_ref()
                        .and_then(|p| p.branch.clone()),
                    osc_title: session.osc_title.clone(),
                    default_title: session.default_title.clone(),
                    running_command: session.running_command.clone(),
                })
            } else {
                Naming::Fixed(session.info.title.clone())
            },
        }
    }

    /// The names of the sessions of `workspace` (indices into `self.sessions`, then the names,
    /// `None` for those the user named), and of `extra`, a session about to be created, last.
    fn derive_titles(
        &self,
        workspace: WorkspaceId,
        extra: Option<(u64, Facts)>,
    ) -> (Vec<usize>, Vec<Option<String>>) {
        let indices: Vec<usize> = (0..self.sessions.len())
            .filter(|&i| self.sessions[i].info.workspace_id == workspace)
            .collect();
        let mut entries: Vec<session_names::Entry> = indices
            .iter()
            .map(|&i| Self::name_entry(&self.sessions[i]))
            .collect();
        entries.extend(extra.map(|(created_at_ms, facts)| session_names::Entry {
            created_at_ms,
            naming: Naming::Auto(facts),
        }));
        let home = std::env::var_os("HOME").map(PathBuf::from);
        (indices, session_names::derive(&entries, home.as_deref()))
    }

    /// Names the automatically named sessions of `workspace` again and announces (and
    /// stores) the ones that changed, except `own`, whose caller announces it. Returns whether
    /// `own` changed.
    fn retitle_workspace(&mut self, workspace: WorkspaceId, own: Option<usize>) -> bool {
        let (indices, titles) = self.derive_titles(workspace, None);
        let mut own_changed = false;
        for (index, title) in indices.into_iter().zip(titles) {
            let Some(title) = title else { continue };
            if self.sessions[index].info.title == title {
                continue;
            }
            self.sessions[index].info.title = title;
            if Some(index) == own {
                own_changed = true;
            } else {
                self.emit(&Event::SessionUpdated {
                    session: self.sessions[index].info.clone(),
                });
                self.mark_dirty(index);
            }
        }
        own_changed
    }

    /// Names session `index` and its namesakes again; whether its own name changed.
    fn retitle(&mut self, index: usize) -> bool {
        let workspace = self.sessions[index].info.workspace_id;
        self.retitle_workspace(workspace, Some(index))
    }

    /// For changes that already happened: a rejected event is logged, not rolled back.
    fn emit(&self, event: &Event) {
        if let Err(error) = self.bus.publish(event) {
            error!(%error, "failed to publish event");
        }
    }

    /// Publishes before the caller commits, so a rejected event leaves state untouched.
    fn publish(&self, event: &Event) -> Result<(), ErrorBody> {
        match self.bus.publish(event) {
            Ok(_) => Ok(()),
            Err(PublishError::TooLarge { len }) => Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("description too large ({len} bytes)"),
            )),
            Err(e) => {
                error!(error = %e, "failed to publish event");
                Err(ErrorBody::new(
                    ErrorCode::Internal,
                    "failed to publish event",
                ))
            }
        }
    }
}

/// Forwards one terminal's metadata and its exit to the registry.
async fn watch_terminal(
    id: TerminalId,
    terminal: Terminal,
    mut meta: broadcast::Receiver<TerminalMeta>,
    mailbox: mpsc::WeakSender<Command>,
) {
    let send = |command: Command| {
        let mailbox = mailbox.clone();
        async move {
            match mailbox.upgrade() {
                Some(tx) => tx.send(command).await.is_ok(),
                None => false,
            }
        }
    };

    // A title set before the subscription only exists in the engine state.
    let early_title = terminal.snapshot(&mut RenderTracker::new()).title;
    if let Some(title) = early_title
        && !send(Command::TerminalMeta(id, TerminalMeta::Title(title))).await
    {
        return;
    }

    let exit = terminal.wait_exit();
    tokio::pin!(exit);
    // Output is activity of the session's workspace, reported at most once per
    // `ACTIVITY_NOTIFY_INTERVAL`; damage that arrives meanwhile is picked up afterwards.
    let mut damage = terminal.damage();
    let mut quiet = std::pin::pin!(sleep_until(Instant::now()));
    let mut throttled = false;
    let mut watching_output = true;
    let status = loop {
        tokio::select! {
            biased;
            () = &mut quiet, if throttled => throttled = false,
            changed = damage.changed(), if watching_output && !throttled => {
                if changed.is_err() {
                    watching_output = false;
                } else {
                    if !send(Command::Activity(id)).await {
                        return;
                    }
                    throttled = true;
                    quiet.as_mut().reset(Instant::now() + ACTIVITY_NOTIFY_INTERVAL);
                }
            }
            received = meta.recv() => match received {
                Ok(event) => {
                    if !send(Command::TerminalMeta(id, event)).await {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(terminal = %id, skipped, "terminal metadata lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break exit.await,
            },
            status = &mut exit => break status,
        }
    };
    // Metadata the child emitted just before exiting.
    loop {
        match meta.try_recv() {
            Ok(event) => {
                if !send(Command::TerminalMeta(id, event)).await {
                    return;
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => break,
        }
    }
    debug!(terminal = %id, ?status, "terminal exited");
    send(Command::TerminalExited(id, status)).await;
}

/// `source`, `agent`, and `agent_session_id` are short, non-empty identifiers.
fn agent_field(name: &str, value: &str) -> Result<(), ErrorBody> {
    if value.is_empty() || value.len() > MAX_AGENT_FIELD_BYTES {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("`{name}` must be 1 to {MAX_AGENT_FIELD_BYTES} bytes"),
        ));
    }
    Ok(())
}

/// The first `max` characters of `text`.
fn clipped(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// What the daemon checks of the node of a report: ids are short identifiers, `usage` says what
/// its numbers mean. Free text is clipped, not refused.
fn check_node_report(node: &NodeReport) -> Result<(), ErrorBody> {
    agent_field("node.id", &node.id)?;
    if let Some(parent) = &node.parent {
        agent_field("node.parent", parent)?;
    }
    if let Some(usage) = &node.usage
        && (usage.kind.is_empty() || usage.kind.len() > MAX_USAGE_KIND_BYTES)
    {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("`node.usage.kind` must be 1 to {MAX_USAGE_KIND_BYTES} bytes"),
        ));
    }
    Ok(())
}

/// The reducer's input for a report. A report with a state replaces what the node is doing
/// (`activity`, else the report's `message`); one without only changes what it carries.
fn node_update(
    source: &str,
    agent: &str,
    state: Option<AgentState>,
    node: Option<NodeReport>,
    message: Option<&str>,
) -> NodeUpdate {
    let mut update = NodeUpdate {
        source: source.to_owned(),
        agent: agent.to_owned(),
        state: state.map(tree::node_state),
        ..NodeUpdate::default()
    };
    let mut text = message.map(str::to_owned);
    if let Some(node) = node {
        update.id = Some(node.id);
        update.parent = node.parent;
        update.kind = node.kind;
        update.name = node.name.map(|name| clipped(&name, MAX_NODE_TEXT_CHARS));
        update.agent_type = node
            .agent_type
            .map(|agent_type| clipped(&agent_type, MAX_NODE_TEXT_CHARS));
        update.outcome = node.outcome;
        update.model = node.model.map(|model| clipped(&model, MAX_NODE_TEXT_CHARS));
        update.usage = node.usage;
        update.transcript_path = node
            .transcript_path
            .filter(|path| !path.is_empty() && path.len() <= MAX_TRANSCRIPT_PATH_BYTES);
        text = node.activity.or(text);
    }
    update.activity = match text {
        Some(text) => Patch::Set(text),
        None if state.is_some() => Patch::Clear,
        None => Patch::Keep,
    };
    update
}

/// `session.close` of a session something runs in: `invalid_state` with `data.busy` so that a
/// client can say what runs and ask.
fn busy_error(title: &str, busy: &SessionBusy) -> ErrorBody {
    let what = match (busy.state.as_str(), &busy.agent, &busy.command) {
        ("turn", ..) => "a reply is being written".to_owned(),
        ("approval", ..) => "a request is waiting for your answer".to_owned(),
        (state, Some(agent), _) => format!(
            "{agent} is {}",
            if state == "blocked" {
                "waiting for you"
            } else {
                "working"
            }
        ),
        (_, None, Some(command)) => format!("`{command}` is still running"),
        (_, None, None) => "something is running".to_owned(),
    };
    let mut error = ErrorBody::new(
        ErrorCode::InvalidState,
        format!("{title} is busy: {what}; close it with stop_running to stop it"),
    );
    error.data = serde_json::to_value(busy)
        .ok()
        .map(|busy| serde_json::json!({ "busy": busy }));
    error
}

fn validated_name(name: String, what: &str) -> Result<String, ErrorBody> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("{what} name must not be empty"),
        ));
    }
    Ok(trimmed.to_owned())
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

enum ChatRestoreKind {
    Keep,
    Rewrite,
    Start,
}

/// What [`SessionRegistry::restore_chat`] left to do for the session it added.
enum ChatRestore {
    /// Nothing: the session is shown as it was.
    Keep,
    /// Its state changed; write it.
    Rewrite(usize),
    /// Start its agent again.
    Start(usize),
}

/// What [`SessionRegistry::restore_row`] left to do for the sessions it added.
#[derive(Debug, Default)]
struct Restoring {
    /// Terminals to spawn.
    respawn: Vec<usize>,
    /// Chat agents to start.
    chat_starts: Vec<usize>,
    /// Sessions whose row changed; write them.
    rewrite: Vec<usize>,
}

fn chat_info(spec: &ChatSpec) -> ChatInfo {
    ChatInfo {
        agent: spec.agent.clone(),
        model: spec.model.clone().unwrap_or_default(),
        approval_policy: spec.approval_policy,
        sandbox: spec.sandbox,
        turn_active: false,
        pending_approvals: 0,
        pending_questions: 0,
        provider_id: spec.provider_id.clone(),
        assistant: spec.assistant,
        images: None,
        mode: None,
        checkpoints: None,
        agent_session_id: (!is_gateway_agent(&spec.agent))
            .then(|| spec.thread_id.clone())
            .flatten(),
    }
}

/// Why a chat agent that ended on its own is a failure.
/// What became of a respawned terminal's process that ended by itself.
fn terminal_exit_reason(code: Option<i32>, signal: Option<i32>) -> String {
    match (code, signal) {
        (_, Some(signal)) => format!("it was killed by signal {signal} after the restart"),
        (Some(code), None) => format!("it exited with code {code} after the restart"),
        (None, None) => "it exited after the restart".to_owned(),
    }
}

fn exit_reason(exit: &ExitInfo) -> String {
    let how = match (exit.code, exit.signal) {
        (_, Some(signal)) => format!("killed by signal {signal}"),
        (Some(code), None) => format!("exited with code {code}"),
        (None, None) => "exited".to_owned(),
    };
    let tail = exit.stderr_tail.trim();
    if tail.is_empty() {
        format!("the agent {how}")
    } else {
        format!("the agent {how}: {tail}")
    }
}

/// argv[0]'s basename, or the login shell's name when the spec runs the default shell.
fn default_title(spec: &TerminalSpec) -> String {
    let program = match spec.command.first() {
        Some(program) => program.clone(),
        None => hutan_terminal::default_shell(&spec.env),
    };
    Path::new(&program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "terminal".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_title_is_the_program_basename() {
        let spec = |command: &[&str], shell: Option<&str>| TerminalSpec {
            command: command.iter().map(|c| (*c).to_owned()).collect(),
            env: shell
                .map(|s| BTreeMap::from([("SHELL".to_owned(), s.to_owned())]))
                .unwrap_or_default(),
            ..TerminalSpec::default()
        };
        assert_eq!(default_title(&spec(&["/usr/bin/omp", "-x"], None)), "omp");
        assert_eq!(default_title(&spec(&["htop"], None)), "htop");
        assert_eq!(
            default_title(&spec(&[], Some("/opt/homebrew/bin/fish"))),
            "fish"
        );
    }
}
