//! The Agent Dashboard drawer (Orca's, `docs/design.md` § Layout): the Agents nav row opens a
//! non-modal drawer over the center, from the left sidebar's right edge, between the title bar
//! and the status bar, at most 1294 px wide. A kanban of the sessions: **Needs you**, **Working**,
//! **Done**, **Idle**. A card opens its session and closes the drawer; Escape or the nav row
//! closes it too. The left sidebar keeps the Projects list while it is open.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use hutan_core::{SessionId, SessionState};
use hutan_protocol::messages::SessionInfo;

use super::panels::{age_label, state_dot, tone_color};
use super::render::{TITLEBAR_HEIGHT, probe};
use super::ui::StyledExt as _;
use super::view::{Region, Workbench};
use crate::icons::{Icon, icon};
use crate::sessions::{Presentation, Tone};
use crate::theme::{DensityExt as _, STATUS_BAR_HEIGHT};

/// The widest the drawer gets (Orca's dashboard drawer).
pub const DRAWER_MAX_WIDTH: f32 = 1294.0;
/// The narrowest a kanban column gets (four fit beside a 280 px sidebar in a 1280 px window);
/// the board scrolls sideways below that.
pub const COLUMN_MIN_WIDTH: f32 = 220.0;
/// Space between two cards of a column.
const CARD_GAP: f32 = 8.0;

/// A column of the dashboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DashboardColumn {
    /// Blocked on an approval or a question, failed unseen, or waiting for review.
    NeedsYou,
    /// An agent or a command is busy.
    Working,
    /// Ended (finished, stopped, failed and seen), or finished and not looked at yet.
    Done,
    /// Alive and waiting for input.
    Idle,
}

impl DashboardColumn {
    /// Left to right.
    pub const ALL: [DashboardColumn; 4] = [
        DashboardColumn::NeedsYou,
        DashboardColumn::Working,
        DashboardColumn::Done,
        DashboardColumn::Idle,
    ];

    pub fn title(self) -> &'static str {
        match self {
            DashboardColumn::NeedsYou => "Needs you",
            DashboardColumn::Working => "Working",
            DashboardColumn::Done => "Done",
            DashboardColumn::Idle => "Idle",
        }
    }
}

/// The column a session belongs to, from how it presents itself.
pub fn column_of(info: &SessionInfo, presentation: &Presentation) -> DashboardColumn {
    if presentation.needs_you {
        // A finished session nobody looked at is done, not blocked.
        return if presentation.tone == Tone::Success {
            DashboardColumn::Done
        } else {
            DashboardColumn::NeedsYou
        };
    }
    match &info.state {
        SessionState::Exited { .. } | SessionState::Stopped | SessionState::Failed { .. } => {
            DashboardColumn::Done
        }
        _ if presentation.tone == Tone::Accent => DashboardColumn::Working,
        _ => DashboardColumn::Idle,
    }
}

/// One card of the dashboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardCard {
    pub session: SessionId,
    pub column: DashboardColumn,
    pub title: String,
    /// Which agent runs in the session (`claude`, `codex`, …), else the kind of session.
    pub agent: String,
    /// The agent's last message, else the state's detail, else the state word.
    pub message: String,
    /// Time since the agent's last report, else since the session started (`3m`).
    pub age: String,
}

impl Workbench {
    /// The sessions of the dashboard by column, in the board's order; every column is listed,
    /// empty or not. Cards are the work of agents: chats, terminals an agent reports from, and
    /// sessions that ended while this window watched and nobody has looked at since (a finished
    /// agent's report goes with its process, the unseen end stays). Idle plain shells and
    /// assistant chats are not agents' work and stay out.
    pub fn agents_dashboard(&self, cx: &gpui::App) -> Vec<(DashboardColumn, Vec<DashboardCard>)> {
        let store = self.sessions.read(cx);
        let mut columns: Vec<(DashboardColumn, Vec<DashboardCard>)> = DashboardColumn::ALL
            .into_iter()
            .map(|column| (column, Vec::new()))
            .collect();
        for info in store.sessions() {
            if info.chat.as_ref().is_some_and(|chat| chat.is_assistant()) {
                continue;
            }
            let agent = match (&info.agent, &info.chat) {
                (Some(report), _) => report.agent.clone(),
                (None, Some(chat)) => chat.agent.clone(),
                (None, None) if store.unread(info.id).is_some() => "terminal".to_owned(),
                (None, None) => continue,
            };
            let presentation = store.presentation(info);
            let column = column_of(info, &presentation);
            let message = info
                .agent
                .as_ref()
                .and_then(|report| report.message.clone())
                .filter(|m| !m.trim().is_empty())
                .or_else(|| (!presentation.detail.is_empty()).then(|| presentation.detail.clone()))
                .unwrap_or_else(|| presentation.label.clone());
            let since = info
                .agent
                .as_ref()
                .map_or(info.created_at_ms, |report| report.updated_at_ms);
            let card = DashboardCard {
                session: info.id,
                column,
                title: info.title.clone(),
                agent,
                message,
                age: age_label(since),
            };
            if let Some((_, cards)) = columns.iter_mut().find(|(c, _)| *c == column) {
                cards.push(card);
            }
        }
        columns
    }

    /// Whether the Agent Dashboard drawer is open.
    pub fn agents_drawer_open(&self) -> bool {
        self.agents_drawer
    }

    /// Opens the drawer when closed, closes it when open (the nav row).
    pub fn toggle_agents_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agents_drawer {
            self.close_agents_drawer(window, cx);
        } else {
            self.agents_drawer = true;
            // Escape needs a key context to start from.
            window.focus(&self.drawer_focus, cx);
            cx.notify();
        }
    }

    /// Closes the drawer and gives the keyboard back to what had it.
    pub fn close_agents_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agents_drawer {
            self.agents_drawer = false;
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    /// A card was chosen: close the drawer and show the session.
    pub fn open_session_from_drawer(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.agents_drawer = false;
        self.open_session_from_overview(session, window, cx);
        cx.notify();
    }

    /// The drawer, over the center; `None` while it is closed.
    pub(super) fn render_agents_drawer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.agents_drawer {
            return None;
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        let left = if self.state.docks.left.visible {
            self.state.docks.left.size
        } else {
            0.0
        };
        let remaining = (f32::from(window.viewport_size().width) - left).max(0.0);
        let width = remaining.min(DRAWER_MAX_WIDTH);
        let board = self.agents_dashboard(cx);
        let total: usize = board.iter().map(|(_, cards)| cards.len()).sum();

        let columns: Vec<AnyElement> = board
            .into_iter()
            .map(|(column, cards)| {
                let count = cards.len();
                let cards: Vec<AnyElement> = cards
                    .into_iter()
                    .map(|card| self.dashboard_card(card, cx))
                    .collect();
                div()
                    .id(SharedString::from(format!(
                        "drawer-column-{}",
                        column.title()
                    )))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(COLUMN_MIN_WIDTH))
                    .min_h_0()
                    .gap(px(CARD_GAP))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(6.0))
                            .h(px(24.0))
                            .px(px(2.0))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.muted)
                                    .child(SharedString::from(column.title().to_uppercase())),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .px(px(6.0))
                                    .rounded(px(crate::theme::PILL_RADIUS))
                                    .bg(theme.sidebar_accent)
                                    .text_size(px(11.0))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(count.to_string())),
                            )
                            .child(probe(&self.regions, Region::DrawerColumn(column))),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "drawer-cards-{}",
                                column.title()
                            )))
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .gap(px(CARD_GAP))
                            .overflow_y_scroll()
                            .children(cards)
                            .when(count == 0, |s| {
                                s.child(
                                    div()
                                        .px(px(2.0))
                                        .text_size(px(12.0))
                                        .text_color(theme.faint)
                                        .child("Nothing here"),
                                )
                            }),
                    )
                    .into_any_element()
            })
            .collect();

        let mut drawer = div().id("agents-drawer");
        drawer.style().box_shadow = Some(theme.shadow());
        Some(
            drawer
                .key_context("AgentsDrawer")
                .track_focus(&self.drawer_focus)
                .absolute()
                .top(px(TITLEBAR_HEIGHT))
                .bottom(px(STATUS_BAR_HEIGHT))
                .left(px(left))
                .w(px(width))
                .flex()
                .flex_col()
                .bg(theme.bg)
                .border_1()
                .border_color(theme.border)
                // Non-modal, but a click on the drawer is not a click on what it covers.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.0))
                        .h(px(40.0))
                        .px(px(16.0))
                        .border_b_1()
                        .border_color(theme.border)
                        .child(icon(Icon::LayoutDashboard, 16.0, theme.muted))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Agent Dashboard"),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(SharedString::from(match total {
                                    1 => "1 session".to_owned(),
                                    n => format!("{n} sessions"),
                                })),
                        ),
                )
                .child(
                    div()
                        .id("drawer-board")
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .gap(px(12.0))
                        .p(px(12.0))
                        .overflow_x_scroll()
                        .children(columns),
                )
                .child(probe(&self.regions, Region::AgentsDrawer))
                .into_any_element(),
        )
    }

    fn dashboard_card(&self, card: DashboardCard, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let session = card.session;
        let presentation = self
            .sessions
            .read(cx)
            .session(session)
            .map(|info| self.sessions.read(cx).presentation(info));
        let tone = presentation.as_ref().map_or(Tone::Muted, |p| p.tone);
        let hover = theme.surface_hover;
        div()
            .id(SharedString::from(format!("drawer-card-{session}")))
            .relative()
            .flex()
            .flex_none()
            .flex_col()
            .gap(px(4.0))
            .p(px(10.0))
            .card(&theme, &tokens)
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_session_from_drawer(session, window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(match &presentation {
                        Some(p) => state_dot(&theme, p),
                        None => div().size(px(8.0)).into_any_element(),
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(SharedString::from(card.title)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(card.agent)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(tone_color(&theme, tone))
                            .child(SharedString::from(card.age)),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .text_size(px(12.0))
                    .text_color(theme.muted)
                    .clamp_lines(2)
                    .child(SharedString::from(card.message)),
            )
            .child(probe(&self.regions, Region::DrawerCard(session)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{Unread, present};
    use hutan_core::{SessionKind, WorkspaceId};
    use hutan_protocol::messages::{AgentReport, AgentState};

    fn info(state: SessionState) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
            kind: SessionKind::Terminal,
            title: "t".into(),
            state,
            command: vec![],
            cwd: None,
            terminal_id: None,
            created_at_ms: 0,
            agent: None,
            restored: false,
            chat: None,
            placement: None,
            folder: None,
            recovery: None,
        }
    }

    fn column(info: &SessionInfo, unread: Option<Unread>) -> DashboardColumn {
        column_of(info, &present(info, unread))
    }

    fn reporting(state: AgentState) -> SessionInfo {
        let mut info = info(SessionState::Running);
        info.agent = Some(AgentReport {
            source: "test".into(),
            agent: "claude".into(),
            state,
            message: None,
            agent_session_id: None,
            updated_at_ms: 0,
            blocked_sub: None,
        });
        info
    }

    #[test]
    fn a_session_lands_in_the_column_of_its_state() {
        assert_eq!(
            column(&reporting(AgentState::Blocked), None),
            DashboardColumn::NeedsYou
        );
        assert_eq!(
            column(&reporting(AgentState::Working), None),
            DashboardColumn::Working
        );
        assert_eq!(
            column(&reporting(AgentState::Idle), None),
            DashboardColumn::Idle
        );
        assert_eq!(
            column(&info(SessionState::Idle), None),
            DashboardColumn::Idle
        );
        let exited = |code| SessionState::Exited {
            code: Some(code),
            signal: None,
        };
        assert_eq!(column(&info(exited(0)), None), DashboardColumn::Done);
        assert_eq!(
            column(&info(exited(0)), Some(Unread::Done)),
            DashboardColumn::Done
        );
        assert_eq!(
            column(&info(exited(2)), Some(Unread::Failed)),
            DashboardColumn::NeedsYou
        );
        assert_eq!(
            column(&info(SessionState::Stopped), None),
            DashboardColumn::Done
        );
    }
}
