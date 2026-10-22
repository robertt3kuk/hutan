//! The row of a session on the Agents page (chrome v3 § 2.3, § 2.5): the 64 pt line (state,
//! title, reason or status, location, harness and model, time, the main action and More), the
//! details below it on Space, and the exact request with its option buttons on *Review request*.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use hutan_core::SessionId;
use hutan_core::agent::PermissionOptionKind;

use super::{AgentsPage, Layout, Look};
use crate::icons::{Icon, glyph, icon};
use crate::sessions::agents_text::{self, Primary};
use crate::sessions::presentation::AgentRecord;
use crate::workbench::actions::{
    InterruptSessionById, SessionMenuAt, ShowAgentMenu, StopSessionById,
};
use crate::workbench::agents_page::primary_action;
use crate::workbench::panels::common::{
    agent_lead_glyph, agent_state_word, agent_title_line, now_ms,
};
use crate::workbench::render::probe;
use crate::workbench::style::TextTooltip;
use crate::workbench::ui::{self, ButtonKind, FocusRingExt as _};
use crate::workbench::view::Region;

/// A fixed-width column of the wide layout.
const LOCATION_WIDTH: f32 = 200.0;
const HARNESS_WIDTH: f32 = 150.0;
const TIME_WIDTH: f32 = 130.0;

/// `label: value` of the details.
fn fact(label: &str, value: impl Into<SharedString>, look: Look) -> AnyElement {
    div()
        .flex()
        .gap(px(12.0))
        .child(
            div()
                .flex_none()
                .w(px(110.0))
                .text_color(look.theme.muted)
                .child(SharedString::from(label.to_owned())),
        )
        .child(div().flex_1().min_w_0().child(value.into()))
        .into_any_element()
}

/// What a status line says after the state word: nothing when it only repeats it, the rest when
/// it starts with it (`State unavailable · no report` after `State unavailable`).
fn after_word(status: &str, word: &str) -> Option<String> {
    if status == word {
        return None;
    }
    let rest = status
        .strip_prefix(word)
        .and_then(|rest| rest.strip_prefix(" · "))
        .unwrap_or(status);
    Some(rest.to_owned())
}

impl AgentsPage {
    pub(super) fn agent_row(
        &self,
        session: SessionId,
        selected: bool,
        focused: bool,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(record) = self.record(session) else {
            // The record went while the order is frozen: nothing to draw, nothing to click.
            return div().into_any_element();
        };
        let (theme, tokens) = (look.theme, look.tokens);
        let now = now_ms();
        let layout = self.layout;
        let hover = theme.surface_hover;
        let primary = agents_text::primary(record);
        let needs = record.group.attention().is_some();
        // The reason without the kind the state word already says; the activity otherwise.
        let status = after_word(
            &agents_text::reason_detail(record).unwrap_or_else(|| agents_text::status_line(record)),
            &agents_text::state_word(record),
        );
        // Where the location and the harness are no columns, they follow on the status line
        // (compact) or on a third line (comfortable): never both a column and a repeat.
        let tail = match layout {
            Layout::Wide => None,
            Layout::Medium | Layout::Narrow => {
                let mut tail = record.location();
                if look.comfortable
                    && let Some(checkout) = &record.checkout
                {
                    tail.push_str(" · ");
                    tail.push_str(&checkout.label);
                }
                tail.push_str(" · ");
                tail.push_str(&agents_text::harness_text(record));
                Some(tail)
            }
        };
        let duration = agents_text::duration_cell(record, now);
        let updated = agents_text::updated_text(record, now);
        let updated_tip = agents_text::updated_tooltip(record);
        let children = agents_text::children_text(record);
        let expanded = self.details.contains(&session);
        let reviewing = self.reviewing == Some(session);

        // ---- the identity: title and the one state word, then the reason or the status ----
        let reason_tip = agents_text::reason_tooltip(record, now).map(SharedString::from);
        let title_line = agent_title_line(record, &theme, &tokens);
        let mut status_line = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .h(px(16.0))
            .text_size(px(tokens.secondary_font_size))
            .child(agent_state_word(record, selected, &theme, &tokens))
            .children(status.map(|status| {
                div()
                    .id(SharedString::from(format!("agents-reason-{session}")))
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted)
                    .when_some(reason_tip, |s, tip| {
                        s.tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                    })
                    .child(SharedString::from(format!("· {status}")))
            }))
            // Seen work waiting for review: not a state, a link to the checkout's changes.
            .when(agents_text::changes_link(record), |line| {
                line.child(
                    div()
                        .id(SharedString::from(format!("agents-changes-{session}")))
                        .relative()
                        .flex_none()
                        .cursor_pointer()
                        .text_color(theme.accent)
                        .hover(|s| s.underline())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_session_row(session);
                            window.dispatch_action(
                                primary_action(session, Primary::ReviewChanges),
                                cx,
                            );
                        }))
                        .child(SharedString::from(format!(
                            "· {}",
                            agents_text::CHANGES_TO_REVIEW
                        )))
                        .child(probe(
                            &self.env.regions,
                            Region::AgentAction(session, "changes-to-review"),
                        )),
                )
            });
        if let Some(tail) = tail.as_ref().filter(|_| !look.comfortable) {
            status_line = status_line.child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_color(theme.muted)
                    .child(SharedString::from(format!("· {tail}"))),
            );
        }
        if let Some(children) = children {
            let open = self.children_open.contains(&session);
            status_line = status_line.child(
                div()
                    .id(SharedString::from(format!("agents-children-{session}")))
                    .relative()
                    .flex_none()
                    .cursor_pointer()
                    .text_color(if record.subagents_blocked > 0 {
                        theme.warning
                    } else {
                        theme.muted
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_session_row(session);
                        this.toggle_children(session, cx);
                    }))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .child(SharedString::from(children.to_string()))
                    .child(glyph(if open {
                        Icon::ActionExpand
                    } else {
                        Icon::ActionDisclose
                    }))
                    .child(probe(
                        &self.env.regions,
                        Region::AgentAction(session, "children"),
                    )),
            );
        }
        let mut identity = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(2.0))
            .child(title_line)
            .child(status_line);
        if let Some(third) = tail.filter(|_| look.comfortable) {
            identity = identity.child(
                div()
                    .h(px(16.0))
                    .truncate()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(third)),
            );
        }

        // ---- the columns that the width allows ----
        let location_column = (layout == Layout::Wide).then(|| {
            div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(LOCATION_WIDTH))
                .min_w_0()
                .gap(px(2.0))
                .child(
                    div()
                        .h(px(16.0))
                        .truncate()
                        .child(SharedString::from(record.location())),
                )
                .children(record.checkout.as_ref().map(|checkout| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .h(px(16.0))
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(if checkout.shared {
                            theme.warning
                        } else {
                            theme.muted
                        })
                        .child(icon(Icon::VcsBranch, 12.0, theme.muted))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(checkout.label.clone())),
                        )
                }))
        });
        let harness_column = (layout == Layout::Wide).then(|| {
            div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(HARNESS_WIDTH))
                .min_w_0()
                .gap(px(2.0))
                .child(
                    div()
                        .h(px(16.0))
                        .truncate()
                        .child(SharedString::from(record.harness.clone())),
                )
                // A missing model is not a column: "Model not reported" lives in the details.
                // The model reads by name; the tooltip carries its id.
                .children(record.model.as_ref().map(|model| {
                    let id = record
                        .model_id
                        .clone()
                        .filter(|id| id != model)
                        .map(SharedString::from);
                    div()
                        .id(SharedString::from(format!("agents-model-{session}")))
                        .h(px(16.0))
                        .truncate()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(model.clone()))
                        .when_some(id, |s, id| {
                            s.tooltip(move |_, cx| cx.new(|_| TextTooltip(id.clone())).into())
                        })
                }))
        });
        let time_column = (layout != Layout::Narrow).then(|| {
            let duration_tip = SharedString::from(duration.tooltip.clone());
            let updated_tip = updated_tip.clone().map(SharedString::from);
            div()
                .flex()
                .flex_col()
                .items_end()
                .flex_none()
                .w(px(TIME_WIDTH))
                .gap(px(2.0))
                .text_color(theme.muted)
                .child(
                    div()
                        .id(SharedString::from(format!("agents-duration-{session}")))
                        .h(px(16.0))
                        .tooltip(move |_, cx| cx.new(|_| TextTooltip(duration_tip.clone())).into())
                        .child(SharedString::from(duration.text.clone())),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("agents-updated-{session}")))
                        .h(px(16.0))
                        .text_size(px(tokens.secondary_font_size))
                        .when_some(updated_tip, |s, tip| {
                            s.tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                        })
                        .child(SharedString::from(updated.clone())),
                )
        });

        // ---- the actions: the main one, and More ----
        let show_primary = layout != Layout::Narrow || selected;
        let primary_button = show_primary.then(|| {
            ui::button(
                SharedString::from(format!("agents-primary-{session}")),
                primary.label(),
                ButtonKind::Secondary,
                true,
                &theme,
                &tokens,
            )
            .relative()
            .h(px(28.0))
            .when(primary == Primary::ReviewRequest, |s| {
                s.when(reviewing, |s| s.bg(theme.accent_soft))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_session_row(session);
                match primary {
                    Primary::ReviewRequest => this.toggle_review(session, cx),
                    other => window.dispatch_action(primary_action(session, other), cx),
                }
            }))
            .child(probe(
                &self.env.regions,
                Region::AgentAction(session, "primary"),
            ))
        });
        let more = div()
            .id(SharedString::from(format!("agents-more-{session}")))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(28.0))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.select_session_row(session);
                window.dispatch_action(
                    Box::new(ShowAgentMenu(SessionMenuAt {
                        session: session.to_string(),
                        position: event.position(),
                    })),
                    cx,
                );
            }))
            .tooltip(|_, cx| cx.new(|_| TextTooltip("More".into())).into())
            .child(icon(Icon::ActionMore, 16.0, theme.muted))
            .child(probe(
                &self.env.regions,
                Region::AgentAction(session, "more"),
            ));
        let actions = div()
            .id(SharedString::from(format!("agents-actions-{session}")))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.0))
            .children(primary_button)
            .child(more)
            // The order of the rows is frozen while the pointer rests here.
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.hovering_action != *hovered {
                    this.hovering_action = *hovered;
                    if !*hovered {
                        cx.notify();
                    }
                }
            }));

        let main = div()
            .id(SharedString::from(format!("agents-row-{session}")))
            .relative()
            .flex()
            .items_center()
            .gap(px(12.0))
            .h(px(look.row()))
            .px(px(16.0))
            .cursor_pointer()
            .when(selected, |s| s.bg(theme.accent_soft))
            .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
            // The keyboard on the selected row: the focus ring, not a colored edge.
            .focus_ring(selected && focused, &theme, &look.tokens)
            .when(record.state.is_finished() && !needs, |s| s.opacity(0.75))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.select_session_row(session);
                if event.click_count() >= 2 {
                    this.open_session(session, window, cx);
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.select_session_row(session);
                    window.dispatch_action(
                        Box::new(ShowAgentMenu(SessionMenuAt {
                            session: session.to_string(),
                            position: event.position,
                        })),
                        cx,
                    );
                }),
            )
            .child(agent_lead_glyph(record, &theme))
            .child(identity)
            .children(location_column)
            .children(harness_column)
            .children(time_column)
            .child(actions)
            .child(probe(&self.env.regions, Region::AgentRow(session)));

        // A hairline between rows, inset to the 16 px spine the lanes start at.
        div()
            .flex()
            .flex_col()
            .w_full()
            .child(main)
            .children(expanded.then(|| self.details_block(record, look, cx)))
            .children(reviewing.then(|| self.review_block(record, look, cx)))
            .child(div().mx(px(16.0)).h(px(1.0)).bg(theme.border))
            .into_any_element()
    }

    /// The facts behind a row, below it: where every number and word comes from, and what is
    /// not reported.
    fn details_block(
        &self,
        record: &AgentRecord,
        look: Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let now = now_ms();
        let session = record.session;
        let mut lines: Vec<AnyElement> = Vec::new();
        let state = if record.detail.is_empty() {
            record.state.label().to_owned()
        } else {
            format!("{} · {}", record.state.label(), record.detail)
        };
        lines.push(fact("State", state, look));
        for provenance in &record.provenance {
            lines.push(fact("", provenance.clone(), look));
        }
        for reason in &record.reasons {
            lines.push(fact(reason.kind.label(), reason.text.clone(), look));
        }
        lines.push(fact(
            "Harness",
            match (&record.model, &record.model_id) {
                (Some(model), Some(id)) if id != model => {
                    format!("{} · {model} ({id})", record.harness)
                }
                (Some(model), _) => format!("{} · {model}", record.harness),
                (None, _) => format!("{} · Model not reported", record.harness),
            },
            look,
        ));
        lines.push(fact("Location", record.location(), look));
        match &record.checkout {
            Some(checkout) => lines.push(fact(
                "Checkout",
                format!("{} ({})", checkout.label, checkout.root.display()),
                look,
            )),
            None => {
                if let Some(folder) = &record.folder {
                    lines.push(fact("Folder", folder.display().to_string(), look));
                }
            }
        }
        let duration = agents_text::duration_cell(record, now);
        lines.push(fact(
            "Duration",
            if duration.text == "—" {
                format!("— ({})", duration.tooltip)
            } else {
                format!("{} ({})", duration.text, duration.tooltip)
            },
            look,
        ));
        let updated = agents_text::updated_text(record, now);
        if !updated.is_empty() {
            lines.push(fact(
                "Activity",
                match agents_text::updated_tooltip(record) {
                    Some(at) => format!("{updated} ({at})"),
                    None => updated,
                },
                look,
            ));
        }
        lines.push(fact(
            "Usage",
            record
                .usage
                .clone()
                .unwrap_or_else(|| "Usage not reported".to_owned()),
            look,
        ));
        lines.push(fact(
            "Subagents",
            match agents_text::children_text(record) {
                Some(text) => text,
                None if !self
                    .env
                    .sessions
                    .read(cx)
                    .connection()
                    .supports_agent_tree() =>
                {
                    "This daemon does not expose subagents. Update hutand to see them.".to_owned()
                }
                None => "None observed".to_owned(),
            },
            look,
        ));
        lines.push(fact("Session", session.to_string(), look));

        let caps = self.caps(cx);
        let interrupt = agents_text::interrupt(record, caps);
        let stop = agents_text::stop(record, caps);
        let command =
            |id: String, label: &'static str, avail: &agents_text::Avail, region: &'static str| {
                let enabled = avail.is_yes();
                let why = match avail {
                    agents_text::Avail::No(why) => Some(SharedString::from(*why)),
                    agents_text::Avail::Yes => None,
                };
                ui::button(
                    SharedString::from(id),
                    label,
                    ButtonKind::Secondary,
                    enabled,
                    &theme,
                    &tokens,
                )
                .relative()
                .h(px(28.0))
                .when_some(why, |s, why| {
                    s.tooltip(move |_, cx| cx.new(|_| TextTooltip(why.clone())).into())
                })
                .child(probe(
                    &self.env.regions,
                    Region::AgentAction(session, region),
                ))
            };
        let interrupt_button = command(
            format!("agents-interrupt-{session}"),
            "Interrupt turn",
            &interrupt,
            "interrupt",
        )
        .when(interrupt.is_yes(), |s| {
            s.on_click(move |_, window, cx| {
                window.dispatch_action(Box::new(InterruptSessionById(session.to_string())), cx);
            })
        });
        let stop_button = command(
            format!("agents-stop-{session}"),
            "Stop session…",
            &stop,
            "stop",
        )
        .when(stop.is_yes(), |s| {
            s.on_click(move |_, window, cx| {
                window.dispatch_action(Box::new(StopSessionById(session.to_string())), cx);
            })
        });
        div()
            .id(SharedString::from(format!("agents-details-{session}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .pl(px(16.0 + 20.0 + 12.0))
            .pr(px(16.0))
            .pb(px(12.0))
            .text_size(px(tokens.secondary_font_size))
            .children(lines)
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .pt(px(6.0))
                    .child(interrupt_button)
                    .child(stop_button),
            )
            .child(probe(&self.env.regions, Region::AgentsPage("details")))
            .into_any_element()
    }

    /// The exact request with its options: the only place a request is answered from the page.
    fn review_block(&self, record: &AgentRecord, look: Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let session = record.session;
        let online = self.caps(cx).online;
        let mut blocks: Vec<AnyElement> = Vec::new();
        let mut option_index = 0usize;
        for request in &record.requests {
            let approval = request.id;
            let mut buttons: Vec<AnyElement> = Vec::new();
            for option in &request.options {
                let id = option.id.clone();
                let ix = option_index;
                option_index += 1;
                let kind = match option.kind {
                    PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways => {
                        ButtonKind::Secondary
                    }
                    _ => ButtonKind::Ghost,
                };
                let button = ui::button(
                    SharedString::from(format!("agents-option-{session}-{ix}")),
                    SharedString::from(option.label.clone()),
                    kind,
                    online,
                    &theme,
                    &tokens,
                )
                .relative()
                .h(px(28.0))
                .child(probe(&self.env.regions, Region::AgentOption(session, ix)))
                .when(online, |s| {
                    s.on_click(cx.listener(move |this, _, _, cx| {
                        this.answer(session, approval, id.clone(), cx);
                    }))
                });
                buttons.push(button.into_any_element());
            }
            blocks.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(SharedString::from(format!("Permission: {}", request.title))),
                    )
                    .children(request.detail.as_ref().map(|detail| {
                        div()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(detail.clone()))
                    }))
                    .child(div().flex().flex_wrap().gap(px(8.0)).children(buttons))
                    .into_any_element(),
            );
        }
        for ask in &record.questions {
            blocks.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(SharedString::from(format!("Question: {}", ask.title))),
                    )
                    .child(
                        ui::button(
                            SharedString::from(format!("agents-answer-{session}")),
                            "Answer question",
                            ButtonKind::Secondary,
                            true,
                            &theme,
                            &tokens,
                        )
                        .h(px(28.0))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.open_session(session, window, cx);
                            },
                        )),
                    )
                    .into_any_element(),
            );
        }
        if record.submitting > 0 {
            blocks.push(
                div()
                    .text_color(theme.muted)
                    .child(SharedString::from(if record.submitting == 1 {
                        "Sending your answer…".to_owned()
                    } else {
                        format!("Sending {} answers…", record.submitting)
                    }))
                    .into_any_element(),
            );
        }
        if blocks.is_empty() {
            blocks.push(
                div()
                    .text_color(theme.muted)
                    .child("Nothing is waiting for an answer here any more (it was answered elsewhere).")
                    .into_any_element(),
            );
        }
        if !online {
            blocks.push(
                div()
                    .text_color(theme.warning)
                    .child("Offline: an answer needs the daemon.")
                    .into_any_element(),
            );
        }
        if let Some((failed, message)) = &self.answer_error
            && *failed == session
        {
            blocks.push(
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(message.clone()))
                    .into_any_element(),
            );
        }
        div()
            .id(SharedString::from(format!("agents-review-{session}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .mx(px(16.0))
            .mb(px(10.0))
            .p(px(12.0))
            .rounded(px(tokens.card_radius))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .text_size(px(tokens.font_size))
            .children(blocks)
            .child(probe(&self.env.regions, Region::AgentsPage("review")))
            .into_any_element()
    }

    /// Selects the row of `session` by a click or a button on it.
    pub(super) fn select_session_row(&mut self, session: SessionId) {
        if let Some(ix) = self
            .listing
            .position(&crate::sessions::agents_list::Key::Agent(session))
        {
            self.select_row(Some(ix));
        }
    }

    /// What the connection lets a row do (the page reads the store directly).
    fn caps(&self, cx: &gpui::App) -> agents_text::Caps {
        let store = self.env.sessions.read(cx);
        agents_text::Caps {
            online: store.connection_state() == hutan_client::ConnectionState::Connected,
            chat: store.connection().supports_chat(),
        }
    }
}
