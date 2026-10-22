//! Drawing the Start… sheet (`launcher.rs` holds its state and behavior): a visual-system-v2
//! sheet with a label column and one row per field, in Tab order.

use gpui::{
    AnyElement, ClickEvent, Context, ElementId, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::*, px, transparent_black,
};

use super::agents::Models;
use super::launcher::{
    LaunchAgent, LaunchWhere, LauncherField, LauncherStop, Readiness, WorktreeList, readiness_note,
};
use super::panels::field_box;
use super::render::probe;
use super::send_key::send_key_control;
use super::ui::motion::{self, PresentExt as _};
use super::ui::{self, Button, FocusRingExt as _, StyledExt as _};
use super::view::{Region, Workbench};
use super::vim_ui::VimConfig;
use super::workspaces::tilde;
use crate::sessions::launch::LaunchKind;
use crate::theme::{DensityExt as _, Theme, Tokens};

/// Width of the label column.
const LABEL_WIDTH: f32 = 84.0;

fn id(text: impl Into<SharedString>) -> ElementId {
    ElementId::from(text.into())
}

impl Workbench {
    /// The sheet over the window (nothing while the launcher is the centre's inline composer).
    pub(crate) fn render_launcher(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.build_launcher(false, cx)
    }

    /// The start composer inside the centre (`LauncherPreset::inline`): the prompt and the
    /// agent, the other rows folded under Options. Built from the same rows as the sheet.
    pub(crate) fn render_launcher_inline(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.build_launcher(true, cx)
    }

    fn build_launcher(&mut self, inline: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let more_present = self.overlays.track(
            "launcher-more",
            self.launcher
                .as_ref()
                .is_some_and(|l| l.inline == inline && l.more_open),
            motion::Kind::Collapse,
            cx,
        );
        let launcher = self.launcher.as_ref().filter(|l| l.inline == inline)?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let vim = VimConfig::of(&self.settings)
            .enabled
            .then_some(&launcher.vim);
        let key_context =
            || crate::workbench::vim_ui::vim_key_context("Launcher", vim.map(|v| v.mode()));
        let stop = launcher.stop;
        let kind = launcher.kind;
        let queue = launcher.queue;
        let starting = launcher.starting;
        let direct = launcher.is_direct();
        let more_open = launcher.more_open;
        // What only a task has (its title, Start now / Queue as task): not for a plain terminal.
        let task_fields = kind.has_task_fields();

        let label = |text: &'static str| {
            div()
                .flex_none()
                .w(px(LABEL_WIDTH))
                .pt(px(6.0))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(text)
        };
        let row = |text: &'static str, body: AnyElement| {
            div()
                .flex()
                .items_start()
                .gap(px(tokens.spacing * 2.0))
                .child(label(text))
                .child(div().flex_1().min_w_0().child(body))
        };
        let note = |text: String, color: gpui::Hsla| {
            div()
                .text_size(px(tokens.secondary_font_size))
                .text_color(color)
                .child(SharedString::from(text))
        };
        // The ring that shows where the keyboard is, on the rows that are not text fields: a
        // tint and an accent border while the row has the keyboard, a stronger tint and a hint
        // while its choices are open (↑ ↓ change the value only then).
        let open_row = launcher.open_row;
        let regions = &self.regions;
        let ring = |row_stop: LauncherStop, body: AnyElement| {
            let active = stop == row_stop;
            let open = open_row == Some(row_stop);
            div()
                .relative()
                .rounded(px(tokens.radius + 2.0))
                .border_1()
                .border_color(transparent_black())
                .focus_ring(active, &theme, &tokens)
                .p(px(1.0))
                .when(active, |s| s.bg(theme.surface_hover))
                .when(open, |s| s.bg(theme.accent_soft))
                .child(body)
                .when(open, |s| {
                    s.child(
                        div()
                            .pt(px(2.0))
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child("← → ↑ ↓ change · ↩ or Esc done"),
                    )
                })
                .when(active, |s| {
                    s.child(probe(regions, Region::Launcher("focus")))
                })
                .into_any_element()
        };

        // ---- Project ----
        let projects = self.launcher_projects(cx);
        let chosen_project = launcher
            .project
            .and_then(|p| projects.iter().find(|c| c.id == p).cloned());
        let project_focused = stop == LauncherStop::Project;
        let project_detail = {
            let root = chosen_project
                .as_ref()
                .and_then(|p| p.root.as_ref())
                .map(|r| tilde(r));
            let branch = launcher
                .current_branch()
                .map(|b| format!("branch {b}"))
                .or_else(|| match &launcher.worktrees {
                    WorktreeList::Loading => Some("checking the repository…".to_owned()),
                    WorktreeList::Ready(list) if list.is_empty() => {
                        Some("not a git repository".to_owned())
                    }
                    _ => None,
                });
            [root, branch]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ")
        };
        let project_name = chosen_project
            .as_ref()
            .map_or_else(|| "Choose a project".to_owned(), |p| p.name.clone());
        let project_box = if project_focused {
            field_box(
                "launch-field-project",
                &launcher.project_query,
                vim,
                &format!("Find a project (now: {project_name})"),
                true,
                false,
                Some(self.launcher_field_ref(LauncherField::Project, cx)),
                &theme,
                &tokens,
            )
        } else {
            let name = project_name.clone();
            div()
                .id("launch-field-project")
                .flex()
                .items_center()
                .min_h(px(tokens.sidebar_row))
                .px(px(tokens.spacing * 2.0))
                .rounded(px(tokens.radius))
                .bg(theme.bg)
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .child(SharedString::from(name))
        }
        .relative()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                this.launcher_focus_stop(LauncherStop::Project, window, cx);
            }),
        )
        .child(probe(&self.regions, Region::Launcher("project")));
        let mut project_body = div().flex().flex_col().gap(px(2.0)).child(project_box);
        if project_focused {
            let matches = self.launcher_project_matches(cx);
            let sel = launcher.project_sel;
            let mut list = div()
                .id("launch-project-list")
                .flex()
                .flex_col()
                .max_h(px(tokens.sidebar_row * 6.0))
                .overflow_y_scroll()
                .rounded(px(tokens.radius))
                .border_1()
                .border_color(theme.border);
            if matches.is_empty() {
                list = list.child(
                    div()
                        .px(px(tokens.spacing * 2.0))
                        .py(px(tokens.spacing))
                        .text_color(theme.faint)
                        .child("No project matches."),
                );
            }
            for (at, project) in matches.into_iter().enumerate() {
                let pid = project.id;
                let hover = theme.surface_hover;
                list = list.child(
                    div()
                        .id(id(format!("launch-project-{at}")))
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 2.0))
                        .h(px(tokens.sidebar_row))
                        .px(px(tokens.spacing * 2.0))
                        .cursor_pointer()
                        .when(at == sel, |s| s.bg(theme.accent_soft))
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.launcher_set_project(pid, cx);
                            this.launcher_focus_stop(LauncherStop::What, window, cx);
                        }))
                        .child(div().flex_none().child(SharedString::from(project.name)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.muted)
                                .text_size(px(tokens.secondary_font_size))
                                .child(SharedString::from(
                                    project.root.as_deref().map(tilde).unwrap_or_default(),
                                )),
                        )
                        .child(probe(&self.regions, Region::LauncherItem("project", at))),
                );
            }
            project_body = project_body.child(list);
        }
        project_body = project_body.child(note(project_detail.clone(), theme.faint));

        // ---- What ----
        // One silhouette (`ui::segmented`): the choices are segments of one strip, the selected
        // one an `accent_soft` fill.
        let mut what = ui::segmented(&theme, &tokens);
        for choice in LaunchKind::ALL {
            what = what.child(
                ui::segment(
                    id(format!("launch-what-{}", choice.key())),
                    choice.label(),
                    kind == choice,
                    &theme,
                    &tokens,
                )
                .relative()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.launcher_focus_stop(LauncherStop::What, window, cx);
                    this.launcher_set_kind(choice, cx);
                }))
                .child(probe(
                    &self.regions,
                    Region::LauncherChoice("what", choice.key()),
                )),
            );
        }
        // In a row of its own, so the strip is as wide as its segments.
        let what = div().flex().child(what);

        // ---- Agent ----
        let mut agent_rows: Vec<AnyElement> = Vec::new();
        if kind.is_agent() {
            let directory = self.env.harnesses.read(cx);
            let mut agents = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(tokens.spacing * 2.0));
            for harness in self.launcher_harness_choices(cx) {
                let hid = harness.id;
                let selected = launcher.agent == LaunchAgent::Harness(hid.to_owned());
                let enabled = harness.enabled();
                // The choices keep their order and place whatever the harnesses report: what a
                // harness needs (Install…, Log in…) is said below the row, never inside it.
                agents = agents.child(
                    ui::choice(
                        id(format!("launch-agent-{hid}")),
                        harness.name,
                        selected,
                        &theme,
                        &tokens,
                    )
                    .relative()
                    .when(!enabled, |s| s.opacity(0.55))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.launcher_focus_stop(LauncherStop::Agent, window, cx);
                        this.launcher_choose_agent(LaunchAgent::Harness(hid.to_owned()), cx);
                    }))
                    .child(probe(&self.regions, Region::LauncherChoice("agent", hid))),
                );
            }
            if kind == LaunchKind::Chat {
                for (at, provider) in self.providers.providers.iter().enumerate() {
                    let pid = provider.id.clone();
                    let selected = launcher.agent == LaunchAgent::Direct(pid.clone());
                    agents = agents.child(
                        ui::choice(
                            id(format!("launch-provider-{at}")),
                            format!("Direct · {}", provider.name),
                            selected,
                            &theme,
                            &tokens,
                        )
                        .relative()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.launcher_focus_stop(LauncherStop::Agent, window, cx);
                            this.launcher_choose_agent(LaunchAgent::Direct(pid.clone()), cx);
                        }))
                        .child(probe(&self.regions, Region::LauncherItem("provider", at))),
                    );
                }
                if self.providers.providers.is_empty() {
                    agents = agents.child(
                        Button::new(
                            "launch-add-provider",
                            "Direct model · Add a provider…",
                            &theme,
                            &tokens,
                        )
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.launcher = None;
                            this.open_models_settings(window, cx);
                        }))
                        .child(probe(&self.regions, Region::Launcher("add-provider"))),
                    );
                }
            }
            let mut body = div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(agents);
            if let Some(harness) = self.launcher_readiness_target(cx)
                && let Some(why) = readiness_note(harness.name, &harness.readiness)
            {
                let hid = harness.id;
                let action: Option<(&'static str, &'static str)> = match &harness.readiness {
                    Readiness::NeedsInstall(_) => self
                        .harness_install_available()
                        .then_some(("install", "Install…")),
                    Readiness::NeedsLogin => {
                        directory.startable(hid).then_some(("login", "Log in…"))
                    }
                    _ => None,
                };
                let mut line = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .child(note(why, theme.warning))
                    .child(probe(&self.regions, Region::Launcher("agent-status")));
                if let Some((key, text)) = action {
                    line = line.child(
                        Button::new(id(format!("launch-{key}-{hid}")), text, &theme, &tokens)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if key == "install" {
                                    window.dispatch_action(
                                        Box::new(super::actions::InstallHarness(hid.to_owned())),
                                        cx,
                                    );
                                } else {
                                    this.launcher = None;
                                    this.account_flow(hid, None, true, window, cx);
                                }
                            }))
                            .child(probe(&self.regions, Region::LauncherChoice(key, hid))),
                    );
                }
                body = body.child(line);
            }
            agent_rows.push(
                row("Agent", ring(LauncherStop::Agent, body.into_any_element())).into_any_element(),
            );
        }

        // ---- Model ----
        let mut model_rows: Vec<AnyElement> = Vec::new();
        if kind == LaunchKind::Chat {
            let models = self.launcher_models(cx);
            let chosen = self.launcher_chosen_model(cx);
            let body: Option<AnyElement> = match &models {
                Models::Loading => {
                    Some(note("Loading the models…".into(), theme.muted).into_any_element())
                }
                Models::Failed(error) => Some(self.render_models_failure(error, cx)),
                Models::Ready(list) if list.is_empty() => Some(
                    note(
                        if direct {
                            "This provider lists no models.".to_owned()
                        } else {
                            "The harness offers no model to choose: it starts with its own default."
                                .to_owned()
                        },
                        theme.muted,
                    )
                    .into_any_element(),
                ),
                Models::Ready(list) => {
                    let current = chosen
                        .as_ref()
                        .map(|m| {
                            if m.is_default {
                                format!("{} (default)", m.display_name)
                            } else {
                                m.display_name.clone()
                            }
                        })
                        .unwrap_or_else(|| "Default".to_owned());
                    let open = launcher.model_open();
                    let hover = theme.surface_hover;
                    let mut body = div().flex().flex_col().gap(px(tokens.spacing)).child(
                        div()
                            .id("launch-model")
                            .relative()
                            .flex()
                            .items_center()
                            .justify_between()
                            .min_h(px(tokens.sidebar_row))
                            .px(px(tokens.spacing * 2.0))
                            .rounded(px(tokens.radius))
                            .bg(theme.bg)
                            .border_1()
                            .border_color(theme.border)
                            .focus_ring(stop == LauncherStop::Model, &theme, &tokens)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.launcher_focus_stop(LauncherStop::Model, window, cx);
                                if let Some(launcher) = &mut this.launcher {
                                    launcher.open_row = (launcher.open_row
                                        != Some(LauncherStop::Model))
                                    .then_some(LauncherStop::Model);
                                }
                                cx.notify();
                            }))
                            .child(SharedString::from(current))
                            .child(div().text_color(theme.muted).child(crate::icons::glyph(
                                if open {
                                    crate::icons::Icon::ActionCollapse
                                } else {
                                    crate::icons::Icon::ActionExpand
                                },
                            )))
                            .child(probe(&self.regions, Region::Launcher("model"))),
                    );
                    if open {
                        let mut group: Option<Option<String>> = None;
                        let mut items = div()
                            .id("launch-model-list")
                            .flex()
                            .flex_col()
                            .max_h(px(tokens.sidebar_row * 7.0))
                            .overflow_y_scroll()
                            .rounded(px(tokens.radius))
                            .border_1()
                            .border_color(theme.border);
                        for (at, row) in self.launcher_model_rows(list, cx).into_iter().enumerate()
                        {
                            let model = row.model;
                            let model_group = row.group.clone();
                            if model_group.is_some() && group.as_ref() != Some(&model_group) {
                                items = items.child(
                                    ui::section_label(
                                        model_group.clone().unwrap_or_default(),
                                        &theme,
                                        &tokens,
                                    )
                                    .px(px(tokens.spacing * 2.0))
                                    .pt(px(tokens.spacing)),
                                );
                            }
                            group = Some(model_group);
                            let selected = row.current;
                            let mid = model.id.clone();
                            let is_default = model.is_default;
                            items = items.child(
                                div()
                                    .id(id(format!("launch-model-{at}")))
                                    .relative()
                                    .flex()
                                    .items_center()
                                    .gap(px(tokens.spacing * 2.0))
                                    .h(px(tokens.sidebar_row))
                                    .px(px(tokens.spacing * 2.0))
                                    .cursor_pointer()
                                    .when(selected, |s| s.bg(theme.accent_soft))
                                    .hover(move |s| s.bg(hover))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let direct =
                                            this.launcher.as_ref().is_some_and(|l| l.is_direct());
                                        this.launcher_choose_model(
                                            (direct || !is_default).then(|| mid.clone()),
                                            cx,
                                        );
                                    }))
                                    .child(ui::current_mark(selected, &theme))
                                    .child(
                                        div()
                                            .flex_none()
                                            .child(SharedString::from(model.display_name.clone())),
                                    )
                                    .children(row.variant.clone().map(|variant| {
                                        div()
                                            .flex_none()
                                            .text_color(theme.muted)
                                            .child(SharedString::from(variant))
                                    }))
                                    .when(model.is_default, |s| {
                                        s.child(div().text_color(theme.muted).child("default"))
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(theme.muted)
                                            .text_size(px(tokens.secondary_font_size))
                                            .child(SharedString::from(model.description.clone())),
                                    )
                                    .child(probe(&self.regions, Region::LauncherItem("model", at))),
                            );
                        }
                        body = body.child(items);
                    }
                    Some(body.into_any_element())
                }
            };
            if let Some(body) = body {
                model_rows.push(row("Model", ring(LauncherStop::Model, body)).into_any_element());
            }
        }

        // ---- Command / Prompt ----
        let text_field = |which: LauncherField,
                          stop_of: LauncherStop,
                          key: &'static str,
                          placeholder: &str,
                          multiline: bool,
                          cx: &mut Context<Self>| {
            let input = match which {
                LauncherField::Prompt => &launcher.prompt,
                LauncherField::Title => &launcher.title,
                LauncherField::Branch => &launcher.branch,
                LauncherField::Base => &launcher.base,
                LauncherField::Command => &launcher.command,
                LauncherField::System => &launcher.system,
                LauncherField::Project => &launcher.project_query,
            };
            field_box(
                format!("launch-field-{key}"),
                input,
                vim,
                placeholder,
                stop == stop_of,
                multiline,
                Some(self.launcher_field_ref(which, cx)),
                &theme,
                &tokens,
            )
            .relative()
            .when(multiline, |s| s.min_h(px(tokens.sidebar_row * 4.0)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.launcher_focus_stop(stop_of, window, cx);
                }),
            )
            .child(probe(&self.regions, Region::Launcher(key)))
            .into_any_element()
        };
        let mut input_rows: Vec<AnyElement> = Vec::new();
        if kind == LaunchKind::Command {
            input_rows.push(
                row(
                    "Command",
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(text_field(
                            LauncherField::Command,
                            LauncherStop::Command,
                            "command",
                            "cargo test -p my-crate && echo done",
                            false,
                            cx,
                        ))
                        .child(note(
                            "Runs in your shell (login shell, -lc): ; && | quotes and globs work."
                                .to_owned(),
                            theme.faint,
                        ))
                        .into_any_element(),
                )
                .into_any_element(),
            );
        }
        if kind.takes_prompt() {
            let send_key = self
                .settings
                .get::<crate::settings::ChatSettings>()
                .send_key;
            input_rows.push(
                row(
                    "Prompt",
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(text_field(
                            LauncherField::Prompt,
                            LauncherStop::Prompt,
                            "prompt",
                            if kind == LaunchKind::Chat {
                                "What should the agent do? It is the first message."
                            } else {
                                "What the agent starts with (optional)"
                            },
                            true,
                            cx,
                        ))
                        .child(
                            div().flex().child(
                                send_key_control(
                                    "launch-send-key",
                                    send_key.hint("start"),
                                    &theme,
                                    &tokens,
                                )
                                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                                    this.show_send_key_menu(event.position(), window, cx);
                                }))
                                .child(probe(&self.regions, Region::Launcher("send-key"))),
                            ),
                        )
                        .into_any_element(),
                )
                .into_any_element(),
            );
        }

        // ---- Where ----
        let worktrees_note: Option<AnyElement> = match &launcher.worktrees {
            WorktreeList::Failed(error) => Some(
                note(
                    format!("Could not list the worktrees: {error}"),
                    theme.danger,
                )
                .into_any_element(),
            ),
            _ => None,
        };
        let git = !matches!(&launcher.worktrees, WorktreeList::Ready(list) if list.is_empty());
        let mut places = div().flex().flex_wrap().gap(px(tokens.spacing));
        places = places
            .child(
                ui::choice(
                    "launch-where-main",
                    "Main checkout",
                    launcher.place == LaunchWhere::Main,
                    &theme,
                    &tokens,
                )
                .relative()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.launcher_focus_stop(LauncherStop::Where, window, cx);
                    this.launcher_set_place(LaunchWhere::Main, cx);
                }))
                .child(probe(
                    &self.regions,
                    Region::LauncherChoice("where", "main"),
                )),
            )
            .child(
                ui::choice(
                    "launch-where-new",
                    "New worktree",
                    launcher.place == LaunchWhere::NewWorktree,
                    &theme,
                    &tokens,
                )
                .relative()
                .when(!git, |s| s.opacity(0.55))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.launcher_focus_stop(LauncherStop::Where, window, cx);
                    this.launcher_set_place(LaunchWhere::NewWorktree, cx);
                }))
                .child(probe(&self.regions, Region::LauncherChoice("where", "new"))),
            );
        let mut where_body = div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .child(places);
        let existing: Vec<(String, String, bool, usize)> = launcher
            .existing_worktrees()
            .into_iter()
            .map(|w| {
                (
                    w.branch.clone().unwrap_or_default(),
                    tilde(&w.path),
                    w.dirty,
                    w.live_sessions.len(),
                )
            })
            .collect();
        if !existing.is_empty() {
            let hover = theme.surface_hover;
            let mut list = div()
                .id("launch-worktrees")
                .flex()
                .flex_col()
                .max_h(px(tokens.sidebar_row * 4.5))
                .overflow_y_scroll()
                .rounded(px(tokens.radius))
                .border_1()
                .border_color(theme.border);
            for (at, (branch, path, dirty, live)) in existing.into_iter().enumerate() {
                let selected = launcher.place == LaunchWhere::Existing(branch.clone());
                let pick = branch.clone();
                list = list.child(
                    div()
                        .id(id(format!("launch-worktree-{at}")))
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 2.0))
                        .h(px(tokens.sidebar_row))
                        .px(px(tokens.spacing * 2.0))
                        .cursor_pointer()
                        .when(selected, |s| s.bg(theme.accent_soft))
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.launcher_focus_stop(LauncherStop::Where, window, cx);
                            this.launcher_set_place(LaunchWhere::Existing(pick.clone()), cx);
                        }))
                        .child(div().w(px(14.0)).text_color(theme.muted).child(
                            crate::icons::glyph(if selected {
                                crate::icons::Icon::StateUnread
                            } else {
                                crate::icons::Icon::StateIdle
                            }),
                        ))
                        .child(
                            div()
                                .flex_none()
                                .child(SharedString::from(format!("wt {branch}"))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.muted)
                                .text_size(px(tokens.secondary_font_size))
                                .child(SharedString::from(path)),
                        )
                        .when(dirty, |s| {
                            s.child(ui::state_label(
                                crate::icons::Icon::StateWarning,
                                "changes",
                                theme.warning,
                                false,
                                &theme,
                                &tokens,
                            ))
                        })
                        .when(live > 0, |s| {
                            s.child(ui::pill(format!("{live} live"), &theme, &tokens))
                        })
                        .child(probe(&self.regions, Region::LauncherItem("worktree", at))),
                );
            }
            where_body = where_body.child(list);
        }
        if let Some(note) = worktrees_note {
            where_body = where_body.child(note);
        }
        if !git && launcher.place == LaunchWhere::Main {
            where_body = where_body.child(note(
                "Not a git repository: the session works in the project's folder.".to_owned(),
                theme.muted,
            ));
        }
        if let Some(warning) = self.launcher_shared_warning(cx) {
            where_body = where_body.child(
                div()
                    .relative()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.warning)
                    .flex()
                    .items_start()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex_none()
                            .child(crate::icons::glyph(crate::icons::Icon::VcsSharedWarning)),
                    )
                    // The sentence wraps in the row's width instead of running out of it.
                    .child(div().flex_1().min_w_0().child(SharedString::from(warning)))
                    .child(probe(&self.regions, Region::Launcher("shared-warning"))),
            );
        }
        let mut where_rows: Vec<AnyElement> = vec![
            row(
                "Where",
                ring(LauncherStop::Where, where_body.into_any_element()),
            )
            .into_any_element(),
        ];
        if launcher.place == LaunchWhere::NewWorktree {
            where_rows.push(
                row(
                    "Branch",
                    text_field(
                        LauncherField::Branch,
                        LauncherStop::Branch,
                        "branch",
                        "hutan/<title>",
                        false,
                        cx,
                    ),
                )
                .into_any_element(),
            );
            where_rows.push(
                row(
                    "From",
                    text_field(
                        LauncherField::Base,
                        LauncherStop::Base,
                        "base",
                        "HEAD",
                        false,
                        cx,
                    ),
                )
                .into_any_element(),
            );
        }

        // ---- Title ----
        let title_row = row(
            "Title",
            text_field(
                LauncherField::Title,
                LauncherStop::Title,
                "title",
                "What it is called in the Tasks panel (from the prompt)",
                false,
                cx,
            ),
        );

        // ---- More ----
        let more_header = div()
            .id("launch-more")
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .cursor_pointer()
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .on_click(cx.listener(|this, _, window, cx| {
                this.launcher_focus_stop(LauncherStop::More, window, cx);
                this.launcher_toggle_more(cx);
            }))
            .child(crate::icons::glyph(if more_open {
                crate::icons::Icon::ActionExpand
            } else {
                crate::icons::Icon::ActionDisclose
            }))
            .child("More")
            .child(div().text_color(theme.faint).child(if direct {
                "after, system prompt"
            } else {
                "after"
            }))
            .child(probe(&self.regions, Region::Launcher("more")));
        let mut more_rows: Vec<AnyElement> = vec![
            row("", ring(LauncherStop::More, more_header.into_any_element())).into_any_element(),
        ];
        if let Some(more_wanted) = more_present {
            let candidates = self.launcher_dependency_rows(cx);
            let hover = theme.surface_hover;
            let mut after = div()
                .id("launch-after")
                .flex()
                .flex_col()
                .max_h(px(tokens.sidebar_row * 4.5))
                .overflow_y_scroll();
            if candidates.is_empty() {
                after = after.child(
                    div()
                        .text_color(theme.faint)
                        .text_size(px(tokens.secondary_font_size))
                        .child("No tasks to wait for."),
                );
            }
            for (at, candidate) in candidates.into_iter().enumerate() {
                let task = candidate.id;
                let ticked = launcher.after.contains(&task);
                let cursor = stop == LauncherStop::After && launcher.after_cursor == at;
                after = after.child(
                    div()
                        .id(id(format!("launch-after-{at}")))
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 2.0))
                        .h(px(tokens.sidebar_row - 4.0))
                        .px(px(tokens.spacing))
                        .rounded(px(tokens.radius))
                        .cursor_pointer()
                        .when(cursor, |s| s.bg(theme.accent_soft))
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.launcher_focus_stop(LauncherStop::After, window, cx);
                            if let Some(launcher) = &mut this.launcher {
                                launcher.after_cursor = at;
                            }
                            this.launcher_toggle_after(task, cx);
                        }))
                        .child(crate::icons::glyph(if ticked {
                            crate::icons::Icon::StateCompleted
                        } else {
                            crate::icons::Icon::StateTodo
                        }))
                        .child(div().truncate().child(SharedString::from(candidate.title)))
                        .child(probe(&self.regions, Region::LauncherItem("after", at))),
                );
            }
            let after_row = row("After", ring(LauncherStop::After, after.into_any_element()));
            let system_row = direct.then(|| {
                row(
                    "System",
                    text_field(
                        LauncherField::System,
                        LauncherStop::System,
                        "system",
                        "Optional instructions for this direct chat",
                        false,
                        cx,
                    ),
                )
            });
            // The rows under More fold open and shut together (150 ms, none for the keyboard).
            more_rows.push(
                motion::expand(
                    "launcher-more-body",
                    more_wanted,
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing * if inline { 3.0 } else { 2.5 }))
                        .child(after_row)
                        .children(system_row),
                )
                .from_closed()
                .into_any_element(),
            );
        }

        // ---- Start now / Queue ----
        let mode = div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .child(
                div().flex().child(
                ui::segmented(&theme, &tokens)
                    .child(
                        ui::segment("launch-mode-now", "Start now", !queue, &theme, &tokens)
                            .relative()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.launcher_focus_stop(LauncherStop::Mode, window, cx);
                                this.launcher_set_queue(false, cx);
                            }))
                            .child(probe(&self.regions, Region::LauncherChoice("mode", "now"))),
                    )
                    .child(
                        ui::segment("launch-mode-queue", "Queue as task", queue, &theme, &tokens)
                            .relative()
                            .when(direct, |s| s.opacity(0.55))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.launcher_focus_stop(LauncherStop::Mode, window, cx);
                                this.launcher_set_queue(true, cx);
                            }))
                            .child(probe(&self.regions, Region::LauncherChoice("mode", "queue"))),
                    ),
                ),
            )
            .child(note(
                if direct {
                    "A direct model chat has no task: it always starts now.".to_owned()
                } else if queue {
                    "Waits in the Tasks panel and starts by itself when ready, up to [tasks] max_parallel at a time."
                        .to_owned()
                } else {
                    "Starts the task now and opens its session.".to_owned()
                },
                theme.faint,
            ));
        let mode_row = row("", ring(LauncherStop::Mode, mode.into_any_element()));

        // ---- footer ----
        let summary = self.launcher_summary(cx);
        let start_label = self.launcher_start_label();
        let footer_frame = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_end()
            .gap(px(tokens.spacing * 2.0))
            .when(!inline, |s| {
                s.px(px(tokens.spacing * 4.0))
                    .py(px(tokens.spacing * 3.0))
                    .border_t_1()
                    .border_color(theme.border)
            });
        let cancel =
            |id: &'static str, label: &'static str| Button::new(id, label, &theme, &tokens);
        let footer = if let Some(confirm) = launcher.confirm.clone() {
            // The first chat on this agent and model that sends its prompt at once: ask.
            let remember = confirm.remember;
            footer_frame
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.warning)
                        .child(SharedString::from(format!(
                            "This sends your prompt to {} now.",
                            confirm.target
                        )))
                        .child(probe(&self.regions, Region::Launcher("confirm"))),
                )
                .child(
                    div()
                        .id("launch-confirm-remember")
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing))
                        .cursor_pointer()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.launcher_toggle_confirm_remember(cx);
                        }))
                        .child(crate::icons::glyph(if remember {
                            crate::icons::Icon::StateCompleted
                        } else {
                            crate::icons::Icon::StateTodo
                        }))
                        .child("Don't ask again for this model")
                        .child(probe(&self.regions, Region::Launcher("confirm-remember"))),
                )
                .child(
                    cancel("launch-confirm-cancel", "Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.launcher_cancel_confirm(cx)))
                        .child(probe(&self.regions, Region::Launcher("confirm-cancel"))),
                )
                .child(
                    Button::new("launch-confirm-start", "Start", &theme, &tokens)
                        .primary()
                        .shortcut("↩")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.launcher_confirm_start(window, cx);
                        }))
                        .child(probe(&self.regions, Region::Launcher("confirm-start"))),
                )
        } else {
            footer_frame
                .child(match launcher.error.clone() {
                    Some(error) => div()
                        .flex_1()
                        .min_w_0()
                        .text_color(theme.danger)
                        .child(SharedString::from(error)),
                    None => div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(summary))
                        .child(probe(&self.regions, Region::Launcher("summary"))),
                })
                .when(!inline, |s| {
                    s.child(
                        cancel("launch-cancel", "Cancel")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_launcher(window, cx)),
                            )
                            .child(probe(&self.regions, Region::Launcher("cancel"))),
                    )
                    .child(
                        Button::new("launch-start-new", "Start and new", &theme, &tokens)
                            .shortcut("⌥⌘↩")
                            .loading(starting)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.launcher_start(true, window, cx);
                            }))
                            .child(probe(&self.regions, Region::Launcher("start-new"))),
                    )
                })
                .child(
                    Button::new("launch-start", start_label, &theme, &tokens)
                        .primary()
                        .loading(starting)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.launcher_start(false, window, cx);
                        }))
                        .child(probe(&self.regions, Region::Launcher("start"))),
                )
        };

        if inline {
            let options_open = launcher.options_open;
            let options_header = div()
                .id("launch-options")
                .relative()
                .flex()
                .items_center()
                .gap(px(tokens.spacing))
                .cursor_pointer()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.launcher_focus_stop(LauncherStop::Options, window, cx);
                    this.launcher_toggle_options(cx);
                }))
                .child(crate::icons::glyph(if options_open {
                    crate::icons::Icon::ActionExpand
                } else {
                    crate::icons::Icon::ActionDisclose
                }))
                .child("Options")
                .child(
                    div()
                        .text_color(theme.faint)
                        .child("project, where, model, title, queue"),
                )
                .child(probe(&self.regions, Region::Launcher("options")));
            let mut column = div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing * 3.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing))
                        .child(ui::headline(SharedString::from(project_name), &theme))
                        .child(note(
                            if project_detail.is_empty() {
                                "Describe the first task, or pick another agent.".to_owned()
                            } else {
                                project_detail
                            },
                            theme.faint,
                        )),
                )
                .children(self.render_start_send_key_offer(cx))
                .children(input_rows)
                .children(agent_rows)
                .child(row(
                    "",
                    ring(LauncherStop::Options, options_header.into_any_element()),
                ));
            if options_open {
                column = column
                    .child(row("Project", project_body.into_any_element()))
                    .child(row(
                        "What",
                        ring(LauncherStop::What, what.into_any_element()),
                    ))
                    .children(model_rows)
                    .children(where_rows)
                    .children(task_fields.then_some(title_row))
                    .children(more_rows)
                    .children(task_fields.then_some(mode_row));
            }
            return Some(
                div()
                    .id("launcher-inline")
                    .key_context(key_context())
                    .track_focus(&self.launcher_focus)
                    .size_full()
                    .flex()
                    .flex_col()
                    .text_size(px(tokens.font_size))
                    .text_color(theme.fg)
                    // A click inside keeps the keyboard here: the pane behind must not take it.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.launcher_refocus(window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        this.launcher_key(event, window, cx);
                    }))
                    // The form scrolls; the primary button stays at the bottom of the pane, so
                    // it is always visible and does not move when rows come and go.
                    .child(
                        div()
                            .id("launcher-inline-form")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .items_center()
                            .pt(px(tokens.spacing * 12.0))
                            .pb(px(tokens.spacing * 4.0))
                            .px(px(tokens.spacing * 6.0))
                            .child(div().w_full().max_w(px(680.0)).child(column)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .justify_center()
                            .px(px(tokens.spacing * 6.0))
                            .py(px(tokens.spacing * 3.0))
                            .border_t_1()
                            .border_color(theme.border)
                            .child(div().w_full().max_w(px(680.0)).child(footer)),
                    )
                    .child(probe(&self.regions, Region::Launcher("composer")))
                    .into_any_element(),
            );
        }

        let body = div()
            .id("launch-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap(px(tokens.spacing * 2.5))
            .px(px(tokens.spacing * 4.0))
            .py(px(tokens.spacing * 3.0))
            .child(row("Project", project_body.into_any_element()))
            .child(row(
                "What",
                ring(LauncherStop::What, what.into_any_element()),
            ))
            .children(agent_rows)
            .children(model_rows)
            .children(input_rows)
            .children(where_rows)
            .children(task_fields.then_some(title_row))
            .children(more_rows)
            .children(task_fields.then_some(mode_row));

        Some(
            div()
                .id("launcher-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(48.0))
                .bg(theme.scrim())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_launcher(window, cx)),
                )
                .child(
                    div()
                        .id("launcher")
                        .key_context(key_context())
                        .track_focus(&self.launcher_focus)
                        .w(px(680.0))
                        .max_h(px(720.0))
                        .flex()
                        .flex_col()
                        .popover(&theme, &tokens)
                        .p(px(0.0))
                        .overflow_hidden()
                        .text_size(px(tokens.font_size))
                        .text_color(theme.fg)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            this.launcher_key(event, window, cx);
                        }))
                        .child(launcher_header(&theme, &tokens))
                        .child(body)
                        .child(footer)
                        .presented(
                            "launcher-motion",
                            motion::Kind::Sheet,
                            true,
                            motion::Slide::Rise,
                        ),
                )
                .presented(
                    "launcher-scrim",
                    motion::Kind::Sheet,
                    true,
                    motion::Slide::None,
                )
                .into_any_element(),
        )
    }
}

fn launcher_header(theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(tokens.spacing * 2.0))
        .h(px(tokens.sidebar_row + 10.0))
        .px(px(tokens.spacing * 4.0))
        .border_b_1()
        .border_color(theme.border)
        .child(div().medium().child("Start…"))
        .child(
            div()
                .text_color(theme.faint)
                .text_size(px(tokens.secondary_font_size))
                .child("an agent or a task, in a project"),
        )
}
