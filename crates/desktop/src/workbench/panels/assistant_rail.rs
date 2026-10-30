//! The **Today** rail of Mawas Home (`docs/assistant.md` § Home): what waits for the user, what
//! is working, what is coming. It is a view of the sessions store — the agent presentation every
//! surface reads — so it needs no polling and no request of its own, and a hidden Home does not
//! draw it at all.
//!
//! *Requests* lists what the Agents page lists in its *Requests* group, in its order (the one
//! reducer, `sessions::presentation`): each permission request with the options the daemon
//! offered (answered in place; Mawas's own first), each question and plan, and one row for a
//! session that is blocked without saying on what. *Failed* and *Ready for review* list the
//! sessions of those groups (one row each): a finished turn is never a request. *Working* lists
//! the agents that are running — never Mawas itself; a memory proposal is a card with its own
//! *Edit & approve*. *Memory* links to the Memory page; *Upcoming* says there are no schedules
//! yet: Skills, Schedules and Traces have no link here until those services exist.

use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use hutan_core::agent::PermissionOptionKind;
use hutan_core::{ApprovalId, QuestionId, SessionId};

use super::assistant::AssistantPanel;
use super::common::{agent_lead_glyph, agent_state_word, agent_title_line};
use crate::icons::{Icon, icon};
use crate::sessions::{
    AgentRecord, Attention, Group, ReasonKind, Role, SessionsStore, agents_text,
};
use crate::settings::AssistantSettings;
use crate::workbench::actions::{
    EditMemoryProposal, JumpToNextAttention, OpenMawasMemory, OpenSession,
};
use crate::workbench::chat::Look;
use crate::workbench::render::probe;
use crate::workbench::ui::{self, Button, StyledExt as _};
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

/// What a row of Today is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodayKind {
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
    /// An agent's question or a plan to approve.
    Question { question: QuestionId, plan: bool },
    /// An agent is blocked.
    Blocked,
    /// A session failed and nobody has looked.
    Failed,
    /// Work an agent left waits for review, and the user has not seen it.
    Review,
    /// A session finished, or the user marked it unread, and nobody has looked.
    Unread,
}

/// One row of a group of attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodayRow {
    pub session: SessionId,
    /// What is asked, or the session's title.
    pub title: String,
    /// The session's title, or what is wrong.
    pub detail: String,
    pub kind: TodayKind,
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
    /// *Requests*: what waits for an answer (an approval, a question, a plan).
    pub requests: Vec<TodayRow>,
    /// *Failed*: sessions that failed.
    pub failed: Vec<TodayRow>,
    /// *Ready for review*: finished work nobody has seen.
    pub review: Vec<TodayRow>,
    /// Agent sessions confirmed working (the same predicate as the Agents page).
    pub working: Vec<WorkingRow>,
    /// Agent sessions whose state is unavailable: never counted as working or idle.
    pub unconfirmed: usize,
}

/// A section of Today that has something in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// One of the three groups of attention.
    Attention(Attention),
    Working,
}

impl Today {
    /// How many sessions have a request: `requests` grouped by session (a session that asks
    /// twice counts once).
    pub fn request_sessions(&self) -> usize {
        let mut sessions: Vec<SessionId> = self.requests.iter().map(|row| row.session).collect();
        sessions.sort();
        sessions.dedup();
        sessions.len()
    }

    /// The groups of attention in the order Today draws them, with their rows.
    pub fn groups(&self) -> [(Attention, &[TodayRow]); 3] {
        [
            (Attention::Request, &self.requests),
            (Attention::Failed, &self.failed),
            (Attention::Review, &self.review),
        ]
    }

    /// The sections Today draws, in order: only those with content. With none, Today says so in
    /// one muted line instead of four empty sections.
    pub fn sections(&self) -> Vec<Section> {
        let mut sections: Vec<Section> = self
            .groups()
            .into_iter()
            .filter(|(_, rows)| !rows.is_empty())
            .map(|(attention, _)| Section::Attention(attention))
            .collect();
        if !self.working.is_empty() || self.unconfirmed > 0 {
            sections.push(Section::Working);
        }
        sections
    }
}

/// The rail's content as of now, from the sessions store alone: the Agents page's records.
pub fn today(store: &SessionsStore) -> Today {
    let presentation = store.agent_presentation();
    let (mut requests, mut failed, mut review): (Vec<TodayRow>, Vec<TodayRow>, Vec<TodayRow>) =
        (Vec::new(), Vec::new(), Vec::new());
    for record in presentation
        .records
        .iter()
        .filter(|record| record.group.attention().is_some())
    {
        let mawas = record.role == Role::Assistant;
        for request in &record.requests {
            let kind = store
                .approvals()
                .iter()
                .find(|approval| approval.id == request.id)
                .map(|approval| approval.kind);
            let control = kind == Some(hutan_core::agent::PermissionKind::Control);
            requests.push(TodayRow {
                session: record.session,
                title: request.title.clone(),
                detail: record.title.clone(),
                kind: TodayKind::Approval {
                    approval: request.id,
                    options: request
                        .options
                        .iter()
                        .map(|o| RailOption {
                            id: o.id.clone(),
                            label: o.label.clone(),
                            kind: o.kind,
                        })
                        .collect(),
                    mawas,
                    control,
                    memory: control
                        && request
                            .title
                            .starts_with(hutan_protocol::memory::PROPOSAL_TITLE_PREFIX),
                },
            });
        }
        for question in &record.questions {
            requests.push(TodayRow {
                session: record.session,
                title: question.title.clone(),
                detail: record.title.clone(),
                kind: TodayKind::Question {
                    question: question.id,
                    plan: question.is_plan(),
                },
            });
        }
        if !record.requests.is_empty() || !record.questions.is_empty() {
            continue;
        }
        // Answers given here and not confirmed yet leave a session with nothing to ask.
        let Some(reason) = record
            .reasons
            .iter()
            .find(|r| !matches!(r.kind, ReasonKind::Approval | ReasonKind::Question))
        else {
            continue;
        };
        let (kind, rows) = match reason.kind {
            ReasonKind::Approval
            | ReasonKind::Question
            | ReasonKind::Plan
            | ReasonKind::Blocked => (TodayKind::Blocked, &mut requests),
            ReasonKind::Failed => (TodayKind::Failed, &mut failed),
            ReasonKind::Review => (TodayKind::Review, &mut review),
            ReasonKind::CompletedUnread | ReasonKind::MarkedUnread => {
                (TodayKind::Unread, &mut review)
            }
        };
        rows.push(TodayRow {
            session: record.session,
            title: record.title.clone(),
            detail: reason.text.clone(),
            kind,
        });
    }
    // Mawas's requests first; everything else keeps the page's order.
    requests.sort_by_key(|row| !matches!(row.kind, TodayKind::Approval { mawas: true, .. }));

    // Working is what the Agents page calls Working: confirmed, and not waiting for the user.
    // Mawas is the one asking, not an agent at work.
    let agents = || {
        presentation
            .records
            .iter()
            .filter(|record| record.role != Role::Assistant)
    };
    let working = agents()
        .filter(|record| record.group == Group::Working)
        .map(|record| WorkingRow {
            session: record.session,
            record: record.clone(),
        })
        .collect();
    let unconfirmed = agents()
        .filter(|record| record.group == Group::Unconfirmed)
        .count();
    Today {
        requests,
        failed,
        review,
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

    /// Shows or hides the rail, and writes `assistant.rail_open`, so a new page and a new window
    /// start the same way. A request that arrives never reopens it: the header's button counts
    /// them ([`today_badge`](Self::today_badge)).
    pub fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        let open = !self.rail_open;
        self.set_rail_open(open, cx);
    }

    /// Shows or hides the rail (see [`toggle_rail`](Self::toggle_rail)).
    pub fn set_rail_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if let Err(error) = self
            .env
            .settings
            .write_user("assistant.rail_open", toml::Value::Boolean(open))
        {
            tracing::warn!("cannot save assistant.rail_open: {error}");
        }
        // What the file says now is what this page has seen: only a later change by somebody
        // else (another window, the file) moves the rail again.
        self.rail_seen = self.env.settings.get::<AssistantSettings>().rail_open;
        self.rail_open = self.rail_seen;
        cx.notify();
    }

    /// What the header's Today button counts while the rail is hidden: the sessions with a
    /// request (`Today::requests`, grouped by session). Zero while the rail is shown, and on a
    /// page below 640 pt, where the button is not drawn.
    pub fn today_badge(&self, cx: &App) -> usize {
        if self.rail_open || self.folded() {
            return 0;
        }
        self.today(cx).request_sessions()
    }

    /// What Mawas's chat asks Hutan for (a tool of Hutan's own, which its harness cannot show in
    /// the transcript), as cards above the conversation, with the same options as the rail.
    pub(super) fn render_chat_asks(
        &self,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let session = self.session(cx)?;
        let asks: Vec<TodayRow> = self
            .today(cx)
            .requests
            .into_iter()
            .filter(|row| {
                row.session == session
                    && matches!(row.kind, TodayKind::Approval { control: true, .. })
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

    /// Today: only the sections that have something in them — *Requests* (each request with
    /// the options the daemon offered, and *Review requests*), *Failed*, *Ready for review*,
    /// *Working* (the Agents page's rows) — or, with nothing at all, one muted line; and a
    /// `Memory ›` link row.
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
            ui::Button::new(id, text, &theme, &tokens)
                .compact()
                .ghost()
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
        let mut offset = 0;
        for (attention, rows) in today.groups() {
            if rows.is_empty() {
                continue;
            }
            let mut items: Vec<AnyElement> = rows
                .iter()
                .enumerate()
                .map(|(index, row)| self.render_needs_row(offset + index, row, true, look, cx))
                .collect();
            if attention == Attention::Request {
                items.push(
                    link("today-review", "Review requests".into())
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(JumpToNextAttention), cx);
                        })
                        .child(probe(&self.env.regions, Region::Assistant("today-review")))
                        .into_any_element(),
                );
            }
            offset += rows.len();
            sections.push(section(
                format!("{} · {}", attention.label(), rows.len()),
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
                    .child("No requests · nothing working · no schedules")
                    .into_any_element(),
            );
        }
        hutan_ui::scroll_y(
            "assistant-today-scroll",
            div()
                .id("assistant-today")
                .relative()
                .flex()
                .flex_col()
                .h_full()
                .gap(px(tokens.spacing * 3.0))
                .p(px(tokens.spacing * 3.0))
                // The rail is a card: a `surface` fill and no hairline. As a page of its own it
                // is the page's background.
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
                .child(probe(&self.env.regions, Region::Assistant("today"))),
        )
        .into_any_element()
    }

    fn render_needs_row(
        &self,
        row_index: usize,
        row: &TodayRow,
        flat: bool,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let session = row.session;
        let label = match &row.kind {
            TodayKind::Approval { memory: true, .. } => "Memory proposal",
            TodayKind::Approval { mawas: true, .. } => "Mawas asks",
            TodayKind::Approval { .. } | TodayKind::Blocked => "Approval",
            TodayKind::Question { plan: true, .. } => "Plan to approve",
            TodayKind::Question { .. } => "Question",
            TodayKind::Failed => "Failed",
            TodayKind::Review | TodayKind::Unread => "Ready for review",
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
            TodayKind::Approval {
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
                        Button::new(
                            SharedString::from(format!("today-option-{row_index}-{index}")),
                            option.label.clone(),
                            &look.theme,
                            &look.tokens,
                        )
                        .kind(kind)
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
                    Button::new(
                        SharedString::from(format!("today-edit-approve-{row_index}")),
                        "Edit & approve",
                        &look.theme,
                        &look.tokens,
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
            TodayKind::Question { .. }
            | TodayKind::Blocked
            | TodayKind::Failed
            | TodayKind::Review
            | TodayKind::Unread => {
                card = card.child(
                    Button::new(
                        SharedString::from(format!("today-open-{row_index}")),
                        "Open",
                        &look.theme,
                        &look.tokens,
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
