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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use hutan_core::checkpoint::{CheckpointKind, CheckpointState};
use hutan_core::insight::{
    AgentInsight, InsightModel, InsightReducer, MAX_TURNS, SeedTurn, harness_insight,
};
use hutan_core::{AgentEvent, SessionId, TurnId};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::Event;
use hutan_protocol::{ControlMessage, MAX_PAYLOAD};
use hutan_storage::{CheckpointFilter, Storage};
use tracing::{debug, warn};

use crate::bus::EventBus;
use crate::conn::{ConnHandle, ConnId, LocalSend};
use crate::registry::now_ms;

/// Journal events a reducer is built from (the newest, of the kinds insight reads).
pub const COLD_EVENTS: u32 = 5_000;
/// Sessions with a reducer; the least recently used goes first.
pub const MAX_SESSIONS: usize = 64;
/// At least this long between two `agent.insight_updated` of one session.
pub const MIN_GAP: Duration = Duration::from_secs(1);
/// Events and costs kept while a reducer is being built; past this the build is read again.
const MAX_BUFFERED: usize = 4_096;

/// What the registry knows of a chat that insight needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsightTarget {
    /// `codex`, `omp`, `claude` or `direct` (an assistant that runs on a provider is `direct`).
    pub harness: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub cwd: Option<String>,
    /// The session is an assistant (Mawas): it has insight, but `/context` is not sent to it.
    pub assistant: bool,
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
}

#[derive(Debug)]
pub struct InsightHub {
    storage: Storage,
    bus: Arc<EventBus>,
    state: Mutex<State>,
    /// Builds are rare and read the journal: one at a time.
    building: tokio::sync::Mutex<()>,
    seq: AtomicU64,
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
        })
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

    /// The session is gone for good (deleted, or its journal was replaced).
    pub fn forget(&self, session: SessionId) {
        let mut state = lock(&self.state);
        state.entries.remove(&session);
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
        let _one_at_a_time = self.building.lock().await;
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
            if state.entries.len() >= MAX_SESSIONS && !state.entries.contains_key(&session) {
                let focused: Vec<SessionId> = state.focus.values().map(|f| f.session).collect();
                let victim = state
                    .entries
                    .iter()
                    .filter(|(id, _)| !focused.contains(id))
                    .min_by_key(|(_, entry)| entry.touched)
                    .map(|(id, _)| *id)
                    .or_else(|| {
                        state
                            .entries
                            .iter()
                            .min_by_key(|(_, entry)| entry.touched)
                            .map(|(id, _)| *id)
                    });
                if let Some(victim) = victim {
                    state.entries.remove(&victim);
                }
            }
            state.entries.insert(
                session,
                Entry {
                    reducer,
                    model: target.model(),
                    touched: tick,
                    dirty: false,
                    armed: false,
                    last_sent: None,
                },
            );
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
            entry
                .reducer
                .snapshot(self.next_seq(), now_ms(), entry.model.clone())
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

    /// `client.focus`: `conn` looks at `session` (`None`: at none).
    pub fn focus(&self, conn: &ConnHandle, session: Option<SessionId>) {
        let mut state = lock(&self.state);
        match session {
            Some(session) => {
                state.focus.insert(
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
            }
            None => {
                state.focus.remove(&conn.id());
            }
        }
    }

    /// A connection closed: its focus goes with it.
    pub fn connection_closed(&self, conn: ConnId) {
        lock(&self.state).focus.remove(&conn);
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
            (
                entry
                    .reducer
                    .snapshot(self.next_seq(), now_ms(), entry.model.clone()),
                targets,
            )
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
}
