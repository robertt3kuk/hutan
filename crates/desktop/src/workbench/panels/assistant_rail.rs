//! The **Today** rail of Mawas Home (`docs/assistant.md` § Home): what needs the user, what is
//! working, what is coming. It is a view of the sessions store — the approvals, questions and
//! agents the daemon already tells every window about — so it needs no polling and no request of
//! its own, and a hidden Home does not draw it at all.
//!
//! *Needs you* lists Mawas's approvals first (each with the options the daemon offered, answered
//! in place), then other agents' approvals, questions and blocked agents. *Working* lists the
//! agents that are running; a memory proposal is a card with its own *Edit & approve*. *Memory*
//! links to the Memory page; *Upcoming* says there are no schedules yet: Skills, Schedules and
//! Traces have no link here until those services exist.

use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use hutan_core::agent::PermissionOptionKind;
use hutan_core::{ApprovalId, QuestionId, SessionId};

use super::assistant::AssistantPanel;
use super::common::{agent_lead_glyph, agent_state_word, agent_title_line};
use crate::icons::{Icon, icon};
use crate::sessions::{AgentRecord, AttentionKind, SessionsStore, agents_text};
use crate::workbench::actions::{
    EditMemoryProposal, JumpToNextAttention, OpenMawasMemory, OpenSession,
};
use crate::workbench::chat::{ButtonKind, Look, button};
use crate::workbench::render::probe;
use crate::workbench::ui::{self, StyledExt as _};
use crate::workbench::view::Region;

/// The width of the rail.
pub const RAIL_WIDTH: f32 = 300.0;
/// The first row index of the cards above the conversation (the rail's rows count from 0), so
/// that the regions of the two do not meet.
pub const CHAT_ASK_BASE: usize = 100;

/// One answer an approval offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailOption {
    pub id: String,
    pub label: String,
    pub kind: PermissionOptionKind,
}

/// What a *Needs you* row is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeedsKind {
    /// A permission request: Mawas's own (a Hutan tool it asks to use, or its harness's native
    /// tool) or another agent's.
    Approval {
        approval: ApprovalId,
        options: Vec<RailOption>,
        /// The request is Mawas's.
        mawas: bool,
        /// Hutan's own gate asks (`PermissionKind::Control`): the transcript of the chat does
        /// not hold it, so the page shows it above the conversation too.
        control: bool,
        /// A memory proposal (`memory.propose`): besides its options the card offers *Edit &
        /// approve*, which opens the Memory page on it.
        memory: bool,
    },
    /// An agent's question.
    Question { question: QuestionId },
    /// An agent is blocked.
    Blocked,
    /// A session failed and nobody has looked.
    Failed,
}

/// One row of *Needs you*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedsRow {
    pub session: SessionId,
    /// What is asked, or the session's title.
    pub title: String,
    /// The session's title, or what is wrong.
    pub detail: String,
    pub kind: NeedsKind,
}

/// One row of *Working*: the agent record the Agents page draws, so the row is the page's.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkingRow {
    pub session: SessionId,
    pub record: AgentRecord,
}

/// What the rail shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Today {
    pub needs_you: Vec<NeedsRow>,
    /// Agent sessions confirmed working (the same predicate as the Agents page).
    pub working: Vec<WorkingRow>,
    /// Agent sessions whose state is unavailable: never counted as working or idle.
    pub unconfirmed: usize,
}

/// A section of Today that has something in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    NeedsYou,
    Working,
}

impl Today {
    /// The sections Today draws, in order: only those with content. With none, Today says so in
    /// one muted line instead of four empty sections.
    pub fn sections(&self) -> Vec<Section> {
        let mut sections = Vec::new();
        if !self.needs_you.is_empty() {
            sections.push(Section::NeedsYou);
        }
        if !self.working.is_empty() || self.unconfirmed > 0 {
            sections.push(Section::Working);
        }
        sections
    }
}

/// The rail's content as of now, from the sessions store alone.
pub fn today(store: &SessionsStore) -> Today {
    let is_mawas = |session: SessionId| {
        store
            .session(session)
            .and_then(|info| info.chat.as_ref())
            .is_some_and(|chat| chat.is_assistant())
    };
    let title_of = |session: SessionId| {
        store
            .session(session)
            .map_or_else(|| "a session".to_owned(), |info| info.title.clone())
    };
    let mut approvals: Vec<NeedsRow> = store
        .approvals()
        .iter()
        // An answer given here and not yet confirmed by the daemon is not asked again.
        .filter(|approval| store.answered_here(approval.id).is_none())
        .map(|approval| NeedsRow {
            session: approval.session_id,
            title: approval.title.clone(),
            detail: title_of(approval.session_id),
            kind: NeedsKind::Approval {
                approval: approval.id,
                options: approval
                    .options
                    .iter()
                    .map(|o| RailOption {
                        id: o.id.clone(),
                        label: o.label.clone(),
                        kind: o.kind,
                    })
                    .collect(),
                mawas: is_mawas(approval.session_id),
                control: approval.kind == hutan_core::agent::PermissionKind::Control,
                memory: approval.kind == hutan_core::agent::PermissionKind::Control
                    && approval
                        .title
                        .starts_with(hutan_protocol::memory::PROPOSAL_TITLE_PREFIX),
            },
        })
        .collect();
    // Mawas's first; each group keeps the daemon's order (oldest first).
    approvals.sort_by_key(|row| !matches!(row.kind, NeedsKind::Approval { mawas: true, .. }));
    let questions = store
        .questions()
        .iter()
        .filter(|question| !store.question_answered_here(question.id))
        .map(|question| NeedsRow {
            session: question.session_id,
            title: question.spec.title.clone(),
            detail: title_of(question.session_id),
            kind: NeedsKind::Question {
                question: question.id,
            },
        });
    let stuck = store
        .attention_entries()
        .into_iter()
        .filter_map(|entry| match entry.kind {
            AttentionKind::Blocked => Some(NeedsRow {
                session: entry.session,
                title: entry.title,
                detail: entry.detail,
                kind: NeedsKind::Blocked,
            }),
            AttentionKind::Failed => Some(NeedsRow {
                session: entry.session,
                title: entry.title,
                detail: entry.detail,
                kind: NeedsKind::Failed,
            }),
            AttentionKind::Approval | AttentionKind::Question => None,
        });
    let needs_you: Vec<NeedsRow> = approvals
        .into_iter()
        .chain(questions)
        .chain(stuck)
        .collect();

    // Working is what the Agents page calls Working: confirmed, and not waiting for the user.
    let presentation = store.agent_presentation();
    let working = presentation
        .records
        .iter()
        .filter(|record| record.group == crate::sessions::Group::Working)
        .map(|record| WorkingRow {
            session: record.session,
            record: record.clone(),
        })
        .collect();
    let unconfirmed = presentation
        .records
        .iter()
        .filter(|record| record.group == crate::sessions::Group::Unconfirmed)
        .count();
    Today {
        needs_you,
        working,
        unconfirmed,
    }
}

impl AssistantPanel {
    /// The rail's content now (see [`today`]).
    pub fn today(&self, cx: &App) -> Today {
        today(self.env.sessions.read(cx))
    }

    /// Answers a permission request from the rail, through the store (the same call the chat's
    /// own card makes).
    pub fn answer_from_today(
        &mut self,
        approval: ApprovalId,
        option: String,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .env
            .sessions
            .update(cx, |store, cx| store.respond_approval(approval, option, cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    this.set_error(format!("Could not answer: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Whether the rail is showing.
    pub fn rail_open(&self) -> bool {
        self.rail_open
    }

    /// Shows or hides the rail.
    pub fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        self.rail_open = !self.rail_open;
        cx.notify();
    }

    /// What Mawas's chat asks Hutan for (a tool of Hutan's own, which its harness cannot show in
    /// the transcript), as cards above the conversation, with the same options as the rail.
    pub(super) fn render_chat_asks(
        &self,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let session = self.session(cx)?;
        let asks: Vec<NeedsRow> = self
            .today(cx)
            .needs_you
            .into_iter()
            .filter(|row| {
                row.session == session
                    && matches!(row.kind, NeedsKind::Approval { control: true, .. })
            })
            .collect();
        if asks.is_empty() {
            return None;
        }
        let tokens = look.tokens;
        let cards: Vec<AnyElement> = asks
            .iter()
            .enumerate()
            .map(|(index, row)| self.render_needs_row(CHAT_ASK_BASE + index, row, false, look, cx))
            .collect();
        Some(
            div()
                .relative()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(tokens.spacing))
                .p(px(tokens.spacing * 2.0))
                .children(cards)
                .child(probe(&self.env.regions, Region::Assistant("asks")))
                .into_any_element(),
        )
    }

    /// Today: only the sections that have something in them — *Needs you* (each request with
    /// the options the daemon offered, and *Review what needs you*), *Working* (the Agents
    /// page's rows) — or, with nothing at all, one muted line; and a `Memory ›` link row.
    /// `full` is the page below 640 pt, where Today is the whole body instead of a column.
    pub(super) fn render_rail(
        &self,
        look: &Look,
        full: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let today = self.today(cx);
        let hover = theme.surface_hover;
        let link = |id: &'static str, text: SharedString| {
            div()
                .id(id)
                .relative()
                .flex()
                .items_center()
                .gap(px(4.0))
                .h(px(28.0))
                .px(px(tokens.spacing * 2.0))
                .rounded(px(tokens.radius))
                .cursor_pointer()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .hover(move |s| s.bg(hover))
                .child(text)
                .child(icon(Icon::ActionDisclose, 12.0, theme.muted))
        };
        let section = |label: String, items: Vec<AnyElement>| {
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(ui::section_label(label, &theme, &tokens))
                .children(items)
                .into_any_element()
        };
        let listed = today.sections();
        let mut sections: Vec<AnyElement> = Vec::new();
        if listed.contains(&Section::NeedsYou) {
            let mut items: Vec<AnyElement> = today
                .needs_you
                .iter()
                .enumerate()
                .map(|(row_index, row)| self.render_needs_row(row_index, row, true, look, cx))
                .collect();
            items.push(
                link("today-review", "Review what needs you".into())
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(JumpToNextAttention), cx);
                    })
                    .child(probe(&self.env.regions, Region::Assistant("today-review")))
                    .into_any_element(),
            );
            sections.push(section(
                format!("Needs you · {}", today.needs_you.len()),
                items,
            ));
        }
        let mut working: Vec<AnyElement> = Vec::new();
        for (index, row) in today.working.iter().enumerate() {
            let session = row.session;
            working.push(
                div()
                    .id(SharedString::from(format!("today-working-{index}")))
                    .relative()
                    .flex()
                    .items_start()
                    .gap(px(tokens.spacing * 1.5))
                    .px(px(tokens.spacing * 2.0))
                    .py(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(OpenSession(session.to_string())), cx);
                    })
                    .child(
                        div()
                            .pt(px(1.0))
                            .child(agent_lead_glyph(&row.record, &theme)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(agent_title_line(&row.record, &theme, &tokens))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.0))
                                    .h(px(16.0))
                                    .child(agent_state_word(&row.record, false, &theme, &tokens))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(tokens.secondary_font_size))
                                            .text_color(theme.muted)
                                            .child(SharedString::from(format!(
                                                "· {}",
                                                agents_text::harness_text(&row.record)
                                            ))),
                                    ),
                            ),
                    )
                    .child(probe(
                        &self.env.regions,
                        Region::AssistantItem("today-working", index),
                    ))
                    .into_any_element(),
            );
        }
        if today.unconfirmed > 0 {
            // Not "No agents are running": their state is not known.
            working.push(
                link(
                    "today-unconfirmed",
                    format!("State unavailable · {}", today.unconfirmed).into(),
                )
                .on_click(|_, window, cx| {
                    window.dispatch_action(
                        Box::new(crate::workbench::actions::OpenAgentsPageWith(
                            super::AgentsRequest {
                                group: Some("unconfirmed".to_owned()),
                                ..super::AgentsRequest::default()
                            },
                        )),
                        cx,
                    );
                })
                .child(probe(
                    &self.env.regions,
                    Region::Assistant("today-unconfirmed"),
                ))
                .into_any_element(),
            );
        }
        if listed.contains(&Section::Working) {
            sections.push(section(
                if today.working.is_empty() {
                    "Working".to_owned()
                } else {
                    format!("Working · {}", today.working.len())
                },
                working,
            ));
        }
        if listed.is_empty() {
            sections.push(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child("Nothing needs you · nothing working · no schedules")
                    .into_any_element(),
            );
        }
        div()
            .id("assistant-today")
            .relative()
            .flex()
            .flex_col()
            .h_full()
            .overflow_y_scroll()
            .gap(px(tokens.spacing * 3.0))
            .p(px(tokens.spacing * 3.0))
            // The rail is a card: a `surface` fill and no hairline. As a page of its own it is
            // the page's background.
            .when(!full, |s| s.flex_none().w(px(RAIL_WIDTH)).bg(theme.surface))
            .when(full, |s| s.flex_1().min_w_0())
            .children(sections)
            .child(
                link("today-memory", "Memory".into())
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(OpenMawasMemory), cx);
                    })
                    .child(probe(&self.env.regions, Region::Assistant("today-memory"))),
            )
            .child(probe(&self.env.regions, Region::Assistant("today")))
            .into_any_element()
    }

    fn render_needs_row(
        &self,
        row_index: usize,
        row: &NeedsRow,
        flat: bool,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let session = row.session;
        let label = match &row.kind {
            NeedsKind::Approval { memory: true, .. } => "Memory proposal",
            NeedsKind::Approval { mawas: true, .. } => "Mawas asks",
            NeedsKind::Approval { .. } => "Approval",
            NeedsKind::Question { .. } => "Question",
            NeedsKind::Blocked => "Blocked",
            NeedsKind::Failed => "Failed",
        };
        // In the rail a request is a flat item of the card that is the rail itself; above the
        // conversation it is a card of its own. Never a card in a card.
        let mut card = div()
            .id(SharedString::from(format!("today-needs-{row_index}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .when(!flat, |s| {
                s.p(px(tokens.spacing * 2.0)).card(&theme, &tokens)
            })
            .child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(label)),
            )
            .child(
                div()
                    .text_size(px(tokens.font_size))
                    .child(SharedString::from(row.title.clone())),
            );
        if !row.detail.is_empty() {
            card = card.child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .truncate()
                    .child(SharedString::from(row.detail.clone())),
            );
        }
        match &row.kind {
            NeedsKind::Approval {
                approval,
                options,
                memory,
                ..
            } => {
                let (approval, memory) = (*approval, *memory);
                let buttons: Vec<AnyElement> = options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| {
                        let id = option.id.clone();
                        let kind = ui::approval_button_kind(option.kind);
                        button(
                            SharedString::from(format!("today-option-{row_index}-{index}")),
                            option.label.clone(),
                            kind,
                            true,
                            look,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.answer_from_today(approval, id.clone(), cx);
                        }))
                        .child(probe(
                            &self.env.regions,
                            Region::AssistantItem("today-option", row_index * 8 + index),
                        ))
                        .into_any_element()
                    })
                    .collect();
                let edit = memory.then(|| {
                    button(
                        SharedString::from(format!("today-edit-approve-{row_index}")),
                        "Edit & approve",
                        ButtonKind::Secondary,
                        true,
                        look,
                    )
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(
                            Box::new(EditMemoryProposal(approval.to_string())),
                            cx,
                        );
                    })
                    .child(probe(
                        &self.env.regions,
                        Region::AssistantItem("today-edit-approve", row_index),
                    ))
                });
                card = card.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(tokens.spacing))
                        .children(buttons)
                        .children(edit),
                );
            }
            NeedsKind::Question { .. } | NeedsKind::Blocked | NeedsKind::Failed => {
                card = card.child(
                    button(
                        SharedString::from(format!("today-open-{row_index}")),
                        "Open",
                        ButtonKind::Secondary,
                        true,
                        look,
                    )
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(OpenSession(session.to_string())), cx);
                    })
                    .child(probe(
                        &self.env.regions,
                        Region::AssistantItem("today-open", row_index),
                    )),
                );
            }
        }
        card.child(probe(
            &self.env.regions,
            Region::AssistantItem("today-row", row_index),
        ))
        .into_any_element()
    }
}
