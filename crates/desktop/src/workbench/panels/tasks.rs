//! The `tasks` panel (M7-C): the tasks of the workspace as a dependency tree — state glyph and
//! color, title, placement badge (`main` / `wt <branch>`), run kind, elapsed time, outcome — with
//! the actions the daemon allows per state (Start / Done / Cancel / Reset / Delete) on the
//! selected row, a "+ New task" button that opens the Start… sheet, and the `tasks.*` filters.
//!
//! The daemon owns tasks (`task.*`, protocol 0.16): [`SessionsStore`](crate::sessions::SessionsStore)
//! lists them and follows `task.updated` / `task.removed`; this panel draws
//! [`tree_rows`] and sends the requests. A click on a row focuses the task's session
//! (`OpenSession`); "Changes" opens the review of its checkout (`ReviewTaskChanges`).

use std::time::Duration;

use gpui::{
    AnyElement, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Task, Window, div, prelude::*, px,
};
use hutan_client::ConnectionState;
use hutan_core::{DeliveryState, TaskId, TaskState};
use hutan_protocol::task::TaskUpdate;

use super::common::{now_ms, panel_header_with, tone_color};
use super::history::pill;
use crate::sessions::tasks::{TaskAction, TaskFilters, TaskRowView, tree_rows};
use crate::settings::{TasksScope, TasksSettings};
use crate::theme::{DensityExt as _, Theme, Tokens, reduce_motion};
use crate::workbench::actions::{
    LauncherOpen, Notify, OpenLauncher, OpenSession, ReviewTaskChanges,
};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::ui::{self, Mascot, StyledExt as _};
use crate::workbench::{Region, style::UiStyle};

/// How often the elapsed times refresh while the panel is drawn and a task runs.
pub const TICK: Duration = Duration::from_secs(1);

/// A delete the daemon refused because other tasks depend on the task: the panel asks whether to
/// delete them too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CascadeAsk {
    pub task: TaskId,
    pub message: String,
}

pub struct TasksPanel {
    env: PanelEnv,
    selected: Option<TaskId>,
    cascade: Option<CascadeAsk>,
    focus: FocusHandle,
    /// Runs while the panel is drawn and a listed task runs.
    ticker: Option<Task<()>>,
    drawn: bool,
}

impl std::fmt::Debug for TasksPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TasksPanel")
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

/// "+ New task" and the empty state: the Start… sheet on "Queue as task", in the project in
/// focus.
fn open_launcher(window: &mut Window, cx: &mut gpui::App) {
    window.dispatch_action(
        Box::new(OpenLauncher(LauncherOpen {
            project: None,
            queue: true,
        })),
        cx,
    );
}

impl TasksPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |_, _, cx| cx.notify()).detach();
        Self {
            env,
            selected: None,
            cascade: None,
            focus: cx.focus_handle(),
            ticker: None,
            drawn: false,
        }
    }

    // ---- read access (also for tests) -----------------------------------------------------

    fn filters(&self, cx: &gpui::App) -> TaskFilters {
        let settings = self.env.settings.get::<TasksSettings>();
        TaskFilters {
            show_done: settings.show_done,
            workspace: match settings.scope {
                TasksScope::All => None,
                TasksScope::Workspace => self.env.sessions.read(cx).active_workspace_id(),
            },
        }
    }

    /// The listed tasks, as the tree draws them.
    pub fn rows(&self, now_ms: u64, cx: &gpui::App) -> Vec<TaskRowView> {
        let store = self.env.sessions.read(cx);
        let parent = |id| store.workspace(id).and_then(|w| w.parent_id);
        tree_rows(store.tasks(), self.filters(cx), &parent, now_ms)
    }

    /// The row whose actions are shown.
    pub fn selected(&self) -> Option<TaskId> {
        self.selected
    }

    /// A delete waiting for the user's word about the tasks that depend on its task.
    pub fn cascade_ask(&self) -> Option<&CascadeAsk> {
        self.cascade.as_ref()
    }

    /// Whether the durations are ticking right now (tests).
    pub fn ticking(&self) -> bool {
        self.ticker.is_some()
    }

    // ---- row actions ----------------------------------------------------------------------

    /// Selects a task's row and focuses its session, when it has one.
    pub fn click_row(&mut self, task: TaskId, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = if self.selected == Some(task) {
            None
        } else {
            Some(task)
        };
        if let Some(session) = self
            .env
            .sessions
            .read(cx)
            .tasks()
            .get(task)
            .and_then(|t| t.sessions.last().copied())
        {
            window.dispatch_action(Box::new(OpenSession(session.to_string())), cx);
        }
        cx.notify();
    }

    /// Sends the request an action stands for.
    pub fn run_action(
        &mut self,
        task: TaskId,
        action: TaskAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = |state| TaskUpdate {
            id: task,
            title: None,
            prompt: None,
            depends_on: None,
            placement: None,
            run: None,
            auto_start: None,
            after_policy: None,
            state: Some(state),
        };
        match action {
            TaskAction::Start => {
                let starting = self
                    .env
                    .sessions
                    .update(cx, |store, cx| store.start_task(task, cx));
                cx.spawn_in(window, async move |_, cx| {
                    let message = match starting.await {
                        Ok(started) => {
                            cx.update(|window, cx| {
                                window.dispatch_action(
                                    Box::new(OpenSession(started.session.id.to_string())),
                                    cx,
                                );
                            })
                            .ok();
                            started.warnings.first().cloned()
                        }
                        Err(error) => Some(format!("Cannot start the task: {error}")),
                    };
                    notify(cx, message);
                })
                .detach();
            }
            TaskAction::Done | TaskAction::Cancel | TaskAction::Reset => {
                let to = match action {
                    TaskAction::Done => TaskState::Done,
                    TaskAction::Cancel => TaskState::Cancelled,
                    _ => TaskState::Todo,
                };
                let updating = self
                    .env
                    .sessions
                    .update(cx, |store, cx| store.update_task(state(to), cx));
                cx.spawn_in(window, async move |_, cx| {
                    if let Err(error) = updating.await {
                        notify(cx, Some(format!("Cannot change the task: {error}")));
                    }
                })
                .detach();
            }
            TaskAction::Delete => self.delete(task, false, window, cx),
        }
    }

    fn delete(&mut self, task: TaskId, cascade: bool, window: &mut Window, cx: &mut Context<Self>) {
        let deleting = self
            .env
            .sessions
            .update(cx, |store, cx| store.delete_task(task, cascade, cx));
        cx.spawn_in(window, async move |this, cx| {
            match deleting.await {
                Ok(_) => {
                    this.update(cx, |this, cx| {
                        if this.selected == Some(task) {
                            this.selected = None;
                        }
                        this.cascade = None;
                        cx.notify();
                    })
                    .ok();
                }
                Err(error) if !cascade && error.contains("Conflict") => {
                    this.update(cx, |this, cx| {
                        this.cascade = Some(CascadeAsk {
                            task,
                            message: error
                                .split_once(": ")
                                .map_or(error.clone(), |(_, rest)| rest.to_owned()),
                        });
                        cx.notify();
                    })
                    .ok();
                }
                Err(error) => notify(cx, Some(format!("Cannot delete the task: {error}"))),
            };
        })
        .detach();
    }

    /// Answers the cascade question: delete the task and every task depending on it, or leave
    /// everything as it is.
    pub fn answer_cascade(&mut self, delete: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ask) = self.cascade.take() else {
            return;
        };
        if delete {
            self.delete(ask.task, true, window, cx);
        }
        cx.notify();
    }

    fn write_setting(&self, key: &str, value: toml::Value) {
        if let Err(err) = self.env.settings.write_user(key, value) {
            tracing::warn!("cannot save {key}: {err}");
        }
    }

    // ---- ticking --------------------------------------------------------------------------

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
                        if !std::mem::take(&mut this.drawn) {
                            this.ticker = None;
                            return false;
                        }
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
}

/// Shows `message` as the workbench's notice.
fn notify(cx: &mut gpui::AsyncWindowContext, message: Option<String>) {
    if let Some(message) = message {
        cx.update(|window, cx| {
            window.dispatch_action(Box::new(Notify(message)), cx);
        })
        .ok();
    }
}

impl Focusable for TasksPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

// ---- rendering ----------------------------------------------------------------------------

/// A compact text button for rows and forms.
/// One line of a row's detail, cut with "…" at the row's width.
fn clipped(text: impl Into<SharedString>) -> gpui::Div {
    div().min_w_0().max_w_full().truncate().child(text.into())
}

fn small_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: ui::ButtonKind,
    enabled: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Stateful<gpui::Div> {
    ui::button(id.into(), label, kind, enabled, theme, tokens)
        .h(px(tokens.font_size + 8.0))
        .px(px(tokens.spacing * 2.0))
        .text_size(px(tokens.secondary_font_size))
}

impl Render for TasksPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let settings = self.env.settings.get::<TasksSettings>();
        let now = now_ms();
        let rows = self.rows(now, cx);
        let running = rows.iter().any(|r| r.state.is_active());
        self.sync_ticker(running, cx);
        let (connected, supported, nothing_at_all) = {
            let store = self.env.sessions.read(cx);
            (
                store.connection_state() == ConnectionState::Connected,
                store.connection().supports_tasks(),
                store.tasks().is_empty(),
            )
        };

        let new_button = small_button(
            "tasks-new",
            "+ New task",
            ui::ButtonKind::Secondary,
            connected && supported,
            &theme,
            &tokens,
        )
        .relative()
        .when(connected && supported, |s| {
            s.on_click(|_, window, cx| open_launcher(window, cx))
        })
        .child(probe(&self.env.regions, Region::Tasks("new")))
        .into_any_element();

        let chips = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .pb(px(tokens.spacing))
            .child(
                pill(
                    "tasks-chip-done",
                    "Done",
                    settings.show_done,
                    &theme,
                    &tokens,
                )
                .relative()
                .on_click(cx.listener({
                    let value = !settings.show_done;
                    move |this, _, _, cx| {
                        this.write_setting("tasks.show_done", toml::Value::Boolean(value));
                        cx.notify();
                    }
                }))
                .child(probe(&self.env.regions, Region::Tasks("chip-done"))),
            )
            .child(
                pill(
                    "tasks-chip-scope",
                    match settings.scope {
                        TasksScope::Workspace => "This workspace",
                        TasksScope::All => "All workspaces",
                    },
                    settings.scope == TasksScope::All,
                    &theme,
                    &tokens,
                )
                .relative()
                .on_click(cx.listener({
                    let next = match settings.scope {
                        TasksScope::Workspace => TasksScope::All,
                        TasksScope::All => TasksScope::Workspace,
                    };
                    move |this, _, _, cx| {
                        this.write_setting(
                            "tasks.scope",
                            toml::Value::String(next.key().to_owned()),
                        );
                        cx.notify();
                    }
                }))
                .child(probe(&self.env.regions, Region::Tasks("chip-scope"))),
            );

        let mut list = div()
            .id("tasks-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        if rows.is_empty() {
            let (text, action) = if !connected {
                ("Not connected to hutand.", None)
            } else if !supported {
                (
                    "This hutand does not know tasks; restart it (Daemon → Restart) to update.",
                    None,
                )
            } else if nothing_at_all {
                (
                    "No tasks yet. Plan work as tasks with dependencies; Hutan starts what is ready.",
                    Some(
                        small_button(
                            "tasks-empty-new",
                            "+ New task",
                            ui::ButtonKind::Primary,
                            true,
                            &theme,
                            &tokens,
                        )
                        .relative()
                        .on_click(|_, window, cx| open_launcher(window, cx))
                        .child(probe(&self.env.regions, Region::Tasks("empty-new")))
                        .into_any_element(),
                    ),
                )
            } else {
                ("Nothing matches the filters.", None)
            };
            let mascot = if nothing_at_all {
                Mascot::Empty
            } else {
                Mascot::Success
            };
            list = list.child(ui::empty_state(mascot, text, action, &theme, &tokens).h(px(280.0)));
        }
        let pulse_dim = !reduce_motion() && (now / 1000) % 2 == 1;
        for (index, row) in rows.iter().enumerate() {
            list = list.child(self.render_row(index, row, pulse_dim, &theme, &tokens, cx));
        }

        let mut root = div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(panel_header_with("Tasks", "tasks", vec![new_button], cx))
            .child(chips);
        if let Some(ask) = self.cascade.clone() {
            root = root.child(self.render_cascade(&ask, &theme, &tokens, cx));
        }
        root.child(list)
    }
}

impl TasksPanel {
    fn render_cascade(
        &self,
        ask: &CascadeAsk,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(tokens.spacing))
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing))
            .p(px(tokens.spacing * 2.0))
            .card(theme, tokens)
            .child(
                div()
                    .text_color(theme.warning)
                    .child(SharedString::from(ask.message.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(tokens.spacing))
                    .child(
                        small_button(
                            "tasks-cascade-yes",
                            "Delete them too",
                            ui::ButtonKind::Primary,
                            true,
                            theme,
                            tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.answer_cascade(true, window, cx);
                        }))
                        .child(probe(&self.env.regions, Region::Tasks("cascade-yes"))),
                    )
                    .child(
                        small_button(
                            "tasks-cascade-no",
                            "Keep",
                            ui::ButtonKind::Ghost,
                            true,
                            theme,
                            tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.answer_cascade(false, window, cx);
                        }))
                        .child(probe(&self.env.regions, Region::Tasks("cascade-no"))),
                    ),
            )
            .into_any_element()
    }

    fn render_row(
        &self,
        index: usize,
        row: &TaskRowView,
        pulse_dim: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.id;
        let hover = theme.surface_hover;
        let selected = self.selected == Some(id);
        let color = tone_color(theme, row.tone);
        let glyph_color = if row.state == TaskState::Running && pulse_dim {
            color.opacity(0.4)
        } else {
            color
        };
        let indent = row.depth as f32 * tokens.spacing * 3.0;
        // A finished task that waits for a reviewer is not finished business.
        let finished = matches!(row.state, TaskState::Done | TaskState::Cancelled)
            && !matches!(
                row.delivery,
                DeliveryState::AwaitingReview | DeliveryState::ChangesRequested
            );

        let mut detail = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(tokens.spacing))
            .pl(px(12.0 + tokens.spacing * 1.5))
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child(ui::pill(row.placement.clone(), theme, tokens))
            .child(clipped(row.run.clone()));
        if row.auto_start {
            detail = detail.child("auto");
        }
        if let Some(elapsed) = &row.elapsed {
            detail = detail.child(clipped(elapsed.clone()));
        }
        if let Some(outcome) = &row.outcome {
            detail = detail.child(clipped(format!("({outcome})")).text_color(
                if row.state == TaskState::Failed {
                    theme.danger
                } else {
                    theme.muted
                },
            ));
        }
        if !row.also_after.is_empty() {
            detail = detail.child(clipped(format!("also after {}", row.also_after.join(", "))));
        }
        if let Some(note) = &row.change_request {
            detail = detail.child(clipped(format!("“{note}”")).text_color(theme.warning));
        }

        let mut element = div()
            .id(SharedString::from(format!("tasks-row-{index}")))
            .relative()
            .flex()
            .flex_col()
            .mx(px(tokens.spacing))
            .pl(px(tokens.spacing + indent))
            .pr(px(tokens.spacing))
            .py(px(2.0))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .when(selected, |s| s.bg(theme.accent_soft))
            .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
            .when(finished, |s| s.opacity(0.6))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.click_row(id, window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 1.5))
                    .h(px(tokens.sidebar_row - 4.0))
                    .child(
                        div()
                            .flex_none()
                            .w(px(12.0))
                            .text_color(glyph_color)
                            .child(crate::icons::glyph(row.glyph)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .medium()
                            .child(SharedString::from(row.title.clone())),
                    )
                    .when_some(row.delivery_label, |s, label| {
                        s.child(
                            ui::state_label(
                                ui::state_glyph(tone_color(theme, row.delivery_tone), theme),
                                label,
                                tone_color(theme, row.delivery_tone),
                                false,
                                theme,
                                tokens,
                            )
                            .relative()
                            .child(probe(&self.env.regions, Region::TaskDelivery(index))),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(color)
                            .child(SharedString::from(row.state.to_string())),
                    ),
            )
            .child(detail);
        if selected {
            let mut actions = div()
                .flex()
                .flex_wrap()
                .gap(px(tokens.spacing))
                .pl(px(12.0 + tokens.spacing * 1.5))
                .py(px(tokens.spacing));
            for action in &row.actions {
                let action = *action;
                actions = actions.child(
                    small_button(
                        SharedString::from(format!("tasks-action-{index}-{}", action.key())),
                        action.label(),
                        if action == TaskAction::Start {
                            ui::ButtonKind::Primary
                        } else {
                            ui::ButtonKind::Secondary
                        },
                        true,
                        theme,
                        tokens,
                    )
                    .relative()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_action(id, action, window, cx);
                        cx.stop_propagation();
                    }))
                    .child(probe(
                        &self.env.regions,
                        Region::TaskAction(index, action.key()),
                    )),
                );
            }
            if row.reviewable {
                actions = actions.child(
                    small_button(
                        SharedString::from(format!("tasks-action-{index}-changes")),
                        "Changes",
                        ui::ButtonKind::Ghost,
                        true,
                        theme,
                        tokens,
                    )
                    .relative()
                    .on_click(cx.listener(move |_, _, window, cx| {
                        window.dispatch_action(Box::new(ReviewTaskChanges(id.to_string())), cx);
                        cx.stop_propagation();
                    }))
                    .child(probe(
                        &self.env.regions,
                        Region::TaskAction(index, "changes"),
                    )),
                );
            }
            element = element.child(actions);
        }
        element
            .child(probe(&self.env.regions, Region::TaskRow(index)))
            .into_any_element()
    }
}
