//! *Review* on the footer of a finished turn (AGENTS-REVIEW; protocol 0.66, capability
//! `agents.review`): the turn's review packet becomes a chip of Mawas's composer, for the user to
//! read (the chip opens the packet) and to send with a question of their own. Nothing is sent and
//! nothing is read until the user opens the chip or sends the message.
//!
//! Offered for the turns of agent chats (not Mawas's own) when the daemon has the capability and
//! Mawas is on; the button only dispatches `ReviewTurn`, the workbench owns the composer.

use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, div, px};
use hutan_core::TurnId;

use super::fork::ended_turns;
use super::markdown_view::Look;
use super::view::ChatView;
use crate::icons::Icon;
use crate::settings::AssistantSettings;
use crate::workbench::actions::{self, ReviewAt};
use crate::workbench::render::probe;
use crate::workbench::ui::Button;
use crate::workbench::view::Region;

impl ChatView {
    /// Whether the turn buttons offer *Review*: Mawas is on, the daemon can review, and this
    /// chat is an agent's, not an assistant's.
    pub(super) fn review_offered(&self, cx: &App) -> bool {
        if !self.settings.get::<AssistantSettings>().enabled {
            return false;
        }
        let sessions = self.sessions.read(cx);
        let connection = sessions.connection();
        connection.supports_assistant()
            && connection.supports_agents_review()
            && sessions
                .session(self.session)
                .and_then(|info| info.chat.as_ref())
                .is_some_and(|chat| !chat.is_assistant())
    }

    /// *Review* for the footer of the turn `turn` (entry `index`).
    pub(super) fn review_button(
        &self,
        index: usize,
        turn: Option<TurnId>,
        look: &Look,
        cx: &App,
    ) -> Option<AnyElement> {
        let turn = turn?;
        if !self.review_offered(cx) {
            return None;
        }
        let session = self.session;
        let number = ended_turns(
            self.model.read(cx).transcript(),
            !self.model.read(cx).has_earlier(),
        )
        .iter()
        .find(|ended| ended.turn == turn)
        .and_then(|ended| ended.ordinal);
        Some(
            Button::new(("chat-review", index), "Review", &look.theme, &look.tokens)
                .ghost()
                .compact()
                .icon(Icon::ActionAddContext)
                .tooltip("Review this turn: attach what ran, what changed and the signals to Mawas's message")
                .child(probe(&self.regions, Region::ChatReview(index)))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(
                        Box::new(actions::ReviewTurn(ReviewAt {
                            session,
                            turn: Some(turn),
                            number,
                        })),
                        cx,
                    );
                })
                .into_any_element(),
        )
    }

    /// The turn's footer buttons: *Fork…* and *Review*, side by side; none when neither is
    /// offered.
    pub(super) fn turn_actions(
        &self,
        index: usize,
        turn: Option<TurnId>,
        look: &Look,
        cx: &App,
    ) -> Option<AnyElement> {
        let fork = self.fork_button(index, turn, look, cx);
        let review = self.review_button(index, turn, look, cx);
        if fork.is_none() && review.is_none() {
            return None;
        }
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(look.tokens.spacing * 1.5))
                .children(fork)
                .children(review)
                .into_any_element(),
        )
    }
}
