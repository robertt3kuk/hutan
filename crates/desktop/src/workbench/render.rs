//! Rendering and action wiring for [`Workbench`].

use super::actions::{self, palette};
use super::model::{
    Axis, Direction, DockMode, DockPosition, DropTarget, Edge, FocusTarget, ItemRef, ItemSource,
    LayoutPreset, Pane, PaneId, PaneTree, PanelDest,
};
use super::panels::AgentsRequest;
use super::registry::{DockState, PerDock, StatusContext, StatusData};
use super::terminals::session_of_item;
use super::ui::{self, Mascot, StyledExt as _};
use super::view::{
    ContextMenu, DragPreview, DraggedItem, Region, Resizing, Workbench, preset_label, steps,
};
use super::vim_ui::OverlayField;
use super::workspaces::PickerKind;
use crate::theme::{Density, DensityExt as _};
use gpui::{
    Action, AnyElement, AnyView, Bounds, ClickEvent, Context, ElementId, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Pixels, Render,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window, anchored, canvas,
    deferred, div, point, prelude::*, px, relative,
};
use std::cell::Cell;
use std::rc::Rc;

use super::regions::Regions;

/// Height of the title bar region (and of the inset above the left dock's panel stack, and of
/// the right sidebar's icon strip).
///
/// The built-in packs' `titlebar.height`; a unit test in `theme.rs` keeps the two equal. The
/// layout code that uses it (this file, `sidebars.rs`) still reads the constant, not the pack.
pub const TITLEBAR_HEIGHT: f32 = 36.0;
/// Space reserved for the traffic lights when the left dock does not cover them.
const TRAFFIC_LIGHT_INSET: f32 = 80.0;
const HANDLE_THICKNESS: f32 = 6.0;

/// "⌘⇧O"-style hint for the highest-precedence binding of `action` in the current context.
fn key_hint(window: &Window, action: &dyn Action) -> Option<SharedString> {
    let binding = window.highest_precedence_binding_for_action(action)?;
    let text: Vec<String> = binding.keystrokes().iter().map(|k| k.to_string()).collect();
    Some(text.join(" ").into())
}

/// The palette's key in the active keymap ("⌘⇧P"), for hints in prose.
fn palette_key(window: &Window) -> SharedString {
    key_hint(window, &actions::TogglePalette).unwrap_or_else(|| "the command palette".into())
}

/// An invisible child that records its parent's painted bounds under `region`.
pub(super) fn probe(regions: &Regions, region: Region) -> impl IntoElement {
    let regions = regions.clone();
    // Recorded when painted, not when laid out: a list measures its items off to the side before
    // it places them, and only the painted bounds are where the element is.
    canvas(
        |bounds, _, _| bounds,
        move |_, bounds, window, _| regions.record(window.current_view(), region, bounds),
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// `view` drawn through `AnyView::cached`: laid out, prepainted and painted again only when it
/// notifies (or the window refreshes: focus, activation, theme, a click), otherwise its last
/// frame is replayed. The view fills its parent. The regions its probes record are kept while
/// it is replayed (see [`Regions`]).
pub(super) fn cached_view(regions: &Regions, view: impl Into<AnyView>) -> AnyElement {
    let view = view.into();
    let (open, close) = regions.scope(view.entity_id());
    div()
        .relative()
        .size_full()
        .child(open)
        .child(view.cached(StyleRefinement::default().size_full()))
        .child(close)
        .into_any_element()
}

fn id(text: impl Into<SharedString>) -> ElementId {
    ElementId::from(text.into())
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::frames::begin();
        self.sync_appearance(window, cx);
        let title = format!("{} — Hutan", self.active_title(cx));
        if title != self.last_title {
            window.set_window_title(&title);
            self.last_title = title;
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        self.regions.begin_frame();
        self.rendered_terminals.clear();
        self.rendered_chats.clear();
        self.rendered_files.clear();

        self.sync_docks(window, cx);
        let left = self.render_dock(DockPosition::Left, window, cx);
        let right = self.render_dock(DockPosition::Right, window, cx);
        let bottom = self.render_dock(DockPosition::Bottom, window, cx);
        let center = self.render_center(window, cx);
        let titlebar = self.render_titlebar(window, cx);
        let status = self.render_status_bar(window, cx);
        self.rescue_orphaned_focus(window, cx);
        self.sync_assistant_focus(window, cx);
        self.sync_agents_page(cx);
        let edge_targets = self.render_hidden_dock_targets(cx);
        let collapse_preview = self.render_collapse_preview();
        let handles: Vec<AnyElement> = DockPosition::ALL
            .into_iter()
            .filter_map(|pos| self.render_dock_handle(pos, cx))
            .collect();
        let palette = self.render_palette(window, cx);
        let picker = self.render_picker(window, cx);
        let agent_picker = self.render_agent_picker(window, cx);
        let launcher = self.render_launcher(window, cx);
        let menu = self.render_context_menu(cx);
        let modal = self.render_modal(cx);
        let close_toast = self.render_close_toast(cx);
        let daemon_popover = self.render_daemon_popover(cx);
        let recovery = self.render_recovery_summary(cx);
        let sheet = self.render_settings_sheet(cx);
        let setup = self.render_setup(window, cx);
        self.regions.end_frame();
        if self.regions.take_fresh() {
            window.on_next_frame(|window, _| window.refresh());
        }

        let resizing_docks = matches!(self.resizing, Some(Resizing::Dock { .. }));
        let root = div()
            .id("workbench")
            .key_context(if resizing_docks {
                "Workbench DockResize"
            } else {
                "Workbench"
            })
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .font_family(super::ui::ui_family())
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .on_mouse_move(cx.listener(|this, event, window, cx| {
                this.on_mouse_move(event, window, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_resize(cx)),
            );
        let root = self.bind_actions(root, cx);

        root.child(
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .children(left)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(titlebar)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_h_0()
                                .min_w_0()
                                .child(center)
                                .children(bottom),
                        ),
                )
                // The right side is a column of its own: the panel body (a 36 px header, the
                // panel) and, outside it, the activity rail.
                .children(right),
        )
        .child(status)
        .children(collapse_preview)
        .children(daemon_popover)
        .children(recovery)
        .children(edge_targets)
        .children(handles)
        .children(palette)
        .children(picker)
        .children(agent_picker)
        .children(launcher)
        .children(close_toast)
        .children(menu)
        .children(sheet)
        .children(setup)
        .children(modal)
        // Last: its paint ends the frame the benchmarks time (`frames`).
        .children(crate::frames::enabled().then(crate::frames::probe))
    }
}

impl Workbench {
    // ---- actions -----------------------------------------------------------------------------

    fn bind_actions<E: InteractiveElement>(&self, root: E, cx: &mut Context<Self>) -> E {
        let (dock_step, split_step) = steps();
        root.on_action(cx.listener(|this, _: &actions::SplitRight, window, cx| {
            this.split_active(Edge::Right, window, cx)
        }))
        .on_action(cx.listener(|this, _: &actions::SplitDown, window, cx| {
            this.split_active(Edge::Bottom, window, cx)
        }))
        .on_action(cx.listener(|this, _: &actions::SplitLeft, window, cx| {
            this.split_active(Edge::Left, window, cx)
        }))
        .on_action(cx.listener(|this, _: &actions::SplitUp, window, cx| {
            this.split_active(Edge::Top, window, cx)
        }))
        .on_action(cx.listener(|this, _: &actions::CloseAllDocks, _, cx| {
            this.mutate(cx, |s| s.close_all_docks());
        }))
        .on_action(cx.listener(|this, _: &actions::IncreaseFontSize, _, cx| {
            this.change_font_size(1.0, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::DecreaseFontSize, _, cx| {
            this.change_font_size(-1.0, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::RedrawTerminal, _, cx| {
            if let Some(view) = this.focused_terminal_view() {
                view.update(cx, |view, cx| view.redraw(cx));
            }
        }))
        .on_action(cx.listener(|this, _: &actions::ResetFontSize, _, cx| {
            this.reset_font_size(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ToggleLeftDock, _, cx| {
            this.toggle_dock_cmd(DockPosition::Left, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ToggleRightDock, _, cx| {
            this.toggle_dock_cmd(DockPosition::Right, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ToggleBottomDock, _, cx| {
            this.toggle_dock_cmd(DockPosition::Bottom, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::HideLeftSidebar, _, cx| {
            this.set_dock_mode_cmd(DockPosition::Left, DockMode::Hidden, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::HideRightRail, _, cx| {
            this.set_dock_mode_cmd(DockPosition::Right, DockMode::Hidden, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::CancelDockResize, _, cx| {
            this.cancel_resize(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ToggleOverview, _, cx| this.toggle_overview(cx)))
        .on_action(
            cx.listener(|this, _: &actions::ShowAgentsPage, window, cx| {
                this.open_agents_page(AgentsRequest::default(), window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::OpenAgentsPageWith, window, cx| {
                this.open_agents_page(a.0.clone(), window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::ShowAgentMenu, window, cx| {
            if let Ok(id) = a.0.session.parse() {
                this.show_agent_menu(id, a.0.position, window, cx);
            }
        }))
        .on_action(
            cx.listener(|this, a: &actions::ShowAgentsScopeMenu, window, cx| {
                this.show_agents_scope_menu(a.0, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::InterruptSessionById, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.interrupt_agent(id, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::RevealSessionById, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.reveal_session(id, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, a: &actions::OpenWorktreeOf, _, cx| {
            if let Ok(id) = a.0.parse() {
                this.open_worktree_of(id, cx);
            }
        }))
        .on_action(cx.listener(|this, a: &actions::OpenTranscriptOf, _, cx| {
            if let Ok(id) = a.0.parse() {
                this.open_transcript_of(id, cx);
            }
        }))
        .on_action(
            cx.listener(|this, a: &actions::ShowAgentChanges, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.show_agent_changes(id, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ClosePaneItem, window, cx| {
            this.close_active_checked(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::ToggleZoom, _, cx| {
                this.mutate(cx, |s| s.toggle_zoom())
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ResetLayout, _, cx| this.reset_layout(cx)))
        .on_action(cx.listener(|this, _: &actions::TogglePalette, window, cx| {
            this.toggle_palette(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::OpenSettings, window, cx| {
            this.open_settings(None, window, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::OpenSettingsFor, window, cx| {
                this.open_settings(Some(&a.0), window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::About, _, cx| {
            this.set_notice(concat!("Hutan desktop ", env!("CARGO_PKG_VERSION")), cx);
        }))
        .on_action(
            cx.listener(|_, _: &actions::MinimizeWindow, window, _| window.minimize_window()),
        )
        .on_action(cx.listener(|_, _: &actions::ZoomWindow, window, _| window.zoom_window()))
        .on_action(cx.listener(|this, _: &actions::ToggleVimMode, _, cx| {
            this.toggle_vim_mode(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ToggleDensity, _, cx| {
            let next = this.density().toggled();
            this.set_density(next, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ResetTheme, _, cx| {
            this.reset_theme(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::SetDensityCompact, _, cx| {
            this.set_density(Density::Compact, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::SetDensityComfortable, _, cx| {
                this.set_density(Density::Comfortable, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::OpenActivePanelAsTab, _, cx| {
                this.move_focused_panel(PanelDest::Center, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ApplyPreset, _, cx| this.apply_preset_named(&a.0, cx)),
        )
        .on_action(cx.listener(|this, a: &actions::OpenPanel, _, cx| this.open_panel(&a.0, cx)))
        .on_action(cx.listener(|this, _: &actions::FocusNeighborLeft, _, cx| {
            this.mutate(cx, |s| {
                s.focus_neighbor(Direction::Left);
            });
        }))
        .on_action(cx.listener(|this, _: &actions::FocusNeighborRight, _, cx| {
            this.mutate(cx, |s| {
                s.focus_neighbor(Direction::Right);
            });
        }))
        .on_action(cx.listener(|this, _: &actions::FocusNeighborUp, _, cx| {
            this.mutate(cx, |s| {
                s.focus_neighbor(Direction::Up);
            });
        }))
        .on_action(cx.listener(|this, _: &actions::FocusNeighborDown, _, cx| {
            this.mutate(cx, |s| {
                s.focus_neighbor(Direction::Down);
            });
        }))
        .on_action(cx.listener(move |this, _: &actions::ResizeLeft, _, cx| {
            this.mutate(cx, |s| {
                s.resize_focused(Direction::Left, dock_step, split_step);
            });
        }))
        .on_action(cx.listener(move |this, _: &actions::ResizeRight, _, cx| {
            this.mutate(cx, |s| {
                s.resize_focused(Direction::Right, dock_step, split_step);
            });
        }))
        .on_action(cx.listener(move |this, _: &actions::ResizeUp, _, cx| {
            this.mutate(cx, |s| {
                s.resize_focused(Direction::Up, dock_step, split_step);
            });
        }))
        .on_action(cx.listener(move |this, _: &actions::ResizeDown, _, cx| {
            this.mutate(cx, |s| {
                s.resize_focused(Direction::Down, dock_step, split_step);
            });
        }))
        .on_action(cx.listener(|this, _: &actions::ActivateTab1, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(0);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber1, window, cx| {
                this.apply_preset_number(1, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab2, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(1);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber2, window, cx| {
                this.apply_preset_number(2, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab3, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(2);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber3, window, cx| {
                this.apply_preset_number(3, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab4, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(3);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber4, window, cx| {
                this.apply_preset_number(4, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab5, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(4);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber5, window, cx| {
                this.apply_preset_number(5, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab6, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(5);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber6, window, cx| {
                this.apply_preset_number(6, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab7, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(6);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber7, window, cx| {
                this.apply_preset_number(7, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab8, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(7);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber8, window, cx| {
                this.apply_preset_number(8, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab9, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(8);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ActivatePreviousTab, _, cx| {
                this.mutate(cx, |s| {
                    s.cycle_tab(false);
                });
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateNextTab, _, cx| {
            this.mutate(cx, |s| {
                s.cycle_tab(true);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber9, window, cx| {
                this.apply_preset_number(9, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::NewTerminal, window, cx| {
            this.new_terminal(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::NewTerminalInWorktree, window, cx| {
                this.new_terminal_in_worktree(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::RemoveWorktree, window, cx| {
                this.open_worktree_remove_picker(window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::NewTask, window, cx| {
            this.new_task(window, cx);
        }))
        .on_action(cx.listener(|this, a: &actions::OpenLauncher, window, cx| {
            this.open_launcher_from(a.0.clone(), window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ShowTasksPanel, _, cx| {
            this.show_tasks_panel(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ReviewChanges, window, cx| {
            this.open_review_picker(window, cx);
        }))
        .on_action(cx.listener(|this, a: &actions::Notify, _, cx| {
            this.set_notice(a.0.clone(), cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::ReviewTaskChanges, window, cx| {
                if let Ok(task) = a.0.parse() {
                    this.review_task_changes(task, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ReviewChangesAt, window, cx| {
                this.open_changes(std::path::PathBuf::from(&a.0), window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::NewAgent, window, cx| {
            this.new_agent(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::ReviewApprovals, window, cx| {
                this.open_approvals_picker(window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::NewChat, window, cx| {
            this.new_direct_chat(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::ManageProviders, window, cx| {
                this.open_models_settings(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::SwitchChatModel, window, cx| {
                if let Some(session) = this.focused_session() {
                    this.open_model_switch(session, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, action: &actions::SwitchChatModelById, window, cx| {
                if let Ok(session) = action.0.parse() {
                    this.open_model_switch(session, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, action: &actions::SetProviderKey, window, cx| {
                this.set_provider_key(&action.0, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &hutan_terminal_view::SendBlockToAgent, _, cx| {
                this.set_notice(
                    "Focus a terminal with a finished command to send it to an agent",
                    cx,
                );
            }),
        )
        .on_action(cx.listener(|this, _: &actions::SearchHistory, window, cx| {
            this.search_history(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::ManageAccounts, window, cx| {
                this.open_accounts(window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::OpenAssistant, window, cx| {
            this.open_assistant(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::NewMawasChat, window, cx| {
            this.new_mawas_chat(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::OpenMawasMemory, window, cx| {
                this.open_memory(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::EditMemoryProposal, window, cx| {
                this.edit_memory_proposal(&a.0, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ResumeHistoryHit, window, cx| {
                this.resume_history_hit(a.0.clone(), window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::CopyHistoryCommand, _, cx| {
            this.copy_history_command(&a.0, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::OpenHistoryHit, window, cx| {
                this.open_history_hit(a.0.clone(), window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::RevealHistoryFile, _, cx| {
            this.reveal_history_file(&a.0, cx);
        }))
        .on_action(cx.listener(|this, a: &actions::OpenHistoryFile, _, cx| {
            this.open_history_file(&a.0, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::OpenHistoryDirectory, window, cx| {
                this.open_history_directory(&a.0, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ShowAgentMonitor, _, cx| {
            this.show_agent_monitor(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ShowResources, _, cx| {
            this.show_resources(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ToggleResources, _, cx| {
            this.toggle_resources(cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::ShowInResources, window, cx| {
                if let Ok(session) = a.0.parse() {
                    this.show_in_resources(session, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowResourceMenu, window, cx| {
                this.show_resource_menu(&a.0, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::ShowOnlyBlockedAgents, _, cx| {
                this.show_only_blocked_agents(cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowAgentNodeMenu, window, cx| {
                if let Ok(session) = a.0.session.parse() {
                    this.show_agent_node_menu(session, &a.0.node, a.0.position, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, a: &actions::JumpToBlock, window, cx| {
            this.jump_to_block(&a.0.session, a.0.block, window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::StopSession, window, cx| {
            if let Some(session) = this.focused_session() {
                this.stop_session(session, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &actions::CloseSession, window, cx| {
            if let Some(session) = this.focused_session() {
                this.close_session(session, window, cx);
            }
        }))
        .on_action(
            cx.listener(|this, a: &actions::CloseSessionById, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.close_session(id, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ReopenSessionById, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.reopen_session(id, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, _: &actions::RenameSession, window, cx| {
            if let Some(session) = this.focused_session() {
                this.rename_session(session, window, cx);
            }
        }))
        .on_action(
            cx.listener(|this, _: &actions::RestartSession, window, cx| {
                if let Some(session) = this.focused_session() {
                    this.restart_session(session, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, _: &actions::CopySessionId, _, cx| {
            if let Some(session) = this.focused_session() {
                this.copy_session_id(session, cx);
            }
        }))
        .on_action(
            cx.listener(|this, _: &actions::JumpToNextAttention, window, cx| {
                this.jump_to_next_attention(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::SaveLayoutAsPreset, window, cx| {
                this.save_layout_as_preset(window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::OpenSession, window, cx| {
            if let Ok(id) = a.0.parse() {
                this.open_session(id, window, cx);
            }
        }))
        .on_action(
            cx.listener(|this, a: &actions::OpenSessionInSplit, window, cx| {
                if let Ok(id) = a.0.session.parse() {
                    this.open_session_in_split(id, a.0.edge, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::OpenSessionFromOverview, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.open_session_from_overview(id, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::StopSessionById, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.stop_session(id, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::RenameSessionById, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.rename_session(id, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::RestartSessionById, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.restart_session(id, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, a: &actions::CopySessionIdOf, _, cx| {
            if let Ok(id) = a.0.parse() {
                this.copy_session_id(id, cx);
            }
        }))
        .on_action(
            cx.listener(|this, a: &actions::ShowSessionMenu, window, cx| {
                if let Ok(id) = a.0.session.parse() {
                    let entries = this.session_menu(id, cx);
                    this.open_menu(a.0.position, entries, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowPlacementMenu, window, cx| {
                if let Ok(id) = a.0.session.parse() {
                    let entries = this.placement_menu(id, cx);
                    this.open_menu(a.0.position, entries, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, a: &actions::MoveActivePanel, _, cx| {
            this.move_focused_panel(a.0, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::MoveActiveTabToSplit, _, cx| {
                this.move_active_tab_to_split(a.0, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &palette::Dismiss, window, cx| {
            // With Vim mode on, the first Escape of a text field leaves Insert mode.
            if this.overlay_vim_escape(cx) {
                return;
            }
            this.close_menu(window, cx);
            this.close_palette(window, cx);
            this.close_picker(window, cx);
            this.close_agent_picker(window, cx);
            this.close_daemon_popover(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::Quit, window, cx| {
            this.request_quit(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::daemon::Status, window, cx| {
                this.show_daemon_status(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::daemon::Restart, window, cx| {
                this.restart_daemon(window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::daemon::Stop, window, cx| {
            this.stop_daemon(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::daemon::Start, _, cx| {
            this.start_daemon(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::daemon::OpenLogs, _, cx| {
            this.open_daemon_logs(cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::daemon::ToggleKeepRunning, _, cx| {
                this.toggle_keep_running(cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::daemon::ToggleStartAtLogin, _, cx| {
                this.toggle_start_at_login(cx);
            }),
        )
        .on_action(
            cx.listener(|this, action: &actions::InstallHarness, window, cx| {
                this.install_harness(&action.0, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::RunSetup, window, cx| {
            this.open_setup(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::daemon::OpenLoginItems, _, cx| {
                this.open_login_items(cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::OpenProject, window, cx| {
            if this
                .picker()
                .is_some_and(|p| p.kind == PickerKind::Projects)
            {
                this.close_picker(window, cx);
            } else {
                this.open_project_picker(window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &actions::OpenFolder, window, cx| {
            this.open_folder_dialog(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::OpenFileDialog, window, cx| {
                this.open_file_dialog(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::OpenFileInSplitRight, window, cx| {
                this.split_focused_file(Edge::Right, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::OpenFileInSplitDown, window, cx| {
                this.split_focused_file(Edge::Bottom, window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::OpenFile, window, cx| {
            let target = super::files::FileOpen {
                path: a.0.path.clone().into(),
                line: a.0.line,
                column: a.0.column,
                // `ui.run` and `hutan open` may come from an agent: the roots apply.
                picked: false,
            };
            this.open_file(target, window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::OpenJump, window, cx| {
            if this.picker().is_some_and(|p| p.kind == PickerKind::Jump) {
                this.close_picker(window, cx);
            } else {
                this.open_jump_palette(window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &actions::GoHome, window, cx| {
            let home = this.sessions.read(cx).home_workspace().map(|w| w.id);
            match home {
                Some(home) => this.switch_workspace(home, window, cx),
                None => this.set_notice("No Home workspace yet", cx),
            }
        }))
        .on_action(cx.listener(|this, a: &actions::OpenWorkspace, window, cx| {
            if let Ok(id) = a.0.parse() {
                this.switch_workspace(id, window, cx);
            }
        }))
        .on_action(
            cx.listener(|this, a: &actions::ShowWorkspaceMenu, window, cx| {
                if let Ok(id) = a.0.workspace.parse() {
                    this.show_workspace_menu(id, a.0.position, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, a: &actions::ShowSortMenu, window, cx| {
            this.show_sort_menu(a.0, window, cx);
        }))
        .on_action(cx.listener(|this, a: &actions::SetWorkspaceSort, _, cx| {
            this.set_workspace_sort(a.0, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::MoveWorkspaceOnto, window, cx| {
                if let (Ok(dragged), Ok(target)) = (a.0.dragged.parse(), a.0.target.parse()) {
                    this.drop_workspace(dragged, target, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::PinActiveWorkspace, window, cx| {
                if let Some(info) = this.active_workspace_info(cx) {
                    this.pin_workspace(info.id, !info.pinned, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::RenameActiveWorkspace, window, cx| {
                if let Some(id) = this.active_workspace() {
                    this.rename_workspace(id, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::AddFolderToWorkspace, window, cx| {
                this.add_folder_to_active(window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::NewWorkspace, window, cx| {
            this.new_workspace_dialog(Vec::new(), true, window, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::ShowAssistantScopeMenu, window, cx| {
                this.show_assistant_scope_menu(a.0, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowAssistantModelMenu, window, cx| {
                this.show_assistant_model_menu(a.0, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowAssistantModeMenu, window, cx| {
                this.show_assistant_mode_menu(a.0, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowSendKeyMenu, window, cx| {
                this.show_send_key_menu(a.0, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ToggleSendKey, _, cx| {
            this.toggle_send_key(cx);
        }))
        .on_action(cx.listener(|this, a: &actions::ShowPlusMenu, window, cx| {
            this.show_plus_menu(a.0, window, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::ShowProjectMenu, window, cx| {
                if let Ok(project) = a.0.project.parse() {
                    let selection: Vec<_> =
                        a.0.selection
                            .iter()
                            .filter_map(|s| s.parse().ok())
                            .collect();
                    this.show_project_menu(project, &selection, a.0.position, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::DropProjectOnto, window, cx| {
                if let Ok(dragged) = a.0.dragged.parse() {
                    let target = a.0.target.as_deref().and_then(|t| t.parse().ok());
                    this.drop_project(dragged, target, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, a: &actions::SetSidebarOrganize, _, cx| {
            this.set_sidebar_organize(a.0, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::RenameSessionTo, window, cx| {
                if let Ok(id) = a.0.session.parse() {
                    this.rename_session_to(id, a.0.title.clone(), window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::RenameWorkspaceTo, window, cx| {
                if let Ok(id) = a.0.workspace.parse() {
                    this.rename_workspace_to(id, a.0.name.clone(), window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::CloseActiveWorkspace, window, cx| {
                if let Some(id) = this.active_workspace() {
                    this.close_workspace(id, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, _: &actions::RevealWorkspace, _, cx| {
            if let Some(id) = this.active_workspace() {
                this.reveal_workspace(id, cx);
            }
        }))
        .on_action(
            cx.listener(|this, _: &actions::ToggleHideEmptyWorkspaces, _, cx| {
                this.toggle_workspace_filter("hide_empty", cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::ToggleHideSleepingWorkspaces, _, cx| {
                this.toggle_workspace_filter("hide_sleeping", cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::ToggleHideCliWorkspaces, _, cx| {
                this.toggle_workspace_filter("hide_cli_created", cx);
            }),
        )
    }

    // ---- title bar ---------------------------------------------------------------------------

    fn render_titlebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let left_extent = self.dock_extent(DockPosition::Left);
        let overview_active = self.state.preset == LayoutPreset::OVERVIEW;
        let overview_hint =
            key_hint(window, &actions::ToggleOverview).unwrap_or_else(|| "⌘⇧O".into());
        let preset_name = preset_label(&self.state.preset);
        let hover = theme.surface_hover;

        let preset_button = div()
            .id("titlebar-preset")
            .px(px(tokens.spacing * 2.0))
            .h(px(tokens.control_height - 4.0))
            .flex()
            .items_center()
            .rounded(px(tokens.radius))
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .cursor_pointer()
            .hover(move |s| s.bg(hover).text_color(theme.fg))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                this.show_preset_menu(event.position(), window, cx);
            }))
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(format!("Layout: {preset_name}"))
            .child(crate::icons::glyph(crate::icons::Icon::ActionMenuDown));

        let accent = theme.accent;
        let overview_button = div()
            .id("titlebar-overview")
            .px(px(tokens.spacing * 2.0))
            .h(px(tokens.control_height - 4.0))
            .flex()
            .items_center()
            .rounded(px(tokens.radius))
            .text_size(px(tokens.secondary_font_size))
            .text_color(if overview_active { accent } else { theme.muted })
            .cursor_pointer()
            .hover(move |s| s.bg(hover).text_color(theme.fg))
            .when(overview_active, move |s| s.bg(theme.accent_soft))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_overview(cx)))
            .gap(px(4.0))
            .child(crate::icons::glyph(crate::icons::Icon::NavAgents))
            .child(format!("Agents overview {overview_hint}"));

        let left_toggle = self.dock_toggle_button(DockPosition::Left, window, cx);
        let right_toggle = self.dock_toggle_button(DockPosition::Right, window, cx);
        div()
            .id("titlebar")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .h(px(TITLEBAR_HEIGHT))
            .w_full()
            .pl(px((TRAFFIC_LIGHT_INSET - left_extent).max(12.0)))
            .pr(px(10.0))
            .bg(theme.sidebar)
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
            .child(left_toggle)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .justify_center()
                    .truncate()
                    .text_size(px(tokens.font_size))
                    .text_color(theme.muted)
                    .child(self.active_title(cx)),
            )
            .child(preset_button)
            .child(overview_button)
            .child(right_toggle)
            .child(probe(&self.regions, Region::Titlebar))
            .into_any_element()
    }

    // ---- docks -------------------------------------------------------------------------------

    fn render_dock(
        &mut self,
        pos: DockPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dock = self.state.docks.get(pos).clone();
        match (pos, self.dock_mode(pos)) {
            (_, DockMode::Hidden) => return None,
            (DockPosition::Left, DockMode::Compact) => {
                return Some(self.left_rail(window, cx));
            }
            (DockPosition::Right, DockMode::Compact) => {
                return Some(self.right_rail(&dock, window, cx));
            }
            _ => {}
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        let is_left = pos == DockPosition::Left;
        let hover = theme.surface_hover;
        let accent = theme.accent;
        // Tabs are rounded chips inset 4 px in their strip.
        let tab_inner = tokens.tab_height - 8.0;

        // Only the bottom dock keeps a strip of text tabs; the left dock is a nav list with the
        // active page below it, the right dock an icon strip (`sidebars.rs`).
        let tab_panels: &[crate::workbench::model::PanelId] = if pos == DockPosition::Bottom {
            &dock.panels
        } else {
            &[]
        };
        let tabs: Vec<AnyElement> = tab_panels
            .iter()
            .enumerate()
            .map(|(index, panel)| {
                let active = dock.active.as_ref() == Some(panel);
                let (icon, title) = self.item_icon_and_title(
                    &ItemRef::Panel {
                        panel: panel.clone(),
                    },
                    cx,
                );
                let source = ItemSource::Dock {
                    pos,
                    panel: panel.clone(),
                };
                let dragged = DraggedItem {
                    source,
                    item: ItemRef::Panel {
                        panel: panel.clone(),
                    },
                    label: title.clone(),
                };
                let click_panel = panel.clone();
                let menu_panel = panel.clone();
                div()
                    .id(id(format!("dock-tab-{pos:?}-{panel}")))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .px(px(tokens.spacing * 2.0))
                    .h(px(tab_inner))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .text_color(if active { theme.fg } else { theme.muted })
                    .when(active, |s| {
                        s.bg(theme.surface).border_1().border_color(theme.border)
                    })
                    .when(!active, |s| s.hover(move |s| s.bg(hover)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let panel = click_panel.clone();
                        this.mutate(cx, |s| {
                            let _ = s.set_dock_active(pos, &panel);
                            s.focus_dock(pos);
                        });
                        this.focus_root(window, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let entries = this.panel_menu(&menu_panel);
                            this.open_menu(event.position, entries, window, cx);
                        }),
                    )
                    .on_drag(dragged, |dragged: &DraggedItem, _, _, cx| {
                        let style = super::style::UiStyle::get(cx);
                        let label = dragged.label.clone();
                        cx.new(|_| DragPreview { label, style })
                    })
                    .drag_over::<DraggedItem>(move |s, _, _, _| s.border_l_2().border_color(accent))
                    .can_drop(|any, _, _| {
                        any.downcast_ref::<DraggedItem>()
                            .is_some_and(|d| d.item.as_panel().is_some())
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                        this.drop_item(d, DropTarget::Dock { pos, index }, window, cx);
                    }))
                    .child(crate::icons::glyph(icon))
                    .child(title)
                    .child(probe(&self.regions, Region::DockTab(pos, index)))
                    .into_any_element()
            })
            .collect();

        let append_index = dock.panels.len();
        let body: AnyElement = match dock
            .active
            .as_ref()
            .and_then(|a| self.panel_view(a, window, cx))
        {
            Some(view) => cached_view(&self.regions, view),
            None => div()
                .size_full()
                .p(px(tokens.spacing * 3.0))
                .text_color(theme.muted)
                .child(format!(
                    "Empty dock. Drag a panel here, or press {} to open one.",
                    palette_key(window)
                ))
                .into_any_element(),
        };

        let dock_index = DockPosition::ALL
            .iter()
            .position(|p| *p == pos)
            .unwrap_or(0);
        let root = div()
            .id(id(format!("dock-{pos:?}")))
            .track_focus(&self.dock_focus[dock_index])
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .bg(match pos {
                DockPosition::Left => theme.sidebar,
                DockPosition::Right => theme.right_sidebar,
                DockPosition::Bottom => theme.bg,
            })
            .border_color(theme.border)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.mutate(cx, |s| s.focus_dock(pos));
                    this.focus_root(window, cx);
                }),
            );
        let root = match pos {
            DockPosition::Left => root.w(px(dock.size)).border_r_1(),
            DockPosition::Right => root.w(px(dock.size)).border_l_1(),
            DockPosition::Bottom => root.h(px(dock.size)).w_full().border_t_1(),
        };

        let tab_header = (pos == DockPosition::Bottom).then(|| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .h(px(tokens.tab_height))
                .w_full()
                .px(px(4.0))
                .gap(px(2.0))
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(tokens.font_size))
                .children(tabs)
                .child(
                    div()
                        .id(id(format!("dock-tab-filler-{pos:?}")))
                        .flex_1()
                        .h_full()
                        .drag_over::<DraggedItem>(move |s, _, _, _| s.bg(accent.opacity(0.18)))
                        .can_drop(|any, _, _| {
                            any.downcast_ref::<DraggedItem>()
                                .is_some_and(|d| d.item.as_panel().is_some())
                        })
                        .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                            this.drop_item(
                                d,
                                DropTarget::Dock {
                                    pos,
                                    index: append_index,
                                },
                                window,
                                cx,
                            );
                        })),
                )
        });
        let top: Vec<AnyElement> = match pos {
            DockPosition::Left => self.left_sidebar_top(&dock, window, cx),
            DockPosition::Right => vec![self.right_header(&dock, cx)],
            DockPosition::Bottom => Vec::new(),
        };
        let toolbar = is_left.then(|| self.left_toolbar());

        let dock_el = root
            .children(top)
            .children(tab_header)
            .child(
                div()
                    .id(id(format!("dock-body-{pos:?}")))
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .drag_over::<DraggedItem>(move |s, _, _, _| s.bg(accent.opacity(0.10)))
                    .can_drop(|any, _, _| {
                        any.downcast_ref::<DraggedItem>()
                            .is_some_and(|d| d.item.as_panel().is_some())
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                        this.drop_item(
                            d,
                            DropTarget::Dock {
                                pos,
                                index: append_index,
                            },
                            window,
                            cx,
                        );
                    }))
                    .child(body),
            )
            .children(toolbar)
            .child(probe(&self.regions, Region::Dock(pos)))
            .into_any_element();
        // The right panel stands beside its activity rail, which is outside the body and stays
        // when the body collapses.
        if pos == DockPosition::Right {
            let rail = self.right_rail(&dock, window, cx);
            return Some(
                div()
                    .flex()
                    .flex_none()
                    .child(dock_el)
                    .child(rail)
                    .into_any_element(),
            );
        }
        Some(dock_el)
    }

    /// The drag handle of a dock's inner edge. It is a child of the workbench root, painted after
    /// every dock and split and occluding what is under it, so nothing inside a dock (nav rows,
    /// a cached panel's replayed hitboxes, the toolbar) can sit above it or take its click.
    fn render_dock_handle(
        &mut self,
        pos: DockPosition,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.dock_mode(pos) != DockMode::Expanded {
            return None;
        }
        let size = self.state.docks.get(pos).size;
        let accent = self.theme.accent;
        let status_h = self.density.ui().status_bar;
        let base = div()
            .id(id(format!("dock-resize-{pos:?}")))
            .absolute()
            .occlude()
            .hover(move |s| s.bg(accent.opacity(0.35)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.start_dock_resize(pos, event.position);
                    cx.stop_propagation();
                }),
            );
        let left = self.dock_extent(DockPosition::Left);
        let right = self.dock_extent(DockPosition::Right);
        Some(
            match pos {
                DockPosition::Left => base
                    .left(px(size - HANDLE_THICKNESS))
                    .top_0()
                    .bottom(px(status_h))
                    .w(px(HANDLE_THICKNESS))
                    .cursor_col_resize(),
                DockPosition::Right => base
                    .right(px(right - HANDLE_THICKNESS))
                    .top_0()
                    .bottom(px(status_h))
                    .w(px(HANDLE_THICKNESS))
                    .cursor_col_resize(),
                DockPosition::Bottom => base
                    .left(px(left))
                    .right(px(right))
                    .bottom(px(status_h + size - HANDLE_THICKNESS))
                    .h(px(HANDLE_THICKNESS))
                    .cursor_row_resize(),
            }
            .into_any_element(),
        )
    }

    /// While a side dock's edge is dragged past its minimum far enough to collapse it, a preview
    /// of the rail it will become on release (Escape cancels).
    fn render_collapse_preview(&mut self) -> Option<AnyElement> {
        let pos = self.armed_collapse()?;
        let theme = self.theme;
        let status_h = self.density.ui().status_bar;
        let base = div()
            .id(id(format!("collapse-preview-{pos:?}")))
            .absolute()
            .top_0()
            .bottom(px(status_h))
            .w(px(pos.rail_width()))
            .bg(theme.accent.opacity(0.28))
            .child(probe(&self.regions, Region::CollapsePreview(pos)));
        Some(
            match pos {
                DockPosition::Right => base.right_0(),
                _ => base.left_0(),
            }
            .into_any_element(),
        )
    }

    /// While a tab/panel is dragged, hidden docks expose a thin drop strip at their window edge.
    fn render_hidden_dock_targets(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if !cx.has_active_drag() {
            return Vec::new();
        }
        let theme = self.theme;
        let status_h = self.density.ui().status_bar;
        let accent = theme.accent;
        DockPosition::ALL
            .into_iter()
            .filter(|pos| self.dock_mode(*pos) == DockMode::Hidden)
            .map(|pos| {
                let index = self.state.docks.get(pos).panels.len();
                let base = div()
                    .id(id(format!("hidden-dock-drop-{pos:?}")))
                    .absolute()
                    .bg(accent.opacity(0.10))
                    .drag_over::<DraggedItem>(move |s, _, _, _| s.bg(accent.opacity(0.35)))
                    .can_drop(|any, _, _| {
                        any.downcast_ref::<DraggedItem>()
                            .is_some_and(|d| d.item.as_panel().is_some())
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                        this.drop_item(d, DropTarget::Dock { pos, index }, window, cx);
                    }));
                match pos {
                    DockPosition::Left => base
                        .left_0()
                        .top(px(TITLEBAR_HEIGHT))
                        .bottom(px(status_h))
                        .w(px(28.0)),
                    DockPosition::Right => base
                        .right_0()
                        .top(px(TITLEBAR_HEIGHT))
                        .bottom(px(status_h))
                        .w(px(28.0)),
                    DockPosition::Bottom => {
                        base.left_0().right_0().bottom(px(status_h)).h(px(28.0))
                    }
                }
                .into_any_element()
            })
            .collect()
    }

    // ---- center ------------------------------------------------------------------------------

    fn render_center(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let zoomed = self
            .state
            .zoomed
            .and_then(|z| self.state.center.pane(z).cloned());
        let body = match zoomed {
            Some(pane) => self.render_pane(&pane, window, cx),
            None => {
                let tree = self.state.center.clone();
                self.render_tree(&tree, Vec::new(), window, cx)
            }
        };
        div()
            .id("center")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(theme.bg)
            .child(body)
            .child(probe(&self.regions, Region::Center))
            .into_any_element()
    }

    fn render_tree(
        &mut self,
        tree: &PaneTree,
        path: Vec<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match tree {
            PaneTree::Pane(pane) => self.render_pane(pane, window, cx),
            PaneTree::Split {
                axis,
                ratios,
                children,
            } => {
                let horizontal = *axis == Axis::Horizontal;
                let bounds_cell: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
                let accent = self.theme.accent;
                let mut container = div()
                    .relative()
                    .size_full()
                    .flex()
                    .when(!horizontal, |s| s.flex_col())
                    .child({
                        // Records the container's painted bounds so a divider drag can convert
                        // mouse pixels to a ratio.
                        let cell = Rc::clone(&bounds_cell);
                        canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                    });
                for (index, (child, ratio)) in children.iter().zip(ratios).enumerate() {
                    let mut child_path = path.clone();
                    child_path.push(index);
                    let element = self.render_tree(child, child_path, window, cx);
                    container = container.child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_basis(px(0.0))
                            .flex_grow(*ratio)
                            .flex_shrink(1.0)
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .child(div().flex_1().min_h_0().min_w_0().child(element)),
                    );
                }
                let mut cumulative = 0.0;
                for (divider, ratio) in ratios
                    .iter()
                    .take(ratios.len().saturating_sub(1))
                    .enumerate()
                {
                    cumulative += *ratio;
                    let cell = Rc::clone(&bounds_cell);
                    let path = path.clone();
                    let handle = div()
                        .id(id(format!("splitter-{path:?}-{divider}")))
                        .absolute()
                        .hover(move |s| s.bg(accent.opacity(0.35)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                let bounds = cell.get();
                                let (origin, extent) = if horizontal {
                                    (f32::from(bounds.origin.x), f32::from(bounds.size.width))
                                } else {
                                    (f32::from(bounds.origin.y), f32::from(bounds.size.height))
                                };
                                this.resizing = Some(Resizing::Split {
                                    path: path.clone(),
                                    divider,
                                    horizontal,
                                    origin,
                                    extent,
                                });
                                cx.stop_propagation();
                            }),
                        );
                    container = container.child(if horizontal {
                        handle
                            .left(relative(cumulative))
                            .ml(px(-HANDLE_THICKNESS / 2.0))
                            .top_0()
                            .h_full()
                            .w(px(HANDLE_THICKNESS))
                            .cursor_col_resize()
                    } else {
                        handle
                            .top(relative(cumulative))
                            .mt(px(-HANDLE_THICKNESS / 2.0))
                            .left_0()
                            .w_full()
                            .h(px(HANDLE_THICKNESS))
                            .cursor_row_resize()
                    });
                }
                container.into_any_element()
            }
        }
    }

    fn render_pane(
        &mut self,
        pane: &Pane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let accent = theme.accent;
        let pane_id = pane.id;
        let multiple = self.state.panes().len() > 1;
        let focused = self.state.focus == FocusTarget::Pane(pane_id);
        let dragging = cx.has_active_drag();

        let tabs: Vec<AnyElement> = pane
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                self.render_tab(pane_id, index, item, index == pane.active, focused, cx)
            })
            .collect();

        let strip = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(tokens.tab_height))
            .w_full()
            .px(px(4.0))
            .gap(px(2.0))
            .overflow_hidden()
            .border_b_1()
            .border_color(theme.border)
            .children(tabs)
            .child(
                div()
                    .id(id(format!("pane-strip-filler-{pane_id}")))
                    .flex_1()
                    .h_full()
                    .drag_over::<DraggedItem>(move |s, _, _, _| s.bg(accent.opacity(0.18)))
                    .on_drop({
                        let append = pane.items.len();
                        cx.listener(move |this, d: &DraggedItem, window, cx| {
                            this.drop_item(
                                d,
                                DropTarget::Pane {
                                    pane: pane_id,
                                    index: append,
                                },
                                window,
                                cx,
                            );
                        })
                    })
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let entries = vec![
                                Workbench::entry("Split Right", move |this, window, cx| {
                                    this.split_pane_with_terminal(pane_id, Edge::Right, window, cx);
                                }),
                                Workbench::entry("Split Down", move |this, window, cx| {
                                    this.split_pane_with_terminal(
                                        pane_id,
                                        Edge::Bottom,
                                        window,
                                        cx,
                                    );
                                }),
                            ];
                            this.open_menu(event.position, entries, window, cx);
                        }),
                    ),
            );

        let content = match pane.active_item() {
            Some(item) => self.render_item(pane_id, item, window, cx),
            None => ui::empty_state(
                Mascot::Empty,
                format!(
                    "No open items — ⌘T for a new tab, {} for commands",
                    palette_key(window)
                ),
                Some(self.empty_start_button(&theme, &tokens)),
                &theme,
                &tokens,
            )
            .into_any_element(),
        };

        let zones: Vec<AnyElement> = if dragging {
            [
                (
                    "top",
                    DropTarget::Split {
                        pane: pane_id,
                        edge: Edge::Top,
                    },
                    (0.0, 0.0, 1.0, 0.25),
                ),
                (
                    "bottom",
                    DropTarget::Split {
                        pane: pane_id,
                        edge: Edge::Bottom,
                    },
                    (0.0, 0.75, 1.0, 0.25),
                ),
                (
                    "left",
                    DropTarget::Split {
                        pane: pane_id,
                        edge: Edge::Left,
                    },
                    (0.0, 0.25, 0.25, 0.5),
                ),
                (
                    "right",
                    DropTarget::Split {
                        pane: pane_id,
                        edge: Edge::Right,
                    },
                    (0.75, 0.25, 0.25, 0.5),
                ),
                (
                    "center",
                    DropTarget::Pane {
                        pane: pane_id,
                        index: pane.items.len(),
                    },
                    (0.25, 0.25, 0.5, 0.5),
                ),
            ]
            .into_iter()
            .map(|(name, target, (l, t, w, h))| {
                div()
                    .id(id(format!("drop-{pane_id}-{name}")))
                    .absolute()
                    .left(relative(l))
                    .top(relative(t))
                    .w(relative(w))
                    .h(relative(h))
                    .drag_over::<DraggedItem>(move |s, _, _, _| {
                        s.bg(accent.opacity(0.25)).border_2().border_color(accent)
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                        this.drop_item(d, target.clone(), window, cx);
                    }))
                    .into_any_element()
            })
            .collect()
        } else {
            Vec::new()
        };

        div()
            .id(id(format!("pane-{pane_id}")))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| this.focus_pane(pane_id, window, cx)),
            )
            .child(probe(&self.regions, Region::Pane(pane_id)))
            .child(strip)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(div().size_full().child(content))
                    .children(zones),
            )
            .when(focused && multiple, move |s| {
                s.child(
                    div()
                        .absolute()
                        .inset_0()
                        .border_1()
                        .border_color(accent.opacity(0.5)),
                )
            })
            .into_any_element()
    }

    fn render_tab(
        &mut self,
        pane_id: PaneId,
        index: usize,
        item: &ItemRef,
        active: bool,
        pane_focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let accent = theme.accent;
        let hover = theme.surface_hover;
        let (icon, title) = self.item_icon_and_title(item, cx);
        let tab_tip = self.item_tooltip(item, cx).map(SharedString::from);
        let badge = session_of_item(item).and_then(|id| {
            let badge = self.sessions.read(cx).agent_badge(id);
            super::panels::agent_badge(
                &theme,
                &badge,
                super::view::BadgeSite::Tab,
                id,
                &self.regions,
                cx,
            )
        });
        let attention = session_of_item(item).and_then(|id| {
            let store = self.sessions.read(cx);
            store
                .session(id)
                .map(|info| store.presentation(info))
                .filter(|p| p.needs_you)
        });
        let dragged = DraggedItem {
            source: ItemSource::Pane {
                pane: pane_id,
                index,
            },
            item: item.clone(),
            label: title.clone(),
        };
        div()
            .id(id(format!("tab-{pane_id}-{index}")))
            .group("tab")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing))
            .pl(px(tokens.spacing * 2.0))
            .pr(px(tokens.spacing))
            .h(px(tokens.tab_height - 8.0))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .text_color(if active { theme.fg } else { theme.muted })
            .when(active, |s| {
                s.bg(theme.surface)
                    .border_1()
                    .border_color(if pane_focused {
                        accent.opacity(0.55)
                    } else {
                        theme.border
                    })
            })
            .when(!active, |s| s.hover(move |s| s.bg(hover)))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.try_mutate(cx, |s| {
                    s.set_active_item(pane_id, index)?;
                    s.focus_pane(pane_id)
                });
                if event.click_count() == 2 {
                    this.mutate(cx, |s| s.toggle_zoom());
                }
                this.focus_root(window, cx);
            }))
            .on_aux_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.is_middle_click() {
                    this.close_item_checked(pane_id, index, window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    let entries = this.tab_session_menu(pane_id, index);
                    this.open_menu(event.position, entries, window, cx);
                }),
            )
            .when_some(tab_tip, |tab, tip| {
                tab.tooltip(move |_, cx| cx.new(|_| super::style::TextTooltip(tip.clone())).into())
            })
            .on_drag(dragged, |dragged: &DraggedItem, _, _, cx| {
                let style = super::style::UiStyle::get(cx);
                let label = dragged.label.clone();
                cx.new(|_| DragPreview { label, style })
            })
            .drag_over::<DraggedItem>(move |s, _, _, _| s.border_l_2().border_color(accent))
            .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                this.drop_item(
                    d,
                    DropTarget::Pane {
                        pane: pane_id,
                        index,
                    },
                    window,
                    cx,
                );
            }))
            .child(crate::icons::glyph(icon))
            .child(title)
            .children(badge)
            .children(attention.map(|p| {
                div()
                    .flex_none()
                    .size(px(7.0))
                    .rounded_full()
                    .bg(super::panels::tone_color(&theme, p.tone))
            }))
            // A file with unsaved changes: a dot in the text color (not a status hue).
            .children(
                item.as_file()
                    .and_then(|path| self.buffers.get(path))
                    .filter(|buffer| buffer.read(cx).is_dirty())
                    .map(|_| {
                        div()
                            .id(id(format!("tab-dirty-{pane_id}-{index}")))
                            .flex_none()
                            .size(px(7.0))
                            .rounded_full()
                            .bg(theme.fg)
                            .tooltip(|_, cx| {
                                cx.new(|_| super::style::TextTooltip("Unsaved changes".into()))
                                    .into()
                            })
                    }),
            )
            .child(
                div()
                    .id(id(format!("tab-close-{pane_id}-{index}")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(tokens.font_size + 4.0))
                    .rounded(px(tokens.radius))
                    .text_color(theme.muted)
                    .hover(move |s| s.bg(hover).text_color(theme.fg))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        // `terminal.tab_close = close_session`: the × closes the session too.
                        let session = this
                            .state
                            .pane(pane_id)
                            .and_then(|p| p.items.get(index))
                            .and_then(super::terminals::session_of_item);
                        match session {
                            Some(session)
                                if this.terminal_settings.tab_close
                                    == crate::settings::TabClose::CloseSession
                                    && this.can_close_sessions(cx) =>
                            {
                                this.close_session(session, window, cx);
                            }
                            _ => {
                                this.close_item_checked(pane_id, index, window, cx);
                            }
                        }
                        cx.stop_propagation();
                    }))
                    .child(crate::icons::glyph(crate::icons::Icon::ActionClose)),
            )
            .child(probe(&self.regions, Region::Tab(pane_id, index)))
            .into_any_element()
    }

    fn render_item(
        &mut self,
        pane: PaneId,
        item: &ItemRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        match item {
            ItemRef::Panel { panel } => match self.panel_view(panel, window, cx) {
                Some(view) => cached_view(&self.regions, view),
                None => self.placeholder_body(format!("Panel `{panel}` is not registered"), cx),
            },
            ItemRef::Placeholder { title } => ui::empty_state(
                Mascot::Empty,
                format!(
                    "{title}. Start an agent here, open a session from the sessions panel, or press ⌘T for a new terminal."
                ),
                Some(self.empty_start_button(&theme, &tokens)),
                &theme,
                &tokens,
            )
            .into_any_element(),
            ItemRef::Terminal { session } => self.render_terminal_item(pane, session, window, cx),
            ItemRef::Chat { session } => self.render_chat_item(pane, session, cx),
            ItemRef::Changes { path } => {
                let view = self.changes_view(path, window, cx);
                cached_view(&self.regions, view)
            }
            ItemRef::File { path } => self.render_file_item(pane, path, window, cx),
        }
    }

    /// The "▶ Start…" button of an empty pane or project: the Start… sheet in the project in
    /// focus.
    fn empty_start_button(
        &self,
        theme: &crate::theme::Theme,
        tokens: &crate::theme::Tokens,
    ) -> AnyElement {
        ui::button(
            "pane-empty-start",
            "Start…",
            ui::ButtonKind::Primary,
            true,
            theme,
            tokens,
        )
        .relative()
        .on_click(|_, window, cx| {
            window.dispatch_action(
                Box::new(actions::OpenLauncher(actions::LauncherOpen::default())),
                cx,
            );
        })
        .child(probe(&self.regions, Region::Launcher("empty-start")))
        .into_any_element()
    }

    pub(crate) fn placeholder_body(&self, text: String, _cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(self.theme.muted)
            .child(text)
            .into_any_element()
    }

    // ---- status bar --------------------------------------------------------------------------

    fn render_status_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let data = {
            let store = self.sessions.read(cx);
            StatusData {
                connection: store.connection_state(),
                connection_line: self.daemon_line(cx),
                epoch: store.state().epoch.map(|e| e.to_string()),
                sessions: store.sessions().len(),
                workspace: store.active_workspace().map(|w| w.name.clone()),
                needs_you: store.needs_you().len(),
                approvals: store.approvals().len(),
                terminal_theme: self.terminal_theme_name.clone(),
                docks: {
                    let state = |pos: DockPosition| {
                        let dock = self.state.docks.get(pos);
                        DockState {
                            // The body is what the button shows and hides.
                            visible: self.dock_mode(pos) == DockMode::Expanded,
                            has_panels: !dock.panels.is_empty(),
                        }
                    };
                    PerDock {
                        left: state(DockPosition::Left),
                        right: state(DockPosition::Right),
                        bottom: state(DockPosition::Bottom),
                    }
                },
                editor: self.editor_status(cx),
            }
        };
        let hint = |action: &dyn Action| key_hint(window, action).unwrap_or_default();
        let weak = cx.weak_entity();
        let editor_items = self
            .editor_items
            .get_or_insert_with(|| super::editor_status::EditorItems::new(weak, cx))
            .clone();
        let ctx = StatusContext {
            theme: &theme,
            density: self.density,
            palette_hint: key_hint(window, &actions::TogglePalette)
                .unwrap_or_else(|| "Commands".into()),
            attention_hint: hint(&actions::JumpToNextAttention),
            dock_hints: PerDock {
                left: hint(&actions::ToggleLeftDock),
                right: hint(&actions::ToggleRightDock),
                bottom: hint(&actions::ToggleBottomDock),
            },
            data: &data,
            editor_items: &editor_items,
            height: tokens.status_bar,
        };
        let status_bar = self.status_bar();
        let render_side = |ids: &[String]| -> Vec<AnyElement> {
            ids.iter()
                .filter_map(|item| self.status_items.get(item))
                .map(|spec| {
                    div()
                        .relative()
                        .child((spec.render)(&ctx))
                        .child(probe(&self.regions, Region::StatusItem(spec.id)))
                        .into_any_element()
                })
                .collect()
        };
        let left = render_side(&status_bar.left);
        let right = render_side(&status_bar.right);
        div()
            .id("status-bar")
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(px(tokens.status_bar))
            .w_full()
            .px(px(tokens.spacing * 2.0))
            .bg(theme.sidebar)
            .border_t_1()
            .border_color(theme.border)
            .text_color(theme.muted)
            .text_size(px(tokens.secondary_font_size))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 3.0))
                    .children(left)
                    .children(
                        self.notice
                            .clone()
                            .map(|n| div().text_color(theme.accent).child(n)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .children(right),
            )
            .child(probe(&self.regions, Region::StatusBar))
            .into_any_element()
    }

    // ---- palette -----------------------------------------------------------------------------

    fn render_palette(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let palette = self.palette.clone()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        const VISIBLE_ROWS: usize = 12;
        let start = palette.selected.saturating_sub(VISIBLE_ROWS - 1);
        let rows: Vec<AnyElement> = palette
            .matches
            .iter()
            .enumerate()
            .skip(start)
            .take(VISIBLE_ROWS)
            .filter_map(|(row, command_index)| {
                let command = self.commands.get(*command_index)?;
                let selected = row == palette.selected;
                let hint = key_hint(window, &*command.hint).unwrap_or_default();
                let hover = theme.surface_hover;
                Some(
                    div()
                        .id(id(format!("palette-row-{row}")))
                        .flex()
                        .items_center()
                        .justify_between()
                        .h(px(tokens.sidebar_row + 2.0))
                        .px(px(tokens.spacing * 2.0))
                        .rounded(px(tokens.radius))
                        .cursor_pointer()
                        .when(selected, |s| s.bg(theme.accent_soft))
                        .when(!selected, |s| s.hover(move |s| s.bg(hover)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.palette_confirm(Some(row), window, cx);
                        }))
                        .child(command.title.clone())
                        .child(div().text_color(theme.muted).child(hint))
                        .into_any_element(),
                )
            })
            .collect();
        let empty = palette.matches.is_empty();

        Some(
            div()
                .id("palette-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(80.0))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
                )
                .child(
                    div()
                        .id("palette")
                        .key_context("Palette")
                        .track_focus(&self.palette_focus)
                        .w(px(560.0))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .popover(&theme, &tokens)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            // Typed text arrives through the field's platform text input.
                            if this.overlay_key(OverlayField::Palette, event, cx) {
                                cx.stop_propagation();
                            }
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectNext, _, cx| {
                            this.palette_select(true, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectPrev, _, cx| {
                            this.palette_select(false, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::Confirm, window, cx| {
                            this.palette_confirm(None, window, cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .h(px(tokens.sidebar_row + 8.0))
                                .px(px(tokens.spacing * 2.0))
                                .mb(px(4.0))
                                .border_b_1()
                                .border_color(theme.border)
                                .child(div().text_color(theme.faint).child("› "))
                                .children(self.overlay_parts(OverlayField::Palette, "", cx)),
                        )
                        .children(rows)
                        .when(empty, |s| {
                            s.child(
                                div()
                                    .px(px(tokens.spacing * 2.0))
                                    .text_color(theme.muted)
                                    .child("No matching commands"),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    // ---- context menu ------------------------------------------------------------------------

    fn render_context_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ContextMenu { position, entries } = self.context_menu.clone()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let hover = theme.surface_hover;
        let rows: Vec<AnyElement> = entries
            .into_iter()
            .enumerate()
            .map(|(i, entry)| {
                let disabled = entry.disabled;
                div()
                    .id(id(format!("menu-row-{i}")))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .h(px(tokens.sidebar_row))
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .when(disabled, |s| s.text_color(theme.faint))
                    .when(!disabled, |s| {
                        s.cursor_pointer().hover(move |s| s.bg(hover))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu_entry(i, window, cx);
                    }))
                    .child(
                        div().w(px(12.0)).children(
                            (entry.checked)
                                .then(|| crate::icons::glyph(crate::icons::Icon::StateCompleted)),
                        ),
                    )
                    .child(div().flex_1().child(entry.label))
                    .when(entry.submenu, |s| {
                        s.child(crate::icons::glyph(crate::icons::Icon::ActionDisclose))
                    })
                    .child(probe(&self.regions, Region::MenuEntry(i)))
                    .into_any_element()
            })
            .collect();
        Some(
            deferred(
                anchored()
                    .position(point(position.x, position.y))
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .id("context-menu")
                            .key_context("ContextMenu")
                            .track_focus(&self.menu_focus)
                            .occlude()
                            .min_w(px(190.0))
                            .flex()
                            .flex_col()
                            .popover(&theme, &tokens)
                            .text_size(px(tokens.font_size))
                            .on_mouse_down_out(
                                cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
                            )
                            .children(rows),
                    ),
            )
            .priority(10)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod titlebar_tests {
    #[test]
    fn the_titlebar_constant_equals_the_built_in_packs_metric() {
        assert_eq!(
            super::TITLEBAR_HEIGHT,
            crate::theme::metrics().titlebar_height
        );
    }
}
