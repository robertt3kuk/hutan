//! [`CheckpointStore`]: the checkpoints of agent chats as this window knows them (ADR 0019,
//! slice CK-5), for the turn footers, the Checkpoints list and *Undo Last Restore*.
//!
//! A chat's checkpoints are read with `checkpoint.list` the first time something shows them,
//! then kept current by `checkpoint.created` / `checkpoint.removed` (which the connection hands
//! out on [`Connection::chat_updates`]); a resync reads every loaded chat again.
//!
//! The daemon does not say which checkpoint a restore restored (`CheckpointInfo` has no parent
//! on the wire), so the restores this window made are remembered here, in order: a turn whose
//! newest restore was not undone shows `Restored — Undo`, and an undo of that undo shows its
//! *Rewind…* again.

use std::collections::HashMap;

use gpui::{Context, EventEmitter, Task};
use hutan_client::{ChatUpdate, CheckpointUpdate, Connection};
use hutan_core::{
    CheckpointId, CheckpointInfo, CheckpointKind, CheckpointState, SessionId, TurnId,
};
use hutan_protocol::checkpoint::{CheckpointListRequest, LIST_MAX_LIMIT};
use tokio::sync::broadcast::error::RecvError;

/// A turn of a chat ended (whatever way): a preview that was refused because the checkout was
/// busy may now go through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnEnded(pub SessionId);

/// What is loaded of one chat.
#[derive(Debug, Default)]
struct Loaded {
    /// Newest first.
    list: Vec<CheckpointInfo>,
    /// A `checkpoint.list` is on its way.
    loading: bool,
    /// The last `checkpoint.list` failed.
    error: Option<String>,
}

pub struct CheckpointStore {
    connection: Connection,
    chats: HashMap<SessionId, Loaded>,
    /// The restores this window made, oldest first: `(restore, what it restored)`.
    restores: Vec<(CheckpointId, CheckpointId)>,
    /// Bumped by every change a footer may show.
    revision: u64,
    _task: Task<()>,
}

impl EventEmitter<TurnEnded> for CheckpointStore {}

impl std::fmt::Debug for CheckpointStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckpointStore")
            .field("chats", &self.chats.len())
            .field("restores", &self.restores.len())
            .finish_non_exhaustive()
    }
}

impl CheckpointStore {
    pub fn new(connection: Connection, cx: &mut Context<Self>) -> Self {
        let mut updates = connection.chat_updates();
        let task = cx.spawn(async move |this, cx| {
            loop {
                let alive = match updates.recv().await {
                    Ok(ChatUpdate::Checkpoint(update)) => {
                        this.update(cx, |this, cx| this.apply(&update, cx))
                    }
                    Ok(ChatUpdate::Event(event)) if event.kind.ends_turn() => {
                        let session = event.session_id;
                        this.update(cx, |_, cx| cx.emit(TurnEnded(session)))
                    }
                    // The connection came back (or this receiver fell behind): what was missed
                    // is read again.
                    Ok(ChatUpdate::Resync { .. }) | Err(RecvError::Lagged(_)) => {
                        this.update(cx, |this, cx| this.reload_all(cx))
                    }
                    Ok(ChatUpdate::Event(_) | ChatUpdate::Queue(_)) => Ok(()),
                    Err(RecvError::Closed) => return,
                };
                if alive.is_err() {
                    return;
                }
            }
        });
        Self {
            connection,
            chats: HashMap::new(),
            restores: Vec::new(),
            revision: 0,
            _task: task,
        }
    }

    /// Whether the daemon has checkpoints (capability `checkpoint`).
    pub fn supported(&self) -> bool {
        self.connection.supports_checkpoints()
    }

    /// Whether the daemon restores files (capability `checkpoint.restore`).
    pub fn restore_supported(&self) -> bool {
        self.connection.supports_checkpoint_restore()
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Bumped by every change a footer may show.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Reads the checkpoints of `session` unless they are loaded or on their way.
    pub fn ensure(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if !self.supported() || self.chats.contains_key(&session) {
            return;
        }
        self.load(session, cx);
    }

    /// Reads the checkpoints of `session` again.
    pub fn reload(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.supported() {
            self.load(session, cx);
        }
    }

    fn reload_all(&mut self, cx: &mut Context<Self>) {
        let sessions: Vec<SessionId> = self.chats.keys().copied().collect();
        for session in sessions {
            self.reload(session, cx);
        }
    }

    fn load(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let loaded = self.chats.entry(session).or_default();
        if loaded.loading {
            return;
        }
        loaded.loading = true;
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            let answer = connection
                .checkpoint_list(CheckpointListRequest {
                    session_id: Some(session),
                    limit: Some(LIST_MAX_LIMIT),
                    ..CheckpointListRequest::default()
                })
                .await;
            this.update(cx, |this, cx| {
                let loaded = this.chats.entry(session).or_default();
                loaded.loading = false;
                match answer {
                    Ok(list) => {
                        loaded.error = None;
                        // What an event brought meanwhile is newer than the answer.
                        let mut merged = list.checkpoints;
                        for known in loaded.list.drain(..) {
                            match merged.iter_mut().find(|c| c.id == known.id) {
                                Some(slot) => *slot = known,
                                None => merged.push(known),
                            }
                        }
                        sort_newest_first(&mut merged);
                        loaded.list = merged;
                    }
                    Err(error) => loaded.error = Some(error.to_string()),
                }
                this.revision += 1;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply(&mut self, update: &CheckpointUpdate, cx: &mut Context<Self>) {
        match update {
            CheckpointUpdate::Created(info) => {
                let Some(loaded) = self.chats.get_mut(&info.session_id) else {
                    return;
                };
                match loaded.list.iter_mut().find(|c| c.id == info.id) {
                    Some(slot) => *slot = info.clone(),
                    None => loaded.list.push(info.clone()),
                }
                sort_newest_first(&mut loaded.list);
            }
            CheckpointUpdate::Removed(ids) => {
                for loaded in self.chats.values_mut() {
                    loaded.list.retain(|c| !ids.contains(&c.id));
                }
                self.restores
                    .retain(|(restore, target)| !ids.contains(restore) && !ids.contains(target));
            }
        }
        self.revision += 1;
        cx.notify();
    }

    /// The checkpoints of `session`, newest first (empty until loaded).
    pub fn of(&self, session: SessionId) -> &[CheckpointInfo] {
        self.chats
            .get(&session)
            .map_or(&[], |loaded| loaded.list.as_slice())
    }

    /// Whether the checkpoints of `session` have been read (or tried).
    pub fn loaded(&self, session: SessionId) -> bool {
        self.chats
            .get(&session)
            .is_some_and(|loaded| !loaded.loading)
    }

    /// Why the checkpoints of `session` could not be read.
    pub fn error(&self, session: SessionId) -> Option<&str> {
        self.chats.get(&session)?.error.as_deref()
    }

    /// The checkpoint of a turn of `session`.
    pub fn of_turn(&self, session: SessionId, turn: TurnId) -> Option<&CheckpointInfo> {
        self.of(session)
            .iter()
            .find(|c| c.kind == CheckpointKind::Turn && c.turn_id == Some(turn))
    }

    /// The checkpoint `id`, wherever it is loaded.
    pub fn get(&self, id: CheckpointId) -> Option<&CheckpointInfo> {
        self.chats
            .values()
            .flat_map(|loaded| loaded.list.iter())
            .find(|c| c.id == id)
    }

    /// The newest restore of `session` (what *Undo Last Restore* restores).
    pub fn last_restore(&self, session: SessionId) -> Option<&CheckpointInfo> {
        self.of(session)
            .iter()
            .find(|c| c.kind == CheckpointKind::Restore && c.state == CheckpointState::Ok)
    }

    /// Remembers that `restore` restored `target` (a turn, or an earlier restore: an undo).
    pub fn note_restore(
        &mut self,
        restore: CheckpointId,
        target: CheckpointId,
        cx: &mut Context<Self>,
    ) {
        self.restores.push((restore, target));
        self.revision += 1;
        cx.notify();
    }

    /// The restore that Undo restores when `checkpoint` is restored now, else `None`.
    pub fn restored_by(&self, checkpoint: CheckpointId) -> Option<CheckpointId> {
        restored_by(&self.restores, checkpoint)
    }
}

/// Newest first; the id breaks ties (ids grow with time).
fn sort_newest_first(list: &mut [CheckpointInfo]) {
    list.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then_with(|| b.id.to_string().cmp(&a.id.to_string()))
    });
}

/// Follows the restores made after `checkpoint` (each the newest that restored the one before):
/// an odd number of them means `checkpoint` stands restored, and the last one is what Undo
/// restores. An even number (a restore and its undo) means it does not.
fn restored_by(
    restores: &[(CheckpointId, CheckpointId)],
    checkpoint: CheckpointId,
) -> Option<CheckpointId> {
    let mut current = checkpoint;
    let mut depth = 0usize;
    // Each step moves to a restore made later, so the walk ends; the bound keeps it finite
    // whatever the list holds.
    for _ in 0..=restores.len() {
        let Some((restore, _)) = restores.iter().rev().find(|(_, target)| *target == current)
        else {
            break;
        };
        current = *restore;
        depth += 1;
    }
    (depth % 2 == 1).then_some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restore_shows_until_it_is_undone_and_again_after_the_undo_is_undone() {
        let turn = CheckpointId::new();
        let mut restores = Vec::new();
        assert_eq!(restored_by(&restores, turn), None);

        let first = CheckpointId::new();
        restores.push((first, turn));
        assert_eq!(restored_by(&restores, turn), Some(first));

        // Undo: the turn is back to its *Rewind…*.
        let undo = CheckpointId::new();
        restores.push((undo, first));
        assert_eq!(restored_by(&restores, turn), None);

        // Undo of the undo: restored again, and Undo now restores the newest.
        let redo = CheckpointId::new();
        restores.push((redo, undo));
        assert_eq!(restored_by(&restores, turn), Some(redo));

        // Rewound again after an undo: the newest restore of the turn counts.
        let mut again = vec![(first, turn), (undo, first)];
        let second = CheckpointId::new();
        again.push((second, turn));
        assert_eq!(restored_by(&again, turn), Some(second));
    }
}
