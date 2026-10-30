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

/// A chip that left the queue and is still folding away.
#[derive(Debug)]
pub(super) struct GoneChip {
    id: u64,
    text: String,
    /// It was written by Mawas.
    mawas: bool,
    /// Where it was in the queue.
    index: usize,
    at: std::time::Instant,
}

/// The queue as the last frame drew it, and the chips that left it since.
#[derive(Debug, Default)]
pub(super) struct QueueSeen {
    items: Vec<(u64, String, bool)>,
    leaving: Vec<GoneChip>,
}

impl ChatView {
    /// The queued messages as chips above the composer; `None` while nothing is queued.
    pub(super) fn render_queue(&self, look: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let now_label = self.send_now_label(cx);
        let (items, held, editing) = {
            let model = self.model.read(cx);
            match model.queue() {
                Some(queue) if !queue.items.is_empty() => {
                    (queue.items.clone(), queue.held, model.editing())
                }
                _ => (Vec::new(), None, None),
            }
        };
        // The chips that left since the last frame stay, folding away, for the length of a chip's
        // exit (none for a keyboard action, which `expand` turns instant).
        let now = cx.background_executor().now();
        let reduced = cx.reduce_motion();
        let exit = motion::Kind::Chip.exit_duration(reduced);
        let mut started = false;
        let gone: Vec<(usize, u64, String, bool)> = {
            let mut guard = self.queue_seen.borrow_mut();
            let seen = &mut *guard;
            for (index, (id, text, mawas)) in seen.items.iter().enumerate() {
                if !items.iter().any(|item| item.id == *id)
                    && !seen.leaving.iter().any(|gone| gone.id == *id)
                {
                    seen.leaving.push(GoneChip {
                        id: *id,
                        text: text.clone(),
                        mawas: *mawas,
                        index,
                        at: now,
                    });
                    started = true;
                }
            }
            seen.items = items
                .iter()
                .map(|i| (i.id, i.text.clone(), i.origin.is_some()))
                .collect();
            seen.leaving
                .retain(|gone| now.saturating_duration_since(gone.at) < exit);
            seen.leaving
                .iter()
                .map(|gone| (gone.index, gone.id, gone.text.clone(), gone.mawas))
                .collect()
        };
        if started && !exit.is_zero() {
            // The chip that only fades (Reduce Motion) asks for no frames: draw again once it
            // has left.
            cx.spawn(async move |view, cx| {
                cx.background_executor().timer(exit).await;
                view.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
        if items.is_empty() && gone.is_empty() {
            return None;
        }
        let has_items = !items.is_empty();
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
        // One chip, as the live ones and as the ones that left are drawn. A live chip sits in a
        // fold, open, so that leaving can fold it: its height runs to nothing while it fades and
        // the chips under it rise with it, and the list does not jump at the end. The gap above
        // a chip is part of the fold for the same reason.
        let chip = |index: usize, id: u64, text: &str, open: bool, live: bool, mawas: bool| {
            let wrap = SharedString::from(format!("chat-queue-wrap-{id}"));
            // Under Reduce Motion nothing folds: the chip that left only fades.
            let fading = !live && reduced;
            // What tells a chip that left from the live one at its old place.
            let slot = if live {
                index.to_string()
            } else {
                format!("gone-{id}")
            };
            let small = |name: &'static str, label: &'static str| {
                Button::new(
                    SharedString::from(format!("chat-queue-{name}-{slot}")),
                    label,
                    theme,
                    tokens,
                )
                .ghost()
                .compact()
            };
            let body = div()
                .id(SharedString::from(format!("chat-queue-item-{slot}")))
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
                // Who wrote it when the user did not: Mawas, with the user's approval.
                .children(mawas.then(|| {
                    div()
                        .relative()
                        .flex_none()
                        .px(px(tokens.spacing))
                        .rounded(px(crate::theme::pill_radius()))
                        .border_1()
                        .border_color(theme.border)
                        .text_color(theme.muted)
                        .child(SharedString::from("from Mawas"))
                        .when(live, |s| {
                            s.child(probe(&self.regions, Region::ChatQueue("origin", index)))
                        })
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(open, |s| s.text_color(theme.muted))
                        .child(SharedString::from(if open {
                            "Editing in the composer…".to_owned()
                        } else {
                            one_line(text)
                        })),
                )
                .child(small("edit", "Edit").when(live, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| {
                        this.model.update(cx, |model, cx| model.edit_queued(id, cx));
                    }))
                    .child(probe(&self.regions, Region::ChatQueue("edit", index)))
                }))
                .child(small("now", now_label).when(live, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| {
                        this.model
                            .update(cx, |model, cx| model.send_queued_now(id, cx));
                    }))
                    .child(probe(&self.regions, Region::ChatQueue("now", index)))
                }))
                .child(
                    IconButton::new(
                        SharedString::from(format!("chat-queue-remove-{slot}")),
                        crate::icons::Icon::ActionClose,
                        "Remove from the queue",
                        theme,
                        tokens,
                    )
                    .compact()
                    .when(live, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            this.model
                                .update(cx, |model, cx| model.remove_queued(id, cx));
                        }))
                        .child(probe(&self.regions, Region::ChatQueue("remove", index)))
                    }),
                )
                .when(live, |s| {
                    s.child(probe(&self.regions, Region::ChatQueue("chip", index)))
                })
                .presented(
                    SharedString::from(format!("chat-queue-chip-{id}")),
                    motion::Kind::Chip,
                    !fading,
                    motion::Slide::None,
                );
            let spaced = div().pt(px(tokens.spacing)).child(body);
            if fading {
                spaced.into_any_element()
            } else {
                motion::expand(wrap, live, spaced).into_any_element()
            }
        };
        let mut rows: Vec<AnyElement> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                chip(
                    index,
                    item.id,
                    &item.text,
                    editing == Some(item.id),
                    true,
                    item.origin.is_some(),
                )
            })
            .collect();
        for (index, id, text, mawas) in gone {
            rows.insert(
                index.min(rows.len()),
                chip(index, id, &text, false, false, mawas),
            );
        }
        Some(
            div()
                .flex()
                .flex_col()
                .children(has_items.then_some(header))
                .children(rows)
                .child(probe(&self.regions, Region::Chat("queue-strip")))
                .into_any_element(),
        )
    }
}
