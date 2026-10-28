//! The checkpoint line of a turn's footer (ADR 0019, slice CK-5): a turn that changed files says
//! `3 files changed · +41 −7 · Rewind…`; once it is restored (by any window, before a restart
//! too: the daemon's restore checkpoint says what it restored), `Restored — Undo` until
//! that restore is undone. Nothing without the daemon's capability `checkpoint`, nothing for a
//! chat whose checkpoints are off or unsupported (the chat's ⋯ menu says why, once), and no
//! button without `checkpoint.restore`. The buttons dispatch to the workbench, which owns the
//! preview sheet and the Undo toast (`workbench/rewind.rs`).

use crate::workbench::ui::Button;
use gpui::{
    AnyElement, App, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px,
};
use hutan_core::TurnId;
use hutan_core::{CheckpointAvailability, CheckpointInfo, CheckpointState};

use super::markdown_view::Look;
use super::view::ChatView;
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

/// What a turn's checkpoint says right now.
enum Peek {
    /// The daemon has not been asked, or has not answered.
    Waiting,
    /// Nothing for this turn: off, unsupported, no files changed, no checkpoint.
    Nothing,
    Found(Box<CheckpointInfo>, FooterAction),
}

/// What a turn's footer offers for its checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterAction {
    /// The turn can be rewound.
    Rewind,
    /// It stands restored (by any window, or the CLI): Undo restores that restore.
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
        match self.peek_checkpoint(turn, cx) {
            Peek::Found(checkpoint, action) => Some((*checkpoint, action)),
            Peek::Nothing => None,
            Peek::Waiting => {
                // Not asked for yet (or on its way): the store tells this view when it lands.
                let store = self.sessions.read(cx).checkpoints().clone();
                let session = self.session;
                store.update(cx, |store, cx| store.ensure(session, cx));
                None
            }
        }
    }

    /// [`turn_checkpoint`](Self::turn_checkpoint) for what is already loaded: it never asks the
    /// daemon (the hover bar of a message draws from here).
    pub(super) fn turn_checkpoint_loaded(
        &self,
        turn: Option<TurnId>,
        cx: &App,
    ) -> Option<(CheckpointInfo, FooterAction)> {
        match self.peek_checkpoint(turn, cx) {
            Peek::Found(checkpoint, action) => Some((*checkpoint, action)),
            Peek::Nothing | Peek::Waiting => None,
        }
    }

    fn peek_checkpoint(&self, turn: Option<TurnId>, cx: &App) -> Peek {
        let Some(turn) = turn else {
            return Peek::Nothing;
        };
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
            return Peek::Nothing;
        }
        let store = sessions.checkpoints().read(cx);
        let session = self.session;
        if !store.supported() {
            return Peek::Nothing;
        }
        if !store.loaded(session) && store.error(session).is_none() {
            return Peek::Waiting;
        }
        let Some(checkpoint) = store.of_turn(session, turn).cloned() else {
            return Peek::Nothing;
        };
        if checkpoint.files_changed == 0 {
            return Peek::Nothing;
        }
        let action = if !store.restore_supported() || checkpoint.state != CheckpointState::Ok {
            FooterAction::None
        } else {
            match store.restored_by(checkpoint.id) {
                Some(restore) => FooterAction::Undo(restore),
                None => FooterAction::Rewind,
            }
        };
        Peek::Found(Box::new(checkpoint), action)
    }

    /// The footer's line about the files the turn changed, with *Rewind…* or *Undo*, and the
    /// turn's *Fork…* (CHAT-FORK-B) after it; a turn that changed no file has the fork alone.
    pub(super) fn checkpoint_footer(
        &self,
        index: usize,
        turn: Option<TurnId>,
        fork: Option<AnyElement>,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let checkpoint = self.turn_checkpoint(turn, cx);
        if checkpoint.is_none() && fork.is_none() {
            return None;
        }
        let (theme, tokens) = (&look.theme, &look.tokens);
        let session = self.session;
        let (checkpoint, control) = match checkpoint {
            Some((checkpoint, action)) => {
                let control = self.rewind_control(index, session, &checkpoint, action, look);
                (Some(checkpoint), control)
            }
            None => (None, None),
        };
        let separated = checkpoint.is_some();
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
                .when_some(checkpoint, |s, checkpoint| {
                    s.child(SharedString::from(changed_text(&checkpoint)))
                        .when_some(control, |s, control| {
                            s.child(div().child("·")).child(control)
                        })
                        .child(probe(&self.regions, Region::ChatCheckpoint(index)))
                })
                .when_some(fork, |s, fork| {
                    s.when(separated, |s| s.child(div().child("·"))).child(fork)
                })
                .into_any_element(),
        )
    }

    /// *Rewind…* or *Restored — Undo* of a turn's footer.
    fn rewind_control(
        &self,
        index: usize,
        session: hutan_core::SessionId,
        checkpoint: &CheckpointInfo,
        action: FooterAction,
        look: &Look,
    ) -> Option<AnyElement> {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let at = CheckpointAt {
            session,
            checkpoint: checkpoint.id,
        };
        match action {
            FooterAction::Rewind => Some(
                Button::new(("chat-rewind", index), "Rewind…", &look.theme, &look.tokens)
                    .ghost()
                    // A quiet inline action: the compact size, not a toolbar button's.
                    .compact()
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
                            Button::new(
                                ("chat-restored-undo", index),
                                "Undo",
                                &look.theme,
                                &look.tokens,
                            )
                            .ghost()
                            .compact()
                            .child(probe(&self.regions, Region::ChatRewind(index)))
                            .on_click(move |_, window, cx| {
                                window.dispatch_action(Box::new(actions::UndoRestore(undo)), cx);
                            }),
                        )
                        .into_any_element(),
                )
            }
            FooterAction::None => None,
        }
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
            restores: None,
            restores_through: None,
        };
        assert_eq!(changed_text(&checkpoint), "3 files changed · +41 −7");
        checkpoint.files_changed = 1;
        assert_eq!(changed_text(&checkpoint), "1 file changed · +41 −7");
    }
}
