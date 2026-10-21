//! Tasks with dependencies (protocol 0.16, capability `task`): `task.create`, `.list`,
//! `.update`, `.delete`, `.start`, the events `task.updated` / `task.removed`, finish
//! detection, and starting ready tasks by themselves within `[tasks] max_parallel`.
//!
//! The model and the pure rules (state machine, readiness, cycles, ordering, the start queue)
//! live in `hutan-core`; this module owns the daemon's side of them:
//!
//! - **State.** Every task is in memory ([`State`]) and written through to SQLite (`tasks`,
//!   `task_deps`, `task_sessions`) at every change. Only the stored states are written; `ready`
//!   and `blocked` are derived whenever a task is shown. Every change to what clients see is
//!   announced as `task.updated`, including the tasks that became `ready` because a dependency
//!   finished.
//! - **Running.** Starting claims the task (`running`) first, then makes the placement (the main
//!   checkout, or a worktree for the branch through [`WorktreeService`]) and starts the session:
//!   a terminal running `argv` with `HUTAN_TASK_ID` in its environment, or a Codex chat that
//!   gets the prompt as its first message. A failed setup puts the task back as it was.
//! - **Finishing.** The service watches the event bus ([`EventBus::set_task_observer`]): the
//!   session's exit (0 → `done`, anything else → `failed`, a stop → `cancelled`), the main
//!   agent of a terminal session going idle after a working stretch (`done`) or failing, and
//!   the end of a chat turn. A reconciliation pass every few seconds looks at the sessions of
//!   running tasks directly, so a dropped event delays a finish instead of losing it.
//! - **Restart.** A task that was `running` when the daemon stopped stays `running` when one of
//!   its sessions was restored alive, and is `failed` with outcome `interrupted` otherwise.
//! - **Intents.** `task_done` / `task_failed` are raised by [`crate::notify`] from the
//!   `task.updated` events.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use hutan_core::{
    AgentEvent, AgentEventKind, AgentNode, Delivery, DeliveryActor, DeliveryRecord,
    DeliveryState, NodeKind, NodeOutcome, NodeState, ReviewDecision, SessionId, SessionState,
    Task, TaskEvent, TaskId, TaskPlacement, TaskPlacementKind, TaskRun, TaskState, TurnId,
    WorkspaceId, task as model,
};
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{
    AgentState, Event, SessionCreate, SessionInfo, SessionListFilter, SessionSpec, TerminalSpec,
    WorkspaceInfo, WorkspaceKind,
};
use hutan_protocol::recovery::Lost;
use hutan_protocol::task::{
    TaskCreate, TaskDelete, TaskDeleted, TaskList, TaskListRequest, TaskReview, TaskReviewed,
    TaskStarted, TaskUpdate,
};
use hutan_protocol::worktree::{
    MergeStrategy, WorktreeCreate, WorktreeListRequest, WorktreeMerged,
};
use hutan_storage::Storage;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::bus::EventBus;
use crate::checkpoints::turns::TurnCheckpoints;
use crate::chat::{AGENT_CODEX, is_gateway_agent};
use crate::config::TasksSettings;
use crate::harness::is_harness_agent;
use crate::placement::PlacementCache;
use crate::projects;
use crate::registry::{RegistryHandle, log_write, now_ms};
use crate::worktrees::{RemovedWorktree, WorktreeService};

/// Queue between the event bus and the service task.
pub const OBSERVER_QUEUE: usize = 1024;
/// How often the sessions of running tasks are looked at directly (the safety net for events
/// that were dropped).
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
/// Environment variable naming the task in the sessions it starts.
pub const TASK_ID_ENV: &str = "HUTAN_TASK_ID";
/// Longest outcome text.
const OUTCOME_MAX_CHARS: usize = 160;

/// One task and what the daemon remembers about it only while it runs.
#[derive(Debug, Clone)]
struct Slot {
    /// With the stored state (`todo`, `running`, `done`, `failed`, `cancelled`).
    task: Task,
    /// The agent in the running task's session waits for the user.
    blocked: bool,
    /// The main agent of the session was seen working since the task started.
    working_seen: bool,
    /// Chat turns of the task's session that started since the daemon did: a judgement of a
    /// turn that ended is dropped when another one started meanwhile.
    turns: u64,
    /// The turn running in the task's chat sent a `file_diff` (what a turn changed when no
    /// checkpoint can say).
    turn_diffs: bool,
}

impl Slot {
    fn new(task: Task) -> Self {
        Self {
            task,
            blocked: false,
            working_seen: false,
            turns: 0,
            turn_diffs: false,
        }
    }
}

/// Everything in memory, behind one lock that is never held across an `await`.
#[derive(Debug, Default)]
struct State {
    tasks: HashMap<TaskId, Slot>,
    /// The session of every running task.
    by_session: HashMap<SessionId, TaskId>,
    /// Who last gave each task's chat a message to send (`chat.send`, `chat.queue.add`, …):
    /// the actor of the record when that message's turn re-opens the task's delivery.
    senders: HashMap<SessionId, DeliveryActor>,
    scheduler: model::Scheduler,
    /// What clients were last told about each task.
    published: HashMap<TaskId, Task>,
    /// Creation times are strictly increasing, which makes them a total order.
    last_created_ms: u64,
}

impl State {
    fn new(tasks: Vec<Task>) -> Self {
        let last_created_ms = tasks.iter().map(|t| t.created_at_ms).max().unwrap_or(0);
        let mut state = Self {
            tasks: tasks
                .into_iter()
                .map(|task| (task.id, Slot::new(task)))
                .collect(),
            last_created_ms,
            ..Self::default()
        };
        state.published = state.views().into_iter().map(|t| (t.id, t)).collect();
        state
    }

    /// The state of `id` as shown: `ready` and `blocked` derived.
    fn effective(&self, id: TaskId) -> Option<TaskState> {
        let slot = self.tasks.get(&id)?;
        Some(model::effective_state(
            slot.task.state,
            slot.blocked,
            &slot.task.depends_on,
            slot.task.after_policy,
            |dep| {
                self.tasks
                    .get(&dep)
                    .map(|d| (d.task.state, d.task.delivery))
            },
        ))
    }

    /// The task as clients see it.
    fn view(&self, id: TaskId) -> Option<Task> {
        let state = self.effective(id)?;
        let mut task = self.tasks.get(&id)?.task.clone();
        task.state = state;
        Some(task)
    }

    /// Every task as clients see it, dependencies first, then oldest first.
    fn views(&self) -> Vec<Task> {
        model::topological_order(self.tasks.values().map(|slot| &slot.task))
            .into_iter()
            .filter_map(|id| self.view(id))
            .collect()
    }

    fn edges(&self) -> HashMap<TaskId, Vec<TaskId>> {
        self.tasks
            .iter()
            .map(|(id, slot)| (*id, slot.task.depends_on.clone()))
            .collect()
    }

    /// Tasks whose session is running (`running` or `blocked`).
    fn active_count(&self) -> usize {
        self.tasks
            .values()
            .filter(|slot| slot.task.state.is_active())
            .count()
    }

    fn label(&self, id: TaskId) -> String {
        self.tasks
            .get(&id)
            .map_or_else(|| id.to_string(), |slot| format!("`{}`", slot.task.title))
    }

    /// The chat task whose session is `session`: the running one it works on, else a chat
    /// task that ran in it last and is `done` (the user goes on with the conversation).
    fn chat_task(&self, session: SessionId) -> Option<TaskId> {
        if let Some(id) = self.by_session.get(&session) {
            return Some(*id);
        }
        self.tasks
            .values()
            .filter(|slot| {
                slot.task.state == TaskState::Done
                    && matches!(slot.task.run, Some(TaskRun::Chat { .. }))
                    && slot.task.sessions.last() == Some(&session)
            })
            .max_by_key(|slot| slot.task.created_at_ms)
            .map(|slot| slot.task.id)
    }

    /// Forgets who sent to sessions no task ran in any more.
    fn prune_senders(&mut self) {
        let Self { tasks, senders, .. } = self;
        senders.retain(|session, _| {
            tasks
                .values()
                .any(|slot| slot.task.sessions.contains(session))
        });
    }

    /// The tasks that depend on `id`, directly or not, dependencies first.
    fn dependants_of(&self, id: TaskId) -> Vec<TaskId> {
        let mut found: HashSet<TaskId> = HashSet::new();
        let mut frontier = vec![id];
        while let Some(current) = frontier.pop() {
            for (other, slot) in &self.tasks {
                if slot.task.depends_on.contains(&current) && found.insert(*other) {
                    frontier.push(*other);
                }
            }
        }
        model::topological_order(
            self.tasks
                .values()
                .map(|slot| &slot.task)
                .filter(|task| found.contains(&task.id)),
        )
    }

    /// Checks a dependency list for `task` (a task not yet created has no entry) and returns it
    /// without duplicates.
    fn check_dependencies(
        &self,
        task: TaskId,
        workspace: WorkspaceId,
        deps: &[TaskId],
    ) -> Result<Vec<TaskId>, ErrorBody> {
        if deps.len() > model::MAX_DEPENDENCIES {
            return Err(bad_request(format!(
                "a task depends on at most {} tasks, not {}",
                model::MAX_DEPENDENCIES,
                deps.len()
            )));
        }
        let mut unique: Vec<TaskId> = Vec::with_capacity(deps.len());
        for dep in deps {
            if unique.contains(dep) {
                continue;
            }
            if *dep == task {
                return Err(bad_request(
                    "dependency cycle: a task cannot depend on itself",
                ));
            }
            let Some(slot) = self.tasks.get(dep) else {
                return Err(ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("dependency {dep} not found"),
                ));
            };
            if slot.task.workspace_id != workspace {
                return Err(bad_request(format!(
                    "dependency {} belongs to another workspace",
                    self.label(*dep)
                )));
            }
            unique.push(*dep);
        }
        if let Some(cycle) = model::find_cycle(&self.edges(), task, &unique) {
            let names: Vec<String> = cycle.iter().map(|id| self.label(*id)).collect();
            return Err(bad_request(format!(
                "dependency cycle: {}",
                names.join(" -> ")
            )));
        }
        Ok(unique)
    }

    /// Why `id` cannot be started now.
    fn not_ready(&self, id: TaskId) -> ErrorBody {
        let Some(slot) = self.tasks.get(&id) else {
            return not_found(id);
        };
        let shown = self.effective(id).unwrap_or(slot.task.state);
        let message = match shown {
            TaskState::Running | TaskState::Blocked => "the task is already running".to_owned(),
            TaskState::Done => "the task is done; set its state to todo to run it again".to_owned(),
            TaskState::Failed => format!(
                "the task failed ({}); set its state to todo to run it again",
                slot.task.outcome.as_deref().unwrap_or("no reason recorded")
            ),
            TaskState::Cancelled => {
                "the task was cancelled; set its state to todo to run it again".to_owned()
            }
            TaskState::Todo | TaskState::Ready => {
                let policy = slot.task.after_policy;
                let waiting: Vec<String> = slot
                    .task
                    .depends_on
                    .iter()
                    .filter(|dep| {
                        !self
                            .tasks
                            .get(dep)
                            .is_some_and(|d| policy.satisfied_by(d.task.state, d.task.delivery))
                    })
                    .map(|dep| {
                        let state = self.effective(*dep).map_or("gone", TaskState::as_str);
                        match self.tasks.get(dep) {
                            // The dependency ran; the policy wants more than that.
                            Some(d) if d.task.state == TaskState::Done => format!(
                                "{} ({state}, {}; waits for {policy})",
                                self.label(*dep),
                                d.task.delivery
                            ),
                            _ => format!("{} ({state})", self.label(*dep)),
                        }
                    })
                    .collect();
                format!("the task is waiting for {}", waiting.join(", "))
            }
        };
        ErrorBody::new(ErrorCode::InvalidState, message)
    }

    /// Queues the ready tasks that start by themselves in creation order behind the ones
    /// already waiting, and drops the waiting tasks that no longer qualify.
    fn refill_queue(&mut self) {
        let wants = |state: &Self, id: TaskId| {
            state
                .tasks
                .get(&id)
                .is_some_and(|slot| slot.task.auto_start)
                && state.effective(id) == Some(TaskState::Ready)
        };
        let waiting: Vec<TaskId> = self.scheduler.waiting().collect();
        for id in waiting {
            if !wants(self, id) {
                self.scheduler.remove(id);
            }
        }
        let order = model::topological_order(self.tasks.values().map(|slot| &slot.task));
        for id in order {
            if wants(self, id) {
                self.scheduler.enqueue(id);
            }
        }
    }
}

/// The task service: [`State`] on one side, the registry, the worktrees, storage and the bus
/// on the other.
#[derive(Debug)]
pub struct TaskService {
    registry: RegistryHandle,
    worktrees: Arc<WorktreeService>,
    /// What each chat turn changed (ADR 0019): whether it left work to review.
    checkpoints: Arc<TurnCheckpoints>,
    storage: Storage,
    bus: Arc<EventBus>,
    /// `[tasks] max_parallel`; changed live by `daemon.settings.set`.
    max_parallel: AtomicUsize,
    state: Mutex<State>,
}

impl TaskService {
    /// Loads the stored tasks and settles the ones that were running when the daemon stopped:
    /// they stay `running` when one of their sessions was restored alive and are `failed`
    /// (`interrupted`) otherwise. Call after the registry restored its sessions.
    pub async fn open(
        registry: RegistryHandle,
        worktrees: Arc<WorktreeService>,
        checkpoints: Arc<TurnCheckpoints>,
        storage: Storage,
        bus: Arc<EventBus>,
        settings: &TasksSettings,
        stored: Vec<Task>,
    ) -> Result<Arc<Self>, ErrorBody> {
        let this = Arc::new(Self {
            registry,
            worktrees,
            checkpoints,
            storage,
            bus,
            max_parallel: AtomicUsize::new(settings.max_parallel),
            state: Mutex::new(State::new(stored)),
        });
        let sessions = this
            .registry
            .list_sessions(SessionListFilter::default())
            .await?
            .sessions;
        let mut lost = Vec::new();
        {
            let mut st = this.state.lock();
            let running: Vec<TaskId> = st
                .tasks
                .values()
                .filter(|slot| slot.task.state == TaskState::Running)
                .map(|slot| slot.task.id)
                .collect();
            for id in running {
                let alive = st.tasks.get(&id).and_then(|slot| {
                    slot.task
                        .sessions
                        .iter()
                        .rev()
                        .find(|session| {
                            sessions
                                .iter()
                                .any(|s| s.id == **session && s.state.is_live())
                        })
                        .copied()
                });
                match alive {
                    Some(session) => {
                        info!(task = %id, %session, "a running task keeps its restored session");
                        st.by_session.insert(session, id);
                    }
                    None => {
                        info!(task = %id, "a running task lost its session with the last daemon");
                        // The session that carried it says so (`session.recovery`).
                        if let Some(slot) = st.tasks.get(&id)
                            && let Some(session) = slot
                                .task
                                .sessions
                                .iter()
                                .rev()
                                .find(|session| sessions.iter().any(|s| s.id == **session))
                        {
                            lost.push((
                                *session,
                                Lost::Task {
                                    task_id: id,
                                    title: slot.task.title.clone(),
                                },
                            ));
                        }
                        this.apply(&mut st, id, TaskEvent::Fail, "interrupted".to_owned());
                    }
                }
            }
        }
        for (session, lost) in lost {
            this.registry.note_recovery_loss(session, lost).await;
        }
        Ok(this)
    }

    /// The most tasks that start by themselves at once, now.
    pub fn max_parallel(&self) -> usize {
        self.max_parallel.load(Ordering::Relaxed)
    }

    /// Starts ready tasks within `max_parallel` from now on; a raised limit lets waiting
    /// tasks start at once.
    pub fn set_max_parallel(self: &Arc<Self>, max_parallel: usize) {
        self.max_parallel.store(max_parallel, Ordering::Relaxed);
        self.schedule();
    }

    /// Watches the event bus until `shutdown`, and starts the ready tasks that start by
    /// themselves.
    pub fn run(self: &Arc<Self>, shutdown: CancellationToken) -> JoinHandle<()> {
        let (tx, mut rx) = mpsc::channel(OBSERVER_QUEUE);
        self.bus.set_task_observer(Some(tx));
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut reconcile = tokio::time::interval(RECONCILE_INTERVAL);
            reconcile.set_missed_tick_behavior(MissedTickBehavior::Delay);
            this.schedule();
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => break,
                    event = rx.recv() => match event {
                        Some(event) => this.handle(event),
                        None => break,
                    },
                    _ = reconcile.tick() => this.reconcile().await,
                }
            }
            this.bus.set_task_observer(None);
        })
    }

    // ---- requests -------------------------------------------------------------------------

    pub async fn create(self: &Arc<Self>, req: TaskCreate) -> Result<Task, ErrorBody> {
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        if !workspaces.iter().any(|w| w.id == req.workspace_id) {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("workspace {} not found", req.workspace_id),
            ));
        }
        let title = clean_title(&req.title)?;
        let prompt = clean_prompt(req.prompt)?;
        let placement = req.placement.unwrap_or_default();
        check_placement(&placement)?;
        check_run(req.run.as_ref(), prompt.as_deref())?;
        let id = TaskId::new();
        let task = {
            let mut st = self.state.lock();
            let depends_on = st.check_dependencies(id, req.workspace_id, &req.depends_on)?;
            let created_at_ms = now_ms().max(st.last_created_ms + 1);
            st.last_created_ms = created_at_ms;
            let task = Task {
                id,
                workspace_id: req.workspace_id,
                title,
                prompt,
                state: TaskState::Todo,
                depends_on,
                placement,
                run: req.run,
                auto_start: req.auto_start,
                sessions: Vec::new(),
                created_at_ms,
                started_at_ms: None,
                ended_at_ms: None,
                outcome: None,
                delivery: DeliveryState::None,
                after_policy: req.after_policy,
                delivery_history: Vec::new(),
            };
            st.tasks.insert(id, Slot::new(task));
            self.commit(&mut st, id);
            st.view(id)
        };
        self.schedule();
        task.ok_or_else(|| not_found(id))
    }

    pub fn list(&self, req: TaskListRequest) -> TaskList {
        let st = self.state.lock();
        TaskList {
            tasks: st
                .views()
                .into_iter()
                .filter(|task| req.workspace_id.is_none_or(|ws| task.workspace_id == ws))
                .filter(|task| req.state.is_none_or(|state| task.state == state))
                .collect(),
        }
    }

    pub async fn update(self: &Arc<Self>, req: TaskUpdate) -> Result<Task, ErrorBody> {
        let title = req.title.as_deref().map(clean_title).transpose()?;
        let prompt = req.prompt.map(|p| clean_prompt(Some(p))).transpose()?;
        if let Some(placement) = &req.placement {
            check_placement(placement)?;
        }
        let changes_fields = title.is_some()
            || prompt.is_some()
            || req.depends_on.is_some()
            || req.placement.is_some()
            || req.run.is_some()
            || req.after_policy.is_some()
            || req.auto_start.is_some();
        let (task, to_stop) = {
            let mut st = self.state.lock();
            let Some(slot) = st.tasks.get(&req.id) else {
                return Err(not_found(req.id));
            };
            let stored = slot.task.state;
            if changes_fields && stored.is_active() {
                return Err(ErrorBody::new(
                    ErrorCode::InvalidState,
                    "the task is running; cancel it before changing it",
                ));
            }
            // Everything is checked before anything changes.
            let run = req.run.clone().or_else(|| slot.task.run.clone());
            let new_prompt = match &prompt {
                Some(p) => p.clone(),
                None => slot.task.prompt.clone(),
            };
            if changes_fields {
                check_run(run.as_ref(), new_prompt.as_deref())?;
            }
            let depends_on = match &req.depends_on {
                Some(deps) => Some(st.check_dependencies(req.id, slot.task.workspace_id, deps)?),
                None => None,
            };
            let event = match req.state {
                None => None,
                Some(TaskState::Cancelled) => Some((TaskEvent::Cancel, "cancelled")),
                Some(TaskState::Done) => Some((TaskEvent::Complete, "marked done")),
                Some(TaskState::Todo) => Some((TaskEvent::Reset, "")),
                Some(other) => {
                    return Err(bad_request(format!(
                        "a task's state can be set to cancelled, done or todo, not {other}"
                    )));
                }
            };
            if let Some((event, _)) = event {
                stored
                    .transition(event)
                    .map_err(|e| ErrorBody::new(ErrorCode::InvalidState, e.to_string()))?;
            }

            let mut to_stop = Vec::new();
            if let Some(slot) = st.tasks.get_mut(&req.id) {
                if let Some(title) = title {
                    slot.task.title = title;
                }
                if let Some(prompt) = prompt {
                    slot.task.prompt = prompt;
                }
                if let Some(deps) = depends_on {
                    slot.task.depends_on = deps;
                }
                if let Some(placement) = req.placement {
                    slot.task.placement = placement;
                }
                if let Some(run) = req.run {
                    slot.task.run = Some(run);
                }
                if let Some(auto) = req.auto_start {
                    slot.task.auto_start = auto;
                }
                if let Some(policy) = req.after_policy {
                    slot.task.after_policy = policy;
                }
            }
            if let Some((event, outcome)) = event {
                if event == TaskEvent::Cancel {
                    to_stop = st
                        .by_session
                        .iter()
                        .filter(|(_, task)| **task == req.id)
                        .map(|(session, _)| *session)
                        .collect();
                }
                self.apply(&mut st, req.id, event, outcome.to_owned());
            }
            self.commit(&mut st, req.id);
            (st.view(req.id), to_stop)
        };
        for session in to_stop {
            if let Err(error) = self.registry.stop_session(session).await {
                debug!(%session, message = %error.message, "the session of a cancelled task was not stopped");
            }
        }
        self.schedule();
        task.ok_or_else(|| not_found(req.id))
    }

    pub fn delete(self: &Arc<Self>, req: TaskDelete) -> Result<TaskDeleted, ErrorBody> {
        let removed = {
            let mut st = self.state.lock();
            if !st.tasks.contains_key(&req.id) {
                return Err(not_found(req.id));
            }
            let dependants = st.dependants_of(req.id);
            if !dependants.is_empty() && !req.cascade {
                let names: Vec<String> = dependants.iter().map(|id| st.label(*id)).collect();
                return Err(ErrorBody::new(
                    ErrorCode::Conflict,
                    format!(
                        "{} task(s) depend on this one: {}; delete them too with cascade",
                        names.len(),
                        names.join(", ")
                    ),
                ));
            }
            let mut doomed = vec![req.id];
            doomed.extend(dependants);
            if let Some(running) = doomed
                .iter()
                .find(|id| st.tasks.get(id).is_some_and(|s| s.task.state.is_active()))
            {
                return Err(ErrorBody::new(
                    ErrorCode::InvalidState,
                    format!(
                        "the task {} is running; cancel it before deleting",
                        st.label(*running)
                    ),
                ));
            }
            // The tasks that depend on others go first, so no edge outlives its task.
            doomed.reverse();
            for id in &doomed {
                self.remove(&mut st, *id);
            }
            doomed
        };
        self.schedule();
        Ok(TaskDeleted { removed })
    }

    pub async fn start(self: &Arc<Self>, id: TaskId) -> Result<TaskStarted, ErrorBody> {
        let task = {
            let mut st = self.state.lock();
            self.claim(&mut st, id)?
        };
        match self.run_claimed(&task).await {
            Ok((session, warnings)) => {
                let st = self.state.lock();
                let task = st.view(id).ok_or_else(|| not_found(id))?;
                Ok(TaskStarted {
                    task,
                    session,
                    warnings,
                })
            }
            Err(error) => {
                {
                    let mut st = self.state.lock();
                    self.unclaim(&mut st, id);
                }
                // The slot is free again.
                self.schedule();
                Err(error)
            }
        }
    }

    // ---- starting -------------------------------------------------------------------------

    /// `running`, with the bookkeeping of a new run. The task must be ready.
    fn claim(&self, st: &mut State, id: TaskId) -> Result<Task, ErrorBody> {
        if st.effective(id) != Some(TaskState::Ready) {
            return Err(st.not_ready(id));
        }
        st.scheduler.remove(id);
        if let Some(slot) = st.tasks.get_mut(&id) {
            slot.task.state = TaskState::Running;
            slot.task.started_at_ms = Some(now_ms());
            slot.task.ended_at_ms = None;
            slot.task.outcome = None;
            slot.blocked = false;
            slot.working_seen = false;
        }
        self.commit(st, id);
        st.tasks
            .get(&id)
            .map(|slot| slot.task.clone())
            .ok_or_else(|| not_found(id))
    }

    /// A start that failed leaves the task as it was before: `todo`, without a run.
    fn unclaim(&self, st: &mut State, id: TaskId) {
        let sessions: Vec<SessionId> = st
            .by_session
            .iter()
            .filter(|(_, task)| **task == id)
            .map(|(session, _)| *session)
            .collect();
        for session in &sessions {
            st.by_session.remove(session);
        }
        if let Some(slot) = st.tasks.get_mut(&id)
            && slot.task.state.is_active()
        {
            slot.task.state = TaskState::Todo;
            slot.task.started_at_ms = None;
            slot.task.sessions.retain(|s| !sessions.contains(s));
            slot.blocked = false;
        }
        self.commit(st, id);
    }

    /// Starts a task `schedule` claimed; a task that cannot be started is `failed`.
    async fn auto_run(self: Arc<Self>, task: Task) {
        let id = task.id;
        if let Err(error) = self.run_claimed(&task).await {
            warn!(task = %id, message = %error.message, "a task could not be started");
            let mut st = self.state.lock();
            self.apply(
                &mut st,
                id,
                TaskEvent::Fail,
                clip(&format!("start failed: {}", error.message)),
            );
            drop(st);
        }
        self.schedule();
    }

    /// Starts the ready tasks that start by themselves while slots are free.
    fn schedule(self: &Arc<Self>) {
        let claimed: Vec<Task> = {
            let mut st = self.state.lock();
            st.refill_queue();
            let active = st.active_count();
            let next = st
                .scheduler
                .take(active, self.max_parallel.load(Ordering::Relaxed));
            next.into_iter()
                .filter_map(|id| self.claim(&mut st, id).ok())
                .collect()
        };
        for task in claimed {
            let this = Arc::clone(self);
            tokio::spawn(async move { this.auto_run(task).await });
        }
    }

    /// Makes the placement and starts the session of a claimed task; the session is linked to
    /// the task before the agent gets its first message.
    async fn run_claimed(
        self: &Arc<Self>,
        task: &Task,
    ) -> Result<(SessionInfo, Vec<String>), ErrorBody> {
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let workspace = workspaces
            .iter()
            .find(|w| w.id == task.workspace_id)
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("workspace {} not found", task.workspace_id),
                )
            })?;
        let (cwd, warnings) = match task.placement.kind {
            TaskPlacementKind::Main => {
                let cwd = main_directory(workspace)?;
                let warnings = self.sharing_warnings(&cwd).await;
                (cwd, warnings)
            }
            TaskPlacementKind::Worktree => (self.worktree_for(task, workspace).await?, Vec::new()),
        };
        // A task of a workspace (the user's container) runs in the project of its folder, which
        // the workspace holds; a task of a project, or without automatic projects, in the
        // workspace it names.
        let session_workspace = (workspace.kind != WorkspaceKind::Workspace
            || !self.registry.auto_projects())
        .then_some(task.workspace_id);
        let session = match &task.run {
            Some(TaskRun::Chat { agent, model }) => {
                self.start_chat(task, session_workspace, agent, model.clone(), cwd)
                    .await?
            }
            run => {
                let argv = match run {
                    Some(TaskRun::Terminal { argv }) => argv.clone(),
                    _ => Vec::new(),
                };
                self.start_terminal(task, session_workspace, argv, cwd)
                    .await?
            }
        };
        // The process may have ended before the link existed; nothing would announce it then.
        self.reconcile().await;
        Ok((session, warnings))
    }

    async fn start_terminal(
        &self,
        task: &Task,
        workspace_id: Option<WorkspaceId>,
        argv: Vec<String>,
        cwd: PathBuf,
    ) -> Result<SessionInfo, ErrorBody> {
        let spec = TerminalSpec {
            command: argv,
            cwd: Some(cwd),
            env: BTreeMap::from([(TASK_ID_ENV.to_owned(), task.id.to_string())]),
            title: Some(task.title.clone()),
            size: None,
        };
        let session = self
            .registry
            .create_session(SessionCreate {
                workspace_id,
                spec: SessionSpec::Terminal(spec),
            })
            .await?;
        self.link(task.id, session.id);
        Ok(session)
    }

    async fn start_chat(
        &self,
        task: &Task,
        workspace_id: Option<WorkspaceId>,
        agent: &str,
        model: Option<String>,
        cwd: PathBuf,
    ) -> Result<SessionInfo, ErrorBody> {
        let prompt = task
            .prompt
            .clone()
            .ok_or_else(|| bad_request("a chat task needs a prompt"))?;
        let session = self
            .registry
            .create_chat(ChatCreate {
                workspace_id,
                cwd,
                agent: agent.to_owned(),
                model,
                approval_policy: None,
                sandbox: None,
                title: Some(task.title.clone()),
                mode: None,
                provider_id: None,
                system_prompt: None,
            })
            .await?;
        self.link(task.id, session.id);
        if let Err(error) = self.registry.chats().send(session.id, prompt, None).await {
            if let Err(stop) = self.registry.stop_session(session.id).await {
                debug!(message = %stop.message, "the chat of a task that did not start was not stopped");
            }
            return Err(error);
        }
        Ok(session)
    }

    /// Records that `session` works on `task`.
    fn link(&self, task: TaskId, session: SessionId) {
        let mut st = self.state.lock();
        if let Some(slot) = st.tasks.get_mut(&task)
            && slot.task.state.is_active()
        {
            slot.task.sessions.push(session);
            st.by_session.insert(session, task);
            self.commit(&mut st, task);
        }
    }

    /// The directory of the worktree of the task's branch: the existing one, else a new one.
    async fn worktree_for(
        &self,
        task: &Task,
        workspace: &WorkspaceInfo,
    ) -> Result<PathBuf, ErrorBody> {
        let branch = task
            .placement
            .branch
            .clone()
            .unwrap_or_else(|| model::default_branch(&task.title));
        let listed = self
            .worktrees
            .list(WorktreeListRequest {
                workspace_id: Some(workspace.id),
            })
            .await?
            .worktrees;
        // The repository `worktree.create` would use: the first of the workspace.
        if let Some(repo) = listed.first().map(|w| w.repo_root.clone())
            && let Some(existing) = listed
                .iter()
                .find(|w| w.repo_root == repo && w.branch.as_deref() == Some(branch.as_str()))
        {
            if existing.main {
                return Err(ErrorBody::new(
                    ErrorCode::InvalidState,
                    format!(
                        "branch `{branch}` is checked out in the main checkout {}; a worktree \
                         needs a branch that is not (use main placement, or another branch)",
                        existing.path.display()
                    ),
                ));
            }
            return Ok(existing.path.clone());
        }
        let created = self
            .worktrees
            .create(WorktreeCreate {
                workspace_id: workspace.id,
                repo: None,
                branch,
                base: task.placement.base.clone(),
                path: None,
            })
            .await?;
        Ok(created.worktree.path)
    }

    /// A line when other live agents already work in the checkout `cwd` belongs to.
    async fn sharing_warnings(&self, cwd: &std::path::Path) -> Vec<String> {
        let dir = cwd.to_owned();
        let root = tokio::task::spawn_blocking(move || {
            PlacementCache::default().checkout(&dir).map(|c| c.root)
        })
        .await
        .ok()
        .flatten();
        let Some(root) = root else {
            return Vec::new();
        };
        let Ok(list) = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await
        else {
            return Vec::new();
        };
        let others: Vec<String> = list
            .sessions
            .iter()
            .filter(|s| s.state.is_live())
            .filter(|s| {
                s.agent.is_some()
                    || s.chat
                        .as_ref()
                        .is_some_and(|chat| !is_gateway_agent(&chat.agent))
            })
            .filter(|s| s.placement.as_ref().is_some_and(|p| p.root == root))
            .map(|s| format!("{} ({})", s.title, s.id))
            .collect();
        if others.is_empty() {
            return Vec::new();
        }
        vec![format!(
            "{} other live agent(s) already work in {}: {}; their edits and this task's can \
             overwrite each other",
            others.len(),
            root.display(),
            others.join(", ")
        )]
    }

    // ---- finishing ------------------------------------------------------------------------

    fn handle(self: &Arc<Self>, event: Event) {
        let changed = {
            let mut st = self.state.lock();
            match event {
                Event::SessionUpdated { session } => self.observe_session(&mut st, &session),
                Event::AgentTreeUpdated {
                    session_id,
                    nodes_changed,
                    ..
                } => self.observe_tree(&mut st, session_id, &nodes_changed),
                Event::ChatEvent { session_id, event } if event.delivery == Delivery::Live => {
                    self.observe_chat(&mut st, session_id, &event)
                }
                Event::WorkspaceClosed { workspace_id } => {
                    self.forget_workspace(&mut st, workspace_id);
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.schedule();
        }
    }

    /// Looks at the sessions of running tasks as they are now.
    async fn reconcile(self: &Arc<Self>) {
        let sessions: HashSet<SessionId> = {
            let st = self.state.lock();
            st.by_session.keys().copied().collect()
        };
        if sessions.is_empty() {
            return;
        }
        let Ok(list) = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await
        else {
            return;
        };
        let changed = {
            let mut st = self.state.lock();
            let mut changed = false;
            for info in list.sessions.iter().filter(|s| sessions.contains(&s.id)) {
                changed |= self.observe_session(&mut st, info);
            }
            changed
        };
        if changed {
            self.schedule();
        }
    }

    /// What a session's state says about its task: how it ended, or whether its agent waits.
    fn observe_session(&self, st: &mut State, info: &SessionInfo) -> bool {
        let Some(&id) = st.by_session.get(&info.id) else {
            return false;
        };
        if let Some((event, outcome)) = session_outcome(&info.state) {
            // A process that exits 0 left work to review only in a worktree; an agent's
            // chat (a Chat run) is judged by `reviewable`.
            return self.apply_run(st, id, event, outcome, false);
        }
        let blocked = info.state == SessionState::WaitingApproval
            || info
                .agent
                .as_ref()
                .is_some_and(|agent| agent.state == AgentState::Blocked)
            || info
                .chat
                .as_ref()
                .is_some_and(|chat| chat.pending_approvals > 0 || chat.pending_questions > 0);
        match st.tasks.get_mut(&id) {
            Some(slot) if slot.blocked != blocked && slot.task.state.is_active() => {
                slot.blocked = blocked;
                self.commit(st, id);
                true
            }
            _ => false,
        }
    }

    /// The main agent of a terminal task's session: a working stretch that ends idle is a finish.
    fn observe_tree(&self, st: &mut State, session: SessionId, nodes: &[AgentNode]) -> bool {
        let Some(&id) = st.by_session.get(&session) else {
            return false;
        };
        let Some(slot) = st.tasks.get_mut(&id) else {
            return false;
        };
        // A chat task finishes with its turn, not with the agent tree Hutan itself reports.
        if matches!(slot.task.run, Some(TaskRun::Chat { .. })) {
            return false;
        }
        let mut finished = None;
        for node in nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Main && n.parent.is_none())
        {
            match (node.state, node.outcome) {
                (NodeState::Working | NodeState::Blocked, _) => slot.working_seen = true,
                (NodeState::Idle, None | Some(NodeOutcome::Completed)) if slot.working_seen => {
                    finished = Some((TaskEvent::Complete, "agent finished".to_owned()));
                }
                (NodeState::Idle, Some(NodeOutcome::Failed)) => {
                    finished = Some((TaskEvent::Fail, "agent failed".to_owned()));
                }
                _ => {}
            }
        }
        match finished {
            Some((event, outcome)) => self.apply_run(st, id, event, outcome, true),
            None => false,
        }
    }

    /// A live event of a task's chat. A turn that starts in the chat of a `done` task re-opens
    /// its work (`awaiting_review` → `none`, in the name of whoever sent the message); a
    /// `file_diff` is remembered for the turn; the end of a turn finishes a running task and
    /// has the turn judged for review ([`judge_turn`](Self::judge_turn)).
    fn observe_chat(self: &Arc<Self>, st: &mut State, session: SessionId, event: &AgentEvent) -> bool {
        let Some(id) = st.chat_task(session) else {
            return false;
        };
        let running = st.by_session.contains_key(&session);
        match &event.kind {
            AgentEventKind::TurnStarted => {
                let by = st
                    .senders
                    .get(&session)
                    .copied()
                    .unwrap_or(DeliveryActor::Daemon);
                let Some(slot) = st.tasks.get_mut(&id) else {
                    return false;
                };
                slot.turns += 1;
                slot.turn_diffs = false;
                if running || slot.task.delivery != DeliveryState::AwaitingReview {
                    return false;
                }
                // The user went on with the conversation: what waited is not the work any
                // more; the next turn that changes files has it judged again.
                slot.task
                    .record_delivery(DeliveryRecord::new(DeliveryState::None, by, now_ms()));
                self.commit(st, id);
                true
            }
            AgentEventKind::FileDiff { changes, patch } => {
                let changed =
                    !changes.is_empty() || patch.as_deref().is_some_and(|p| !p.trim().is_empty());
                if changed && let Some(slot) = st.tasks.get_mut(&id) {
                    slot.turn_diffs = true;
                }
                false
            }
            kind => {
                let Some((task_event, outcome)) = chat_outcome(kind) else {
                    return false;
                };
                let moved = running && self.apply_run(st, id, task_event, outcome, true);
                // The run that finished `done`, or a later turn of a done task's chat.
                if (moved || !running)
                    && st
                        .tasks
                        .get(&id)
                        .is_some_and(|slot| slot.task.state == TaskState::Done)
                {
                    self.judge_turn(st, id, session, event.turn_id);
                }
                moved
            }
        }
    }

    /// The turn of task `id` that just ended in `session` left work for review when its
    /// checkpoint (else, without one, its `file_diff` events) shows a change: the delivery
    /// becomes `awaiting_review`. A turn that changed nothing leaves the delivery as it was.
    /// The checkpoint's end is recorded in the background, so the judgement is too; it is
    /// dropped when another turn started meanwhile (that turn re-opened the work).
    fn judge_turn(self: &Arc<Self>, st: &State, id: TaskId, session: SessionId, turn: Option<TurnId>) {
        let Some(slot) = st.tasks.get(&id) else {
            return;
        };
        let (generation, diffs) = (slot.turns, slot.turn_diffs);
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let files = this.checkpoints.files_changed(session, turn).await;
            if !left_work(files, diffs) {
                return;
            }
            {
                let mut st = this.state.lock();
                let Some(slot) = st.tasks.get_mut(&id) else {
                    return;
                };
                if slot.turns != generation
                    || slot.task.state != TaskState::Done
                    || slot.task.delivery == DeliveryState::AwaitingReview
                {
                    return;
                }
                slot.task.record_delivery(DeliveryRecord::new(
                    DeliveryState::AwaitingReview,
                    DeliveryActor::Daemon,
                    now_ms(),
                ));
                this.commit(&mut st, id);
            }
            this.schedule();
        });
    }

    /// A client gave the chat of `session` a message (`chat.send`, `chat.queue.add`, …): the
    /// turn it starts re-opens a done task's work in that client's name.
    pub fn note_sender(&self, session: SessionId, by: DeliveryActor) {
        let mut st = self.state.lock();
        if st
            .tasks
            .values()
            .any(|slot| slot.task.sessions.contains(&session))
        {
            st.senders.insert(session, by);
        }
    }

    /// Moves a task along its state machine and writes it down; whether it moved.
    fn apply(&self, st: &mut State, id: TaskId, event: TaskEvent, outcome: String) -> bool {
        self.transition(st, id, event, outcome, None)
    }

    /// [`apply`](Self::apply) for the end of a run (the session's exit, the agent going idle,
    /// the end of a chat turn): a terminal run that finished `done` leaves work for review
    /// when the task has a worktree or an agent did it (`by_agent`: an agent, not just a
    /// process, finished). A chat's turns are judged by what they changed instead
    /// ([`judge_turn`](Self::judge_turn)).
    fn apply_run(
        &self,
        st: &mut State,
        id: TaskId,
        event: TaskEvent,
        outcome: String,
        by_agent: bool,
    ) -> bool {
        self.transition(st, id, event, outcome, Some(by_agent))
    }

    fn transition(
        &self,
        st: &mut State,
        id: TaskId,
        event: TaskEvent,
        outcome: String,
        run_end: Option<bool>,
    ) -> bool {
        let Some(slot) = st.tasks.get_mut(&id) else {
            return false;
        };
        let Ok(next) = slot.task.state.transition(event) else {
            return false;
        };
        slot.task.state = next;
        match event {
            TaskEvent::Complete | TaskEvent::Fail | TaskEvent::Cancel => {
                slot.task.ended_at_ms = Some(now_ms());
                slot.task.outcome = Some(clip(&outcome));
                slot.blocked = false;
                if event == TaskEvent::Complete
                    && let Some(by_agent) = run_end
                    && reviewable(&slot.task, by_agent)
                {
                    slot.task.record_delivery(DeliveryRecord::new(
                        DeliveryState::AwaitingReview,
                        DeliveryActor::Daemon,
                        now_ms(),
                    ));
                }
                st.by_session.retain(|_, task| *task != id);
            }
            TaskEvent::Reset => {
                slot.task.started_at_ms = None;
                slot.task.ended_at_ms = None;
                slot.task.outcome = None;
                slot.task.delivery = DeliveryState::None;
                slot.blocked = false;
                slot.working_seen = false;
            }
            TaskEvent::Start | TaskEvent::Block | TaskEvent::Unblock | TaskEvent::Resume => {}
        }
        self.commit(st, id);
        true
    }

    // ---- delivery -------------------------------------------------------------------------

    /// `task.review`: a person's verdict on a `done` task. A change request with a note goes
    /// to the task's chat as a user message when that session still lives, and the task runs
    /// again; otherwise the note stays on the task.
    pub async fn review(
        self: &Arc<Self>,
        req: TaskReview,
        by: DeliveryActor,
    ) -> Result<TaskReviewed, ErrorBody> {
        let note = req
            .note
            .as_deref()
            .map(str::trim)
            .filter(|note| !note.is_empty())
            .map(str::to_owned);
        if let Some(note) = &note
            && note.len() > model::REVIEW_NOTE_MAX_BYTES
        {
            return Err(bad_request(format!(
                "a review note has at most {} bytes, not {}",
                model::REVIEW_NOTE_MAX_BYTES,
                note.len()
            )));
        }
        // The registry is asked before the lock: it is async.
        let sessions = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await?
            .sessions;
        let (before, resumed) = {
            let mut st = self.state.lock();
            let Some(slot) = st.tasks.get(&req.id) else {
                return Err(not_found(req.id));
            };
            if slot.task.state != TaskState::Done {
                return Err(invalid_state(format!(
                    "the task is {}; only a done task can be reviewed",
                    st.effective(req.id).unwrap_or(slot.task.state)
                )));
            }
            let next = slot
                .task
                .delivery
                .after_review(req.decision)
                .map_err(invalid_state)?;
            if next == slot.task.delivery && note.is_none() {
                // Nothing new to record.
                let task = st.view(req.id).ok_or_else(|| not_found(req.id))?;
                return Ok(TaskReviewed {
                    task,
                    session: None,
                });
            }
            let before = slot.task.clone();
            let session = (req.decision == ReviewDecision::RequestChanges
                && note.is_some()
                && matches!(slot.task.run, Some(TaskRun::Chat { .. })))
            .then(|| {
                slot.task.sessions.iter().rev().copied().find(|session| {
                    sessions
                        .iter()
                        .any(|s| s.id == *session && s.state.is_live() && s.chat.is_some())
                })
            })
            .flatten();
            let mut record = DeliveryRecord::new(next, by, now_ms());
            record.note = note.clone();
            if let Some(slot) = st.tasks.get_mut(&req.id) {
                slot.task.record_delivery(record);
                if session.is_some() {
                    slot.task.state = TaskState::Running;
                    slot.task.ended_at_ms = None;
                    slot.task.outcome = None;
                    slot.blocked = false;
                    slot.working_seen = false;
                }
            }
            if let Some(session) = session {
                // Linked before the message goes out: the turn's events must find the task.
                st.by_session.insert(session, req.id);
            }
            self.commit(&mut st, req.id);
            (before, session)
        };
        if let (Some(session), Some(note)) = (resumed, &note) {
            let text = format!("Changes requested on the work of this task:\n\n{note}");
            if let Err(error) = self.registry.chats().send(session, text, None).await {
                // The decision is not recorded when its note could not be delivered.
                let mut st = self.state.lock();
                st.by_session.remove(&session);
                if let Some(slot) = st.tasks.get_mut(&req.id) {
                    slot.task = before;
                    slot.blocked = false;
                }
                self.commit(&mut st, req.id);
                return Err(error);
            }
        }
        self.schedule();
        let st = self.state.lock();
        let task = st.view(req.id).ok_or_else(|| not_found(req.id))?;
        Ok(TaskReviewed {
            task,
            session: resumed,
        })
    }

    /// `worktree.merge` succeeded: the work of every finished task on the merged branch (in a
    /// workspace of the repository) is `merged`, with the commit and strategy.
    pub fn worktree_merged(
        self: &Arc<Self>,
        merged: &WorktreeMerged,
        workspaces: &[WorkspaceId],
        by: DeliveryActor,
    ) {
        {
            let mut st = self.state.lock();
            let ids: Vec<TaskId> = st
                .tasks
                .values()
                .filter(|slot| {
                    slot.task.state == TaskState::Done
                        && !slot.task.delivery.is_final()
                        && on_branch(&slot.task, &merged.branch, workspaces)
                })
                .map(|slot| slot.task.id)
                .collect();
            for id in ids {
                if let Some(slot) = st.tasks.get_mut(&id) {
                    let mut record = DeliveryRecord::new(DeliveryState::Merged, by, now_ms());
                    record.commit = Some(merged.commit.clone());
                    record.strategy = Some(strategy_name(merged.strategy).to_owned());
                    slot.task.record_delivery(record);
                }
                self.commit(&mut st, id);
            }
        }
        self.schedule();
    }

    /// `worktree.remove` succeeded: work of a task on that branch that nobody merged is
    /// `discarded`.
    pub fn worktree_removed(self: &Arc<Self>, removed: &RemovedWorktree, by: DeliveryActor) {
        let Some(branch) = removed.branch.as_deref() else {
            return;
        };
        {
            let mut st = self.state.lock();
            let ids: Vec<TaskId> = st
                .tasks
                .values()
                .filter(|slot| {
                    slot.task.delivery.is_open()
                        && on_branch(&slot.task, branch, &removed.workspaces)
                })
                .map(|slot| slot.task.id)
                .collect();
            for id in ids {
                if let Some(slot) = st.tasks.get_mut(&id) {
                    slot.task.record_delivery(DeliveryRecord::new(
                        DeliveryState::Discarded,
                        by,
                        now_ms(),
                    ));
                }
                self.commit(&mut st, id);
            }
        }
        self.schedule();
    }

    /// `worktree.finish` committed (and pushed) the checkout `checkout` (canonical) into
    /// `commit`: the open work (`awaiting_review`, `accepted`, `changes_requested`) of every
    /// task whose sessions ran in that checkout is `merged` with the commit.
    pub async fn checkout_finished(
        self: &Arc<Self>,
        checkout: &std::path::Path,
        commit: &str,
        by: DeliveryActor,
    ) {
        let Ok(list) = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await
        else {
            return;
        };
        let ran_there: HashSet<SessionId> = list
            .sessions
            .iter()
            .filter(|s| match &s.placement {
                Some(placement) => placement.root == checkout,
                None => s.cwd.as_deref().is_some_and(|cwd| cwd.starts_with(checkout)),
            })
            .map(|s| s.id)
            .collect();
        {
            let mut st = self.state.lock();
            let ids: Vec<TaskId> = st
                .tasks
                .values()
                .filter(|slot| {
                    slot.task.delivery.is_open()
                        && slot.task.sessions.iter().any(|s| ran_there.contains(s))
                })
                .map(|slot| slot.task.id)
                .collect();
            for id in ids {
                if let Some(slot) = st.tasks.get_mut(&id) {
                    let mut record = DeliveryRecord::new(DeliveryState::Merged, by, now_ms());
                    record.commit = Some(commit.to_owned());
                    slot.task.record_delivery(record);
                }
                self.commit(&mut st, id);
            }
        }
        self.schedule();
    }

    // ---- state changes --------------------------------------------------------------------

    /// Writes the task down and tells the clients what changed.
    fn commit(&self, st: &mut State, id: TaskId) {
        if let Some(slot) = st.tasks.get(&id) {
            let mut task = slot.task.clone();
            task.state = task.state.stored();
            log_write(self.storage.tasks().upsert(task), "a task");
        }
        self.announce(st);
    }

    /// Publishes `task.updated` for every task whose shown state differs from what clients
    /// were last told.
    fn announce(&self, st: &mut State) {
        for task in st.views() {
            if st.published.get(&task.id) == Some(&task) {
                continue;
            }
            if let Err(error) = self.bus.publish(&Event::TaskUpdated { task: task.clone() }) {
                warn!(%error, task = %task.id, "could not publish task.updated");
            }
            st.published.insert(task.id, task);
        }
    }

    /// Drops one task and tells the clients.
    fn remove(&self, st: &mut State, id: TaskId) {
        st.tasks.remove(&id);
        st.by_session.retain(|_, task| *task != id);
        st.prune_senders();
        st.scheduler.remove(id);
        st.published.remove(&id);
        log_write(self.storage.tasks().delete(id), "a task");
        if let Err(error) = self.bus.publish(&Event::TaskRemoved { id }) {
            warn!(%error, task = %id, "could not publish task.removed");
        }
        // Tasks that were waiting for this one are not any more; there are none (delete
        // refuses them), but the view of the others is announced anyway.
        self.announce(st);
    }

    /// A closed workspace takes its tasks with it (storage cascades; the clients are told).
    fn forget_workspace(&self, st: &mut State, workspace: WorkspaceId) {
        let gone: Vec<TaskId> = st
            .tasks
            .values()
            .filter(|slot| slot.task.workspace_id == workspace)
            .map(|slot| slot.task.id)
            .collect();
        for id in gone {
            st.tasks.remove(&id);
            st.by_session.retain(|_, task| *task != id);
            st.scheduler.remove(id);
            st.published.remove(&id);
            if let Err(error) = self.bus.publish(&Event::TaskRemoved { id }) {
                warn!(%error, task = %id, "could not publish task.removed");
            }
        }
        st.prune_senders();
    }
}

// ---- pure helpers -------------------------------------------------------------------------

fn bad_request(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::BadRequest, message)
}

fn not_found(id: TaskId) -> ErrorBody {
    ErrorBody::new(ErrorCode::NotFound, format!("task {id} not found"))
}

fn invalid_state(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidState, message)
}

/// Whether a terminal run that finished `done` left work for a person to look at: the task
/// has a worktree, or an agent in the terminal did the work in the main checkout. A plain
/// command that exits 0 in the main checkout has nothing to review. A chat is judged turn by
/// turn by what it changed ([`left_work`]), never here.
fn reviewable(task: &Task, by_agent: bool) -> bool {
    !matches!(task.run, Some(TaskRun::Chat { .. }))
        && (task.placement.kind == TaskPlacementKind::Worktree || by_agent)
}

/// Whether a chat turn left work to review: the files its checkpoint says it changed, else
/// (no checkpoint: not in git, checkpoints off, a failed snapshot) whether it sent a
/// `file_diff`.
fn left_work(files_changed: Option<u64>, diffs: bool) -> bool {
    files_changed.map_or(diffs, |files| files > 0)
}

/// The branch a worktree task works on: the one it names, else `task/<slug of the title>`.
fn branch_of(task: &Task) -> String {
    task.placement
        .branch
        .clone()
        .unwrap_or_else(|| model::default_branch(&task.title))
}

/// Whether `task` works in a worktree of `branch`, in one of the repository's `workspaces`.
fn on_branch(task: &Task, branch: &str, workspaces: &[WorkspaceId]) -> bool {
    task.placement.kind == TaskPlacementKind::Worktree
        && workspaces.contains(&task.workspace_id)
        && branch_of(task) == branch
}

fn strategy_name(strategy: MergeStrategy) -> &'static str {
    match strategy {
        MergeStrategy::Merge => "merge",
        MergeStrategy::Squash => "squash",
        MergeStrategy::Rebase => "rebase",
    }
}

fn clean_title(title: &str) -> Result<String, ErrorBody> {
    let title = title.trim();
    if title.is_empty() {
        return Err(bad_request("a task needs a title"));
    }
    if title.chars().count() > model::TITLE_MAX_CHARS {
        return Err(bad_request(format!(
            "a title has at most {} characters",
            model::TITLE_MAX_CHARS
        )));
    }
    if title.chars().any(char::is_control) {
        return Err(bad_request("a title is one line of plain text"));
    }
    Ok(title.to_owned())
}

/// A prompt that is only white space is none.
fn clean_prompt(prompt: Option<String>) -> Result<Option<String>, ErrorBody> {
    let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) else {
        return Ok(None);
    };
    if prompt.len() > model::PROMPT_MAX_BYTES {
        return Err(bad_request(format!(
            "a prompt has at most {} bytes, not {}",
            model::PROMPT_MAX_BYTES,
            prompt.len()
        )));
    }
    Ok(Some(prompt))
}

fn check_placement(placement: &TaskPlacement) -> Result<(), ErrorBody> {
    match placement.kind {
        TaskPlacementKind::Main if placement.branch.is_some() || placement.base.is_some() => Err(
            bad_request("a branch and a base only make sense in a worktree placement"),
        ),
        TaskPlacementKind::Main => Ok(()),
        TaskPlacementKind::Worktree => {
            for (what, value) in [("branch", &placement.branch), ("base", &placement.base)] {
                if let Some(value) = value
                    && (value.is_empty()
                        || value.starts_with('-')
                        || value.chars().any(|c| c.is_whitespace() || c.is_control()))
                {
                    return Err(bad_request(format!("`{value}` is not a valid {what}")));
                }
            }
            Ok(())
        }
    }
}

fn check_run(run: Option<&TaskRun>, prompt: Option<&str>) -> Result<(), ErrorBody> {
    match run {
        None => Ok(()),
        Some(TaskRun::Terminal { argv }) => {
            if argv.is_empty() || argv[0].is_empty() {
                return Err(bad_request("a terminal run needs a command"));
            }
            if argv.len() > model::MAX_ARGV_WORDS
                || argv.iter().map(String::len).sum::<usize>() > model::MAX_ARGV_BYTES
            {
                return Err(bad_request("the command is too long"));
            }
            if argv.iter().any(|word| word.contains('\0')) {
                return Err(bad_request("a command cannot contain NUL"));
            }
            Ok(())
        }
        Some(TaskRun::Chat { agent, model }) => {
            if model.as_deref().is_some_and(|m| m.trim().is_empty()) {
                return Err(bad_request("a chat task's model cannot be empty"));
            }
            if agent != AGENT_CODEX && !is_harness_agent(agent) {
                return Err(bad_request(format!(
                    "a chat task runs the `{AGENT_CODEX}`, `omp` or `claude` agent, not `{agent}`"
                )));
            }
            if prompt.is_none() {
                return Err(bad_request("a chat task needs a prompt: its first message"));
            }
            Ok(())
        }
    }
}

/// Where a task in the main checkout works: the first folder of the workspace that is a git
/// repository, else its first folder.
fn main_directory(workspace: &WorkspaceInfo) -> Result<PathBuf, ErrorBody> {
    let root = workspace
        .roots
        .iter()
        .find(|root| projects::git_root(root).is_some())
        .or_else(|| workspace.roots.first())
        .ok_or_else(|| {
            bad_request(format!(
                "workspace `{}` has no folder for the task to work in",
                workspace.name
            ))
        })?;
    if !root.is_dir() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidState,
            format!("{} is not a directory", root.display()),
        ));
    }
    Ok(root.clone())
}

/// How a session that ended finishes its task; `None` while it lives.
fn session_outcome(state: &SessionState) -> Option<(TaskEvent, String)> {
    match state {
        SessionState::Exited { code, signal } => exit_outcome(*code, *signal),
        SessionState::Failed { reason } => Some((
            TaskEvent::Fail,
            format!("session failed: {}", one_line(reason)),
        )),
        SessionState::Stopped => Some((TaskEvent::Cancel, "session stopped".to_owned())),
        SessionState::Starting
        | SessionState::Running
        | SessionState::Idle
        | SessionState::WaitingApproval => None,
    }
}

fn exit_outcome(code: Option<i32>, signal: Option<i32>) -> Option<(TaskEvent, String)> {
    Some(match (code, signal) {
        (Some(0), _) => (TaskEvent::Complete, "exited 0".to_owned()),
        (Some(code), _) => (TaskEvent::Fail, format!("exited {code}")),
        (None, Some(signal)) => (TaskEvent::Fail, format!("killed by signal {signal}")),
        (None, None) => (TaskEvent::Fail, "exited".to_owned()),
    })
}

/// How the end of a chat turn finishes its task.
fn chat_outcome(kind: &AgentEventKind) -> Option<(TaskEvent, String)> {
    match kind {
        AgentEventKind::TurnCompleted { .. } => {
            Some((TaskEvent::Complete, "turn completed".to_owned()))
        }
        AgentEventKind::TurnFailed { message, .. } => Some((
            TaskEvent::Fail,
            format!("turn failed: {}", one_line(message)),
        )),
        AgentEventKind::TurnCancelled => Some((TaskEvent::Fail, "turn cancelled".to_owned())),
        _ => None,
    }
}

/// `text` redacted, on one line.
fn one_line(text: &str) -> String {
    hutan_index::redact::redact(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// At most [`OUTCOME_MAX_CHARS`] characters; a cut ends in `…`.
fn clip(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(OUTCOME_MAX_CHARS).collect();
    if chars.next().is_some() {
        out.pop();
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use hutan_core::{AfterPolicy, AgentEventKind, TaskPlacement};

    use super::*;

    fn task(title: &str, created: u64, deps: &[TaskId], state: TaskState) -> Task {
        Task {
            id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            title: title.to_owned(),
            prompt: None,
            state,
            depends_on: deps.to_vec(),
            placement: TaskPlacement::default(),
            run: None,
            auto_start: false,
            sessions: Vec::new(),
            created_at_ms: created,
            started_at_ms: None,
            ended_at_ms: None,
            outcome: None,
            delivery: DeliveryState::None,
            after_policy: AfterPolicy::Done,
            delivery_history: Vec::new(),
        }
    }

    /// Tasks of one workspace.
    fn state_of(tasks: Vec<Task>) -> State {
        let ws = tasks[0].workspace_id;
        State::new(
            tasks
                .into_iter()
                .map(|mut t| {
                    t.workspace_id = ws;
                    t
                })
                .collect(),
        )
    }

    #[test]
    fn titles_prompts_and_runs_are_checked() {
        assert_eq!(clean_title("  Fix it  ").unwrap(), "Fix it");
        assert!(clean_title("   ").is_err());
        assert!(clean_title("two\nlines").is_err());
        assert!(clean_title(&"x".repeat(201)).is_err());
        assert_eq!(clean_prompt(Some("  \n".into())).unwrap(), None);
        assert_eq!(
            clean_prompt(Some("go".into())).unwrap().as_deref(),
            Some("go")
        );
        assert!(clean_prompt(Some("x".repeat(model::PROMPT_MAX_BYTES + 1))).is_err());

        let terminal = |argv: &[&str]| TaskRun::Terminal {
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        };
        assert!(check_run(Some(&terminal(&["make"])), None).is_ok());
        assert!(check_run(Some(&terminal(&[])), None).is_err());
        assert!(check_run(Some(&terminal(&[""])), None).is_err());
        assert!(check_run(Some(&terminal(&["a\0b"])), None).is_err());
        let chat = TaskRun::Chat {
            agent: "codex".into(),
            model: None,
        };
        assert!(check_run(Some(&chat), Some("go")).is_ok());
        assert!(
            check_run(Some(&chat), None).is_err(),
            "a chat needs its first message"
        );
        let other = TaskRun::Chat {
            agent: "direct".into(),
            model: None,
        };
        assert!(check_run(Some(&other), Some("go")).is_err());
        let empty_model = TaskRun::Chat {
            agent: "codex".into(),
            model: Some(" ".into()),
        };
        assert!(check_run(Some(&empty_model), Some("go")).is_err());
        assert!(check_run(None, None).is_ok());

        let worktree = |branch: Option<&str>| TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: branch.map(str::to_owned),
            base: None,
        };
        assert!(check_placement(&worktree(None)).is_ok());
        assert!(check_placement(&worktree(Some("task/x"))).is_ok());
        assert!(check_placement(&worktree(Some("-x"))).is_err());
        assert!(check_placement(&worktree(Some("a b"))).is_err());
        assert!(check_placement(&TaskPlacement::default()).is_ok());
        let main_with_branch = TaskPlacement {
            kind: TaskPlacementKind::Main,
            branch: Some("x".into()),
            base: None,
        };
        assert!(check_placement(&main_with_branch).is_err());
    }

    #[test]
    fn finishes_are_told_from_the_end_of_a_session_a_turn_and_an_agent() {
        let complete = |s: &str| Some((TaskEvent::Complete, s.to_owned()));
        let fail = |s: &str| Some((TaskEvent::Fail, s.to_owned()));
        assert_eq!(exit_outcome(Some(0), None), complete("exited 0"));
        assert_eq!(exit_outcome(Some(3), None), fail("exited 3"));
        assert_eq!(exit_outcome(None, Some(9)), fail("killed by signal 9"));
        assert_eq!(
            session_outcome(&SessionState::Stopped),
            Some((TaskEvent::Cancel, "session stopped".into()))
        );
        assert_eq!(
            session_outcome(&SessionState::Failed {
                reason: "spawn\nfailed".into()
            }),
            fail("session failed: spawn failed")
        );
        for live in [
            SessionState::Starting,
            SessionState::Running,
            SessionState::Idle,
            SessionState::WaitingApproval,
        ] {
            assert_eq!(session_outcome(&live), None);
        }
        assert_eq!(
            chat_outcome(&AgentEventKind::TurnCompleted { duration_ms: None }),
            complete("turn completed")
        );
        assert_eq!(
            chat_outcome(&AgentEventKind::TurnFailed {
                message: "rate limited\nretry later".into(),
                details: None
            }),
            fail("turn failed: rate limited retry later")
        );
        assert_eq!(
            chat_outcome(&AgentEventKind::TurnCancelled),
            fail("turn cancelled")
        );
        assert_eq!(chat_outcome(&AgentEventKind::TurnStarted), None);
        assert_eq!(clip(&"é".repeat(300)).chars().count(), OUTCOME_MAX_CHARS);
        assert!(clip(&"é".repeat(300)).ends_with('…'));
    }

    #[test]
    fn readiness_follows_the_dependencies_and_the_listing_their_order() {
        let a = task("a", 10, &[], TaskState::Done);
        let b = task("b", 20, &[a.id], TaskState::Todo);
        let c = task("c", 15, &[b.id], TaskState::Todo);
        let d = task("d", 5, &[], TaskState::Running);
        let state = state_of(vec![c.clone(), d.clone(), b.clone(), a.clone()]);
        assert_eq!(state.effective(a.id), Some(TaskState::Done));
        assert_eq!(state.effective(b.id), Some(TaskState::Ready));
        assert_eq!(state.effective(c.id), Some(TaskState::Todo));
        assert_eq!(state.active_count(), 1);
        let titles: Vec<String> = state.views().into_iter().map(|t| t.title).collect();
        assert_eq!(titles, ["d", "a", "b", "c"]);

        let message = state.not_ready(c.id).message;
        assert!(message.contains("`b` (ready)"), "{message}");
        assert_eq!(state.not_ready(d.id).message, "the task is already running");
    }

    #[test]
    fn cycles_unknown_and_foreign_dependencies_are_refused() {
        let a = task("build", 10, &[], TaskState::Todo);
        let b = task("test", 20, &[a.id], TaskState::Todo);
        let c = task("ship", 30, &[b.id], TaskState::Todo);
        let state = state_of(vec![a.clone(), b.clone(), c.clone()]);
        let ws = a.workspace_id;

        let error = state.check_dependencies(a.id, ws, &[c.id]).unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert_eq!(
            error.message,
            "dependency cycle: `build` -> `ship` -> `test` -> `build`"
        );
        let error = state.check_dependencies(a.id, ws, &[a.id]).unwrap_err();
        assert!(error.message.contains("itself"), "{}", error.message);
        let error = state
            .check_dependencies(a.id, ws, &[TaskId::new()])
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        let error = state
            .check_dependencies(TaskId::new(), WorkspaceId::new(), &[a.id])
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert!(error.message.contains("another workspace"));
        // Duplicates collapse; a new task cannot close a cycle.
        assert_eq!(
            state
                .check_dependencies(TaskId::new(), ws, &[b.id, a.id, b.id])
                .unwrap(),
            [b.id, a.id]
        );
        assert_eq!(state.dependants_of(a.id), [b.id, c.id]);
        assert!(state.dependants_of(c.id).is_empty());
    }

    #[test]
    fn the_start_queue_holds_ready_auto_tasks_in_the_order_they_became_ready() {
        let mut first = task("first", 10, &[], TaskState::Todo);
        first.auto_start = true;
        let mut second = task("second", 20, &[first.id], TaskState::Todo);
        second.auto_start = true;
        let mut manual = task("manual", 5, &[], TaskState::Todo);
        manual.auto_start = false;
        let mut blocked = task("blocked", 7, &[second.id], TaskState::Todo);
        blocked.auto_start = true;
        let mut state = state_of(vec![
            second.clone(),
            blocked.clone(),
            manual.clone(),
            first.clone(),
        ]);
        state.refill_queue();
        assert_eq!(
            state.scheduler.waiting().collect::<Vec<_>>(),
            [first.id],
            "only ready tasks that start by themselves wait"
        );

        // The first ran and ended: it leaves the queue, the second is ready now and queues
        // (the manual one never does; the last one still waits for the second).
        state.tasks.get_mut(&first.id).unwrap().task.state = TaskState::Done;
        state.refill_queue();
        assert_eq!(state.scheduler.waiting().collect::<Vec<_>>(), [second.id]);

        // A waiting task that is cancelled leaves the queue.
        state.tasks.get_mut(&second.id).unwrap().task.state = TaskState::Cancelled;
        state.refill_queue();
        assert_eq!(state.scheduler.waiting().count(), 0);
    }

    #[test]
    fn only_worktree_work_or_an_agents_work_awaits_review() {
        let mut t = task("x", 1, &[], TaskState::Done);
        assert!(!reviewable(&t, false), "a command in the main checkout");
        assert!(reviewable(&t, true), "an agent in the main checkout");
        t.placement.kind = TaskPlacementKind::Worktree;
        assert!(reviewable(&t, false), "a worktree holds work");
        t.run = Some(TaskRun::Chat {
            agent: "codex".into(),
            model: None,
        });
        assert!(
            !reviewable(&t, true),
            "a chat is judged turn by turn, by what it changed"
        );
    }

    #[test]
    fn a_turn_left_work_when_its_checkpoint_says_so_else_when_it_sent_a_diff() {
        assert!(left_work(Some(2), false));
        assert!(!left_work(Some(0), false));
        // The checkpoint wins: a diff the agent proposed and the user declined changed nothing.
        assert!(!left_work(Some(0), true));
        // No checkpoint (not in git, checkpoints off, a failed snapshot): the diffs say.
        assert!(left_work(None, true));
        assert!(!left_work(None, false));
    }

    #[test]
    fn a_task_is_on_the_branch_it_names_or_the_one_of_its_title() {
        let ws = WorkspaceId::new();
        let mut t = task("Fix the login bug", 1, &[], TaskState::Done);
        t.workspace_id = ws;
        assert!(
            !on_branch(&t, "task/fix-the-login-bug", &[ws]),
            "main placement"
        );
        t.placement.kind = TaskPlacementKind::Worktree;
        assert!(on_branch(&t, "task/fix-the-login-bug", &[ws]));
        assert!(!on_branch(
            &t,
            "task/fix-the-login-bug",
            &[WorkspaceId::new()]
        ));
        t.placement.branch = Some("feature/x".into());
        assert!(on_branch(&t, "feature/x", &[ws]));
        assert!(!on_branch(&t, "task/fix-the-login-bug", &[ws]));
    }

    #[test]
    fn the_dependency_policy_decides_when_a_task_is_ready() {
        let mut a = task("a", 1, &[], TaskState::Done);
        a.delivery = DeliveryState::AwaitingReview;
        let mut b = task("b", 2, &[a.id], TaskState::Todo);
        b.workspace_id = a.workspace_id;
        let (a_id, b_id) = (a.id, b.id);
        let mut state = state_of(vec![a, b]);
        assert_eq!(state.effective(b_id), Some(TaskState::Ready));
        for (policy, delivery, ready) in [
            (AfterPolicy::Accepted, DeliveryState::AwaitingReview, false),
            (AfterPolicy::Accepted, DeliveryState::Accepted, true),
            (AfterPolicy::Accepted, DeliveryState::Merged, true),
            (AfterPolicy::Merged, DeliveryState::Accepted, false),
            (AfterPolicy::Merged, DeliveryState::Merged, true),
        ] {
            state.tasks.get_mut(&b_id).unwrap().task.after_policy = policy;
            state.tasks.get_mut(&a_id).unwrap().task.delivery = delivery;
            assert_eq!(
                state.effective(b_id) == Some(TaskState::Ready),
                ready,
                "{policy} policy, dependency {delivery}"
            );
        }
        // The message of a refused start names what is missing.
        state.tasks.get_mut(&a_id).unwrap().task.delivery = DeliveryState::AwaitingReview;
        let refusal = state.not_ready(b_id);
        assert!(
            refusal.message.contains("awaiting_review") && refusal.message.contains("merged"),
            "{}",
            refusal.message
        );
    }
}
