//! Event bus: one bounded ring per epoch with strictly increasing sequence numbers, and fan-out
//! to subscribed connections (ADR 0002).
//!
//! Publishing never blocks: a subscriber whose outbound queue is full is dropped (and its
//! connection closed) instead of slowing the bus.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc;

use hutan_core::{Epoch, Seq};
use hutan_protocol::messages::{ClientKind, Event, ResumeOutcome, ResumePoint};
use hutan_protocol::{ControlMessage, Frame, MAX_PAYLOAD, ProtocolError};

use crate::conn::{ConnHandle, ConnId, Outbound};

/// Bounds of the replay ring. Both limits apply; the oldest events are evicted first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingLimits {
    pub max_events: usize,
    pub max_bytes: usize,
}

impl Default for RingLimits {
    fn default() -> Self {
        Self {
            max_events: 10_000,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("event of {len} bytes exceeds the {MAX_PAYLOAD}-byte frame limit")]
    TooLarge { len: usize },
    #[error(transparent)]
    Encode(#[from] ProtocolError),
}

#[derive(Debug)]
struct Entry {
    seq: Seq,
    frame: Frame,
}

#[derive(Debug)]
struct Inner {
    last: Seq,
    ring: VecDeque<Entry>,
    ring_bytes: usize,
    subscribers: HashMap<ConnId, ConnHandle>,
    /// Internal consumer of the events the history index cares about.
    tap: Option<mpsc::Sender<Event>>,
    /// Internal consumer of the events notification intents are derived from.
    observer: Option<mpsc::Sender<Event>>,
    /// Internal consumer of the events the task service follows.
    task_observer: Option<mpsc::Sender<Event>>,
    /// Told (never with the event) when something the menu bar item shows may have changed.
    attention: Option<mpsc::Sender<()>>,
    /// Internal consumer of the file changes agents report (`file_diff` journal events), which
    /// the file service attributes `fs.changed` events with.
    fs_observer: Option<mpsc::Sender<Event>>,
}

#[derive(Debug)]
pub struct EventBus {
    epoch: Epoch,
    limits: RingLimits,
    inner: Mutex<Inner>,
}

impl EventBus {
    pub fn new(epoch: Epoch, limits: RingLimits) -> Self {
        Self {
            epoch,
            limits,
            inner: Mutex::new(Inner {
                last: Seq::default(),
                ring: VecDeque::new(),
                ring_bytes: 0,
                subscribers: HashMap::new(),
                tap: None,
                observer: None,
                task_observer: None,
                attention: None,
                fs_observer: None,
            }),
        }
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // The critical sections never leave `Inner` half-updated across a panic point.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends a copy of every session and terminal block event, and every chat event that is
    /// journaled, to `tap` from now on (`None`
    /// stops). Never blocks: when the tap's queue is full the event is dropped for it.
    pub fn set_tap(&self, tap: Option<mpsc::Sender<Event>>) {
        self.lock().tap = tap;
    }

    /// Sends a copy of every event notification intents are derived from (see
    /// [`feeds_notifications`]) to `observer` from now on (`None` stops). Never blocks: when the
    /// observer's queue is full the event is dropped for it.
    pub fn set_observer(&self, observer: Option<mpsc::Sender<Event>>) {
        self.lock().observer = observer;
    }
    /// Sends a copy of every event the task service follows (see [`feeds_tasks`]) to `observer`
    /// from now on (`None` stops). Never blocks: when the queue is full the event is dropped
    /// for it (the service looks at its sessions directly now and then).
    pub fn set_task_observer(&self, observer: Option<mpsc::Sender<Event>>) {
        self.lock().task_observer = observer;
    }

    /// Sends a copy of every `file_diff` event of a chat (see [`feeds_fs`]) to `observer` from
    /// now on (`None` stops). Never blocks: when the queue is full the event is dropped for it.
    pub fn set_fs_observer(&self, observer: Option<mpsc::Sender<Event>>) {
        self.lock().fs_observer = observer;
    }

    /// Pokes `attention` whenever an event that may change what needs the user (see
    /// [`feeds_attention`]) is published, from now on (`None` stops). Never blocks, and a poke
    /// that finds the queue full is dropped: one is already waiting.
    pub fn set_attention(&self, attention: Option<mpsc::Sender<()>>) {
        self.lock().attention = attention;
    }

    /// Seq of the most recent event (`0` before the first).
    pub fn last_seq(&self) -> Seq {
        self.lock().last
    }

    pub fn subscriber_count(&self) -> usize {
        self.lock().subscribers.len()
    }

    /// Subscribed connections whose client said hello as `kind`.
    pub fn subscriber_count_of(&self, kind: ClientKind) -> usize {
        self.lock()
            .subscribers
            .values()
            .filter(|sub| sub.client_kind() == Some(kind))
            .count()
    }

    /// Assigns the next seq, retains the event in the ring, and queues it to every subscriber.
    /// An event that cannot be framed is rejected without consuming a seq.
    pub fn publish(&self, event: &Event) -> Result<Seq, PublishError> {
        let mut inner = self.lock();
        let seq = inner.last.next();
        let frame = ControlMessage::event(self.epoch, seq, event)?.to_frame()?;
        let len = frame.payload.len();
        if len > MAX_PAYLOAD {
            return Err(PublishError::TooLarge { len });
        }
        inner.last = seq;
        inner.ring_bytes += len;
        inner.ring.push_back(Entry {
            seq,
            frame: frame.clone(),
        });
        while inner.ring.len() > self.limits.max_events || inner.ring_bytes > self.limits.max_bytes
        {
            let Some(evicted) = inner.ring.pop_front() else {
                break;
            };
            inner.ring_bytes -= evicted.frame.payload.len();
        }
        inner
            .subscribers
            .retain(|_, sub| sub.enqueue(Outbound::Frame(frame.clone())));
        // Session and block events, and the journaled (not streamed) events of chats.
        let tapped = match event {
            Event::SessionUpdated { .. } | Event::TerminalBlock { .. } => true,
            Event::ChatEvent { event, .. } => !event.kind.is_streaming(),
            _ => false,
        };
        if tapped && let Some(tap) = &inner.tap {
            match tap.try_send(event.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::debug!("history tap is full; dropped an event");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => inner.tap = None,
            }
        }
        if feeds_notifications(event)
            && let Some(observer) = &inner.observer
        {
            match observer.try_send(event.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!("notification observer is full; dropped an event");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => inner.observer = None,
            }
        }
        if feeds_tasks(event)
            && let Some(observer) = &inner.task_observer
        {
            match observer.try_send(event.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!("task observer is full; dropped an event");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => inner.task_observer = None,
            }
        }
        if feeds_fs(event)
            && let Some(observer) = &inner.fs_observer
        {
            match observer.try_send(event.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::debug!("file observer is full; dropped an event");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => inner.fs_observer = None,
            }
        }
        if feeds_attention(event)
            && let Some(attention) = &inner.attention
            && matches!(
                attention.try_send(()),
                Err(mpsc::error::TrySendError::Closed(_))
            )
        {
            inner.attention = None;
        }
        Ok(seq)
    }

    /// Sends `event` to one connection only, with a seq of its own (so the client's "newer than
    /// what I applied" rule lets it through) but outside the replay ring and the internal
    /// consumers: other clients never see it, and a reconnecting client never gets it replayed.
    /// For requests aimed at one client (`ui.command`). `false` when it could not be queued.
    pub fn publish_to(&self, conn: &ConnHandle, event: &Event) -> Result<bool, PublishError> {
        let mut inner = self.lock();
        let seq = inner.last.next();
        let frame = ControlMessage::event(self.epoch, seq, event)?.to_frame()?;
        let len = frame.payload.len();
        if len > MAX_PAYLOAD {
            return Err(PublishError::TooLarge { len });
        }
        inner.last = seq;
        Ok(conn.enqueue(Outbound::Frame(frame)))
    }

    /// Completes a handshake atomically with respect to publishing: queues the welcome built by
    /// `welcome`, then (if the client can resume) every missed event, and subscribes the
    /// connection so no event is lost or duplicated in between.
    pub fn open(
        &self,
        conn: &ConnHandle,
        resume: Option<ResumePoint>,
        welcome: impl FnOnce(ResumeOutcome) -> Result<Frame, ProtocolError>,
    ) -> Result<ResumeOutcome, ProtocolError> {
        let mut inner = self.lock();
        let replay = resume
            .filter(|r| r.epoch == self.epoch)
            .and_then(|r| inner.replay_after(r.seq));
        let outcome = match replay {
            Some(_) => ResumeOutcome::Replayed,
            None => ResumeOutcome::SnapshotRequired,
        };
        let welcome = welcome(outcome)?;
        if !conn.enqueue(Outbound::Frame(welcome)) {
            return Ok(outcome);
        }
        if let Some(missed) = replay {
            if !missed.is_empty() && !conn.enqueue(Outbound::Batch(missed)) {
                return Ok(outcome);
            }
            inner.subscribers.insert(conn.id(), conn.clone());
        }
        Ok(outcome)
    }

    /// Registers `conn` for every subsequent event and returns the last seq already emitted.
    pub fn subscribe(&self, conn: &ConnHandle) -> Seq {
        let mut inner = self.lock();
        inner.subscribers.insert(conn.id(), conn.clone());
        inner.last
    }

    pub fn unsubscribe(&self, id: ConnId) {
        self.lock().subscribers.remove(&id);
    }
}

impl Inner {
    /// Frames of every event after `seq`, or `None` if some were evicted (or `seq` is ahead of
    /// anything emitted, which can only come from a confused client).
    fn replay_after(&self, seq: Seq) -> Option<Vec<Frame>> {
        if seq > self.last {
            return None;
        }
        let oldest = self.ring.front().map_or(self.last.next(), |e| e.seq);
        if seq.next() < oldest {
            return None;
        }
        Some(
            self.ring
                .iter()
                .filter(|e| e.seq > seq)
                .map(|e| e.frame.clone())
                .collect(),
        )
    }
}

/// The events that may change what the menu bar item of the daemon lists as needing the user:
/// session states and agent reports, exits, approvals, questions, and raised intents.
fn feeds_attention(event: &Event) -> bool {
    matches!(
        event,
        Event::SessionUpdated { .. }
            | Event::SessionExited { .. }
            | Event::SessionClosed { .. }
            | Event::ApprovalRequired { .. }
            | Event::ApprovalResolved { .. }
            | Event::QuestionRequired { .. }
            | Event::QuestionResolved { .. }
            | Event::NotifyIntent { .. }
    )
}

/// The events the file service attributes changes with: the files an agent says it changed
/// (live `file_diff` events of a chat's journal, never replays).
fn feeds_fs(event: &Event) -> bool {
    use hutan_core::AgentEventKind;
    use hutan_core::agent::Delivery;
    matches!(
        event,
        Event::ChatEvent { event, .. }
            if event.delivery == Delivery::Live
                && matches!(event.kind, AgentEventKind::FileDiff { .. })
    )
}

/// The events the task service follows: session states (ends, blocked agents), agent tree
/// changes, the end of a live chat turn, and closed workspaces.
fn feeds_tasks(event: &Event) -> bool {
    use hutan_core::agent::Delivery;
    match event {
        Event::SessionUpdated { .. }
        | Event::AgentTreeUpdated { .. }
        | Event::WorkspaceClosed { .. } => true,
        Event::ChatEvent { event, .. } => {
            event.delivery == Delivery::Live && event.kind.ends_turn()
        }
        _ => false,
    }
}

/// The events notification intents are derived from: approvals, questions, agent tree changes, session
/// exits, task updates, and the journaled end of a chat turn or of an assistant message (never
/// streaming deltas, never replays).
fn feeds_notifications(event: &Event) -> bool {
    use hutan_core::AgentEventKind;
    use hutan_core::agent::{Delivery, MessageRole};
    match event {
        Event::ApprovalRequired { .. }
        | Event::ApprovalResolved { .. }
        | Event::QuestionRequired { .. }
        | Event::QuestionResolved { .. }
        | Event::AgentTreeUpdated { .. }
        | Event::SessionExited { .. }
        | Event::SessionClosed { .. }
        | Event::TaskUpdated { .. } => true,
        Event::ChatEvent { event, .. } => {
            event.delivery == Delivery::Live
                && matches!(
                    event.kind,
                    AgentEventKind::TurnStarted
                        | AgentEventKind::TurnCompleted { .. }
                        | AgentEventKind::MessageEnd {
                            role: MessageRole::Assistant,
                            ..
                        }
                )
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use hutan_core::WorkspaceId;

    use super::*;

    fn event() -> Event {
        Event::WorkspaceClosed {
            workspace_id: WorkspaceId::new(),
        }
    }

    fn bus(max_events: usize, max_bytes: usize) -> EventBus {
        EventBus::new(
            Epoch::new(),
            RingLimits {
                max_events,
                max_bytes,
            },
        )
    }

    #[test]
    fn seq_is_strictly_increasing_from_one() {
        let bus = bus(10, usize::MAX);
        assert_eq!(bus.last_seq(), Seq(0));
        assert_eq!(bus.publish(&event()).unwrap(), Seq(1));
        assert_eq!(bus.publish(&event()).unwrap(), Seq(2));
        assert_eq!(bus.last_seq(), Seq(2));
    }

    #[test]
    fn ring_is_bounded_by_count() {
        let bus = bus(3, usize::MAX);
        for _ in 0..5 {
            bus.publish(&event()).unwrap();
        }
        let inner = bus.lock();
        assert_eq!(inner.ring.len(), 3);
        assert_eq!(inner.ring.front().unwrap().seq, Seq(3));
    }

    #[test]
    fn ring_is_bounded_by_bytes() {
        let bus = bus(100, 1);
        bus.publish(&event()).unwrap();
        bus.publish(&event()).unwrap();
        let inner = bus.lock();
        assert!(inner.ring.is_empty());
        assert_eq!(inner.ring_bytes, 0);
    }

    #[test]
    fn replay_window() {
        let bus = bus(3, usize::MAX);
        for _ in 0..5 {
            bus.publish(&event()).unwrap();
        }
        let inner = bus.lock();
        // ring holds 3..=5
        assert_eq!(inner.replay_after(Seq(5)).unwrap().len(), 0);
        assert_eq!(inner.replay_after(Seq(2)).unwrap().len(), 3);
        assert_eq!(inner.replay_after(Seq(3)).unwrap().len(), 2);
        assert!(inner.replay_after(Seq(1)).is_none());
        assert!(inner.replay_after(Seq(6)).is_none());
    }

    #[test]
    fn replay_from_zero_on_fresh_bus() {
        let bus = bus(3, usize::MAX);
        assert_eq!(bus.lock().replay_after(Seq(0)).unwrap().len(), 0);
    }
}
