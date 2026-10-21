//! Checkpoints of chat turns (ADR 0019, slice CK-2): a snapshot of the session's checkout when
//! a turn starts and when it ends, one ref per turn, a row per turn in SQLite.
//!
//! - **Open** ([`TurnCheckpoints::open`], when an agent chat starts): the session is known and
//!   its checkout is warmed up in the background (the private index is seeded and hashed), so
//!   the first turn's start snapshot is cheap.
//! - **Begin** ([`TurnCheckpoints::begin`], before the prompt goes to the agent): the row is
//!   written (state `running`), then the start snapshot runs. The prompt waits for it at most
//!   [`Tuning::start_budget`] (5 s, counting the wait for the previous turn's end); past that
//!   it goes anyway, the snapshot finishes in the background, and the turn is `late`.
//! - **End** ([`TurnCheckpoints::turn_ended`], whatever ended the turn: completion, failure,
//!   interrupt, the agent exiting, the daemon stopping): the end snapshot, with the start
//!   commit as its parent, under `refs/hutan/checkpoints/<session>/<turn>`; the start ref
//!   goes. A turn that changed nothing writes no end ref (`unchanged`). Retention runs for the
//!   repository afterwards.
//! - A folder outside git, the home directory, a volume root, too many untracked files, no
//!   git: the turn's row is `unsupported` with the reason, and there is no snapshot. Three
//!   failed snapshots in a row, or less than 2 GiB free, switch the checkout off (`off`) until
//!   the checkpoint settings change with `mode = "on"`.
//! - `[checkpoints] mode = "off"` (globally or for the project) takes effect for the next
//!   snapshot: no new turn is recorded, and a running turn's end is not taken.
//!
//! **Order of writes.** A row is written before its first ref, and a ref is deleted after the
//! row stops naming it, so at any time every checkpoint ref has a row or is about to be
//! deleted. [`TurnCheckpoints::reconcile`] (at daemon start) repairs what a crash left: a
//! `running` row of an earlier daemon adopts its end ref when there is one (the trailer
//! `Hutan-Checkpoint` names it) and is `failed` otherwise; a row whose ref is gone is dropped;
//! a ref with no row is adopted from its trailers, or deleted when it is a start.
//!
//! Mawas conversations (assistants) and direct model chats have no checkpoints: they do not
//! edit files.
//!
//! **Clients** (protocol 0.37, capability `checkpoint`) list, diff and remove checkpoints
//! through the requests of `requests`; `checkpoint.created` goes out when a turn's (or a
//! restore's) checkpoint is recorded, `checkpoint.removed` when checkpoints go
//! (`checkpoint.remove`, retention), and a chat's `ChatInfo.checkpoints` (with
//! `checkpoint.state`) follows [`TurnCheckpoints::status`]. They preview and restore the files
//! of turns through `restore` (protocol 0.38, capability `checkpoint.restore`); a file the user
//! saves while a turn runs ([`TurnCheckpoints::saved`]) is a conflict of that turn's restore.

mod requests;
mod restore;

pub use requests::LISTED;
pub use restore::{Announce, RestoreContext};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use hutan_core::{
    CheckpointId, CheckpointKind, CheckpointState, CheckpointStatus, SessionId, SkippedFile, TurnId,
};
use hutan_protocol::messages::Event;
use hutan_storage::{CheckpointFilter, CheckpointRow, Storage, StorageError};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::refs::RefInfo;
use super::retention::{self, Policy};
use super::{Caps, CheckpointError, Checkpoints, Config, Kind, Label, STORE_DIR, Snapshot, caps};
use crate::bus::EventBus;
use crate::config::{CheckpointMode, CheckpointSettings, ProjectCheckpoints};
use crate::registry::ChatNotifier;

/// How long a turn's prompt waits for its start snapshot (ADR 0019 Decision 6).
pub const START_BUDGET: Duration = Duration::from_secs(5);

/// How often retention runs besides after each turn and at start.
const RETENTION_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Paths left out that a checkpoint lists (the rest are counted by nobody: a summary).
pub const SKIPPED_KEPT: usize = 200;

/// Files saved during a turn that its checkpoint remembers.
pub const DURING_KEPT: usize = 1000;

/// The reason of a `running` row an earlier daemon left without an end.
pub const STOPPED_REASON: &str = "hutand stopped before the end of the turn was recorded";

/// Knobs of the service that are not settings. Tests lower them.
#[derive(Debug, Clone)]
pub struct Tuning {
    /// How long a turn's prompt waits for its start snapshot.
    pub start_budget: Duration,
    /// Extra environment of every git call (tests point `PATH` at a fake git and
    /// `GIT_CONFIG_GLOBAL` at an empty file).
    pub git_env: BTreeMap<String, String>,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            start_budget: START_BUDGET,
            git_env: BTreeMap::new(),
        }
    }
}

/// The project of a chat session: what `[checkpoints.projects.<name or id>]` is matched with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    pub project_id: String,
    pub project_name: String,
}

/// The checkpoints of the daemon's chat turns.
#[derive(Debug)]
pub struct TurnCheckpoints {
    engine: Checkpoints,
    storage: Storage,
    settings: parking_lot::RwLock<CheckpointSettings>,
    start_budget: Duration,
    /// When this daemon started: rows created before are an earlier daemon's.
    boot_ms: u64,
    sessions: parking_lot::Mutex<HashMap<SessionId, Arc<Slot>>>,
    /// Turn ends (and cleanups of turns that never started) still being recorded.
    in_flight: watch::Sender<usize>,
    /// Reconcile and retention, one at a time.
    upkeep: tokio::sync::Mutex<()>,
    /// Where events go, once the chat manager connected the service.
    events: OnceLock<Events>,
}

/// The bus that takes `checkpoint.*` events and the registry that keeps `ChatInfo.checkpoints`.
#[derive(Debug)]
struct Events {
    bus: Arc<EventBus>,
    registry: ChatNotifier,
}

/// One chat session.
#[derive(Debug)]
struct Slot {
    session: SessionId,
    cwd: PathBuf,
    scope: parking_lot::Mutex<Scope>,
    state: parking_lot::Mutex<SlotState>,
}

#[derive(Debug, Default)]
struct SlotState {
    /// What the last snapshot said about the checkout (`None`: nothing yet, so on).
    status: Option<CheckpointStatus>,
    /// The running turn's checkpoint.
    pending: Option<Pending>,
    /// The previous turn's end, while it is being recorded (`true` once it is).
    ending: Option<watch::Receiver<bool>>,
}

#[derive(Debug)]
struct Pending {
    row: CheckpointRow,
    /// The turn, once `chat.send` knows it.
    turn: Option<TurnId>,
    start: Start,
    /// Files the user saved through the file service meanwhile (canonical, at most
    /// [`DURING_KEPT`]).
    during: BTreeSet<PathBuf>,
}

/// The row a start wrote (with its repository and ref) and the start snapshot.
type Started = (CheckpointRow, Result<Snapshot, CheckpointError>);

#[derive(Debug)]
enum Start {
    /// Taken within the budget; the pending row is the one written.
    Taken(Box<Snapshot>),
    /// Still running after the budget.
    Late(JoinHandle<Started>),
}

impl Start {
    /// The start, waited for when it is late; `row` is the pending row.
    async fn outcome(self, row: CheckpointRow) -> Started {
        match self {
            Self::Taken(snapshot) => (row, Ok(*snapshot)),
            Self::Late(handle) => settled(&row, handle.await),
        }
    }
}

/// Ends one piece of in-flight work, whatever happens to its task.
struct InFlight {
    count: watch::Sender<usize>,
    done: Option<watch::Sender<bool>>,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.send(true);
        }
        self.count.send_modify(|n| *n = n.saturating_sub(1));
    }
}

/// How a failed snapshot shows in the row and in the session's status.
fn classify(error: &CheckpointError) -> (CheckpointState, Option<CheckpointStatus>) {
    let reason = error.to_string();
    match error {
        CheckpointError::NotARepository { .. }
        | CheckpointError::Unsupported(_)
        | CheckpointError::GitMissing => (
            CheckpointState::Unsupported,
            Some(CheckpointStatus::unsupported(reason)),
        ),
        CheckpointError::SwitchedOff { .. } | CheckpointError::LowDisk { .. } => {
            (CheckpointState::Off, Some(CheckpointStatus::off(reason)))
        }
        CheckpointError::InvalidLabel(_)
        | CheckpointError::GitFailed { .. }
        | CheckpointError::TimedOut { .. }
        | CheckpointError::Io { .. }
        | CheckpointError::Internal(_) => (CheckpointState::Failed, None),
    }
}

/// A start task's outcome; `row` stands in when the task did not finish.
fn settled(row: &CheckpointRow, result: Result<Started, tokio::task::JoinError>) -> Started {
    result.unwrap_or_else(|error| {
        let error = CheckpointError::Internal(error.to_string());
        (row.clone(), Err(error))
    })
}

/// The start's and the end's skipped paths, the end's winning, at most [`SKIPPED_KEPT`].
fn skipped(start: &Snapshot, end: Option<&Snapshot>) -> Vec<SkippedFile> {
    let mut by_path: BTreeMap<PathBuf, &caps::Skipped> = BTreeMap::new();
    for skip in start
        .skipped
        .iter()
        .chain(end.into_iter().flat_map(|e| &e.skipped))
    {
        by_path.insert(skip.path.clone(), skip);
    }
    by_path
        .into_values()
        .take(SKIPPED_KEPT)
        .map(|skip| SkippedFile {
            path: skip.path.to_string_lossy().into_owned(),
            reason: skip.reason.clone(),
            bytes: skip.bytes,
        })
        .collect()
}

fn now_ms() -> u64 {
    crate::registry::now_ms()
}

/// The paths of `saved` inside `checkout`, relative to it.
fn saved_within(saved: &BTreeSet<PathBuf>, checkout: &Path) -> Vec<String> {
    let canonical = std::fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_owned());
    saved
        .iter()
        .filter_map(|path| path.strip_prefix(&canonical).ok())
        .filter_map(|relative| relative.to_str().map(str::to_owned))
        .collect()
}

impl TurnCheckpoints {
    /// The service over `storage`, keeping private indexes under `<base>/checkpoints`;
    /// `user_home`'s checkout is never snapshotted.
    pub fn new(
        storage: Storage,
        base: &Path,
        user_home: Option<PathBuf>,
        settings: CheckpointSettings,
        tuning: Tuning,
    ) -> Arc<Self> {
        let mut config = Config::new(base.join(STORE_DIR), user_home);
        config.git_env = tuning.git_env;
        Arc::new(Self {
            engine: Checkpoints::new(config),
            storage,
            settings: parking_lot::RwLock::new(settings),
            start_budget: tuning.start_budget,
            boot_ms: now_ms(),
            sessions: parking_lot::Mutex::new(HashMap::new()),
            in_flight: watch::Sender::new(0),
            upkeep: tokio::sync::Mutex::new(()),
            events: OnceLock::new(),
        })
    }

    /// The snapshot engine.
    pub fn engine(&self) -> &Checkpoints {
        &self.engine
    }

    /// Sends the service's events to `bus`, and tells `registry` when a chat's checkpoint state
    /// may have changed. Once; later calls change nothing.
    pub(crate) fn connect(&self, bus: Arc<EventBus>, registry: ChatNotifier) {
        let _ = self.events.set(Events { bus, registry });
    }

    /// The registry reads the session's [`status`](Self::status) again.
    fn state_changed(&self, session: SessionId) {
        if let Some(events) = self.events.get() {
            let registry = events.registry.clone();
            tokio::spawn(async move { registry.checkpoints(session).await });
        }
    }

    fn publish(&self, event: &Event) {
        if let Some(events) = self.events.get()
            && let Err(error) = events.bus.publish(event)
        {
            warn!(%error, "checkpoints: publishing an event");
        }
    }

    /// Writes the final row of a turn's checkpoint; `checkpoint.created` follows once it is
    /// written, for a checkpoint that `checkpoint.list` shows.
    async fn record(&self, row: CheckpointRow) {
        let created = (self.events.get().is_some() && LISTED.contains(&row.state)).then(|| {
            Event::CheckpointCreated {
                checkpoint: Box::new(row.info()),
            }
        });
        if self.write(row).await
            && let Some(event) = created
        {
            self.publish(&event);
        }
    }

    /// `checkpoint.removed`.
    fn announce_removed(&self, ids: Vec<CheckpointId>) {
        if !ids.is_empty() {
            self.publish(&Event::CheckpointRemoved { ids });
        }
    }

    /// `[checkpoints]` as the service uses it now.
    pub fn settings(&self) -> CheckpointSettings {
        self.settings.read().clone()
    }

    /// New `[checkpoints]` settings, from the next snapshot on. With `mode = "on"` the
    /// checkouts that failures or low disk switched off are switched on again.
    pub fn apply_settings(&self, settings: CheckpointSettings) {
        let on = settings.mode == CheckpointMode::On;
        *self.settings.write() = settings;
        let slots: Vec<(SessionId, Arc<Slot>)> = self
            .sessions
            .lock()
            .iter()
            .map(|(id, slot)| (*id, slot.clone()))
            .collect();
        for (session, slot) in slots {
            // `mode` may have changed what the session's status says.
            self.state_changed(session);
            if !on {
                continue;
            }
            let switched_off = {
                let mut state = slot.state.lock();
                let off = matches!(
                    &state.status,
                    Some(status) if status.state == hutan_core::CheckpointAvailability::Off
                );
                if off {
                    state.status = None;
                }
                off
            };
            if switched_off {
                let engine = self.engine.clone();
                let cwd = slot.cwd.clone();
                tokio::spawn(async move {
                    if let Err(error) = engine.switch_on(&cwd).await {
                        debug!(%error, cwd = %cwd.display(), "could not switch checkpoints on");
                    }
                });
            }
        }
    }

    /// The session's checkpoints right now; `None` for a session that has none (not an agent
    /// chat, or not running).
    pub fn status(&self, session: SessionId) -> Option<CheckpointStatus> {
        let slot = self.slot(session)?;
        if self.project(&slot).mode == CheckpointMode::Off {
            return Some(CheckpointStatus::off(
                "checkpoints are turned off in the settings",
            ));
        }
        Some(
            slot.state
                .lock()
                .status
                .clone()
                .unwrap_or_else(CheckpointStatus::on),
        )
    }

    fn slot(&self, session: SessionId) -> Option<Arc<Slot>> {
        self.sessions.lock().get(&session).cloned()
    }

    fn project(&self, slot: &Slot) -> ProjectCheckpoints {
        let scope = slot.scope.lock().clone();
        self.settings
            .read()
            .for_project(&scope.project_id, &scope.project_name)
    }

    fn caps(&self, project: &ProjectCheckpoints) -> Caps {
        Caps {
            max_file_bytes: project.max_file_bytes,
            snapshot_budget_bytes: project.snapshot_budget_bytes,
            max_files: usize::try_from(project.max_files).unwrap_or(usize::MAX),
            max_untracked_files: usize::try_from(project.max_untracked_files).unwrap_or(usize::MAX),
            exclude: self.settings.read().exclude.clone(),
            ..Caps::default()
        }
    }

    async fn store<T: Send + 'static>(
        &self,
        what: &'static str,
        op: impl FnOnce(&Storage) -> Result<T, StorageError> + Send + 'static,
    ) -> Option<T> {
        let storage = self.storage.clone();
        match tokio::task::spawn_blocking(move || op(&storage)).await {
            Ok(Ok(value)) => Some(value),
            Ok(Err(error)) => {
                warn!(%error, "checkpoints: {what}");
                None
            }
            Err(error) => {
                warn!(%error, "checkpoints: {what}");
                None
            }
        }
    }

    async fn write(&self, row: CheckpointRow) -> bool {
        self.store("writing a checkpoint", move |s| s.checkpoints().upsert(row))
            .await
            .is_some()
    }

    /// What the last snapshot said about the session's checkout; the registry hears of a
    /// change.
    fn set_status(&self, slot: &Slot, status: Option<CheckpointStatus>) {
        let changed = {
            let mut state = slot.state.lock();
            let changed = state.status != status;
            state.status = status;
            changed
        };
        if changed {
            self.state_changed(slot.session);
        }
    }

    // Sessions -------------------------------------------------------------------------------

    /// An agent chat started (or started again) in `cwd`: its turns get checkpoints, and its
    /// checkout is warmed up in the background.
    pub fn open(self: &Arc<Self>, session: SessionId, cwd: PathBuf, scope: Scope) {
        let slot = {
            let mut sessions = self.sessions.lock();
            match sessions.get(&session) {
                Some(slot) if slot.cwd == cwd => {
                    *slot.scope.lock() = scope;
                    slot.clone()
                }
                _ => {
                    let slot = Arc::new(Slot {
                        session,
                        cwd,
                        scope: parking_lot::Mutex::new(scope),
                        state: parking_lot::Mutex::new(SlotState::default()),
                    });
                    sessions.insert(session, slot.clone());
                    slot
                }
            }
        };
        let project = self.project(&slot);
        if project.mode == CheckpointMode::Off {
            // `ChatInfo.checkpoints` says so at once.
            self.state_changed(session);
            return;
        }
        let this = Arc::clone(self);
        let caps = self.caps(&project);
        tokio::spawn(async move {
            let warm = this
                .engine
                .snapshot_with(&slot.cwd, Label::warm(session.to_string()), caps)
                .await;
            let status = match &warm {
                Ok(_) => None,
                Err(error) => {
                    debug!(%session, %error, "checkpoint warm-up");
                    classify(error).1
                }
            };
            // `ChatInfo.checkpoints` gets its first value now, whether or not it changed.
            slot.state.lock().status = status;
            this.state_changed(session);
        });
    }

    /// The session's agent is gone: a turn it left open is ended, and the session is
    /// forgotten.
    pub fn close(self: &Arc<Self>, session: SessionId) {
        self.turn_ended(session, None);
        self.sessions.lock().remove(&session);
    }

    // Turns ----------------------------------------------------------------------------------

    /// Takes the start snapshot of a turn about to be sent; returns once it is taken or the
    /// budget is spent. `None` when the turn gets no checkpoint (the session has none, they
    /// are off, or the folder cannot have them: its row says so).
    pub async fn begin(self: &Arc<Self>, session: SessionId) -> Option<CheckpointId> {
        let slot = self.slot(session)?;
        // A turn that never ended (its agent never started it) is ended first.
        self.turn_ended(session, None);
        let project = self.project(&slot);
        if project.mode == CheckpointMode::Off {
            return None;
        }
        let deadline = tokio::time::Instant::now() + self.start_budget;
        let ending = slot.state.lock().ending.clone();
        if let Some(mut ending) = ending {
            // The previous turn's end comes first: its snapshot must not see this turn.
            let _ = tokio::time::timeout_at(deadline, ending.wait_for(|done| *done)).await;
        }

        let id = CheckpointId::new();
        let mut row = CheckpointRow::new(
            id,
            session,
            CheckpointKind::Turn,
            CheckpointState::Running,
            now_ms(),
        );
        row.checkout = Some(slot.cwd.clone());
        // Everything git does counts against the budget, finding the repository included.
        let this = Arc::clone(self);
        let cwd = slot.cwd.clone();
        let caps = self.caps(&project);
        let first = row.clone();
        let mut handle = tokio::spawn(async move {
            let mut row = first;
            let (toplevel, common_dir) = match this.engine.repository(&cwd).await {
                Ok(found) => found,
                Err(error) => return (row, Err(error)),
            };
            let label = Label::before_turn(row.session_id.to_string(), row.id.to_string());
            row.checkout = Some(toplevel);
            row.common_dir = Some(common_dir);
            row.reference = label.reference();
            // Without its row no ref may be written (reconcile would take it for a leftover).
            if !this.write(row.clone()).await {
                row.reference = None;
                let error =
                    CheckpointError::Internal("the checkpoint could not be recorded".into());
                return (row, Err(error));
            }
            let snapshot = this.engine.snapshot_with(&cwd, label, caps).await;
            (row, snapshot)
        });
        let start = match tokio::time::timeout_at(deadline, &mut handle).await {
            Ok(result) => match settled(&row, result) {
                (written, Ok(snapshot)) => {
                    row = written;
                    self.set_status(&slot, None);
                    Start::Taken(Box::new(snapshot))
                }
                (written, Err(error)) => {
                    self.record_failure(&slot, written, &error).await;
                    return None;
                }
            },
            Err(_) => {
                info!(%session, checkpoint = %id, budget_ms = self.start_budget.as_millis(), "the start snapshot is late; the prompt goes first");
                Start::Late(handle)
            }
        };
        slot.state.lock().pending = Some(Pending {
            row,
            turn: None,
            start,
            during: BTreeSet::new(),
        });
        Some(id)
    }

    /// The user saved `path` (canonical) through the file service: every turn running now
    /// remembers it, and the one whose checkout holds it lists it as `during` when restored.
    pub fn saved(&self, path: &Path) {
        let slots: Vec<Arc<Slot>> = self.sessions.lock().values().cloned().collect();
        for slot in slots {
            if let Some(pending) = slot.state.lock().pending.as_mut()
                && pending.during.len() < DURING_KEPT
            {
                pending.during.insert(path.to_owned());
            }
        }
    }

    /// The turn of checkpoint `id` was sent; the agent calls it `turn`.
    pub fn sent(&self, session: SessionId, id: CheckpointId, turn: TurnId) {
        if let Some(slot) = self.slot(session)
            && let Some(pending) = slot.state.lock().pending.as_mut()
            && pending.row.id == id
        {
            pending.turn = Some(turn);
        }
    }

    /// The turn of checkpoint `id` could not be sent: its start is removed, row and ref.
    pub fn abandoned(self: &Arc<Self>, session: SessionId, id: CheckpointId) {
        let Some(slot) = self.slot(session) else {
            return;
        };
        let Some((pending, guard)) = self.take_pending(&slot, |p| p.row.id == id) else {
            return;
        };
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let _guard = guard;
            let (_, start) = pending.start.outcome(pending.row.clone()).await;
            if let Ok(start) = &start
                && let Some(reference) = &start.reference
                && let Err(error) = this
                    .engine
                    .delete_refs(&start.common_dir, std::slice::from_ref(reference))
                    .await
            {
                warn!(%error, "checkpoints: removing the start of a turn that was not sent");
            }
            let id = pending.row.id;
            this.store("removing a checkpoint", move |s| {
                s.checkpoints().delete(vec![id])
            })
            .await;
        });
    }

    /// The session's running turn ended (completed, failed, interrupted, cut off by the
    /// agent's exit or the daemon's stop); `turn` is the agent's id of it. The end is
    /// recorded in the background; the next turn's start waits for it.
    pub fn turn_ended(self: &Arc<Self>, session: SessionId, turn: Option<TurnId>) {
        let Some(slot) = self.slot(session) else {
            return;
        };
        let Some((pending, guard)) = self.take_pending(&slot, |_| true) else {
            return;
        };
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let turn = turn.or(pending.turn);
            let repository = this.end(&slot, pending, turn).await;
            // The next turn may start; retention follows.
            drop(guard);
            if let Some(repository) = repository {
                this.retain(Some(repository)).await;
            }
        });
    }

    /// How many files turn `turn` of `session` changed (`None`: its latest turn), once the
    /// turn's end is recorded. `None` when no checkpoint of the turn can say: the session has
    /// none (not an agent chat), they were off, the folder cannot have them, or a snapshot
    /// failed. The task service judges what a turn left for review by it (FX-REVIEW).
    pub async fn files_changed(&self, session: SessionId, turn: Option<TurnId>) -> Option<u64> {
        let slot = self.slot(session)?;
        let ending = slot.state.lock().ending.clone();
        if let Some(mut ending) = ending {
            // Gone without saying so: the end was recorded or never will be; the row says.
            let _ = ending.wait_for(|done| *done).await;
        }
        let rows = self
            .store("reading the checkpoints of a turn", move |s| {
                s.checkpoints().list(CheckpointFilter::Session(session))
            })
            .await?;
        let row = rows
            .iter()
            .rev()
            .filter(|row| row.kind == CheckpointKind::Turn)
            .find(|row| turn.is_none() || row.turn_id == turn)?;
        matches!(
            row.state,
            CheckpointState::Ok | CheckpointState::Late | CheckpointState::Unchanged
        )
        .then_some(row.files_changed)
    }

    /// Takes the pending turn that `which` accepts, marking its end as in flight.
    fn take_pending(
        &self,
        slot: &Slot,
        which: impl FnOnce(&Pending) -> bool,
    ) -> Option<(Pending, InFlight)> {
        let mut state = slot.state.lock();
        if !state.pending.as_ref().is_some_and(which) {
            return None;
        }
        let pending = state.pending.take()?;
        let (done, ending) = watch::channel(false);
        state.ending = Some(ending);
        self.in_flight.send_modify(|n| *n += 1);
        Some((
            pending,
            InFlight {
                count: self.in_flight.clone(),
                done: Some(done),
            },
        ))
    }

    async fn record_failure(&self, slot: &Slot, mut row: CheckpointRow, error: &CheckpointError) {
        let (state, status) = classify(error);
        if state != CheckpointState::Failed {
            self.set_status(slot, status);
        }
        row.state = state;
        row.reason = Some(error.to_string());
        row.reference = None;
        row.ended_ms = Some(now_ms());
        debug!(checkpoint = %row.id, %error, state = state.as_str(), "no checkpoint for this turn");
        self.record(row).await;
    }

    /// Records the end of a turn; returns the repository whose retention is due.
    async fn end(&self, slot: &Slot, pending: Pending, turn: Option<TurnId>) -> Option<PathBuf> {
        let Pending {
            row, start, during, ..
        } = pending;
        let late = matches!(start, Start::Late(_));
        let (mut row, start) = start.outcome(row).await;
        row.turn_id = turn;
        let start = match start {
            Ok(start) => start,
            Err(error) => {
                self.record_failure(slot, row, &error).await;
                return None;
            }
        };
        let start_ref = start.reference.clone();
        let common_dir = start.common_dir.clone();
        row.checkout = Some(start.checkout.clone());
        row.common_dir = Some(common_dir.clone());
        row.start_commit = start.commit.clone();
        row.reason = late.then(|| {
            format!(
                "the start was recorded after the prompt went to the agent (it took longer than {} s)",
                self.start_budget.as_secs_f32()
            )
        });

        let project = self.project(slot);
        if project.mode == CheckpointMode::Off {
            row.state = CheckpointState::Off;
            row.reason = Some("checkpoints were turned off during the turn".to_owned());
            row.reference = None;
            row.ended_ms = Some(now_ms());
            row.bytes = start.bytes_hashed;
            self.record(row).await;
            self.delete_refs(&common_dir, start_ref).await;
            return None;
        }

        let mut label = Label::new(
            row.session_id.to_string(),
            turn.map_or_else(|| row.id.to_string(), |t| t.to_string()),
            Kind::End,
        )
        .with_checkpoint(row.id.to_string())
        .unless_tree(start.tree.clone());
        label.turn = turn.map(|t| t.to_string());
        if let Some(commit) = &start.commit {
            label = label.with_parent(commit.clone());
        }
        let ended = self
            .engine
            .snapshot_with(&start.checkout, label, self.caps(&project))
            .await;
        row.ended_ms = Some(now_ms());
        match ended {
            Ok(end) => {
                self.set_status(slot, None);
                row.bytes = start.bytes_hashed.saturating_add(end.bytes_hashed);
                row.skipped = skipped(&start, Some(&end));
                row.during = saved_within(&during, &start.checkout);
                row.tree = Some(end.tree.clone());
                row.state = if late {
                    CheckpointState::Late
                } else {
                    CheckpointState::Ok
                };
                match (&end.commit, &start.commit) {
                    (Some(commit), Some(start_commit)) => {
                        match self
                            .engine
                            .diff_stat(&common_dir, start_commit, commit)
                            .await
                        {
                            Ok(stat) => {
                                row.files_changed = stat.files;
                                row.lines_added = stat.added;
                                row.lines_removed = stat.removed;
                            }
                            Err(error) => warn!(%error, "checkpoints: counting a turn's changes"),
                        }
                        row.commit = Some(commit.clone());
                        row.reference = end.reference.clone();
                    }
                    _ => {
                        // Nothing changed: no end ref (a late turn stays `late`: its start
                        // may hold its edits already).
                        if !late {
                            row.state = CheckpointState::Unchanged;
                        }
                        row.commit = None;
                        row.reference = None;
                    }
                }
            }
            Err(error) => {
                let (state, status) = classify(&error);
                if state != CheckpointState::Failed {
                    self.set_status(slot, status);
                }
                row.state = state;
                row.reason = Some(error.to_string());
                row.reference = None;
                row.bytes = start.bytes_hashed;
                row.skipped = skipped(&start, None);
            }
        }
        debug!(checkpoint = %row.id, state = row.state.as_str(), files = row.files_changed, "turn checkpoint recorded");
        // The row names the end ref before the start ref goes.
        self.record(row).await;
        self.delete_refs(&common_dir, start_ref).await;
        Some(common_dir)
    }

    async fn delete_refs(&self, common_dir: &Path, reference: Option<String>) {
        if let Some(reference) = reference
            && let Err(error) = self.engine.delete_refs(common_dir, &[reference]).await
        {
            warn!(%error, "checkpoints: deleting a start ref");
        }
    }

    /// Waits up to `wait` for the turn ends being recorded (at daemon stop).
    pub async fn drain(&self, wait: Duration) {
        let mut count = self.in_flight.subscribe();
        if tokio::time::timeout(wait, count.wait_for(|n| *n == 0))
            .await
            .is_err()
        {
            warn!(
                remaining = *count.borrow(),
                "checkpoints of ended turns are still being recorded; leaving them"
            );
        }
    }

    // Upkeep ---------------------------------------------------------------------------------

    /// Reconcile and retention at start, then retention once a day, until `shutdown`.
    pub async fn upkeep(self: Arc<Self>, shutdown: CancellationToken) {
        self.reconcile().await;
        self.retain_all().await;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = tokio::time::sleep(RETENTION_EVERY) => self.retain_all().await,
            }
        }
    }

    /// Brings the rows of earlier daemons and the refs of their repositories back in line
    /// (see the module documentation).
    pub async fn reconcile(&self) {
        let _one = self.upkeep.lock().await;
        let Some(repositories) = self
            .store("listing checkpoint repositories", |s| {
                s.checkpoints().repositories()
            })
            .await
        else {
            return;
        };
        for repository in repositories {
            self.reconcile_repository(&repository).await;
        }
    }

    async fn reconcile_repository(&self, repository: &Path) {
        // Refs first, rows second: a ref this daemon writes from now on has its row already.
        let refs = if repository.is_dir() {
            match self.engine.list_refs(repository).await {
                Ok(refs) => refs,
                Err(error) => {
                    warn!(%error, repository = %repository.display(), "checkpoints: listing refs");
                    return;
                }
            }
        } else {
            Vec::new()
        };
        let dir = repository.to_owned();
        let Some(rows) = self
            .store("reading checkpoints", move |s| {
                s.checkpoints()
                    .list(CheckpointFilter::Repository(Some(dir)))
            })
            .await
        else {
            return;
        };
        let by_name: HashMap<&str, &RefInfo> = refs.iter().map(|r| (r.name.as_str(), r)).collect();
        let ends_by_checkpoint: HashMap<&str, &RefInfo> = refs
            .iter()
            .filter(|r| {
                !r.kind.as_deref().is_some_and(Kind::is_start) && !r.name.ends_with("-start")
            })
            .filter_map(|r| r.checkpoint.as_deref().map(|c| (c, r)))
            .collect();
        let known: HashSet<String> = rows.iter().map(|r| r.id.to_string()).collect();
        let mut referenced: HashSet<String> = HashSet::new();
        let mut gone: Vec<CheckpointId> = Vec::new();
        let mut repaired: Vec<CheckpointRow> = Vec::new();
        for row in &rows {
            if row.created_ms >= self.boot_ms {
                // This daemon's: leave it and its refs alone.
                referenced.extend(row.reference.clone());
                continue;
            }
            if row.state == CheckpointState::Running {
                let mut row = row.clone();
                match ends_by_checkpoint.get(row.id.to_string().as_str()) {
                    Some(end) => {
                        referenced.insert(end.name.clone());
                        self.adopt_into(&mut row, end, repository).await;
                    }
                    None => {
                        row.state = CheckpointState::Failed;
                        row.reason = Some(STOPPED_REASON.to_owned());
                        row.reference = None;
                    }
                }
                repaired.push(row);
                continue;
            }
            if let Some(reference) = &row.reference {
                if by_name.contains_key(reference.as_str()) {
                    referenced.insert(reference.clone());
                } else {
                    gone.push(row.id);
                }
            }
        }
        let mut stale: Vec<String> = Vec::new();
        for info in &refs {
            if referenced.contains(&info.name) {
                continue;
            }
            let start =
                info.kind.as_deref().is_some_and(Kind::is_start) || info.name.ends_with("-start");
            let claimed = info.checkpoint.as_ref().is_some_and(|c| known.contains(c));
            if start {
                stale.push(info.name.clone());
            } else if claimed {
                // Its row is this daemon's turn ending right now.
            } else if let Some(row) = self.adopted(info, repository).await {
                repaired.push(row);
            } else {
                stale.push(info.name.clone());
            }
        }
        let (fixed, dropped, deleted) = (repaired.len(), gone.len(), stale.len());
        for row in repaired {
            self.write(row).await;
        }
        if !gone.is_empty() {
            self.store("dropping checkpoints without a ref", move |s| {
                s.checkpoints().delete(gone)
            })
            .await;
        }
        if !stale.is_empty()
            && let Err(error) = self.engine.delete_refs(repository, &stale).await
        {
            warn!(%error, "checkpoints: deleting leftover refs");
        }
        if fixed + dropped + deleted > 0 {
            info!(
                repository = %repository.display(),
                repaired = fixed,
                dropped,
                deleted,
                "checkpoints reconciled"
            );
        }
    }

    /// Fills `row` from its end ref (a turn whose end was written but not recorded).
    async fn adopt_into(&self, row: &mut CheckpointRow, end: &RefInfo, repository: &Path) {
        row.state = CheckpointState::Ok;
        row.reason = None;
        row.reference = Some(end.name.clone());
        row.commit = Some(end.commit.clone());
        row.tree = Some(end.tree.clone());
        row.start_commit = end.parent.clone();
        row.ended_ms = Some(end.created_ms);
        if row.turn_id.is_none() {
            row.turn_id = end.turn.as_deref().and_then(|t| t.parse().ok());
        }
        if let Some(parent) = &end.parent
            && let Ok(stat) = self.engine.diff_stat(repository, parent, &end.commit).await
        {
            row.files_changed = stat.files;
            row.lines_added = stat.added;
            row.lines_removed = stat.removed;
        }
    }

    /// A row for a checkpoint ref nobody knows, from its trailers; `None` when they do not
    /// say whose it is.
    async fn adopted(&self, info: &RefInfo, repository: &Path) -> Option<CheckpointRow> {
        let session: SessionId = info.session.as_deref()?.parse().ok()?;
        let id = info
            .checkpoint
            .as_deref()
            .and_then(|c| c.parse().ok())
            .unwrap_or_default();
        let kind = if info
            .kind
            .as_deref()
            .is_some_and(|k| k.starts_with("restore"))
        {
            CheckpointKind::Restore
        } else {
            CheckpointKind::Turn
        };
        let mut row = CheckpointRow::new(id, session, kind, CheckpointState::Ok, info.created_ms);
        // The main worktree's top level holds the common directory `.git`; a linked
        // worktree's is unknown, the repository stands for it.
        row.checkout = Some(match repository.file_name() {
            Some(name) if name == ".git" => repository.parent().unwrap_or(repository).to_owned(),
            _ => repository.to_owned(),
        });
        row.common_dir = Some(repository.to_owned());
        self.adopt_into(&mut row, info, repository).await;
        Some(row)
    }

    /// Retention over every repository, and over the sessions outside git.
    pub async fn retain_all(&self) {
        let repositories = self
            .store("listing checkpoint repositories", |s| {
                s.checkpoints().repositories()
            })
            .await
            .unwrap_or_default();
        for repository in repositories {
            self.retain(Some(repository)).await;
        }
        self.retain(None).await;
    }

    /// Retention over the checkpoints of one repository (`None`: outside git).
    pub async fn retain(&self, repository: Option<PathBuf>) {
        let _one = self.upkeep.lock().await;
        let filter = CheckpointFilter::Repository(repository.clone());
        let Some(rows) = self
            .store("reading checkpoints", move |s| s.checkpoints().list(filter))
            .await
        else {
            return;
        };
        let policy = {
            let settings = self.settings.read();
            Policy {
                keep_turns: usize::try_from(settings.keep_turns).unwrap_or(usize::MAX),
                keep_days: settings.keep_days,
                max_repo_bytes: settings.max_repo_bytes,
                max_repo_refs: usize::try_from(settings.max_repo_refs).unwrap_or(usize::MAX),
            }
        };
        let drop: HashSet<CheckpointId> = retention::plan(&rows, now_ms(), &policy)
            .into_iter()
            .collect();
        if !drop.is_empty() {
            let refs: Vec<String> = rows
                .iter()
                .filter(|row| drop.contains(&row.id))
                .filter_map(|row| row.reference.clone())
                .collect();
            if let Some(dir) = repository.as_deref().filter(|d| d.is_dir())
                && let Err(error) = self.engine.delete_refs(dir, &refs).await
            {
                // The rows stay, so the next run tries again.
                warn!(%error, "checkpoints: retention could not delete refs");
                return;
            }
            let ids: Vec<CheckpointId> = drop.into_iter().collect();
            let count = ids.len();
            let gone = ids.clone();
            let deleted = self
                .store("dropping old checkpoints", move |s| {
                    s.checkpoints().delete(gone)
                })
                .await;
            if deleted.is_some() {
                self.announce_removed(ids);
            }
            info!(repository = ?repository, dropped = count, refs = refs.len(), "checkpoint retention");
        }
        if let Some(dir) = repository.as_deref().filter(|d| d.is_dir())
            && let Err(error) = self.engine.pack_refs_if_many(dir).await
        {
            warn!(%error, "checkpoints: packing refs");
        }
    }
}
