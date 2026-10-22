//! What [`Connection::chat_updates`](crate::Connection::chat_updates) delivers.

use std::sync::Arc;

use hutan_core::{AgentEvent, CheckpointId, CheckpointInfo, Epoch};
use hutan_protocol::chat::ChatQueue;

/// One step of the chat event stream of a [`Connection`](crate::Connection).
///
/// Events carry their session and a per-session `seq`; a consumer that renders a transcript keeps
/// the highest `seq` it applied and ignores anything at or below it. The stream has holes by
/// design: after a reconnect the daemon replays what the connection missed when it still can, and
/// otherwise nothing; and a receiver that lagged lost events. Both are announced so the consumer
/// can read the journal (`chat.history`) and fill in what it missed:
///
/// * [`Resync`](Self::Resync) is sent every time the connection finished synchronising (the first
///   time too), right before the events the synchronisation replayed. It names the daemon's
///   epoch: another epoch than before means the daemon restarted, and its per-session sequence
///   numbers may start over (an unjournaled delta's number is used again).
/// * `RecvError::Lagged` on the receiver means the same.
///
/// Streaming deltas are not journaled: a gap in them heals when the item's `*_end` event arrives.
///
/// [`Queue`](Self::Queue) (protocol 0.36) is a chat's whole queue after it changed: a consumer
/// keeps the newest one, and reads it again (`chat.queue.list`) after a resync.
///
/// [`Checkpoint`](Self::Checkpoint) (protocol 0.37) is a checkpoint recorded or removed: a
/// consumer that shows a chat's checkpoints folds it into what it read with `checkpoint.list`,
/// and lists again after a resync.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatUpdate {
    Event(Arc<AgentEvent>),
    Resync { epoch: Epoch },
    Queue(Arc<ChatQueue>),
    Checkpoint(Arc<CheckpointUpdate>),
}

/// A change to the checkpoints of chats (`checkpoint.created`, `checkpoint.removed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointUpdate {
    /// A turn's or a restore's checkpoint was recorded (or recorded again with its end).
    Created(Box<CheckpointInfo>),
    /// Checkpoints went (`checkpoint.remove`, retention).
    Removed(Vec<CheckpointId>),
}
