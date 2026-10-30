//! The queue of each chat session (protocol 0.36, capability `chat.queue`): messages the user
//! sent while a turn ran, sent one by one as turns end.
//!
//! [`Queues`] is the state and its rules: bounded ([`QUEUE_MAX_ITEMS`] messages,
//! [`QUEUE_MAX_BYTES`] of text), written whole to `chat_queue` after every change (in the order
//! the changes happen: they are queued to the storage writer under the lock), read back the first
//! time a session's queue is asked for. A queue a previous daemon left behind comes back held
//! ([`QueueHold::AgentGone`]): its agent went away mid-turn, and nothing is sent until the user
//! says so. What is sent and when is [`crate::chat::ChatManager`]'s: it asks for the next message
//! once a turn ended and the queue does not hold.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use hutan_core::SessionId;
use hutan_protocol::chat::{
    ChatQueue, MessageOrigin, QUEUE_MAX_BYTES, QUEUE_MAX_ITEMS, QueueChange, QueueHold,
    QueuedMessage,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_storage::{QueuedRow, Storage};

use crate::registry;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded data is never left half-updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One session's queue.
#[derive(Debug, Default)]
struct Queue {
    items: VecDeque<QueuedMessage>,
    held: Option<QueueHold>,
    /// The next item's id.
    next_id: u64,
}

impl Queue {
    fn bytes(&self) -> usize {
        self.items.iter().map(|item| item.text.len()).sum()
    }

    fn position(&self, item: u64) -> Result<usize, ErrorBody> {
        self.items
            .iter()
            .position(|queued| queued.id == item)
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("message {item} is no longer queued (sent, removed or cleared)"),
                )
            })
    }

    /// Room for `added` more bytes (and, with `new_item`, one more message).
    fn room_for(&self, added: usize, removed: usize, new_item: bool) -> Result<(), ErrorBody> {
        if new_item && self.items.len() >= QUEUE_MAX_ITEMS {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!(
                    "the queue is full: at most {QUEUE_MAX_ITEMS} messages wait; remove one or \
                     wait for the turn"
                ),
            ));
        }
        if self.bytes() - removed + added > QUEUE_MAX_BYTES {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!(
                    "the queue is full: its messages may hold at most {} KiB of text together",
                    QUEUE_MAX_BYTES / 1024
                ),
            ));
        }
        Ok(())
    }

    fn new_item(&mut self, text: String, origin: Option<MessageOrigin>) -> QueuedMessage {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        QueuedMessage {
            id,
            text,
            queued_at_ms: registry::now_ms(),
            origin,
        }
    }
}

/// The queues of every chat session of one daemon.
#[derive(Debug)]
pub(crate) struct Queues {
    storage: Storage,
    queues: Mutex<HashMap<SessionId, Queue>>,
    /// Sessions whose running agent takes a message during its turn (Codex `turn/steer`).
    steer: Mutex<HashSet<SessionId>>,
}

/// A queue after a change: what the `chat.queue` event carries.
pub(crate) type Changed = (ChatQueue, QueueChange);

impl Queues {
    pub(crate) fn new(storage: Storage) -> Self {
        Self {
            storage,
            queues: Mutex::new(HashMap::new()),
            steer: Mutex::new(HashSet::new()),
        }
    }

    /// Whether the session's running agent steers (`ChatQueue::steer`).
    pub(crate) fn set_steer(&self, id: SessionId, steer: bool) {
        let mut steering = lock(&self.steer);
        if steer {
            steering.insert(id);
        } else {
            steering.remove(&id);
        }
    }

    /// Reads the session's queue from the database the first time it is needed.
    pub(crate) async fn load(&self, id: SessionId) -> Result<(), ErrorBody> {
        if lock(&self.queues).contains_key(&id) {
            return Ok(());
        }
        let storage = self.storage.clone();
        let rows = tokio::task::spawn_blocking(move || storage.chat().queue(id))
            .await
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the queue: {e}")))?
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the queue: {e}")))?;
        let mut queues = lock(&self.queues);
        // Another caller may have loaded it meanwhile; what it holds is newer.
        queues.entry(id).or_insert_with(|| {
            let next_id = rows.iter().map(|row| row.item_id).max().unwrap_or(0) + 1;
            Queue {
                held: (!rows.is_empty()).then_some(QueueHold::AgentGone),
                items: rows
                    .into_iter()
                    .map(|row| QueuedMessage {
                        id: row.item_id,
                        text: row.text,
                        queued_at_ms: row.queued_at_ms,
                        origin: row
                            .origin_json
                            .and_then(|json| serde_json::from_str(&json).ok()),
                    })
                    .collect(),
                next_id,
            }
        });
        Ok(())
    }

    fn snapshot(&self, id: SessionId, queue: &Queue) -> ChatQueue {
        ChatQueue {
            session_id: id,
            items: queue.items.iter().cloned().collect(),
            held: queue.held,
            steer: lock(&self.steer).contains(&id),
        }
    }

    /// Runs `change` on the (loaded) queue of `id`; writes the queue when it says it changed.
    fn with<R>(
        &self,
        id: SessionId,
        change: impl FnOnce(&mut Queue) -> (bool, R),
    ) -> (R, Option<ChatQueue>) {
        let mut queues = lock(&self.queues);
        let queue = queues.entry(id).or_default();
        let (changed, out) = change(queue);
        if !changed {
            return (out, None);
        }
        let rows = queue
            .items
            .iter()
            .map(|item| QueuedRow {
                session_id: id,
                item_id: item.id,
                text: item.text.clone(),
                queued_at_ms: item.queued_at_ms,
                origin_json: item
                    .origin
                    .as_ref()
                    .and_then(|origin| serde_json::to_string(origin).ok()),
            })
            .collect();
        // Under the lock: the writer applies the queues in the order they changed.
        registry::log_write(self.storage.chat().set_queue(id, rows), "a chat queue");
        (out, Some(self.snapshot(id, queue)))
    }

    /// The queue as it is.
    pub(crate) fn list(&self, id: SessionId) -> ChatQueue {
        let mut queues = lock(&self.queues);
        let queue = queues.entry(id).or_default();
        self.snapshot(id, queue)
    }

    /// A message at the end (or, with `front`, first).
    pub(crate) fn add(
        &self,
        id: SessionId,
        text: String,
        front: bool,
        origin: Option<MessageOrigin>,
    ) -> Result<(QueuedMessage, Changed), ErrorBody> {
        let (added, queue) = self.with(id, |queue| {
            if let Err(error) = queue.room_for(text.len(), 0, true) {
                return (false, Err(error));
            }
            let item = queue.new_item(text, origin);
            if front {
                queue.items.push_front(item.clone());
            } else {
                queue.items.push_back(item.clone());
            }
            (true, Ok(item))
        });
        let item = added?;
        let change = QueueChange::Added { item_id: item.id };
        Ok((item, (queue.unwrap_or_else(|| self.list(id)), change)))
    }

    pub(crate) fn edit(
        &self,
        id: SessionId,
        item: u64,
        text: String,
    ) -> Result<(QueuedMessage, Changed), ErrorBody> {
        let (edited, queue) = self.with(id, |queue| {
            let at = match queue.position(item) {
                Ok(at) => at,
                Err(error) => return (false, Err(error)),
            };
            let old = queue.items[at].text.len();
            if let Err(error) = queue.room_for(text.len(), old, false) {
                return (false, Err(error));
            }
            queue.items[at].text = text;
            // The words are the user's now: the label of an approved message no longer fits.
            queue.items[at].origin = None;
            (true, Ok(queue.items[at].clone()))
        });
        let edited = edited?;
        let change = QueueChange::Edited { item_id: item };
        Ok((edited, (queue.unwrap_or_else(|| self.list(id)), change)))
    }

    /// Takes `item` out unsent: the user removed it, or `chat.send_now` sends it.
    pub(crate) fn take(
        &self,
        id: SessionId,
        item: u64,
    ) -> Result<(QueuedMessage, ChatQueue), ErrorBody> {
        let (taken, queue) = self.with(id, |queue| match queue.position(item) {
            Ok(at) => {
                let taken = queue.items.remove(at);
                if queue.items.is_empty() {
                    queue.held = None;
                }
                (true, Ok(taken))
            }
            Err(error) => (false, Err(error)),
        });
        let taken = taken?.ok_or_else(|| ErrorBody::new(ErrorCode::Internal, "lost a message"))?;
        Ok((taken, queue.unwrap_or_else(|| self.list(id))))
    }

    /// Moves `item` to the front.
    pub(crate) fn to_front(&self, id: SessionId, item: u64) -> Result<Changed, ErrorBody> {
        let (moved, queue) = self.with(id, |queue| match queue.position(item) {
            Ok(0) => (false, Ok(())),
            Ok(at) => {
                if let Some(moved) = queue.items.remove(at) {
                    queue.items.push_front(moved);
                }
                (true, Ok(()))
            }
            Err(error) => (false, Err(error)),
        });
        moved?;
        Ok((
            queue.unwrap_or_else(|| self.list(id)),
            QueueChange::Edited { item_id: item },
        ))
    }

    /// Takes every message out unsent; `None` when there were none.
    pub(crate) fn clear(&self, id: SessionId, stopped: bool) -> Option<Changed> {
        let (count, queue) = self.with(id, |queue| {
            let count = queue.items.len();
            queue.items.clear();
            let had_hold = queue.held.take().is_some();
            (count > 0 || had_hold, count)
        });
        let queue = queue?;
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        Some((queue, QueueChange::Cleared { count, stopped }))
    }

    /// The queue holds for `why` (when it has anything to hold).
    pub(crate) fn hold(&self, id: SessionId, why: QueueHold) -> Option<Changed> {
        let mut queues = lock(&self.queues);
        let queue = queues.get_mut(&id)?;
        if queue.items.is_empty() || queue.held == Some(why) {
            return None;
        }
        queue.held = Some(why);
        Some((self.snapshot(id, queue), QueueChange::Held))
    }

    /// The queue no longer holds.
    pub(crate) fn release(&self, id: SessionId) -> Option<Changed> {
        let mut queues = lock(&self.queues);
        let queue = queues.get_mut(&id)?;
        queue.held.take()?;
        Some((self.snapshot(id, queue), QueueChange::Released))
    }

    /// Whether a message waits and the queue does not hold.
    pub(crate) fn has_next(&self, id: SessionId) -> bool {
        lock(&self.queues)
            .get(&id)
            .is_some_and(|queue| queue.held.is_none() && !queue.items.is_empty())
    }

    /// The first message, out of the queue, unless it holds.
    pub(crate) fn take_next(&self, id: SessionId) -> Option<(QueuedMessage, ChatQueue)> {
        let (taken, queue) = self.with(id, |queue| {
            if queue.held.is_some() {
                return (false, None);
            }
            let taken = queue.items.pop_front();
            (taken.is_some(), taken)
        });
        Some((taken?, queue?))
    }

    /// A message that could not be sent goes back to the front, and the queue holds.
    pub(crate) fn put_back(&self, id: SessionId, item: QueuedMessage, why: QueueHold) -> Changed {
        let ((), queue) = self.with(id, |queue| {
            queue.items.push_front(item);
            queue.held = Some(why);
            (true, ())
        });
        (queue.unwrap_or_else(|| self.list(id)), QueueChange::Held)
    }
}
