//! `fs.watch`: one `notify` watcher per root while some connection holds interest in it, the
//! coalescing of what it reports into `fs.changed` events, and the attribution of each change
//! to a writer.
//!
//! A watcher covers a whole tracked root (recursively: FSEvents is cheap that way) or, for a
//! picked path outside the roots, its directory. Events are filtered before anything else
//! (`.git`, `target`, `node_modules`, the service's own temporary files), gathered for 50 ms,
//! looked at on disk once, and sent to the connections whose watched paths they concern.
//! Everything between the platform and a connection's queue is bounded: the callback hands
//! batches to a bounded channel and, when it is full, only remembers that something was lost;
//! a flush lists at most 500 changes; a connection whose queue is full is told so in its next
//! event (`overflow`) instead of being closed.
//!
//! Nothing a connection could have missed goes unreported (FX-FSWATCH): the version of a file
//! a connection last read or wrote is remembered, and when it watches the file later, a
//! change made in between is fed to the file's watcher as if the platform had reported it
//! (a client reads a file first and watches it once it has it). And since FSEvents keeps a
//! path's "created" flag for a while (a write of a file created shortly before, or a save that
//! renames over a file, still says so), a path is reported `created` only when its file was
//! born after the watcher started and nothing was known at the path yet (`Births`).

use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant as StdInstant, SystemTime};

use hutan_core::agent::Delivery;
use hutan_core::{AgentEventKind, CheckpointId, ItemId, SessionId};
use hutan_protocol::fs::{
    AgentConfidence, COALESCE_MS, ChangeKind, FileVersion, FsChange, FsOrigin, FsWatching,
    GENERATION_INTERVAL_MS, WATCH_MAX_PATHS,
};
use hutan_protocol::messages::{AgentState, Event, SessionInfo, SessionListFilter};
use hutan_protocol::{ControlMessage, Frame, MAX_PAYLOAD};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use super::confine::FsError;
use super::find::is_noise_dir;
use crate::bus::EventBus;
use crate::conn::{ConnHandle, ConnId, LocalSend};
use crate::registry::RegistryHandle;

/// Batches the platform callback may leave unread before it starts counting them as lost.
const RAW_QUEUE: usize = 1024;
/// Own writes remembered, and for how long.
const OWN_WRITES_MAX: usize = 1024;
const OWN_WRITE_TTL: Duration = Duration::from_secs(10);
/// Files agents reported changing, remembered, and how long before a change they still count.
const NOTES_MAX: usize = 512;
const NOTE_WINDOW: Duration = Duration::from_secs(15);
/// Versions connections were told of files (reads, own writes), remembered for their watches.
const SERVED_MAX: usize = 1024;
/// Paths whose birth a watcher remembers, for telling created from modified.
const BIRTHS_MAX: usize = 1024;
/// Paths of restores remembered (a restore announces each file it writes or deletes).
const RESTORES_MAX: usize = 4096;

/// What one watcher covers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RootKey {
    pub path: PathBuf,
    pub recursive: bool,
}

/// One path a connection watches.
#[derive(Debug, Clone)]
struct Spec {
    recursive: bool,
    root: RootKey,
}

#[derive(Debug)]
struct ConnWatch {
    handle: ConnHandle,
    paths: HashMap<PathBuf, Spec>,
    /// An event for this connection was dropped: the next one says so.
    lagged: bool,
}

struct RootEntry {
    /// Stops the platform watcher when dropped.
    _watcher: RecommendedWatcher,
    /// The watcher's window, for changes found outside the platform's stream.
    feed: Feed,
    /// What the watcher knows exists, for telling created from modified.
    births: Arc<Mutex<Births>>,
    generation: Arc<AtomicU64>,
    stop: CancellationToken,
}

/// Where a watcher's raw changes go: its window's bounded queue, and the flag that says some
/// were lost because it was full.
#[derive(Debug, Clone)]
struct Feed {
    raws: mpsc::Sender<Vec<Raw>>,
    lost: Arc<AtomicBool>,
}

impl Feed {
    fn send(&self, raws: Vec<Raw>) {
        if !raws.is_empty() && self.raws.try_send(raws).is_err() {
            self.lost.store(true, Ordering::Release);
        }
    }
}

impl std::fmt::Debug for RootEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootEntry")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct State {
    conns: HashMap<ConnId, ConnWatch>,
    roots: HashMap<RootKey, RootEntry>,
}

/// A write of this daemon's own, waiting to be recognised in the stream of changes.
#[derive(Debug, Clone)]
struct OwnWrite {
    path: PathBuf,
    /// Everything below `path` is the write's too (a folder an operation made, moved or
    /// removed).
    tree: bool,
    write_id: String,
    conn: ConnId,
    /// The session of the writing connection, when it is an agent's.
    agent: Option<SessionId>,
    /// Unknown until the write is done.
    version: Option<FileVersion>,
    at: StdInstant,
}

/// A file an agent said it changed.
#[derive(Debug, Clone)]
struct Note {
    session: SessionId,
    item: Option<ItemId>,
    path: String,
    at: StdInstant,
}

/// The version connection `conn` last read or wrote of `path`.
#[derive(Debug, Clone)]
struct Served {
    conn: ConnId,
    path: PathBuf,
    version: FileVersion,
}

/// A path a restore changed, waiting to be recognised in the stream of changes.
#[derive(Debug, Clone)]
struct Restored {
    path: PathBuf,
    checkpoint: CheckpointId,
    at: StdInstant,
}

#[derive(Debug)]
pub struct WatchHub {
    registry: RegistryHandle,
    bus: Arc<EventBus>,
    shutdown: CancellationToken,
    /// Most changes of one `fs.changed` event.
    changes_max: usize,
    state: Mutex<State>,
    writes: Mutex<VecDeque<OwnWrite>>,
    notes: Mutex<VecDeque<Note>>,
    served: Mutex<VecDeque<Served>>,
    restores: Mutex<VecDeque<Restored>>,
}

impl WatchHub {
    pub fn new(
        registry: RegistryHandle,
        bus: Arc<EventBus>,
        shutdown: CancellationToken,
        changes_max: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            registry,
            bus,
            shutdown,
            changes_max,
            state: Mutex::new(State::default()),
            writes: Mutex::new(VecDeque::new()),
            notes: Mutex::new(VecDeque::new()),
            served: Mutex::new(VecDeque::new()),
            restores: Mutex::new(VecDeque::new()),
        })
    }

    // Interest ---------------------------------------------------------------------------------

    /// Adds `paths` (canonical, with the watcher each belongs to) to the connection's interest,
    /// starting the watchers that are not running. Blocking: starting a watcher is a syscall.
    pub fn watch(
        self: &Arc<Self>,
        conn: &ConnHandle,
        paths: Vec<(PathBuf, bool, RootKey)>,
    ) -> Result<FsWatching, FsError> {
        let mut state = self.state.lock();
        let held = state.conns.get(&conn.id());
        let fresh = paths
            .iter()
            .filter(|(path, _, _)| held.is_none_or(|held| !held.paths.contains_key(path)))
            .map(|(path, _, _)| path)
            .collect::<std::collections::HashSet<_>>()
            .len();
        let have = held.map_or(0, |held| held.paths.len());
        if have + fresh > WATCH_MAX_PATHS {
            return Err(FsError::bad(format!(
                "a connection watches at most {WATCH_MAX_PATHS} paths ({have} are watched, {fresh} more were asked for)"
            )));
        }
        // Watchers first: a refusal leaves the interest as it was.
        for (_, _, key) in &paths {
            if !state.roots.contains_key(key) {
                let entry = self.start_root(key)?;
                state.roots.insert(key.clone(), entry);
            }
        }
        let watch = state.conns.entry(conn.id()).or_insert_with(|| ConnWatch {
            handle: conn.clone(),
            paths: HashMap::new(),
            lagged: false,
        });
        let mut added = Vec::new();
        for (path, recursive, root) in paths {
            debug!(conn = ?conn.id(), path = %path.display(), recursive, root = %root.path.display(), "watch interest");
            let spec = Spec {
                recursive,
                root: root.clone(),
            };
            if watch.paths.insert(path.clone(), spec).is_none() {
                added.push((path, root));
            }
        }
        // A newly watched path exists: a later platform "created" of it is no creation. And
        // what the connection was told of it may be older than the watch: a change since then
        // is fed to the path's watcher, which is running and now has the interest, so that it
        // is reported like any other (attribution included).
        struct Added {
            path: PathBuf,
            known: Option<FileVersion>,
            feed: Feed,
            births: Arc<Mutex<Births>>,
        }
        let mut served = self.take_served(conn.id(), &added);
        let added: Vec<Added> = added
            .into_iter()
            .filter_map(|(path, root)| {
                let entry = state.roots.get(&root)?;
                Some(Added {
                    known: served.remove(&path),
                    path,
                    feed: entry.feed.clone(),
                    births: entry.births.clone(),
                })
            })
            .collect();
        let watching = summary(&state, conn.id());
        drop(state);
        for Added {
            path,
            known,
            feed,
            births,
        } in added
        {
            let meta = std::fs::symlink_metadata(&path).ok();
            if let Some(meta) = &meta {
                births.lock().exists(&path, meta, false);
            }
            let Some(known) = known else { continue };
            let now = meta.filter(|m| m.is_file()).map(|m| version_of(&m));
            if now == Some(known) {
                continue;
            }
            debug!(path = %path.display(), ?known, ?now, "changed between the connection's read and its watch");
            feed.send(vec![if now.is_some() {
                Raw::Modified(path)
            } else {
                Raw::Removed(path)
            }]);
        }
        Ok(watching)
    }

    /// Connection `conn` was told that `path` (canonical) has `version`: it read it, or wrote
    /// it. A watch of the file it starts later reports a change since.
    pub fn served(&self, conn: ConnId, path: &Path, version: FileVersion) {
        let mut served = self.served.lock();
        served.retain(|s| !(s.conn == conn && s.path == path));
        if served.len() >= SERVED_MAX {
            served.pop_front();
        }
        served.push_back(Served {
            conn,
            path: path.to_owned(),
            version,
        });
    }

    /// The versions connection `conn` was told of `paths`, forgotten from now on: the watch
    /// reports what follows.
    fn take_served(
        &self,
        conn: ConnId,
        paths: &[(PathBuf, RootKey)],
    ) -> HashMap<PathBuf, FileVersion> {
        let mut known = HashMap::new();
        if paths.is_empty() {
            return known;
        }
        self.served.lock().retain(|s| {
            let watched = s.conn == conn && paths.iter().any(|(path, _)| *path == s.path);
            if watched {
                known.insert(s.path.clone(), s.version);
            }
            !watched
        });
        known
    }

    /// Releases `paths` of the connection; a watcher nobody needs any more stops.
    pub fn unwatch(&self, conn: ConnId, paths: &[PathBuf]) -> FsWatching {
        let mut state = self.state.lock();
        if let Some(watch) = state.conns.get_mut(&conn) {
            for path in paths {
                watch.paths.remove(path);
            }
        }
        prune(&mut state);
        summary(&state, conn)
    }

    pub fn connection_closed(&self, conn: ConnId) {
        self.served.lock().retain(|s| s.conn != conn);
        let mut state = self.state.lock();
        state.conns.remove(&conn);
        prune(&mut state);
    }

    /// The change counter of the watcher that covers `path`, when one runs: the finder's index
    /// of a watched root is valid while it holds.
    pub fn generation_of(&self, path: &Path) -> Option<u64> {
        let state = self.state.lock();
        state
            .roots
            .iter()
            .filter(|(key, _)| path.starts_with(&key.path))
            .map(|(_, entry)| entry.generation.load(Ordering::Acquire))
            .max()
    }

    // Attribution inputs -----------------------------------------------------------------------

    /// A write of connection `conn` is about to replace `path`. Registered before the rename so
    /// that the change it causes cannot be reported before it is known to be ours.
    pub fn write_started(
        &self,
        path: &Path,
        tree: bool,
        conn: ConnId,
        agent: Option<SessionId>,
        write_id: &str,
    ) {
        let mut writes = self.writes.lock();
        if writes.len() >= OWN_WRITES_MAX {
            writes.pop_front();
        }
        writes.push_back(OwnWrite {
            path: path.to_owned(),
            tree,
            write_id: write_id.to_owned(),
            conn,
            agent,
            version: None,
            at: StdInstant::now(),
        });
    }

    /// A file operation (`fs.create`, `fs.move`, …) is done: what it announced counts as its own
    /// for the time a change takes to arrive. There is no version to compare.
    pub fn write_finished(&self, write_id: &str) {
        let now = StdInstant::now();
        for write in self
            .writes
            .lock()
            .iter_mut()
            .filter(|w| w.write_id == write_id)
        {
            write.at = now;
        }
    }

    /// The write is done and the file has `version`.
    pub fn write_done(&self, write_id: &str, version: Option<FileVersion>) {
        let mut writes = self.writes.lock();
        match version {
            Some(version) => {
                if let Some(write) = writes.iter_mut().find(|w| w.write_id == write_id) {
                    write.version = Some(version);
                    write.at = StdInstant::now();
                }
            }
            None => writes.retain(|w| w.write_id != write_id),
        }
    }

    /// A restore of checkpoint `checkpoint` is about to change `path` (canonical): a file it
    /// writes or deletes, or a folder it makes or removes. Registered before the change.
    pub fn restoring(&self, path: &Path, checkpoint: CheckpointId) {
        let mut restores = self.restores.lock();
        if restores.len() >= RESTORES_MAX {
            restores.pop_front();
        }
        restores.push_back(Restored {
            path: path.to_owned(),
            checkpoint,
            at: StdInstant::now(),
        });
    }

    /// Notes the files of a chat's `file_diff` event.
    pub fn observe(&self, event: &Event) {
        let Event::ChatEvent { session_id, event } = event else {
            return;
        };
        if event.delivery != Delivery::Live {
            return;
        }
        let AgentEventKind::FileDiff { changes, .. } = &event.kind else {
            return;
        };
        let mut notes = self.notes.lock();
        for change in changes {
            if notes.len() >= NOTES_MAX {
                notes.pop_front();
            }
            notes.push_back(Note {
                session: *session_id,
                item: event.item_id,
                path: change.path.clone(),
                at: StdInstant::now(),
            });
        }
    }

    // Watchers ---------------------------------------------------------------------------------

    fn start_root(self: &Arc<Self>, key: &RootKey) -> Result<RootEntry, FsError> {
        let (tx, rx) = mpsc::channel::<Vec<Raw>>(RAW_QUEUE);
        let lost = Arc::new(AtomicBool::new(false));
        let feed = Feed {
            raws: tx,
            lost: lost.clone(),
        };
        let from_platform = feed.clone();
        let root_for_log = key.path.clone();
        let mut watcher = notify::recommended_watcher(
            move |result: notify::Result<notify::Event>| match result {
                Ok(event) => {
                    trace!(root = %root_for_log.display(), kind = ?event.kind, paths = ?event.paths, "platform event");
                    from_platform.send(raw_of(event));
                }
                Err(error) => warn!(root = %root_for_log.display(), %error, "file watcher error"),
            },
        )
        .map_err(|e| {
            FsError::io(
                format!("watching {}", key.path.display()),
                std::io::Error::other(e),
            )
        })?;
        let mode = if key.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        // Files born before this are not created as far as this watcher's clients go.
        let births = Arc::new(Mutex::new(Births::new(SystemTime::now())));
        watcher.watch(&key.path, mode).map_err(|e| {
            FsError::io(
                format!("watching {}", key.path.display()),
                std::io::Error::other(e),
            )
        })?;
        let generation = Arc::new(AtomicU64::new(0));
        let stop = self.shutdown.child_token();
        tokio::spawn(run_root(
            self.clone(),
            key.clone(),
            rx,
            lost,
            generation.clone(),
            stop.clone(),
            births.clone(),
        ));
        debug!(root = %key.path.display(), recursive = key.recursive, "watching");
        Ok(RootEntry {
            _watcher: watcher,
            feed,
            births,
            generation,
            stop,
        })
    }

    // Delivery ---------------------------------------------------------------------------------

    pub(super) fn frame(&self, event: &Event) -> Option<Frame> {
        let frame = ControlMessage::event(self.bus.epoch(), self.bus.last_seq(), event)
            .ok()?
            .to_frame()
            .ok()?;
        (frame.payload.len() <= MAX_PAYLOAD).then_some(frame)
    }

    /// Sends what happened below `key` to the connections that watch it.
    fn deliver(&self, key: &RootKey, changes: &[Prepared], overflow: bool) {
        // What each interested connection gets, decided under the lock; sent outside it.
        let mut outgoing: Vec<(ConnHandle, ConnId, Vec<FsChange>, bool)> = Vec::new();
        {
            let mut state = self.state.lock();
            for (id, watch) in &mut state.conns {
                let specs: Vec<(&PathBuf, &Spec)> = watch
                    .paths
                    .iter()
                    .filter(|(_, spec)| spec.root == *key)
                    .collect();
                if specs.is_empty() {
                    continue;
                }
                let mut mine: Vec<FsChange> = changes
                    .iter()
                    .filter(|change| {
                        specs.iter().any(|(path, spec)| {
                            interested(path, spec, &change.change.path)
                                || change
                                    .change
                                    .from
                                    .as_deref()
                                    .is_some_and(|from| interested(path, spec, from))
                        })
                    })
                    .map(|change| change.for_connection(*id))
                    .collect();
                let mut overflowed = overflow;
                if mine.len() > self.changes_max {
                    mine.truncate(self.changes_max);
                    overflowed = true;
                }
                if mine.is_empty() && !overflowed {
                    continue;
                }
                outgoing.push((watch.handle.clone(), *id, mine, overflowed || watch.lagged));
            }
        }
        for (handle, id, changes, overflow) in outgoing {
            let count = changes.len();
            let Some(frame) = self.frame(&Event::FsChanged { changes, overflow }) else {
                warn!("an fs.changed event does not fit a protocol frame; skipped");
                continue;
            };
            let sent = handle.enqueue_local(frame);
            debug!(conn = ?id, changes = count, overflow, ?sent, "fs.changed");
            let mut state = self.state.lock();
            match sent {
                LocalSend::Queued => {
                    if let Some(watch) = state.conns.get_mut(&id) {
                        watch.lagged = false;
                    }
                }
                LocalSend::Dropped => {
                    if let Some(watch) = state.conns.get_mut(&id) {
                        watch.lagged = true;
                    }
                }
                LocalSend::Gone => {
                    state.conns.remove(&id);
                    prune(&mut state);
                }
            }
        }
    }

    fn send_generation(&self, key: &RootKey, generation: u64) {
        let targets: Vec<ConnHandle> = {
            let state = self.state.lock();
            state
                .conns
                .values()
                .filter(|watch| watch.paths.values().any(|spec| spec.root == *key))
                .map(|watch| watch.handle.clone())
                .collect()
        };
        let event = Event::FsGeneration {
            root: key.path.clone(),
            generation,
        };
        let Some(frame) = self.frame(&event) else {
            return;
        };
        for handle in targets {
            // A lost generation is replaced by the next one.
            let _ = handle.enqueue_local(frame.clone());
        }
    }
}

fn summary(state: &State, conn: ConnId) -> FsWatching {
    let (watching, mut roots): (usize, Vec<PathBuf>) = state.conns.get(&conn).map_or_else(
        || (0, Vec::new()),
        |watch| {
            (
                watch.paths.len(),
                watch.paths.values().map(|s| s.root.path.clone()).collect(),
            )
        },
    );
    roots.sort();
    roots.dedup();
    FsWatching {
        watching: u32::try_from(watching).unwrap_or(u32::MAX),
        roots,
    }
}

/// Stops the watchers nobody watches through any more, and forgets connections without paths.
fn prune(state: &mut State) {
    state.conns.retain(|_, watch| !watch.paths.is_empty());
    let used: std::collections::HashSet<&RootKey> = state
        .conns
        .values()
        .flat_map(|watch| watch.paths.values().map(|spec| &spec.root))
        .collect();
    let unused: Vec<RootKey> = state
        .roots
        .keys()
        .filter(|key| !used.contains(key))
        .cloned()
        .collect();
    for key in unused {
        if let Some(entry) = state.roots.remove(&key) {
            entry.stop.cancel();
            debug!(root = %key.path.display(), "no longer watching");
        }
    }
}

/// Whether a change at `path` concerns a connection that watches `watched`.
fn interested(watched: &Path, spec: &Spec, path: &Path) -> bool {
    path == watched
        || (spec.recursive && path.starts_with(watched))
        || (!spec.recursive && path.parent() == Some(watched))
}

// Raw events and the coalescing window ---------------------------------------------------------

#[derive(Debug)]
enum Raw {
    Created(PathBuf),
    Removed(PathBuf),
    Modified(PathBuf),
    /// A rename seen from one side; whether the path exists tells which.
    Name(PathBuf),
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
}

fn raw_of(event: notify::Event) -> Vec<Raw> {
    use notify::event::{ModifyKind, RenameMode};
    match event.kind {
        EventKind::Access(_) => Vec::new(),
        EventKind::Create(_) => event.paths.into_iter().map(Raw::Created).collect(),
        EventKind::Remove(_) => event.paths.into_iter().map(Raw::Removed).collect(),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() == 2 => {
            let mut paths = event.paths.into_iter();
            match (paths.next(), paths.next()) {
                (Some(from), Some(to)) => vec![Raw::Renamed { from, to }],
                _ => Vec::new(),
            }
        }
        EventKind::Modify(ModifyKind::Name(_)) => event.paths.into_iter().map(Raw::Name).collect(),
        EventKind::Modify(_) | EventKind::Any | EventKind::Other => {
            event.paths.into_iter().map(Raw::Modified).collect()
        }
    }
}

/// What one path went through in a window (whether it is gone, disk tells).
#[derive(Debug, Default, Clone)]
struct Entry {
    created: bool,
    from: Option<PathBuf>,
}

#[derive(Debug)]
struct Window {
    entries: HashMap<PathBuf, Entry>,
    overflow: bool,
    /// Paths the window remembers (twice the changes of an event); more are lost (`overflow`).
    capacity: usize,
}

impl Window {
    fn new(changes_max: usize) -> Self {
        Self {
            entries: HashMap::new(),
            overflow: false,
            capacity: 2 * changes_max,
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty() && !self.overflow
    }

    fn entry(&mut self, key: &RootKey, path: PathBuf) -> Option<&mut Entry> {
        if !accepted(key, &path) {
            trace!(root = %key.path.display(), path = %path.display(), "change filtered");
            return None;
        }
        if !self.entries.contains_key(&path) && self.entries.len() >= self.capacity {
            self.overflow = true;
            return None;
        }
        Some(self.entries.entry(path).or_default())
    }

    fn add(&mut self, key: &RootKey, raws: Vec<Raw>) {
        for raw in raws {
            match raw {
                Raw::Created(path) => {
                    if let Some(entry) = self.entry(key, path) {
                        entry.created = true;
                    }
                }
                Raw::Removed(path) | Raw::Modified(path) | Raw::Name(path) => {
                    self.entry(key, path);
                }
                Raw::Renamed { from, to } => {
                    self.entry(key, from.clone());
                    if let Some(entry) = self.entry(key, to) {
                        entry.from = Some(from);
                    }
                }
            }
        }
    }
}

/// Whether a path below the watcher's root is worth reporting: not inside `.git`, `target` or
/// `node_modules`, not one of the service's own temporary files.
fn accepted(key: &RootKey, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(&key.path) else {
        return false;
    };
    let named = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect::<Vec<_>>();
    !named.iter().any(|name| is_noise_dir(name))
        && !named.last().is_some_and(|name| {
            let name = name.to_string_lossy();
            name.starts_with(".hutan-write-") && name.ends_with(".tmp")
        })
}

/// One change, looked at on disk, with what could be told about who made it.
#[derive(Debug, Clone)]
struct Prepared {
    change: FsChange,
    attribution: Attribution,
}

#[derive(Debug, Clone)]
enum Attribution {
    Own {
        conn: ConnId,
        write_id: String,
        agent: Option<SessionId>,
        version_matches: bool,
    },
    Agent {
        session: SessionId,
        item: Option<ItemId>,
        confidence: AgentConfidence,
    },
    /// A restore of files (`checkpoint.restore`): exact, like an own write.
    Restore {
        checkpoint: CheckpointId,
    },
    External,
}

impl Prepared {
    fn for_connection(&self, conn: ConnId) -> FsChange {
        let origin = match &self.attribution {
            Attribution::Own {
                conn: writer,
                write_id,
                version_matches,
                ..
            } if *writer == conn && *version_matches => FsOrigin::Own {
                write_id: write_id.clone(),
            },
            // Somebody else's write through the service: an agent's, or another client's.
            Attribution::Own {
                agent: Some(session),
                ..
            } => FsOrigin::Agent {
                session_id: *session,
                tool_call_id: None,
                confidence: AgentConfidence::Session,
            },
            Attribution::Own { .. } | Attribution::External => FsOrigin::External,
            Attribution::Restore { checkpoint } => FsOrigin::Restore {
                checkpoint_id: *checkpoint,
            },
            Attribution::Agent {
                session,
                item,
                confidence,
            } => FsOrigin::Agent {
                session_id: *session,
                tool_call_id: *item,
                confidence: *confidence,
            },
        };
        FsChange {
            origin: Some(origin),
            ..self.change.clone()
        }
    }
}

/// Everything a flush needs to attribute changes, taken before the blocking work.
struct Evidence {
    writes: Vec<OwnWrite>,
    restores: Vec<Restored>,
    notes: Vec<Note>,
    sessions: Vec<SessionInfo>,
}

/// Which of a watcher's platform "created" flags are news. FSEvents keeps a path's flags for a
/// while, and a file renamed into place carries its own: a write of a file created shortly
/// before, or a save that renames over a file, still says "created". A path is created only
/// when its file was born after the watcher started and nothing was known at the path yet
/// (from an earlier change, or because a connection watches it); otherwise it is modified.
#[derive(Debug)]
struct Births {
    /// When the watcher started.
    since: SystemTime,
    /// The birth time of what is known at each path; cleared when full (a creation then may be
    /// reported twice).
    known: HashMap<PathBuf, SystemTime>,
}

impl Births {
    fn new(since: SystemTime) -> Self {
        Self {
            since,
            known: HashMap::new(),
        }
    }

    /// `path` exists as `meta`, the platform said it was created (`flagged`) or not: whether
    /// it is created as far as the watcher's clients go. Remembered either way.
    fn exists(&mut self, path: &Path, meta: &std::fs::Metadata, flagged: bool) -> bool {
        // Without a birth time the platform's word is all there is.
        let Ok(born) = meta.created() else {
            return flagged;
        };
        if self.known.len() >= BIRTHS_MAX && !self.known.contains_key(path) {
            self.known.clear();
        }
        let before = self.known.insert(path.to_owned(), born);
        flagged && born >= self.since && before.is_none()
    }

    /// `path` is gone: something born there later is created.
    fn gone(&mut self, path: &Path) {
        self.known.remove(path);
    }
}

fn prepare(
    window: Vec<(PathBuf, Entry)>,
    evidence: Evidence,
    births: &mut Births,
) -> Vec<Prepared> {
    let folders = |session: &SessionInfo| -> Vec<PathBuf> {
        let mut folders: Vec<PathBuf> = [
            session.cwd.as_deref(),
            session.folder.as_deref(),
            session.placement.as_ref().map(|p| p.root.as_path()),
        ]
        .into_iter()
        .flatten()
        .map(canonical_lenient)
        .collect();
        folders.sort();
        folders.dedup();
        folders
    };
    // The files agents reported, made absolute against the folders of their session.
    let mut reported: HashMap<PathBuf, (SessionId, Option<ItemId>, StdInstant)> = HashMap::new();
    for note in evidence
        .notes
        .iter()
        .filter(|n| n.at.elapsed() <= NOTE_WINDOW)
    {
        let raw = Path::new(&note.path);
        let candidates: Vec<PathBuf> = if raw.is_absolute() {
            vec![canonical_lenient(raw)]
        } else {
            evidence
                .sessions
                .iter()
                .filter(|s| s.id == note.session)
                .flat_map(&folders)
                .map(|folder| canonical_lenient(&folder.join(raw)))
                .collect()
        };
        for candidate in candidates {
            let newer = reported
                .get(&candidate)
                .is_none_or(|(_, _, at)| *at <= note.at);
            if newer {
                reported.insert(candidate, (note.session, note.item, note.at));
            }
        }
    }
    // Sessions whose agent is at work, with the folders it works in.
    let working: Vec<(SessionId, Vec<PathBuf>)> = evidence
        .sessions
        .iter()
        .filter(|s| s.state.is_live())
        .filter(|s| {
            s.agent
                .as_ref()
                .is_some_and(|a| a.state == AgentState::Working)
                || s.chat.as_ref().is_some_and(|c| c.turn_active)
        })
        .map(|s| (s.id, folders(s)))
        .collect();

    window
        .into_iter()
        .map(|(path, entry)| {
            let (kind, version) = match std::fs::symlink_metadata(&path) {
                Ok(meta) => {
                    let version = meta.is_file().then(|| version_of(&meta));
                    let created = births.exists(&path, &meta, entry.created);
                    let kind = if entry.from.is_some() {
                        ChangeKind::Renamed
                    } else if created {
                        ChangeKind::Created
                    } else {
                        ChangeKind::Modified
                    };
                    (kind, version)
                }
                // Even when the platform says it was created inside the window: the flag may
                // be of an older creation (FSEvents), of a file a client knows.
                Err(_) => {
                    births.gone(&path);
                    (ChangeKind::Removed, None)
                }
            };
            let own = evidence.writes.iter().rfind(|w| {
                (w.path == path || (w.tree && path.starts_with(&w.path)))
                    && w.at.elapsed() <= OWN_WRITE_TTL
            });
            let restored = evidence
                .restores
                .iter()
                .rfind(|r| r.path == path && r.at.elapsed() <= OWN_WRITE_TTL)
                .filter(|r| own.is_none_or(|w| w.at <= r.at));
            let attribution = if let Some(restored) = restored {
                Attribution::Restore {
                    checkpoint: restored.checkpoint,
                }
            } else if let Some(write) = own {
                Attribution::Own {
                    conn: write.conn,
                    write_id: write.write_id.clone(),
                    agent: write.agent,
                    version_matches: write.version.is_none_or(|v| Some(v) == version),
                }
            } else if let Some((session, item, _)) = reported.get(&path) {
                Attribution::Agent {
                    session: *session,
                    item: *item,
                    confidence: AgentConfidence::ToolCall,
                }
            } else if let Some(session) = working_in(&working, &path) {
                Attribution::Agent {
                    session,
                    item: None,
                    confidence: AgentConfidence::Session,
                }
            } else {
                Attribution::External
            };
            Prepared {
                change: FsChange {
                    path,
                    kind,
                    from: entry.from,
                    version,
                    origin: None,
                },
                attribution,
            }
        })
        .collect()
}

/// The working session whose folder contains `path`, the deepest folder first.
fn working_in(working: &[(SessionId, Vec<PathBuf>)], path: &Path) -> Option<SessionId> {
    working
        .iter()
        .flat_map(|(session, folders)| folders.iter().map(move |folder| (session, folder)))
        .filter(|(_, folder)| path.starts_with(folder))
        .max_by_key(|(_, folder)| folder.components().count())
        .map(|(session, _)| *session)
}

fn version_of(meta: &std::fs::Metadata) -> FileVersion {
    use std::os::unix::fs::MetadataExt;
    FileVersion {
        mtime_ns: meta
            .mtime()
            .saturating_mul(1_000_000_000)
            .saturating_add(meta.mtime_nsec()),
        size: meta.size(),
        ino: meta.ino(),
    }
}

/// `path` made canonical as far as it exists: the deepest existing ancestor is resolved and the
/// rest appended as written.
pub fn canonical_lenient(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut rest = Vec::new();
    let mut current = path;
    while let (Some(parent), Some(name)) = (current.parent(), current.file_name()) {
        rest.push(name.to_owned());
        if let Ok(mut canonical) = parent.canonicalize() {
            canonical.extend(rest.iter().rev());
            return canonical;
        }
        current = parent;
    }
    path.to_owned()
}

// One watcher's task ---------------------------------------------------------------------------

async fn run_root(
    hub: Arc<WatchHub>,
    key: RootKey,
    mut raws: mpsc::Receiver<Vec<Raw>>,
    lost: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    stop: CancellationToken,
    births: Arc<Mutex<Births>>,
) {
    let mut window = Window::new(hub.changes_max);
    let mut flush_at: Option<Instant> = None;
    let mut generation_at: Option<Instant> = None;
    let mut generation_sent: Option<Instant> = None;
    loop {
        let wake = [flush_at, generation_at].into_iter().flatten().min();
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            () = sleep_until(wake.unwrap_or_else(Instant::now)), if wake.is_some() => {
                let now = Instant::now();
                if flush_at.is_some_and(|at| at <= now) {
                    flush_at = None;
                    let taken = std::mem::replace(&mut window, Window::new(hub.changes_max));
                    let overflow = taken.overflow | lost.swap(false, Ordering::AcqRel);
                    if flush(&hub, &key, taken, overflow, &births).await {
                        generation.fetch_add(1, Ordering::AcqRel);
                        let earliest = generation_sent.map_or(now, |sent| {
                            sent + Duration::from_millis(GENERATION_INTERVAL_MS)
                        });
                        generation_at.get_or_insert(earliest.max(now));
                    }
                }
                if generation_at.is_some_and(|at| at <= now) {
                    generation_at = None;
                    generation_sent = Some(now);
                    hub.send_generation(&key, generation.load(Ordering::Acquire));
                }
            }
            batch = raws.recv() => {
                let Some(batch) = batch else { break };
                window.add(&key, batch);
                if flush_at.is_none() && (!window.is_empty() || lost.load(Ordering::Acquire)) {
                    flush_at = Some(Instant::now() + Duration::from_millis(COALESCE_MS));
                }
            }
        }
    }
}

/// Looks at the window's paths on disk and delivers them. `true` when something was reported.
async fn flush(
    hub: &Arc<WatchHub>,
    key: &RootKey,
    window: Window,
    overflow: bool,
    births: &Arc<Mutex<Births>>,
) -> bool {
    let mut paths: Vec<(PathBuf, Entry)> = window.entries.into_iter().collect();
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    let mut overflow = overflow;
    if paths.len() > hub.changes_max {
        paths.truncate(hub.changes_max);
        overflow = true;
    }
    if paths.is_empty() && !overflow {
        return false;
    }
    let sessions = hub
        .registry
        .list_sessions(SessionListFilter::default())
        .await
        .map(|list| list.sessions)
        .unwrap_or_default();
    let evidence = Evidence {
        writes: hub.writes.lock().iter().cloned().collect(),
        restores: hub.restores.lock().iter().cloned().collect(),
        notes: hub.notes.lock().iter().cloned().collect(),
        sessions,
    };
    let births = births.clone();
    let prepared =
        match tokio::task::spawn_blocking(move || prepare(paths, evidence, &mut births.lock()))
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                warn!(%error, "looking at changed files failed");
                return false;
            }
        };
    let any = !prepared.is_empty() || overflow;
    for change in &prepared {
        debug!(root = %key.path.display(), path = %change.change.path.display(), kind = ?change.change.kind, version = ?change.change.version, attribution = ?change.attribution, "change");
    }
    if any {
        hub.deliver(key, &prepared, overflow);
    }
    any
}

#[cfg(test)]
mod tests {
    use hutan_protocol::fs::CHANGES_MAX;

    use super::*;

    fn key(path: &str) -> RootKey {
        RootKey {
            path: PathBuf::from(path),
            recursive: true,
        }
    }

    #[test]
    fn noise_and_temporary_files_never_reach_the_window() {
        let key = key("/repo");
        let mut window = Window::new(CHANGES_MAX);
        window.add(
            &key,
            vec![
                Raw::Modified("/repo/src/a.rs".into()),
                Raw::Modified("/repo/target/debug/x".into()),
                Raw::Modified("/repo/node_modules/y/z.js".into()),
                Raw::Modified("/repo/.git/index".into()),
                Raw::Created("/repo/src/.hutan-write-01.tmp".into()),
                Raw::Modified("/elsewhere/a.rs".into()),
                Raw::Modified("/repo/.gitignore".into()),
            ],
        );
        let mut seen: Vec<_> = window.entries.keys().cloned().collect();
        seen.sort();
        assert_eq!(
            seen,
            [
                PathBuf::from("/repo/.gitignore"),
                PathBuf::from("/repo/src/a.rs")
            ]
        );
    }

    #[test]
    fn a_window_is_bounded_and_says_when_it_lost_paths() {
        let key = key("/repo");
        let mut window = Window::new(CHANGES_MAX);
        let raws = (0..2 * CHANGES_MAX + 50)
            .map(|i| Raw::Created(PathBuf::from(format!("/repo/f{i}"))))
            .collect();
        window.add(&key, raws);
        assert_eq!(window.entries.len(), 2 * CHANGES_MAX);
        assert!(window.overflow);
        // A path already in the window is still merged when the window is full.
        window.add(
            &key,
            vec![Raw::Renamed {
                from: "/repo/f0".into(),
                to: "/repo/f1".into(),
            }],
        );
        assert_eq!(
            window.entries[Path::new("/repo/f1")].from.as_deref(),
            Some(Path::new("/repo/f0"))
        );
    }

    #[test]
    fn repeated_events_of_one_path_are_one_change_and_renames_name_both_paths() {
        let key = key("/repo");
        let mut window = Window::new(CHANGES_MAX);
        window.add(
            &key,
            (0..100)
                .map(|_| Raw::Modified("/repo/a.rs".into()))
                .collect(),
        );
        assert_eq!(window.entries.len(), 1, "a hundred writes are one change");
        window.add(
            &key,
            vec![
                Raw::Created("/repo/a.rs".into()),
                Raw::Renamed {
                    from: "/repo/old.rs".into(),
                    to: "/repo/new.rs".into(),
                },
            ],
        );
        assert!(window.entries[Path::new("/repo/a.rs")].created);
        assert_eq!(
            window.entries[Path::new("/repo/new.rs")].from.as_deref(),
            Some(Path::new("/repo/old.rs"))
        );
        assert!(window.entries.contains_key(Path::new("/repo/old.rs")));
    }

    #[test]
    fn platform_events_become_raw_events_and_reads_are_not_changes() {
        use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind, RenameMode};
        let event = |kind| notify::Event::new(kind).add_path("/repo/a.rs".into());
        assert!(raw_of(event(EventKind::Access(AccessKind::Any))).is_empty());
        assert!(matches!(
            raw_of(event(EventKind::Create(CreateKind::File))).as_slice(),
            [Raw::Created(_)]
        ));
        assert!(matches!(
            raw_of(event(EventKind::Remove(RemoveKind::File))).as_slice(),
            [Raw::Removed(_)]
        ));
        assert!(matches!(
            raw_of(event(EventKind::Modify(ModifyKind::Name(RenameMode::Any)))).as_slice(),
            [Raw::Name(_)]
        ));
        let both = notify::Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path("/repo/a.rs".into())
            .add_path("/repo/b.rs".into());
        assert!(matches!(raw_of(both).as_slice(), [Raw::Renamed { .. }]));
    }

    #[test]
    fn a_window_is_looked_at_on_disk_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("kept.rs"), "kept").unwrap();
        std::fs::write(root.join("old.rs"), "old").unwrap();
        std::fs::write(root.join("new.rs"), "new!").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        // The watcher started after `old.rs` was born, before `new.rs` was.
        let born = |name: &str| {
            std::fs::metadata(root.join(name))
                .unwrap()
                .created()
                .unwrap()
        };
        assert!(born("old.rs") < born("new.rs"));
        let mut births = Births::new(born("new.rs"));
        let entry = |created: bool| Entry {
            created,
            from: None,
        };
        let window = || {
            vec![
                (root.join("kept.rs"), entry(false)),
                // The platform's "created" of a file older than the watcher: modified.
                (root.join("old.rs"), entry(true)),
                (root.join("new.rs"), entry(true)),
                (root.join("gone.rs"), entry(false)),
                // Created and gone inside the window, or a file that was there before and
                // carries an old flag: told as removed either way.
                (root.join("flash.tmp"), entry(true)),
                (root.join("sub"), entry(true)),
            ]
        };
        let evidence = || Evidence {
            writes: Vec::new(),
            restores: Vec::new(),
            notes: Vec::new(),
            sessions: Vec::new(),
        };
        let seen = |prepared: Vec<Prepared>| -> Vec<(String, ChangeKind, Option<u64>)> {
            prepared
                .iter()
                .map(|p| {
                    (
                        p.change
                            .path
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                        p.change.kind,
                        p.change.version.map(|v| v.size),
                    )
                })
                .collect()
        };
        assert_eq!(
            seen(prepare(window(), evidence(), &mut births)),
            [
                ("kept.rs".to_owned(), ChangeKind::Modified, Some(4)),
                ("old.rs".to_owned(), ChangeKind::Modified, Some(3)),
                ("new.rs".to_owned(), ChangeKind::Created, Some(4)),
                ("gone.rs".to_owned(), ChangeKind::Removed, None),
                ("flash.tmp".to_owned(), ChangeKind::Removed, None),
                ("sub".to_owned(), ChangeKind::Created, None),
            ]
        );
        // The same flags again in a later window: those creations were reported already.
        let later = seen(prepare(window(), evidence(), &mut births));
        assert_eq!(
            later[2],
            ("new.rs".to_owned(), ChangeKind::Modified, Some(4))
        );
        assert_eq!(later[5], ("sub".to_owned(), ChangeKind::Modified, None));
        // Replaced inside one window (a save that renames over the file): modified...
        std::fs::remove_file(root.join("new.rs")).unwrap();
        std::fs::write(root.join("new.rs"), "again").unwrap();
        let replaced = seen(prepare(window(), evidence(), &mut births));
        assert_eq!(
            replaced[2],
            ("new.rs".to_owned(), ChangeKind::Modified, Some(5))
        );
        // ...gone in one window and born again in a later one: created.
        std::fs::remove_file(root.join("new.rs")).unwrap();
        let gone = seen(prepare(window(), evidence(), &mut births));
        assert_eq!(gone[2], ("new.rs".to_owned(), ChangeKind::Removed, None));
        std::fs::write(root.join("new.rs"), "born again").unwrap();
        let again = seen(prepare(window(), evidence(), &mut births));
        assert_eq!(
            again[2],
            ("new.rs".to_owned(), ChangeKind::Created, Some(10))
        );
    }

    #[test]
    fn interest_covers_the_file_the_entries_of_a_folder_or_a_subtree() {
        let spec = |recursive| Spec {
            recursive,
            root: key("/repo"),
        };
        let file = Path::new("/repo/a.rs");
        assert!(interested(file, &spec(false), Path::new("/repo/a.rs")));
        assert!(!interested(file, &spec(false), Path::new("/repo/b.rs")));
        let folder = Path::new("/repo/src");
        assert!(interested(
            folder,
            &spec(false),
            Path::new("/repo/src/a.rs")
        ));
        assert!(!interested(
            folder,
            &spec(false),
            Path::new("/repo/src/deep/a.rs")
        ));
        assert!(interested(
            folder,
            &spec(true),
            Path::new("/repo/src/deep/a.rs")
        ));
        assert!(!interested(
            folder,
            &spec(true),
            Path::new("/repo/srcx/a.rs")
        ));
    }

    fn change(path: &str, version: Option<FileVersion>) -> Prepared {
        Prepared {
            change: FsChange {
                path: path.into(),
                kind: ChangeKind::Modified,
                from: None,
                version,
                origin: None,
            },
            attribution: Attribution::External,
        }
    }

    #[test]
    fn a_write_is_its_own_connections_and_somebody_elses_to_everyone_else() {
        let version = FileVersion {
            mtime_ns: 1,
            size: 2,
            ino: 3,
        };
        let mut prepared = change("/repo/a.rs", Some(version));
        prepared.attribution = Attribution::Own {
            conn: ConnId(1),
            write_id: "w1".into(),
            agent: None,
            version_matches: true,
        };
        assert_eq!(
            prepared.for_connection(ConnId(1)).origin,
            Some(FsOrigin::Own {
                write_id: "w1".into()
            })
        );
        assert_eq!(
            prepared.for_connection(ConnId(2)).origin,
            Some(FsOrigin::External)
        );
        // Changed again after our write: not ours any more.
        if let Attribution::Own {
            version_matches, ..
        } = &mut prepared.attribution
        {
            *version_matches = false;
        }
        assert_eq!(
            prepared.for_connection(ConnId(1)).origin,
            Some(FsOrigin::External)
        );
        // A write of an agent's connection shows to the others as the agent's.
        let session = SessionId::new();
        prepared.attribution = Attribution::Own {
            conn: ConnId(1),
            write_id: "w1".into(),
            agent: Some(session),
            version_matches: true,
        };
        assert_eq!(
            prepared.for_connection(ConnId(2)).origin,
            Some(FsOrigin::Agent {
                session_id: session,
                tool_call_id: None,
                confidence: AgentConfidence::Session
            })
        );
    }

    #[test]
    fn the_deepest_working_folder_claims_a_change() {
        let outer = SessionId::new();
        let inner = SessionId::new();
        let working = vec![
            (outer, vec![PathBuf::from("/repo")]),
            (inner, vec![PathBuf::from("/repo/crates/a")]),
        ];
        assert_eq!(
            working_in(&working, Path::new("/repo/crates/a/src/x.rs")),
            Some(inner)
        );
        assert_eq!(
            working_in(&working, Path::new("/repo/README.md")),
            Some(outer)
        );
        assert_eq!(working_in(&working, Path::new("/other/x")), None);
    }

    #[test]
    fn a_path_is_made_canonical_as_far_as_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(
            canonical_lenient(&dir.path().join("not/yet/there.rs")),
            root.join("not/yet/there.rs")
        );
    }
}
