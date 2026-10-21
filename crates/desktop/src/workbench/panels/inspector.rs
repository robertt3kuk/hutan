//! The `inspector` panel: the permission requests of every agent chat (answered here or in the
//! chat), and session info for the focused item; the changes tab arrives with M7.

use crate::theme::DensityExt as _;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use hutan_core::SessionKind;
use hutan_protocol::blocks::BlockState;
use hutan_protocol::messages::SessionInfo;
use hutan_terminal_view::blocks::{self as block_ui, Tone};

use super::common::{panel_header, state_dot, tone_color};
use crate::settings::InspectorPanelSettings;
use crate::workbench::actions::{BlockRef, JumpToBlock};
use crate::workbench::chat::{self, Look};
use crate::workbench::harnesses::harness_name;
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::UiStyle;
use crate::workbench::ui::{self, Mascot, StyledExt as _};
use crate::workbench::view::Region;

/// Blocks the info tab lists.
const BLOCK_ROWS: usize = 20;

/// One command of the info tab's block list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRow {
    pub id: u64,
    pub command: String,
    pub state: BlockState,
    pub exit_code: Option<i32>,
    /// `1.2 s`; `None` while it runs or when no time is known.
    pub duration: Option<String>,
    pub cwd: Option<String>,
}

pub struct InspectorPanel {
    env: PanelEnv,
    /// The tab the user picked; falls back to the first configured tab when it is not shown.
    active: Option<String>,
}

impl std::fmt::Debug for InspectorPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InspectorPanel")
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

fn tab_title(id: &str) -> &str {
    match id {
        "approvals" => "Requests",
        "changes" => "Changes",
        "info" => "Info",
        other => other,
    }
}

fn kind_name(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Terminal => "Terminal",
        SessionKind::Harness => "Agent (harness)",
        SessionKind::Direct => "Chat (direct)",
    }
}

impl InspectorPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |_, _, cx| cx.notify()).detach();
        Self { env, active: None }
    }

    /// The last [`BLOCK_ROWS`] commands of the session's terminal, newest first (tests and
    /// rendering share this). Empty while the terminal is not attached or has no blocks.
    pub fn block_rows(&self, info: &SessionInfo, cx: &gpui::App) -> Vec<BlockRow> {
        let Some(terminal) = self.env.sessions.read(cx).attached(info.id) else {
            return Vec::new();
        };
        let shared = terminal.read(cx).shared().clone();
        let model = shared.read(cx).model();
        model
            .blocks()
            .iter()
            .rev()
            .filter(|block| block.state != BlockState::Prompt)
            .take(BLOCK_ROWS)
            .map(|block| BlockRow {
                id: block.id,
                command: block
                    .command
                    .as_deref()
                    .map(|c| c.lines().next().unwrap_or_default().trim().to_owned())
                    .unwrap_or_default(),
                state: block.state,
                exit_code: block.exit_code,
                duration: block_ui::duration_ms(block).map(block_ui::format_duration),
                cwd: block.cwd.as_ref().map(|c| c.display().to_string()),
            })
            .collect()
    }

    /// Label/value rows of the info tab for `info` (tests and rendering share this).
    pub fn info_rows(&self, info: &SessionInfo, cx: &gpui::App) -> Vec<(&'static str, String)> {
        let store = self.env.sessions.read(cx);
        let presentation = store.presentation(info);
        let state = match &info.state {
            hutan_core::SessionState::Exited { code, signal } => {
                let mut text = "exited".to_owned();
                if let Some(code) = code {
                    text.push_str(&format!(" with code {code}"));
                }
                if let Some(signal) = signal {
                    text.push_str(&format!(", signal {signal}"));
                }
                text
            }
            hutan_core::SessionState::Failed { reason } => format!("failed: {reason}"),
            _ => presentation.label.clone(),
        };
        let size = store
            .attached(info.id)
            .and_then(|terminal| {
                let shared = terminal.read(cx).shared().clone();
                let model = shared.read(cx).model();
                model
                    .is_ready()
                    .then(|| format!("{} × {}", model.cols(), model.screen_rows()))
            })
            .unwrap_or_else(|| "not attached".to_owned());
        let workspace = store
            .workspace(info.workspace_id)
            .map(|w| w.name.clone())
            .unwrap_or_default();
        vec![
            ("ID", info.id.to_string()),
            ("Kind", kind_name(info.kind).to_owned()),
            ("Title", info.title.clone()),
            ("Workspace", workspace),
            (
                "Command",
                if info.command.is_empty() {
                    "login shell".to_owned()
                } else {
                    info.command.join(" ")
                },
            ),
            (
                "Directory",
                info.cwd
                    .as_ref()
                    .map(|c| c.display().to_string())
                    .unwrap_or_default(),
            ),
            ("State", state),
            (
                "Terminal",
                info.terminal_id
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "none".to_owned()),
            ),
            ("Size", size),
            (
                "Created",
                format!(
                    "{} ({})",
                    hutan_client::time::local_time_seconds_text(info.created_at_ms),
                    hutan_client::time::relative_to_now(info.created_at_ms)
                ),
            ),
        ]
    }
}

impl InspectorPanel {
    /// The session a request or question is in, as a link that opens it (`in omp · api →`).
    fn session_link(
        &self,
        id: impl Into<SharedString>,
        session: hutan_core::SessionId,
        prefix: &str,
        theme: &crate::theme::Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = self
            .env
            .sessions
            .read(cx)
            .session(session)
            .map_or_else(|| "a session that is gone".to_owned(), |s| s.title.clone());
        let session = session.to_string();
        div()
            .id(id.into())
            .min_w_0()
            .truncate()
            .text_color(theme.accent)
            .cursor_pointer()
            .on_click(move |_, window, cx| {
                window.dispatch_action(
                    Box::new(crate::workbench::actions::OpenSession(session.clone())),
                    cx,
                );
            })
            .child(SharedString::from(format!("{prefix}{title} →")))
            .into_any_element()
    }

    /// One pending question: what is asked and in which session; it is answered in the chat.
    fn question_row(
        &self,
        question: &hutan_protocol::chat::QuestionInfo,
        theme: &crate::theme::Theme,
        tokens: &crate::theme::Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let link = self.session_link(
            format!("inspector-open-question-{}", question.id),
            question.session_id,
            "answer in ",
            theme,
            cx,
        );
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .p(px(tokens.spacing * 3.0))
            .card(theme, tokens)
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(SharedString::from(format!(
                        "Question: {}",
                        question.spec.title
                    ))),
            )
            .child(link)
            .into_any_element()
    }

    /// One pending request: the chat's approval group (the question, the tool, where it acts,
    /// a button per offered option with its key and the Always sentence), on a card under the
    /// link to its session.
    fn approval_row(
        &self,
        approval: &hutan_protocol::chat::ApprovalInfo,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let (ordinal, agent) = {
            let store = self.env.sessions.read(cx);
            (
                store
                    .approvals()
                    .iter()
                    .position(|a| a.id == approval.id)
                    .unwrap_or(0),
                store
                    .session(approval.session_id)
                    .and_then(|info| info.chat.as_ref())
                    .map(|chat| harness_name(&chat.agent)),
            )
        };
        let regions = self.env.regions.clone();
        let buttons: Vec<AnyElement> = approval
            .options
            .iter()
            .enumerate()
            .map(|(number, option)| {
                let (id, option_id) = (approval.id, option.id.clone());
                chat::approval_option(
                    SharedString::from(format!("inspector-approve-{id}-{number}")),
                    number,
                    option,
                    chat::scope_tooltip(number, option, &approval.title, agent.as_deref()),
                    look,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let task = this.env.sessions.update(cx, |store, cx| {
                        store.respond_approval(id, option_id.clone(), cx)
                    });
                    cx.spawn(async move |_, _| {
                        if let Err(error) = task.await {
                            tracing::warn!("could not answer a permission request: {error}");
                        }
                    })
                    .detach();
                }))
                .child(probe(
                    &regions,
                    Region::InspectorOption(ordinal * 8 + number),
                ))
                .into_any_element()
            })
            .collect();
        let link = self.session_link(
            format!("inspector-open-{}", approval.id),
            approval.session_id,
            "in ",
            theme,
            cx,
        );
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .card(theme, tokens)
            .child(link)
            .child(chat::approval_group(
                approval.kind,
                Some(&approval.title),
                approval.detail.as_deref(),
                buttons,
                chat::always_note(&approval.options, &approval.title),
                look,
            ))
            .into_any_element()
    }
}

impl Render for InspectorPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let settings = self.env.settings.get::<InspectorPanelSettings>();
        let active = self
            .active
            .as_ref()
            .filter(|a| settings.tabs.contains(a))
            .or_else(|| settings.tabs.first())
            .cloned()
            .unwrap_or_default();
        let hover = theme.surface_hover;
        let focused = {
            let store = self.env.sessions.read(cx);
            store
                .last_focused()
                .and_then(|id| store.session(id).cloned())
        };

        let tabs: Vec<AnyElement> = settings
            .tabs
            .iter()
            .map(|tab| {
                let selected = *tab == active;
                let id = tab.clone();
                div()
                    .id(SharedString::from(format!("inspector-tab-{tab}")))
                    .px(px(tokens.spacing * 2.0))
                    .h(px(tokens.sidebar_row))
                    .flex()
                    .items_center()
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .text_color(if selected { theme.fg } else { theme.muted })
                    .when(selected, |s| {
                        s.font_weight(gpui::FontWeight::MEDIUM)
                            .bg(theme.accent_soft)
                    })
                    .when(!selected, |s| s.hover(move |s| s.bg(hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active = Some(id.clone());
                        cx.notify();
                    }))
                    .child(SharedString::from(tab_title(tab).to_owned()))
                    .into_any_element()
            })
            .collect();

        let note = |text: &str| -> AnyElement {
            div()
                .p(px(tokens.spacing * 3.0))
                .text_color(theme.muted)
                .child(SharedString::from(text.to_owned()))
                .into_any_element()
        };
        let body: AnyElement = match active.as_str() {
            "approvals" => {
                let approvals = self.env.sessions.read(cx).approvals().to_vec();
                let questions = self.env.sessions.read(cx).questions().to_vec();
                if approvals.is_empty() && questions.is_empty() {
                    ui::empty_state(
                        Mascot::Success,
                        "Nothing is waiting for you.",
                        None,
                        &theme,
                        &tokens,
                    )
                    .h(px(260.0))
                    .into_any_element()
                } else {
                    let look = Look::of(cx);
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing * 2.0))
                        .p(px(tokens.spacing * 3.0))
                        .children(
                            approvals
                                .iter()
                                .map(|approval| self.approval_row(approval, &look, cx)),
                        )
                        .children(
                            questions
                                .iter()
                                .map(|question| self.question_row(question, &theme, &tokens, cx)),
                        )
                        .into_any_element()
                }
            }
            "changes" => note("No changes to show. Worktree changes arrive with M7."),
            "info" => match &focused {
                None => note("Focus a session to see its details."),
                Some(info) => {
                    let presentation = self.env.sessions.read(cx).presentation(info);
                    let rows = self.info_rows(info, cx);
                    let block_rows = self.block_rows(info, cx);
                    let session = info.id.to_string();
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing))
                        .p(px(tokens.spacing * 3.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(tokens.spacing * 1.5))
                                .child(state_dot(&theme, &presentation))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(SharedString::from(info.title.clone())),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(tone_color(&theme, presentation.tone))
                                        .child(SharedString::from(presentation.label.clone())),
                                ),
                        )
                        .children(rows.into_iter().map(|(label, value)| {
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(tokens.secondary_font_size))
                                        .text_color(theme.muted)
                                        .child(SharedString::from(label.to_owned())),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("info-{label}")))
                                        .child(SharedString::from(value)),
                                )
                        }))
                        .when(!block_rows.is_empty(), |s| {
                            s.child(
                                div()
                                    .mt(px(tokens.spacing * 2.0))
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.muted)
                                    .child("Recent commands"),
                            )
                            .children(block_rows.into_iter().map(|row| {
                                let tone = match row.state {
                                    BlockState::Running => Tone::Neutral,
                                    _ => match row.exit_code {
                                        Some(0) => Tone::Success,
                                        Some(_) => Tone::Error,
                                        None => Tone::Neutral,
                                    },
                                };
                                let color = match tone {
                                    Tone::Success => theme.success,
                                    Tone::Error => theme.danger,
                                    Tone::Neutral => theme.muted,
                                };
                                let status = match (row.state, row.exit_code) {
                                    (BlockState::Running, _) => "running".to_owned(),
                                    (_, Some(0)) => "ok".to_owned(),
                                    (_, Some(code)) => code.to_string(),
                                    (_, None) => "–".to_owned(),
                                };
                                let status_icon = match (row.state, row.exit_code) {
                                    (BlockState::Running, _) => {
                                        Some(crate::icons::Icon::StateRunning)
                                    }
                                    (_, Some(0)) => Some(crate::icons::Icon::StateCompleted),
                                    (_, Some(_)) => Some(crate::icons::Icon::StateFailed),
                                    (_, None) => None,
                                };
                                let jump = BlockRef {
                                    session: session.clone(),
                                    block: row.id,
                                };
                                div()
                                    .id(SharedString::from(format!("inspector-block-{}", row.id)))
                                    .flex()
                                    .flex_col()
                                    .px(px(tokens.spacing))
                                    .py(px(tokens.spacing * 0.5))
                                    .rounded(px(tokens.radius))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(hover))
                                    .on_click(cx.listener(move |_, _, window, cx| {
                                        window.dispatch_action(
                                            Box::new(JumpToBlock(jump.clone())),
                                            cx,
                                        );
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(tokens.spacing * 1.5))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .child(SharedString::from(row.command)),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .text_color(color)
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(2.0))
                                                    .children(status_icon.map(crate::icons::glyph))
                                                    .child(SharedString::from(status)),
                                            )
                                            .children(row.duration.map(|d| {
                                                div()
                                                    .flex_none()
                                                    .text_color(theme.muted)
                                                    .child(SharedString::from(d))
                                            })),
                                    )
                                    .children(row.cwd.map(|cwd| {
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(tokens.font_size - 1.0))
                                            .text_color(theme.muted)
                                            .child(SharedString::from(cwd))
                                    }))
                            }))
                        })
                        .into_any_element()
                }
            },
            other => note(&format!("Unknown inspector tab `{other}`.")),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(panel_header("Requests", "panels.inspector", cx))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_wrap()
                    .gap(px(2.0))
                    .px(px(tokens.spacing * 2.0))
                    .pb(px(tokens.spacing))
                    .children(tabs),
            )
            .child(
                div()
                    .id("inspector-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}
