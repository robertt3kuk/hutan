//! The resource manager (protocol 0.18, capability `resources`, `docs/resources.md`): process
//! trees per live session with CPU and memory, sampled only while somebody watches, and the
//! actions that stop things — `session.kill`, `process.signal`, `agent.interrupt`. Every action
//! is audited (target `hutan::audit` in `hutand.log`: who, what, result).
//!
//! The process table is read through [`ProcessSource`] so the assembly ([`collect`]) and the
//! refusal rules ([`verify_descendant`]) are tested on injected samples; [`SystemProcesses`] is
//! the real one (`libproc`, `hutan_terminal::procs`).
//!
//! What may be signalled is decided here, from a fresh look at the process table: a pid that is
//! the session's own process or one of its descendants. Never `hutand`, never pid 1, never a
//! process outside the sessions.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use hutan_core::{AgentNode, NodeKind, SessionId};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{ClientKind, Event, SessionInfo};
use hutan_protocol::resources::{
    AgentInterrupt, AgentInterrupted, AgentProcess, DEFAULT_INTERVAL_MS, InterruptVia,
    MAX_COMMAND_CHARS, MAX_PROCESSES_PER_SESSION, MAX_PROCESSES_TOTAL, MIN_INTERVAL_MS,
    ProcessInfo, ProcessSignal, ProcessSignaled, ProcessState, ResourceSample, ResourceTotals,
    ResourcesSubscribed, SessionResources, SignalName,
};
use hutan_protocol::{ControlMessage, Frame, MAX_PAYLOAD};
use hutan_terminal::procs::{self, ProcSignal, ProcState, ProcessSnapshot};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::time::{Instant, sleep};
use tracing::{debug, info, warn};

use crate::bus::EventBus;
use crate::chat::ThreadInterruptError;
use crate::conn::{ConnHandle, ConnId};
use crate::registry::{ProcessTarget, RegistryHandle, now_ms};
use crate::terminals::TerminalHub;

/// A CPU figure needs at least this much wall time between its two readings; a shorter window
/// reports the figure of the previous one.
const MIN_CPU_WINDOW: Duration = Duration::from_millis(200);
/// How far apart the two readings of a `resources.list` without subscribers are.
const PRIME_WAIT: Duration = Duration::from_millis(250);
/// A subscriber's last sample is a usable CPU baseline for `resources.list` this long.
const BASELINE_MAX_AGE: Duration = Duration::from_secs(30);
/// How long `session.kill` waits for the session to end.
const KILL_WAIT: Duration = Duration::from_secs(10);
/// How long a process gets to leave after SIGINT before `agent.interrupt --force` sends SIGTERM.
pub const FORCE_GRACE: Duration = Duration::from_secs(2);
const FORCE_POLL: Duration = Duration::from_millis(50);
/// ^C, what a terminal agent cancels its turn on.
const CTRL_C: u8 = 0x03;
/// Longest `command` kept when a sample is too big for a frame.
const SHORT_COMMAND_CHARS: usize = 128;

// The process table ---------------------------------------------------------------------------

/// Where processes are read from.
pub trait ProcessSource: Send + Sync + fmt::Debug + 'static {
    /// Every process below `root`, parents before children, at most `limit` of them.
    fn descendants(&self, root: u32, limit: usize) -> Vec<u32>;
    fn inspect(&self, pid: u32) -> Option<ProcessSnapshot>;
    fn arguments(&self, pid: u32) -> Option<Vec<String>>;
}

/// The machine's process table.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcesses;

impl ProcessSource for SystemProcesses {
    fn descendants(&self, root: u32, limit: usize) -> Vec<u32> {
        procs::descendants(root, limit)
    }

    fn inspect(&self, pid: u32) -> Option<ProcessSnapshot> {
        procs::inspect(pid)
    }

    fn arguments(&self, pid: u32) -> Option<Vec<String>> {
        procs::arguments(pid)
    }
}

/// SIGKILLs everything below `root`, leaves first (so a parent cannot start more while its
/// children go), then `root` itself. Blocking: a burst of system calls.
pub(crate) fn kill_tree(root: u32) {
    let mut pids = procs::descendants(root, MAX_PROCESSES_TOTAL);
    pids.reverse();
    pids.push(root);
    for pid in pids {
        // A process that is already gone is what was asked for.
        if let Err(error) = procs::signal(pid, ProcSignal::Kill) {
            debug!(%error, pid, "could not kill a process of a session");
        }
    }
}

// Redaction -----------------------------------------------------------------------------------

/// Name fragments that make a flag or variable look secret (`--token`, `API_KEY`).
const SECRET_FRAGMENTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "api-key",
    "api_key",
    "apikey",
    "private-key",
    "private_key",
    "access-key",
    "access_key",
];

/// `--token` (no value attached): the next argument is the secret.
fn is_secret_flag(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    lower.starts_with('-')
        && !lower.contains('=')
        && SECRET_FRAGMENTS.iter().any(|f| lower.contains(f))
        // Counts and limits are not secrets (`--max-tokens 100`).
        && !lower.ends_with("tokens")
        && !lower.ends_with("count")
        && !lower.ends_with("limit")
}

/// The command line as shown: arguments joined by spaces (an argument with spaces or quotes is
/// single-quoted), secret-looking ones replaced like the history index does
/// ([`hutan_index::redact::redact`]), a value after `--token`-like flag too, cut to
/// [`MAX_COMMAND_CHARS`] characters. Redaction comes before the cut, so a secret cannot survive
/// as its first characters.
pub fn command_line(args: &[String]) -> String {
    let mut shown = Vec::with_capacity(args.len());
    let mut after_secret_flag = false;
    for arg in args {
        let text = if after_secret_flag {
            hutan_index::redact::REDACTED.to_owned()
        } else {
            hutan_index::redact::redact(arg).into_owned()
        };
        after_secret_flag = is_secret_flag(arg);
        shown.push(
            if text.is_empty() || text.contains([' ', '\t', '\'', '"', '\n']) {
                format!("'{}'", text.replace('\'', "'\\''"))
            } else {
                text
            },
        );
    }
    let joined = shown.join(" ");
    // A secret split across arguments (`Bearer` then the token) is still caught on the whole.
    clipped(&hutan_index::redact::redact(&joined), MAX_COMMAND_CHARS)
}

fn clipped(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}

// CPU baseline --------------------------------------------------------------------------------

/// What one process used when it was last looked at.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mark {
    /// Start time: with the pid it tells a reused pid from the process it was.
    started_at_ms: u64,
    cpu_time_ns: u64,
    at: Instant,
    percent: Option<f64>,
}

/// The previous reading of every process of the last full sample.
#[derive(Debug, Clone, Default)]
pub struct Marks {
    by_pid: HashMap<u32, Mark>,
    at: Option<Instant>,
}

impl Marks {
    fn is_empty(&self) -> bool {
        self.by_pid.is_empty()
    }

    fn fresh(&self, now: Instant) -> bool {
        self.at
            .is_some_and(|at| now.saturating_duration_since(at) <= BASELINE_MAX_AGE)
    }

    /// CPU share of `snapshot` since its mark, and the mark to keep.
    fn cpu(&self, snapshot: &ProcessSnapshot, now: Instant) -> (Option<f64>, Mark) {
        let fresh = Mark {
            started_at_ms: snapshot.started_at_ms,
            cpu_time_ns: snapshot.cpu_time_ns,
            at: now,
            percent: None,
        };
        let Some(old) = self
            .by_pid
            .get(&snapshot.pid)
            .filter(|old| old.started_at_ms == snapshot.started_at_ms)
        else {
            return (None, fresh);
        };
        let window = now.saturating_duration_since(old.at);
        if window < MIN_CPU_WINDOW {
            // Too short to mean anything: keep the old reading and its figure.
            return (old.percent, *old);
        }
        let used = snapshot.cpu_time_ns.saturating_sub(old.cpu_time_ns);
        let percent = used as f64 / window.as_nanos() as f64 * 100.0;
        (
            Some(percent),
            Mark {
                percent: Some(percent),
                ..fresh
            },
        )
    }
}

// Assembly ------------------------------------------------------------------------------------

fn state_of(state: ProcState) -> ProcessState {
    match state {
        ProcState::Running => ProcessState::Running,
        ProcState::Sleeping => ProcessState::Sleeping,
        ProcState::Stopped => ProcessState::Stopped,
        ProcState::Zombie => ProcessState::Zombie,
        ProcState::Idle => ProcessState::Idle,
        ProcState::Unknown => ProcessState::Unknown,
    }
}

fn info_of(
    source: &dyn ProcessSource,
    snapshot: &ProcessSnapshot,
    marks: &Marks,
    now: Instant,
    next: &mut HashMap<u32, Mark>,
) -> ProcessInfo {
    let (cpu_percent, mark) = marks.cpu(snapshot, now);
    next.insert(snapshot.pid, mark);
    let command = source
        .arguments(snapshot.pid)
        .filter(|args| !args.is_empty())
        .map_or_else(|| snapshot.name.clone(), |args| command_line(&args));
    ProcessInfo {
        pid: snapshot.pid,
        ppid: snapshot.ppid,
        name: snapshot.name.clone(),
        command,
        state: state_of(snapshot.state),
        cpu_percent,
        rss_bytes: snapshot.rss_bytes,
        threads: snapshot.threads,
        started_at_ms: (snapshot.started_at_ms > 0).then_some(snapshot.started_at_ms),
        agent_node: None,
    }
}

/// `root` and what is below it, depth first: a parent before its children, siblings by pid. A
/// process whose parent is not in the set (it exited while the table was read) follows the
/// others as a root of its own.
fn tree_order(root: u32, mut processes: Vec<ProcessSnapshot>) -> Vec<ProcessSnapshot> {
    let pids: HashSet<u32> = processes.iter().map(|p| p.pid).collect();
    processes.sort_by_key(|p| p.pid);
    let mut children: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    let mut tops = Vec::new();
    for (index, process) in processes.iter().enumerate() {
        if process.pid == root || !pids.contains(&process.ppid) || process.ppid == process.pid {
            tops.push(index);
        } else {
            children.entry(process.ppid).or_default().push(index);
        }
    }
    // The session's own process first, strays after it.
    tops.sort_by_key(|index| (processes[*index].pid != root, processes[*index].pid));
    let mut order = Vec::with_capacity(processes.len());
    let mut seen = HashSet::new();
    let mut stack: Vec<usize> = tops.into_iter().rev().collect();
    while let Some(index) = stack.pop() {
        if !seen.insert(index) {
            continue;
        }
        order.push(index);
        if let Some(kids) = children.get(&processes[index].pid) {
            stack.extend(kids.iter().rev());
        }
    }
    let mut slots: Vec<Option<ProcessSnapshot>> = processes.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|index| slots[index].take())
        .collect()
}

/// One session: its process and everything below it, at most `limit` processes. `None` when the
/// session has no process.
fn session_resources(
    source: &dyn ProcessSource,
    target: &ProcessTarget,
    limit: usize,
    marks: &Marks,
    now: Instant,
    next: &mut HashMap<u32, Mark>,
) -> Option<(SessionResources, bool)> {
    let root = target.root_pid?;
    let mut snapshots: Vec<ProcessSnapshot> = Vec::new();
    // `limit` slots in all, one of them the root.
    let below = limit.saturating_sub(1);
    let found = source.descendants(root, below.saturating_add(1));
    let cut = found.len() > below;
    snapshots.extend(source.inspect(root));
    snapshots.extend(
        found
            .into_iter()
            .take(below)
            .filter_map(|pid| source.inspect(pid)),
    );
    let mut processes: Vec<ProcessInfo> = tree_order(root, snapshots)
        .iter()
        .map(|snapshot| info_of(source, snapshot, marks, now, next))
        .collect();
    let mut agents = Vec::new();
    let mut nodes: Vec<&AgentNode> = target.agents.iter().collect();
    // The main agent first, then by start; the tree's own order is by id.
    nodes.sort_by_key(|node| {
        (
            node.kind != NodeKind::Main,
            node.started_at_ms,
            node.node_id.clone(),
        )
    });
    for node in nodes {
        // A pid the process tree does not contain is not shown: it may be anyone's now.
        let pid = node
            .pid
            .filter(|pid| processes.iter().any(|p| p.pid == *pid));
        if let Some(pid) = pid
            && let Some(process) = processes
                .iter_mut()
                .find(|p| p.pid == pid && p.agent_node.is_none())
        {
            process.agent_node = Some(node.node_id.clone());
        }
        agents.push(AgentProcess {
            node_id: node.node_id.clone(),
            parent: node.parent.clone(),
            kind: node.kind,
            label: node.label().to_owned(),
            state: node.state,
            pid,
        });
    }
    let totals = ResourceTotals {
        processes: u32::try_from(processes.len()).unwrap_or(u32::MAX),
        cpu_percent: processes.iter().filter_map(|p| p.cpu_percent).sum(),
        rss_bytes: processes.iter().map(|p| p.rss_bytes).sum(),
    };
    Some((
        SessionResources {
            session_id: target.session_id,
            processes,
            totals,
            agents,
            truncated: cut,
        },
        cut,
    ))
}

/// One sample of every target and of `daemon_pid`, from `source`. `marks` is the previous full
/// sample; the new marks (of the processes seen now) come back with the sample. At most
/// [`MAX_PROCESSES_PER_SESSION`] processes per session and [`MAX_PROCESSES_TOTAL`] in all.
pub fn collect(
    source: &dyn ProcessSource,
    targets: &[ProcessTarget],
    daemon_pid: u32,
    marks: &Marks,
    now: Instant,
    at_ms: u64,
) -> (ResourceSample, Marks) {
    let mut next = HashMap::new();
    let mut remaining = MAX_PROCESSES_TOTAL;
    let mut truncated = false;
    let mut sessions = Vec::new();
    for target in targets {
        if target.root_pid.is_none() {
            continue;
        }
        let limit = MAX_PROCESSES_PER_SESSION.min(remaining);
        if limit == 0 {
            truncated = true;
            sessions.push(SessionResources {
                session_id: target.session_id,
                processes: Vec::new(),
                totals: ResourceTotals::default(),
                agents: Vec::new(),
                truncated: true,
            });
            continue;
        }
        let Some((resources, cut)) =
            session_resources(source, target, limit, marks, now, &mut next)
        else {
            continue;
        };
        // Cut by the total, not by the per-session limit.
        truncated |= cut && limit < MAX_PROCESSES_PER_SESSION;
        remaining = remaining.saturating_sub(resources.processes.len());
        sessions.push(resources);
    }
    let daemon = source
        .inspect(daemon_pid)
        .map(|snapshot| info_of(source, &snapshot, marks, now, &mut next))
        .unwrap_or_else(|| ProcessInfo {
            pid: daemon_pid,
            ppid: 0,
            name: "hutand".to_owned(),
            command: "hutand".to_owned(),
            state: ProcessState::Unknown,
            cpu_percent: None,
            rss_bytes: 0,
            threads: 0,
            started_at_ms: None,
            agent_node: None,
        });
    (
        ResourceSample {
            at_ms,
            sessions,
            daemon,
            truncated,
        },
        Marks {
            by_pid: next,
            at: Some(now),
        },
    )
}

// Refusal -------------------------------------------------------------------------------------

/// Why a pid may not be signalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Pid 0 or 1.
    Special,
    /// `hutand` itself.
    Daemon,
    /// Not the session's process or one of its descendants.
    Outside,
}

/// Checks `pid` against a fresh look at the process table: it must be the session's own process
/// (`root`) or one of its descendants; never `daemon_pid`, never pid 1.
pub fn verify_descendant(
    source: &dyn ProcessSource,
    root: u32,
    daemon_pid: u32,
    pid: u32,
) -> Result<(), Refusal> {
    if pid <= 1 {
        return Err(Refusal::Special);
    }
    if pid == daemon_pid {
        return Err(Refusal::Daemon);
    }
    if pid == root {
        return Ok(());
    }
    if source.descendants(root, MAX_PROCESSES_TOTAL).contains(&pid) {
        Ok(())
    } else {
        Err(Refusal::Outside)
    }
}

fn refusal_error(refusal: Refusal, session: SessionId, pid: u32) -> ErrorBody {
    let message = match refusal {
        Refusal::Special => format!("pid {pid} is not a process of a Hutan session"),
        Refusal::Daemon => "hutand itself cannot be signalled through a session".to_owned(),
        Refusal::Outside => {
            format!("pid {pid} is not the process of session {session} or one of its descendants")
        }
    };
    ErrorBody::new(ErrorCode::Forbidden, message)
}

fn proc_signal(signal: SignalName) -> ProcSignal {
    match signal {
        SignalName::Term => ProcSignal::Term,
        SignalName::Kill => ProcSignal::Kill,
        SignalName::Int => ProcSignal::Int,
        SignalName::Hup => ProcSignal::Hup,
        SignalName::Stop => ProcSignal::Stop,
        SignalName::Cont => ProcSignal::Cont,
    }
}

// Who and audit -------------------------------------------------------------------------------

/// Who asks, for the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    pub kind: Option<ClientKind>,
    pub conn: ConnId,
}

impl Actor {
    pub fn of(conn: &ConnHandle) -> Self {
        Self {
            kind: conn.client_kind(),
            conn: conn.id(),
        }
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            Some(kind) => write!(f, "{kind:?}#{}", self.conn),
            None => write!(f, "client#{}", self.conn),
        }
    }
}

/// One line of the audit trail: who did what, and how it ended.
pub fn audit<T>(actor: Actor, action: &str, what: &str, outcome: &Result<T, ErrorBody>) {
    match outcome {
        Ok(_) => info!(
            target: "hutan::audit",
            who = %actor, action, what, result = "ok",
            "{action} {what}: ok"
        ),
        Err(error) => info!(
            target: "hutan::audit",
            who = %actor, action, what, result = ?error.code, reason = %error.message,
            "{action} {what}: {:?}: {}", error.code, error.message
        ),
    }
}

fn invalid_state(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidState, message)
}

// The service ---------------------------------------------------------------------------------

#[derive(Debug)]
struct Subscriber {
    conn: ConnHandle,
    interval: Duration,
    next_due: Instant,
}

#[derive(Debug, Default)]
struct State {
    subscribers: HashMap<ConnId, Subscriber>,
    /// The sampler task is running.
    sampling: bool,
}

/// Samples and acts on the processes of the live sessions.
#[derive(Debug)]
pub struct ResourceService {
    registry: RegistryHandle,
    hub: Arc<TerminalHub>,
    bus: Arc<EventBus>,
    source: Arc<dyn ProcessSource>,
    daemon_pid: u32,
    state: Mutex<State>,
    /// The previous full sample; held for the whole of one sample so they never overlap.
    marks: AsyncMutex<Marks>,
    /// Wakes the sampler when a subscriber leaves, so it stops at once instead of at its next
    /// due time.
    changed: Notify,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded data is never left half-updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ResourceService {
    pub fn new(
        registry: RegistryHandle,
        hub: Arc<TerminalHub>,
        bus: Arc<EventBus>,
        source: Arc<dyn ProcessSource>,
    ) -> Arc<Self> {
        Arc::new(Self {
            registry,
            hub,
            bus,
            source,
            daemon_pid: std::process::id(),
            state: Mutex::new(State::default()),
            marks: AsyncMutex::new(Marks::default()),
            changed: Notify::new(),
        })
    }

    // Sampling --------------------------------------------------------------------------------

    /// Reads the process table once, off the async threads.
    async fn read(
        &self,
        targets: Vec<ProcessTarget>,
        marks: Marks,
    ) -> Result<(ResourceSample, Marks), ErrorBody> {
        let source = self.source.clone();
        let daemon_pid = self.daemon_pid;
        tokio::task::spawn_blocking(move || {
            collect(
                &*source,
                &targets,
                daemon_pid,
                &marks,
                Instant::now(),
                now_ms(),
            )
        })
        .await
        .map_err(|error| {
            warn!(%error, "reading the process table failed");
            ErrorBody::new(ErrorCode::Internal, "reading the process table failed")
        })
    }

    /// A full sample against the stored baseline, which it replaces.
    async fn sample_and_store(&self) -> Result<ResourceSample, ErrorBody> {
        let mut marks = self.marks.lock().await;
        let targets = self.registry.process_targets(None).await?;
        let (sample, next) = self.read(targets, std::mem::take(&mut *marks)).await?;
        *marks = next;
        Ok(sample)
    }

    /// `resources.list`: one fresh sample, of one session or of all. CPU figures compare with
    /// the sampler's last reading while somebody watches; otherwise the table is read twice,
    /// [`PRIME_WAIT`] apart. Nothing is kept: sampling stays tied to subscribers.
    pub async fn list(&self, session: Option<SessionId>) -> Result<ResourceSample, ErrorBody> {
        let targets = self.registry.process_targets(session).await?;
        let baseline = {
            let marks = self.marks.lock().await;
            (!marks.is_empty() && marks.fresh(Instant::now())).then(|| marks.clone())
        };
        let baseline = match baseline {
            Some(baseline) => baseline,
            None => {
                let (_, primed) = self.read(targets.clone(), Marks::default()).await?;
                sleep(PRIME_WAIT).await;
                primed
            }
        };
        Ok(self.read(targets, baseline).await?.0)
    }

    /// `resources.subscribe`: starts sampling when this is the first subscriber, and answers with
    /// a first sample (without CPU figures when nobody sampled before).
    pub async fn subscribe(
        self: &Arc<Self>,
        conn: &ConnHandle,
        interval_ms: Option<u32>,
    ) -> Result<ResourcesSubscribed, ErrorBody> {
        let interval_ms = interval_ms.unwrap_or(DEFAULT_INTERVAL_MS);
        if interval_ms < MIN_INTERVAL_MS {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("`interval_ms` must be at least {MIN_INTERVAL_MS}"),
            ));
        }
        let interval = Duration::from_millis(u64::from(interval_ms));
        {
            let mut state = lock(&self.state);
            state.subscribers.insert(
                conn.id(),
                Subscriber {
                    conn: conn.clone(),
                    interval,
                    next_due: Instant::now() + interval,
                },
            );
            if !state.sampling {
                state.sampling = true;
                tokio::spawn(self.clone().run());
            }
        }
        self.changed.notify_one();
        match self.sample_and_store().await {
            Ok(sample) => Ok(ResourcesSubscribed {
                interval_ms,
                sample,
            }),
            Err(error) => {
                self.unsubscribe(conn.id());
                Err(error)
            }
        }
    }

    /// `resources.unsubscribe`, and a closing connection. Sampling ends with the last one.
    pub fn unsubscribe(&self, conn: ConnId) {
        lock(&self.state).subscribers.remove(&conn);
        self.changed.notify_one();
    }

    /// Whether the sampler runs (somebody watches).
    pub fn sampling(&self) -> bool {
        lock(&self.state).sampling
    }

    /// The sampler: wakes at the shortest subscriber interval, samples when a subscriber is due
    /// and sends it the sample. Ends — and forgets the baseline — when the last subscriber is
    /// gone.
    async fn run(self: Arc<Self>) {
        loop {
            let wake = {
                let mut state = lock(&self.state);
                let Some(wake) = state.subscribers.values().map(|s| s.next_due).min() else {
                    state.sampling = false;
                    break;
                };
                wake
            };
            tokio::select! {
                () = tokio::time::sleep_until(wake) => {}
                // A subscriber left (or joined with a shorter interval): look again.
                () = self.changed.notified() => continue,
            }
            let now = Instant::now();
            let due: Vec<ConnHandle> = {
                let mut state = lock(&self.state);
                state
                    .subscribers
                    .values_mut()
                    .filter(|s| s.next_due <= now)
                    .map(|s| {
                        s.next_due = now + s.interval;
                        s.conn.clone()
                    })
                    .collect()
            };
            if due.is_empty() {
                continue;
            }
            match self.sample_and_store().await {
                Ok(sample) => self.send(&due, sample),
                Err(error) => debug!(message = %error.message, "sampling failed"),
            }
        }
        // Nobody watches: nothing is compared with a reading from before. (A subscriber that
        // arrived meanwhile started a sampler of its own, and its first sample is stored
        // after this at worst: one CPU figure lost.)
        *self.marks.lock().await = Marks::default();
    }

    /// Queues the sample to `to`; a sample that would not fit a frame is sent with short
    /// command lines instead.
    fn send(&self, to: &[ConnHandle], mut sample: ResourceSample) {
        let epoch = self.bus.epoch();
        // Outside the replay ring: the seq of the last retained event, no new one.
        let seq = self.bus.last_seq();
        let mut frame = encode(epoch, seq, &sample);
        if frame
            .as_ref()
            .is_none_or(|frame| frame.payload.len() > MAX_PAYLOAD)
        {
            for session in &mut sample.sessions {
                for process in &mut session.processes {
                    process.command = clipped(&process.command, SHORT_COMMAND_CHARS);
                }
            }
            frame = encode(epoch, seq, &sample).filter(|frame| frame.payload.len() <= MAX_PAYLOAD);
        }
        let Some(frame) = frame else {
            warn!("a resource sample does not fit a protocol frame; skipped");
            return;
        };
        for conn in to {
            if !conn.enqueue_lossy(frame.clone()) {
                self.unsubscribe(conn.id());
            }
        }
    }

    // Actions ---------------------------------------------------------------------------------

    /// `session.kill`: SIGKILL for the whole process tree of the session now; answers once the
    /// session has ended.
    pub async fn kill_session(
        &self,
        actor: Actor,
        session: SessionId,
    ) -> Result<SessionInfo, ErrorBody> {
        let outcome = self.kill_inner(session).await;
        audit(actor, "session.kill", &session.to_string(), &outcome);
        outcome
    }

    async fn kill_inner(&self, session: SessionId) -> Result<SessionInfo, ErrorBody> {
        let target = self.live_target(session).await?;
        if !target.chat
            && let Some(root) = target.root_pid
        {
            // A shell with job control puts its jobs in groups of their own, which the group
            // kill below does not reach: take the tree first, leaves before parents.
            let _ = tokio::task::spawn_blocking(move || kill_tree(root)).await;
        }
        match tokio::time::timeout(KILL_WAIT, self.registry.kill_session(session)).await {
            Ok(result) => result,
            Err(_) => Err(ErrorBody::new(
                ErrorCode::Internal,
                format!(
                    "session {session} did not end within {}s of SIGKILL",
                    KILL_WAIT.as_secs()
                ),
            )),
        }
    }

    /// `process.signal`: signals one process of the session; see [`verify_descendant`].
    pub async fn signal(
        &self,
        actor: Actor,
        req: ProcessSignal,
    ) -> Result<ProcessSignaled, ErrorBody> {
        let what = format!(
            "{} pid {} of session {}",
            req.signal.as_str(),
            req.pid,
            req.session_id
        );
        let outcome = self.signal_inner(req).await;
        audit(actor, "process.signal", &what, &outcome);
        outcome
    }

    async fn signal_inner(&self, req: ProcessSignal) -> Result<ProcessSignaled, ErrorBody> {
        let target = self.live_target(req.session_id).await?;
        let root = target
            .root_pid
            .ok_or_else(|| invalid_state(format!("session {} has no process", req.session_id)))?;
        self.send_signal(req.session_id, root, req.pid, req.signal)
            .await?;
        Ok(ProcessSignaled {
            session_id: req.session_id,
            pid: req.pid,
            signal: req.signal,
        })
    }

    /// Verifies `pid` against the session's tree, then signals it.
    async fn send_signal(
        &self,
        session: SessionId,
        root: u32,
        pid: u32,
        signal: SignalName,
    ) -> Result<(), ErrorBody> {
        let source = self.source.clone();
        let daemon_pid = self.daemon_pid;
        tokio::task::spawn_blocking(move || {
            verify_descendant(&*source, root, daemon_pid, pid)
                .map_err(|refusal| refusal_error(refusal, session, pid))?;
            procs::signal(pid, proc_signal(signal)).map_err(|error| {
                if error.raw_os_error() == Some(libc::ESRCH) {
                    ErrorBody::new(ErrorCode::NotFound, format!("process {pid} is gone"))
                } else {
                    ErrorBody::new(
                        ErrorCode::Internal,
                        format!("could not signal process {pid}: {error}"),
                    )
                }
            })
        })
        .await
        .map_err(|_| ErrorBody::new(ErrorCode::Internal, "signalling failed"))?
    }

    /// `agent.interrupt`: see [`AgentInterrupt`].
    pub async fn interrupt(
        &self,
        actor: Actor,
        req: AgentInterrupt,
    ) -> Result<AgentInterrupted, ErrorBody> {
        let what = match &req.node_id {
            Some(node) => format!("node {node} of session {}", req.session_id),
            None => format!("main agent of session {}", req.session_id),
        };
        let outcome = self.interrupt_inner(req).await;
        audit(actor, "agent.interrupt", &what, &outcome);
        outcome
    }

    async fn interrupt_inner(&self, req: AgentInterrupt) -> Result<AgentInterrupted, ErrorBody> {
        let session = req.session_id;
        let target = self.live_target(session).await?;
        let done = |via, pid| AgentInterrupted {
            session_id: session,
            node_id: req.node_id.clone(),
            via,
            pid,
        };
        let node = match &req.node_id {
            Some(id) => {
                let node = target
                    .agents
                    .iter()
                    .find(|node| node.node_id == *id)
                    .cloned();
                match node {
                    Some(node) => Some(node),
                    None => return Err(self.missing_node(session, id).await),
                }
            }
            None => None,
        };
        let is_main = node
            .as_ref()
            .is_none_or(|node| target.root_node.as_deref() == Some(node.node_id.as_str()));
        if is_main {
            if target.chat {
                // A Stop: the queue goes too, as for `chat.interrupt`.
                self.registry.chats().interrupt(session, false).await?;
                return Ok(done(InterruptVia::Turn, None));
            }
            let terminal = target.terminal.ok_or_else(|| {
                invalid_state(format!("session {session} has no terminal to interrupt"))
            })?;
            // The session was running a moment ago; input for a terminal that ended meanwhile
            // is dropped by the hub.
            self.hub
                .input(terminal, Bytes::from_static(&[CTRL_C]))
                .await;
            return Ok(done(InterruptVia::Keystroke, None));
        }
        let Some(node) = node else {
            return Err(invalid_state("no agent node given"));
        };
        // A Codex subagent is a thread of the harness: its own turn is interrupted.
        if target.chat {
            match self
                .registry
                .chats()
                .interrupt_thread(session, &node.node_id)
                .await
            {
                Ok(()) => return Ok(done(InterruptVia::Thread, None)),
                Err(ThreadInterruptError::NoTurn) => {
                    return Err(invalid_state(format!(
                        "`{}` has no turn in progress",
                        node.label()
                    )));
                }
                Err(ThreadInterruptError::Failed(error)) => return Err(error),
                Err(ThreadInterruptError::Unsupported) => {}
            }
        }
        // A subagent that is a process of the session: SIGINT, and with `force` SIGTERM.
        if let (Some(pid), Some(root)) = (node.pid, target.root_pid) {
            return self
                .interrupt_process(session, root, pid, req.force)
                .await
                .map(|via| done(via, Some(pid)));
        }
        Err(invalid_state(format!(
            "this harness cannot stop a single subagent: `{}` ({}) has no process id and is not a \
             thread the harness can interrupt; interrupt the main agent instead",
            node.label(),
            node.agent
        )))
    }

    /// SIGINT to `pid`; with `force`, SIGTERM when it is still there after [`FORCE_GRACE`].
    async fn interrupt_process(
        &self,
        session: SessionId,
        root: u32,
        pid: u32,
        force: bool,
    ) -> Result<InterruptVia, ErrorBody> {
        let started = self.source.inspect(pid).map(|p| p.started_at_ms);
        self.send_signal(session, root, pid, SignalName::Int)
            .await?;
        if !force {
            return Ok(InterruptVia::Sigint);
        }
        let deadline = Instant::now() + FORCE_GRACE;
        while Instant::now() < deadline {
            sleep(FORCE_POLL).await;
            if !self.still_running(pid, started) {
                return Ok(InterruptVia::Sigint);
            }
        }
        if !self.still_running(pid, started) {
            return Ok(InterruptVia::Sigint);
        }
        // Verified again: the process may have been replaced by an unrelated one meanwhile.
        match self.send_signal(session, root, pid, SignalName::Term).await {
            Ok(()) => Ok(InterruptVia::SigintTerm),
            Err(error) if error.code == ErrorCode::NotFound => Ok(InterruptVia::Sigint),
            Err(error) => Err(error),
        }
    }

    /// The process that was `pid` (started at `started`) is still there and has not exited.
    fn still_running(&self, pid: u32, started: Option<u64>) -> bool {
        self.source.inspect(pid).is_some_and(|p| {
            p.state != ProcState::Zombie && started.is_none_or(|started| p.started_at_ms == started)
        })
    }

    /// The error for a node the session's agent tree does not have, or has finished.
    async fn missing_node(&self, session: SessionId, node: &str) -> ErrorBody {
        let known = match self.registry.agent_tree(Some(session)).await {
            Ok(tree) => tree
                .observations
                .iter()
                .any(|observation| observation.nodes.contains_key(node)),
            Err(error) => return error,
        };
        if known {
            invalid_state(format!("agent `{node}` has already finished"))
        } else {
            ErrorBody::new(
                ErrorCode::NotFound,
                format!("session {session} has no agent `{node}`"),
            )
        }
    }

    /// The session's process facts; `not_found` for an unknown session, `invalid_state` for one
    /// that is not running.
    async fn live_target(&self, session: SessionId) -> Result<ProcessTarget, ErrorBody> {
        self.registry
            .process_targets(Some(session))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| invalid_state(format!("session {session} is not running")))
    }
}

fn encode(
    epoch: hutan_core::Epoch,
    seq: hutan_core::Seq,
    sample: &ResourceSample,
) -> Option<Frame> {
    ControlMessage::event(
        epoch,
        seq,
        &Event::ResourcesUpdated {
            sample: Box::new(sample.clone()),
        },
    )
    .ok()?
    .to_frame()
    .ok()
}

#[cfg(test)]
mod tests {
    use std::time::Instant as StdInstant;

    use hutan_core::{Liveness, NodeState};

    use super::*;

    /// A process table that is a plain list.
    #[derive(Debug, Default)]
    struct Fake {
        table: Mutex<Vec<(ProcessSnapshot, Vec<String>)>>,
    }

    impl Fake {
        fn with(processes: &[(u32, u32, &str, u64)]) -> Self {
            let fake = Fake::default();
            for (pid, ppid, command, cpu_ms) in processes {
                fake.set(*pid, *ppid, command, *cpu_ms);
            }
            fake
        }

        fn set(&self, pid: u32, ppid: u32, command: &str, cpu_ms: u64) {
            let mut table = lock(&self.table);
            table.retain(|(p, _)| p.pid != pid);
            table.push((
                ProcessSnapshot {
                    pid,
                    ppid,
                    name: command.split(' ').next().unwrap_or("").to_owned(),
                    state: ProcState::Sleeping,
                    cpu_time_ns: cpu_ms * 1_000_000,
                    rss_bytes: 1024 * 1024,
                    threads: 2,
                    started_at_ms: 1_000 + u64::from(pid),
                },
                command.split(' ').map(str::to_owned).collect(),
            ));
        }
    }

    impl ProcessSource for Fake {
        fn descendants(&self, root: u32, limit: usize) -> Vec<u32> {
            let table = lock(&self.table);
            let mut found = Vec::new();
            let mut frontier = vec![root];
            while let Some(parent) = frontier.pop() {
                let mut kids: Vec<u32> = table
                    .iter()
                    .filter(|(p, _)| p.ppid == parent)
                    .map(|(p, _)| p.pid)
                    .collect();
                kids.sort_unstable();
                for kid in kids {
                    if found.len() < limit {
                        found.push(kid);
                        frontier.push(kid);
                    }
                }
            }
            found
        }

        fn inspect(&self, pid: u32) -> Option<ProcessSnapshot> {
            lock(&self.table)
                .iter()
                .find(|(p, _)| p.pid == pid)
                .map(|(p, _)| p.clone())
        }

        fn arguments(&self, pid: u32) -> Option<Vec<String>> {
            lock(&self.table)
                .iter()
                .find(|(p, _)| p.pid == pid)
                .map(|(_, args)| args.clone())
        }
    }

    fn target(session: SessionId, root: u32) -> ProcessTarget {
        ProcessTarget {
            session_id: session,
            terminal: None,
            root_pid: Some(root),
            chat: false,
            root_node: None,
            agents: Vec::new(),
        }
    }

    fn node(id: &str, kind: NodeKind, pid: Option<u32>) -> AgentNode {
        AgentNode {
            node_id: id.to_owned(),
            parent: None,
            kind,
            source: "test".into(),
            agent: "omp".into(),
            name: Some(id.to_owned()),
            agent_type: None,
            state: NodeState::Working,
            outcome: None,
            liveness: Liveness::Alive,
            model: None,
            started_at_ms: Some(1),
            ended_at_ms: None,
            last_activity_ms: 1,
            activity: None,
            usage: None,
            transcript_path: None,
            pid,
        }
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_command_line_loses_secrets_before_it_is_cut() {
        let line = command_line(&args(&[
            "curl",
            "--token=abcdef123456",
            "--api-key",
            "sk-proj-abcdefghijklmnop1234567890",
            "API_KEY=supersecretvalue",
            "-H",
            "Authorization: Bearer abcdef0123456789abcdef",
            "--max-tokens",
            "100",
            "https://user:hunter2hunter2@example.com/x",
            "two words",
        ]));
        for secret in [
            "abcdef123456",
            "sk-proj",
            "supersecretvalue",
            "abcdef0123456789abcdef",
            "hunter2hunter2",
        ] {
            assert!(!line.contains(secret), "{secret} survived: {line}");
        }
        assert!(line.contains("--token=[REDACTED]"), "{line}");
        assert!(line.contains("--api-key [REDACTED]"), "{line}");
        assert!(line.contains("API_KEY=[REDACTED]"), "{line}");
        // A count is not a secret.
        assert!(line.contains("--max-tokens 100"), "{line}");
        assert!(line.ends_with("'two words'"), "{line}");
    }

    #[test]
    fn long_commands_are_cut_at_512_characters_after_redaction() {
        let long = "é".repeat(2000);
        let line = command_line(&args(&["echo", &long]));
        assert_eq!(line.chars().count(), MAX_COMMAND_CHARS);
        // A secret that starts before the cut is gone, not shortened to its first characters.
        let padded = format!("{} --token=abcdefghijklmnop", "x".repeat(500));
        let line = command_line(&args(&[&padded]));
        assert!(!line.contains("abcdef"), "{line}");
    }

    #[test]
    fn a_tree_is_assembled_from_the_table_with_parents_first() {
        let fake = Fake::with(&[
            (100, 1, "-zsh", 10),
            (120, 100, "cargo test", 20),
            (110, 100, "sleep 60", 5),
            (130, 120, "rustc --crate-name x", 30),
            // Not below the session: another session's shell, and an unrelated process.
            (200, 1, "-zsh", 1),
            (210, 200, "vim", 1),
        ]);
        let session = SessionId::new();
        let (sample, _) = collect(
            &fake,
            &[target(session, 100)],
            999,
            &Marks::default(),
            Instant::now(),
            7,
        );
        let resources = &sample.sessions[0];
        let pids: Vec<u32> = resources.processes.iter().map(|p| p.pid).collect();
        assert_eq!(pids, [100, 110, 120, 130]);
        assert_eq!(resources.totals.processes, 4);
        assert_eq!(resources.totals.rss_bytes, 4 * 1024 * 1024);
        assert_eq!(resources.processes[3].command, "rustc --crate-name x");
        assert!(resources.processes.iter().all(|p| p.cpu_percent.is_none()));
        assert!(!resources.truncated && !sample.truncated);
        // The daemon row exists even when it cannot be read.
        assert_eq!(
            (sample.daemon.pid, sample.daemon.name.as_str()),
            (999, "hutand")
        );
        assert_eq!(sample.at_ms, 7);
    }

    #[test]
    fn cpu_is_the_delta_of_cpu_time_over_wall_time() {
        let fake = Fake::with(&[(100, 1, "sh", 1_000), (110, 100, "yes", 1_000)]);
        let session = SessionId::new();
        let targets = [target(session, 100)];
        let start = Instant::now();
        let (_, marks) = collect(&fake, &targets, 1, &Marks::default(), start, 0);
        // `yes` used 500 ms of CPU in 1 s of wall time; the shell 10 ms.
        fake.set(110, 100, "yes", 1_500);
        fake.set(100, 1, "sh", 1_010);
        let (sample, again) = collect(
            &fake,
            &targets,
            1,
            &marks,
            start + Duration::from_secs(1),
            1,
        );
        let by_pid = |pid: u32| {
            sample.sessions[0]
                .processes
                .iter()
                .find(|p| p.pid == pid)
                .and_then(|p| p.cpu_percent)
        };
        let yes = by_pid(110).unwrap();
        assert!((yes - 50.0).abs() < 0.01, "{yes}");
        assert!((by_pid(100).unwrap() - 1.0).abs() < 0.01);
        assert!((sample.sessions[0].totals.cpu_percent - 51.0).abs() < 0.01);
        // A window too short to mean anything repeats the figure and keeps the old reading.
        fake.set(110, 100, "yes", 9_999);
        let (soon, kept) = collect(
            &fake,
            &targets,
            1,
            &again,
            start + Duration::from_millis(1_050),
            2,
        );
        assert_eq!(soon.sessions[0].processes[1].cpu_percent, Some(yes));
        assert_eq!(
            kept.by_pid[&110].cpu_time_ns,
            again.by_pid[&110].cpu_time_ns
        );
        // A pid that was reused (another start time) has no history.
        let reused = Fake::with(&[(100, 1, "sh", 1_000)]);
        reused.table.lock().unwrap_or_else(PoisonError::into_inner)[0]
            .0
            .started_at_ms = 5;
        let (fresh, _) = collect(
            &reused,
            &targets,
            1,
            &marks,
            start + Duration::from_secs(2),
            3,
        );
        assert_eq!(fresh.sessions[0].processes[0].cpu_percent, None);
        let _ = StdInstant::now();
    }

    #[test]
    fn lists_are_bounded_per_session_and_in_total() {
        let fake = Fake::default();
        fake.set(100, 1, "sh", 0);
        for pid in 101..(101 + 600) {
            fake.set(pid, 100, "sleep 1", 0);
        }
        let session = SessionId::new();
        let (sample, _) = collect(
            &fake,
            &[target(session, 100)],
            1,
            &Marks::default(),
            Instant::now(),
            0,
        );
        assert_eq!(
            sample.sessions[0].processes.len(),
            MAX_PROCESSES_PER_SESSION
        );
        assert_eq!(sample.sessions[0].processes[0].pid, 100);
        assert!(sample.sessions[0].truncated);
        assert!(!sample.truncated, "cut by the session limit, not the total");
        // Nine such sessions would need more than the total allows.
        let many: Vec<ProcessTarget> = (0..9).map(|_| target(SessionId::new(), 100)).collect();
        let (sample, _) = collect(&fake, &many, 1, &Marks::default(), Instant::now(), 0);
        let total: usize = sample.sessions.iter().map(|s| s.processes.len()).sum();
        assert_eq!(total, MAX_PROCESSES_TOTAL);
        assert!(sample.truncated);
        assert!(sample.sessions[8].truncated && sample.sessions[8].processes.is_empty());
    }

    #[test]
    fn agent_pids_are_matched_to_processes_of_the_tree_only() {
        let fake = Fake::with(&[
            (100, 1, "omp", 0),
            (110, 100, "omp-task", 0),
            (500, 1, "other", 0),
        ]);
        let session = SessionId::new();
        let mut target = target(session, 100);
        target.agents = vec![
            node("root", NodeKind::Main, Some(100)),
            node("worker", NodeKind::Sub, Some(110)),
            // Reported a pid that is not below the session: not shown, not matched.
            node("liar", NodeKind::Sub, Some(500)),
            node("thread", NodeKind::Sub, None),
        ];
        let (sample, _) = collect(&fake, &[target], 1, &Marks::default(), Instant::now(), 0);
        let resources = &sample.sessions[0];
        let pid_of = |id: &str| {
            resources
                .agents
                .iter()
                .find(|a| a.node_id == id)
                .unwrap()
                .pid
        };
        assert_eq!(pid_of("root"), Some(100));
        assert_eq!(pid_of("worker"), Some(110));
        assert_eq!(pid_of("liar"), None);
        assert_eq!(pid_of("thread"), None);
        assert_eq!(resources.agents[0].node_id, "root", "the main agent first");
        let node_of = |pid: u32| {
            resources
                .processes
                .iter()
                .find(|p| p.pid == pid)
                .unwrap()
                .agent_node
                .clone()
        };
        assert_eq!(node_of(100).as_deref(), Some("root"));
        assert_eq!(node_of(110).as_deref(), Some("worker"));
    }

    #[test]
    fn only_the_sessions_own_tree_may_be_signalled() {
        let fake = Fake::with(&[
            (100, 1, "-zsh", 0),
            (110, 100, "sleep", 0),
            (120, 110, "sleep", 0),
            (900, 1, "hutand", 0),
            (910, 900, "bystander", 0),
        ]);
        let daemon = 900;
        assert_eq!(verify_descendant(&fake, 100, daemon, 100), Ok(()));
        assert_eq!(verify_descendant(&fake, 100, daemon, 110), Ok(()));
        assert_eq!(verify_descendant(&fake, 100, daemon, 120), Ok(()));
        assert_eq!(
            verify_descendant(&fake, 100, daemon, 910),
            Err(Refusal::Outside)
        );
        assert_eq!(
            verify_descendant(&fake, 100, daemon, 12345),
            Err(Refusal::Outside)
        );
        assert_eq!(
            verify_descendant(&fake, 100, daemon, daemon),
            Err(Refusal::Daemon)
        );
        for pid in [0, 1] {
            assert_eq!(
                verify_descendant(&fake, 100, daemon, pid),
                Err(Refusal::Special)
            );
        }
        // Gone after the session's tree changed: refused, not signalled blindly.
        lock(&fake.table).retain(|(p, _)| p.pid != 120);
        assert_eq!(
            verify_descendant(&fake, 100, daemon, 120),
            Err(Refusal::Outside)
        );
        let body = refusal_error(Refusal::Outside, SessionId::new(), 5);
        assert_eq!(body.code, ErrorCode::Forbidden);
    }
}
