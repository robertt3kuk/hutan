//! Rendering and action wiring for [`Workbench`].

use super::actions::{self, palette};
use super::model::{
    Axis, Direction, DockMode, DockPosition, DropTarget, Edge, FocusTarget, ItemRef, ItemSource,
    Pane, PaneId, PaneTree, PanelDest,
};
use super::panels::AgentsRequest;
use super::registry::{BarWidth, DockState, PerDock, StatusContext, StatusData, StatusSide};
use super::terminals::session_of_item;
use super::ui::motion::{self, PresentExt as _};
use super::ui::scroll::ScrollExt as _;
use super::ui::{self, Mascot, StyledExt as _};
use super::view::{ContextMenu, DragPreview, DraggedItem, Region, Resizing, Workbench, steps};
use super::vim_ui::OverlayField;
use super::workspaces::PickerKind;
use crate::theme::{Density, DensityExt as _};
use gpui::{
    Action, AnyElement, AnyView, Bounds, ClickEvent, Context, Div, ElementId, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement,
    Pixels, Render, SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window,
    anchored, canvas, deferred, div, point, prelude::*, px, relative, uniform_list,
};
use std::cell::Cell;
use std::ops::Range;
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
pub(super) const HANDLE_THICKNESS: f32 = 6.0;
/// The accent line a hovered divider draws in the middle of its hit area (FX-UI-B: a hairline
/// of 1–2 px beside the 1 px border it sits on, where a 6 px tinted bar was heavy).
pub(super) const HANDLE_LINE: f32 = 2.0;
/// How much wider than [`HANDLE_THICKNESS`] the left dock's handle is, on the center's side of its
/// edge: the right one has the dock's whole edge, the left one lay inside it and a drag at the
/// very edge (a pixel outside the dock) hit the center instead (N-04).
const LEFT_HANDLE_OUTSET: f32 = 4.0;
/// Room above a center pane's tabs in their strip; a tab reaches the strip's bottom, so the
/// active one is connected to its pane.
const TAB_TOP_INSET: f32 = 6.0;
/// A tab's title is cut with an ellipsis beyond this width (the tooltip has it in full).
const TAB_TITLE_MAX: f32 = 180.0;
/// How often a tab's working spinner is redrawn: it stays on screen for as long as an agent
/// works, and every frame draws the whole window.
const TAB_SPINNER_FPS: f32 = 8.0;
/// How far down the palette's panel starts, and the margin it keeps at the window's bottom.
const PALETTE_TOP: f32 = 80.0;
const PALETTE_BOTTOM_MARGIN: f32 = 24.0;
/// A title this long is likely cut, so its tab gets the title as a tooltip when it has none.
const TAB_TITLE_TOOLTIP_CHARS: usize = 20;

/// How a tab of a center pane is drawn (CL-CHROME). The strip is chrome (`sidebar`); the active
/// tab is a `bg` fill connected to its pane; the other tabs have no fill (a hover fill only).
/// The focused pane's active tab is `fg` medium, every other tab `muted` regular: the weight
/// and the color of one tab are the whole focus indication, with the terminal's hollow cursor.
/// No outline anywhere, no accent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabLook {
    pub text: gpui::Hsla,
    pub weight: gpui::FontWeight,
    /// The connected fill of the active tab; `None` for the others.
    pub fill: Option<gpui::Hsla>,
}

impl TabLook {
    pub fn of(theme: &crate::theme::Theme, active: bool, pane_focused: bool) -> Self {
        let focused = active && pane_focused;
        Self {
            text: if focused { theme.fg } else { theme.muted },
            weight: if focused {
                gpui::FontWeight::MEDIUM
            } else {
                gpui::FontWeight::NORMAL
            },
            fill: active.then_some(theme.bg),
        }
    }
}

/// "⌘⇧O"-style hint for the highest-precedence binding of `action` in the current context.
pub(super) fn key_hint(window: &Window, action: &dyn Action) -> Option<SharedString> {
    let binding = window.highest_precedence_binding_for_action(action)?;
    let text: Vec<String> = binding.keystrokes().iter().map(|k| k.to_string()).collect();
    Some(text.join(" ").into())
}

/// The palette's key in the active keymap ("⌘⇧P"), for hints in prose.
fn palette_key(window: &Window) -> SharedString {
    key_hint(window, &actions::TogglePalette).unwrap_or_else(|| "the command palette".into())
}

/// The line under the empty centre's sentence: the two ways to put something in it.
fn empty_centre_hint(quick_open: &str, new_terminal: &str) -> String {
    format!("{quick_open} opens a file or a session · {new_terminal} a new terminal")
}

/// The line a divider's hit area draws while the pointer is over it: `horizontal` is a vertical
/// bar (the divider between side-by-side panes). The hit area carries `.group(group)`.
pub(super) fn divider_line(group: &SharedString, horizontal: bool, accent: Hsla) -> Div {
    let inset = (HANDLE_THICKNESS - HANDLE_LINE) / 2.0;
    let line = div().absolute();
    let line = if horizontal {
        line.top_0().bottom_0().left(px(inset)).w(px(HANDLE_LINE))
    } else {
        line.left_0().right_0().top(px(inset)).h(px(HANDLE_LINE))
    };
    line.group_hover(group.clone(), move |s| s.bg(accent))
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

/// [`probe`] for a view that lays itself out by its parent's width (a header that gives way
/// chip by chip): when the painted width is not the one `seen` holds, the view that painted it
/// is rendered again, once, with the width it now has in the regions.
pub(super) fn probe_width(
    regions: &Regions,
    region: Region,
    seen: std::rc::Rc<std::cell::Cell<Option<f32>>>,
) -> impl IntoElement {
    let regions = regions.clone();
    canvas(
        |bounds, _, _| bounds,
        move |_, bounds, window, cx| {
            regions.record(window.current_view(), region, bounds);
            let width = f32::from(bounds.size.width);
            if seen.replace(Some(width)) != Some(width) {
                cx.notify(window.current_view());
            }
        },
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
        // The tab says what is open; the window says where (the project), never the same twice.
        let title = match self.window_label(cx) {
            Some(label) => format!("{label} — Hutan"),
            None => "Hutan".to_owned(),
        };
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
        let peek = self.render_peek(window, cx);
        let left = self.render_dock(DockPosition::Left, window, cx);
        let right = self.render_dock(DockPosition::Right, window, cx);
        let bottom = self.render_dock(DockPosition::Bottom, window, cx);
        self.drop_stale_composer();
        let center = self.render_center(window, cx);
        let titlebar = self.render_titlebar(window, cx);
        let status = self.render_status_bar(window, cx);
        let offer = self.render_notify_offer(cx);
        let layout_offer = self.render_layout_offer(cx);
        let reinstall_card = self.render_reinstall_card(cx);
        let start_failure = self.render_start_failure_banner(cx);
        let mismatch = self.render_daemon_mismatch_banner();
        self.rescue_orphaned_focus(window, cx);
        self.sync_assistant_focus(window, cx);
        self.sync_context_panel(window, cx);
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
        let modal = self.render_modal(window, cx);
        let close_toast = self.render_close_toast(cx);
        let attention_toasts = self.render_attention_toasts(window, cx);
        let restore_toast = self.render_restore_toast(cx);
        let layout_toast = self.render_layout_toast(cx);
        let instructions_toast = self.render_instructions_toast(cx);
        let usage_toast = self.render_usage_toast(cx);
        let paste_toast = self.render_paste_toast(cx);
        let setup_toast = self.render_setup_toast(cx);
        let toasts = self.toast_stack([
            attention_toasts,
            close_toast,
            restore_toast,
            layout_toast,
            instructions_toast,
            usage_toast,
            paste_toast,
            setup_toast,
        ]);
        let checkpoints_list = self.render_checkpoints_list(cx);
        let rewind_sheet = self.render_rewind_sheet(cx);
        let daemon_popover = self.render_daemon_popover(cx);
        let folders_popover = self.render_folders_popover(window, cx);
        let fork_popover = self.render_fork_popover(window, cx);
        let recovery = self.render_recovery_summary(cx);
        let sheet = self.render_settings_sheet(cx);
        let setup = self.render_setup(window, cx);
        // A tooltip shown before a sheet, menu or palette opened must not stay over it.
        let overlay = [
            palette.is_some(),
            picker.is_some(),
            agent_picker.is_some(),
            launcher.is_some(),
            menu.is_some(),
            modal.is_some(),
            sheet.is_some(),
            setup.is_some(),
            checkpoints_list.is_some(),
            rewind_sheet.is_some(),
            folders_popover.is_some(),
            fork_popover.is_some(),
        ]
        .contains(&true);
        if overlay && !self.overlay_open {
            super::style::TooltipGate::bump(cx);
        }
        self.overlay_open = overlay;
        self.regions.end_frame();
        if self.regions.take_fresh() {
            window.on_next_frame(|window, _| window.refresh());
        }

        let resizing_docks = matches!(self.resizing, Some(Resizing::Dock { .. }));
        let root = div()
            .id("workbench")
            .key_context(
                match (
                    resizing_docks,
                    self.layout_undo.is_some(),
                    self.peek.is_some(),
                ) {
                    (true, true, _) => "Workbench DockResize LayoutToast",
                    (true, false, _) => "Workbench DockResize",
                    (false, true, true) => "Workbench LayoutToast Peek",
                    (false, true, false) => "Workbench LayoutToast",
                    (false, false, true) => "Workbench Peek",
                    (false, false, false) => "Workbench",
                },
            )
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .font_family(super::ui::ui_family())
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            // A drag of a dock edge or a divider is followed by `docks::resize_observer`, not by
            // this element: the handle under the pointer occludes it.
            ;
        let root = self.bind_actions(root, cx);
        let root = self.bind_rewind_actions(root, cx);

        root.child(
            div()
                .relative()
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
                        .children(offer)
                        .children(layout_offer)
                        .children(start_failure)
                        .children(mismatch)
                        .children(reinstall_card)
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
                .children(right)
                // The peek floats over the center, after everything of the row.
                .children(peek),
        )
        .child(status)
        .child(super::ui::keyboard_focus_observer())
        .child(super::style::tooltip_scroll_observer())
        .child(super::docks::resize_observer(cx.entity()))
        // The dock handles come before every popover and menu: an overlay painted after them
        // takes the hit-test and the hover over a handle it covers (the daemon popover's
        // *Start* was dead where it crossed the bottom dock's edge).
        .children(edge_targets)
        .children(handles)
        .children(collapse_preview)
        .children(daemon_popover)
        .children(folders_popover)
        .children(fork_popover)
        .children(recovery)
        .children(palette)
        .children(picker)
        .children(agent_picker)
        .children(launcher)
        .children(toasts)
        .children(menu)
        .children(sheet)
        .children(setup)
        .children(modal)
        .children(checkpoints_list)
        .children(rewind_sheet)
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
        .on_action(
            cx.listener(|this, _: &actions::SplitTerminalRight, window, cx| {
                this.split_terminal_tab(Edge::Right, window, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::FindInTerminal, window, cx| {
                this.open_terminal_find(window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::FindNextInTerminal, _, cx| {
            this.step_terminal_find(true, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::FindPreviousInTerminal, _, cx| {
                this.step_terminal_find(false, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::SplitTerminalDown, window, cx| {
                this.split_terminal_tab(Edge::Bottom, window, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::FocusSplitLeft, window, cx| {
                this.focus_split_or_neighbor(Direction::Left, window, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::FocusSplitRight, window, cx| {
                this.focus_split_or_neighbor(Direction::Right, window, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &actions::FocusSplitUp, window, cx| {
            this.focus_split_or_neighbor(Direction::Up, window, cx)
        }))
        .on_action(
            cx.listener(|this, _: &actions::FocusSplitDown, window, cx| {
                this.focus_split_or_neighbor(Direction::Down, window, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ResizeSplitLeft, _, cx| {
            this.resize_split_or_pane(Direction::Left, cx)
        }))
        .on_action(cx.listener(|this, _: &actions::ResizeSplitRight, _, cx| {
            this.resize_split_or_pane(Direction::Right, cx)
        }))
        .on_action(cx.listener(|this, _: &actions::ResizeSplitUp, _, cx| {
            this.resize_split_or_pane(Direction::Up, cx)
        }))
        .on_action(cx.listener(|this, _: &actions::ResizeSplitDown, _, cx| {
            this.resize_split_or_pane(Direction::Down, cx)
        }))
        .on_action(
            cx.listener(|this, _: &actions::ToggleSplitZoom, window, cx| {
                this.toggle_split_zoom(window, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &actions::CloseSplit, window, cx| {
            this.close_focused_split(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::CloseTerminalTab, window, cx| {
                this.close_terminal_tab(window, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::MoveSplitToNewTab, window, cx| {
                if let Some(session) = this
                    .state
                    .active_term_split()
                    .and_then(|(_, _, s)| s.focused_session().map(str::to_owned))
                {
                    this.move_split_to_new_tab(&session, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, _: &actions::EqualizeSplits, _, cx| this.equalize_splits(cx)))
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
        .on_action(cx.listener(|this, _: &actions::ClosePeek, window, cx| {
            this.close_peek(window, cx);
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
            cx.listener(|this, a: &actions::ShowAgentsPageMenu, window, cx| {
                this.show_agents_page_menu(a.0, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowAgentsPanelMenu, window, cx| {
                this.show_agents_panel_menu(a.0, window, cx);
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
            // With the keyboard in the Files tree ⌘W closes the focused tab, never the panel.
            if this.files_tree_has_focus(window, cx) {
                this.close_center_tab(window, cx);
            // With splits ⌘W closes the focused split; the tab closes with its last one.
            } else if !this.close_focused_split(window, cx) {
                this.close_active_checked(window, cx);
            }
        }))
        .on_action(
            cx.listener(|this, _: &actions::ToggleZoom, _, cx| {
                this.mutate(cx, |s| s.toggle_zoom())
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ResetLayout, _, cx| this.reset_layout(cx)))
        .on_action(
            cx.listener(|this, _: &actions::UseNewDefaultLayout, _, cx| {
                this.use_new_default_layout(cx);
            }),
        )
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
            cx.listener(|this, _: &actions::ToggleQuickTerminal, _, cx| {
                this.toggle_quick_terminal(cx);
            }),
        )
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
            cx.listener(|this, _: &actions::CleanUpWorktrees, window, cx| {
                this.clean_up_worktrees(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ReviewWorktreeSetup, window, cx| {
                this.review_worktree_setup(std::path::PathBuf::from(&a.0), window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowWorktreeSetupOutput, window, cx| {
                this.show_worktree_setup_output(std::path::PathBuf::from(&a.0), window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::RerunWorktreeSetup, window, cx| {
                this.rerun_worktree_setup(std::path::PathBuf::from(&a.0), window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ReviewChangesAt, window, cx| {
                this.open_changes(std::path::PathBuf::from(&a.0), window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::ShowInFiles, window, cx| {
            this.show_in_files(std::path::Path::new(&a.0.checkout), &a.0.file, window, cx);
        }))
        .on_action(cx.listener(|this, a: &actions::ReviewFileAt, window, cx| {
            this.open_changes_file(
                std::path::PathBuf::from(&a.0.checkout),
                &a.0.file,
                window,
                cx,
            );
        }))
        .on_action(
            cx.listener(|this, a: &actions::FinishChangesAt, window, cx| {
                this.open_changes_finish(std::path::PathBuf::from(&a.0), window, cx);
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
        .on_action(
            cx.listener(|this, a: &actions::RevealInstructionFile, _, cx| {
                this.reveal_instruction_file(&a.0, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ShowInstructions, _, cx| {
            this.show_instructions(cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::ShareInstructions, window, cx| {
                this.share_instructions(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::CreateAgentsMd, window, cx| {
                this.create_agents_md(window, cx);
            }),
        )
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
        .on_action(cx.listener(|this, _: &actions::ReviewRequest, window, cx| {
            this.review_request(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::DismissAttentionToasts, _, cx| {
                this.dismiss_attention_toasts(cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::UndoLayoutChange, _, cx| {
            this.undo_layout_change(cx);
        }))
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
                    let mut entries = this.session_menu(id, cx);
                    let after_rename = entries
                        .iter()
                        .position(|entry| entry.label.starts_with("Rename"))
                        .map_or(entries.len(), |at| at + 1);
                    let mut extra: Vec<_> = this
                        .folders_menu_entry(id, a.0.position, cx)
                        .into_iter()
                        .collect();
                    extra.extend(this.fork_menu_entries(id, a.0.position, cx));
                    for (at, entry) in extra.into_iter().enumerate() {
                        entries.insert(after_rename + at, entry);
                    }
                    Workbench::tidy_session_menu(&mut entries);
                    this.open_menu(a.0.position, entries, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowSessionFolders, window, cx| {
                if let Ok(id) = a.0.session.parse() {
                    this.show_session_folders(id, a.0.position, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, a: &actions::ForkChat, window, cx| {
            this.show_fork_popover(a.0.session, a.0.turn, a.0.position, window, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::ForkSessionLatest, window, cx| {
                if let Ok(id) = a.0.session.parse() {
                    this.fork_session_latest(id, a.0.position, false, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ForkTerminalSession, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.fork_terminal_session(id, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::ForkFocusedChat, window, cx| {
                this.fork_focused_chat(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::AddFolderToSession, window, cx| {
                this.add_folder_to_shown_session(window, cx);
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
        .on_action(cx.listener(|this, a: &actions::ShowStopMenu, window, cx| {
            if let Ok(id) = a.0.session.parse() {
                let entries = this.stop_menu(id, cx);
                this.open_menu(a.0.position, entries, window, cx);
            }
        }))
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
            this.close_folders_popover(window, cx);
            this.close_fork_popover(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::Quit, window, cx| {
            this.request_quit(window, cx);
        }))
        .on_action(
            cx.listener(|this, action: &actions::InstallHarness, window, cx| {
                this.install_harness(&action.0, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::RunSetup, window, cx| {
            this.open_setup(window, cx);
        }))
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
        .on_action(cx.listener(|this, _: &actions::QuickOpen, window, cx| {
            if this
                .picker()
                .is_some_and(|p| p.kind == PickerKind::QuickOpen)
            {
                this.close_picker(window, cx);
            } else {
                this.open_quick_open(window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &actions::ShowFiles, window, cx| {
            this.show_files(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::TrackHomeChanges, window, cx| {
                this.track_home_changes(window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ShowProblems, window, cx| {
            this.show_problems(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ReviewLspTrust, _, cx| {
            this.review_lsp_trust(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::ShowSearch, window, cx| {
            this.show_search(false, window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::ShowSearchReplace, window, cx| {
                this.show_search(true, window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::ReplaceInFiles, _, cx| {
            this.replace_in_files(a.0.clone(), cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::OpenAtPosition, window, cx| {
                let position = hutan_protocol::lsp::LspPosition {
                    line: a.0.line,
                    character: a.0.character,
                };
                this.open_at_lsp(a.0.path.clone().into(), position, false, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::RevealActiveFile, window, cx| {
                this.reveal_active_file(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::ShowFileHistory, window, cx| {
                this.show_active_file_history(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowFilesRootMenu, window, cx| {
                this.show_files_root_menu(a.0, window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::ShowFilesMenu, window, cx| {
            this.show_files_menu(&a.0, window, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::ShowFilesMoreMenu, window, cx| {
                this.show_files_more_menu(a.0, window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::CloseFileTabs, window, cx| {
            this.close_file_tabs(&a.0, window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::FocusEditor, window, cx| {
            this.focus_editor(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::FilesCollapseAll, _, cx| {
            this.files_collapse_all(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::FilesRefresh, _, cx| {
            this.files_refresh(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::FilesNewFile, window, cx| {
            this.files_command(super::panels::FilesCommand::NewFile, window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::FilesNewFolder, window, cx| {
                this.files_command(super::panels::FilesCommand::NewFolder, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::FilesRename, window, cx| {
            this.files_command(super::panels::FilesCommand::Rename, window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::FilesDuplicate, window, cx| {
                this.files_command(super::panels::FilesCommand::Duplicate, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::FilesTrash, window, cx| {
            this.files_command(super::panels::FilesCommand::Trash, window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::FilesCopy, window, cx| {
            this.files_command(super::panels::FilesCommand::Copy, window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::FilesCut, window, cx| {
            this.files_command(super::panels::FilesCommand::Cut, window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::FilesPaste, window, cx| {
            this.files_command(super::panels::FilesCommand::Paste, window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::FilesUndo, window, cx| {
            this.files_command(super::panels::FilesCommand::Undo, window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::FilesRedo, window, cx| {
            this.files_command(super::panels::FilesCommand::Redo, window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::FilesFilter, window, cx| {
            this.files_command(super::panels::FilesCommand::Filter, window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::FilesFilterInFolder, window, cx| {
                this.files_command(super::panels::FilesCommand::FilterInFolder, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::FilesCopyPath, window, cx| {
            this.files_row_command(super::files_menu::FilesRowAction::CopyPath, window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::FilesCopyRelativePath, window, cx| {
                this.files_row_command(
                    super::files_menu::FilesRowAction::CopyRelativePath,
                    window,
                    cx,
                );
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::FilesRevealInFinder, window, cx| {
                this.files_row_command(super::files_menu::FilesRowAction::Reveal, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::FilesOpenInTerminal, window, cx| {
                this.files_row_command(
                    super::files_menu::FilesRowAction::OpenInTerminal,
                    window,
                    cx,
                );
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::FilesAttachToChat, window, cx| {
                this.files_row_command(super::files_menu::FilesRowAction::AttachToChat, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::FilesFindInFolder, window, cx| {
                this.files_row_command(super::files_menu::FilesRowAction::FindInFolder, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::FilesSelectForCompare, window, cx| {
                this.files_row_command(
                    super::files_menu::FilesRowAction::SelectForCompare,
                    window,
                    cx,
                );
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::FilesCompareWithSelected, window, cx| {
                this.files_row_command(
                    super::files_menu::FilesRowAction::CompareWithSelected,
                    window,
                    cx,
                );
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::FilesCompareWithHead, window, cx| {
                this.files_row_command(
                    super::files_menu::FilesRowAction::CompareWithHead,
                    window,
                    cx,
                );
            }),
        )
        .on_action(cx.listener(|this, a: &actions::FilesAskTrash, window, cx| {
            this.files_ask_trash(&a.0, window, cx);
        }))
        .on_action(cx.listener(|this, a: &actions::FilesAskMove, window, cx| {
            this.files_ask_move(&a.0, window, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::FilesTabsMoved, window, cx| {
                this.files_tabs_moved(&a.0, window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::FilesTabsTrashed, _, cx| {
            this.files_tabs_trashed(&a.0, cx);
        }))
        .on_action(cx.listener(|this, a: &actions::FilesTabsRestored, _, cx| {
            this.files_tabs_restored(&a.0, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::OpenChosenFile, window, cx| {
                let target = super::files::FileOpen {
                    path: a.0.path.clone().into(),
                    line: a.0.line,
                    column: a.0.column,
                    picked: true,
                };
                this.open_file_placed(target, a.0.place, window, cx);
            }),
        )
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
        .on_action(cx.listener(|this, _: &actions::NextContainer, window, cx| {
            this.cycle_container(1, window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::PreviousContainer, window, cx| {
                this.cycle_container(-1, window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::OpenCheckout, window, cx| {
            if let Ok(project) = a.0.project.parse() {
                this.open_checkout(project, std::path::PathBuf::from(&a.0.path), window, cx);
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
            cx.listener(|this, a: &actions::ShowAssistantMoreMenu, window, cx| {
                this.show_assistant_more_menu(a.0, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowMemoryMenu, window, cx| {
                this.show_memory_menu(a.0, window, cx);
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

    /// The title bar row over the center: the two dock recovery buttons and the title of the
    /// active item, nothing else (CL-CHROME). Layout presets live in View → Layout Preset, the
    /// palette and ⌘⇧1…9; the Agents overview in View, the palette and ⌘⇧O.
    fn render_titlebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let left_extent = self.dock_extent(DockPosition::Left);
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
                    .child(SharedString::from(
                        self.window_label(cx).unwrap_or_default(),
                    )),
            )
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
                            let entries = this.panel_menu(&menu_panel, event.position);
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
        let projects_hidden = pos == DockPosition::Left
            && dock
                .active
                .as_ref()
                .is_some_and(|a| a.as_str() == "sessions")
            && !self
                .settings
                .get::<crate::settings::SidebarSettings>()
                .show_projects;
        let body: AnyElement = match dock
            .active
            .as_ref()
            .filter(|_| !projects_hidden)
            .and_then(|a| self.panel_view(a, window, cx))
        {
            Some(view) => cached_view(&self.regions, view),
            None if projects_hidden => div()
                .size_full()
                .p(px(tokens.spacing * 3.0))
                .text_color(theme.muted)
                .child("The projects list is hidden. Right click the rows above to show it.")
                .into_any_element(),
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

        let bottom_menu = (pos == DockPosition::Bottom)
            .then(|| dock.active.clone())
            .flatten()
            .map(|panel| self.panel_menu_button(panel, "bottom", cx));
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
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                let entries = this.dock_checklist(pos, event.position);
                                this.open_menu(event.position, entries, window, cx);
                            }),
                        )
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
                .children(bottom_menu)
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

    /// The peek (EX-2): the panel of a side dock whose body would leave the center under 480 pt,
    /// floating over the center's edge beside the dock's rail — a transparent layer that closes
    /// it on a click outside (and keeps that click from reaching what is under it), then the
    /// panel itself with its header, below the title bar so the titlebar buttons stay usable.
    /// The center never moves.
    fn render_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(pos) = self.peek_dock() else {
            return Vec::new();
        };
        let dock = self.state.docks.get(pos).clone();
        let Some(active) = dock.active.clone() else {
            return Vec::new();
        };
        let theme = self.theme;
        let body: AnyElement = match self.panel_view(&active, window, cx) {
            Some(view) => cached_view(&self.regions, view),
            None => div().size_full().into_any_element(),
        };
        let header = self.right_header(&dock, cx);
        let rail = self.dock_extent(pos);
        let beside_left = self.dock_extent(DockPosition::Left);
        let beside_right = self.dock_extent(DockPosition::Right);
        let backdrop = div()
            .id("peek-backdrop")
            .absolute()
            .top(px(TITLEBAR_HEIGHT))
            .bottom_0()
            .left(px(beside_left))
            .right(px(beside_right))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.close_peek(window, cx);
                    cx.stop_propagation();
                }),
            )
            .into_any_element();
        let mut layer = div()
            .id("peek")
            .track_focus(&self.dock_focus_handle(pos))
            .absolute()
            .top(px(TITLEBAR_HEIGHT))
            .bottom_0()
            .w(px(self.peek_width(pos)))
            .flex()
            .flex_col()
            .bg(match pos {
                DockPosition::Left => theme.sidebar,
                _ => theme.right_sidebar,
            })
            .border_color(theme.border)
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.mutate(cx, |s| s.focus_dock(pos));
                }),
            )
            .child(header)
            .child(
                div()
                    .id("peek-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(body),
            )
            .child(probe(&self.regions, Region::Peek));
        layer = match pos {
            DockPosition::Left => layer.left(px(rail)).border_r_1(),
            _ => layer.right(px(rail)).border_l_1(),
        };
        layer.style().box_shadow = Some(theme.shadow());
        vec![backdrop, layer.into_any_element()]
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
        let group = SharedString::from(format!("dock-resize-{pos:?}"));
        let base = div()
            .id(id(format!("dock-resize-{pos:?}")))
            .absolute()
            .occlude()
            .group(group.clone())
            .child(divider_line(
                &group,
                !matches!(pos, DockPosition::Bottom),
                accent,
            ))
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
                    .w(px(HANDLE_THICKNESS + LEFT_HANDLE_OUTSET))
                    .child(probe(&self.regions, Region::Toolbar("left-dock-handle")))
                    .cursor_col_resize(),
                DockPosition::Right => base
                    .right(px(right - HANDLE_THICKNESS))
                    .top_0()
                    .bottom(px(status_h))
                    .w(px(HANDLE_THICKNESS))
                    .child(probe(&self.regions, Region::Toolbar("right-dock-handle")))
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
                    // One hairline between two panes, owned by the second (CL-CHROME): the
                    // boundary, never a focus outline.
                    let border = self.theme.border;
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
                            .when(index > 0 && horizontal, |s| {
                                s.border_l_1().border_color(border)
                            })
                            .when(index > 0 && !horizontal, |s| {
                                s.border_t_1().border_color(border)
                            })
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
                    let group = SharedString::from(format!("splitter-{path:?}-{divider}"));
                    let handle = div()
                        .id(id(format!("splitter-{path:?}-{divider}")))
                        .absolute()
                        .group(group.clone())
                        .child(divider_line(&group, horizontal, accent))
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
        let focused = self.state.focus == FocusTarget::Pane(pane_id);
        let dragging = cx.has_active_drag();

        // Every activation (and every new tab) brings the active tab into view; a strip the
        // user scrolled by hand stays where it is until then.
        let tab_scroll = self.tab_scrolls.entry(pane_id).or_default().clone();
        let reveal = (pane.active, pane.items.len());
        if self.tab_revealed.get(&pane_id) != Some(&reveal) {
            self.tab_revealed.insert(pane_id, reveal);
            tab_scroll.scroll_to_item(pane.active);
        }
        // A tab that just got an attention mark comes into view whole: at the edge of a
        // scrolled strip its dot or warning was cut off.
        let marked: Vec<usize> = pane
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| self.tab_has_mark(item, cx))
            .map(|(index, _)| index)
            .collect();
        let before = self.tab_marked.insert(pane_id, marked.clone());
        if let Some(newest) = marked
            .iter()
            .rev()
            .find(|index| before.as_ref().is_some_and(|b| !b.contains(index)))
        {
            tab_scroll.scroll_to_item(*newest);
        }
        let mut tabs: Vec<AnyElement> = pane
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                self.render_tab(pane_id, index, item, index == pane.active, focused, cx)
            })
            .collect();
        if let Some((index, ghost)) = self.closing_tab_ghost(pane_id, cx) {
            tabs.insert(index.min(tabs.len()), ghost);
        }

        // The page the pane shows (Mawas Home, the Agents page, a panel opened as a tab) has
        // the panel menu in its strip.
        let center_menu = pane
            .active_item()
            .and_then(|item| item.as_panel().cloned())
            .map(|panel| self.panel_menu_button(panel, "center", cx));

        // The strip is chrome (the `sidebar` tone, like the title bar above it); its hairline is
        // drawn first and the active tab, a `bg` fill reaching the strip's bottom, covers it: the
        // tab is connected to its pane (CL-CHROME).
        let strip = div()
            .relative()
            .flex()
            .flex_none()
            .items_end()
            .h(px(tokens.tab_height))
            .w_full()
            .px(px(4.0))
            .gap(px(2.0))
            .overflow_hidden()
            .bg(theme.sidebar)
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(1.0))
                    .bg(theme.border),
            )
            .child(
                div()
                    .id(id(format!("pane-strip-scroll-{pane_id}")))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_end()
                    .h_full()
                    .gap(px(2.0))
                    .overflow_x_scroll()
                    .track_scroll(&tab_scroll)
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
                                            this.split_pane_with_terminal(
                                                pane_id,
                                                Edge::Right,
                                                window,
                                                cx,
                                            );
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
                    ),
            )
            .children(
                center_menu
                    .map(|menu| div().flex_none().h_full().flex().items_center().child(menu)),
            );

        let content = match pane.active_item() {
            Some(item) => self.render_item(pane_id, item, window, cx),
            None => {
                let hint = empty_centre_hint(
                    &key_hint(window, &actions::QuickOpen).unwrap_or_else(|| "⌘P".into()),
                    &key_hint(window, &actions::NewTerminal).unwrap_or_else(|| "⌘T".into()),
                );
                ui::EmptyState::new("Nothing is open here.")
                    .mascot(Mascot::Empty)
                    .action(self.empty_start_button(&theme, &tokens))
                    .hint(hint)
                    .render(&theme, &tokens)
                    .relative()
                    .child(probe(&self.regions, Region::Launcher("empty-centre")))
                    .into_any_element()
            }
        };

        let leaf_zones = matches!(pane.active_item(), Some(ItemRef::TerminalSplit { .. }));
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
            // A tab of splits has edge zones on each of its splits instead.
            .filter(|(name, ..)| !leaf_zones || *name == "center")
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
                        this.drop_on_pane_zone(d, target.clone(), None, window, cx);
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
            .into_any_element()
    }

    /// Whether the tab of `item` wears an attention mark: a request called for one of its
    /// sessions, or one of them needs attention (the dot, or the agent badge's warning).
    fn tab_has_mark(&self, item: &ItemRef, cx: &Context<Self>) -> bool {
        let store = self.sessions.read(cx);
        item.sessions()
            .into_iter()
            .filter_map(|s| s.parse::<hutan_core::SessionId>().ok())
            .any(|id| {
                store.is_called(id)
                    || store
                        .session(id)
                        .is_some_and(|info| store.presentation(info).attention.is_some())
            })
    }

    /// Whether a session of the tab has a request waiting (an agent blocked, an approval).
    fn waits_on_tab(&self, sessions: &[hutan_core::SessionId], cx: &Context<Self>) -> bool {
        let store = self.sessions.read(cx);
        sessions.iter().any(|id| {
            store
                .session(*id)
                .is_some_and(|info| store.presentation(info).is_request())
        })
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
        let hover = theme.sidebar_hover;
        let look = TabLook::of(&theme, active, pane_focused);
        let (icon, title) = self.item_icon_and_title(item, cx);
        let tab_tip = self
            .item_tooltip(item, cx)
            .map(SharedString::from)
            .or_else(|| (title.chars().count() > TAB_TITLE_TOOLTIP_CHARS).then(|| title.clone()));
        // The tab's sessions (every split of a tab of splits): what their work and their
        // terminals say shows on the tab itself, from the unread state the window keeps.
        let tab_sessions: Vec<hutan_core::SessionId> = item
            .sessions()
            .into_iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        let called = tab_sessions
            .iter()
            .any(|id| self.sessions.read(cx).is_called(*id));
        let marks = {
            let store = self.sessions.read(cx);
            tab_sessions
                .iter()
                .filter_map(|id| {
                    let info = store.session(*id)?;
                    Some(crate::sessions::tab_marks(
                        info,
                        &store.presentation(info),
                        store.unread(*id),
                    ))
                })
                .fold(crate::sessions::TabMarks::default(), |all, one| {
                    crate::sessions::TabMarks {
                        state: all.state.or(one.state),
                        bell: all.bell || one.bell,
                    }
                })
        };
        let bell = marks.bell || called;
        let waits = self.waits_on_tab(&tab_sessions, cx);
        let agent_badge = session_of_item(item).map(|id| {
            let mut badge = self.sessions.read(cx).agent_badge(id);
            if bell {
                // The bell already says it waits; the badge's ▲ does not say it twice.
                badge.blocked.clear();
            }
            (id, badge)
        });
        // One attention mark per tab: the bell, else the dot (CL-CHROME).
        let badge = agent_badge.and_then(|(id, badge)| {
            super::panels::agent_badge(
                &theme,
                &badge,
                super::view::BadgeSite::Tab,
                id,
                &self.regions,
                cx,
            )
        });
        // The attention of any split of a tab of splits shows on the tab.
        let attention = tab_sessions
            .iter()
            .find_map(|id| {
                let store = self.sessions.read(cx);
                store
                    .session(*id)
                    .map(|info| store.presentation(info))
                    .filter(|p| p.attention.is_some())
            })
            .filter(|_| !bell && marks.state != Some(crate::sessions::TabState::Failed));
        let state_mark = marks.state.map(|state| {
            use crate::sessions::{TabState, Tone};
            let (glyph, tone, tip) = match state {
                TabState::Working => (None, Tone::Accent, "Working"),
                TabState::Done => (
                    Some(crate::icons::Icon::StateCompleted),
                    Tone::Success,
                    "Done: nobody has looked yet",
                ),
                TabState::Failed => (Some(crate::icons::Icon::StateFailed), Tone::Error, "Failed"),
            };
            let color = super::panels::tone_color(&theme, tone);
            let mark = div()
                .id(id(format!("tab-state-{pane_id}-{index}")))
                .relative()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(px(14.0))
                .text_color(color)
                .tooltip(move |_, cx| {
                    cx.new(|_| super::style::TextTooltip::new(tip.into()))
                        .into()
                })
                .child(probe(&self.regions, Region::TabState(pane_id, index)));
            match glyph {
                Some(glyph) => mark.child(crate::icons::glyph(glyph)),
                None => mark.child(super::ui::spinner_at(
                    id(format!("tab-spinner-{pane_id}-{index}")),
                    12.0,
                    color,
                    TAB_SPINNER_FPS,
                )),
            }
        });
        // A fork nobody renamed: its name and the mark that sets it apart from its parent.
        let (name, mark) = match session_of_item(item) {
            Some(session) => {
                let store = self.sessions.read(cx);
                match store.session(session) {
                    Some(info) => {
                        let parts = store.title_parts(info);
                        (SharedString::from(parts.head), parts.tail)
                    }
                    None => (title.clone(), None),
                }
            }
            None => (title.clone(), None),
        };
        let split_badge = self.split_count_badge(pane_id, index, item);
        let preview = item.as_file().is_some_and(|path| self.is_preview(path));
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
            .h(px(tokens.tab_height - TAB_TOP_INSET))
            .rounded_t(px(tokens.radius))
            .cursor_pointer()
            .text_color(look.text)
            .font_weight(look.weight)
            .when_some(look.fill, |s, fill| s.bg(fill))
            .when(look.fill.is_none(), |s| s.hover(move |s| s.bg(hover)))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.try_mutate(cx, |s| {
                    s.set_active_item(pane_id, index)?;
                    s.focus_pane(pane_id)
                });
                if event.click_count() == 2 {
                    // A double click on a preview tab keeps it; on any other it zooms the pane.
                    let preview = this
                        .state
                        .pane(pane_id)
                        .and_then(|p| p.items.get(index))
                        .and_then(|item| item.as_file())
                        .filter(|path| this.is_preview(path))
                        .map(str::to_owned);
                    match preview {
                        Some(path) => this.keep_open(&path, cx),
                        None => this.mutate(cx, |s| s.toggle_zoom()),
                    }
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
                tab.tooltip(move |_, cx| {
                    cx.new(|_| super::style::TextTooltip::new(tip.clone()))
                        .into()
                })
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
            .child(
                div()
                    .max_w(px(TAB_TITLE_MAX))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing / 2.0))
                    .when(preview, |s| s.italic())
                    // The name gives way with "…"; a fork's mark (`⑂ 2`) never does.
                    .child(
                        div()
                            .id(id(format!("tab-name-{pane_id}-{index}")))
                            .relative()
                            .min_w_0()
                            .truncate()
                            .child(name)
                            .child(probe(&self.regions, Region::TabName(pane_id, index))),
                    )
                    .children(mark.map(|tail| {
                        super::panels::common::title_mark(
                            id(format!("tab-mark-{pane_id}-{index}")),
                            &tail,
                            theme.muted,
                            tokens.secondary_font_size,
                        )
                        .relative()
                        .child(probe(&self.regions, Region::TabMark(pane_id, index)))
                    })),
            )
            .children(split_badge)
            .children(badge)
            .children(state_mark)
            // A bell: an agent waits for an answer (or asked for you while the tab was out of
            // sight), or the terminal rang or sent a notification while it was. It goes when
            // the tab is shown or the question answered.
            .children(bell.then(|| {
                let (tone, tip) = if called {
                    (theme.warning, "Asked for you")
                } else if waits {
                    (theme.warning, "Waiting for you")
                } else {
                    (theme.accent, "The terminal rang")
                };
                div()
                    .id(id(format!("tab-bell-{pane_id}-{index}")))
                    .relative()
                    .flex_none()
                    .text_color(tone)
                    .tooltip(move |_, cx| {
                        cx.new(|_| super::style::TextTooltip::new(tip.into()))
                            .into()
                    })
                    .child(crate::icons::glyph(crate::icons::Icon::StateBell))
                    .child(probe(&self.regions, Region::TabBell(pane_id, index)))
                    .when(called, |s| {
                        s.child(probe(&self.regions, Region::TabCall(pane_id, index)))
                    })
            }))
            .children(attention.map(|p| {
                div()
                    .relative()
                    .flex_none()
                    .size(px(7.0))
                    .rounded_full()
                    .bg(super::panels::tone_color(&theme, p.tone))
                    .child(probe(&self.regions, Region::TabAttention(pane_id, index)))
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
                                cx.new(|_| super::style::TextTooltip::new("Unsaved changes".into()))
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
                        // `terminal.tab_close = close_session`: the × closes the session too
                        // (every split's, in a tab of splits).
                        let sessions: Vec<hutan_core::SessionId> = this
                            .state
                            .pane(pane_id)
                            .and_then(|p| p.items.get(index))
                            .map(|item| {
                                item.sessions()
                                    .into_iter()
                                    .filter_map(|s| s.parse().ok())
                                    .collect()
                            })
                            .unwrap_or_default();
                        if !sessions.is_empty()
                            && this.terminal_settings.tab_close
                                == crate::settings::TabClose::CloseSession
                            && this.can_close_sessions(cx)
                        {
                            for session in sessions {
                                this.close_session(session, window, cx);
                            }
                        } else {
                            this.close_tab_with_fade(pane_id, index, window, cx);
                        }
                        cx.stop_propagation();
                    }))
                    .child(crate::icons::glyph(crate::icons::Icon::ActionClose)),
            )
            .child(probe(&self.regions, Region::Tab(pane_id, index)))
            .into_any_element()
    }

    /// How the tab at `index` of `pane` is drawn now ([`TabLook`]); `None` when there is no such
    /// tab.
    pub fn tab_look(&self, pane: PaneId, index: usize) -> Option<TabLook> {
        let found = self.state.pane(pane)?;
        (index < found.items.len()).then(|| {
            TabLook::of(
                &self.theme,
                index == found.active,
                self.state.focus == FocusTarget::Pane(pane),
            )
        })
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
            ItemRef::Placeholder { title } => match self.render_welcome(pane, cx) {
                Some(welcome) => welcome,
                None => ui::empty_state(
                    Mascot::Empty,
                    format!(
                        "{title}. Start an agent here, open a session from the sessions panel, or press ⌘T for a new terminal."
                    ),
                    Some(self.empty_start_button(&theme, &tokens)),
                    &theme,
                    &tokens,
                )
                .into_any_element(),
            },
            ItemRef::Terminal { session } => self.render_terminal_item(pane, session, window, cx),
            ItemRef::TerminalSplit { split } => {
                self.render_terminal_split(pane, split, window, cx)
            }
            ItemRef::Chat { session } => self.render_chat_item(pane, session, cx),
            ItemRef::Changes { path } => {
                let view = self.changes_view(path, window, cx);
                cached_view(&self.regions, view)
            }
            ItemRef::File { path } => self.render_file_item(pane, path, window, cx),
            ItemRef::History { path } => self.render_history_item(path, window, cx),
            ItemRef::Revision { path, rev } => self.render_revision_item(path, rev, cx),
            item @ (ItemRef::Compare { .. } | ItemRef::CompareHead { .. }) => {
                self.render_compare_item(item, cx)
            }
        }
    }

    /// The "▶ Start…" button of an empty pane or project: the Start… sheet in the project in
    /// focus.
    fn empty_start_button(
        &self,
        theme: &crate::theme::Theme,
        tokens: &crate::theme::Tokens,
    ) -> AnyElement {
        ui::Button::new("pane-empty-start", "Start…", theme, tokens)
            .primary()
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

    /// What the status items show right now.
    pub fn status_data(&self, cx: &gpui::App) -> StatusData {
        let store = self.sessions.read(cx);
        StatusData {
            connection: self.connection_chip(cx),
            sessions: store.sessions().len(),
            workspace: self.workspace_label(cx),
            // `status.workspace.show_branch`.
            branch: self.workspace_branch(cx).filter(|_| {
                self.settings
                    .get::<crate::settings::StatusSettings>()
                    .workspace
                    .show_branch
            }),
            requests: store.requests().len(),
            // The agents at work as of the last time the daemon said so: not while it is gone.
            working: if self.daemon_connected(cx) {
                store
                    .agent_presentation()
                    .counts(crate::sessions::Scope::All)
                    .working
            } else {
                0
            },
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
            usage: self.env.usage.read(cx).status(),
            meters: self.env.usage.read(cx).meters(),
        }
    }

    fn render_status_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        self.keep_daemon_status_fresh(cx);
        self.follow_status_branch(cx);
        let data = self.status_data(cx);
        let hint = |action: &dyn Action| key_hint(window, action).unwrap_or_default();
        let workbench = cx.entity();
        let editor_items = self
            .editor_items
            .get_or_insert_with(|| super::editor_status::EditorItems::new(&workbench, cx))
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
            width: BarWidth::of(f32::from(window.viewport_size().width)),
            editor_items: &editor_items,
            height: tokens.status_bar,
            regions: &self.regions,
        };
        let status_bar = self.status_bar_drawn();
        let render_side =
            |side: StatusSide, ids: &[String], cx: &mut Context<Self>| -> Vec<AnyElement> {
                ids.iter()
                    .filter_map(|item| self.status_items.get(item))
                    .map(|spec| self.status_item_slot(spec.id, side, (spec.render)(&ctx), cx))
                    .collect()
            };
        let left = render_side(StatusSide::Left, &status_bar.left, cx);
        let right = render_side(StatusSide::Right, &status_bar.right, cx);
        let whats_new = self.render_whats_new(cx);
        let left_group = self.status_group(
            StatusSide::Left,
            div()
                .id(super::customize_bar::group_id(StatusSide::Left))
                // However long a notice is, it gives way to ellipsis: the right side of the
                // bar (⌘⇧P, the editor's position) never leaves the window.
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 3.0))
                .children(left)
                .children(whats_new)
                .children(
                    self.notice
                        .clone()
                        // A notice is words, not an action: `fg`, never the accent.
                        .map(|n| div().min_w_0().truncate().text_color(theme.fg).child(n)),
                ),
            cx,
        );
        let right_group = self.status_group(
            StatusSide::Right,
            div()
                .id(super::customize_bar::group_id(StatusSide::Right))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .children(right),
            cx,
        );
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
            // Right click on the bar itself: the checklist of its items.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    let entries = this.status_checklist(event.position);
                    this.open_menu(event.position, entries, window, cx);
                }),
            )
            .child(left_group)
            .child(right_group)
            .child(probe(&self.regions, Region::StatusBar))
            .into_any_element()
    }

    // ---- palette -----------------------------------------------------------------------------

    fn render_palette(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.palette.is_some() {
            self.exits.palette.cancel();
            return self.build_palette(true, window, cx);
        }
        let now = cx.background_executor().now();
        motion::while_leaving(
            self,
            now,
            |this| &mut this.palette,
            |this| &mut this.exits.palette,
            |this| this.build_palette(false, window, cx),
        )
        .flatten()
    }

    /// The palette at its current state; `wanted` is false while it leaves (inert, the field
    /// drawn as plain text).
    fn build_palette(
        &mut self,
        wanted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let palette = self.palette.clone()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        // As many rows as the window holds: the panel sits 80 px down, has 6 px of padding and a
        // header, and leaves a margin at the bottom; the rest scrolls.
        let row_height = tokens.sidebar_row + 2.0;
        let header_height = tokens.sidebar_row + 8.0 + 4.0;
        let list_max = (window.viewport_size().height.as_f32()
            - PALETTE_TOP
            - 12.0
            - header_height
            - PALETTE_BOTTOM_MARGIN)
            .max(row_height);
        let count = palette.matches.len();
        let empty = palette.matches.is_empty();

        Some(
            div()
                .id("palette-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(PALETTE_TOP))
                .when(wanted, |s| s.shield_behind())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
                )
                .child(
                    div()
                        .id("palette")
                        .key_context(super::vim_ui::vim_key_context(
                            "Palette",
                            self.overlay_vim_mode(super::vim_ui::OverlayField::Palette),
                        ))
                        .when(wanted, |s| s.track_focus(&self.palette_focus))
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
                                .children(if wanted {
                                    self.overlay_parts(OverlayField::Palette, "", cx)
                                } else {
                                    vec![div().child(palette.query.clone()).into_any_element()]
                                }),
                        )
                        .when(count > 0, |s| {
                            s.child(
                                div()
                                    .relative()
                                    .w_full()
                                    .child(
                                        uniform_list(
                                            "palette-list",
                                            count,
                                            cx.processor(
                                                |this, range: Range<usize>, window, cx| {
                                                    this.palette_rows(range, window, cx)
                                                },
                                            ),
                                        )
                                        .track_scroll(&self.palette_scroll)
                                        .with_sizing_behavior(gpui::ListSizingBehavior::Infer)
                                        .w_full()
                                        .max_h(px(list_max)),
                                    )
                                    .child(self.palette_bar.element())
                                    .child(probe(&self.regions, Region::Palette("list"))),
                            )
                        })
                        .when(empty, |s| {
                            s.child(
                                div()
                                    .px(px(tokens.spacing * 2.0))
                                    .text_color(theme.muted)
                                    .child("No matching commands"),
                            )
                        })
                        .presented(
                            "palette-motion",
                            motion::Kind::Popover,
                            wanted,
                            motion::Slide::Drop,
                        )
                        .inert(!wanted),
                )
                .into_any_element(),
        )
    }

    /// The rows `range` of the palette's list (a frame builds only the ones in view).
    fn palette_rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let Some(palette) = &self.palette else {
            return Vec::new();
        };
        let selected_row = palette.selected;
        range
            .filter_map(|row| {
                let command = self.commands.get(*palette.matches.get(row)?)?;
                let selected = row == selected_row;
                let hint = key_hint(window, &*command.hint).unwrap_or_default();
                let hover = theme.surface_hover;
                Some(
                    div()
                        .id(id(format!("palette-row-{row}")))
                        .flex()
                        .items_center()
                        .justify_between()
                        .w_full()
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
                        .child(probe(&self.regions, Region::PaletteRow(row)))
                        .into_any_element(),
                )
            })
            .collect()
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
                let separator = entry.separator;
                let tip = entry.tip.clone();
                let row = div()
                    .id(id(format!("menu-row-{i}")))
                    .relative()
                    .when_some(tip, |s, tip| {
                        s.tooltip(move |_, cx| {
                            cx.new(|_| crate::workbench::style::TextTooltip::new(tip.clone()))
                                .into()
                        })
                    })
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
                    // A check mark you can see: the accent, bold, in a column of its own.
                    .child(
                        div()
                            .w(px(14.0))
                            .flex_none()
                            .text_color(theme.accent)
                            .font_weight(gpui::FontWeight::BOLD)
                            .when(entry.checked, |s| s.child("✓")),
                    )
                    .child(div().flex_1().child(entry.label))
                    // The key that does the same, muted, at the right.
                    .children(entry.hint.map(|hint| {
                        div()
                            .flex_none()
                            .pl(px(tokens.spacing * 4.0))
                            .text_color(theme.faint)
                            .text_size(px(tokens.secondary_font_size))
                            .child(hint)
                    }))
                    .when(entry.submenu, |s| {
                        s.child(crate::icons::glyph(crate::icons::Icon::ActionDisclose))
                    })
                    .child(probe(&self.regions, Region::MenuEntry(i)));
                // A group starts under a line.
                div()
                    .flex()
                    .flex_col()
                    .when(separator, |s| {
                        s.child(
                            div()
                                .h(px(1.0))
                                .mx(px(tokens.spacing * 2.0))
                                .my(px(tokens.spacing * 0.5))
                                .bg(theme.border),
                        )
                    })
                    .child(row)
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
                            .shield_behind()
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

#[cfg(test)]
mod empty_centre_tests {
    #[test]
    fn the_hint_names_quick_open_and_new_terminal() {
        assert_eq!(
            super::empty_centre_hint("⌘P", "⌘T"),
            "⌘P opens a file or a session · ⌘T a new terminal"
        );
    }
}

#[cfg(test)]
mod divider_tests {
    use super::{HANDLE_LINE, HANDLE_THICKNESS};

    // FX-UI-B: a hovered divider draws a hairline of 1–2 px inside its wider hit area, not
    // the 6 px tinted bar the hit area is.
    const _: () = assert!(HANDLE_LINE >= 1.0);
    const _: () = assert!(HANDLE_LINE <= 2.0);
    const _: () = assert!(HANDLE_THICKNESS >= 3.0 * HANDLE_LINE);
}
