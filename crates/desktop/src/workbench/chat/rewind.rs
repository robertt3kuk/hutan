//! The checkpoint line of a turn's footer (ADR 0019, slice CK-5): a turn that changed files says
//! `3 files changed · +41 −7 · Rewind…`; once this window restored it, `Restored — Undo` until
//! that restore is undone. Nothing without the daemon's capability `checkpoint`, nothing for a
//! chat whose checkpoints are off or unsupported (the chat's ⋯ menu says why, once), and no
//! button without `checkpoint.restore`. The buttons dispatch to the workbench, which owns the
//! preview sheet and the Undo toast (`workbench/rewind.rs`).

use gpui::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, div, prelude::*, px,
};
use hutan_core::TurnId;
use hutan_core::{CheckpointAvailability, CheckpointInfo, CheckpointState};

use super::markdown_view::Look;
use super::view::ChatView;
use super::widgets::{ButtonKind, button};
use crate::workbench::actions::{self, CheckpointAt};
use crate::workbench::render::probe;
use crate::workbench::view::Region;

/// `3 files changed · +41 −7`.
pub fn changed_text(checkpoint: &CheckpointInfo) -> String {
    let files = checkpoint.files_changed;
    let noun = if files == 1 { "file" } else { "files" };
    format!(
        "{files} {noun} changed · +{} −{}",
        checkpoint.added, checkpoint.removed
    )
}

/// What a turn's footer offers for its checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterAction {
    /// The turn can be rewound.
    Rewind,
    /// This window restored it: Undo restores that restore.
    Undo(hutan_core::CheckpointId),
    /// Nothing (a late checkpoint, a daemon that cannot restore).
    None,
}

impl ChatView {
    /// The checkpoint of the turn whose footer is `entry`, with what its footer offers; `None`
    /// when the footer has nothing to say about files.
    pub(super) fn turn_checkpoint(
        &self,
        turn: Option<TurnId>,
        cx: &mut Context<Self>,
    ) -> Option<(CheckpointInfo, FooterAction)> {
        let turn = turn?;
        let sessions = self.sessions.read(cx);
        // Off and unsupported chats show nothing per turn.
        let availability = sessions
            .session(self.session)
            .and_then(|s| s.chat.as_ref())
            .and_then(|chat| chat.checkpoints.as_ref())
            .map(|status| status.state);
        if matches!(
            availability,
            Some(CheckpointAvailability::Off | CheckpointAvailability::Unsupported)
        ) {
            return None;
        }
        let store = sessions.checkpoints().clone();
        let session = self.session;
        let (supported, waiting) = {
            let store = store.read(cx);
            (
                store.supported(),
                !store.loaded(session) && store.error(session).is_none(),
            )
        };
        if !supported {
            return None;
        }
        if waiting {
            store.update(cx, |store, cx| store.ensure(session, cx));
            return None;
        }
        let store = store.read(cx);
        let checkpoint = store.of_turn(session, turn)?.clone();
        if checkpoint.files_changed == 0 {
            return None;
        }
        let action = if !store.restore_supported() || checkpoint.state != CheckpointState::Ok {
            FooterAction::None
        } else {
            match store.restored_by(checkpoint.id) {
                Some(restore) => FooterAction::Undo(restore),
                None => FooterAction::Rewind,
            }
        };
        Some((checkpoint, action))
    }

    /// The footer's line about the files the turn changed, with *Rewind…* or *Undo*.
    pub(super) fn checkpoint_footer(
        &self,
        index: usize,
        turn: Option<TurnId>,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (checkpoint, action) = self.turn_checkpoint(turn, cx)?;
        let (theme, tokens) = (&look.theme, &look.tokens);
        let session = self.session;
        let at = CheckpointAt {
            session,
            checkpoint: checkpoint.id,
        };
        let control = match action {
            FooterAction::Rewind => Some(
                button(
                    ("chat-rewind", index),
                    "Rewind…",
                    ButtonKind::Ghost,
                    true,
                    look,
                )
                .child(probe(&self.regions, Region::ChatRewind(index)))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(actions::RewindCheckpoint(at)), cx);
                })
                .into_any_element(),
            ),
            FooterAction::Undo(restore) => {
                let undo = CheckpointAt {
                    session,
                    checkpoint: restore,
                };
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing))
                        .child(div().text_color(theme.fg).child("Restored —"))
                        .child(
                            button(
                                ("chat-restored-undo", index),
                                "Undo",
                                ButtonKind::Ghost,
                                true,
                                look,
                            )
                            .child(probe(&self.regions, Region::ChatRewind(index)))
                            .on_click(move |_, window, cx| {
                                window.dispatch_action(Box::new(actions::UndoRestore(undo)), cx);
                            }),
                        )
                        .into_any_element(),
                )
            }
            FooterAction::None => None,
        };
        Some(
            div()
                .relative()
                .mt(px(tokens.spacing * 0.5))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(tokens.spacing * 1.5))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(SharedString::from(changed_text(&checkpoint)))
                .when_some(control, |s, control| {
                    s.child(div().child("·")).child(control)
                })
                .child(probe(&self.regions, Region::ChatCheckpoint(index)))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::{CheckpointId, CheckpointKind, SessionId};

    #[test]
    fn the_line_counts_files_and_lines() {
        let mut checkpoint = CheckpointInfo {
            id: CheckpointId::new(),
            session_id: SessionId::new(),
            turn_id: None,
            kind: CheckpointKind::Turn,
            state: CheckpointState::Ok,
            reason: None,
            created_ms: 0,
            checkout: None,
            files_changed: 3,
            added: 41,
            removed: 7,
            bytes: 0,
            skipped: Vec::new(),
        };
        assert_eq!(changed_text(&checkpoint), "3 files changed · +41 −7");
        checkpoint.files_changed = 1;
        assert_eq!(changed_text(&checkpoint), "1 file changed · +41 −7");
    }
}
