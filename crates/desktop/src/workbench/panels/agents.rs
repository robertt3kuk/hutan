//! The `agents` panel (M13-B): the monitor of every session's observation tree. One section per
//! session that has agents (workspace › session title), the tree indented below it: state glyph,
//! name, model, live duration, usage and the current activity of every node.
//!
//! The daemon owns the tree (`agent.tree` / `agent.tree_updated`, `StoreState.trees`); the panel
//! only filters (`agents.*` settings, chips at the top) and draws it. Durations tick once a
//! second while the panel is drawn and something runs, and not at all otherwise.

use std::collections::HashSet;
use std::time::Duration;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Task, Window, div,
    prelude::*, px,
};
use hutan_core::{NodeState, SessionId};

use super::common::{now_ms, panel_header, tone_color};
use super::history::pill;
use crate::sessions::Tone;
use crate::sessions::agents::{
    self, Filters, NodeRow, activity_text, any_running, duration_ms, format_duration, glyph,
    usage_label, visible_rows,
};
use crate::settings::{AgentsScope, AgentsSettings};
use crate::theme::{DensityExt as _, reduce_motion};
use crate::workbench::Region;
use crate::workbench::actions::{AgentNodeAt, OpenSession, ShowAgentNodeMenu};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::UiStyle;
use crate::workbench::ui::{self, Mascot};

/// How often the durations (and the working dots) refresh while the panel is drawn.
pub const TICK: Duration = Duration::from_secs(1);

/// One listed node, as the panel draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct RowView {
    pub node_id: String,
    pub depth: usize,
    /// Name, else definition, else the agent program or id.
    pub label: String,
    /// The subagent's definition when it is not the name already.
    pub agent_type: Option<String>,
    pub glyph: &'static str,
    /// What the glyph says: `working`, `blocked`, `completed`, `disconnected`, …
    pub meaning: &'static str,
    pub tone: Tone,
    pub animated: bool,
    pub finished: bool,
    pub blocked: bool,
    pub foldable: bool,
    pub collapsed: bool,
    /// Nodes the fold hides.
    pub hidden: usize,
    pub model: Option<String>,
    /// `12s`, `3m 05s`; none for a node without a start time.
    pub duration: Option<String>,
    /// `1.5k tok · context`; none when the source sent no counter.
    pub usage: Option<String>,
    /// The current activity on one short line.
    pub activity: Option<String>,
    pub has_transcript: bool,
}

/// One session with agents: its place and its listed nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionView {
    pub session: SessionId,
    pub workspace: String,
    pub title: String,
    pub rows: Vec<RowView>,
}

impl SectionView {
    /// `workspace › session title`.
    pub fn heading(&self) -> String {
        format!("{} › {}", self.workspace, self.title)
    }
}

fn row_view(row: &NodeRow<'_>, now_ms: u64) -> RowView {
    let node = row.node;
    let g = glyph(node);
    let label = agents::display_name(node).to_owned();
    RowView {
        node_id: node.node_id.clone(),
        depth: row.depth,
        agent_type: node
            .agent_type
            .clone()
            .filter(|t| node.name.is_some() && *t != label),
        label,
        glyph: g.text,
        meaning: g.meaning,
        tone: g.tone,
        animated: g.animated,
        finished: node.is_finished(),
        blocked: node.state == NodeState::Blocked && !node.is_finished(),
        foldable: row.foldable,
        collapsed: row.collapsed,
        hidden: row.hidden,
        model: node.model.clone(),
        duration: duration_ms(node, now_ms).map(format_duration),
        usage: node.usage.as_ref().and_then(usage_label),
        activity: node
            .activity
            .as_deref()
            .map(activity_text)
            .filter(|a| !a.is_empty() && !node.is_finished()),
        has_transcript: node.transcript_path.is_some(),
    }
}

pub struct AgentsPanel {
    env: PanelEnv,
    /// Finished subtrees the user unfolded.
    expanded: HashSet<(SessionId, String)>,
    /// Runs while the panel is drawn and a listed node works.
    ticker: Option<Task<()>>,
    /// Drawn since the ticker last looked: the ticker stops once a tick finds it was not.
    drawn: bool,
    ticks: usize,
}

impl std::fmt::Debug for AgentsPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentsPanel")
            .field("ticking", &self.ticker.is_some())
            .field("ticks", &self.ticks)
            .finish_non_exhaustive()
    }
}

impl AgentsPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |_, _, cx| cx.notify()).detach();
        Self {
            env,
            expanded: HashSet::new(),
            ticker: None,
            drawn: false,
            ticks: 0,
        }
    }

    /// Whether the durations are ticking right now (tests).
    pub fn ticking(&self) -> bool {
        self.ticker.is_some()
    }

    /// How many once-a-second refreshes ran (tests).
    pub fn ticks(&self) -> usize {
        self.ticks
    }

    /// Sections in display order, as the settings filter them: workspaces in the sidebar's
    /// manual order, sessions in creation order. Sections without a listed node are left out.
    pub fn sections(&self, now_ms: u64, cx: &gpui::App) -> Vec<SectionView> {
        let settings = self.env.settings.get::<AgentsSettings>();
        let filters = Filters {
            show_finished: settings.show_finished,
            only_blocked: settings.only_blocked,
        };
        let store = self.env.sessions.read(cx);
        let active = store.active_workspace_id();
        let mut out = Vec::new();
        for workspace in store.workspaces() {
            if settings.scope == AgentsScope::ActiveWorkspace
                && active.is_some_and(|a| a != workspace.id)
            {
                continue;
            }
            for session in store
                .sessions()
                .iter()
                .filter(|s| s.workspace_id == workspace.id)
            {
                let Some(tree) = store.agent_tree(session.id) else {
                    continue;
                };
                let expanded: HashSet<String> = self
                    .expanded
                    .iter()
                    .filter(|(id, _)| *id == session.id)
                    .map(|(_, node)| node.clone())
                    .collect();
                let rows: Vec<RowView> = visible_rows(tree, filters, &expanded)
                    .iter()
                    .map(|row| row_view(row, now_ms))
                    .collect();
                if !rows.is_empty() {
                    out.push(SectionView {
                        session: session.id,
                        workspace: workspace.name.clone(),
                        title: session.title.clone(),
                        rows,
                    });
                }
            }
        }
        out
    }

    /// Whether anything the panel lists is working or waiting for the user.
    fn anything_running(&self, cx: &gpui::App) -> bool {
        let settings = self.env.settings.get::<AgentsSettings>();
        let filters = Filters {
            show_finished: settings.show_finished,
            only_blocked: settings.only_blocked,
        };
        let store = self.env.sessions.read(cx);
        let active = store.active_workspace_id();
        store.agent_trees().any(|tree| {
            let in_scope = settings.scope == AgentsScope::All
                || active.is_none_or(|a| {
                    store
                        .session(tree.session_id)
                        .is_some_and(|s| s.workspace_id == a)
                });
            in_scope && any_running(&visible_rows(tree, filters, &HashSet::new()))
        })
    }

    /// Starts the once-a-second refresh, or stops it; called from `render`, so it only ever runs
    /// while the panel is drawn.
    fn sync_ticker(&mut self, running: bool, cx: &mut Context<Self>) {
        self.drawn = true;
        if !running {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let keep = this
                    .update(cx, |this, cx| {
                        // Not drawn since the last tick: the panel is hidden. Stop; the next
                        // draw starts the ticker again.
                        if !std::mem::take(&mut this.drawn) {
                            this.ticker = None;
                            return false;
                        }
                        this.ticks += 1;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn write_setting(&self, key: &str, value: toml::Value) {
        if let Err(err) = self.env.settings.write_user(key, value) {
            tracing::warn!("cannot save {key}: {err}");
        }
    }
}

impl Render for AgentsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let settings = self.env.settings.get::<AgentsSettings>();
        let now = now_ms();
        let sections = self.sections(now, cx);
        let running = self.anything_running(cx);
        self.sync_ticker(running, cx);
        let pulse_dim = !reduce_motion() && (now / 1000) % 2 == 1;
        let hover = theme.surface_hover;

        let chip_row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .pb(px(tokens.spacing))
            .child(
                pill(
                    "agents-chip-finished",
                    "Finished",
                    settings.show_finished,
                    &theme,
                    &tokens,
                )
                .relative()
                .on_click(cx.listener({
                    let value = !settings.show_finished;
                    move |this, _, _, cx| {
                        this.write_setting("agents.show_finished", toml::Value::Boolean(value));
                        cx.notify();
                    }
                }))
                .child(probe(&self.env.regions, Region::MonitorChip("finished"))),
            )
            .child(
                pill(
                    "agents-chip-blocked",
                    "Blocked only",
                    settings.only_blocked,
                    &theme,
                    &tokens,
                )
                .relative()
                .on_click(cx.listener({
                    let value = !settings.only_blocked;
                    move |this, _, _, cx| {
                        this.write_setting("agents.only_blocked", toml::Value::Boolean(value));
                        cx.notify();
                    }
                }))
                .child(probe(&self.env.regions, Region::MonitorChip("blocked"))),
            )
            .child(
                pill(
                    "agents-chip-scope",
                    match settings.scope {
                        AgentsScope::ActiveWorkspace => "This workspace",
                        AgentsScope::All => "All workspaces",
                    },
                    settings.scope == AgentsScope::All,
                    &theme,
                    &tokens,
                )
                .relative()
                .on_click(cx.listener({
                    let next = match settings.scope {
                        AgentsScope::ActiveWorkspace => AgentsScope::All,
                        AgentsScope::All => AgentsScope::ActiveWorkspace,
                    };
                    move |this, _, _, cx| {
                        this.write_setting(
                            "agents.scope",
                            toml::Value::String(next.key().to_owned()),
                        );
                        cx.notify();
                    }
                }))
                .child(probe(&self.env.regions, Region::MonitorChip("scope"))),
            );

        let mut list = div()
            .id("agents-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        if sections.is_empty() {
            let hidden = self.env.sessions.read(cx).agent_trees().next().is_some();
            list = list.child(
                ui::empty_state(
                    Mascot::Empty,
                    if hidden {
                        "Nothing matches the filters."
                    } else {
                        "No agents reported yet. Coding agents that report to Hutan appear here."
                    },
                    None,
                    &theme,
                    &tokens,
                )
                .h(px(260.0)),
            );
        }
        let mut index = 0usize;
        for section in &sections {
            let session = section.session;
            list = list.child(
                div()
                    .id(SharedString::from(format!("agents-section-{session}")))
                    .flex()
                    .items_center()
                    .h(px(tokens.sidebar_row))
                    .mt(px(tokens.spacing))
                    .px(px(tokens.spacing * 2.0))
                    .text_size(px(tokens.section_font_size))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.muted)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(OpenSession(session.to_string())), cx);
                    })
                    .child(
                        div()
                            .truncate()
                            .child(SharedString::from(section.heading())),
                    ),
            );
            for row in &section.rows {
                list = list
                    .child(self.render_row(session, row, index, pulse_dim, &theme, &tokens, cx));
                index += 1;
            }
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(panel_header("Agents", "agents", cx))
            .child(chip_row)
            .child(list)
    }
}

impl AgentsPanel {
    #[allow(clippy::too_many_arguments)] // one call site; a struct would only rename them
    fn render_row(
        &self,
        session: SessionId,
        row: &RowView,
        index: usize,
        pulse_dim: bool,
        theme: &crate::theme::Theme,
        tokens: &crate::theme::Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = theme.surface_hover;
        let color = tone_color(theme, row.tone);
        let menu_node = row.node_id.clone();
        let fold_node = row.node_id.clone();
        let indent = row.depth as f32 * tokens.spacing * 3.0;
        let glyph_color = if row.animated && pulse_dim {
            color.opacity(0.4)
        } else {
            color
        };
        // Finished nodes recede; a blocked one keeps full weight.
        let dim = row.finished;
        let mut name_line = div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 1.5))
            .w_full()
            .child(
                div()
                    .flex_none()
                    .w(px(tokens.spacing * 3.0))
                    .child(if row.foldable {
                        div()
                            .id(SharedString::from(format!("agents-fold-{index}")))
                            .relative()
                            .cursor_pointer()
                            .text_color(theme.muted)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let key = (session, fold_node.clone());
                                if !this.expanded.remove(&key) {
                                    this.expanded.insert(key);
                                }
                                cx.notify();
                            }))
                            .child(if row.collapsed { "▸" } else { "▾" })
                            .child(probe(&self.env.regions, Region::MonitorFold(index)))
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(12.0))
                    .text_color(glyph_color)
                    .child(row.glyph),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(if row.depth == 0 {
                        gpui::FontWeight::MEDIUM
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .child(SharedString::from(row.label.clone())),
            );
        if let Some(agent_type) = &row.agent_type {
            name_line = name_line.child(
                div()
                    .flex_none()
                    .max_w(px(90.0))
                    .truncate()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(agent_type.clone())),
            );
        }
        if row.collapsed && row.hidden > 0 {
            name_line = name_line.child(
                div()
                    .flex_none()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(format!("+{}", row.hidden)),
            );
        }
        if let Some(model) = &row.model {
            name_line = name_line.child(
                div()
                    .flex_none()
                    .max_w(px(110.0))
                    .truncate()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(model.clone())),
            );
        }
        if let Some(duration) = &row.duration {
            name_line = name_line.child(
                div()
                    .flex_none()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(duration.clone())),
            );
        }

        let detail: Vec<String> = row
            .usage
            .iter()
            .cloned()
            .chain(row.activity.iter().cloned())
            .collect();

        let mut element = div()
            .id(SharedString::from(format!("agents-row-{index}")))
            .relative()
            .flex()
            .flex_col()
            .mx(px(tokens.spacing))
            .pl(px(tokens.spacing + indent))
            .pr(px(tokens.spacing))
            .py(px(1.0))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .when(dim, |s| s.opacity(0.55))
            .on_click(move |_, window, cx| {
                window.dispatch_action(Box::new(OpenSession(session.to_string())), cx);
            })
            .on_mouse_down(
                MouseButton::Right,
                move |event: &MouseDownEvent, window, cx| {
                    window.dispatch_action(
                        Box::new(ShowAgentNodeMenu(AgentNodeAt {
                            session: session.to_string(),
                            node: menu_node.clone(),
                            position: event.position,
                        })),
                        cx,
                    );
                },
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(tokens.sidebar_row - 4.0))
                    .child(name_line),
            );
        if !detail.is_empty() {
            element = element.child(
                div()
                    .pl(px(tokens.spacing * 3.0 + 12.0 + tokens.spacing * 1.5))
                    .pb(px(tokens.spacing / 2.0))
                    .truncate()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(detail.join(" · "))),
            );
        }
        element
            .child(probe(&self.env.regions, Region::MonitorRow(index)))
            .into_any_element()
    }
}
