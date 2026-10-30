//! The **Today** rail of Mawas Home (`docs/assistant.md` § Home): what waits for the user, what
//! is working, what is coming. It is a view of the sessions store — the agent presentation every
//! surface reads — so it needs no polling and no request of its own, and a hidden Home does not
//! draw it at all.
//!
//! *Requests* lists what the Agents page lists in its *Requests* group, in its order (the one
//! reducer, `hutan_agents_view::presentation`): each permission request with the options the daemon
//! offered (answered in place; Mawas's own first), each question and plan, and one row for a
//! session that is blocked without saying on what. *Failed* and *Ready for review* list the
//! sessions of those groups (one row each): a finished turn is never a request. *Working* lists
//! the agents that are running — never Mawas itself; a memory proposal is a card with its own
//! *Edit & approve*. *Idle* lists the agents that wait for a message, folded until opened.
//!
//! The rail has two tabs (`assistant.rail_tab`, remembered): **Now**, the board of every agent
//! as above, and **Memory**, what waits for the user in Mawas's memory (the proposals), its latest
//! entries and the link to the Memory page. Its width is `assistant.rail_width_rem`, dragged on
//! its left border and written on release.

use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use hutan_core::agent::PermissionOptionKind;
use hutan_core::{ApprovalId, QuestionId, SessionId};
use hutan_protocol::activity::{
    ActivityDetail, ActivityEvent, ActivitySummary, RequestKind, TurnOutcome,
};
use hutan_protocol::chat::ControlTarget;
use hutan_protocol::suggest::{AskTarget, SuggestedAnswer};

use super::assistant::AssistantPanel;
use super::common::{agent_lead_glyph, agent_state_word, agent_title_line};
use crate::icons::{Icon, icon};
use crate::sessions::{SessionsStore, agents_text};
use crate::settings::{AssistantSettings, RailTab};
use crate::workbench::actions::{
    EditMemoryProposal, JumpToNextAttention, OpenMawasMemory, OpenSession,
};
use crate::workbench::chat::{self, Look};
use crate::workbench::render::probe;
use crate::workbench::ui::{self, Button, StyledExt as _};
use crate::workbench::view::Region;
use hutan_agents_view::attention::Attention;
use hutan_agents_view::presentation::{AgentRecord, Group, ReasonKind, Role};

/// The most events the Recent tab lists under one agent.
const RECENT_EVENTS_PER_AGENT: usize = 3;
/// The shortest pause between two reads of the Recent tab caused by the agents changing.
/// How far back the Flow section of the Recent tab looks.
const FLOW_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const RECENT_REFRESH: std::time::Duration = std::time::Duration::from_secs(2);
/// How many memory entries the Memory tab lists.
const MEMORY_ENTRIES_SHOWN: usize = 6;
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

/// What Mawas suggests for an approval, as a row of Today shows it (protocol 0.64).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RailSuggestion {
    /// The option she would choose.
    pub option_id: String,
    /// Its label.
    pub label: String,
    pub reason: String,
}

/// What a row of Today is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodayKind {
    /// A permission request: Mawas's own (a Hutan tool it asks to use, or its harness's native
    /// tool) or another agent's.
    Approval {
        approval: ApprovalId,
        /// Protocol 0.62: the agent a request of Mawas acts on, and the whole text it would
        /// send; absent for every other approval.
        target: Option<ControlTarget>,
        /// Protocol 0.64: what Mawas suggests for this request, when she did and the user has
        /// not dismissed it; advice only.
        suggestion: Option<RailSuggestion>,
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
    /// Agent sessions confirmed idle (the Agents page's *Idle* group): waiting for a message.
    pub idle: Vec<WorkingRow>,
}

/// A section of Today that has something in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// One of the three groups of attention.
    Attention(Attention),
    Working,
    /// Idle agents, folded until opened.
    Idle,
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

    /// How many memory proposals wait (the Memory tab's count): requests of the
    /// `memory.propose` tool, whose card carries *Edit & approve*.
    pub fn memory_proposals(&self) -> usize {
        self.requests
            .iter()
            .filter(|row| matches!(row.kind, TodayKind::Approval { memory: true, .. }))
            .count()
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
        if !self.idle.is_empty() {
            sections.push(Section::Idle);
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
                    suggestion: store
                        .approval_suggestion(request.id)
                        .and_then(|suggestion| {
                            let SuggestedAnswer::Option { option_id } = &suggestion.answer else {
                                return None;
                            };
                            let option = request.options.iter().find(|o| o.id == *option_id)?;
                            Some(RailSuggestion {
                                option_id: option_id.clone(),
                                label: option.label.clone(),
                                reason: suggestion.reason.clone(),
                            })
                        }),
                    target: store
                        .approvals()
                        .iter()
                        .find(|approval| approval.id == request.id)
                        .and_then(|approval| approval.target.clone()),
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
    let idle = agents()
        .filter(|record| record.group == Group::Idle)
        .map(|record| WorkingRow {
            session: record.session,
            record: record.clone(),
        })
        .collect();
    Today {
        requests,
        failed,
        review,
        working,
        unconfirmed,
        idle,
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

    /// Today, with its two tabs: **Now** (only the sections that have something in them —
    /// *Requests* (each request with the options the daemon offered, and *Review requests*),
    /// *Failed*, *Ready for review*, *Working* (the Agents page's rows), *Idle* (folded) — or,
    /// with nothing at all, one muted line) and **Memory** (what waits in Mawas's memory).
    /// `full` is the page below 640 pt, where Today is the whole body instead of a column;
    /// `rem_px` is the size of a rem, which the column's width is written in.
    pub(super) fn render_rail(
        &self,
        look: &Look,
        full: bool,
        rem_px: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = look.theme;
        let today = self.today(cx);
        let tab = self.env.settings.get::<AssistantSettings>().rail_tab;
        let body = match tab {
            RailTab::Now => self.render_now(&today, look, cx),
            RailTab::Recent => self.render_recent(look, cx),
            RailTab::Memory => self.render_memory(&today, look, cx),
        };
        let width = self.rail_width_rem() * rem_px;
        div()
            .id("assistant-today")
            .relative()
            .flex()
            .flex_col()
            .h_full()
            // The rail is a card: a `surface` fill. On a light pack that fill is 1.05:1 against
            // the page, so its left edge carries the cards' hairline (`Theme::card_edge`); on a
            // dark pack the fill alone separates it. As a page of its own it is the page's
            // background.
            .when(!full, |s| {
                s.flex_none()
                    .w(px(width))
                    .bg(theme.surface)
                    // Drawn over the rail's first pixel column, not as a border: the rail's
                    // width (and the page beside it) stays what the setting says.
                    .children(rail_edge(&theme).map(|edge| {
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(px(1.0))
                            .bg(edge)
                    }))
            })
            .when(full, |s| s.flex_1().min_w_0())
            .child(self.render_rail_tabs(tab, &today, look, cx))
            .child(body)
            .when(!full, |s| s.child(self.rail_handle(look, cx)))
            .child(probe(&self.env.regions, Region::Assistant("today")))
            .into_any_element()
    }

    /// The tab strip: **Now** with the number of sessions with a request, **Memory** with the
    /// number of proposals that wait.
    fn render_rail_tabs(
        &self,
        current: RailTab,
        today: &Today,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let tab = |id: &'static str, which: RailTab, label: &str, count: usize| {
            let selected = current == which;
            let text = if count > 0 {
                format!("{label} · {count}")
            } else {
                label.to_owned()
            };
            div()
                .id(id)
                .relative()
                .px(px(tokens.spacing * 2.0))
                .py(px(tokens.spacing))
                .rounded(px(tokens.radius))
                .cursor_pointer()
                .text_size(px(tokens.font_size))
                .text_color(if selected { theme.fg } else { theme.muted })
                .when(selected, |s| s.bg(theme.surface_hover))
                .hover(move |s| s.bg(theme.surface_hover))
                .child(SharedString::from(text))
                .on_click(cx.listener(move |this, _, _, cx| this.set_rail_tab(which, cx)))
                .child(probe(&self.env.regions, Region::Assistant(id)))
        };
        div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 3.0))
            .pt(px(tokens.spacing * 2.0))
            .child(tab(
                "today-tab-now",
                RailTab::Now,
                "Now",
                today.request_sessions(),
            ))
            .child(tab(
                "today-tab-recent",
                RailTab::Recent,
                "Recent",
                self.recent_count(),
            ))
            .child(tab(
                "today-tab-memory",
                RailTab::Memory,
                "Memory",
                today.memory_proposals(),
            ))
            .child(probe(&self.env.regions, Region::Assistant("today-tabs")))
            .into_any_element()
    }

    /// Opens a tab of the rail and writes `assistant.rail_tab`, so the next page starts there.
    pub fn set_rail_tab(&mut self, tab: RailTab, cx: &mut Context<Self>) {
        let word = match tab {
            RailTab::Now => "now",
            RailTab::Recent => "recent",
            RailTab::Memory => "memory",
        };
        if let Err(error) = self
            .env
            .settings
            .write_user("assistant.rail_tab", toml::Value::String(word.to_owned()))
        {
            tracing::warn!("cannot save assistant.rail_tab: {error}");
        }
        cx.notify();
    }

    /// The rail's left border: a strip that takes the drag of its width (a double click puts the
    /// width back).
    fn rail_handle(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let theme = look.theme;
        let (hover, held) = (theme.accent.opacity(0.45), theme.accent.opacity(0.8));
        let dragging = self.resizing_rail();
        div()
            .id("rail-edge")
            .group("rail-edge")
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(px(6.0))
            .cursor_col_resize()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.start_rail_drag(event, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(2.0))
                    .group_hover("rail-edge", move |s| s.bg(hover))
                    .when(dragging, move |s| s.bg(held)),
            )
            .child(probe(&self.env.regions, Region::Assistant("rail-edge")))
            .into_any_element()
    }

    /// One agent of *Working* or *Idle*: the Agents page's row, with what its state is worth
    /// (`exact`: a chat the daemon runs; `state only`: a terminal agent's own report).
    fn render_agent_row(
        &self,
        id: &'static str,
        index: usize,
        row: &WorkingRow,
        look: &Look,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let hover = theme.surface_hover;
        let session = row.session;
        let worth = if row.record.managed {
            "exact"
        } else {
            "state only"
        };
        div()
            .id(SharedString::from(format!("{id}-{index}")))
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
                                        "· {} · {worth}",
                                        agents_text::harness_text(&row.record)
                                    ))),
                            ),
                    ),
            )
            .child(probe(&self.env.regions, Region::AssistantItem(id, index)))
            .into_any_element()
    }

    /// The Now tab: the board.
    fn render_now(&self, today: &Today, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
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
        let mut working: Vec<AnyElement> = today
            .working
            .iter()
            .enumerate()
            .map(|(index, row)| self.render_agent_row("today-working", index, row, look))
            .collect();
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
        if listed.contains(&Section::Idle) {
            // Idle agents are not news: one folded line, the rows when it is opened.
            let open = self.idle_open;
            let mut items: Vec<AnyElement> = vec![
                link(
                    "today-idle",
                    format!(
                        "Idle · {}{}",
                        today.idle.len(),
                        if open { "" } else { " (hidden)" }
                    )
                    .into(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.idle_open = !this.idle_open;
                    cx.notify();
                }))
                .child(probe(&self.env.regions, Region::Assistant("today-idle")))
                .into_any_element(),
            ];
            if open {
                items.extend(
                    today.idle.iter().enumerate().map(|(index, row)| {
                        self.render_agent_row("today-idle-row", index, row, look)
                    }),
                );
            }
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .children(items)
                    .into_any_element(),
            );
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
                .id("assistant-today-now")
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(tokens.spacing * 3.0))
                .p(px(tokens.spacing * 3.0))
                .children(sections),
        )
        .into_any_element()
    }

    /// The Memory tab: the proposals that wait for the user (answered in place; *Edit &
    /// approve* opens the Memory page on one), the latest entries Mawas keeps, and the link to
    /// the page.
    fn render_memory(&self, today: &Today, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let mut sections: Vec<AnyElement> = Vec::new();
        let proposals: Vec<(usize, &TodayRow)> = today
            .requests
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row.kind, TodayKind::Approval { memory: true, .. }))
            .collect();
        if !proposals.is_empty() {
            let cards: Vec<AnyElement> = proposals
                .iter()
                .map(|(index, row)| self.render_needs_row(*index, row, true, look, cx))
                .collect();
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing))
                    .child(ui::section_label(
                        format!("Waiting for you · {}", proposals.len()),
                        &theme,
                        &tokens,
                    ))
                    .children(cards)
                    .into_any_element(),
            );
        }
        let entries: Vec<AnyElement> = if self.memory.unsupported {
            vec![muted_line(
                "This daemon keeps no memory (it predates protocol 0.32).",
                look,
            )]
        } else if !self.memory.loaded && self.memory.entries.is_empty() {
            vec![muted_line("Reading Mawas's memory…", look)]
        } else if self.memory.entries.is_empty() {
            vec![muted_line("Nothing remembered yet.", look)]
        } else {
            self.memory
                .entries
                .iter()
                .take(MEMORY_ENTRIES_SHOWN)
                .enumerate()
                .map(|(index, entry)| {
                    let first = entry.body.lines().next().unwrap_or("").trim().to_owned();
                    div()
                        .relative()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .px(px(tokens.spacing * 2.0))
                        .child(
                            div()
                                .text_size(px(tokens.font_size))
                                .truncate()
                                .child(SharedString::from(first)),
                        )
                        .child(
                            div()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(SharedString::from(format!(
                                    "{}{}",
                                    category_word(entry.category),
                                    if entry.inferred {
                                        " · written by Mawas"
                                    } else {
                                        ""
                                    }
                                ))),
                        )
                        .child(probe(
                            &self.env.regions,
                            Region::AssistantItem("today-memory-entry", index),
                        ))
                        .into_any_element()
                })
                .collect()
        };
        sections.push(
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(ui::section_label(
                    "Latest entries".to_owned(),
                    &theme,
                    &tokens,
                ))
                .children(entries)
                .into_any_element(),
        );
        sections.push(
            ui::Button::new("today-memory", "Open Memory", &theme, &tokens)
                .compact()
                .ghost()
                .child(icon(Icon::ActionDisclose, 12.0, theme.muted))
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(OpenMawasMemory), cx);
                })
                .child(probe(&self.env.regions, Region::Assistant("today-memory")))
                .into_any_element(),
        );
        hutan_ui::scroll_y(
            "assistant-today-scroll",
            div()
                .id("assistant-today-memory")
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(tokens.spacing * 3.0))
                .p(px(tokens.spacing * 3.0))
                .children(sections),
        )
        .into_any_element()
    }

    /// The catch-up the Recent tab shows, once the daemon answered.
    pub fn recent_activity(&self) -> Option<&hutan_protocol::activity::AgentsActivity> {
        self.recent.activity.as_ref()
    }

    /// How many events the Recent tab has since the user last caught up (zero until read).
    fn recent_count(&self) -> usize {
        self.recent
            .activity
            .as_ref()
            .map_or(0, |activity| activity.events.len() + activity.omitted)
    }

    /// The Recent tab: what happened since the user last caught up, from the daemon's records
    /// (`agents.activity`, no model call): the counts, the events grouped by agent, what is not
    /// recorded, *Summarise with Mawas* (a prepared ask in the composer; nothing is sent) and
    /// *Mark as seen*.
    fn render_recent(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let mut sections: Vec<AnyElement> = Vec::new();
        let now = hutan_client::time::now_ms();
        match (&self.recent.activity, self.recent.unsupported) {
            (_, true) => sections.push(muted_line(
                "This daemon keeps no activity feed (it predates protocol 0.65).",
                look,
            )),
            (None, _) => sections.push(muted_line(
                self.recent
                    .error
                    .as_deref()
                    .unwrap_or("Reading what happened…"),
                look,
            )),
            (Some(activity), _) => {
                let since = match self.recent.for_seen {
                    Some(Some(seen)) => format!(
                        "Since you last looked: {} ({})",
                        hutan_client::time::local_time_text(seen),
                        hutan_client::time::relative_text(now, seen)
                    ),
                    _ => "The last 24 hours (nothing marked as seen yet)".to_owned(),
                };
                sections.push(
                    div()
                        .relative()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(since))
                        .child(probe(
                            &self.env.regions,
                            Region::Assistant("today-recent-since"),
                        ))
                        .into_any_element(),
                );
                let lines = recent_summary_lines(&activity.summary);
                if lines.is_empty() {
                    sections.push(muted_line("Nothing recorded in this window.", look));
                } else {
                    sections.push(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .text_size(px(tokens.font_size))
                            .children(lines.into_iter().map(SharedString::from))
                            .into_any_element(),
                    );
                }
                sections.push(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(tokens.spacing))
                        .child(
                            Button::new(
                                "today-recent-summarise",
                                "Summarise with Mawas",
                                &theme,
                                &tokens,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.summarise_recent(cx)))
                            .child(probe(
                                &self.env.regions,
                                Region::Assistant("today-recent-summarise"),
                            )),
                        )
                        .child(
                            Button::new("today-recent-seen", "Mark as seen", &theme, &tokens)
                                .on_click(cx.listener(|this, _, _, cx| this.mark_recent_seen(cx)))
                                .child(probe(
                                    &self.env.regions,
                                    Region::Assistant("today-recent-seen"),
                                )),
                        )
                        .into_any_element(),
                );
                if let Some(flow) = &self.recent.flow {
                    sections.push(self.render_flow(flow, look, cx));
                }
                // Events by agent, newest agent first (the events are newest first).
                let mut groups: Vec<(Option<SessionId>, String, String, Vec<&ActivityEvent>)> =
                    Vec::new();
                for event in &activity.events {
                    match groups
                        .iter_mut()
                        .find(|g| g.0 == event.session_id && g.1 == event.title)
                    {
                        Some(group) => group.3.push(event),
                        None => groups.push((
                            event.session_id,
                            event.title.clone(),
                            event.project.clone(),
                            vec![event],
                        )),
                    }
                }
                for (index, (session, title, project, events)) in groups.iter().enumerate() {
                    let rows: Vec<AnyElement> = events
                        .iter()
                        .take(RECENT_EVENTS_PER_AGENT)
                        .map(|event| {
                            div()
                                .flex()
                                .gap(px(tokens.spacing))
                                .text_size(px(tokens.secondary_font_size))
                                .child(div().flex_none().w(px(64.0)).text_color(theme.muted).child(
                                    SharedString::from(hutan_client::time::relative_text(
                                        now,
                                        event.at_ms,
                                    )),
                                ))
                                .child(
                                    div().flex_1().min_w_0().child(SharedString::from(
                                        recent_event_text(&event.detail),
                                    )),
                                )
                                .into_any_element()
                        })
                        .collect();
                    let more = events.len().saturating_sub(RECENT_EVENTS_PER_AGENT);
                    let open = *session;
                    sections.push(
                        div()
                            .id(SharedString::from(format!("today-recent-group-{index}")))
                            .relative()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .when(open.is_some(), |s| {
                                s.cursor_pointer().on_click(move |_, window, cx| {
                                    if let Some(session) = open {
                                        window.dispatch_action(
                                            Box::new(OpenSession(session.to_string())),
                                            cx,
                                        );
                                    }
                                })
                            })
                            .child(
                                div()
                                    .text_size(px(tokens.font_size))
                                    .truncate()
                                    .child(SharedString::from(format!("{title} · {project}"))),
                            )
                            .children(rows)
                            .children((more > 0).then(|| {
                                div()
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(format!("+{more} more")))
                            }))
                            .child(probe(
                                &self.env.regions,
                                Region::AssistantItem("today-recent-group", index),
                            ))
                            .into_any_element(),
                    );
                }
                if activity.omitted > 0 {
                    sections.push(muted_line(
                        &format!(
                            "{} older events are not listed; the counts above cover them.",
                            activity.omitted
                        ),
                        look,
                    ));
                }
                if activity
                    .coverage_from_ms
                    .is_some_and(|from| from > activity.from_ms)
                {
                    sections.push(muted_line(
                        "Hutan's records begin after the start of this window.",
                        look,
                    ));
                }
                if !activity.not_recorded.is_empty() {
                    sections.push(
                        div()
                            .relative()
                            .child(muted_line(
                                &format!("Not recorded: {}.", activity.not_recorded.join("; ")),
                                look,
                            ))
                            .child(probe(
                                &self.env.regions,
                                Region::Assistant("today-recent-not-recorded"),
                            ))
                            .into_any_element(),
                    );
                }
            }
        }
        hutan_ui::scroll_y(
            "assistant-today-scroll",
            div()
                .id("assistant-today-recent")
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(tokens.spacing * 3.0))
                .p(px(tokens.spacing * 3.0))
                .children(sections),
        )
        .into_any_element()
    }

    /// *Mark as seen*: the user is caught up with what the last read showed. What *since I was
    /// away* counts from is kept in the state file, per window's user, never in the daemon.
    pub fn mark_recent_seen(&mut self, cx: &mut Context<Self>) {
        let Some(to_ms) = self.recent.activity.as_ref().map(|a| a.to_ms) else {
            return;
        };
        self.env
            .sessions
            .update(cx, |store, cx| store.see_agents(to_ms, cx));
        cx.notify();
    }

    /// The Flow section of the Recent tab: waits on the user, failures that came again and tokens
    /// by project over the last seven days, from `agents.stats`, each line with how well it is
    /// known. *Analyse with Mawas* prepares an ask; nothing is sent.
    fn render_flow(
        &self,
        flow: &hutan_protocol::stats::AgentsStats,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let lines = flow_lines(flow);
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .child(
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child("Flow · the last 7 days"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .text_size(px(tokens.font_size))
                    .children(lines.into_iter().map(SharedString::from)),
            )
            .child(
                div().flex().child(
                    Button::new("today-flow-analyse", "Analyse with Mawas", &theme, &tokens)
                        .on_click(cx.listener(|this, _, _, cx| this.analyse_flow(cx)))
                        .child(probe(
                            &self.env.regions,
                            Region::Assistant("today-flow-analyse"),
                        )),
                ),
            )
            .child(probe(&self.env.regions, Region::Assistant("today-flow")))
            .into_any_element()
    }

    /// The flow statistics the Recent tab shows, once read.
    pub fn recent_flow(&self) -> Option<&hutan_protocol::stats::AgentsStats> {
        self.recent.flow.as_ref()
    }

    /// *Analyse with Mawas*: puts a prepared ask in Mawas's composer, for the user to read,
    /// change and send; nothing is sent. The ask names the read-only tool, so the numbers in her
    /// answer come from the records and carry their coverage.
    pub fn analyse_flow(&mut self, cx: &mut Context<Self>) {
        let note = "Analyse how my agents worked over the last 7 days (agents_stats with last_days 7, grouped by project and again by day). Tell me where the time went, what they waited for me on and for how long, which failures came again, and what the tokens were spent on. Say how well each number is known and what is not recorded; do not guess what the records do not hold.";
        if let Some(model) = self
            .session(cx)
            .and_then(|session| self.chat_model(session))
        {
            model.update(cx, |model, cx| model.add_note(note, cx));
        }
        self.focus_wanted = true;
        cx.notify();
    }

    /// *Summarise with Mawas*: puts a prepared ask in Mawas's composer, for the user to read,
    /// change and send; nothing is sent. The ask names the window and the read-only tools that
    /// answer it, so the numbers in her answer come from the records and not from this tab.
    pub fn summarise_recent(&mut self, cx: &mut Context<Self>) {
        let Some(activity) = self.recent.activity.as_ref() else {
            return;
        };
        let note = format!(
            "Summarise what my agents did since {} (agents_activity with since_ms {}, then agents_board for what is happening now). Tell me what finished, what failed and what waits for me, and say what is not recorded.",
            hutan_client::time::local_time_text(activity.from_ms),
            activity.from_ms
        );
        if let Some(model) = self
            .session(cx)
            .and_then(|session| self.chat_model(session))
        {
            model.update(cx, |model, cx| model.add_note(&note, cx));
        }
        self.focus_wanted = true;
        cx.notify();
    }

    /// Whether the empty conversation of Mawas is on screen: the *Catch up* chip would be drawn,
    /// so what it says is wanted.
    fn catch_up_wanted(&self, cx: &App) -> bool {
        let Some(model) = self
            .session(cx)
            .and_then(|session| self.chat_model(session))
        else {
            return false;
        };
        let model = model.read(cx);
        matches!(model.status(), crate::workbench::chat::LoadStatus::Ready)
            && model.transcript().is_empty()
    }

    /// What Mawas Home's empty conversation offers: the line of what happened since the user
    /// last caught up (the Recent tab's own read, `ActivitySummary::line`), or nothing. Set on
    /// the shown chat every frame; nothing here asks a model.
    pub(super) fn sync_catch_up(&self, cx: &mut Context<Self>) {
        let Some(view) = self.session(cx).and_then(|session| self.chat_view(session)) else {
            return;
        };
        let line = self
            .recent
            .activity
            .as_ref()
            .and_then(|activity| activity.summary.line());
        let chip = line.map(|line| {
            let panel = cx.entity().downgrade();
            chat::CatchUp {
                text: line.into(),
                open: std::rc::Rc::new(move |_window, cx| {
                    panel.update(cx, |panel, cx| panel.open_recent(cx)).ok();
                }),
            }
        });
        view.read(cx).set_catch_up(chip);
    }

    /// The *Catch up* chip's click: the rail's Recent tab, opening the rail first when it is
    /// hidden (below 640 pt, where Today is a page of its own, that page).
    pub fn open_recent(&mut self, cx: &mut Context<Self>) {
        self.set_rail_tab(RailTab::Recent, cx);
        if self.folded() {
            if !self.today_only() {
                self.toggle_today_only(cx);
            }
        } else if !self.rail_open {
            self.set_rail_open(true, cx);
        }
    }

    /// Reads the catch-up once the Recent tab is on screen or the *Catch up* chip is wanted,
    /// again when the point the user caught up at moved, and again — at most every two seconds —
    /// when the agents' state changed. A hidden rail with a conversation under way, and the other
    /// tabs, do no work.
    pub(super) fn ensure_recent(&mut self, cx: &mut Context<Self>) {
        let wanted = (self.rail_open
            && self.env.settings.get::<AssistantSettings>().rail_tab == RailTab::Recent)
            || self.catch_up_wanted(cx);
        if !wanted || self.recent.loading || self.recent.unsupported || !self.connected(cx) {
            return;
        }
        let (seen, revision, connection) = {
            let store = self.env.sessions.read(cx);
            // What the catch-up is made of changing: the agents' state (the revision) and the
            // number of sessions and of requests (a plain shell starting moves neither the
            // revision nor an agent).
            (
                store.agents_seen(),
                (
                    store.revision(),
                    store.sessions().len(),
                    store.approvals().len() + store.questions().len(),
                ),
                store.connection().clone(),
            )
        };
        if !connection.supports_agents_activity() {
            self.recent.unsupported = true;
            return;
        }
        let fresh = self.recent.activity.is_some()
            && self.recent.for_seen == Some(seen)
            && self.recent.revision == revision;
        if fresh {
            return;
        }
        if let Some(last) = self.recent.last_load
            && self.recent.for_seen == Some(seen)
            && last.elapsed() < RECENT_REFRESH
        {
            // Too soon after the last read: look again when the pause is over.
            if !self.recent.retry {
                self.recent.retry = true;
                let wait = RECENT_REFRESH.saturating_sub(last.elapsed());
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    this.update(cx, |this, cx| {
                        this.recent.retry = false;
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            return;
        }
        self.recent.loading = true;
        self.recent.last_load = Some(std::time::Instant::now());
        cx.spawn(async move |this, cx| {
            let answer = connection
                .agents_activity(hutan_protocol::activity::AgentsActivityRequest {
                    since_ms: seen,
                    ..Default::default()
                })
                .await;
            // The flow of the last week goes with the catch-up; a daemon without it shows none.
            let flow = if connection.supports_agents_stats() {
                connection
                    .agents_stats(hutan_protocol::stats::AgentsStatsRequest {
                        since_ms: Some(hutan_client::time::now_ms().saturating_sub(FLOW_WINDOW_MS)),
                        ..Default::default()
                    })
                    .await
                    .map_err(|error| tracing::warn!("cannot read the agents' flow: {error}"))
                    .ok()
            } else {
                None
            };
            this.update(cx, |this, cx| {
                this.recent.loading = false;
                this.recent.flow = flow;
                this.recent.for_seen = Some(seen);
                this.recent.revision = revision;
                match answer {
                    Ok(activity) => {
                        this.recent.error = None;
                        this.recent.activity = Some(activity);
                    }
                    Err(error) => {
                        tracing::warn!("cannot read the agents' activity: {error}");
                        this.recent.error = Some(format!("Could not read what happened: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reads the latest memory entries once the Memory tab is on screen (and again after
    /// `memory.changed`). A hidden rail, and the Now tab, do no work.
    pub(super) fn ensure_memory(&mut self, cx: &mut Context<Self>) {
        let wanted = self.rail_open
            && self.env.settings.get::<AssistantSettings>().rail_tab == RailTab::Memory;
        if !wanted || self.memory.loaded || !self.connected(cx) {
            return;
        }
        if self.memory.loading {
            self.memory.again = true;
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        if !connection.supports_memory() {
            self.memory.unsupported = true;
            self.memory.loaded = true;
            return;
        }
        self.memory.loading = true;
        let workspace = self.env.sessions.read(cx).active_workspace().map(|w| w.id);
        cx.spawn(async move |this, cx| {
            let entries = connection
                .memory_list(hutan_protocol::memory::MemoryList {
                    workspace_id: workspace,
                    ..Default::default()
                })
                .await;
            this.update(cx, |this, cx| {
                this.memory.loading = false;
                match entries {
                    Ok(mut entries) => {
                        entries.sort_by_key(|entry| std::cmp::Reverse(entry.updated_ms));
                        entries.truncate(MEMORY_ENTRIES_SHOWN);
                        this.memory.entries = entries;
                        this.memory.loaded = !std::mem::take(&mut this.memory.again);
                    }
                    Err(error) => {
                        tracing::warn!("cannot read Mawas's memory: {error}");
                        this.memory.loaded = true;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
            TodayKind::Approval { .. } => "Approval",
            TodayKind::Blocked => blocked_label(&row.detail),
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
                    .child(SharedString::from(own_words(row))),
            );
        // A request of Mawas's own says in its label who asks; the session line would say
        // "Mawas" a third time.
        let by_mawas = matches!(row.kind, TodayKind::Approval { mawas: true, .. });
        if !row.detail.is_empty() && !by_mawas {
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
                target,
                suggestion,
                ..
            } => {
                // What Mawas asks to do to another agent: who it is, and every word it would
                // say, so that nothing is approved unseen.
                if let Some(target) = target {
                    card = card.child(
                        div()
                            .relative()
                            .flex()
                            .gap(px(tokens.spacing))
                            .text_size(px(tokens.secondary_font_size))
                            .child(div().flex_none().text_color(theme.muted).child("To"))
                            .child(
                                div()
                                    .min_w_0()
                                    .text_color(theme.fg)
                                    .child(SharedString::from(target.title.clone())),
                            )
                            .child(probe(
                                &self.env.regions,
                                Region::AssistantItem("today-target", row_index),
                            )),
                    );
                    if let Some(text) = &target.text {
                        card = card.child(
                            div()
                                .relative()
                                .p(px(tokens.spacing * 1.5))
                                .rounded(px(tokens.radius - 2.0))
                                .bg(theme.bg)
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.fg)
                                .child(SharedString::from(text.clone()))
                                .child(probe(
                                    &self.env.regions,
                                    Region::AssistantItem("today-text", row_index),
                                )),
                        );
                    }
                }
                // What Mawas suggests, apart from the options and visibly hers; *Use* is the
                // user's own click on that option.
                if let Some(suggestion) = suggestion {
                    let (id, use_id) = (*approval, suggestion.option_id.clone());
                    let (use_button, dismiss) =
                        chat::suggestion_buttons(&format!("today-{row_index}"), look);
                    let buttons = vec![
                        use_button
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.answer_from_today(id, use_id.clone(), cx);
                            }))
                            .child(probe(
                                &self.env.regions,
                                Region::Suggestion("today-use", row_index),
                            ))
                            .into_any_element(),
                        dismiss
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.env.sessions.update(cx, |store, cx| {
                                    store.dismiss_suggestion(AskTarget::Approval { id }, cx);
                                });
                            }))
                            .child(probe(
                                &self.env.regions,
                                Region::Suggestion("today-dismiss", row_index),
                            ))
                            .into_any_element(),
                    ];
                    card = card.child(
                        chat::suggestion_box(&suggestion.label, &suggestion.reason, buttons, look)
                            .child(probe(
                                &self.env.regions,
                                Region::Suggestion("today-box", row_index),
                            )),
                    );
                }
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

/// One muted line of a tab.
fn muted_line(text: &str, look: &Look) -> AnyElement {
    div()
        .text_color(look.theme.muted)
        .text_size(px(look.tokens.secondary_font_size))
        .child(SharedString::from(text.to_owned()))
        .into_any_element()
}

/// What an entry is, as the Memory page words it.
fn category_word(category: hutan_protocol::memory::MemoryCategory) -> &'static str {
    use hutan_protocol::memory::MemoryCategory;
    match category {
        MemoryCategory::Profile => "Profile",
        MemoryCategory::Fact => "Fact",
        MemoryCategory::Note => "Note",
    }
}

/// The counts of the Recent tab, one line each, only what is not zero.
fn recent_summary_lines(s: &ActivitySummary) -> Vec<String> {
    let mut lines = Vec::new();
    let mut turns = Vec::new();
    if s.turns_finished > 0 {
        turns.push(format!("{} finished", s.turns_finished));
    }
    if s.turns_failed > 0 {
        turns.push(format!("{} failed", s.turns_failed));
    }
    if s.turns_cancelled > 0 {
        turns.push(format!("{} cancelled", s.turns_cancelled));
    }
    if s.turns_running > 0 {
        turns.push(format!("{} running", s.turns_running));
    }
    if !turns.is_empty() {
        lines.push(format!("Turns: {}", turns.join(" · ")));
    }
    if s.requests_asked > 0 || s.requests_resolved > 0 {
        let mut text = format!(
            "Requests: {} asked · {} answered",
            s.requests_asked, s.requests_resolved
        );
        if s.requests_open > 0 {
            text.push_str(&format!(" · {} still waiting", s.requests_open));
        }
        if let Some(longest) = s.wait_longest_ms {
            text.push_str(&format!(" · longest wait {}", duration_text(longest)));
        }
        lines.push(text);
    }
    if let (Some(files), Some(added), Some(removed)) =
        (s.files_changed, s.lines_added, s.lines_removed)
    {
        lines.push(format!(
            "Changes: {} · +{added} −{removed} {}",
            count_of(files, "file", "files"),
            if added == 1 && removed == 0 || removed == 1 && added == 0 {
                "line"
            } else {
                "lines"
            }
        ));
    }
    let mut tasks = Vec::new();
    if s.tasks_done > 0 {
        tasks.push(format!("{} done", s.tasks_done));
    }
    if s.tasks_failed > 0 {
        tasks.push(format!("{} failed", s.tasks_failed));
    }
    if s.tasks_review > 0 {
        tasks.push(format!("{} sent to review", s.tasks_review));
    }
    if !tasks.is_empty() {
        lines.push(format!("Tasks: {}", tasks.join(" · ")));
    }
    if s.sessions_started > 0 || s.sessions_ended > 0 {
        lines.push(format!(
            "Sessions: {} started · {} ended",
            s.sessions_started, s.sessions_ended
        ));
    }
    lines
}

/// The hairline on the rail's left edge: the cards' (`Theme::card_edge`) on a light pack, where
/// the rail's `surface` fill is 1.05:1 against the page; none on a dark pack.
pub(crate) fn rail_edge(theme: &crate::theme::Theme) -> Option<gpui::Hsla> {
    (!theme.dark).then(|| theme.card_edge())
}

/// The lines of the Flow section: waits on the user, failures that came again, tokens by project
/// and what is worth a look — every line only from what was recorded, with its coverage.
pub fn flow_lines(flow: &hutan_protocol::stats::AgentsStats) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(waits) = &flow.total.waits {
        let open: usize = waits.by_source.iter().map(|side| side.open).sum();
        let mut text = format!(
            "Waiting for you: {} · {} in all · longest {}",
            waits.count,
            duration_text(waits.total_ms),
            duration_text(waits.longest_ms)
        );
        if open > 0 {
            text.push_str(&format!(" · {open} still going"));
        }
        lines.push(text);
    } else {
        lines.push("Waiting for you: none recorded".to_owned());
    }
    if !flow.coverage.state_log {
        lines.push(
            "The state log is off: how long terminal agents waited is not recorded.".to_owned(),
        );
    } else if flow.coverage.terminal_agents > 0
        && let Some(from) = flow.coverage.state_log_from_ms
        && from > flow.from_ms
    {
        lines.push(format!(
            "Terminal agents' waits are counted from {}.",
            hutan_client::time::local_time_text(from)
        ));
    }
    for cluster in flow.failures.iter().take(2) {
        lines.push(format!(
            "Failed {}× the same way: {}",
            cluster.count, cluster.pattern
        ));
    }
    if let Some(turns) = &flow.total.turns
        && turns.failed > 0
        && flow.failures.is_empty()
    {
        lines.push(format!(
            "{} failed turns, none came again the same way",
            turns.failed
        ));
    }
    let mut by_project: Vec<(&str, u64)> = flow
        .groups
        .iter()
        .filter_map(|group| {
            group
                .tokens
                .as_ref()
                .map(|tokens| (group.label.as_str(), tokens.total))
        })
        .collect();
    by_project.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    if !by_project.is_empty() {
        let top: Vec<String> = by_project
            .iter()
            .take(3)
            .map(|(label, total)| format!("{label} {}", tokens_text(*total)))
            .collect();
        lines.push(format!(
            "Tokens by project (summed per turn as the harness reports them, cache included): {}",
            top.join(" · ")
        ));
    } else {
        lines.push("Tokens: none reported".to_owned());
    }
    if !flow.flags.is_empty() {
        lines.push(format!("{} agents worth a look", flow.flags.len()));
    }
    lines
}

/// `820`, `41.2k`, `3.1M`.
fn tokens_text(tokens: u64) -> String {
    match tokens {
        0..=999 => tokens.to_string(),
        1_000..=999_999 => format!("{:.1}k", tokens as f64 / 1_000.0),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.0),
    }
}

/// `3.9 s`, `3m 20s`, `2h 05m`.
fn duration_text(ms: u64) -> String {
    let seconds = ms / 1_000;
    match seconds {
        0 => format!("{ms} ms"),
        1..=9 => format!("{:.1} s", ms as f64 / 1_000.0),
        10..=59 => format!("{seconds} s"),
        60..=3_599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3_600, (seconds % 3_600) / 60),
    }
}

/// One event of the Recent tab, in words.
fn recent_event_text(detail: &ActivityDetail) -> String {
    match detail {
        ActivityDetail::TurnEnded {
            outcome,
            duration_ms,
            error,
        } => {
            let word = match outcome {
                TurnOutcome::Completed => "turn finished",
                TurnOutcome::Failed => "turn failed",
                TurnOutcome::Cancelled => "turn cancelled",
            };
            let took = duration_ms
                .map(|ms| format!(" after {}", duration_text(ms)))
                .unwrap_or_default();
            let why = error.as_ref().map(|e| format!(": {e}")).unwrap_or_default();
            format!("{word}{took}{why}")
        }
        ActivityDetail::Request {
            request,
            text,
            resolved_ms,
            wait_ms,
            outcome,
            ..
        } => {
            let noun = match request {
                RequestKind::Approval => "approval",
                RequestKind::Question => "question",
            };
            match resolved_ms {
                Some(_) => format!(
                    "{noun} {} after {}: {text}",
                    outcome.as_deref().unwrap_or("resolved"),
                    duration_text(wait_ms.unwrap_or(0))
                ),
                None => format!("{noun} waiting: {text}"),
            }
        }
        ActivityDetail::Task { state, run } => {
            if *run {
                format!("task {state}")
            } else {
                // The delivery states, in words: `awaiting_review` is not a thing to read.
                match state.as_str() {
                    "awaiting_review" => "task work waits for review".to_owned(),
                    "none" => "task work no longer waits for review".to_owned(),
                    "accepted" => "task work accepted".to_owned(),
                    "changes_requested" => "task work: changes requested".to_owned(),
                    "merged" => "task work merged".to_owned(),
                    "discarded" => "task work discarded".to_owned(),
                    other => format!("task work {}", other.replace('_', " ")),
                }
            }
        }
        ActivityDetail::SessionStarted { chat } => {
            if *chat {
                "chat started".to_owned()
            } else {
                "terminal started".to_owned()
            }
        }
        ActivityDetail::SessionEnded { state } => format!("ended: {state}"),
        ActivityDetail::Checkpoint {
            files,
            added,
            removed,
        } => format!(
            "{} changed · +{added} −{removed}",
            count_of(*files, "file", "files")
        ),
    }
}

/// `1 file`, `3 files`.
fn count_of(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The label of a terminal agent that waits for the user without the reducer saying for what:
/// what its own words are (`Permission: Bash`, `Run the migration?`), else the honest
/// *Waiting for you*.
fn blocked_label(detail: &str) -> &'static str {
    let lower = detail.trim().to_lowercase();
    if lower.starts_with("permission") || lower.contains("approval") {
        "Approval"
    } else if lower.starts_with("question") || lower.ends_with('?') {
        "Question"
    } else {
        "Waiting for you"
    }
}

/// What a row says of itself: for Mawas's own requests the sentence without the "Mawas asks
/// Hutan to" the label already says, capitalised (`Suggest an answer to …`).
fn own_words(row: &TodayRow) -> String {
    if matches!(row.kind, TodayKind::Approval { mawas: true, .. })
        && let Some(rest) = row.title.strip_prefix("Mawas asks Hutan to ")
    {
        let mut chars = rest.chars();
        return chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
    }
    row.title.clone()
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use hutan_protocol::stats::{
        AgentsStats, FailureCluster, FailureSource, GroupCoverage, StatsBy, StatsCoverage,
        StatsGroup, TokenStats,
    };

    fn group(label: &str, tokens: Option<u64>) -> StatsGroup {
        StatsGroup {
            key: label.into(),
            label: label.into(),
            coverage: GroupCoverage {
                chats: 0,
                terminal_agents: 0,
            },
            turns: None,
            tasks: None,
            waits: None,
            retries: None,
            tokens: tokens.map(|total| TokenStats {
                turns_reported: 1,
                input: total,
                cached_input: 0,
                output: 0,
                total,
                cost_usd_estimate: None,
            }),
        }
    }

    fn stats(state_log: bool) -> AgentsStats {
        AgentsStats {
            from_ms: 1_000,
            to_ms: 2_000,
            group_by: StatsBy::Project,
            coverage: StatsCoverage {
                state_log,
                state_log_from_ms: None,
                tokens_from_ms: None,
                chats: 0,
                terminal_agents: 0,
            },
            total: group("all", None),
            groups: Vec::new(),
            omitted_groups: 0,
            failures: Vec::new(),
            flags: Vec::new(),
            not_recorded: Vec::new(),
        }
    }

    #[test]
    fn nothing_recorded_is_said_so_and_a_log_that_is_off_is_named() {
        let lines = flow_lines(&stats(false));
        assert_eq!(lines[0], "Waiting for you: none recorded");
        assert!(lines[1].starts_with("The state log is off"), "{lines:?}");
        assert_eq!(lines.last().unwrap(), "Tokens: none reported");
        let lines = flow_lines(&stats(true));
        assert!(lines.iter().all(|l| !l.contains("state log")), "{lines:?}");
    }

    #[test]
    fn tokens_are_listed_by_project_largest_first_and_failures_that_came_again_by_count() {
        let mut s = stats(true);
        s.groups = vec![
            group("shop", Some(3_000)),
            group("hutan", Some(41_200)),
            group("quiet", None),
        ];
        s.failures = vec![FailureCluster {
            pattern: "rate limited: retry in # s".into(),
            count: 3,
            sessions: 2,
            last_ms: 1_500,
            source: FailureSource::TurnError,
        }];
        let lines = flow_lines(&s);
        assert!(
            lines.contains(&"Tokens by project (summed per turn as the harness reports them, cache included): hutan 41.2k · shop 3.0k".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"Failed 3× the same way: rate limited: retry in # s".to_owned()),
            "{lines:?}"
        );
    }
}

#[cfg(test)]
mod wording_tests {
    use super::*;

    #[test]
    fn recent_says_one_file_and_names_task_states_in_words() {
        let summary = ActivitySummary {
            files_changed: Some(1),
            lines_added: Some(4),
            lines_removed: Some(0),
            ..ActivitySummary::default()
        };
        assert!(
            recent_summary_lines(&summary).contains(&"Changes: 1 file · +4 −0 lines".to_owned()),
            "{:?}",
            recent_summary_lines(&summary)
        );
        let task = |state: &str, run| ActivityDetail::Task {
            state: state.to_owned(),
            run,
        };
        assert_eq!(
            recent_event_text(&task("awaiting_review", false)),
            "task work waits for review"
        );
        assert_eq!(
            recent_event_text(&task("none", false)),
            "task work no longer waits for review"
        );
        assert_eq!(
            recent_event_text(&task("changes_requested", false)),
            "task work: changes requested"
        );
        assert_eq!(recent_event_text(&task("done", true)), "task done");
        assert_eq!(
            recent_event_text(&ActivityDetail::Checkpoint {
                files: 1,
                added: 2,
                removed: 0
            }),
            "1 file changed · +2 −0"
        );
    }

    #[test]
    fn a_waiting_terminal_agent_is_labelled_by_what_it_says() {
        assert_eq!(blocked_label("Permission: Bash"), "Approval");
        assert_eq!(blocked_label("Run the migration?"), "Question");
        assert_eq!(blocked_label("Question: which branch"), "Question");
        assert_eq!(blocked_label("Waiting for you"), "Waiting for you");
    }
}

#[cfg(test)]
mod rail_edge_tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn the_rail_has_the_cards_hairline_on_a_light_pack_and_none_on_a_dark_one() {
        let light = Theme::light();
        assert_eq!(rail_edge(&light), Some(light.card_edge()));
        assert_eq!(rail_edge(&Theme::dark()), None);
    }
}
