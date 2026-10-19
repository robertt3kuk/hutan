//! The **Today** rail of Mawas Home (`docs/assistant.md` § Home): what needs the user, what is
//! working, what is coming. It is a view of the sessions store — the approvals, questions and
//! agents the daemon already tells every window about — so it needs no polling and no request of
//! its own, and a hidden Home does not draw it at all.
//!
//! *Needs you* lists Mawas's approvals first (each with the options the daemon offered, answered
//! in place), then other agents' approvals, questions and blocked agents. *Working* lists the
//! agents that are running. *Upcoming* says there are no schedules yet: Memory, Skills, Schedules
//! and Traces have no link here until those services exist.

use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use hutan_core::agent::PermissionOptionKind;
use hutan_core::{ApprovalId, QuestionId, SessionId, SessionKind};
use hutan_protocol::messages::AgentState;

use super::assistant::AssistantPanel;
use crate::sessions::{AttentionKind, SessionsStore};
use crate::workbench::actions::OpenSession;
use crate::workbench::chat::{ButtonKind, Look, button};
use crate::workbench::render::probe;
use crate::workbench::ui;
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

/// One row of *Working*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingRow {
    pub session: SessionId,
    pub title: String,
    /// `Claude Code`, `OMP`, `Codex`, the agent that runs.
    pub agent: String,
}

/// What the rail shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Today {
    pub needs_you: Vec<NeedsRow>,
    pub working: Vec<WorkingRow>,
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

    let waiting: std::collections::HashSet<SessionId> =
        needs_you.iter().map(|row| row.session).collect();
    let working = store
        .sessions()
        .iter()
        .filter(|info| info.state.is_live() && !waiting.contains(&info.id))
        .filter(|info| match info.kind {
            SessionKind::Terminal => info
                .agent
                .as_ref()
                .is_some_and(|agent| agent.state == AgentState::Working),
            SessionKind::Harness | SessionKind::Direct => {
                info.chat.as_ref().is_some_and(|chat| chat.turn_active)
            }
        })
        .map(|info| WorkingRow {
            session: info.id,
            title: info.title.clone(),
            agent: match (&info.chat, &info.agent) {
                (Some(chat), _) if chat.is_assistant() => "Mawas".to_owned(),
                (Some(chat), _) => crate::workbench::harnesses::harness_name(&chat.agent),
                (None, Some(agent)) => crate::workbench::harnesses::harness_name(&agent.agent),
                (None, None) => String::new(),
            },
        })
        .collect();
    Today { needs_you, working }
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
            .map(|(index, row)| self.render_needs_row(CHAT_ASK_BASE + index, row, look, cx))
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

    pub(super) fn render_rail(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let today = self.today(cx);
        let heading = |text: &str, count: Option<usize>| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(ui::section_label(text, &theme, &tokens))
                .children(count.filter(|n| *n > 0).map(|n| {
                    div()
                        .px(px(6.0))
                        .rounded(px(crate::theme::PILL_RADIUS))
                        .bg(theme.warning.opacity(0.18))
                        .text_color(theme.warning)
                        .text_size(px(tokens.secondary_font_size))
                        .child(SharedString::from(n.to_string()))
                }))
        };
        let mut needs: Vec<AnyElement> = Vec::new();
        for (row_index, row) in today.needs_you.iter().enumerate() {
            needs.push(self.render_needs_row(row_index, row, look, cx));
        }
        if needs.is_empty() {
            needs.push(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child("Nothing needs you.")
                    .into_any_element(),
            );
        }
        let mut working: Vec<AnyElement> = Vec::new();
        for (index, row) in today.working.iter().enumerate() {
            let session = row.session;
            working.push(
                div()
                    .id(SharedString::from(format!("today-working-{index}")))
                    .relative()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(tokens.spacing))
                    .px(px(tokens.spacing * 2.0))
                    .py(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme.surface_hover))
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(OpenSession(session.to_string())), cx);
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(row.title.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted)
                            .text_size(px(tokens.secondary_font_size))
                            .child(SharedString::from(row.agent.clone())),
                    )
                    .child(probe(
                        &self.env.regions,
                        Region::AssistantItem("today-working", index),
                    ))
                    .into_any_element(),
            );
        }
        if working.is_empty() {
            working.push(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child("No agents are running.")
                    .into_any_element(),
            );
        }
        div()
            .id("assistant-today")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(RAIL_WIDTH))
            .h_full()
            .overflow_y_scroll()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .border_l_1()
            .border_color(theme.border)
            .child(heading("Needs you", Some(today.needs_you.len())))
            .children(needs)
            .child(heading("Working", Some(today.working.len())))
            .children(working)
            .child(heading("Upcoming", None))
            .child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child("No schedules yet."),
            )
            .child(probe(&self.env.regions, Region::Assistant("today")))
            .into_any_element()
    }

    fn render_needs_row(
        &self,
        row_index: usize,
        row: &NeedsRow,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let session = row.session;
        let (label, mawas) = match &row.kind {
            NeedsKind::Approval { mawas, .. } => {
                (if *mawas { "Mawas asks" } else { "Approval" }, *mawas)
            }
            NeedsKind::Question { .. } => ("Question", false),
            NeedsKind::Blocked => ("Blocked", false),
            NeedsKind::Failed => ("Failed", false),
        };
        let mut card = div()
            .id(SharedString::from(format!("today-needs-{row_index}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .p(px(tokens.spacing * 2.0))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(if mawas { theme.accent } else { theme.border })
            .bg(theme.surface)
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
                approval, options, ..
            } => {
                let approval = *approval;
                let buttons: Vec<AnyElement> = options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| {
                        let id = option.id.clone();
                        let kind = match option.kind {
                            PermissionOptionKind::AllowOnce => ButtonKind::Primary,
                            PermissionOptionKind::RejectOnce
                            | PermissionOptionKind::RejectAlways
                            | PermissionOptionKind::Cancel => ButtonKind::Secondary,
                            _ => ButtonKind::Ghost,
                        };
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
                card = card.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(tokens.spacing))
                        .children(buttons),
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
