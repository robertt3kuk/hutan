//! [`SessionsStore`]: the desktop's view of the daemon's sessions, as a GPUI entity.
//!
//! Bridges the [`Connection`]'s `state()` watch and `notices()` broadcast (Tokio types that
//! work on any executor) onto GPUI's executor, tracks attention, hands out one
//! [`TerminalSession`] per terminal, and runs the session lifecycle commands.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{AppContext as _, Context, Entity, EventEmitter, Task, WeakEntity};
use hutan_client::recovery::{self, Summary};
use hutan_client::time::relative_to_now;
use hutan_client::{Connection, ConnectionState, Notice, StoreState};
use hutan_core::agent::ModelInfo;
use hutan_core::{
    AgentObservation, ApprovalId, Epoch, ReviewDecision, SessionId, Task as DaemonTask, TaskId,
    TerminalId, WorkspaceId,
};
use hutan_protocol::chat::{ApprovalInfo, ChatCreate};
use hutan_protocol::checkpoint::{CheckpointPreviewRequest, CheckpointRestore, CheckpointRestored};
use hutan_protocol::messages::{
    AgentReportRequest, RecentProject, RecentProjects, Request, SessionCreate, SessionInfo,
    SessionRef, SessionRename, SessionResult, SessionSpec, TerminalSpec, WorkspaceCreate,
    WorkspaceInfo, WorkspaceKind, WorkspaceMove, WorkspaceOpen, WorkspaceOpened, WorkspacePin,
    WorkspaceRecent, WorkspaceRef, WorkspaceRename, WorkspaceRoot,
};
use hutan_protocol::notify::NotificationIntent;
use hutan_protocol::project::{WorkspaceAssign, WorkspaceDelete};
use hutan_protocol::recovery::{RecoveryAck, RecoveryAcked};
use hutan_protocol::resources::{ResourceSample, ResourcesListRequest};
use hutan_protocol::task::{
    TaskCreate, TaskDelete, TaskDeleted, TaskList, TaskListRequest, TaskRef, TaskResult,
    TaskReview, TaskReviewed, TaskStarted, TaskUpdate,
};
use hutan_protocol::worktree::{
    PlacementKind, WorktreeCommit, WorktreeCommitted, WorktreeCreate, WorktreeDiff,
    WorktreeDiffRequest, WorktreeDiscard, WorktreeDiscarded, WorktreeFileDiff,
    WorktreeFileDiffRequest, WorktreeFinish, WorktreeFinished, WorktreeHunks, WorktreeHunksRequest,
    WorktreeIndexChange, WorktreeIndexed, WorktreeInfo, WorktreeList, WorktreeListRequest,
    WorktreeRemove, WorktreeRemoved,
};
use hutan_terminal_view::DefaultColors;
use tokio::sync::broadcast::error::RecvError;

use super::agents::{self, AgentAttentionChanged, AgentBadge};
use super::attention::{
    Attention, AttentionEntry, AttentionKind, AttentionTracker, Marks, Presentation, Tone, Unread,
    classify, present,
};
use super::calls;
use super::models::{self, ModelNames};
use super::presentation::{self, AgentPresentation, Facts};
use super::tasks::TaskBook;
use super::terminal::TerminalSession;
use super::untracked::{self, Untracked};

/// Who reports an agent that the user chose to track (`agent.report` `source`).
pub const TRACK_SOURCE: &str = "hutan-desktop";

/// Notifications that arrive this soon after (re)connecting are replays of what happened before
/// the desktop was there; they must not flag attention.
pub const REPLAY_WINDOW: Duration = Duration::from_millis(400);

/// A `ui.open` older than this many milliseconds when it reaches the desktop is a replay of
/// something the user did a while ago, not a request to move the window now.
pub const UI_OPEN_MAX_AGE_MS: u64 = 15_000;

/// Dirty checkouts whose uncommitted files are counted for the sidebar's badges; the rest show
/// none.
const MAX_COUNTED_CHECKOUTS: usize = 16;

/// Most notification intents remembered to acknowledge when their sessions are read (the
/// daemon keeps at most this many pending).
const MAX_SURFACED: usize = 100;

/// One checkout of a repository: its main checkout or a linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    pub path: PathBuf,
    /// `None` for a detached HEAD.
    pub branch: Option<String>,
    pub main: bool,
    pub dirty: bool,
}

/// What kind of thing a scope is: the glyph of its chip and the sidebar's look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Home,
    Workspace,
    /// A project that is a git repository (its main checkout).
    Repository,
    /// A project outside git: a folder, never with a branch.
    Folder,
    /// One linked worktree of a repository.
    Worktree,
    /// Mawas's scope while Mawas Home is the focused center tab (`Mawas · Global`,
    /// `Mawas · <workspace>`).
    Mawas,
}

/// Which scope of Mawas Home the focused center tab shows: Files, Search and Problems follow
/// it (`docs/design.md` § Files at Home). Desktop state only; the daemon is not told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MawasFocus {
    /// The Home workspace's Mawas: she works in the Home folder.
    Global,
    /// The Mawas of a workspace or project: she works in its (active) project.
    Workspace(WorkspaceId),
}

/// The folder Mawas works in under a [`MawasFocus`] (`SessionsStore::mawas_folder`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MawasFolder {
    /// The workspace, project or Home the folder belongs to: the container new terminals join.
    pub container: WorkspaceId,
    /// The folder is the Home folder (Global scope).
    pub home: bool,
    pub root: PathBuf,
}

/// The container a scoped panel is about (`docs/design.md` § The active container): its name,
/// kind and, for a repository or one of its worktrees, the branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeLabel {
    pub name: String,
    pub kind: ScopeKind,
    pub branch: Option<String>,
    /// Who works in this project or worktree right now (`Codex working`, `Codex and 1 more
    /// working`); the chip shows a dot for it. `None` for a container that is not a checkout,
    /// or when no agent runs a turn there.
    pub working: Option<String>,
}

/// What changed in the [`SessionsStore`], emitted with every notification so that a subscriber
/// can ignore the ones that are not its business (a sidebar that lists sessions does not care
/// that the focus moved to another tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoreChanged(u16);

impl StoreChanged {
    /// The connection state or the daemon epoch.
    pub const CONNECTION: Self = Self(1);
    /// A session appeared, went away or moved in the list.
    pub const SESSIONS: Self = Self(1 << 1);
    /// A session's metadata (title, state, placement, agent report …).
    pub const SESSION_INFO: Self = Self(1 << 2);
    /// Workspaces and projects.
    pub const WORKSPACES: Self = Self(1 << 3);
    /// Approvals, questions and the agent trees.
    pub const AGENTS: Self = Self(1 << 4);
    /// Which sessions have something unread.
    pub const ATTENTION: Self = Self(1 << 5);
    /// The daemon's tasks.
    pub const TASKS: Self = Self(1 << 6);
    /// The workspace and project the window shows.
    pub const ACTIVE: Self = Self(1 << 7);
    /// The session in the focused tab.
    pub const FOCUS: Self = Self(1 << 8);
    /// Answers given here, remembered choices and flags nothing lists.
    pub const OTHER: Self = Self(1 << 9);
    /// Which terminals run an agent that never reported.
    pub const UNTRACKED: Self = Self(1 << 10);
    /// The checkouts (main checkout and worktrees) of the repositories, as `worktree.list` says.
    pub const CHECKOUTS: Self = Self(1 << 11);
    /// Everything a list of the sessions and workspaces is made of.
    pub const LISTED: Self = Self(
        Self::CONNECTION.0
            | Self::SESSIONS.0
            | Self::SESSION_INFO.0
            | Self::WORKSPACES.0
            | Self::AGENTS.0
            | Self::ATTENTION.0
            | Self::TASKS.0
            | Self::ACTIVE.0
            | Self::CHECKOUTS.0,
    );
    /// Everything the agent presentation is made of (the focus and the active workspace are
    /// not): a surface that shows agents rebuilds on these and on nothing else.
    pub const PRESENTED: Self = Self(
        Self::CONNECTION.0
            | Self::SESSIONS.0
            | Self::SESSION_INFO.0
            | Self::WORKSPACES.0
            | Self::AGENTS.0
            | Self::ATTENTION.0
            | Self::TASKS.0
            | Self::OTHER.0,
    );

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// What differs between two states of the daemon.
    fn between(before: &StoreState, after: &StoreState) -> Self {
        let mut changed = Self::default();
        if before.connection != after.connection || before.epoch != after.epoch {
            changed |= Self::CONNECTION;
        }
        if before.workspaces != after.workspaces {
            changed |= Self::WORKSPACES;
        }
        if before.sessions.len() != after.sessions.len()
            || before
                .sessions
                .iter()
                .zip(&after.sessions)
                .any(|(a, b)| a.id != b.id)
        {
            changed |= Self::SESSIONS | Self::SESSION_INFO;
        } else if before.sessions != after.sessions {
            changed |= Self::SESSION_INFO;
        }
        if before.approvals != after.approvals
            || before.questions != after.questions
            || before.trees != after.trees
        {
            changed |= Self::AGENTS;
        }
        changed
    }
}

impl std::ops::BitOr for StoreChanged {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for StoreChanged {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

/// Emitted by [`SessionsStore`] when the daemon asks this window to show a session (the
/// "Open" action of a notification intent, `ui.open`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiOpenRequested {
    pub session_id: SessionId,
    pub workspace_id: Option<WorkspaceId>,
}

/// Emitted by [`SessionsStore`] when a session was closed (`session.closed`), by this window
/// or by another client: its tabs go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionClosed {
    pub session_id: SessionId,
}

/// Emitted by [`SessionsStore`] when a request (an approval, a question, a blocked agent) calls
/// for a session this window is not looking at (`notify.intent`, seen fresh): the workbench
/// decides whether a toast says so.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestCalled {
    pub intent: Box<NotificationIntent>,
}

/// What happened in a session that may have changed files of the checkout it works in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityCause {
    /// An agent's turn ended (a chat turn, or a terminal agent that reported it stopped), or the
    /// agent's session ended.
    TurnEnded,
    /// A command finished in a terminal (`terminal.block`, `OSC 133;D`).
    CommandFinished,
}

/// Emitted by [`SessionsStore`] when something happened that may have changed the files of the
/// checkout a session works in: Changes views and the Changes panel read the checkout again
/// (`UX-CHANGES-FRESH`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutActivity {
    pub session_id: SessionId,
    pub cause: ActivityCause,
    /// Where the session works: its checkout, the folder it is in and, for a finished command,
    /// the directory the shell reported for it.
    pub places: Vec<PathBuf>,
}

impl CheckoutActivity {
    /// Whether the activity happened in (or below) the checkout at `path` (or its canonical
    /// spelling).
    pub fn touches(&self, path: &std::path::Path, canonical: &std::path::Path) -> bool {
        self.places
            .iter()
            .any(|place| place.starts_with(path) || place.starts_with(canonical))
    }
}

/// Whether an agent works in `info` right now: a chat turn is in progress or waits for an answer,
/// or a terminal agent reported that it works or is blocked.
fn agent_busy(info: &SessionInfo) -> bool {
    use hutan_core::SessionState;
    use hutan_protocol::messages::AgentState;
    if info.chat.is_some() {
        return matches!(
            info.state,
            SessionState::Running | SessionState::WaitingApproval
        );
    }
    info.agent
        .as_ref()
        .is_some_and(|report| report.state != AgentState::Idle)
        && matches!(
            info.state,
            SessionState::Running | SessionState::Idle | SessionState::WaitingApproval
        )
}

/// The places a session works in, for [`CheckoutActivity`].
fn session_places(info: &SessionInfo) -> Vec<PathBuf> {
    let mut places: Vec<PathBuf> = Vec::new();
    for place in [
        info.placement.as_ref().map(|p| p.root.clone()),
        info.folder.clone(),
        info.cwd.clone(),
    ]
    .into_iter()
    .flatten()
    {
        if !places.contains(&place) {
            places.push(place);
        }
    }
    places
}

/// Why `session.close` did not close the session.
#[derive(Debug, Clone, PartialEq)]
pub enum CloseFailure {
    /// Something runs in it; closing needs `stop_running`.
    Busy(hutan_protocol::messages::SessionBusy),
    Failed(String),
}

/// Emitted by [`SessionsStore`] when a harness's login or logout session ended
/// (`account.changed`): its accounts may have changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountsChanged {
    pub harness: String,
}

/// Emitted by [`SessionsStore`] when the daemon says usage moved (`usage.changed`, protocol
/// 0.45): the whole summary as it is now.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSummaryChanged {
    pub summary: Box<hutan_protocol::usage::UsageSummary>,
}

/// Emitted by [`SessionsStore`] when Mawas's memory changed or a proposal arrived or was answered
/// (`memory.changed`, protocol 0.32): whoever lists memory asks again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryChanged {
    pub scope_revision: u64,
}

/// Emitted by [`SessionsStore`] when the summary of what a daemon restart did to the sessions
/// should be put in front of the user: once for each restart this window gets to know about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoverySummaryDue {
    /// When the daemon that restored the sessions started; identifies the restart.
    pub restarted_at_ms: u64,
}

/// Whether a `ui.open` sent at `at_ms` is still current at `now_ms` (both Unix milliseconds on
/// this machine; a stamp from the future counts as current).
fn ui_open_is_fresh(at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(at_ms) <= UI_OPEN_MAX_AGE_MS
}

/// The text of the newest user message in the chat's journal.
async fn last_user_message(connection: &Connection, session: SessionId) -> Result<String, String> {
    use hutan_core::AgentEventKind;
    use hutan_core::agent::MessageRole;
    let mut before = None;
    // The newest turns are at the end; a long answer is a few events, so a few pages are plenty.
    for _ in 0..5 {
        let page = connection
            .chat_history(session, before, hutan_protocol::chat::HISTORY_DEFAULT_LIMIT)
            .await
            .map_err(|e| e.to_string())?;
        let found = page
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                AgentEventKind::MessageEnd {
                    role: MessageRole::User,
                    text,
                    ..
                } if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            });
        if let Some(text) = found {
            return Ok(text);
        }
        match page.events.first() {
            Some(first) if page.has_more => before = Some(first.seq),
            _ => break,
        }
    }
    Err("There is no message of yours in this chat to send again".to_owned())
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// From this many sessions on, restart chips are icons and the restart summary is one row.
pub const LARGE_SIDEBAR: usize = 30;

pub struct SessionsStore {
    connection: Connection,
    state: Arc<StoreState>,
    attention: AttentionTracker,
    focused: Option<SessionId>,
    /// The session that had focus most recently; the inspector keeps showing it while a dock or
    /// panel has focus (clicking inside the inspector must not empty it).
    last_focused: Option<SessionId>,
    /// The container the workbench shows (its layout, settings and new sessions): a workspace,
    /// a project in no workspace, or Home.
    active_workspace: Option<WorkspaceId>,
    /// The project within it the user is working in (the one of the focused session, or the one
    /// they chose): its settings layer applies and new sessions start in it.
    active_project: Option<WorkspaceId>,
    /// The checkouts of every repository (`worktree.list` of all workspaces): the sidebar's
    /// branch labels and worktree rows. Empty while unknown or on a daemon without worktrees.
    checkouts: Vec<WorktreeInfo>,
    checkouts_loaded: bool,
    checkouts_in_flight: bool,
    /// A listing asked for while one was in flight: one more follows it.
    checkouts_again: bool,
    /// How many files each dirty checkout has uncommitted (`worktree.diff`, the file list only),
    /// for the sidebar's badges. Read after a checkout listing, never while drawing; a checkout
    /// that is clean or could not be counted has no entry.
    changed_files: HashMap<PathBuf, usize>,
    changed_in_flight: bool,
    /// Worktree roots a session worked in that a listing was asked for (never asked again for
    /// the same root: a root the listing does not know must not loop).
    checkouts_tried: HashSet<PathBuf>,
    /// The worktree the user picked inside a project (the sidebar's worktree row): Files and
    /// Search follow it. Kept per project for when the project comes back.
    chosen_checkout: HashMap<WorkspaceId, PathBuf>,
    terminals: HashMap<SessionId, WeakEntity<TerminalSession>>,
    /// Sessions this desktop created or changed whose events have not reached `state` yet.
    recent: HashMap<SessionId, SessionInfo>,
    colors: Option<DefaultColors>,
    connected_since: Option<Instant>,
    /// The user stopped the daemon from the app; attached terminals say so.
    daemon_stopped: bool,
    /// The daemon epoch the connection was on when the stop was asked (`None`: not connected
    /// then). A daemon of another epoch connecting means it was started again, by any path.
    stopped_epoch: Option<Epoch>,
    /// Permission requests this window answered, and with which option (the cards say
    /// "answered here" for them, "answered elsewhere" for the rest).
    answered: HashMap<ApprovalId, String>,
    /// Questions this window answered (to say "from this window").
    answered_questions: std::collections::HashSet<hutan_core::QuestionId>,
    /// The blocked nodes of each session as of the last synchronised state, once the first
    /// state of the daemon epoch has been taken as the baseline (what was already blocked when
    /// the desktop arrived is not news).
    blocked_seen: Option<HashMap<SessionId, BTreeSet<String>>>,
    blocked_epoch: Option<Epoch>,
    /// The focus the daemon last acknowledged for this connection (`client.focus`); `None`
    /// when nothing was reported on the current connection.
    /// With the [`Connection::generation`] it was acknowledged on.
    focus_sent: Option<(u64, Option<SessionId>)>,
    /// The `client.focus` request in flight; at most one, so reports cannot overtake each other.
    /// With the generation it was started on: a request that outlives its connection is
    /// cancelled, not waited for.
    focus_task: Option<(u64, Task<()>)>,
    /// The session of the Assistant panel while the panel has the focus.
    assistant_focused: Option<SessionId>,
    /// Mawas Home is the focused center tab, showing this scope: Files, Search and Problems
    /// follow it instead of the active container.
    mawas_focus: Option<MawasFocus>,
    /// The session marked unread while it was on screen: reading it is held off until the focus
    /// has been elsewhere.
    hold_unread: Option<SessionId>,
    /// The wait for review the user has seen, by session ([`TaskBook::review_key`]): the
    /// chat or its Changes view was looked at after the work became `awaiting_review`. A seen
    /// wait no longer needs the user: the session says its state with a *Changes to review* link.
    reviews_seen: HashMap<SessionId, u64>,
    /// The notification intents raised for each session (`notify.intent`) since it was last
    /// read: *Mark as read* acknowledges them, so their banners leave the notification centre.
    /// At most [`MAX_SURFACED`] ids; the daemon forgets intents too.
    surfaced: HashMap<SessionId, Vec<String>>,
    /// The sessions a request called for while this window was not looking at them, until the
    /// user looks (`is_called`, the dot on the tab); `true` once the state showed the request.
    called: HashMap<SessionId, bool>,
    /// Terminals in which a known agent program runs that never reported, as the last process
    /// listing showed (`UX-ATTENTION-CONTROLS`); see [`SessionsStore::untracked_agent`].
    untracked: HashMap<SessionId, Untracked>,
    /// The `resources.list` in flight; one at a time, a trigger that arrives meanwhile asks for
    /// one more afterwards.
    scan_task: Option<Task<()>>,
    scan_again: bool,
    /// The restart whose summary was already put up (`restarted_at_ms`): it is not shown again,
    /// whatever the user did with it.
    recovery_shown: Option<u64>,
    /// Sessions this window is reopening (Undo of a close, History's Reopen): the daemon gives a
    /// reopened session a recovery of its own, which says nothing the user does not know.
    reopening: HashSet<SessionId>,
    /// The daemon's tasks (`task.list`, then `task.updated` / `task.removed`).
    tasks: TaskBook,
    /// The `task.list` in flight; dropped (cancelled) with the connection.
    tasks_loading: Option<Task<()>>,
    /// The places (`None`: Global) whose assistant backend was chosen with `assistant.configure`,
    /// as far as this window knows: the daemon cannot say what is stored.
    assistant_pinned: std::collections::HashSet<Option<WorkspaceId>>,
    /// Whether `daemon.toml` names the assistants' backend or provider; `None` until asked.
    assistant_file_choice: Option<bool>,
    /// Counts what the agent presentation is made of changing; the presentation is rebuilt
    /// (once) the first time somebody reads it after a change.
    revision: u64,
    agent_cache: RefCell<Option<Arc<AgentPresentation>>>,
    presentation_builds: Cell<usize>,
    /// When the daemon's state was last seen confirmed (connected), for "Offline · last
    /// confirmed …".
    last_confirmed_ms: Option<u64>,
    /// Folders watched (`fs.watch`) for the Files panel and the ⌘P finder, with how many of
    /// them want each: the connection keeps one wish per path, so the last one to let go
    /// unwatches it.
    dir_watches: HashMap<PathBuf, usize>,
    /// The checkpoints of agent chats (`checkpoints.rs`): turn footers, the Checkpoints list.
    checkpoints: Entity<super::checkpoints::CheckpointStore>,
    /// The display names of the harnesses' models, from every catalog this desktop was given,
    /// kept in `model_names_path` between runs.
    model_names: ModelNames,
    model_names_path: Option<PathBuf>,
    /// The last save of the model names; the next one waits for it.
    model_names_save: Option<Task<()>>,
    /// Agents whose `chat.models` this run asked for because a chat showed a model without a
    /// name: asked once per agent per run, never per row.
    models_asked: std::collections::HashSet<String>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<StoreChanged> for SessionsStore {}
impl EventEmitter<AgentAttentionChanged> for SessionsStore {}
impl EventEmitter<UiOpenRequested> for SessionsStore {}
impl EventEmitter<AccountsChanged> for SessionsStore {}
impl EventEmitter<UsageSummaryChanged> for SessionsStore {}
impl EventEmitter<MemoryChanged> for SessionsStore {}
impl EventEmitter<SessionClosed> for SessionsStore {}
impl EventEmitter<CheckoutActivity> for SessionsStore {}
impl EventEmitter<RecoverySummaryDue> for SessionsStore {}
impl EventEmitter<RequestCalled> for SessionsStore {}

impl std::fmt::Debug for SessionsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionsStore")
            .field("connection", &self.state.connection)
            .field("sessions", &self.state.sessions.len())
            .finish_non_exhaustive()
    }
}

impl SessionsStore {
    pub fn new(connection: Connection, cx: &mut Context<Self>) -> Self {
        let mut state = connection.state();
        let mut notices = connection.notices();
        let state_task = cx.spawn(async move |this, cx| {
            loop {
                let latest = state.borrow_and_update().clone();
                if this
                    .update(cx, |this, cx| this.apply_state(latest, cx))
                    .is_err()
                {
                    break;
                }
                if state.changed().await.is_err() {
                    break;
                }
            }
        });
        let notice_task = cx.spawn(async move |this, cx| {
            loop {
                match notices.recv().await {
                    Ok(notice) => {
                        if this
                            .update(cx, |this, cx| this.apply_notice(&notice, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(RecvError::Lagged(missed)) => {
                        tracing::debug!("missed {missed} daemon notices");
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
        let checkpoints =
            cx.new(|cx| super::checkpoints::CheckpointStore::new(connection.clone(), cx));
        Self {
            connection,
            state: Arc::new(StoreState {
                connection: ConnectionState::Connecting,
                epoch: None,
                workspaces: Vec::new(),
                sessions: Vec::new(),
                approvals: Vec::new(),
                questions: Vec::new(),
                trees: Default::default(),
                daemon_start: None,
                autostart_halted: false,
                daemon: None,
                stopped_by: None,
            }),
            attention: AttentionTracker::new(),
            focused: None,
            last_focused: None,
            active_workspace: None,
            active_project: None,
            checkouts: Vec::new(),
            checkouts_loaded: false,
            checkouts_in_flight: false,
            checkouts_again: false,
            changed_files: HashMap::new(),
            changed_in_flight: false,
            checkouts_tried: HashSet::new(),
            chosen_checkout: HashMap::new(),
            terminals: HashMap::new(),
            recent: HashMap::new(),
            colors: None,
            connected_since: None,
            daemon_stopped: false,
            stopped_epoch: None,
            answered: HashMap::new(),
            answered_questions: std::collections::HashSet::new(),
            blocked_seen: None,
            blocked_epoch: None,
            focus_sent: None,
            focus_task: None,
            assistant_focused: None,
            mawas_focus: None,
            hold_unread: None,
            reviews_seen: HashMap::new(),
            surfaced: HashMap::new(),
            called: HashMap::new(),
            untracked: HashMap::new(),
            scan_task: None,
            scan_again: false,
            recovery_shown: None,
            reopening: HashSet::new(),
            tasks: TaskBook::default(),
            tasks_loading: None,
            assistant_pinned: std::collections::HashSet::new(),
            assistant_file_choice: None,
            revision: 0,
            agent_cache: RefCell::new(None),
            presentation_builds: Cell::new(0),
            last_confirmed_ms: None,
            dir_watches: HashMap::new(),
            checkpoints,
            model_names: ModelNames::default(),
            model_names_path: None,
            model_names_save: None,
            models_asked: std::collections::HashSet::new(),
            _tasks: vec![state_task, notice_task],
        }
    }

    fn apply_state(&mut self, latest: Arc<StoreState>, cx: &mut Context<Self>) {
        // The daemon's watch publishes after every event, whether or not it changed what this
        // window shows: nothing to do (and nobody to wake) for the same state again.
        if Arc::ptr_eq(&self.state, &latest) || *self.state == *latest {
            return;
        }
        let mut changed = StoreChanged::between(&self.state, &latest);
        let was_connected = self.state.connection == ConnectionState::Connected;
        let connected = latest.connection == ConnectionState::Connected;
        if connected && !was_connected {
            self.connected_since = Some(Instant::now());
            hutan_terminal_view::trace::startup("connected");
        } else if !connected {
            // Notices that arrive while (re)connecting replay what happened meanwhile.
            self.connected_since = None;
            self.tasks_loading = None;
        }
        let before = std::mem::replace(&mut self.state, latest);
        self.revision += 1;
        if connected {
            self.last_confirmed_ms = Some(unix_ms());
        }
        if connected {
            self.fetch_missing_model_names(cx);
        }
        // A daemon of a new epoch is up: whoever started it (the CLI, autostart, another
        // client), the stop the user asked for is over.
        if connected && self.daemon_stopped && self.state.epoch != self.stopped_epoch {
            self.set_daemon_stopped(false, cx);
        }
        if connected && !was_connected {
            self.load_tasks(cx);
            self.scan_agents(cx);
        }
        if connected {
            let unread = self.attention.unread_map().clone();
            let snoozes = self.attention.snoozed_map().clone();
            self.attention
                .observe(self.state.epoch, &self.state.sessions);
            self.lapse_snoozes();
            if *self.attention.unread_map() != unread || *self.attention.snoozed_map() != snoozes {
                changed |= StoreChanged::ATTENTION;
            }
            self.observe_blocked_nodes(cx);
            if self.observe_calls() {
                changed |= StoreChanged::ATTENTION;
            }
            self.observe_recovery(cx);
            if was_connected && before.epoch == self.state.epoch {
                self.observe_turn_ends(&before, cx);
            }
            if changed.intersects(StoreChanged::SESSIONS) {
                self.scan_agents(cx);
            }
            let known: Vec<SessionId> = self.state.sessions.iter().map(|s| s.id).collect();
            self.recent.retain(|id, _| !known.contains(id));
        }
        // Tell every attached terminal what the daemon now says about its session. While
        // disconnected the last known state stays (a reconnect resyncs it). Terminals of
        // sessions that did not change have nothing to hear.
        if connected
            && changed.intersects(
                StoreChanged::CONNECTION | StoreChanged::SESSIONS | StoreChanged::SESSION_INFO,
            )
        {
            let live: Vec<(SessionId, WeakEntity<TerminalSession>)> = self
                .terminals
                .iter()
                .map(|(id, t)| (*id, t.clone()))
                .collect();
            let epoch_moved = before.epoch != self.state.epoch || !was_connected;
            for (id, terminal) in live {
                let info = self.session(id).cloned();
                let heard = epoch_moved
                    || before
                        .sessions
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| s as &SessionInfo)
                        != info.as_ref();
                if !heard {
                    continue;
                }
                match terminal.upgrade() {
                    Some(terminal) => {
                        let epoch = self.state.epoch;
                        terminal.update(cx, |terminal, cx| terminal.update_info(info, epoch, cx));
                    }
                    None => {
                        self.terminals.remove(&id);
                    }
                }
            }
        }
        // The daemon forgets a connection's focus with the connection: say it again on the new
        // one.
        if connected {
            self.report_focus(cx);
        } else {
            self.focus_sent = None;
            // The request, if any, was for the connection that is gone.
            self.focus_task = None;
        }
        self.changed(changed, cx);
    }

    /// Wakes the observers and tells the subscribers what changed; nothing when nothing did.
    fn changed(&mut self, what: StoreChanged, cx: &mut Context<Self>) {
        if what.intersects(StoreChanged::PRESENTED) {
            self.revision += 1;
        }
        if what.is_empty() {
            return;
        }
        cx.emit(what);
        cx.notify();
        if what.intersects(
            StoreChanged::CONNECTION
                | StoreChanged::SESSIONS
                | StoreChanged::SESSION_INFO
                | StoreChanged::WORKSPACES,
        ) {
            self.follow_checkouts(cx);
        }
    }

    /// Emits [`RecoverySummaryDue`] once per restart that left something unacknowledged.
    /// A window that opens after the restart is told too: what a restart did is news until it is
    /// acknowledged, wherever the user was.
    fn observe_recovery(&mut self, cx: &mut Context<Self>) {
        let Some(summary) = self.recovery_summary() else {
            return;
        };
        if self.recovery_shown == Some(summary.restarted_at_ms) {
            return;
        }
        self.recovery_shown = Some(summary.restarted_at_ms);
        cx.emit(RecoverySummaryDue {
            restarted_at_ms: summary.restarted_at_ms,
        });
    }

    /// What the daemon restart did to the sessions that are not acknowledged yet; `None` when
    /// there is nothing to say.
    pub fn recovery_summary(&self) -> Option<Summary> {
        if self.reopening.is_empty() {
            return recovery::summary(&self.state.sessions, relative_to_now);
        }
        let others: Vec<SessionInfo> = self
            .state
            .sessions
            .iter()
            .filter(|s| !self.reopening.contains(&s.id))
            .cloned()
            .collect();
        recovery::summary(&others, relative_to_now)
    }

    /// `session.recovery_ack` for one session, or for all of them.
    pub fn ack_recovery(
        &self,
        session: Option<SessionId>,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<SessionId>, String>> {
        self.call(
            Request::SessionRecoveryAck(RecoveryAck {
                session_id: session,
            }),
            |acked: RecoveryAcked| acked.sessions,
            cx,
        )
    }

    /// Sends the last message the user wrote in the chat again: what "Retry last message" of a
    /// turn the restart interrupted and *Retry now* of a failed turn do. With `chat.retry`
    /// (protocol 0.34) the daemon finds the message and does not journal it a second time; an
    /// older daemon gets it from the journal here, as a new `chat.send`.
    pub fn retry_last_message(
        &self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let connection = self.connection.clone();
        cx.background_spawn(async move {
            if connection.supports_chat_retry() {
                return connection
                    .chat_retry(session)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string());
            }
            let text = last_user_message(&connection, session).await?;
            connection
                .chat_send_with(session, text, Vec::new())
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }

    /// Emits [`CheckoutActivity`] for every session whose agent stopped working since `before`.
    fn observe_turn_ends(&self, before: &StoreState, cx: &mut Context<Self>) {
        for old in &before.sessions {
            if !agent_busy(old) {
                continue;
            }
            let Some(now) = self.state.sessions.iter().find(|s| s.id == old.id) else {
                continue;
            };
            if !agent_busy(now) {
                cx.emit(CheckoutActivity {
                    session_id: now.id,
                    cause: ActivityCause::TurnEnded,
                    places: session_places(now),
                });
            }
        }
    }

    /// Emits [`AgentAttentionChanged`] for every session in which a node became blocked since
    /// the last state. The first state of an epoch is the baseline.
    fn observe_blocked_nodes(&mut self, cx: &mut Context<Self>) {
        if self.blocked_epoch != self.state.epoch {
            self.blocked_epoch = self.state.epoch;
            self.blocked_seen = None;
        }
        let now: HashMap<SessionId, Vec<String>> = self
            .state
            .trees
            .iter()
            .filter_map(|(session, tree)| {
                let blocked: Vec<String> = agents::blocked_nodes(tree)
                    .into_iter()
                    .map(|n| n.node_id)
                    .collect();
                (!blocked.is_empty()).then_some((*session, blocked))
            })
            .collect();
        if let Some(before) = &self.blocked_seen {
            let mut sessions: Vec<SessionId> = now.keys().copied().collect();
            sessions.sort();
            for session in sessions {
                let blocked = &now[&session];
                let known = before.get(&session);
                if blocked
                    .iter()
                    .any(|id| !known.is_some_and(|k| k.contains(id)))
                {
                    cx.emit(AgentAttentionChanged {
                        session_id: session,
                        blocked_nodes: blocked.clone(),
                    });
                }
            }
        }
        self.blocked_seen = Some(
            now.into_iter()
                .map(|(session, ids)| (session, ids.into_iter().collect()))
                .collect(),
        );
    }

    fn apply_notice(&mut self, notice: &Notice, cx: &mut Context<Self>) {
        match notice {
            Notice::TerminalNotification { terminal, .. } => {
                // Right after (re)connecting the daemon replays what was missed; that is history,
                // not news. Before the connection is seen as up there is nothing to attribute.
                let replay = self
                    .connected_since
                    .is_none_or(|since| since.elapsed() < REPLAY_WINDOW);
                if replay {
                    return;
                }
                if let Some(session) = self.session_of(*terminal) {
                    self.attention.notified(session);
                    self.changed(StoreChanged::ATTENTION, cx);
                }
            }
            // Blocks go to the attachment that shows the terminal, if there is one.
            Notice::TerminalBlock { terminal, block } => {
                // A command that starts an agent: look at the process tree for one that never
                // reported.
                if block.state == hutan_protocol::blocks::BlockState::Running
                    && block.command.as_deref().is_some_and(untracked::is_agent_command)
                {
                    self.scan_agents(cx);
                }
                if block.state == hutan_protocol::blocks::BlockState::Finished
                    && let Some(info) = self.state.sessions.iter().find(|s| s.terminal_id == Some(*terminal))
                {
                    let mut places = session_places(info);
                    if let Some(cwd) = &block.cwd
                        && !places.contains(cwd)
                    {
                        places.push(cwd.clone());
                    }
                    cx.emit(CheckoutActivity {
                        session_id: info.id,
                        cause: ActivityCause::CommandFinished,
                        places,
                    });
                }
                let attached: Vec<_> = self
                    .terminals
                    .values()
                    .filter_map(WeakEntity::upgrade)
                    .collect();
                for session in attached {
                    session.update(cx, |session, cx| {
                        session.apply_block_event(*terminal, block.clone(), cx);
                    });
                }
            }
            // A notification's "Open" (`ui.open`): this window shows the session.
            Notice::UiOpen {
                session,
                workspace,
                at_ms,
            } => {
                if ui_open_is_fresh(*at_ms, unix_ms()) {
                    cx.emit(UiOpenRequested {
                        session_id: *session,
                        workspace_id: *workspace,
                    });
                }
            }
            // A session closed by any client: the window drops its tabs.
            Notice::SessionClosed { session } => cx.emit(SessionClosed {
                session_id: *session,
            }),
            // The desktop shows approvals and agent state from its own state; it only notes the
            // intents (M6-B notifiers and `hutan notify watch` show them) so that reading the
            // session acknowledges them.
            Notice::NotifyIntent { intent } => self.note_intent(intent, cx),
            // A worktree was made or removed: the sidebar's rows list again.
            Notice::WorktreesChanged { .. } => self.refresh_checkouts(cx),
            Notice::SessionExited { .. }
            | Notice::Bell { .. }
            | Notice::DaemonRestarted { .. }
            // `workbench/remote.rs` follows `Connection::notices` itself.
            | Notice::UiCommand { .. }
            // The buffer store and the Files panel follow the file changes themselves
            // (`workbench/file_sync.rs`, `panels/files.rs`), the ⌘P finder the generations.
            | Notice::FsChanged { .. }
            | Notice::FsGeneration { .. }
            // The Search panel follows its own searches (`panels/search.rs`).
            | Notice::FsSearchProgress { .. }
            // The file tabs' language servers (`workbench/lsp.rs`) and the Problems panel
            // follow these themselves.
            | Notice::LspState { .. }
            | Notice::LspDiagnostics { .. } => {}
            // The resources panel listens for itself too; the sample also says which terminals
            // run an agent that never reported.
            Notice::ResourcesUpdated { sample } => self.apply_sample(sample, cx),
            Notice::TaskUpdated { task } => {
                self.tasks.updated((**task).clone());
                let lapsed = self.lapse_snoozes();
                self.changed(
                    if lapsed {
                        StoreChanged::TASKS | StoreChanged::ATTENTION
                    } else {
                        StoreChanged::TASKS
                    },
                    cx,
                );
            }
            Notice::TaskRemoved { id } => {
                self.tasks.removed(*id);
                let lapsed = self.lapse_snoozes();
                self.changed(
                    if lapsed {
                        StoreChanged::TASKS | StoreChanged::ATTENTION
                    } else {
                        StoreChanged::TASKS
                    },
                    cx,
                );
            }
            // A login or logout session of a harness ended: whoever lists the accounts asks
            // again.
            Notice::AccountsChanged { harness } => cx.emit(AccountsChanged {
                harness: harness.clone(),
            }),
            // The usage model of the window folds it in (status item, Agents page, toasts).
            Notice::UsageChanged { summary } => cx.emit(UsageSummaryChanged {
                summary: summary.clone(),
            }),
            // The Memory page listens for itself and lists again when it is on screen.
            Notice::MemoryChanged { scope_revision, .. } => cx.emit(MemoryChanged {
                scope_revision: *scope_revision,
            }),
        }
    }

    // ---- read access -----------------------------------------------------------------------

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// The checkpoints of agent chats as this window knows them.
    pub fn checkpoints(&self) -> &Entity<super::checkpoints::CheckpointStore> {
        &self.checkpoints
    }

    pub fn state(&self) -> &StoreState {
        &self.state
    }

    pub fn connection_state(&self) -> ConnectionState {
        self.state.connection
    }

    /// Whether the first synchronisation with the daemon has completed.
    pub fn synced(&self) -> bool {
        self.state.epoch.is_some()
    }

    pub fn sessions(&self) -> &[SessionInfo] {
        &self.state.sessions
    }

    /// Whether the sidebar lists so many sessions that restart chips and the restart summary
    /// shrink to keep the rows readable (`LARGE_SIDEBAR`).
    pub fn large_sidebar(&self) -> bool {
        self.state.sessions.len() >= LARGE_SIDEBAR
    }

    pub fn workspaces(&self) -> &[WorkspaceInfo] {
        &self.state.workspaces
    }

    pub fn session(&self, id: SessionId) -> Option<&SessionInfo> {
        self.state
            .sessions
            .iter()
            .find(|s| s.id == id)
            .or_else(|| self.recent.get(&id))
    }

    pub fn workspace(&self, id: WorkspaceId) -> Option<&WorkspaceInfo> {
        self.state.workspaces.iter().find(|w| w.id == id)
    }

    /// The Home workspace (`kind == home`); the daemon keeps exactly one.
    pub fn home_workspace(&self) -> Option<&WorkspaceInfo> {
        self.state
            .workspaces
            .iter()
            .find(|w| w.kind == WorkspaceKind::Home)
    }

    /// The container the workbench shows, when the daemon knows it.
    pub fn active_workspace(&self) -> Option<&WorkspaceInfo> {
        self.active_workspace.and_then(|id| self.workspace(id))
    }

    /// The id of the container the workbench shows, known or not.
    pub fn active_workspace_id(&self) -> Option<WorkspaceId> {
        self.active_workspace
    }

    /// The project the user is working in, when the daemon knows it.
    pub fn active_project(&self) -> Option<&WorkspaceInfo> {
        self.active_project.and_then(|id| self.workspace(id))
    }

    /// The id of the project the user is working in, known or not.
    pub fn active_project_id(&self) -> Option<WorkspaceId> {
        self.active_project
    }

    /// Tells the panels which container and project the workbench shows.
    pub fn set_active(
        &mut self,
        workspace: Option<WorkspaceId>,
        project: Option<WorkspaceId>,
        cx: &mut Context<Self>,
    ) {
        if self.active_workspace != workspace || self.active_project != project {
            self.active_workspace = workspace;
            self.active_project = project;
            self.changed(StoreChanged::ACTIVE, cx);
        }
    }

    /// Mawas Home is the focused center tab with this scope, or no longer is (`None`).
    pub fn set_mawas_focus(&mut self, focus: Option<MawasFocus>, cx: &mut Context<Self>) {
        if self.mawas_focus != focus {
            self.mawas_focus = focus;
            // Not `ACTIVE`: the window's layout does not follow it, only the panels do.
            self.changed(StoreChanged::OTHER, cx);
        }
    }

    pub fn mawas_focus(&self) -> Option<MawasFocus> {
        self.mawas_focus
    }

    /// The container Mawas's scope names while Mawas Home is the focused center tab: the Home
    /// workspace for Global, else the workspace or project of the scope.
    pub fn mawas_container(&self) -> Option<&WorkspaceInfo> {
        match self.mawas_focus? {
            MawasFocus::Global => self.home_workspace(),
            MawasFocus::Workspace(id) => self.workspace(id),
        }
    }

    /// The container Files, Search and Problems are about: Mawas's scope while Mawas Home is
    /// the focused center tab, else the container the window shows.
    pub fn scope_container(&self) -> Option<&WorkspaceInfo> {
        if self.mawas_focus.is_some() {
            return self.mawas_container();
        }
        self.active_workspace()
    }

    /// The id of [`scope_container`](Self::scope_container), known or not.
    pub fn scope_container_id(&self) -> Option<WorkspaceId> {
        match self.mawas_focus {
            Some(MawasFocus::Global) => self.home_workspace().map(|w| w.id),
            Some(MawasFocus::Workspace(id)) => Some(id),
            None => self.active_workspace,
        }
    }

    /// The folder the Files panel and ⌘P's files show: Mawas's folder while Mawas Home is the
    /// focused center tab (Global: the Home folder; a workspace: its active project), else the
    /// root of the project the user works in (the active project, else the first project of the
    /// active workspace, else the workspace's own first folder); at Home, the Home folder.
    /// `None` before the daemon says what is active.
    pub fn files_root(&self) -> Option<PathBuf> {
        if self.mawas_focus.is_some() {
            return self
                .mawas_container()
                .and_then(|container| self.root_in(container));
        }
        if let Some(checkout) = self.active_checkout() {
            return Some(checkout);
        }
        self.root_in(self.active_workspace()?)
    }

    /// The folder Mawas works in under `focus`, with the container it belongs to: the Home
    /// folder for Global, else the (active) project of the workspace. `None` while the daemon
    /// has not said what the container is.
    pub fn mawas_folder(&self, focus: MawasFocus) -> Option<MawasFolder> {
        let container = match focus {
            MawasFocus::Global => self.home_workspace()?,
            MawasFocus::Workspace(id) => self.workspace(id)?,
        };
        let root = self.root_in(container)?;
        Some(MawasFolder {
            container: container.id,
            home: container.kind == WorkspaceKind::Home,
            root,
        })
    }

    /// The Home folder (`~/Hutan`), when the daemon has said what Home is.
    pub fn home_folder(&self) -> Option<PathBuf> {
        self.home_workspace()?.roots.first().cloned()
    }

    /// Whether the Home folder is a git repository, as far as the daemon's checkout listing
    /// says (the wire does not mark Home's workspace with a project). `false` until a listing
    /// has been read.
    pub fn home_is_repository(&self) -> bool {
        let Some(home) = self.home_folder() else {
            return false;
        };
        self.checkouts.iter().any(|c| c.repo_root == home)
    }

    /// Whether *Track changes in this folder…* is on offer for the Home folder: the daemon can
    /// do it, the checkouts have been listed, and none of them is the Home folder.
    pub fn can_track_home(&self) -> bool {
        self.state.connection == ConnectionState::Connected
            && self.connection.supports_project_git_init()
            && self.checkouts_loaded
            && self.home_folder().is_some()
            && !self.home_is_repository()
    }

    /// `project.git_init` of the Home workspace's folder. The daemon refuses what must not be
    /// tracked (the user's home directory, a folder inside a repository, a very large folder,
    /// no git identity); the store follows the change through `worktree.changed`.
    pub fn track_home(
        &self,
        cx: &mut Context<Self>,
    ) -> Task<Result<hutan_protocol::project::ProjectGitInitialized, String>> {
        let Some(home) = self.home_workspace().map(|w| w.id) else {
            return Task::ready(Err("Home is not known yet".to_owned()));
        };
        let connection = self.connection.clone();
        cx.background_spawn(async move {
            connection
                .project_git_init(home)
                .await
                .map_err(|e| e.to_string())
        })
    }

    /// The folder ⌘P searches: the root of the project the user works in, whatever Mawas Home
    /// shows; `None` at Home (the jump list alone).
    pub fn project_files_root(&self) -> Option<PathBuf> {
        let container = self.active_workspace()?;
        if container.kind == WorkspaceKind::Home {
            return None;
        }
        self.active_checkout().or_else(|| self.root_in(container))
    }

    /// The folder `container` shows: Home's folder, a project's root, a workspace's active (else
    /// first) project's root. The picked worktree counts only for the container the window
    /// shows.
    fn root_in(&self, container: &WorkspaceInfo) -> Option<PathBuf> {
        let project = match container.kind {
            WorkspaceKind::Home => return container.roots.first().cloned(),
            WorkspaceKind::Project => container,
            WorkspaceKind::Workspace => self
                .active_project()
                .filter(|p| p.parent_id == Some(container.id))
                .or_else(|| self.state.projects_in(container.id).next())
                .unwrap_or(container),
        };
        if self.active_workspace == Some(container.id)
            && let Some(checkout) = self.active_checkout()
        {
            return Some(checkout);
        }
        project
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .or_else(|| project.roots.first().cloned())
    }

    /// The worktree the user picked inside the project they work in (the sidebar's worktree
    /// row), while it is still one of the project's checkouts; `None` is the project's own
    /// folder.
    pub fn active_checkout(&self) -> Option<PathBuf> {
        let project = self.active_project()?;
        let path = self.chosen_checkout.get(&project.id)?;
        self.checkouts_of(project)
            .into_iter()
            .any(|checkout| !checkout.main && checkout.path == *path)
            .then(|| path.clone())
    }

    /// Picks (or, with `None`, leaves) the worktree of `project` the sidebar's scope names.
    pub fn set_active_checkout(
        &mut self,
        project: WorkspaceId,
        path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let changed = match path {
            Some(path) => self.chosen_checkout.insert(project, path.clone()) != Some(path),
            None => self.chosen_checkout.remove(&project).is_some(),
        };
        if changed && self.active_project == Some(project) {
            self.changed(StoreChanged::ACTIVE, cx);
        }
    }

    /// Every checkout of `project`'s repository, main checkout first (what `worktree.list`
    /// said, plus the worktrees its sessions work in that the listing does not know yet, so a
    /// daemon without `worktree.list` still shows the ones in use). Empty for a folder outside
    /// git.
    pub fn checkouts_of(&self, project: &WorkspaceInfo) -> Vec<Checkout> {
        let Some(info) = project.project.as_ref().filter(|p| p.common_dir.is_some()) else {
            return Vec::new();
        };
        let mut out: Vec<Checkout> = self
            .checkouts
            .iter()
            .filter(|c| c.repo_root == info.root && !c.prunable)
            .map(|c| Checkout {
                path: c.path.clone(),
                branch: c.branch.clone(),
                main: c.main,
                dirty: c.dirty,
            })
            .collect();
        for session in self
            .state
            .sessions
            .iter()
            .filter(|s| s.workspace_id == project.id)
        {
            let Some(placement) = session.placement.as_ref() else {
                continue;
            };
            if out.iter().any(|c| c.path == placement.root) {
                continue;
            }
            out.push(Checkout {
                path: placement.root.clone(),
                branch: placement.branch.clone(),
                main: placement.kind != PlacementKind::Worktree,
                dirty: false,
            });
        }
        out.sort_by_key(|c| !c.main);
        out
    }

    /// The branch checked out in `project`'s main checkout, when known.
    pub fn main_branch(&self, project: &WorkspaceInfo) -> Option<String> {
        self.checkouts_of(project)
            .into_iter()
            .find(|c| c.main)
            .and_then(|c| c.branch)
    }

    /// What the panels scoped to the container (Changes, Problems, the Agents rail) are about:
    /// the container the window shows (a workspace, a project, Home).
    pub fn container_scope(&self) -> Option<ScopeLabel> {
        self.active_workspace().map(|w| self.scope_of(w, None))
    }

    /// What the panels scoped to a folder (Files, Search) are about: the worktree the user
    /// picked, else the project they work in, else the container.
    pub fn project_scope(&self) -> Option<ScopeLabel> {
        if self.mawas_focus.is_some() {
            return self.mawas_scope();
        }
        match self.active_project() {
            Some(project) => Some(self.scope_of(project, self.active_checkout())),
            None => self.container_scope(),
        }
    }

    /// What Problems and Changes are about: Mawas's scope while Mawas Home is the focused center
    /// tab, else the container the window shows.
    pub fn followed_scope(&self) -> Option<ScopeLabel> {
        if self.mawas_focus.is_some() {
            return self.mawas_scope();
        }
        self.container_scope()
    }

    /// `Mawas · Global` or `Mawas · <workspace>`; `None` when the scope's container is not
    /// known (yet).
    fn mawas_scope(&self) -> Option<ScopeLabel> {
        let container = self.mawas_container()?;
        let name = match self.mawas_focus? {
            MawasFocus::Global => "Mawas · Global".to_owned(),
            MawasFocus::Workspace(_) => format!("Mawas · {}", container.name),
        };
        Some(ScopeLabel {
            name,
            kind: ScopeKind::Mawas,
            branch: None,
            working: None,
        })
    }

    fn scope_of(&self, info: &WorkspaceInfo, checkout: Option<PathBuf>) -> ScopeLabel {
        let working = (info.kind == WorkspaceKind::Project)
            .then(|| {
                checkout
                    .clone()
                    .or_else(|| info.project.as_ref().map(|p| p.root.clone()))
                    .or_else(|| info.roots.first().cloned())
            })
            .flatten()
            .and_then(|root| self.working_label(&root));
        let repository = info
            .project
            .as_ref()
            .is_some_and(|p| p.common_dir.is_some());
        let (kind, branch) = match (info.kind, checkout) {
            (WorkspaceKind::Home, _) => (ScopeKind::Home, None),
            (WorkspaceKind::Workspace, _) => (ScopeKind::Workspace, None),
            (WorkspaceKind::Project, Some(path)) => (
                ScopeKind::Worktree,
                self.checkouts_of(info)
                    .into_iter()
                    .find(|c| c.path == path)
                    .and_then(|c| c.branch),
            ),
            (WorkspaceKind::Project, None) if repository => {
                (ScopeKind::Repository, self.main_branch(info))
            }
            (WorkspaceKind::Project, None) => (ScopeKind::Folder, None),
        };
        ScopeLabel {
            name: info.name.clone(),
            kind,
            branch,
            working,
        }
    }

    /// `Codex working` for the agents running a turn in the checkout at `root` (the first one's
    /// name; `and N more` for the rest); `None` when none does.
    fn working_label(&self, root: &std::path::Path) -> Option<String> {
        let working = self.agents_working_near(&[root.to_owned()]);
        let (title, agent) = working.first()?;
        let name = match agent {
            Some(agent) => crate::workbench::harnesses::harness_name(agent),
            None if title.is_empty() => "An agent".to_owned(),
            None => title.clone(),
        };
        Some(match working.len() {
            1 => format!("{name} working"),
            n => format!("{name} and {} more working", n - 1),
        })
    }

    /// Asks the daemon for the checkouts of every repository when none were asked for yet, a
    /// session works in a worktree no listing has covered (a worktree made by a command), or a
    /// repository project appeared that no listing has covered (a folder just opened: its
    /// uncommitted files are counted for the badges at once).
    fn follow_checkouts(&mut self, cx: &mut Context<Self>) {
        if self.state.connection != ConnectionState::Connected
            || self.state.epoch.is_none()
            || !self.connection.supports_worktrees()
        {
            return;
        }
        let unseen = self
            .state
            .sessions
            .iter()
            .filter_map(|s| s.placement.as_ref())
            .filter(|p| p.kind == PlacementKind::Worktree)
            .any(|p| {
                !self.checkouts_tried.contains(&p.root)
                    && !self.checkouts.iter().any(|c| c.path == p.root)
            });
        let new_project = self.state.workspaces.iter().any(|w| {
            w.project.as_ref().is_some_and(|p| {
                p.common_dir.is_some()
                    && !self.checkouts_tried.contains(&p.root)
                    && !self.checkouts.iter().any(|c| c.repo_root == p.root)
            })
        });
        if self.checkouts_in_flight {
            // A listing that started before this worktree or project appeared may not have it:
            // one more follows it.
            self.checkouts_again |= unseen || new_project;
            return;
        }
        if self.checkouts_loaded && !unseen && !new_project {
            return;
        }
        self.refresh_checkouts(cx);
    }

    /// Lists the checkouts again (`worktree.list`, all workspaces); a change reaches the
    /// sidebar as [`StoreChanged::CHECKOUTS`]. A call while a listing is in flight asks for one
    /// more afterwards, so a burst of triggers is at most two listings. After every listing the
    /// uncommitted files of the dirty checkouts are counted again (the sidebar's badges).
    pub fn refresh_checkouts(&mut self, cx: &mut Context<Self>) {
        if self.state.connection != ConnectionState::Connected
            || !self.connection.supports_worktrees()
        {
            return;
        }
        if self.checkouts_in_flight {
            self.checkouts_again = true;
            return;
        }
        self.checkouts_in_flight = true;
        self.checkouts_again = false;
        for session in &self.state.sessions {
            if let Some(placement) = &session.placement {
                self.checkouts_tried.insert(placement.root.clone());
            }
        }
        for workspace in &self.state.workspaces {
            if let Some(project) = &workspace.project {
                self.checkouts_tried.insert(project.root.clone());
            }
        }
        let listing = self.list_all_worktrees(cx);
        cx.spawn(async move |this, cx| {
            let result = listing.await;
            this.update(cx, |store, cx| {
                store.checkouts_in_flight = false;
                store.checkouts_loaded = true;
                match result {
                    Ok(list) if list != store.checkouts => {
                        store.checkouts = list;
                        store.changed(StoreChanged::CHECKOUTS, cx);
                    }
                    Ok(_) => {}
                    Err(error) => tracing::debug!("worktree.list for the sidebar failed: {error}"),
                }
                store.refresh_changed_files(cx);
                if std::mem::take(&mut store.checkouts_again) {
                    store.refresh_checkouts(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// How many files `project`'s checkouts have uncommitted, as last counted: the sidebar's
    /// badge. A lookup; nothing is read.
    pub fn changed_files_of(&self, project: &WorkspaceInfo) -> usize {
        let Some(info) = project.project.as_ref().filter(|p| p.common_dir.is_some()) else {
            return 0;
        };
        self.checkouts
            .iter()
            .filter(|c| c.repo_root == info.root && !c.prunable)
            .filter_map(|c| self.changed_files.get(&c.path))
            .sum()
    }

    /// How many files the checkout at `path` has uncommitted, as last counted.
    pub fn changed_files_at(&self, path: &std::path::Path) -> Option<usize> {
        self.changed_files.get(path).copied()
    }

    /// Counts the uncommitted files of every dirty checkout (`worktree.diff`, the file list
    /// only; at most [`MAX_COUNTED_CHECKOUTS`]), one pass at a time. A checkout that cannot be
    /// counted keeps no entry; a change reaches the sidebar as [`StoreChanged::CHECKOUTS`].
    fn refresh_changed_files(&mut self, cx: &mut Context<Self>) {
        if self.changed_in_flight
            || self.state.connection != ConnectionState::Connected
            || !self.connection.supports_worktree_review()
        {
            return;
        }
        let wanted: Vec<PathBuf> = self
            .checkouts
            .iter()
            .filter(|c| c.dirty && !c.prunable)
            .take(MAX_COUNTED_CHECKOUTS)
            .map(|c| c.path.clone())
            .collect();
        if wanted.is_empty() {
            if !self.changed_files.is_empty() {
                self.changed_files.clear();
                self.changed(StoreChanged::CHECKOUTS, cx);
            }
            return;
        }
        self.changed_in_flight = true;
        let reads: Vec<_> = wanted
            .iter()
            .map(|path| {
                let request = WorktreeDiffRequest {
                    path: path.clone(),
                    base: None,
                    include_uncommitted: true,
                };
                (path.clone(), self.worktree_diff(request, cx))
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let mut counted: HashMap<PathBuf, usize> = HashMap::new();
            for (path, read) in reads {
                if let Ok(diff) = read.await {
                    let files = diff.files.iter().filter(|f| f.uncommitted).count();
                    if files > 0 {
                        counted.insert(path, files);
                    }
                }
            }
            this.update(cx, |store, cx| {
                store.changed_in_flight = false;
                if store.changed_files != counted {
                    store.changed_files = counted;
                    store.changed(StoreChanged::CHECKOUTS, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Watches the entries of folder `dir` (`fs.watch`, not recursive) for one more wisher;
    /// its changes arrive as [`Notice::FsChanged`], and [`Notice::FsGeneration`] for its root.
    pub fn watch_dir(&mut self, dir: &std::path::Path, cx: &mut Context<Self>) {
        let count = self.dir_watches.entry(dir.to_owned()).or_insert(0);
        *count += 1;
        // While disconnected the connection keeps the wish and sends it on the next connection.
        if *count > 1 {
            return;
        }
        let connection = self.connection.clone();
        let dir = dir.to_owned();
        cx.background_spawn(async move {
            if let Err(error) = connection.fs_watch(&dir, false).await {
                tracing::debug!(dir = %dir.display(), %error, "fs.watch of a folder failed");
            }
        })
        .detach();
    }

    /// One wisher less for `dir` ([`Self::watch_dir`]); the last one unwatches it.
    pub fn unwatch_dir(&mut self, dir: &std::path::Path, cx: &mut Context<Self>) {
        let Some(count) = self.dir_watches.get_mut(dir) else {
            return;
        };
        *count -= 1;
        if *count > 0 {
            return;
        }
        self.dir_watches.remove(dir);
        let connection = self.connection.clone();
        let dir = dir.to_owned();
        cx.background_spawn(async move { connection.fs_unwatch(&dir).await })
            .detach();
    }

    /// The folders watched for the Files panel and the finder (tests).
    pub fn watched_dirs(&self) -> usize {
        self.dir_watches.len()
    }

    /// How many live sessions of each workspace run a turn (or a working agent).
    pub fn working_by_workspace(&self) -> HashMap<WorkspaceId, usize> {
        let mut counts = HashMap::new();
        for session in self.state.sessions.iter().filter(|s| self.is_working(s)) {
            *counts.entry(session.workspace_id).or_insert(0) += 1;
        }
        counts
    }

    /// A live session runs a turn (or its agent works).
    pub fn is_working(&self, info: &SessionInfo) -> bool {
        info.state.is_live() && Self::working(info, &self.raw_presentation(info))
    }

    /// The live sessions whose agent works in a place that holds one of `paths`, or that one of
    /// them holds (a folder about to go to the Trash that a session runs in): the session's title
    /// and the agent's id (`codex`, `claude`, `omp`, a chat's harness) when it has one. A place
    /// is where the session runs (its checkout, its folder, its directory) and, while those are
    /// not known yet, the folders of the project it belongs to (never Home's: that is everything).
    pub fn agents_working_near(&self, paths: &[PathBuf]) -> Vec<(String, Option<String>)> {
        self.state
            .sessions
            .iter()
            .filter(|s| self.is_working(s))
            .filter(|s| {
                let mut places = session_places(s);
                if places.is_empty()
                    && let Some(workspace) = self.workspace(s.workspace_id)
                    && workspace.kind != WorkspaceKind::Home
                {
                    places.extend(workspace.roots.iter().cloned());
                }
                places.iter().any(|place| {
                    paths
                        .iter()
                        .any(|p| p.starts_with(place) || place.starts_with(p))
                })
            })
            .map(|s| {
                let agent = s
                    .chat
                    .as_ref()
                    .map(|c| c.agent.clone())
                    .or_else(|| s.agent.as_ref().map(|a| a.agent.clone()));
                (s.title.clone(), agent)
            })
            .collect()
    }

    /// How many sessions of each workspace have a request waiting.
    pub fn requests_by_workspace(&self) -> HashMap<WorkspaceId, usize> {
        let mut counts = HashMap::new();
        for session in self
            .state
            .sessions
            .iter()
            .filter(|s| self.presentation(s).is_request())
        {
            *counts.entry(session.workspace_id).or_insert(0) += 1;
        }
        counts
    }

    pub fn session_of(&self, terminal: TerminalId) -> Option<SessionId> {
        self.state
            .sessions
            .iter()
            .find(|s| s.terminal_id == Some(terminal))
            .map(|s| s.id)
    }

    pub fn unread(&self, id: SessionId) -> Option<Unread> {
        self.attention.unread(id)
    }

    /// How `info` shows up: its own state, except that a live session whose main agent is
    /// blocked in the observation tree always has a request waiting, whatever the session
    /// summary says (blocked subagents alone never make one), and that the session of a task
    /// waiting for review is a soft *Ready for review* item (accent, never a badge:
    /// [`classify`] leaves it alone) until the user has seen it. Seen, it is its own state
    /// again (*idle*), with [`changes_to_review`](Self::changes_to_review) as a link beside it,
    /// never a state of its own. A turn that runs (or is stopping) is *working*, whatever
    /// waits.
    pub fn presentation(&self, info: &SessionInfo) -> Presentation {
        let presentation = self.raw_presentation(info);
        if presentation.attention.is_some() && self.attention.snoozed_key(info.id).is_some() {
            return Presentation {
                detail: if presentation.detail.is_empty() {
                    "snoozed".to_owned()
                } else {
                    format!("{} · snoozed", presentation.detail)
                },
                tone: Tone::Muted,
                filled: false,
                attention: None,
                ..presentation
            };
        }
        presentation
    }

    /// [`presentation`](Self::presentation) before a snooze is applied.
    fn raw_presentation(&self, info: &SessionInfo) -> Presentation {
        let mut presentation = present(info, self.attention.unread(info.id), &self.model_names);
        if presentation.attention.is_none()
            && info.state.is_live()
            && let Some(tree) = self.state.trees.get(&info.id)
        {
            let blocked = agents::blocked_nodes(tree);
            if blocked.iter().any(|node| node.main) {
                return agents::blocked_presentation(&blocked);
            }
        }
        let working = Self::working(info, &presentation);
        if presentation.attention.is_none() && !working && self.review_seen(info.id) == Some(false)
        {
            return Presentation {
                label: "review".into(),
                detail: "awaiting review".into(),
                tone: Tone::Accent,
                filled: true,
                attention: Some(Attention::Review),
            };
        }
        // A plan that waits is a request of its own kind: it says so.
        if presentation.label == "question"
            && presentation.is_request()
            && self
                .state
                .questions
                .iter()
                .filter(|q| q.session_id == info.id)
                .all(|q| presentation::is_plan_title(&q.spec.title))
        {
            presentation.label = "plan".into();
        }
        presentation
    }

    /// A turn runs (or is stopping) in `info`, as its own `presentation` says: review waits.
    fn working(info: &SessionInfo, presentation: &Presentation) -> bool {
        presentation.label == "working" || info.chat.as_ref().is_some_and(|chat| chat.turn_active)
    }

    /// Whether `session`'s work waits for review and the user has seen it, while no turn runs:
    /// the rows say its state (*Idle*) with a small *Changes to review* link beside it, until a
    /// Finish, an accept or a new turn.
    pub fn changes_to_review(&self, session: SessionId) -> bool {
        self.review_seen(session) == Some(true)
            && self.session(session).is_some_and(|info| {
                !Self::working(
                    info,
                    &present(info, self.attention.unread(info.id), &self.model_names),
                )
            })
    }

    /// Whether the user has seen the work of `session` that waits for review; `None` while
    /// nothing waits.
    fn review_seen(&self, session: SessionId) -> Option<bool> {
        let key = self.tasks.review_key(session)?;
        Some(self.reviews_seen.get(&session) == Some(&key))
    }

    /// The user has seen the work of `session` that waits for review (its chat or its Changes
    /// view is on screen): it no longer needs them, and says *Changes to review* beside its
    /// state. Whether that changed anything.
    fn see_review(&mut self, session: SessionId) -> bool {
        let Some(key) = self.tasks.review_key(session) else {
            return false;
        };
        if self.reviews_seen.get(&session) == Some(&key) {
            return false;
        }
        // Waits that are over are forgotten: what is left is bounded by the tasks.
        let tasks = &self.tasks;
        self.reviews_seen
            .retain(|id, _| tasks.review_key(*id).is_some());
        self.reviews_seen.insert(session, key);
        true
    }

    /// The Changes view of the checkout at `root` is on screen: the work waiting for review
    /// of every session that works there is seen.
    pub fn mark_checkout_looked_at(&mut self, root: &std::path::Path, cx: &mut Context<Self>) {
        let unseen: Vec<(SessionId, PathBuf)> = self
            .state
            .sessions
            .iter()
            .filter(|s| self.review_seen(s.id) == Some(false))
            .filter_map(|s| s.placement.as_ref().map(|p| (s.id, p.root.clone())))
            .collect();
        if unseen.is_empty() {
            return;
        }
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
        let mut changed = false;
        for (session, at) in unseen {
            if at == canonical || at == root {
                changed |= self.see_review(session);
            }
        }
        if changed {
            self.changed(StoreChanged::ATTENTION, cx);
        }
    }

    /// The observation tree of `session` (capability `agent.tree`), when it has agents.
    pub fn agent_tree(&self, session: SessionId) -> Option<&AgentObservation> {
        self.state.trees.get(&session)
    }

    /// Every observation tree, by session.
    pub fn agent_trees(&self) -> impl Iterator<Item = &AgentObservation> {
        self.state.trees.values()
    }

    /// What the row, tab or card of `session` says about its agents (empty without a tree).
    pub fn agent_badge(&self, session: SessionId) -> AgentBadge {
        self.state
            .trees
            .get(&session)
            .map(agents::badge)
            .unwrap_or_default()
    }

    /// When something last happened in `info`'s session, for the age a row shows: the last
    /// report or node change of its agent, else when it was created.
    pub fn last_activity_ms(&self, info: &SessionInfo) -> u64 {
        self.agent_presentation()
            .get(info.id)
            .and_then(|record| record.updated_at_ms)
            .map_or(info.created_at_ms, |at| at.max(info.created_at_ms))
    }

    /// The one agent presentation (see [`presentation`]): every agent session, joined and
    /// reduced. Built from the daemon's state when it is first asked for after a change and
    /// shared until the next one; no polling, no rebuild per frame.
    pub fn agent_presentation(&self) -> Arc<AgentPresentation> {
        if let Some(cached) = &*self.agent_cache.borrow()
            && cached.revision == self.revision
        {
            return cached.clone();
        }
        let unread = self.attention.unread_map();
        let mut reviews: HashSet<SessionId> = HashSet::new();
        let mut seen: HashSet<SessionId> = HashSet::new();
        for session in &self.state.sessions {
            match self.review_seen(session.id) {
                Some(false) => {
                    reviews.insert(session.id);
                }
                Some(true) => {
                    seen.insert(session.id);
                }
                None => {}
            }
        }
        let snoozed: HashSet<SessionId> = self.attention.snoozed_map().keys().copied().collect();
        let built = Arc::new(presentation::build(&Facts {
            state: &self.state,
            unread,
            reviews: &reviews,
            changes_to_review: &seen,
            snoozed: &snoozed,
            answered_approvals: &self.answered,
            answered_questions: &self.answered_questions,
            last_confirmed_ms: self.last_confirmed_ms,
            model_names: &self.model_names,
            revision: self.revision,
        }));
        self.presentation_builds
            .set(self.presentation_builds.get() + 1);
        *self.agent_cache.borrow_mut() = Some(built.clone());
        built
    }

    /// How many times the agent presentation was built (tests: a store change nobody asked
    /// about builds nothing).
    pub fn presentation_builds(&self) -> usize {
        self.presentation_builds.get()
    }

    /// Counts what the agent presentation is made of changing.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Sessions in the group `attention`, in daemon order.
    pub fn in_attention(&self, attention: Attention) -> Vec<SessionId> {
        self.state
            .sessions
            .iter()
            .filter(|s| self.presentation(s).attention == Some(attention))
            .map(|s| s.id)
            .collect()
    }

    /// Sessions with a request waiting (an approval, a question, a plan), in daemon order: the
    /// only ones that block an agent, and what the status bar counts.
    pub fn requests(&self) -> Vec<SessionId> {
        self.in_attention(Attention::Request)
    }

    /// The sessions that need the user for the menu bar item and the Dock badge: blocked
    /// agents, pending approvals and unread failures, in daemon order. See [`classify`].
    pub fn attention_entries(&self) -> Vec<AttentionEntry> {
        let mut pending: HashMap<SessionId, usize> = HashMap::new();
        for approval in &self.state.approvals {
            *pending.entry(approval.session_id).or_default() += 1;
        }
        let mut asked: HashMap<SessionId, usize> = HashMap::new();
        for question in &self.state.questions {
            *asked.entry(question.session_id).or_default() += 1;
        }
        self.state
            .sessions
            .iter()
            .filter_map(|info| {
                let presentation = self.presentation(info);
                let approvals = pending.get(&info.id).copied().unwrap_or(0);
                let questions = asked.get(&info.id).copied().unwrap_or(0);
                let kind = classify(&presentation, approvals, questions)?;
                let detail = match kind {
                    AttentionKind::Approval if approvals == 1 => String::new(),
                    AttentionKind::Approval => format!("{approvals} requests"),
                    AttentionKind::Question if questions == 1 => String::new(),
                    AttentionKind::Question => format!("{questions} questions"),
                    AttentionKind::Blocked | AttentionKind::Failed => presentation.detail,
                };
                Some(AttentionEntry {
                    session: info.id,
                    title: info.title.clone(),
                    kind,
                    detail,
                })
            })
            .collect()
    }

    /// The session after `current` that wants the user, most urgent first: a request that
    /// waits, then a failure, a review, and last what is only finished or marked unread;
    /// sessions of the same urgency in list order. Wraps around.
    pub fn next_needing_attention(&self, current: Option<SessionId>) -> Option<SessionId> {
        let mut needing: Vec<(u8, usize, SessionId)> = self
            .state
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let presentation = self.presentation(s);
                // A failure the user has seen stays in its group but is not worth a jump.
                presentation.attention.is_some()
                    && (presentation.attention != Some(Attention::Failed) || presentation.filled)
            })
            .map(|(at, s)| (self.urgency(s), at, s.id))
            .collect();
        needing.sort_unstable();
        let at = current.and_then(|c| needing.iter().position(|(_, _, id)| *id == c));
        let next = match at {
            Some(at) => needing.get((at + 1) % needing.len()),
            None => needing.first(),
        };
        next.map(|(_, _, id)| *id)
    }

    /// How urgently `info` wants the user: 0 a request waits, 1 it failed, 2 its work waits for
    /// review, 3 it is finished or marked unread.
    fn urgency(&self, info: &SessionInfo) -> u8 {
        let waits = self.state.approvals.iter().any(|a| a.session_id == info.id)
            || self.state.questions.iter().any(|q| q.session_id == info.id);
        if waits {
            return 0;
        }
        match self.presentation(info).attention {
            Some(Attention::Request) => 0,
            Some(Attention::Failed) => 1,
            _ if self.presentation(info).label == "review" => 2,
            _ => 3,
        }
    }

    pub fn focused(&self) -> Option<SessionId> {
        self.focused
    }

    // ---- approvals -------------------------------------------------------------------------

    /// Permission requests of every agent chat waiting for an answer, oldest first.
    pub fn approvals(&self) -> &[ApprovalInfo] {
        &self.state.approvals
    }

    /// Questions of every agent chat waiting for an answer, oldest first (protocol 0.24).
    pub fn questions(&self) -> &[hutan_protocol::chat::QuestionInfo] {
        &self.state.questions
    }

    /// Whether this window answered `question`.
    pub fn question_answered_here(&self, question: hutan_core::QuestionId) -> bool {
        self.answered_questions.contains(&question)
    }

    /// `question.answer`. Remembered before the daemon replies (the resolved event can overtake
    /// the reply) and forgotten again when the daemon refuses it.
    pub fn answer_question(
        &mut self,
        question: hutan_core::QuestionId,
        response: hutan_core::QuestionResponse,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        self.answered_questions.insert(question);
        self.changed(StoreChanged::OTHER, cx);
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            let answered = connection
                .answer_question(question, response)
                .await
                .map_err(|e| e.to_string());
            if answered.is_err() {
                this.update(cx, |this, cx| {
                    this.answered_questions.remove(&question);
                    this.changed(StoreChanged::OTHER, cx);
                })
                .ok();
            }
            answered
        })
    }

    /// The option this window chose for `approval`, if it answered it.
    pub fn answered_here(&self, approval: ApprovalId) -> Option<&str> {
        self.answered.get(&approval).map(String::as_str)
    }

    /// `approval.respond`. The answer is remembered before the daemon replies (the resolved
    /// event can overtake the reply) and forgotten again when the daemon refuses it.
    pub fn respond_approval(
        &mut self,
        approval: ApprovalId,
        option_id: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        self.answered.insert(approval, option_id.clone());
        self.changed(StoreChanged::OTHER, cx);
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            let answered = connection
                .approve(approval, option_id)
                .await
                .map_err(|e| e.to_string());
            if answered.is_err() {
                this.update(cx, |this, cx| {
                    this.answered.remove(&approval);
                    this.changed(StoreChanged::OTHER, cx);
                })
                .ok();
            }
            answered
        })
    }

    /// The session that had focus most recently (still the focused one while it has focus).
    pub fn last_focused(&self) -> Option<SessionId> {
        self.focused.or(self.last_focused)
    }

    /// The focus the daemon acknowledged for the current connection (`client.focus`): `None`
    /// before the first acknowledgement and after the connection was lost, `Some(None)` once
    /// "nothing is looked at" was acknowledged.
    pub fn focus_reported(&self) -> Option<Option<SessionId>> {
        let generation = self.connection.generation();
        self.focus_sent
            .filter(|(sent_on, _)| *sent_on == generation)
            .map(|(_, focus)| focus)
    }

    /// The session the user is looking at for `client.focus`: the focused item's session, else
    /// the docked Assistant's while it has the focus.
    fn looked_at(&self) -> Option<SessionId> {
        self.focused.or(self.assistant_focused)
    }

    /// The Assistant panel has (or no longer has) the focus with `session` shown. Only the
    /// daemon's idea of "looked at" changes; attention and the inspector follow
    /// [`set_focused`](Self::set_focused) alone.
    pub fn set_assistant_focused(&mut self, session: Option<SessionId>, cx: &mut Context<Self>) {
        if self.assistant_focused != session {
            self.assistant_focused = session;
            self.report_focus(cx);
        }
    }

    // ---- attention -------------------------------------------------------------------------

    /// The session shown in the focused item changed (`None`: no session has focus).
    pub fn set_focused(&mut self, session: Option<SessionId>, cx: &mut Context<Self>) {
        if self.focused != session {
            self.focused = session;
            if self.hold_unread != session {
                self.hold_unread = None;
            }
            if let Some(looked_at) = session {
                self.last_focused = session;
                // A daemon with `notify.mute` withdraws what the session has pending itself.
                if self.connection.supports_notify_mute() {
                    self.surfaced.remove(&looked_at);
                }
                self.see_call(looked_at, cx);
            }
            self.report_focus(cx);
            self.changed(StoreChanged::FOCUS, cx);
        }
    }

    /// Tells the daemon which session this window shows (`client.focus`), so that it raises no
    /// "turn finished" notification for a session the user is looking at. One request at a
    /// time; when the focus changed while it was in flight the next one follows.
    fn report_focus(&mut self, cx: &mut Context<Self>) {
        if self.state.connection != ConnectionState::Connected || !self.connection.supports_notify()
        {
            return;
        }
        let generation = self.connection.generation();
        let focused = self.looked_at();
        match &self.focus_task {
            // One request at a time: this one's answer decides whether another follows.
            Some((started_on, _)) if *started_on == generation => return,
            // It was for a connection that is gone: its answer would stamp this one with a
            // focus the daemon never got.
            Some(_) => self.focus_task = None,
            None => {}
        }
        if self.focus_sent == Some((generation, focused)) {
            return;
        }
        let connection = self.connection.clone();
        let task = cx.spawn(async move |this, cx| {
            let sent = connection.client_focus(focused).await;
            this.update(cx, |this, cx| {
                this.focus_task = None;
                match sent {
                    Ok(sent_on) => {
                        this.focus_sent = Some((sent_on, focused));
                        this.report_focus(cx);
                    }
                    Err(error) => tracing::debug!(%error, "could not report the focus"),
                }
            })
            .ok();
        });
        self.focus_task = Some((generation, task));
    }

    /// The user looked at `session` (it is the one on screen): finished work, marks and the work
    /// it left for review are seen; a failure stays listed in *Failed*, no longer a badge, until
    /// a new run starts or *Mark as read*. A session just marked unread while it is the one on
    /// screen stays marked until the focus has gone elsewhere and come back.
    pub fn mark_looked_at(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.hold_unread == Some(session) {
            return;
        }
        let read = self.attention.look_at(session);
        self.see_call(session, cx);
        if self.see_review(session) || read {
            self.changed(StoreChanged::ATTENTION, cx);
        }
        self.ack_surfaced(session, cx);
    }

    /// *Mark as read*: what is unread about `session` is read and a failure is dismissed,
    /// whether or not it is on screen, and the work it left for review is seen. The
    /// notification intents raised for it are acknowledged: their banners go.
    pub fn mark_read(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.hold_unread == Some(session) {
            self.hold_unread = None;
        }
        let read = self.attention.mark_read(session);
        self.see_call(session, cx);
        if self.see_review(session) || read {
            self.changed(StoreChanged::ATTENTION, cx);
        }
        self.ack_surfaced(session, cx);
    }

    /// Remembers an intent raised for a session, to acknowledge it when the session is read.
    fn note_intent(&mut self, intent: &NotificationIntent, cx: &mut Context<Self>) {
        let total: usize = self.surfaced.values().map(Vec::len).sum();
        if total >= MAX_SURFACED {
            // The oldest are forgotten with their session's whole list; the daemon lets intents
            // expire on its own.
            self.surfaced.clear();
        }
        self.surfaced
            .entry(intent.session_id)
            .or_default()
            .push(intent.id.clone());
        // A request for a session this window is not looking at: the tab gets its dot until it
        // is seen, and the workbench may show a toast. An old intent is a replay.
        if calls::is_request(intent.kind)
            && self.looked_at() != Some(intent.session_id)
            && ui_open_is_fresh(intent.created_at_ms, unix_ms())
        {
            self.called.insert(intent.session_id, false);
            cx.emit(RequestCalled {
                intent: Box::new(intent.clone()),
            });
            self.changed(StoreChanged::ATTENTION, cx);
        }
    }

    /// Whether a call for `session` is still held: not seen, and its request not yet answered
    /// (before the state has shown the request, too).
    pub fn has_call(&self, session: SessionId) -> bool {
        self.called.contains_key(&session)
    }

    /// Whether a request called for `session` while it was not on screen and the user has not
    /// looked at it since: its tab shows a dot. Only while the request still waits.
    pub fn is_called(&self, session: SessionId) -> bool {
        self.called.contains_key(&session)
            && self
                .session(session)
                .is_some_and(|info| self.presentation(info).attention == Some(Attention::Request))
    }

    /// The call for `session` is seen (the session is on screen, or marked read). Whether there
    /// was one.
    pub fn see_call(&mut self, session: SessionId, cx: &mut Context<Self>) -> bool {
        let seen = self.called.remove(&session).is_some();
        if seen {
            self.changed(StoreChanged::ATTENTION, cx);
        }
        seen
    }

    /// Drops the calls whose request has been answered, or whose session is gone: a call is
    /// confirmed once the state shows its request, and ends when that request does. Whether
    /// anything changed.
    fn observe_calls(&mut self) -> bool {
        let ids: Vec<(SessionId, bool)> = self.called.iter().map(|(id, c)| (*id, *c)).collect();
        let mut changed = false;
        for (id, confirmed) in ids {
            let request = self
                .session(id)
                .map(|info| self.presentation(info).attention == Some(Attention::Request));
            match (request, confirmed) {
                (Some(true), false) => {
                    self.called.insert(id, true);
                }
                (Some(false) | None, true) => {
                    self.called.remove(&id);
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    /// `notify.ack` for every intent noted for `session`. One that is gone already (acted on,
    /// expired, withdrawn) answers `not_found`, which is fine.
    fn ack_surfaced(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let Some(ids) = self.surfaced.remove(&session) else {
            return;
        };
        if self.state.connection != ConnectionState::Connected {
            return;
        }
        for id in ids {
            let acked = self.call(
                Request::NotifyAck(hutan_protocol::notify::NotifyAck { id }),
                |_: hutan_protocol::messages::Empty| (),
                cx,
            );
            cx.background_spawn(async move {
                if let Err(error) = acked.await {
                    tracing::debug!(%error, "an intent was not acknowledged");
                }
            })
            .detach();
        }
    }

    /// Whether notifications of `session` are muted (`notify.mute`, protocol 0.34).
    pub fn is_muted(&self, session: SessionId) -> bool {
        self.session(session).is_some_and(|info| info.muted)
    }

    /// Whether the daemon reports harness modes (`ChatInfo.mode`, protocol 0.34): without it a
    /// missing mode means "not said", not "not reported by the harness".
    pub fn supports_chat_mode(&self) -> bool {
        self.connection.supports_chat_mode()
    }

    /// Whether the daemon takes safety presets on chats and reports `ChatInfo.safety`
    /// (protocol 0.51).
    pub fn supports_chat_safety(&self) -> bool {
        self.connection.supports_chat_safety()
    }

    /// Whether the daemon can run a session inside Hutan's Seatbelt boundary (capability
    /// `sandbox`, protocol 0.52).
    pub fn supports_sandbox(&self) -> bool {
        self.connection.supports_sandbox()
    }

    /// Whether the daemon can mute a session's notifications.
    pub fn supports_mute(&self) -> bool {
        self.connection.supports_notify_mute()
    }

    /// `notify.mute`: silences the notifications of `session` (`muted`) or lets them through.
    /// The session's `muted` follows from the daemon's `session.updated`.
    pub fn set_muted(
        &self,
        session: SessionId,
        muted: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        self.call(
            Request::NotifyMute(hutan_protocol::notify::NotifyMute {
                session_id: session,
                muted,
            }),
            |result: SessionResult| result.session,
            cx,
        )
    }

    /// *Mark as unread*: `session` needs the user again until it is looked at. Ends a snooze.
    /// False when there is no such session.
    pub fn mark_unread(&mut self, session: SessionId, cx: &mut Context<Self>) -> bool {
        if self.session(session).is_none() {
            return false;
        }
        let marked = self.attention.mark_unread(session);
        let woken = self.attention.unsnooze(session);
        if self.focused == Some(session) {
            self.hold_unread = Some(session);
        }
        if marked || woken {
            self.changed(StoreChanged::ATTENTION, cx);
        }
        true
    }

    /// *Snooze until it changes*: `session` leaves what needs the user (the list, the counts,
    /// the next-attention jump) until its attention is something else — another request, a
    /// failure, a turn that ended. False when it does not need the user now.
    pub fn snooze(&mut self, session: SessionId, cx: &mut Context<Self>) -> bool {
        let Some(info) = self.session(session).cloned() else {
            return false;
        };
        if self.raw_presentation(&info).attention.is_none() {
            return false;
        }
        let key = self.attention_key(&info);
        if self.attention.snooze(session, key) {
            self.changed(StoreChanged::ATTENTION, cx);
        }
        true
    }

    /// Ends a snooze: the session needs the user again.
    pub fn unsnooze(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.attention.unsnooze(session) {
            self.changed(StoreChanged::ATTENTION, cx);
        }
    }

    /// Whether `session` is marked unread by the user.
    pub fn is_marked_unread(&self, session: SessionId) -> bool {
        self.attention.is_marked(session)
    }

    /// Whether `session` is snoozed.
    pub fn is_snoozed(&self, session: SessionId) -> bool {
        self.attention.snoozed_key(session).is_some()
    }

    /// What the desktop keeps across restarts (`desktop-state.json`).
    pub fn marks(&self) -> Marks {
        self.attention.marks()
    }

    /// Takes the marks of an earlier run back.
    pub fn restore_marks(&mut self, marks: &Marks, cx: &mut Context<Self>) {
        self.attention.restore(marks);
        self.changed(StoreChanged::ATTENTION, cx);
    }

    // ---- agents nobody reported ----------------------------------------------------------

    /// The known agent that appears to run in terminal `session` without ever having reported
    /// there (`UX-ATTENTION-CONTROLS`): the daemon counts a terminal as an agent only once an
    /// integration reported, so this one is invisible to the attention queue. `None` for a
    /// session that has reported, is tracked, runs a chat or is not running.
    pub fn untracked_agent(&self, session: SessionId) -> Option<Untracked> {
        let info = self.session(session)?;
        let plain_terminal = info.kind == hutan_core::SessionKind::Terminal
            && info.state.is_live()
            && info.agent.is_none()
            && info.chat.is_none()
            && !self.state.trees.contains_key(&session);
        if plain_terminal {
            self.untracked.get(&session).copied()
        } else {
            None
        }
    }

    /// Looks at the processes of the live sessions for agents that never reported: when the
    /// connection comes up, when a session appears, when a command that starts an agent runs,
    /// and whenever the window comes back. One listing at a time; a trigger that arrives while
    /// it is in flight asks for one more.
    pub fn scan_agents(&mut self, cx: &mut Context<Self>) {
        if self.state.connection != ConnectionState::Connected
            || !self.connection.supports_resources()
        {
            return;
        }
        if self.scan_task.is_some() {
            self.scan_again = true;
            return;
        }
        let connection = self.connection.clone();
        self.scan_task = Some(cx.spawn(async move |this, cx| {
            let sample = connection
                .call::<ResourceSample>(Request::ResourcesList(ResourcesListRequest {
                    session_id: None,
                }))
                .await;
            this.update(cx, |this, cx| {
                this.scan_task = None;
                if let Ok(sample) = sample {
                    this.apply_sample(&sample, cx);
                }
                if std::mem::take(&mut this.scan_again) {
                    this.scan_agents(cx);
                }
            })
            .ok();
        }));
    }

    /// Takes the agents a process sample shows (a scan of ours, or the Resources panel's).
    fn apply_sample(&mut self, sample: &ResourceSample, cx: &mut Context<Self>) {
        let found = untracked::scan(sample);
        if found != self.untracked {
            self.untracked = found;
            self.changed(StoreChanged::UNTRACKED, cx);
        }
    }

    /// *Track as <agent>*: reports the agent found in terminal `session` (`agent.report`, source
    /// `hutan-desktop`, with the pid, without a state: the agent shows as *State unavailable*
    /// until a hook or an extension says more, never as idle). From then on the session is an
    /// agent session: it is in the Agents page and a blocked report puts it in *Requests*.
    pub fn track_agent(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let found = self.untracked_agent(session);
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            let found = found.ok_or_else(|| "No agent was found in this terminal".to_owned())?;
            connection
                .agent_report(AgentReportRequest {
                    session_id: session,
                    source: TRACK_SOURCE.to_owned(),
                    agent: found.agent.to_owned(),
                    state: None,
                    message: Some(
                        "Tracked from Hutan: no integration reports what this agent does"
                            .to_owned(),
                    ),
                    // Increases per source and session across restarts of the desktop.
                    seq: unix_ms(),
                    agent_session_id: None,
                    resume_argv: None,
                    node: None,
                    pid: Some(found.pid),
                })
                .await
                .map_err(|e| e.to_string())?;
            this.update(cx, |this, cx| {
                this.untracked.remove(&session);
                this.changed(StoreChanged::UNTRACKED, cx);
            })
            .ok();
            Ok(())
        })
    }

    /// What `info`'s attention is made of, for [`snooze`](Self::snooze): the state it shows, the
    /// requests and questions that wait, the agents that are blocked and what is unread. A
    /// snooze lasts while this stays the same.
    fn attention_key(&self, info: &SessionInfo) -> String {
        let presentation = self.raw_presentation(info);
        let mut parts = vec![
            presentation.label,
            presentation.detail,
            format!("{:?}", self.attention.unread(info.id)),
        ];
        parts.extend(
            self.state
                .approvals
                .iter()
                .filter(|a| a.session_id == info.id)
                .map(|a| format!("approval:{}", a.id)),
        );
        parts.extend(
            self.state
                .questions
                .iter()
                .filter(|q| q.session_id == info.id)
                .map(|q| format!("question:{}", q.id)),
        );
        if let Some(tree) = self.state.trees.get(&info.id) {
            parts.extend(
                agents::blocked_nodes(tree)
                    .into_iter()
                    .map(|n| format!("blocked:{}", n.node_id)),
            );
        }
        parts.join("|")
    }

    /// Ends the snoozes whose session is gone or whose attention changed. True when any ended.
    fn lapse_snoozes(&mut self) -> bool {
        let lapsed: Vec<SessionId> = self
            .attention
            .snoozed_map()
            .iter()
            .filter(|(id, key)| {
                self.session(**id)
                    .is_none_or(|info| self.attention_key(info) != **key)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in &lapsed {
            self.attention.unsnooze(*id);
        }
        !lapsed.is_empty()
    }

    // ---- terminals -------------------------------------------------------------------------

    /// The shared attachment of `session`'s terminal: attached on first use, detached when the
    /// last holder drops it. `None` when the session is unknown or has no terminal.
    pub fn terminal_session(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalSession>> {
        if let Some(existing) = self.terminals.get(&session).and_then(WeakEntity::upgrade) {
            return Some(existing);
        }
        let info = self.session(session)?.clone();
        let terminal = TerminalSession::attach(
            info,
            self.state.epoch,
            self.connection.clone(),
            self.colors,
            cx,
        )?;
        if self.daemon_stopped {
            terminal.update(cx, |terminal, cx| terminal.set_daemon_stopped(true, cx));
        }
        self.terminals.insert(session, terminal.downgrade());
        Some(terminal)
    }

    /// Whether the user stopped the daemon from the app (it stays down until they start it).
    pub fn daemon_stopped(&self) -> bool {
        self.daemon_stopped
    }

    /// Records that the backend of `workspace`'s assistants (`None`: Global) was chosen
    /// (`pinned`) or handed back to inheritance.
    pub fn note_assistant_pinned(
        &mut self,
        workspace: Option<WorkspaceId>,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = if pinned {
            self.assistant_pinned.insert(workspace)
        } else {
            self.assistant_pinned.remove(&workspace)
        };
        if changed {
            self.changed(StoreChanged::OTHER, cx);
        }
    }

    /// Records whether `daemon.toml` names the assistants' backend or provider.
    pub fn set_assistant_file_choice(&mut self, named: bool, cx: &mut Context<Self>) {
        if self.assistant_file_choice != Some(named) {
            self.assistant_file_choice = Some(named);
            self.changed(StoreChanged::OTHER, cx);
        }
    }

    /// Whether the assistants of `workspace` (`None`: Global) run on the automatic backend: no
    /// choice of theirs, of their parent workspace or of Global is known to this window, and
    /// `daemon.toml` names none. The daemon does not say, so a choice made by another client
    /// or before this window started is not known.
    pub fn assistant_is_automatic(&self, workspace: Option<WorkspaceId>) -> bool {
        if self.assistant_file_choice == Some(true) || self.assistant_pinned.contains(&None) {
            return false;
        }
        let Some(id) = workspace else {
            return true;
        };
        let parent = self.workspace(id).and_then(|w| w.parent_id);
        !self.assistant_pinned.contains(&Some(id))
            && !parent.is_some_and(|p| self.assistant_pinned.contains(&Some(p)))
    }

    /// Tells every attached terminal that the daemon was stopped on purpose (or started again).
    pub fn set_daemon_stopped(&mut self, stopped: bool, cx: &mut Context<Self>) {
        if self.daemon_stopped == stopped {
            return;
        }
        self.daemon_stopped = stopped;
        self.stopped_epoch = if stopped && self.state.connection == ConnectionState::Connected {
            self.state.epoch
        } else {
            None
        };
        for terminal in self.terminals.values().filter_map(WeakEntity::upgrade) {
            terminal.update(cx, |terminal, cx| terminal.set_daemon_stopped(stopped, cx));
        }
        self.changed(StoreChanged::CONNECTION | StoreChanged::OTHER, cx);
    }

    /// The attachment of `session`'s terminal if some view holds it right now; never attaches.
    pub fn attached(&self, session: SessionId) -> Option<Entity<TerminalSession>> {
        self.terminals.get(&session).and_then(WeakEntity::upgrade)
    }

    /// Number of terminals attached right now.
    pub fn attached_count(&self) -> usize {
        self.terminals
            .values()
            .filter(|t| t.upgrade().is_some())
            .count()
    }

    /// Tell every attached terminal the colors the terminal theme draws with, so OSC 10/11/12
    /// replies match what the user sees; terminals attached later get them on attach.
    pub fn set_terminal_colors(&mut self, colors: DefaultColors, cx: &mut Context<Self>) {
        if self.colors == Some(colors) {
            return;
        }
        self.colors = Some(colors);
        for terminal in self.terminals.values().filter_map(WeakEntity::upgrade) {
            terminal.update(cx, |terminal, _| terminal.set_colors(colors));
        }
    }

    // ---- lifecycle commands ----------------------------------------------------------------

    /// Starts the terminal session `spec` describes in `workspace` (the daemon's default when
    /// `None`).
    pub fn create_session(
        &mut self,
        workspace: Option<WorkspaceId>,
        spec: TerminalSpec,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        let request = Request::SessionCreate(SessionCreate {
            workspace_id: workspace,
            spec: SessionSpec::Terminal(spec),
        });
        self.run(request, cx)
    }

    /// `chat.create`: starts an agent conversation as a session (resolves once the agent is
    /// ready).
    pub fn create_chat(
        &mut self,
        request: ChatCreate,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            let info = connection
                .chat_create(request)
                .await
                .map_err(|e| e.to_string())?;
            let remembered = info.clone();
            this.update(cx, |this, _| {
                if !this.state.sessions.iter().any(|s| s.id == remembered.id) {
                    this.recent.insert(remembered.id, remembered);
                }
            })
            .ok();
            Ok(info)
        })
    }

    /// `chat.models`: what `agent` offers. The names of the answer are remembered (and saved),
    /// so every surface shows the models by name, also after a restart.
    pub fn chat_models(
        &mut self,
        agent: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<ModelInfo>, String>> {
        let connection = self.connection.clone();
        let agent = agent.to_owned();
        cx.spawn(async move |this, cx| {
            let answer = connection
                .chat_models(&agent)
                .await
                .map(|answer| answer.models)
                .map_err(|e| e.to_string());
            if let Ok(models) = &answer {
                this.update(cx, |this, cx| this.learn_model_names(&agent, models, cx))
                    .ok();
            }
            answer
        })
    }

    /// A chat shows a model this desktop knows no name for: asks its agent's `chat.models`
    /// once per agent per run (the answer fills the names). Names already known (a restart
    /// with the cache) ask nothing.
    fn fetch_missing_model_names(&mut self, cx: &mut Context<Self>) {
        let missing: Vec<String> = self
            .state
            .sessions
            .iter()
            .filter_map(|s| s.chat.as_ref())
            .filter(|chat| {
                chat.provider_id.is_none()
                    && !chat.model.is_empty()
                    && !self.models_asked.contains(&chat.agent)
                    && self.model_names.name(&chat.agent, &chat.model).is_none()
            })
            .map(|chat| chat.agent.clone())
            .collect();
        for agent in missing {
            if self.models_asked.insert(agent.clone()) {
                self.chat_models(&agent, cx).detach();
            }
        }
    }

    /// Reads the model names saved at `path` and keeps the ones learned from now on there
    /// (`None`: nothing is saved).
    pub fn use_model_names_file(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.model_names = path.as_deref().map(ModelNames::load).unwrap_or_default();
        self.model_names_path = path;
        self.revision += 1;
        cx.notify();
    }

    /// The display names of the harnesses' models, as far as this desktop knows them.
    pub fn model_names(&self) -> &ModelNames {
        &self.model_names
    }

    /// The display name of `model` in the catalog of `agent`, as far as this desktop knows it.
    pub fn model_name(&self, agent: &str, model: &str) -> Option<&str> {
        self.model_names.name(agent, model)
    }

    /// Takes `models` as the catalog of `agent` (see [`ModelNames::learn`]).
    pub fn learn_model_names(&mut self, agent: &str, models: &[ModelInfo], cx: &mut Context<Self>) {
        if !self.model_names.learn(agent, models) {
            return;
        }
        self.revision += 1;
        if let Some(path) = self.model_names_path.clone() {
            let json = self.model_names.to_json();
            // One write after the other: an older catalog never lands over a newer one.
            let previous = self.model_names_save.take();
            self.model_names_save = Some(cx.background_spawn(async move {
                if let Some(previous) = previous {
                    previous.await;
                }
                if let Err(error) = models::save(&path, &json) {
                    tracing::warn!(%error, path = %path.display(), "saving the model names failed");
                }
            }));
        }
        cx.notify();
    }

    /// `account.login` / `account.logout`: the harness's own flow in a terminal session. The
    /// session is remembered until its events arrive, so the window can show it at once.
    pub fn account_flow(
        &mut self,
        harness: &str,
        provider: Option<String>,
        login: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<hutan_protocol::account::AccountSession, String>> {
        self.account_flow_as(harness, provider, None, login, cx)
    }

    /// [`account_flow`](Self::account_flow) for a managed account (protocol 0.50): the
    /// harness's command runs with that account's home.
    pub fn account_flow_as(
        &mut self,
        harness: &str,
        provider: Option<String>,
        account: Option<String>,
        login: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<hutan_protocol::account::AccountSession, String>> {
        let connection = self.connection.clone();
        let harness = harness.to_owned();
        cx.spawn(async move |this, cx| {
            let answer = if login {
                connection
                    .account_login_as(&harness, provider, account)
                    .await
            } else {
                connection.account_logout(&harness, provider).await
            }
            .map_err(|e| e.to_string())?;
            let remembered = answer.session.clone();
            this.update(cx, |this, _| {
                if !this.state.sessions.iter().any(|s| s.id == remembered.id) {
                    this.recent.insert(remembered.id, remembered);
                }
            })
            .ok();
            Ok(answer)
        })
    }

    /// `account.add`: a managed account with an empty home (protocol 0.50).
    pub fn account_add(
        &mut self,
        harness: &str,
        name: &str,
        label: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<hutan_protocol::account::ManagedAccount, String>> {
        let connection = self.connection.clone();
        let (harness, name) = (harness.to_owned(), name.to_owned());
        cx.spawn(async move |_, _| {
            connection
                .account_add(&harness, &name, label)
                .await
                .map_err(|e| e.to_string())
        })
    }

    /// `account.remove`: logs the account out, deletes its home and forgets it.
    pub fn account_remove(
        &mut self,
        harness: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<hutan_protocol::account::AccountRemoved, String>> {
        let connection = self.connection.clone();
        let (harness, name) = (harness.to_owned(), name.to_owned());
        cx.spawn(async move |_, _| {
            connection
                .account_remove(&harness, &name)
                .await
                .map_err(|e| e.to_string())
        })
    }

    /// `account.default`: the default account of a harness (`None` clears it).
    pub fn account_default(
        &mut self,
        harness: &str,
        account: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let connection = self.connection.clone();
        let harness = harness.to_owned();
        cx.spawn(async move |_, _| {
            connection
                .account_default(&harness, account, None)
                .await
                .map_err(|e| e.to_string())
        })
    }

    pub fn stop(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        self.run(
            Request::SessionStop(SessionRef {
                session_id: session,
            }),
            cx,
        )
    }

    /// Whether the daemon can close and reopen sessions (capability `session.close`); the
    /// close buttons and History's Reopen exist only then.
    pub fn supports_close(&self) -> bool {
        self.connection.supports_session_close()
    }

    /// `session.close`. A session something runs in is refused as [`CloseFailure::Busy`]
    /// unless `stop_running`.
    pub fn close(
        &self,
        session: SessionId,
        stop_running: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), CloseFailure>> {
        let connection = self.connection.clone();
        cx.spawn(async move |_, _| {
            connection
                .session_close(session, stop_running)
                .await
                .map_err(|error| match error.busy() {
                    Some(busy) => CloseFailure::Busy(busy),
                    None => CloseFailure::Failed(error.to_string()),
                })
        })
    }

    /// `session.reopen`: a closed session comes back. The recovery the daemon attaches to it
    /// (it is brought back the way a restart brings one back) is acknowledged at once: the user
    /// asked for this session, so no restart card or chip is due.
    pub fn reopen(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        self.reopening.insert(session);
        let reopened = self.run(
            Request::SessionReopen(SessionRef {
                session_id: session,
            }),
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = reopened.await;
            if result.is_ok()
                && let Ok(ack) = this.update(cx, |store, cx| store.ack_recovery(Some(session), cx))
            {
                // Refused by an older daemon: the chip stays, harmless.
                let _ = ack.await;
            }
            this.update(cx, |store, _| {
                store.reopening.remove(&session);
            })
            .ok();
            result
        })
    }

    pub fn restart(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        self.run(
            Request::SessionRestart(SessionRef {
                session_id: session,
            }),
            cx,
        )
    }

    pub fn rename(
        &mut self,
        session: SessionId,
        title: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        self.run(
            Request::SessionRename(SessionRename {
                session_id: session,
                title,
            }),
            cx,
        )
    }

    fn run(
        &mut self,
        request: Request,
        cx: &mut Context<Self>,
    ) -> Task<Result<SessionInfo, String>> {
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            let result = connection
                .call::<SessionResult>(request)
                .await
                .map_err(|e| e.to_string())?;
            let info = result.session;
            let remembered = info.clone();
            this.update(cx, |this, _| {
                if !this.state.sessions.iter().any(|s| s.id == remembered.id) {
                    this.recent.insert(remembered.id, remembered);
                }
            })
            .ok();
            Ok(info)
        })
    }

    // ---- tasks -----------------------------------------------------------------------------

    /// The daemon's tasks as this window knows them.
    pub fn tasks(&self) -> &TaskBook {
        &self.tasks
    }

    /// Asks for the tasks of every workspace; events that arrive meanwhile win over the answer.
    fn load_tasks(&mut self, cx: &mut Context<Self>) {
        if !self.connection.supports_tasks() {
            return;
        }
        self.tasks.start_loading();
        let listing = self.call(
            Request::TaskList(TaskListRequest::default()),
            |list: TaskList| list.tasks,
            cx,
        );
        self.tasks_loading = Some(cx.spawn(async move |this, cx| {
            let Ok(listed) = listing.await else {
                return;
            };
            this.update(cx, |this, cx| {
                this.tasks.merge_listed(listed);
                this.tasks_loading = None;
                this.changed(StoreChanged::TASKS, cx);
            })
            .ok();
        }));
    }

    /// `task.create`.
    pub fn create_task(
        &self,
        request: TaskCreate,
        cx: &mut Context<Self>,
    ) -> Task<Result<DaemonTask, String>> {
        self.call(
            Request::TaskCreate(request),
            |created: TaskResult| created.task,
            cx,
        )
    }

    /// `task.update` (a state change, or fields).
    pub fn update_task(
        &self,
        request: TaskUpdate,
        cx: &mut Context<Self>,
    ) -> Task<Result<DaemonTask, String>> {
        self.call(
            Request::TaskUpdate(request),
            |updated: TaskResult| updated.task,
            cx,
        )
    }

    /// `task.delete`: the ids that went.
    pub fn delete_task(
        &self,
        id: TaskId,
        cascade: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<TaskId>, String>> {
        self.call(
            Request::TaskDelete(TaskDelete { id, cascade }),
            |deleted: TaskDeleted| deleted.removed,
            cx,
        )
    }

    /// `task.start`. The session is remembered until its events arrive, so the window can show
    /// it at once.
    pub fn start_task(
        &mut self,
        id: TaskId,
        cx: &mut Context<Self>,
    ) -> Task<Result<TaskStarted, String>> {
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            let started = connection
                .call::<TaskStarted>(Request::TaskStart(TaskRef { id }))
                .await
                .map_err(|e| e.to_string())?;
            let remembered = started.session.clone();
            this.update(cx, |this, _| {
                if !this.state.sessions.iter().any(|s| s.id == remembered.id) {
                    this.recent.insert(remembered.id, remembered);
                }
            })
            .ok();
            Ok(started)
        })
    }

    /// `task.review`: accept a done task, or ask for changes (the note goes to its chat).
    pub fn review_task(
        &self,
        id: TaskId,
        decision: ReviewDecision,
        note: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<TaskReviewed, String>> {
        self.call(
            Request::TaskReview(TaskReview { id, decision, note }),
            |reviewed| reviewed,
            cx,
        )
    }

    // ---- worktree commands -----------------------------------------------------------------

    /// `worktree.diff`: what a checkout changed against its base.
    pub fn worktree_diff(
        &self,
        request: WorktreeDiffRequest,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeDiff, String>> {
        self.call(Request::WorktreeDiff(request), |diff| diff, cx)
    }

    /// `worktree.file_diff`: the patch of one changed file.
    pub fn worktree_file_diff(
        &self,
        request: WorktreeFileDiffRequest,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeFileDiff, String>> {
        self.call(Request::WorktreeFileDiff(request), |diff| diff, cx)
    }

    /// `git.log` (capability `git.history`): the commits of a checkout, or of a file or folder.
    pub fn git_log(
        &self,
        request: hutan_protocol::git::GitLog,
        cx: &mut Context<Self>,
    ) -> Task<Result<hutan_protocol::git::GitLogResult, String>> {
        self.call(Request::GitLog(request), |log| log, cx)
    }

    /// `git.blame`: which commit last changed each line of a file.
    pub fn git_blame(
        &self,
        request: hutan_protocol::git::GitBlame,
        cx: &mut Context<Self>,
    ) -> Task<Result<hutan_protocol::git::GitBlameResult, String>> {
        self.call(Request::GitBlame(request), |blame| blame, cx)
    }

    /// `git.show`: one commit's message, files and patch.
    pub fn git_show(
        &self,
        request: hutan_protocol::git::GitShow,
        cx: &mut Context<Self>,
    ) -> Task<Result<hutan_protocol::git::GitShowResult, String>> {
        self.call(Request::GitShow(request), |shown| shown, cx)
    }

    /// `git.file`: a file as it was at a revision.
    pub fn git_file(
        &self,
        request: hutan_protocol::git::GitFile,
        cx: &mut Context<Self>,
    ) -> Task<Result<hutan_protocol::git::GitFileResult, String>> {
        self.call(Request::GitFile(request), |file| file, cx)
    }

    /// `worktree.finish`: the user's Finish of a checkout (commit, merge a worktree, push). A
    /// step that failed is in the answer; the error is a refusal before anything changed.
    pub fn finish_checkout(
        &self,
        request: WorktreeFinish,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeFinished, String>> {
        self.call(Request::WorktreeFinish(request), |done| done, cx)
    }

    /// `worktree.commit`: commits everything in a worktree so a merge takes it.
    pub fn commit_worktree(
        &self,
        request: WorktreeCommit,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeCommitted, String>> {
        self.call(Request::WorktreeCommit(request), |done| done, cx)
    }

    /// `worktree.hunks`: the hunks of some files of a checkout, with their ids (protocol 0.53).
    pub fn worktree_hunks(
        &self,
        request: WorktreeHunksRequest,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeHunks, String>> {
        self.call(Request::WorktreeHunks(request), |done| done, cx)
    }

    /// `worktree.stage` (`stage`) or `worktree.unstage`: files or hunks into or out of git's
    /// index; never the working tree.
    pub fn change_index(
        &self,
        request: WorktreeIndexChange,
        stage: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeIndexed, String>> {
        let request = if stage {
            Request::WorktreeStage(request)
        } else {
            Request::WorktreeUnstage(request)
        };
        self.call(request, |done| done, cx)
    }

    /// `worktree.discard`: throws working-tree changes away after a checkpoint.
    pub fn discard_worktree(
        &self,
        request: WorktreeDiscard,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeDiscarded, String>> {
        self.call(Request::WorktreeDiscard(request), |done| done, cx)
    }

    /// Undo of a discard: previews the checkpoint it took and restores it. Refused while a turn
    /// runs in the checkout (the preview says so).
    pub fn undo_discard(
        &self,
        checkpoint: hutan_core::CheckpointId,
        cx: &mut Context<Self>,
    ) -> Task<Result<CheckpointRestored, String>> {
        let connection = self.connection.clone();
        cx.background_spawn(async move {
            let preview = connection
                .checkpoint_preview(CheckpointPreviewRequest {
                    checkpoint_id: checkpoint,
                    through: None,
                    paths: Vec::new(),
                })
                .await
                .map_err(|e| e.to_string())?;
            if !preview.busy.is_empty() {
                return Err("a turn is running in the checkout: undo when it ends".to_owned());
            }
            connection
                .checkpoint_restore(CheckpointRestore {
                    checkpoint_id: checkpoint,
                    through: None,
                    paths: Vec::new(),
                    accept_conflicts: Vec::new(),
                    plan_digest: preview.plan_digest,
                })
                .await
                .map_err(|e| e.to_string())
        })
    }

    /// `worktree.list`: the worktrees of the repositories of `workspace`.
    pub fn list_worktrees(
        &self,
        workspace: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<WorktreeInfo>, String>> {
        let request = Request::WorktreeList(WorktreeListRequest {
            workspace_id: Some(workspace),
        });
        self.call(request, |list: WorktreeList| list.worktrees, cx)
    }

    /// `worktree.list` for every workspace.
    pub fn list_all_worktrees(
        &self,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<WorktreeInfo>, String>> {
        let request = Request::WorktreeList(WorktreeListRequest { workspace_id: None });
        self.call(request, |list: WorktreeList| list.worktrees, cx)
    }

    /// `worktree.create`.
    pub fn create_worktree(
        &self,
        request: WorktreeCreate,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeInfo, String>> {
        self.call(
            Request::WorktreeCreate(request),
            |created: hutan_protocol::worktree::WorktreeCreated| created.worktree,
            cx,
        )
    }

    /// `worktree.remove`.
    pub fn remove_worktree(
        &self,
        request: WorktreeRemove,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeRemoved, String>> {
        self.call(Request::WorktreeRemove(request), |removed| removed, cx)
    }

    /// `worktree.setup`: runs a worktree's setup again.
    pub fn worktree_setup(
        &self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeInfo, String>> {
        self.call(
            Request::WorktreeSetup(hutan_protocol::worktree::WorktreeSetupRequest { path }),
            |result: hutan_protocol::worktree::WorktreeSetupResult| result.worktree,
            cx,
        )
    }

    /// `worktree.setup_trust` (user-only): confirms the repository's setup commands (by the hash
    /// the user was shown) and runs the setup.
    pub fn worktree_setup_trust(
        &self,
        path: PathBuf,
        hash: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorktreeInfo, String>> {
        self.call(
            Request::WorktreeSetupTrust(hutan_protocol::worktree::WorktreeSetupTrust {
                path,
                hash,
            }),
            |result: hutan_protocol::worktree::WorktreeSetupResult| result.worktree,
            cx,
        )
    }

    /// `worktree.cleanup`: the worktrees the cleanup policy would remove now.
    pub fn worktree_cleanup(
        &self,
        workspace: Option<WorkspaceId>,
        cx: &mut Context<Self>,
    ) -> Task<Result<hutan_protocol::worktree::WorktreeCleanupList, String>> {
        self.call(
            Request::WorktreeCleanup(hutan_protocol::worktree::WorktreeCleanupRequest {
                workspace_id: workspace,
                mode: None,
                idle_days: None,
            }),
            |list| list,
            cx,
        )
    }

    /// `worktree.prune` (user-only): removes exactly `paths` of the worktrees the cleanup policy
    /// lists, teardown first.
    pub fn worktree_prune(
        &self,
        workspace: Option<WorkspaceId>,
        paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Task<Result<hutan_protocol::worktree::WorktreePruned, String>> {
        self.call(
            Request::WorktreePrune(hutan_protocol::worktree::WorktreePrune {
                workspace_id: workspace,
                mode: None,
                idle_days: None,
                paths,
            }),
            |pruned| pruned,
            cx,
        )
    }

    /// The checkouts of the repositories as `worktree.list` last said (setup state included).
    pub fn checkouts_all(&self) -> &[WorktreeInfo] {
        &self.checkouts
    }

    // ---- messages to agents ----------------------------------------------------------------

    /// `chat.send` with attachments: one user message to an agent chat (the turn's events
    /// follow on the chat's own stream).
    pub fn chat_send(
        &self,
        session: SessionId,
        text: String,
        attachments: Vec<hutan_protocol::assistant::Attachment>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let connection = self.connection.clone();
        cx.background_spawn(async move {
            connection
                .chat_send_with(session, text, attachments)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }

    /// `session.send_text` of `text` pasted into the terminal of `session` and submitted: in a
    /// bracketed paste while the application has switched that mode on (or the terminal is not
    /// attached here, so its mode is unknown: the agents that run in terminals all do), else
    /// typed with carriage returns for the line breaks.
    pub fn paste_into_terminal(
        &self,
        session: SessionId,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let bracketed = self.attached(session).is_none_or(|attached| {
            attached
                .read(cx)
                .shared()
                .read(cx)
                .model()
                .modes()
                .bracketed_paste
        });
        let bytes = hutan_terminal_view::paste::paste_bytes(text, bracketed);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let request = Request::SessionSendText(hutan_protocol::ui::SessionSendText {
            session_id: session,
            text,
            enter: true,
        });
        self.call(request, |_: hutan_protocol::ui::SessionTextSent| (), cx)
    }

    // ---- workspace commands ----------------------------------------------------------------

    /// One request on the connection, its answer decoded as `T` and mapped by `done`. The store
    /// itself follows through the events the daemon publishes for the change.
    fn call<T, R>(
        &self,
        request: Request,
        done: impl FnOnce(T) -> R + Send + 'static,
        cx: &mut Context<Self>,
    ) -> Task<Result<R, String>>
    where
        T: serde::de::DeserializeOwned + Send + 'static,
        R: Send + 'static,
    {
        let connection = self.connection.clone();
        cx.background_spawn(async move {
            connection
                .call::<T>(request)
                .await
                .map(done)
                .map_err(|e| e.to_string())
        })
    }

    /// `workspace.open`: the workspace containing `path`, created when there is none.
    pub fn open_workspace(
        &mut self,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorkspaceOpened, String>> {
        let request = Request::WorkspaceOpen(WorkspaceOpen {
            path,
            created_by: None,
        });
        self.call(request, |opened: WorkspaceOpened| opened, cx)
    }

    /// `workspace.recent`: the open and remembered projects, newest first.
    pub fn recent_projects(
        &mut self,
        limit: u32,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<RecentProject>, String>> {
        let request = Request::WorkspaceRecent(WorkspaceRecent { limit: Some(limit) });
        self.call(request, |recent: RecentProjects| recent.projects, cx)
    }

    /// `workspace.move`: `workspace` right before `before`, or last.
    pub fn move_workspace(
        &mut self,
        workspace: WorkspaceId,
        before: Option<WorkspaceId>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspaceMove(WorkspaceMove {
            workspace_id: workspace,
            before,
        });
        self.call(request, |_: serde_json::Value| (), cx)
    }

    pub fn pin_workspace(
        &mut self,
        workspace: WorkspaceId,
        pinned: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspacePin(WorkspacePin {
            workspace_id: workspace,
            pinned,
        });
        self.call(request, |_: WorkspaceInfo| (), cx)
    }

    pub fn rename_workspace(
        &mut self,
        workspace: WorkspaceId,
        name: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspaceRename(WorkspaceRename {
            workspace_id: workspace,
            name,
        });
        self.call(request, |_: WorkspaceInfo| (), cx)
    }

    pub fn add_workspace_root(
        &mut self,
        workspace: WorkspaceId,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspaceAddRoot(WorkspaceRoot {
            workspace_id: workspace,
            path,
        });
        self.call(request, |_: WorkspaceInfo| (), cx)
    }

    pub fn close_workspace(
        &mut self,
        workspace: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspaceClose(WorkspaceRef {
            workspace_id: workspace,
        });
        self.call(request, |_: serde_json::Value| (), cx)
    }

    /// `workspace.create`: a workspace holding the projects of `paths` (made when new) and the
    /// existing `projects`.
    pub fn create_workspace(
        &mut self,
        name: String,
        paths: Vec<PathBuf>,
        projects: Vec<WorkspaceId>,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorkspaceInfo, String>> {
        let request = Request::WorkspaceCreate(WorkspaceCreate {
            name,
            paths,
            project_ids: projects,
            created_by: None,
        });
        self.call(request, |created: WorkspaceInfo| created, cx)
    }

    /// `workspace.assign`: `project` into `workspace`, or out of its workspace when `None`.
    pub fn assign_project(
        &mut self,
        project: WorkspaceId,
        workspace: Option<WorkspaceId>,
        cx: &mut Context<Self>,
    ) -> Task<Result<WorkspaceInfo, String>> {
        let request = Request::WorkspaceAssign(WorkspaceAssign {
            project_id: project,
            workspace_id: workspace,
        });
        self.call(request, |assigned: WorkspaceInfo| assigned, cx)
    }

    /// `workspace.delete`: removes a workspace; its projects stay.
    pub fn delete_workspace(
        &mut self,
        workspace: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let request = Request::WorkspaceDelete(WorkspaceDelete {
            workspace_id: workspace,
        });
        self.call(request, |_: serde_json::Value| (), cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ui_open_is_current_for_fifteen_seconds() {
        assert!(ui_open_is_fresh(1_000, 1_000));
        assert!(ui_open_is_fresh(1_000, 16_000));
        assert!(!ui_open_is_fresh(1_000, 16_001));
        assert!(ui_open_is_fresh(5_000, 1_000), "a future stamp is current");
    }

    fn info(state: hutan_core::SessionState) -> SessionInfo {
        SessionInfo {
            containment: None,
            id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
            kind: hutan_core::SessionKind::Terminal,
            title: "t".into(),
            state,
            command: vec![],
            cwd: None,
            terminal_id: None,
            created_at_ms: 0,
            agent: None,
            restored: false,
            chat: None,
            placement: None,
            folder: None,
            recovery: None,
            muted: false,
        }
    }

    fn report(
        state: hutan_protocol::messages::AgentState,
    ) -> hutan_protocol::messages::AgentReport {
        hutan_protocol::messages::AgentReport {
            source: "hook".into(),
            agent: "claude".into(),
            state,
            message: None,
            agent_session_id: None,
            updated_at_ms: 0,
            blocked_sub: None,
        }
    }

    #[test]
    fn a_terminal_agent_is_busy_while_it_works_or_waits_for_a_decision() {
        use hutan_core::SessionState;
        use hutan_protocol::messages::AgentState;
        let mut terminal = info(SessionState::Running);
        assert!(!agent_busy(&terminal), "a plain shell is not an agent");
        for (state, busy) in [
            (AgentState::Working, true),
            (AgentState::Blocked, true),
            (AgentState::Idle, false),
        ] {
            terminal.agent = Some(report(state));
            assert_eq!(agent_busy(&terminal), busy, "{state:?}");
        }
        // The agent's process went away with its last report still "working".
        terminal.agent = Some(report(AgentState::Working));
        terminal.state = SessionState::Stopped;
        assert!(!agent_busy(&terminal));
    }

    #[test]
    fn activity_touches_the_checkouts_it_happened_in_or_below() {
        let activity = CheckoutActivity {
            session_id: SessionId::new(),
            cause: ActivityCause::CommandFinished,
            places: vec![PathBuf::from("/private/repo/src")],
        };
        let path = std::path::Path::new("/tmp/repo");
        assert!(activity.touches(path, std::path::Path::new("/private/repo")));
        assert!(!activity.touches(path, std::path::Path::new("/private/other")));
        assert!(!activity.touches(
            std::path::Path::new("/private/rep"),
            std::path::Path::new("/private/rep")
        ));
    }
}
