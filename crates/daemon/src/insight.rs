//! Agent insight (protocol 0.59, capability `agent.insight`; `docs/agents.md` § Insight).
//!
//! [`InsightHub`] keeps one [`InsightReducer`] per chat session that someone looked at or that
//! runs: the chat pump feeds it every event that can change the insight and the cost an ACP
//! harness reports; a request asks for a snapshot; a connection that said `client.focus` on the
//! session is told when it changes.
//!
//! - **Seeded from the journal.** A reducer is built from the newest [`COLD_EVENTS`] events of the
//!   session's journal (what insight reads, with payloads and tool output cut out in SQLite) and
//!   the session's `usage_turns` rows (the cost of a turn, and the turns older than the journal
//!   window). It is built when a chat's agent starts and when a request finds none.
//! - **Bounded.** At most [`MAX_SESSIONS`] sessions are tracked (the one least recently used
//!   goes; it is built again from the journal when asked for), each reducer holds at most what
//!   `hutan_core::insight` caps (200 turns, 100 files, 17 tool rows), and a snapshot is cut to
//!   32 KiB.
//! - **Quiet.** Nothing runs while nothing changes: no timer, no polling. A change arms one
//!   coalescing task that sends the snapshot to the focused connections and ends; at most one
//!   `agent.insight_updated` per session per [`MIN_GAP`]. A session nobody focuses is never
//!   serialised.
//! - **Transcripts of terminal agents** (INSIGHT-C). For a terminal that runs Claude Code or
//!   Codex the hub also follows the agent's transcript (`transcript_tail`): only when the user
//!   allowed that agent's `[history]` source, only while the session lives and a connection
//!   focuses it (a task that reads once a second and ends 30 s after the focus was lost), or
//!   when a request or a detector asks (one read, at most one a second). What it reads goes into a
//!   reducer of its own, merged with the integrations' by source (`insight_merge`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use hutan_core::checkpoint::{CheckpointKind, CheckpointState};
use hutan_core::insight::{
    AgentInsight, BreakdownRow, InsightModel, InsightReducer, InsightReport, MAX_TURNS, SeedTurn,
    harness_insight,
};
use hutan_core::{AgentEvent, SessionId, TurnId};
use hutan_index::Source;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::Event;
use hutan_protocol::{ControlMessage, MAX_PAYLOAD};
use hutan_storage::{CheckpointFilter, Storage};
use tracing::{debug, warn};

use crate::bus::EventBus;
use crate::conn::{ConnHandle, ConnId, LocalSend};
use crate::history::HistoryService;
use crate::insight_merge::{self, Extras};
use crate::registry::{WeakRegistry, now_ms};
use crate::transcript_file::{CATCH_UP_BYTES, TailFile};
use crate::transcript_fold::{self, Folded};
use crate::transcript_tail::{self, Candidate, Lookup, Resolution, Route, TailHints, Why};

/// Journal events a reducer is built from (the newest, of the kinds insight reads).
pub const COLD_EVENTS: u32 = 5_000;
/// Sessions with a reducer; the least recently used goes first.
pub const MAX_SESSIONS: usize = 64;
/// At least this long between two `agent.insight_updated` of one session.
pub const MIN_GAP: Duration = Duration::from_secs(1);
/// Events and costs kept while a reducer is being built; past this the build is read again.
const MAX_BUFFERED: usize = 4_096;

/// What the registry knows of a chat that insight needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InsightTarget {
    /// `codex`, `omp`, `claude` or `direct` (an assistant that runs on a provider is `direct`).
    pub harness: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub cwd: Option<String>,
    /// The session is an assistant (Mawas): it has insight, but `/context` is not sent to it.
    pub assistant: bool,
    /// The session is a terminal that runs an agent (protocol 0.71): no journal, only what the
    /// agent's integrations report; `harness` is the agent's program.
    pub terminal: bool,
    /// A terminal's agent: what finding its transcript needs (empty for a chat).
    pub tail: TailHints,
}

impl InsightTarget {
    fn model(&self) -> Option<InsightModel> {
        self.model.as_ref().map(|id| InsightModel {
            id: id.clone(),
            provider: self.provider.clone(),
            thinking: None,
        })
    }
}

#[derive(Debug)]
enum Input {
    Event(Box<AgentEvent>),
    Cost(f64),
}

#[derive(Debug)]
struct Entry {
    reducer: InsightReducer,
    model: Option<InsightModel>,
    /// Least recently used first goes.
    touched: u64,
    dirty: bool,
    /// A task will flush this entry.
    armed: bool,
    last_sent: Option<Instant>,
}

impl Entry {
    fn new(reducer: InsightReducer, model: Option<InsightModel>, touched: u64) -> Self {
        Self {
            reducer,
            model,
            touched,
            dirty: false,
            armed: false,
            last_sent: None,
        }
    }
}

#[derive(Debug)]
struct Focus {
    conn: ConnHandle,
    session: SessionId,
}

#[derive(Debug, Default)]
struct State {
    entries: HashMap<SessionId, Entry>,
    /// Sessions whose reducer is being read from the journal: what arrives meanwhile.
    building: HashMap<SessionId, Vec<Input>>,
    overflowed: Vec<SessionId>,
    focus: HashMap<ConnId, Focus>,
    tick: u64,
    /// The transcript of each terminal session that has one looked at.
    tails: HashMap<SessionId, Tail>,
}

impl State {
    /// Makes room for an entry of `session`: past [`MAX_SESSIONS`] the least recently used one
    /// that nobody looks at goes (when everybody looks at some, the least recently used).
    fn make_room(&mut self, session: SessionId) {
        if self.entries.len() < MAX_SESSIONS || self.entries.contains_key(&session) {
            return;
        }
        let focused: Vec<SessionId> = self.focus.values().map(|f| f.session).collect();
        let victim = self
            .entries
            .iter()
            .filter(|(id, _)| !focused.contains(id))
            .min_by_key(|(_, entry)| entry.touched)
            .map(|(id, _)| *id)
            .or_else(|| {
                self.entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.touched)
                    .map(|(id, _)| *id)
            });
        if let Some(victim) = victim {
            self.entries.remove(&victim);
            self.tails.remove(&victim);
        }
    }
}

#[derive(Debug)]
pub struct InsightHub {
    storage: Storage,
    bus: Arc<EventBus>,
    state: Mutex<State>,
    /// Builds are rare and read the journal: one at a time.
    building: tokio::sync::Mutex<()>,
    seq: AtomicU64,
    /// What following a terminal agent's transcript asks of the daemon (set once, after the
    /// registry and the history service exist; weak, because the registry owns this hub).
    links: OnceLock<Links>,
}

#[derive(Debug)]
struct Links {
    registry: WeakRegistry,
    history: Weak<HistoryService>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // No critical section leaves the state half updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl InsightHub {
    pub fn new(storage: Storage, bus: Arc<EventBus>) -> Arc<Self> {
        Arc::new(Self {
            storage,
            bus,
            state: Mutex::new(State::default()),
            building: tokio::sync::Mutex::new(()),
            seq: AtomicU64::new(0),
            links: OnceLock::new(),
        })
    }

    /// Connects the hub to the services it asks for a terminal agent's transcript: the registry
    /// (what the session runs, what its hooks reported) and the history service (whether the user
    /// allowed reading it, and where the index found it). Without it no transcript is read.
    pub fn wire(&self, registry: WeakRegistry, history: Weak<HistoryService>) {
        // Set once at start-up; a second call changes nothing.
        let _ = self.links.set(Links { registry, history });
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    // Feeding ---------------------------------------------------------------------------------

    /// A journaled event of a chat. O(1) for an event insight does not read; a session without
    /// a reducer costs nothing.
    pub fn observe(self: &Arc<Self>, event: &AgentEvent) {
        if !InsightReducer::wants(&event.kind) {
            return;
        }
        let mut guard = lock(&self.state);
        let state = &mut *guard;
        if let Some(buffer) = state.building.get_mut(&event.session_id) {
            if buffer.len() < MAX_BUFFERED {
                buffer.push(Input::Event(Box::new(event.clone())));
            } else if !state.overflowed.contains(&event.session_id) {
                state.overflowed.push(event.session_id);
            }
            return;
        }
        state.tick += 1;
        let tick = state.tick;
        let Some(entry) = state.entries.get_mut(&event.session_id) else {
            return;
        };
        if entry.reducer.apply(event) {
            entry.touched = tick;
            self.changed(state, event.session_id);
        }
    }

    /// The harness reported the cumulative cost of the session, in US dollars.
    pub fn cost(self: &Arc<Self>, session: SessionId, cost_usd: f64) {
        let mut guard = lock(&self.state);
        let state = &mut *guard;
        if let Some(buffer) = state.building.get_mut(&session) {
            if buffer.len() < MAX_BUFFERED {
                buffer.push(Input::Cost(cost_usd));
            }
            return;
        }
        if let Some(entry) = state.entries.get_mut(&session) {
            entry.reducer.cost_reported(cost_usd);
        }
    }

    /// What the integrations of a terminal agent reported (`agent.report`'s `insight`, protocol
    /// 0.71). `agent` is the agent's program (`omp`, `claude`, `codex`), `model` the one its
    /// node says. `fresh`: the report's `seq` is the newest of its source. A report that is not
    /// still adds its tools, files and tokens (deltas that arrive out of order are as true as
    /// the others) but not its levels (the turn's edges, context, cost, API time), which a newer
    /// report has already replaced.
    ///
    /// A session whose reducer is of another agent (the user ran a different one in the same
    /// terminal) starts again. O(report); no allocation for a report that says nothing.
    pub fn terminal_report(
        self: &Arc<Self>,
        session: SessionId,
        agent: &str,
        cwd: Option<&str>,
        model: Option<InsightModel>,
        mut report: InsightReport,
        fresh: bool,
    ) {
        if !fresh {
            report.turn = None;
            report.context = None;
            report.cost_usd = None;
            report.api_ms = None;
        }
        if report.is_empty() {
            return;
        }
        let mut guard = lock(&self.state);
        let state = &mut *guard;
        state.tick += 1;
        let tick = state.tick;
        // A chat's reducer is never replaced by a report; another agent's is.
        let same_agent = match state.entries.get(&session) {
            Some(entry) if !entry.reducer.is_terminal() => return,
            Some(entry) => Some(entry.reducer.harness() == agent),
            None => None,
        };
        if same_agent != Some(true) {
            if same_agent == Some(false) {
                // Another agent in the same terminal: what was read of the last one's
                // transcript is not about this one.
                state.tails.remove(&session);
            }
            state.make_room(session);
            state.entries.insert(
                session,
                Entry::new(
                    InsightReducer::new_terminal(session, agent, cwd),
                    None,
                    tick,
                ),
            );
        }
        let Some(entry) = state.entries.get_mut(&session) else {
            return;
        };
        if model.is_some() {
            entry.model = model;
        }
        if entry.reducer.apply_report(&report, now_ms()) {
            entry.touched = tick;
            self.changed(state, session);
        }
        // A report from a session somebody looks at: its transcript is followed too.
        let focused = state.focus.values().any(|focus| focus.session == session);
        drop(guard);
        if focused {
            self.start_tail(session);
        }
    }

    /// The session is gone for good (deleted, or its journal was replaced).
    pub fn forget(&self, session: SessionId) {
        let mut state = lock(&self.state);
        state.entries.remove(&session);
        state.tails.remove(&session);
        state.focus.retain(|_, focus| focus.session != session);
    }

    // Building --------------------------------------------------------------------------------

    /// Reads the reducer of `session` from the journal and the usage records.
    async fn read(
        &self,
        session: SessionId,
        target: &InsightTarget,
        fresh: bool,
    ) -> InsightReducer {
        let storage = self.storage.clone();
        let harness = target.harness.clone();
        let cwd = target.cwd.clone();
        let built = tokio::task::spawn_blocking(move || {
            let mut reducer = InsightReducer::new(session, &harness, cwd.as_deref(), fresh);
            let items = match storage.chat().insight_items(session, COLD_EVENTS) {
                Ok(items) => items,
                Err(error) => {
                    warn!(%session, %error, "could not read the journal for insight");
                    Vec::new()
                }
            };
            let cut = u32::try_from(items.len()).is_ok_and(|n| n >= COLD_EVENTS);
            for row in &items {
                match serde_json::from_str::<AgentEvent>(&row.event_json) {
                    Ok(event) => {
                        reducer.apply(&event);
                    }
                    Err(error) => {
                        debug!(%session, seq = row.seq, %error, "an event insight cannot read")
                    }
                }
            }
            let turns = match storage
                .usage()
                .turns_of(session, u32::try_from(MAX_TURNS).unwrap_or(u32::MAX))
            {
                Ok(rows) => rows,
                Err(error) => {
                    warn!(%session, %error, "could not read the usage records for insight");
                    Vec::new()
                }
            };
            let seeds: Vec<SeedTurn> = turns
                .into_iter()
                .map(|row| SeedTurn {
                    turn_id: row.turn_id,
                    at_ms: row.at_ms,
                    tokens: hutan_core::agent::TokenCounts {
                        input_tokens: row.input_tokens,
                        cached_input_tokens: row.cached_input_tokens,
                        output_tokens: row.output_tokens,
                        reasoning_output_tokens: row.reasoning_output_tokens,
                        total_tokens: row.total_tokens,
                        cache_write_tokens: None,
                    },
                    cost_usd: row.cost_usd,
                })
                .collect();
            reducer.seed(&seeds, cut);
            reducer
        })
        .await;
        match built {
            Ok(reducer) => reducer,
            Err(error) => {
                warn!(%session, %error, "building an insight failed");
                InsightReducer::new(session, &target.harness, target.cwd.as_deref(), fresh)
            }
        }
    }

    /// Builds the reducer of `session` and puts it in place, with what arrived meanwhile.
    /// `replace`: a reducer that exists is built again (a chat's agent just started).
    async fn build(
        self: &Arc<Self>,
        session: SessionId,
        target: &InsightTarget,
        fresh: bool,
        replace: bool,
    ) {
        if !replace && lock(&self.state).entries.contains_key(&session) {
            return;
        }
        if target.terminal {
            // A terminal agent has no journal: what is known is what its integrations report,
            // so an empty reducer waits for the first report.
            let mut state = lock(&self.state);
            state.tick += 1;
            let tick = state.tick;
            state.make_room(session);
            let reducer =
                InsightReducer::new_terminal(session, &target.harness, target.cwd.as_deref());
            state
                .entries
                .entry(session)
                .or_insert_with(|| Entry::new(reducer, target.model(), tick));
            return;
        }
        let _one_at_a_time = self.building.lock().await;
        // Built by whoever held the lock before.
        if !replace && lock(&self.state).entries.contains_key(&session) {
            return;
        }
        for _ in 0..3 {
            lock(&self.state).building.insert(session, Vec::new());
            let reducer = self.read(session, target, fresh).await;
            let mut state = lock(&self.state);
            let buffered = state.building.remove(&session).unwrap_or_default();
            if let Some(at) = state.overflowed.iter().position(|id| *id == session) {
                // Too much happened while reading: what was lost cannot be told; read again.
                state.overflowed.swap_remove(at);
                continue;
            }
            let mut reducer = reducer;
            for input in buffered {
                match input {
                    Input::Event(event) => {
                        reducer.apply(&event);
                    }
                    Input::Cost(cost) => reducer.cost_reported(cost),
                }
            }
            state.tick += 1;
            let tick = state.tick;
            state.make_room(session);
            state
                .entries
                .insert(session, Entry::new(reducer, target.model(), tick));
            return;
        }
        warn!(%session, "insight could not keep up with the session's events; it will be built again when asked for");
    }

    /// A chat's agent started: its reducer is read from the journal now (`fresh`: the
    /// conversation began with this start), so every event of the run lands in it.
    pub async fn attach(self: &Arc<Self>, session: SessionId, target: &InsightTarget, fresh: bool) {
        self.build(session, target, fresh, true).await;
    }

    // Asking ----------------------------------------------------------------------------------

    /// The insight of `session` now. Builds the reducer when there is none.
    pub async fn snapshot(
        self: &Arc<Self>,
        session: SessionId,
        target: &InsightTarget,
    ) -> Result<AgentInsight, ErrorBody> {
        self.build(session, target, false, false).await;
        if target.terminal {
            // A request (or a detector) asks: the transcript is read once, when the user allowed
            // it and it was not read a moment ago.
            self.sync(session, Some(target.clone()), Reason::Pull).await;
            // A session somebody looks at is followed from now on (its agent may have started
            // after the window began to look).
            let focused = lock(&self.state)
                .focus
                .values()
                .any(|focus| focus.session == session);
            if focused {
                self.start_tail(session);
            }
        }
        let mut insight = {
            let mut state = lock(&self.state);
            state.tick += 1;
            let tick = state.tick;
            let Some(entry) = state.entries.get_mut(&session) else {
                // Evicted again at once, or the build gave up: say so rather than guess.
                return Err(ErrorBody::new(
                    ErrorCode::InvalidState,
                    "the insight of this session could not be built just now; ask again",
                ));
            };
            entry.touched = tick;
            entry.model = target.model();
            let seq = self.next_seq();
            let Some(insight) = state.snapshot_of(session, seq, now_ms()) else {
                return Err(ErrorBody::new(
                    ErrorCode::InvalidState,
                    "the insight of this session could not be built just now; ask again",
                ));
            };
            insight
        };
        self.add_checkpoints(session, &mut insight).await;
        Ok(insight)
    }

    /// Writes each turn's checkpoint state into `insight` (a client shows Fork only for turns
    /// that cannot be rewound to).
    async fn add_checkpoints(&self, session: SessionId, insight: &mut AgentInsight) {
        if insight.turns.is_empty() {
            return;
        }
        let storage = self.storage.clone();
        let rows = tokio::task::spawn_blocking(move || {
            storage
                .checkpoints()
                .list(CheckpointFilter::Session(session))
        })
        .await;
        let rows = match rows {
            Ok(Ok(rows)) => rows,
            Ok(Err(error)) => {
                warn!(%session, %error, "could not read the checkpoints for insight");
                return;
            }
            Err(error) => {
                warn!(%session, %error, "reading the checkpoints for insight failed");
                return;
            }
        };
        let states: HashMap<TurnId, CheckpointState> = rows
            .into_iter()
            .filter(|row| row.kind == CheckpointKind::Turn)
            .filter_map(|row| Some((row.turn_id?, row.state)))
            .collect();
        for turn in &mut insight.turns {
            turn.checkpoint = turn.turn_id.and_then(|id| states.get(&id).copied());
        }
    }

    /// The breakdown time the session's insight has now: what `agent.measure_context` compares.
    pub async fn breakdown_stamp(
        self: &Arc<Self>,
        session: SessionId,
        target: &InsightTarget,
    ) -> Option<u64> {
        self.build(session, target, false, false).await;
        lock(&self.state)
            .entries
            .get(&session)?
            .reducer
            .breakdown_at()
    }

    // Telling ---------------------------------------------------------------------------------

    /// `client.focus`: `conn` looks at `session` (`None`: at none). A terminal session that is
    /// looked at has its agent's transcript followed (when the user allowed it).
    pub fn focus(self: &Arc<Self>, conn: &ConnHandle, session: Option<SessionId>) {
        let mut state = lock(&self.state);
        let before = match session {
            Some(session) => {
                let before = state.focus.insert(
                    conn.id(),
                    Focus {
                        conn: conn.clone(),
                        session,
                    },
                );
                state.tick += 1;
                let tick = state.tick;
                if let Some(entry) = state.entries.get_mut(&session) {
                    entry.touched = tick;
                }
                before
            }
            None => state.focus.remove(&conn.id()),
        };
        if let Some(left) = before
            .map(|focus| focus.session)
            .filter(|left| Some(*left) != session)
        {
            state.lost_focus(left);
        }
        drop(state);
        if let Some(session) = session {
            self.start_tail(session);
        }
    }

    /// A connection closed: its focus goes with it.
    pub fn connection_closed(&self, conn: ConnId) {
        let mut state = lock(&self.state);
        if let Some(focus) = state.focus.remove(&conn) {
            state.lost_focus(focus.session);
        }
    }

    /// An entry changed: when somebody looks at the session, one task sends the snapshot once
    /// [`MIN_GAP`] has passed since the last one.
    fn changed(self: &Arc<Self>, state: &mut State, session: SessionId) {
        if !state.focus.values().any(|focus| focus.session == session) {
            return;
        }
        let Some(entry) = state.entries.get_mut(&session) else {
            return;
        };
        entry.dirty = true;
        if entry.armed {
            return;
        }
        entry.armed = true;
        let wait = entry
            .last_sent
            .map_or(Duration::ZERO, |at| MIN_GAP.saturating_sub(at.elapsed()));
        let hub = self.clone();
        tokio::spawn(async move {
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
            hub.flush(session).await;
        });
    }

    async fn flush(self: &Arc<Self>, session: SessionId) {
        let (mut insight, targets) = {
            let mut state = lock(&self.state);
            let targets: Vec<ConnHandle> = state
                .focus
                .values()
                .filter(|focus| focus.session == session)
                .map(|focus| focus.conn.clone())
                .collect();
            let Some(entry) = state.entries.get_mut(&session) else {
                return;
            };
            entry.armed = false;
            if !entry.dirty || targets.is_empty() {
                return;
            }
            entry.dirty = false;
            entry.last_sent = Some(Instant::now());
            let Some(insight) = state.snapshot_of(session, self.next_seq(), now_ms()) else {
                return;
            };
            (insight, targets)
        };
        self.add_checkpoints(session, &mut insight).await;
        let event = Event::AgentInsightUpdated {
            session_id: session,
            insight: Box::new(insight),
        };
        let frame = ControlMessage::event(self.bus.epoch(), self.bus.last_seq(), &event)
            .ok()
            .and_then(|message| message.to_frame().ok())
            .filter(|frame| frame.payload.len() <= MAX_PAYLOAD);
        let Some(frame) = frame else {
            warn!(%session, "an insight snapshot does not fit a protocol frame; skipped");
            return;
        };
        for conn in targets {
            // A connection too slow for this one misses it: the next replaces it.
            if matches!(conn.enqueue_local(frame.clone()), LocalSend::Gone) {
                lock(&self.state).focus.remove(&conn.id());
            }
        }
    }

    /// How many sessions have a reducer.
    pub fn tracked(&self) -> usize {
        lock(&self.state).entries.len()
    }

    /// Whether `session` has a reducer now.
    pub fn is_tracked(&self, session: SessionId) -> bool {
        lock(&self.state).entries.contains_key(&session)
    }

    /// Approximate bytes held by the reducers (a test of the budget reads it).
    pub fn bytes(&self) -> usize {
        lock(&self.state)
            .entries
            .values()
            .map(|entry| entry.reducer.approx_bytes())
            .sum()
    }
}

// Transcripts of terminal agents ----------------------------------------------------------------

/// How often the transcript of a focused terminal session is looked at.
pub const TAIL_POLL: Duration = Duration::from_secs(1);
/// How long after the last connection lost its focus the transcript is still followed.
pub const TAIL_GRACE: Duration = Duration::from_secs(30);
/// At least this long between two reads of one transcript.
const READ_GAP: Duration = Duration::from_secs(1);
/// At least this long between two looks at the history catalog for one session.
const CATALOG_GAP: Duration = Duration::from_secs(3);

/// Where following the transcript of a terminal session stands (`hutan integration status`, the
/// findings' coverage, tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptStatus {
    /// Nothing looked: nobody focused the session and no request asked.
    NotLooked,
    /// The user has not allowed Hutan to read this agent's transcripts (`[history]`).
    Off(Source),
    /// Allowed, but no transcript was found (yet), and why.
    Searching(Why),
    /// Found, and not written yet.
    Waiting(Route),
    /// Being read.
    Reading {
        route: Route,
        /// Followed from the middle of a long transcript: older turns are not counted.
        cut: bool,
    },
    Unreadable(String),
}

impl TranscriptStatus {
    /// The status in a sentence.
    pub fn text(&self) -> String {
        match self {
            Self::NotLooked => "its transcript has not been looked at".to_owned(),
            Self::Off(source) => format!(
                "{} history is off: Hutan does not read its transcript",
                source_name(*source)
            ),
            Self::Searching(why) => format!("no transcript found: {}", why.text()),
            Self::Waiting(route) => format!(
                "transcript found through {} ({}); not written yet",
                route.label(),
                route.sureness()
            ),
            Self::Reading { route, cut } => format!(
                "reading its transcript, found through {} ({}){}",
                route.label(),
                route.sureness(),
                if *cut {
                    "; only the end of a long transcript is counted"
                } else {
                    ""
                }
            ),
            Self::Unreadable(why) => format!("its transcript cannot be read: {why}"),
        }
    }

    /// Whether the transcript feeds the insight.
    pub fn is_reading(&self) -> bool {
        matches!(self, Self::Reading { .. })
    }
}

/// What a source is called to the user.
pub fn source_name(source: Source) -> &'static str {
    match source {
        Source::Claude => "Claude Code",
        Source::Codex => "Codex",
        Source::Omp => "OMP",
        Source::Hutan => "Hutan",
    }
}

/// What the transcript of one session gave the insight.
#[derive(Debug)]
struct Feed {
    reducer: InsightReducer,
    model: Option<String>,
    /// The context by category of the newest `/context` record, and when it was written.
    breakdown: Option<(Vec<BreakdownRow>, u64)>,
    /// Followed from the middle of a long transcript.
    cut: bool,
}

impl Feed {
    fn new(session: SessionId, agent: &str, cwd: Option<&str>) -> Self {
        Self {
            reducer: InsightReducer::new_terminal(session, agent, cwd),
            model: None,
            breakdown: None,
            cut: false,
        }
    }
}

/// The transcript of one terminal session.
#[derive(Debug)]
struct Tail {
    /// The agent the transcript is of: another one starts again.
    agent: String,
    /// Out while a read runs (`busy`).
    file: Option<TailFile>,
    feed: Option<Feed>,
    status: TranscriptStatus,
    busy: bool,
    /// A task follows the transcript.
    running: bool,
    last_read: Option<Instant>,
    last_catalog: Option<Instant>,
    /// The last time somebody looked at the session or asked for its insight.
    demanded: Instant,
}

impl Tail {
    fn new(agent: &str) -> Self {
        Self {
            agent: agent.to_owned(),
            file: None,
            feed: None,
            status: TranscriptStatus::NotLooked,
            busy: false,
            running: false,
            last_read: None,
            last_catalog: None,
            demanded: Instant::now(),
        }
    }
}

/// Why a transcript is looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// The task that follows a focused session.
    Loop,
    /// A request or a detector asked for the insight.
    Pull,
}

/// Whether the transcript of the session `hints` describe is not the one `file` is any more: its
/// agent started another conversation (`/clear`, `/new`), or now says which file it is when
/// the match was a guess.
fn rebind_wanted(file: &TailFile, hints: &TailHints) -> bool {
    let other_id = hints
        .agent_session_id
        .as_deref()
        .is_some_and(|id| file.key.as_deref() != Some(id));
    let other_path = hints
        .transcript_path
        .as_deref()
        .is_some_and(|path| std::path::Path::new(path) != file.path);
    let better_route = file.route == Route::Folder
        && (hints.agent_session_id.is_some() || hints.transcript_path.is_some());
    other_id || other_path || better_route
}

impl State {
    /// `session` lost a viewer: its transcript is followed for [`TAIL_GRACE`] more.
    fn lost_focus(&mut self, session: SessionId) {
        if let Some(tail) = self.tails.get_mut(&session) {
            tail.demanded = Instant::now();
        }
    }

    /// Somebody focuses `session`, or looked at it less than [`TAIL_GRACE`] ago.
    fn demanded(&self, session: SessionId) -> bool {
        self.focus.values().any(|focus| focus.session == session)
            || self
                .tails
                .get(&session)
                .is_some_and(|tail| tail.demanded.elapsed() < TAIL_GRACE)
    }

    /// The insight of `session`: its integrations' reducer, merged with what its transcript
    /// gave (`insight_merge`).
    fn snapshot_of(&self, session: SessionId, seq: u64, as_of_ms: u64) -> Option<AgentInsight> {
        let entry = self.entries.get(&session)?;
        let base = entry.reducer.snapshot(seq, as_of_ms, entry.model.clone());
        let Some(feed) = self.tails.get(&session).and_then(|tail| tail.feed.as_ref()) else {
            return Some(base);
        };
        let transcript = feed.reducer.snapshot(seq, as_of_ms, None);
        Some(insight_merge::merge(
            &base,
            &transcript,
            Extras {
                model: feed.model.as_deref(),
                breakdown: feed
                    .breakdown
                    .as_ref()
                    .map(|(rows, at)| (rows.as_slice(), *at)),
                cut: feed.cut,
            },
        ))
    }
}

impl InsightHub {
    /// Where following the transcript of `session` stands.
    pub fn transcript_status(&self, session: SessionId) -> TranscriptStatus {
        lock(&self.state)
            .tails
            .get(&session)
            .map_or(TranscriptStatus::NotLooked, |tail| tail.status.clone())
    }

    /// Starts the task that follows the transcript of `session` unless one runs. It ends by
    /// itself: when nobody has looked at the session for [`TAIL_GRACE`], when the session is
    /// not a terminal agent's, or when the user does not allow reading the transcript.
    fn start_tail(self: &Arc<Self>, session: SessionId) {
        if self.links.get().is_none() {
            return;
        }
        let spawn = {
            let mut state = lock(&self.state);
            let tail = state.tails.entry(session).or_insert_with(|| Tail::new(""));
            tail.demanded = Instant::now();
            !std::mem::replace(&mut tail.running, true)
        };
        if spawn {
            let hub = self.clone();
            tokio::spawn(async move {
                while hub.sync(session, None, Reason::Loop).await {
                    tokio::time::sleep(TAIL_POLL).await;
                }
            });
        }
    }

    /// The task of `session` ends. A tail that read nothing and means nothing is forgotten.
    fn park(&self, session: SessionId) -> bool {
        let mut state = lock(&self.state);
        let idle = state.tails.get_mut(&session).is_some_and(|tail| {
            tail.running = false;
            tail.file.is_none() && tail.feed.is_none() && tail.status == TranscriptStatus::NotLooked
        });
        if idle {
            state.tails.remove(&session);
        }
        false
    }

    /// The user did not allow reading the transcripts of `source` (or took it back): what was
    /// read is dropped, and nothing is opened.
    fn withdraw(self: &Arc<Self>, session: SessionId, agent: &str, source: Source) {
        let mut guard = lock(&self.state);
        let state = &mut *guard;
        let tail = state
            .tails
            .entry(session)
            .or_insert_with(|| Tail::new(agent));
        let had = tail.feed.take().is_some();
        tail.file = None;
        tail.agent = agent.to_owned();
        tail.status = TranscriptStatus::Off(source);
        if had {
            self.changed(state, session);
        }
    }

    /// One look at the transcript of the terminal session `session`: consent, finding the file,
    /// reading what was appended, folding it into the session's transcript reducer. Returns
    /// whether the task of a focused session goes on.
    async fn sync(
        self: &Arc<Self>,
        session: SessionId,
        given: Option<InsightTarget>,
        reason: Reason,
    ) -> bool {
        let Some(links) = self.links.get() else {
            return self.park(session);
        };
        let Some(history) = links.history.upgrade() else {
            return self.park(session);
        };
        let target = match given {
            Some(target) => target,
            None => match links.registry.insight_target(session).await {
                Ok(target) => target,
                Err(_) => {
                    // The session is gone.
                    lock(&self.state).tails.remove(&session);
                    return false;
                }
            },
        };
        if !target.terminal {
            return self.park(session);
        }
        let Some(source) = transcript_tail::source_of(&target.harness) else {
            return self.park(session);
        };
        let Some(access) = history.transcript_access(source).await else {
            self.withdraw(session, &target.harness, source);
            return self.park(session);
        };

        // What this look starts from.
        let (mut file, claimed, last_catalog) = {
            let mut guard = lock(&self.state);
            let state = &mut *guard;
            if reason == Reason::Loop && !state.tails.contains_key(&session) {
                return false;
            }
            let wanted = state.demanded(session);
            let claimed: HashSet<PathBuf> = state
                .tails
                .iter()
                .filter(|(id, _)| **id != session)
                .filter_map(|(_, tail)| tail.file.as_ref().map(|file| file.path.clone()))
                .collect();
            let tail = state
                .tails
                .entry(session)
                .or_insert_with(|| Tail::new(&target.harness));
            if tail.agent != target.harness {
                // Another agent in the same terminal.
                let running = tail.running;
                *tail = Tail::new(&target.harness);
                tail.running = running;
            }
            if reason == Reason::Loop && !wanted {
                tail.running = false;
                return false;
            }
            if tail.busy
                || (reason == Reason::Pull
                    && tail.last_read.is_some_and(|at| at.elapsed() < READ_GAP))
            {
                return true;
            }
            tail.busy = true;
            tail.last_read = Some(Instant::now());
            let taken = (tail.file.take(), claimed, tail.last_catalog);
            // The integrations' reducer exists (a transcript can be the only source), and is the
            // agent's: a chat's never is.
            state.tick += 1;
            let tick = state.tick;
            let current = state.entries.get(&session).is_some_and(|entry| {
                entry.reducer.is_terminal() && entry.reducer.harness() == target.harness
            });
            if !current {
                state.make_room(session);
                state.entries.insert(
                    session,
                    Entry::new(
                        InsightReducer::new_terminal(
                            session,
                            &target.harness,
                            target.cwd.as_deref(),
                        ),
                        target.model(),
                        tick,
                    ),
                );
            }
            taken
        };

        // Which file is it?
        let mut reset = false;
        let mut status: Option<TranscriptStatus> = None;
        let mut catalog_asked = false;
        if file
            .as_ref()
            .is_none_or(|held| rebind_wanted(held, &target.tail))
        {
            let resolve = |rows: Option<&[Candidate]>| {
                transcript_tail::resolve(&Lookup {
                    agent: &target.harness,
                    hints: &target.tail,
                    cwd: target.cwd.as_deref(),
                    root: &access.root,
                    rows,
                    claimed: &claimed,
                })
            };
            let mut resolution = resolve(None);
            if resolution == Resolution::NeedsCatalog
                && !last_catalog.is_some_and(|at| at.elapsed() < CATALOG_GAP)
            {
                catalog_asked = true;
                match access.sessions(source).await {
                    Ok(rows) => {
                        let rows: Vec<Candidate> = rows.iter().filter_map(Candidate::of).collect();
                        resolution = resolve(Some(&rows));
                    }
                    Err(error) => status = Some(TranscriptStatus::Unreadable(error.message)),
                }
            }
            match resolution {
                Resolution::Found { path, key, route } => {
                    if let Some(held) = file.as_mut().filter(|held| held.path == path) {
                        held.route = route;
                        held.key = key;
                    } else {
                        file = Some(TailFile::new(path, key, route, source));
                        reset = true;
                    }
                }
                Resolution::Missing(why) => {
                    reset |= file.take().is_some();
                    status = Some(TranscriptStatus::Searching(why));
                }
                Resolution::NeedsCatalog => {}
            }
        }

        // Read what was appended.
        let mut read = None;
        if let Some(mut open) = file.take() {
            match tokio::task::spawn_blocking(move || {
                let result = open.step(CATCH_UP_BYTES);
                (open, result)
            })
            .await
            {
                Ok((open, result)) => {
                    file = Some(open);
                    read = Some(result);
                }
                Err(error) => {
                    warn!(%session, %error, "reading a transcript failed");
                    status = Some(TranscriptStatus::Unreadable("the read failed".to_owned()));
                }
            }
        }

        // Fold it in.
        let mut guard = lock(&self.state);
        let state = &mut *guard;
        let Some(tail) = state.tails.get_mut(&session) else {
            // Evicted meanwhile.
            return false;
        };
        tail.busy = false;
        if catalog_asked {
            tail.last_catalog = Some(Instant::now());
        }
        let mut changed = false;
        if reset {
            changed |= tail.feed.take().is_some();
        }
        tail.file = file;
        let found = tail.file.as_ref().map(|open| open.route);
        match (read, found) {
            (Some(Ok(step)), Some(route)) => {
                if step.rotated {
                    changed |= tail.feed.take().is_some();
                }
                let cut = tail.file.as_ref().is_some_and(|open| open.cut);
                tail.status = TranscriptStatus::Reading { route, cut };
                if !step.events.is_empty() {
                    let mut folded = Folded::default();
                    transcript_fold::fold(step.events, now_ms(), &mut folded);
                    let feed = tail.feed.get_or_insert_with(|| {
                        Feed::new(session, &target.harness, target.cwd.as_deref())
                    });
                    for (report, at) in &folded.reports {
                        changed |= feed.reducer.apply_report(report, *at);
                    }
                    if let Some(model) = folded.model
                        && feed.model.as_ref() != Some(&model)
                    {
                        feed.model = Some(model);
                        changed = true;
                    }
                    if let Some(breakdown) = folded.breakdown {
                        feed.breakdown = Some(breakdown);
                        changed = true;
                    }
                    feed.cut = cut;
                }
            }
            (Some(Err(error)), Some(route)) if error.kind() == std::io::ErrorKind::NotFound => {
                tail.status = TranscriptStatus::Waiting(route);
            }
            (Some(Err(error)), _) => {
                tail.status = TranscriptStatus::Unreadable(error.to_string());
            }
            (_, _) => {
                if let Some(status) = status {
                    tail.status = status;
                }
            }
        }
        let ended = reason == Reason::Loop && !target.tail.live;
        if ended {
            tail.running = false;
        }
        if changed {
            self.changed(state, session);
        }
        !ended
    }
}

/// What `harness` can measure, for an error message.
pub fn measure_unsupported(harness: &str) -> ErrorBody {
    let facts = harness_insight(harness);
    debug_assert!(!facts.can_measure);
    ErrorBody::new(
        ErrorCode::Unsupported,
        format!("{harness} has no command that prints its context by category"),
    )
}

#[cfg(test)]
mod tests {
    use hutan_core::agent::{
        AgentEventKind, Delivery, FileChange, FileChangeKind, ProviderIds, TokenCounts, ToolKind,
        ToolStatus,
    };
    use hutan_core::{Epoch, ItemId};

    use super::*;
    use crate::bus::RingLimits;

    fn hub() -> (tempfile::TempDir, Arc<InsightHub>) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("hutan.db")).unwrap();
        let bus = Arc::new(EventBus::new(Epoch::new(), RingLimits::default()));
        (dir, InsightHub::new(storage, bus))
    }

    fn target() -> InsightTarget {
        InsightTarget {
            harness: "omp".into(),
            model: Some("claude-haiku-5-5".into()),
            provider: Some("anthropic".into()),
            cwd: Some("/work/app".into()),
            assistant: false,
            terminal: false,
            tail: TailHints::default(),
        }
    }

    fn event(session: SessionId, seq: u64, turn: TurnId, kind: AgentEventKind) -> AgentEvent {
        AgentEvent {
            session_id: session,
            seq,
            ts_ms: 1_000_000 + seq,
            delivery: Delivery::Live,
            turn_id: Some(turn),
            item_id: Some(ItemId::new()),
            provider: ProviderIds::default(),
            kind,
            payload: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn at_most_sixty_four_sessions_are_tracked_and_the_least_recently_used_goes() {
        let (_dir, hub) = hub();
        let kept = SessionId::new();
        hub.snapshot(kept, &target()).await.unwrap();
        for _ in 0..MAX_SESSIONS + 6 {
            hub.snapshot(SessionId::new(), &target()).await.unwrap();
            // Asked for again and again: it stays.
            hub.snapshot(kept, &target()).await.unwrap();
        }
        assert_eq!(hub.tracked(), MAX_SESSIONS);
        assert!(hub.is_tracked(kept));
    }

    /// A session at every cap holds what the budget says: 200 turns, 100 files with long paths,
    /// 17 tool rows. (The spike's 48 KiB counted a turn as 96 bytes; a turn is 16 optional
    /// numbers, so the whole is about 85 KiB and 64 sessions stay under 6 MiB.)
    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_at_every_cap_stays_within_its_memory_budget() {
        let (_dir, hub) = hub();
        let session = SessionId::new();
        hub.attach(session, &target(), true).await;
        let mut seq = 0;
        let mut next = || {
            seq += 1;
            seq
        };
        for turn_n in 0..300 {
            let turn = TurnId::new();
            hub.observe(&event(session, next(), turn, AgentEventKind::TurnStarted));
            for tool in 0..3 {
                let name = format!("tool{}", (turn_n + tool) % 25);
                let item = ItemId::new();
                for kind in [
                    AgentEventKind::ToolStart {
                        tool: ToolKind::Command,
                        name,
                        title: String::new(),
                        input: None,
                    },
                    AgentEventKind::ToolEnd {
                        tool: ToolKind::Command,
                        status: ToolStatus::Completed,
                        output: None,
                        truncated: false,
                        exit_code: None,
                        duration_ms: Some(5),
                    },
                ] {
                    let mut e = event(session, next(), turn, kind);
                    e.item_id = Some(item);
                    hub.observe(&e);
                }
            }
            let long = format!("/work/app/{}{turn_n}.rs", "d/".repeat(110));
            hub.observe(&event(
                session,
                next(),
                turn,
                AgentEventKind::FileDiff {
                    changes: vec![FileChange {
                        path: long,
                        kind: FileChangeKind::Update,
                        move_path: None,
                        patch: Some("+x\n".into()),
                        before: None,
                        after: None,
                    }],
                    patch: None,
                },
            ));
            let counts = TokenCounts {
                input_tokens: Some(1),
                total_tokens: Some(1),
                ..TokenCounts::default()
            };
            hub.observe(&event(
                session,
                next(),
                turn,
                AgentEventKind::Usage {
                    total: None,
                    last: Some(counts),
                    context_window: None,
                },
            ));
            hub.observe(&event(
                session,
                next(),
                turn,
                AgentEventKind::TurnCompleted {
                    duration_ms: Some(1),
                },
            ));
        }
        let insight = hub.snapshot(session, &target()).await.unwrap();
        assert!(insight.turns.len() <= MAX_TURNS);
        // 100 paths of 240 bytes do not fit a snapshot together with the turns: the wire cuts
        // the files (and says so), the reducer keeps them all.
        let files = insight.files.as_ref().expect("files");
        assert!(files.items.len() <= 100 && (files.items.len() == 100 || files.truncated));
        assert_eq!(insight.tools.as_ref().map(|t| t.by_name.len()), Some(17));
        let held = hub.bytes();
        assert!(held < 100 * 1024, "{held} bytes");
        assert!(
            serde_json::to_vec(&insight).unwrap().len() <= hutan_core::insight::MAX_SNAPSHOT_BYTES
        );
        eprintln!("insight: one session at every cap holds about {held} bytes");
    }

    fn followed(route: Route, key: Option<&str>, path: &str) -> TailFile {
        TailFile::new(
            PathBuf::from(path),
            key.map(str::to_owned),
            route,
            Source::Claude,
        )
    }

    fn hints(id: Option<&str>, path: Option<&str>) -> TailHints {
        TailHints {
            agent_session_id: id.map(str::to_owned),
            transcript_path: path.map(str::to_owned),
            ..TailHints::default()
        }
    }

    #[test]
    fn a_transcript_is_followed_until_the_agent_names_another_conversation() {
        let file = followed(Route::SessionId, Some("a"), "/r/a.jsonl");
        assert!(!rebind_wanted(&file, &hints(Some("a"), None)));
        assert!(!rebind_wanted(&file, &hints(Some("a"), Some("/r/a.jsonl"))));
        // `/clear`: another id, another file.
        assert!(rebind_wanted(&file, &hints(Some("b"), None)));
        assert!(rebind_wanted(&file, &hints(None, Some("/r/b.jsonl"))));
        // Nothing said: nothing to change.
        assert!(!rebind_wanted(&file, &hints(None, None)));
        // A guess by folder gives way to a certain route as soon as the agent names one.
        let guess = followed(Route::Folder, Some("a"), "/r/a.jsonl");
        assert!(!rebind_wanted(&guess, &hints(None, None)));
        assert!(rebind_wanted(&guess, &hints(Some("a"), None)));
    }

    #[test]
    fn the_status_says_in_words_why_a_transcript_is_or_is_not_read() {
        assert!(
            TranscriptStatus::Off(Source::Claude)
                .text()
                .contains("Claude Code history is off")
        );
        assert!(
            TranscriptStatus::Searching(Why::Rivals)
                .text()
                .contains("another session runs the same agent")
        );
        let reading = TranscriptStatus::Reading {
            route: Route::Folder,
            cut: true,
        };
        assert!(reading.is_reading());
        assert!(reading.text().contains("likely") && reading.text().contains("only the end"));
        assert!(!TranscriptStatus::NotLooked.is_reading());
    }
}
