//! The chat's queue above the composer (protocol 0.36, capability `chat.queue`): the messages
//! sent while the agent works, in the order they go, each with Edit (it opens in the composer;
//! the send key saves it), Send now and ✕, and why the queue waits when it holds. The daemon
//! owns the queue; this only draws what it last said and sends the user's choices.

use crate::workbench::ui::motion::{self, PresentExt as _};
use crate::workbench::ui::{Button, IconButton};
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div,
    prelude::*, px,
};
use hutan_protocol::chat::QueueHold;

use super::markdown_view::Look;
use super::view::ChatView;
use crate::workbench::render::probe;
use crate::workbench::view::Region;

/// What a holding queue says about why it waits.
fn held_text(hold: QueueHold) -> &'static str {
    match hold {
        QueueHold::Stopped => "Waiting: you stopped the turn. Send one now to go on.",
        QueueHold::TurnFailed => "Waiting: the last turn failed. Send one now to go on.",
        QueueHold::SendFailed => "Waiting: the first message could not be sent.",
        QueueHold::AgentGone => "Waiting: the agent stopped. Send one now to go on.",
    }
}

/// A queued message on one line: its first line, and `…` when there is more.
fn one_line(text: &str) -> String {
    let mut lines = text.trim().lines();
    let first = lines.next().unwrap_or_default();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_owned()
    }
}

impl ChatView {
    /// The queued messages as chips above the composer; `None` while nothing is queued.
    pub(super) fn render_queue(&self, look: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let now_label = self.send_now_label(cx);
        let (items, held, editing) = {
            let model = self.model.read(cx);
            let queue = model.queue()?;
            if queue.items.is_empty() {
                return None;
            }
            (queue.items.clone(), queue.held, model.editing())
        };
        let header = div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child(SharedString::from(format!("Queued · {}", items.len())))
            .children(held.map(|hold| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(held_text(hold)))
            }))
            .when(held.is_none(), |s| {
                s.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from("sent one by one when the turn ends")),
                )
            })
            .child(
                Button::new("chat-queue-clear", "Clear", &look.theme, &look.tokens)
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model.update(cx, |model, cx| model.clear_queue(cx));
                    }))
                    .child(probe(&self.regions, Region::Chat("queue-clear"))),
            );
        let rows = items.into_iter().enumerate().map(|(index, item)| {
            let id = item.id;
            let open = editing == Some(id);
            let small = |name: &'static str, label: &'static str| {
                Button::new(
                    SharedString::from(format!("chat-queue-{name}-{index}")),
                    label,
                    theme,
                    tokens,
                )
                .ghost()
                .compact()
            };
            div()
                .id(SharedString::from(format!("chat-queue-item-{index}")))
                .relative()
                .flex()
                .items_center()
                .gap(px(tokens.spacing))
                .pl(px(tokens.spacing * 2.0))
                .pr(px(tokens.spacing))
                .h(px(tokens.sidebar_row - 4.0))
                .rounded(px(crate::theme::pill_radius()))
                // The neutral surface; the chip open in the composer is only outlined.
                .when(open, |s| s.border_1().border_color(theme.border))
                .when(!open, |s| s.bg(theme.surface_hover))
                .text_size(px(tokens.secondary_font_size))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(open, |s| s.text_color(theme.muted))
                        .child(SharedString::from(if open {
                            "Editing in the composer…".to_owned()
                        } else {
                            one_line(&item.text)
                        })),
                )
                .child(
                    small("edit", "Edit")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model.update(cx, |model, cx| model.edit_queued(id, cx));
                        }))
                        .child(probe(&self.regions, Region::ChatQueue("edit", index))),
                )
                .child(
                    small("now", now_label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model
                                .update(cx, |model, cx| model.send_queued_now(id, cx));
                        }))
                        .child(probe(&self.regions, Region::ChatQueue("now", index))),
                )
                .child(
                    IconButton::new(
                        SharedString::from(format!("chat-queue-remove-{index}")),
                        crate::icons::Icon::ActionClose,
                        "Remove from the queue",
                        theme,
                        tokens,
                    )
                    .compact()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model
                            .update(cx, |model, cx| model.remove_queued(id, cx));
                    }))
                    .child(probe(&self.regions, Region::ChatQueue("remove", index))),
                )
                .child(probe(&self.regions, Region::ChatQueue("chip", index)))
                .presented(
                    SharedString::from(format!("chat-queue-chip-{id}")),
                    motion::Kind::Chip,
                    true,
                    motion::Slide::None,
                )
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(header)
                .children(rows)
                .child(probe(&self.regions, Region::Chat("queue-strip")))
                .into_any_element(),
        )
    }
}
