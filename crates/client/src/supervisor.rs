//! The task behind [`Connection`](crate::Connection): connect, synchronise, serve, reconnect.
//!
//! One task owns the [`StoreState`] and every terminal attachment, so no locks guard them and
//! all updates keep a single order. It runs on the connection's own runtime thread
//! (`connection.rs`), never on a caller's executor.
//!
//! Synchronisation follows ADR 0002:
//! * `resume: replayed` — the daemon already queued the missed events behind the welcome. An
//!   `events.subscribe` round trip is the barrier that proves they have all arrived.
//! * `snapshot_required` (or a lagging event receiver) — `events.subscribe`, then
//!   `session.snapshot`; events that arrived meanwhile are applied only if `seq` is newer than
//!   the snapshot's.
//!
//! Events at or below the last applied seq are skipped everywhere, so replay never duplicates.

use std::collections::HashMap;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use hutan_core::{Epoch, Seq, SessionId, TerminalId};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ApprovalList, ApprovalListRequest, QuestionList, QuestionListRequest};
use hutan_protocol::messages::{
    Empty, Event, Request, ResumeOutcome, ResumePoint, SessionInfo, SessionSnapshot, Subscribed,
    TerminalAck, TerminalAttach, TerminalAttached, TerminalColors, TerminalRef, TerminalResize,
    TerminalSetColors, TerminalSize,
};
use hutan_protocol::render::TerminalRender;
use hutan_protocol::tree::{AgentTree, AgentTreeRequest};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::chat::ChatUpdate;
use crate::client::{
    AUTOSTART_GIVE_UP, AutostartThrottle, COLORS_CAPABILITY, Client, ConnectOptions, Resumed,
    is_unreachable,
};
use crate::connection::Shared;
use crate::error::ClientError;
use crate::identity::DaemonIdentity;
use crate::socket::default_socket_path;
use crate::start_failure::{DaemonFiles, StartFailure};
use crate::store::{ConnectionState, Effects, Notice, StoreState};
use crate::terminal::{Command, GoneReason, TerminalUpdate};

const BACKOFF_START: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Upper bound for synchronising after a handshake (two round trips plus the event backlog).
const SYNC_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound for one terminal round trip; a daemon slower than this counts as gone.
const STEP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a re-attach waits for the previous forwarder to drain and announce `Reconnecting`.
const FORWARDER_GRACE: Duration = Duration::from_secs(1);
/// Events folded into one published state.
const MAX_BATCH: usize = 256;
/// Snapshot attempts before giving up on an event stream that keeps lagging.
const MAX_SYNC_ATTEMPTS: usize = 3;

/// Delay before reconnect attempt number `attempt` (1-based): 100 ms doubling to 5 s, ±20 %.
fn backoff(attempt: u32) -> Duration {
    let doublings = attempt.saturating_sub(1).min(16);
    let base = BACKOFF_START
        .saturating_mul(1 << doublings)
        .min(BACKOFF_MAX);
    base.mul_f64(0.8 + 0.4 * unit_random())
}

/// Uniform in `[0, 1)` from the std hasher's per-instance random keys.
fn unit_random() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let bits = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    // 53 random bits are exactly representable in an f64 mantissa.
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

enum Served {
    Stopped,
    /// The connection ended; `connected` tells whether it ever completed synchronisation.
    Lost {
        connected: bool,
    },
}

#[derive(Clone, Copy)]
enum Mode {
    Replay,
    Snapshot,
}

struct Synced {
    effects: Effects,
    /// Epoch of the previous daemon when it differs from the one just connected to.
    old_epoch: Option<Epoch>,
    epoch: Epoch,
}

enum Drain {
    Events(Vec<(Seq, Event)>),
    Lagged,
    Closed,
}

fn drain(events: &mut broadcast::Receiver<(Seq, Event)>) -> Drain {
    let mut batch = Vec::new();
    loop {
        match events.try_recv() {
            Ok(event) => batch.push(event),
            Err(broadcast::error::TryRecvError::Empty) => return Drain::Events(batch),
            Err(broadcast::error::TryRecvError::Lagged(_)) => return Drain::Lagged,
            Err(broadcast::error::TryRecvError::Closed) => return Drain::Closed,
        }
    }
}

struct Live {
    stop: CancellationToken,
    forwarder: JoinHandle<()>,
}

struct Attachment {
    handle: u64,
    updates: mpsc::Sender<TerminalUpdate>,
    size: Option<TerminalSize>,
    /// The colors last set through the handle, sent again after every re-attach.
    colors: Option<TerminalColors>,
    /// The session known to run this terminal, once the state has seen it.
    session: Option<SessionId>,
    live: Option<Live>,
}

impl Attachment {
    fn stop(&self) {
        if let Some(live) = &self.live {
            live.stop.cancel();
        }
    }
}

/// Moves renders from one connection's sink to the handle, preserving order and applying the
/// handle's back pressure. When the sink ends without being stopped, the connection was lost
/// (after everything buffered has been forwarded): the handle is told.
async fn forward(
    mut renders: mpsc::Receiver<TerminalRender>,
    updates: mpsc::Sender<TerminalUpdate>,
    stop: CancellationToken,
) {
    loop {
        let next = tokio::select! {
            biased;
            _ = stop.cancelled() => return,
            next = renders.recv() => next,
        };
        let Some(render) = next else { break };
        let sent = tokio::select! {
            biased;
            _ = stop.cancelled() => return,
            sent = updates.send(TerminalUpdate::Render(render)) => sent,
        };
        if sent.is_err() {
            return;
        }
    }
    if updates.try_send(TerminalUpdate::Reconnecting).is_err() {
        debug!("could not tell a terminal handle about the reconnect (queue full or handle gone)");
    }
}

#[derive(Default)]
struct Terminals {
    map: HashMap<TerminalId, Attachment>,
}

impl Terminals {
    /// Registers a handle, replacing (and ending) an earlier one for the same terminal.
    fn register(
        &mut self,
        handle: u64,
        terminal: TerminalId,
        size: Option<TerminalSize>,
        updates: mpsc::Sender<TerminalUpdate>,
        sessions: &[SessionInfo],
    ) {
        let previous = self.map.insert(
            terminal,
            Attachment {
                handle,
                updates,
                size,
                session: sessions
                    .iter()
                    .find(|s| s.terminal_id == Some(terminal))
                    .map(|s| s.id),
                colors: None,
                live: None,
            },
        );
        if let Some(previous) = previous {
            previous.stop();
        }
    }

    /// Learns the owning session of attachments that were made before the state knew it.
    fn bind(&mut self, sessions: &[SessionInfo]) {
        for (terminal, attachment) in &mut self.map {
            if attachment.session.is_none() {
                attachment.session = sessions
                    .iter()
                    .find(|s| s.terminal_id == Some(*terminal))
                    .map(|s| s.id);
            }
        }
    }

    /// One session update: a session that owns an attached terminal and now runs another one
    /// was restarted. (Updates without a terminal, as published at the start of a restart,
    /// change nothing.)
    fn observe(&mut self, session: SessionId, terminal: Option<TerminalId>) {
        let Some(new_terminal) = terminal else {
            return;
        };
        let restarted: Vec<TerminalId> = self
            .map
            .iter_mut()
            .filter_map(|(attached, attachment)| {
                if attachment.session.is_none() && new_terminal == *attached {
                    attachment.session = Some(session);
                    return None;
                }
                (attachment.session == Some(session) && new_terminal != *attached)
                    .then_some(*attached)
            })
            .collect();
        for old in restarted {
            self.gone(old, GoneReason::SessionRestarted { new_terminal });
        }
    }

    fn owns(&self, handle: u64, terminal: TerminalId) -> bool {
        self.map.get(&terminal).is_some_and(|a| a.handle == handle)
    }

    /// Removes the attachment if `handle` still owns it.
    fn detach(&mut self, handle: u64, terminal: TerminalId) -> bool {
        if !self.owns(handle, terminal) {
            return false;
        }
        if let Some(attachment) = self.map.remove(&terminal) {
            attachment.stop();
        }
        true
    }

    /// Ends the attachment with a final `Gone` update.
    fn gone(&mut self, terminal: TerminalId, reason: GoneReason) {
        let Some(attachment) = self.map.remove(&terminal) else {
            return;
        };
        attachment.stop();
        // Delivered by its own task so a handle with a full queue cannot stall the connection.
        tokio::spawn(async move {
            let _ = attachment
                .updates
                .send(TerminalUpdate::Gone { reason })
                .await;
        });
    }

    fn gone_all(&mut self, reason: GoneReason) {
        let all: Vec<TerminalId> = self.map.keys().copied().collect();
        for terminal in all {
            self.gone(terminal, reason);
        }
    }

    /// Ends every attachment without a final update (the connection itself is going away).
    fn close_all(&mut self) {
        for (_, attachment) in self.map.drain() {
            attachment.stop();
        }
    }

    /// Handles a command while no connection is up: registrations and the remembered size
    /// survive for the next attach; everything that needs the daemon is dropped.
    fn offline(&mut self, command: Command, sessions: &[SessionInfo]) {
        match command {
            Command::Attach {
                handle,
                terminal,
                size,
                updates,
            } => self.register(handle, terminal, size, updates, sessions),
            Command::Detach { handle, terminal } => {
                self.detach(handle, terminal);
            }
            Command::Ack { .. } => debug!("ack ignored while disconnected"),
            Command::Input { terminal, .. } => {
                warn!(%terminal, "terminal input dropped while disconnected");
            }
            Command::Resize {
                handle,
                terminal,
                size,
            } => {
                if let Some(attachment) = self.map.get_mut(&terminal)
                    && attachment.handle == handle
                {
                    attachment.size = Some(size);
                }
            }
            Command::SetColors {
                handle,
                terminal,
                colors,
            } => {
                if let Some(attachment) = self.map.get_mut(&terminal)
                    && attachment.handle == handle
                {
                    attachment.colors = Some(colors);
                }
            }
        }
    }
}

pub(crate) struct Supervisor {
    opts: ConnectOptions,
    shared: Arc<Shared>,
    commands: mpsc::Receiver<Command>,
    cancel: CancellationToken,
    state: StoreState,
    /// Seq of the last event folded into `state` (within `state.epoch`).
    applied: Seq,
    terminals: Terminals,
    throttle: AutostartThrottle,
    /// Where the daemon behind the socket leaves its start-error record.
    files: Option<DaemonFiles>,
}

impl Supervisor {
    pub(crate) fn new(
        opts: ConnectOptions,
        shared: Arc<Shared>,
        commands: mpsc::Receiver<Command>,
        cancel: CancellationToken,
    ) -> Self {
        let files = opts
            .socket
            .clone()
            .or_else(default_socket_path)
            .map(|socket| DaemonFiles::for_socket(&socket));
        Self {
            opts,
            shared,
            commands,
            cancel,
            state: StoreState::initial(),
            applied: Seq::default(),
            terminals: Terminals::default(),
            throttle: AutostartThrottle::default(),
            files,
        }
    }

    /// Connects and keeps the connection alive until cancelled.
    pub(crate) async fn run(mut self) {
        let mut attempt = 0u32;
        loop {
            let resume = self.state.epoch.map(|epoch| ResumePoint {
                epoch,
                seq: self.applied,
            });
            match self.dial(resume).await {
                None => break,
                Some(Ok(resumed)) => {
                    self.throttle.succeeded();
                    self.state.daemon_start = None;
                    self.state.autostart_halted = false;
                    match self.serve(resumed).await {
                        Served::Stopped => break,
                        Served::Lost { connected } => {
                            if connected {
                                attempt = 0;
                            }
                            attempt += 1;
                        }
                    }
                }
                Some(Err(error)) => {
                    debug!(%error, "connecting to hutand failed");
                    self.throttle.failed(&error);
                    if self.throttle.halted() && !self.state.autostart_halted {
                        warn!(
                            %error,
                            "hutand failed to start the same way {AUTOSTART_GIVE_UP} times; not \
                             starting it again until asked"
                        );
                    }
                    self.state.autostart_halted = self.throttle.halted();
                    self.note_failure(error).await;
                    attempt += 1;
                }
            }
            self.shared.set_client(None);
            let retry_in = backoff(attempt);
            self.set_connection(ConnectionState::Reconnecting { attempt, retry_in });
            if !self.idle(retry_in).await {
                break;
            }
        }
        self.finish();
    }

    /// Marks the connection stopped and releases everything. Also the exit path when the
    /// runtime cannot be built.
    pub(crate) fn finish(mut self) {
        self.shared.set_client(None);
        self.terminals.close_all();
        self.set_connection(ConnectionState::Stopped);
    }

    fn publish(&self) {
        self.shared.state.send_if_modified(|current| {
            if **current == self.state {
                return false;
            }
            *current = Arc::new(self.state.clone());
            true
        });
    }

    fn set_connection(&mut self, connection: ConnectionState) {
        self.state.connection = connection;
        self.publish();
    }

    /// Keeps [`StoreState::daemon_start`] current after a failed dial: an autostart's failure
    /// as it is; otherwise, while nothing answers, the record a daemon left (a record that is
    /// gone takes its failure with it, one without a record stays until something replaces it).
    async fn note_failure(&mut self, error: ClientError) {
        let unreachable = match error {
            ClientError::DaemonStart(failure) => {
                self.state.daemon_start = Some(*failure);
                return;
            }
            ClientError::Io(error) => is_unreachable(&error),
            _ => false,
        };
        let Some(files) = self.files.clone().filter(|_| unreachable) else {
            return;
        };
        let recorded = tokio::task::spawn_blocking(move || StartFailure::recorded(&files))
            .await
            .ok()
            .flatten();
        let current = self.state.daemon_start.as_ref();
        match recorded {
            Some(recorded)
                if current.and_then(|c| c.record.as_ref()) != recorded.record.as_ref() =>
            {
                self.state.daemon_start = Some(recorded);
            }
            Some(_) => {}
            None if current.is_some_and(|c| c.record.is_some()) => self.state.daemon_start = None,
            None => {}
        }
    }

    /// `None` when cancelled.
    async fn dial(&mut self, resume: Option<ResumePoint>) -> Option<Result<Resumed, ClientError>> {
        let throttle = &mut self.throttle;
        let enabled = &self.shared.autostart;
        let connect = Client::connect_gated(self.opts.clone(), resume, || {
            enabled.load(Ordering::Acquire) && throttle.permit()
        });
        let mut connect = pin!(connect);
        loop {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return None,
                result = &mut connect => return Some(result),
                Some(command) = self.commands.recv() => {
                    self.terminals.offline(command, &self.state.sessions);
                }
            }
        }
    }

    /// Waits out a backoff delay; `false` when cancelled. A [`Connection::set_autostart`]
    /// that re-enables autostart ends the wait early and lifts the autostart interval.
    async fn idle(&mut self, delay: Duration) -> bool {
        let mut timer = pin!(sleep(delay));
        loop {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return false,
                () = &mut timer => return true,
                () = self.shared.kick.notified() => {
                    // The user asked for a start (or allowed one again): now, from scratch.
                    self.throttle.reset();
                    self.state.autostart_halted = false;
                    self.state.stopped_by = None;
                    return true;
                }
                Some(command) = self.commands.recv() => {
                    self.terminals.offline(command, &self.state.sessions);
                }
            }
        }
    }

    async fn serve(&mut self, resumed: Resumed) -> Served {
        let Resumed { client, mut events } = resumed;
        let epoch = client.epoch();
        let mode = match client.welcome().resume {
            ResumeOutcome::Replayed if self.state.epoch == Some(epoch) => Mode::Replay,
            _ => Mode::Snapshot,
        };
        let cancel = self.cancel.clone();
        let synced = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Served::Stopped,
            synced = timeout(SYNC_TIMEOUT, self.sync(&client, &mut events, mode)) => synced,
        };
        let synced = match synced {
            Ok(Ok(synced)) => synced,
            Ok(Err(error)) => {
                warn!(%error, "synchronising with hutand failed");
                return Served::Lost { connected: false };
            }
            Err(_) => {
                warn!("synchronising with hutand timed out");
                return Served::Lost { connected: false };
            }
        };
        self.shared.set_client(Some(client.clone()));
        // A daemon answers: whatever stop was announced is over.
        self.state.daemon = Some(DaemonIdentity::of(client.welcome()));
        self.state.stopped_by = None;
        self.complete_sync(synced);
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Served::Stopped,
            served = self.connected(&client, &mut events) => served,
        }
    }

    /// Brings `state` up to date with the daemon. Changes `state` only at the very end, with
    /// no await in between, so a cancelled or failed attempt leaves it untouched.
    async fn sync(
        &mut self,
        client: &Client,
        events: &mut broadcast::Receiver<(Seq, Event)>,
        mode: Mode,
    ) -> Result<Synced, ClientError> {
        let epoch = client.epoch();
        let old_epoch = self.state.epoch.filter(|old| *old != epoch);
        let mut replay = matches!(mode, Mode::Replay);
        for _ in 0..MAX_SYNC_ATTEMPTS {
            // Subscribing is idempotent. After a replayed resume it is only the barrier: the
            // replay was queued before this request, so its events are in `events` by the
            // time the answer arrives.
            client
                .call::<Subscribed>(Request::EventsSubscribe(Empty {}))
                .await?;
            if replay {
                match drain(events) {
                    Drain::Events(batch) => {
                        let effects = self.fold(batch);
                        return Ok(Synced {
                            effects,
                            old_epoch,
                            epoch,
                        });
                    }
                    Drain::Lagged => {
                        replay = false;
                        continue;
                    }
                    Drain::Closed => return Err(ClientError::Disconnected),
                }
            }
            let snapshot: SessionSnapshot = client.call(Request::SessionSnapshot(Empty {})).await?;
            let approvals = if client.supports_chat() {
                let listed: ApprovalList = client
                    .call(Request::ApprovalList(ApprovalListRequest::default()))
                    .await?;
                Some(listed.approvals)
            } else {
                None
            };
            let questions = if client.supports_questions() {
                let listed: QuestionList = client
                    .call(Request::QuestionList(QuestionListRequest::default()))
                    .await?;
                Some(listed.questions)
            } else {
                None
            };
            let trees = if client.supports_agent_tree() {
                Some(
                    client
                        .call::<AgentTree>(Request::AgentTree(AgentTreeRequest::default()))
                        .await?,
                )
            } else {
                None
            };
            match drain(events) {
                Drain::Events(batch) => {
                    if old_epoch.is_some() {
                        self.state.clear();
                    }
                    let seq = snapshot.seq;
                    let mut effects = self.state.install_snapshot(snapshot);
                    if let Some(approvals) = approvals {
                        self.state.install_approvals(approvals);
                    }
                    if let Some(questions) = questions {
                        self.state.install_questions(questions);
                    }
                    if let Some(trees) = trees {
                        self.state.install_trees(trees.observations);
                    }
                    self.applied = seq;
                    effects.extend(self.fold(batch));
                    if old_epoch.is_some() {
                        // Every handle ends with `EpochChanged` instead.
                        effects.session_terminals.clear();
                        effects.removed.clear();
                    }
                    return Ok(Synced {
                        effects,
                        old_epoch,
                        epoch,
                    });
                }
                Drain::Lagged => {}
                Drain::Closed => return Err(ClientError::Disconnected),
            }
        }
        Err(ClientError::Handshake(
            "event stream kept lagging during synchronisation".into(),
        ))
    }

    /// Folds events newer than `applied` into `state`.
    fn fold(&mut self, batch: Vec<(Seq, Event)>) -> Effects {
        let mut effects = Effects::default();
        for (seq, event) in batch {
            // Outside the sequence: it carries the seq of the last retained event, which is
            // `applied` already, so the check below would drop every sample.
            if event.is_connection_local() {
                effects.extend(self.state.apply(event));
                continue;
            }
            if seq <= self.applied {
                continue;
            }
            self.applied = seq;
            effects.extend(self.state.apply(event));
        }
        effects
    }

    /// Publishes the synchronised state, then announces what changed.
    fn complete_sync(&mut self, synced: Synced) {
        let Synced {
            effects,
            old_epoch,
            epoch,
        } = synced;
        self.state.epoch = Some(epoch);
        self.state.connection = ConnectionState::Connected;
        self.publish();
        // Before the replayed events: a consumer reloads the journal, then folds those.
        let _ = self.shared.chat.send(ChatUpdate::Resync { epoch });
        if let Some(old) = old_epoch {
            info!(%old, new = %epoch, "hutand restarted; terminals are gone");
            let _ = self
                .shared
                .notices
                .send(Notice::DaemonRestarted { old, new: epoch });
            self.terminals.gone_all(GoneReason::EpochChanged);
        }
        self.dispatch(effects);
    }

    /// Notices and terminal consequences of folded events; always after `publish`.
    fn dispatch(&mut self, effects: Effects) {
        if let Some(by) = self.state.stopped_by
            && self.shared.autostart.swap(false, Ordering::AcqRel)
        {
            // Before the connection drops: nothing may bring this daemon back by itself.
            info!(
                ?by,
                "hutand is stopping on request; not starting it again until asked"
            );
        }
        for notice in effects.notices {
            // No receivers is fine.
            let _ = self.shared.notices.send(notice);
        }
        for event in effects.chat {
            // No receivers is fine.
            let _ = self.shared.chat.send(ChatUpdate::Event(Arc::new(event)));
        }
        for queue in effects.queues {
            // No receivers is fine.
            let _ = self.shared.chat.send(ChatUpdate::Queue(Arc::new(queue)));
        }
        for update in effects.checkpoints {
            // No receivers is fine.
            let _ = self
                .shared
                .chat
                .send(ChatUpdate::Checkpoint(Arc::new(update)));
        }
        self.terminals.bind(&self.state.sessions);
        for (session, terminal) in effects.session_terminals {
            self.terminals.observe(session, terminal);
        }
        for terminal in effects.removed {
            self.terminals.gone(terminal, GoneReason::NotFound);
        }
    }

    /// Serves one synchronised connection until it ends. Never returns `Stopped` itself.
    async fn connected(
        &mut self,
        client: &Client,
        events: &mut broadcast::Receiver<(Seq, Event)>,
    ) -> Served {
        let attached: Vec<TerminalId> = self.terminals.map.keys().copied().collect();
        for terminal in attached {
            if let Err(error) = self.attach_now(client, terminal).await {
                debug!(%error, "re-attaching failed; connection lost");
                return Served::Lost { connected: true };
            }
        }
        if !self.resubscribe_resources(client).await {
            return Served::Lost { connected: true };
        }
        if !self.rewatch_files(client).await {
            return Served::Lost { connected: true };
        }
        loop {
            tokio::select! {
                biased;
                event = events.recv() => match event {
                    Ok(first) => {
                        if !self.apply_events(first, events, client).await {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        warn!(missed, "event stream lagged; resynchronising from a snapshot");
                        if !self.resync(client, events).await {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                Some(command) = self.commands.recv() => {
                    if let Err(error) = self.online(client, command).await {
                        debug!(%error, "terminal command failed; connection lost");
                        break;
                    }
                }
            }
        }
        Served::Lost { connected: true }
    }

    /// Watches belong to their connection too: a reconnect watches the files again
    /// (`Connection::fs_watch`) and announces an empty `fs.changed` with `overflow`, since what
    /// changed while the connection was away was never sent. `false` if the connection is gone.
    async fn rewatch_files(&mut self, client: &Client) -> bool {
        let watched = self.shared.watched_files();
        if watched.is_empty() || !client.supports_fs_watch() {
            return true;
        }
        for picked in [false, true] {
            let paths: Vec<hutan_protocol::fs::FsWatchPath> = watched
                .iter()
                .filter(|(_, p)| *p == picked)
                .map(|(path, _)| hutan_protocol::fs::FsWatchPath {
                    path: path.clone(),
                    recursive: false,
                })
                .collect();
            if paths.is_empty() {
                continue;
            }
            match client
                .fs_watch(hutan_protocol::fs::FsWatch { paths, picked })
                .await
            {
                Ok(_) => {}
                // A file gone or no longer allowed: the next check of the files says so.
                Err(ClientError::Remote(error)) => {
                    warn!(message = %error.message, "fs.watch refused after a reconnect");
                }
                Err(error) => {
                    debug!(%error, "watching the files again failed; connection lost");
                    return false;
                }
            }
        }
        let _ = self.shared.notices.send(Notice::FsChanged {
            changes: Vec::new(),
            overflow: true,
        });
        true
    }

    /// A subscription belongs to its connection: a reconnect asks again while somebody wants
    /// resource samples (`Connection::resources_subscribe`), and the answer's sample is
    /// announced like the ones that follow. `false` if the connection is gone.
    async fn resubscribe_resources(&mut self, client: &Client) -> bool {
        let Some(interval) = self.shared.resources_interval() else {
            return true;
        };
        match client.resources_subscribe(Some(interval)).await {
            Ok(subscribed) => {
                // Unsubscribed meanwhile: the request above must not outlive the wish.
                if self.shared.resources_interval().is_none() {
                    let _ = client.resources_unsubscribe().await;
                } else {
                    let _ = self.shared.notices.send(Notice::ResourcesUpdated {
                        sample: Box::new(subscribed.sample),
                    });
                }
                true
            }
            // The daemon refuses (too old, bad interval): nothing to retry on every reconnect.
            Err(ClientError::Remote(error)) => {
                warn!(message = %error.message, "resources.subscribe refused; not asked again");
                self.shared.set_resources_interval(None);
                true
            }
            Err(error) => {
                debug!(%error, "resubscribing to resources failed; connection lost");
                false
            }
        }
    }

    /// Folds `first` plus whatever else is already queued into one published state. `false`
    /// if the connection must be abandoned.
    async fn apply_events(
        &mut self,
        first: (Seq, Event),
        events: &mut broadcast::Receiver<(Seq, Event)>,
        client: &Client,
    ) -> bool {
        let mut batch = vec![first];
        let mut lagged = false;
        while batch.len() < MAX_BATCH {
            match events.try_recv() {
                Ok(event) => batch.push(event),
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    lagged = true;
                    break;
                }
                Err(_) => break,
            }
        }
        let effects = self.fold(batch);
        self.publish();
        self.dispatch(effects);
        !lagged || self.resync(client, events).await
    }

    /// Re-reads the snapshot on the live connection after the event stream lost events.
    async fn resync(
        &mut self,
        client: &Client,
        events: &mut broadcast::Receiver<(Seq, Event)>,
    ) -> bool {
        match timeout(SYNC_TIMEOUT, self.sync(client, events, Mode::Snapshot)).await {
            Ok(Ok(synced)) => {
                self.complete_sync(synced);
                true
            }
            Ok(Err(error)) => {
                warn!(%error, "resynchronising failed");
                false
            }
            Err(_) => {
                warn!("resynchronising timed out");
                false
            }
        }
    }

    /// Handles one terminal command on a live connection. `Err` means the connection is gone.
    async fn online(&mut self, client: &Client, command: Command) -> Result<(), ClientError> {
        match command {
            Command::Attach {
                handle,
                terminal,
                size,
                updates,
            } => {
                self.terminals
                    .register(handle, terminal, size, updates, &self.state.sessions);
                self.attach_now(client, terminal).await
            }
            Command::Detach { handle, terminal } => {
                if self.terminals.detach(handle, terminal) {
                    client.notify(&Request::TerminalDetach(TerminalRef {
                        terminal_id: terminal,
                    }))?;
                }
                Ok(())
            }
            Command::Ack {
                handle,
                terminal,
                frame,
            } => {
                if self.terminals.owns(handle, terminal) {
                    client.notify(&Request::TerminalAck(TerminalAck {
                        terminal_id: terminal,
                        frame,
                    }))?;
                }
                Ok(())
            }
            Command::Input {
                handle,
                terminal,
                bytes,
            } => {
                if !self.terminals.owns(handle, terminal) {
                    debug!(%terminal, "input for a detached or ended terminal dropped");
                    return Ok(());
                }
                match timeout(STEP_TIMEOUT, client.send_input(terminal, &bytes)).await {
                    Ok(sent) => sent,
                    Err(_) => Err(ClientError::Disconnected),
                }
            }
            Command::Resize {
                handle,
                terminal,
                size,
            } => {
                if let Some(attachment) = self.terminals.map.get_mut(&terminal)
                    && attachment.handle == handle
                {
                    attachment.size = Some(size);
                    client.notify(&Request::TerminalResize(TerminalResize {
                        terminal_id: terminal,
                        size,
                    }))?;
                }
                Ok(())
            }
            Command::SetColors {
                handle,
                terminal,
                colors,
            } => {
                if let Some(attachment) = self.terminals.map.get_mut(&terminal)
                    && attachment.handle == handle
                {
                    attachment.colors = Some(colors);
                    send_colors(client, terminal, colors)?;
                }
                Ok(())
            }
        }
    }

    /// (Re-)attaches `terminal` on `client`: the daemon queues a snapshot before it answers, and
    /// the forwarder started here hands it to the handle.
    async fn attach_now(
        &mut self,
        client: &Client,
        terminal: TerminalId,
    ) -> Result<(), ClientError> {
        let Some(attachment) = self.terminals.map.get_mut(&terminal) else {
            return Ok(());
        };
        if let Some(mut previous) = attachment.live.take() {
            // The previous connection's forwarder announces `Reconnecting` once it has passed
            // on everything buffered; the new snapshot must not overtake that.
            if timeout(FORWARDER_GRACE, &mut previous.forwarder)
                .await
                .is_err()
            {
                previous.stop.cancel();
            }
        }
        let stop = CancellationToken::new();
        let renders = client.terminal_frames(terminal);
        let forwarder = tokio::spawn(forward(renders, attachment.updates.clone(), stop.clone()));
        attachment.live = Some(Live { stop, forwarder });
        let request = Request::TerminalAttach(TerminalAttach {
            terminal_id: terminal,
            size: attachment.size,
        });
        match timeout(STEP_TIMEOUT, client.call::<TerminalAttached>(request)).await {
            Ok(Ok(_)) => {
                if let Some(colors) = attachment.colors {
                    send_colors(client, terminal, colors)?;
                }
                Ok(())
            }
            Ok(Err(ClientError::Remote(error))) if error.code == ErrorCode::NotFound => {
                self.terminals.gone(terminal, GoneReason::NotFound);
                Ok(())
            }
            Ok(Err(ClientError::Remote(error))) => {
                warn!(%terminal, code = ?error.code, message = %error.message, "attach refused");
                self.terminals.detach_any(terminal);
                Ok(())
            }
            Ok(Err(error)) => {
                self.terminals.stop_live(terminal);
                Err(error)
            }
            Err(_) => {
                self.terminals.stop_live(terminal);
                Err(ClientError::Disconnected)
            }
        }
    }
}

impl Terminals {
    /// Drops the attachment without a final update.
    fn detach_any(&mut self, terminal: TerminalId) {
        if let Some(attachment) = self.map.remove(&terminal) {
            attachment.stop();
        }
    }

    /// Ends the forwarder of a failed attach; the attachment itself stays for the next one.
    fn stop_live(&mut self, terminal: TerminalId) {
        if let Some(attachment) = self.map.get_mut(&terminal)
            && let Some(live) = attachment.live.take()
        {
            live.stop.cancel();
        }
    }
}

/// Tells the daemon the terminal's default colors, if it understands `terminal.set_colors`.
fn send_colors(
    client: &Client,
    terminal: TerminalId,
    colors: TerminalColors,
) -> Result<(), ClientError> {
    if client
        .welcome()
        .capabilities
        .iter()
        .any(|c| c == COLORS_CAPABILITY)
    {
        client.notify(&Request::TerminalSetColors(TerminalSetColors {
            terminal_id: terminal,
            colors,
        }))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_the_cap_with_bounded_jitter() {
        for (attempt, base_ms) in [(1u32, 100u64), (2, 200), (3, 400), (6, 3200), (7, 5000)] {
            for _ in 0..200 {
                let delay = backoff(attempt);
                let base = Duration::from_millis(base_ms);
                assert!(delay >= base.mul_f64(0.8), "{attempt}: {delay:?}");
                assert!(delay <= base.mul_f64(1.2), "{attempt}: {delay:?}");
            }
        }
        for _ in 0..200 {
            assert!(backoff(u32::MAX) <= BACKOFF_MAX.mul_f64(1.2));
        }
    }
}
