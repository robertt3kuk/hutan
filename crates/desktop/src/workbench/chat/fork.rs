//! Fork from a turn, in the chat view (CHAT-FORK-B; protocol 0.56, ADR 0023): the *Fork…* of a
//! turn's footer, the hover bar of a user's message (copy · fork · rewind), the header chip
//! *Forked from “…” · turn N* that opens the parent, and which turn the transcript is showing
//! (for ⌘K *Chat: Fork from Here*). Nothing without the daemon's capability `chat.fork`, nothing
//! for an assistant, and no button while a turn runs in the chat (the daemon refuses a fork then:
//! the footer says so in a tooltip instead of failing).
//!
//! The buttons only dispatch `ForkChat` with the turn; the workbench owns the popover and talks
//! to the daemon (`workbench/fork.rs`).

use gpui::{
    AnyElement, App, ClipboardItem, InteractiveElement, IntoElement, ParentElement, SharedString,
    Styled, div, px,
};
use hutan_chat_view::{Body, Entry, Transcript};
use hutan_core::{ForkSupport, TurnId};
use hutan_protocol::messages::SessionInfo;

use super::markdown_view::Look;
use super::rewind::FooterAction;
use super::view::ChatView;
use crate::icons::Icon;
use crate::sessions::fork::chip_text;
use crate::theme::{Theme, Tokens};
use crate::workbench::actions::{self, CheckpointAt, ForkAt};
use crate::workbench::render::probe;
use crate::workbench::ui::{Button, IconButton};
use crate::workbench::view::Region;

/// A turn of the transcript that has ended: its place and where its footer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndedTurn {
    pub turn: TurnId,
    /// Counted from 1 among the chat's turns; `None` while the transcript does not start at the
    /// chat's first turn (the earlier ones are not loaded: the place would be a guess).
    pub ordinal: Option<u32>,
    /// The footer's entry.
    pub footer: usize,
    /// No turn runs and none came after: a fork here is a fork at the chat's head.
    pub newest: bool,
}

/// The turns of `transcript` that ended, oldest first. `complete`: the transcript starts at the
/// chat's first event.
pub fn ended_turns(transcript: &Transcript, complete: bool) -> Vec<EndedTurn> {
    let mut out: Vec<EndedTurn> = Vec::new();
    for (index, entry) in transcript.entries().iter().enumerate() {
        let (Body::Footer(_), Some(turn)) = (&entry.body, entry.turn) else {
            continue;
        };
        if out.iter().any(|ended| ended.turn == turn) {
            continue;
        }
        let ordinal = complete.then(|| u32::try_from(out.len() + 1).unwrap_or(u32::MAX));
        out.push(EndedTurn {
            turn,
            ordinal,
            footer: index,
            newest: false,
        });
    }
    if let Some(last) = out.last_mut() {
        last.newest = !transcript.turn_active();
    }
    out
}

/// The turn entry `index` belongs to: its own, else the first one after it that has one (a
/// user's message is journaled before its turn starts) — but never past the next message of the
/// user.
pub fn turn_of_entry(entries: &[Entry], index: usize) -> Option<TurnId> {
    let entry = entries.get(index)?;
    if entry.turn.is_some() {
        return entry.turn;
    }
    entries[index + 1..]
        .iter()
        .take_while(|next| !matches!(next.body, Body::User(_)))
        .find_map(|next| next.turn)
}

/// What the Fork buttons do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Not offered: an older daemon, an assistant, a chat that cannot be forked.
    Hidden,
    /// A turn runs: forking waits for it.
    Waiting,
    Ready,
}

impl ChatView {
    /// Whether the Fork buttons are offered, and whether they work right now.
    pub(super) fn fork_gate(&self, cx: &App) -> Gate {
        let sessions = self.sessions.read(cx);
        if !sessions.supports_chat_fork() {
            return Gate::Hidden;
        }
        let Some(chat) = sessions.session(self.session).and_then(|s| s.chat.as_ref()) else {
            return Gate::Hidden;
        };
        if chat.is_assistant()
            || chat
                .fork
                .as_ref()
                .is_some_and(|fork| fork.support == ForkSupport::None)
        {
            return Gate::Hidden;
        }
        if chat.turn_active {
            Gate::Waiting
        } else {
            Gate::Ready
        }
    }

    /// *Fork…* for the footer of the turn `turn` (entry `index`).
    pub(super) fn fork_button(
        &self,
        index: usize,
        turn: Option<TurnId>,
        look: &Look,
        cx: &App,
    ) -> Option<AnyElement> {
        let turn = turn?;
        let gate = self.fork_gate(cx);
        if gate == Gate::Hidden {
            return None;
        }
        let session = self.session;
        let button = Button::new(("chat-fork", index), "Fork…", &look.theme, &look.tokens)
            .ghost()
            .compact()
            .icon(Icon::ActionFork)
            .tooltip(match gate {
                Gate::Waiting => "A turn is running: fork when it ends",
                _ => "Fork: a new chat with the conversation up to this turn",
            })
            .disabled(gate == Gate::Waiting)
            .child(probe(&self.regions, Region::ChatFork(index)));
        Some(
            button
                .on_click(move |event, window, cx| {
                    window.dispatch_action(
                        Box::new(actions::ForkChat(ForkAt {
                            session,
                            turn: Some(turn),
                            position: event.position(),
                        })),
                        cx,
                    );
                })
                .into_any_element(),
        )
    }

    /// The bar over the corner of a user's message when the pointer is on it: copy the message,
    /// fork from its turn, rewind its files. `group` is the hover group of the bubble.
    pub(super) fn message_bar(
        &self,
        index: usize,
        turn: Option<TurnId>,
        typed: &str,
        group: SharedString,
        look: &Look,
        cx: &App,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let session = self.session;
        let copied = typed.to_owned();
        let regions = &self.regions;
        let copy = IconButton::new(
            ("chat-message-copy", index),
            Icon::ActionCopy,
            "Copy message",
            theme,
            tokens,
        )
        .compact()
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
        })
        .child(probe(regions, Region::ChatMessageAction(index, "copy")));
        // A turn that has not ended has no footer yet: nothing to fork or rewind.
        let gate = self.fork_gate(cx);
        let ended = turn.filter(|turn| {
            ended_turns(self.model.read(cx).transcript(), false)
                .iter()
                .any(|ended| ended.turn == *turn)
        });
        let fork = ended.filter(|_| gate != Gate::Hidden).map(|turn| {
            IconButton::new(
                ("chat-message-fork", index),
                Icon::ActionFork,
                if gate == Gate::Waiting {
                    "A turn is running: fork when it ends"
                } else {
                    "Fork from this turn…"
                },
                theme,
                tokens,
            )
            .compact()
            .disabled(gate == Gate::Waiting)
            .on_click(move |event, window, cx| {
                window.dispatch_action(
                    Box::new(actions::ForkChat(ForkAt {
                        session,
                        turn: Some(turn),
                        position: event.position(),
                    })),
                    cx,
                );
            })
            .child(probe(regions, Region::ChatMessageAction(index, "fork")))
        });
        let rewind = turn
            .and_then(|turn| self.turn_checkpoint_loaded(Some(turn), cx))
            .filter(|(_, action)| *action == FooterAction::Rewind)
            .map(|(checkpoint, _)| {
                let at = CheckpointAt {
                    session,
                    checkpoint: checkpoint.id,
                };
                IconButton::new(
                    ("chat-message-rewind", index),
                    Icon::ActionRecover,
                    "Rewind this turn's files…",
                    theme,
                    tokens,
                )
                .compact()
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(actions::RewindCheckpoint(at)), cx);
                })
                .child(probe(regions, Region::ChatMessageAction(index, "rewind")))
            });
        div()
            .absolute()
            .top(px(-tokens.spacing * 3.0))
            .right(px(tokens.spacing * 2.0))
            .flex()
            .items_center()
            .gap(px(2.0))
            .px(px(2.0))
            .rounded(px(tokens.radius))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .opacity(0.0)
            .group_hover(group, |s| s.opacity(1.0))
            .child(copy)
            .children(fork)
            .children(rewind)
            .child(probe(regions, Region::ChatMessageAction(index, "bar")))
            .into_any_element()
    }

    /// The turn the reader is at: the newest while the transcript follows its end, else the
    /// turn of the last entry that is not below the window. Only a turn that has ended counts;
    /// `None` without one.
    pub fn turn_in_view(&self, cx: &App) -> Option<TurnId> {
        let model = self.model.read(cx);
        let transcript = model.transcript();
        let ended = ended_turns(transcript, !model.has_earlier());
        let newest = ended.last().map(|e| e.turn);
        if self.list.is_following_tail() {
            return newest;
        }
        let entries = transcript.entries();
        let here = (0..entries.len())
            .rev()
            .find(|ix| self.list.item_is_below_viewport(*ix) == Some(false));
        here.and_then(|ix| turn_of_entry(entries, ix))
            .filter(|turn| ended.iter().any(|e| e.turn == *turn))
            .or(newest)
    }

    /// The header chip of a fork: *Forked from “Parent” · turn 4*, which opens the parent. A
    /// parent that is closed or deleted leaves plain words.
    pub(super) fn forked_from_chip(
        &self,
        info: &SessionInfo,
        narrow: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &App,
    ) -> Option<AnyElement> {
        let origin = info.forked_from.as_ref()?;
        let parent = self.sessions.read(cx).session(origin.session_id);
        let title = parent.map(|p| p.title.clone());
        let text = if narrow {
            match origin.ordinal {
                0 => "Forked".to_owned(),
                n => format!("Forked · turn {n}"),
            }
        } else {
            chip_text(title.as_deref(), origin.ordinal)
        };
        let tip = match &title {
            Some(title) => format!("Open the original, “{title}”"),
            None => "The original is archived or deleted: Reopen it from History".to_owned(),
        };
        let button = Button::new("chat-forked-from", text, theme, tokens)
            .ghost()
            .compact()
            .icon(Icon::ActionFork)
            .max_width(320.0)
            .tooltip(tip)
            .disabled(title.is_none())
            .child(probe(&self.regions, Region::Chat("forked-from")));
        let parent_id = origin.session_id;
        Some(
            div()
                .flex_none()
                .min_w_0()
                .child(button.on_click(move |_, window, cx| {
                    window
                        .dispatch_action(Box::new(actions::OpenSession(parent_id.to_string())), cx);
                }))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::SessionId;
    use hutan_core::agent::{AgentEvent, AgentEventKind, Delivery, ProviderIds};

    fn event(seq: u64, session: SessionId, turn: TurnId, kind: AgentEventKind) -> AgentEvent {
        AgentEvent {
            session_id: session,
            seq,
            ts_ms: seq,
            delivery: Delivery::Live,
            turn_id: Some(turn),
            item_id: None,
            provider: ProviderIds::default(),
            kind,
            payload: None,
        }
    }

    #[test]
    fn ended_turns_are_counted_from_the_first_and_only_the_last_is_the_head() {
        let session = SessionId::new();
        let (a, b) = (TurnId::new(), TurnId::new());
        let mut transcript = Transcript::new();
        let mut seq = 0;
        let mut push = |t: &mut Transcript, turn, kind| {
            seq += 1;
            t.apply(&event(seq, session, turn, kind));
        };
        for turn in [a, b] {
            push(&mut transcript, turn, AgentEventKind::TurnStarted);
            push(
                &mut transcript,
                turn,
                AgentEventKind::TurnCompleted { duration_ms: None },
            );
        }
        let ended = ended_turns(&transcript, true);
        assert_eq!(ended.len(), 2);
        assert_eq!(
            (ended[0].turn, ended[0].ordinal, ended[0].newest),
            (a, Some(1), false)
        );
        assert_eq!(
            (ended[1].turn, ended[1].ordinal, ended[1].newest),
            (b, Some(2), true)
        );
        // Not from the chat's start: the place is unknown.
        assert_eq!(ended_turns(&transcript, false)[1].ordinal, None);
        // A turn running now makes no ended turn the head.
        push(&mut transcript, TurnId::new(), AgentEventKind::TurnStarted);
        assert!(ended_turns(&transcript, true).iter().all(|e| !e.newest));
    }
}
