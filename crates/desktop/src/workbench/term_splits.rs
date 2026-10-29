//! Terminal splits in the workbench: a tab that holds several terminals (Ghostty / iTerm style).
//!
//! The model is [`TermSplit`] (`model/term_split.rs`); every leaf is a daemon session. This file
//! has what the window does with it: the keys (⌘D, ⌘⇧D, ⌘⌥←→↑↓, ⌃⌘←→↑↓, ⌘⇧↩, ⌘W, ⌘⇧W), the
//! context menu, drawing the tree with its dividers, and the drag of a divider.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, Bounds, ClickEvent, Context, ElementId, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, SharedString, StatefulInteractiveElement,
    Styled, Window, canvas, div, prelude::*, px, relative,
};
use hutan_core::SessionId;

use crate::theme::DensityExt as _;

use super::model::{
    Direction, DropTarget, Edge, FocusTarget, ItemRef, ItemSource, MIN_RATIO, MIN_SPLIT_COLS,
    MIN_SPLIT_ROWS, PaneId, PaneTree, TermSplit,
};
use super::render::probe;
use super::view::{DraggedItem, MenuEntry, Region, Resizing, Workbench, steps};

/// Width of the strip around a divider that takes the mouse.
const HANDLE_THICKNESS: f32 = 6.0;
/// The focused split's top edge (accent, low opacity), in points.
const FOCUS_EDGE: f32 = 2.0;
/// How much of the text the unfocused splits lose (`terminal.dim_unfocused_splits`).
const DIM: f32 = 0.08;

fn element_id(text: impl Into<SharedString>) -> ElementId {
    ElementId::from(text.into())
}

/// What a tab of splits says about them: `⊞ 3`, and `⊞ 3 · zoomed` while one fills the tab (the
/// count stays: the other splits are hidden, not gone).
fn split_badge_label(split: &TermSplit) -> String {
    if split.zoomed {
        format!("⊞ {} · zoomed", split.len())
    } else {
        format!("⊞ {}", split.len())
    }
}

impl Workbench {
    // ---- queries ----------------------------------------------------------------------------

    /// The session of the focused terminal of the focused pane, when that is a terminal tab
    /// (with or without splits).
    fn focused_terminal_tab_session(&self) -> Option<String> {
        let FocusTarget::Pane(_) = self.state.focus else {
            return None;
        };
        match self.state.active_pane()?.active_item()? {
            item @ (ItemRef::Terminal { .. } | ItemRef::TerminalSplit { .. }) => {
                item.session().map(str::to_owned)
            }
            _ => None,
        }
    }

    /// Whether the focused tab holds several terminals.
    pub fn focused_tab_has_splits(&self) -> bool {
        self.state.active_term_split().is_some()
    }

    /// The smallest share of a split container a divider drag leaves a side: 6 columns or 3
    /// rows of the terminal's text, at the current font size.
    pub(crate) fn term_split_min_share(&self, horizontal: bool, extent: f32) -> f32 {
        let style = &self.terminal_style;
        let pad = style.padding * 2.0;
        let min_px = if horizontal {
            style.font_size * 0.6 * f32::from(MIN_SPLIT_COLS)
        } else {
            style.font_size * style.line_height * f32::from(MIN_SPLIT_ROWS)
        } + pad;
        if extent > 0.0 {
            (min_px / extent).clamp(MIN_RATIO, 0.5)
        } else {
            MIN_RATIO
        }
    }

    // ---- commands ---------------------------------------------------------------------------

    /// ⌘D / ⌘⇧D in a terminal: a new terminal next to the focused one, inside the tab, starting
    /// in the focused terminal's directory. Outside a terminal tab it splits the pane as before.
    pub(crate) fn split_terminal_tab(
        &mut self,
        edge: Edge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.focused_terminal_tab_session() {
            Some(origin) => self.split_terminal_of(&origin, edge, window, cx),
            None => self.split_active(edge, window, cx),
        }
    }

    /// Starts a terminal in the directory of `origin` (a session of an open terminal tab) and
    /// shows it as a split of that tab at `edge` of `origin`.
    pub(crate) fn split_terminal_of(
        &mut self,
        origin: &str,
        edge: Edge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sessions.read(cx).connection_state() != hutan_client::ConnectionState::Connected {
            self.set_notice("Not connected to hutand: cannot start a terminal", cx);
            return;
        }
        let origin_id = origin.parse::<SessionId>().ok();
        let (workspace, cwd) = self.session_split_target(origin_id, cx);
        let spec = self.new_terminal_spec(workspace, cwd, cx);
        let task = self
            .sessions
            .update(cx, |store, cx| store.create_session(workspace, spec, cx));
        let origin = origin.to_owned();
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(info) => {
                this.update_in(cx, |this, window, cx| {
                    this.add_terminal_split(&origin, edge, info.id, window, cx);
                })
                .ok();
            }
            Err(err) => {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot start a terminal: {err}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Puts `session` next to `origin` in the tab that shows it; when that tab went away it
    /// opens `session` as a tab of its own.
    fn add_terminal_split(
        &mut self,
        origin: &str,
        edge: Edge,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane, index)) = self.state.find_session_tab(origin) else {
            self.open_session(session, window, cx);
            return;
        };
        let leaf = match self.state.pane(pane).and_then(|p| p.items.get(index)) {
            Some(ItemRef::TerminalSplit { split }) => split.leaf_of(origin),
            _ => None,
        };
        let id = session.to_string();
        self.try_mutate(cx, |s| s.split_terminal(pane, index, leaf, edge, &id));
        self.focus_root(window, cx);
    }

    /// ⌘⌥←→↑↓ in a terminal: the neighbouring split; at the edge of the tab's splits the
    /// neighbouring pane (or dock), as everywhere else.
    pub(crate) fn focus_split_or_neighbor(
        &mut self,
        dir: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mutate(cx, |s| {
            if !s.focus_term_neighbor(dir) {
                s.focus_neighbor(dir);
            }
        });
        self.focus_root(window, cx);
    }

    /// ⌃⌘←→↑↓ in a terminal: moves the divider of the focused split; a tab without splits
    /// resizes its pane.
    pub(crate) fn resize_split_or_pane(&mut self, dir: Direction, cx: &mut Context<Self>) {
        let (dock_step, split_step) = steps();
        self.mutate(cx, |s| {
            if !s.resize_term_split(dir, split_step) {
                s.resize_focused(dir, dock_step, split_step);
            }
        });
    }

    /// ⌘⇧↩ in a terminal: the focused split fills the tab, or goes back. A tab without splits
    /// zooms its pane.
    pub(crate) fn toggle_split_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mutate(cx, |s| {
            if !s.toggle_term_zoom() {
                s.toggle_zoom();
            }
        });
        self.focus_root(window, cx);
    }

    /// Every split of the focused tab gets an equal share.
    pub(crate) fn equalize_splits(&mut self, cx: &mut Context<Self>) {
        let Some((pane, ..)) = self.state.active_term_split() else {
            return;
        };
        self.mutate(cx, |s| {
            let _ = s.equalize_term_split(pane, &[]);
            // Nested splits too.
            if let Some(ItemRef::TerminalSplit { split }) = s
                .center
                .pane_mut(pane)
                .and_then(|p| p.items.get_mut(p.active))
            {
                split.tree.equalize();
            }
        });
    }

    /// Gives the keyboard to the split of `session` (a click on it, a session opened from the
    /// sidebar).
    pub fn focus_split(&mut self, session: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        let id = session.to_string();
        let Some((pane, index)) = self.state.find_session_tab(&id) else {
            return;
        };
        let unchanged = self.state.focus == FocusTarget::Pane(pane)
            && self
                .state
                .pane(pane)
                .is_some_and(|p| p.active == index && p.items[index].session() == Some(&id));
        if !unchanged {
            self.try_mutate(cx, |s| {
                s.set_active_item(pane, index)?;
                let _ = s.focus_term_split(pane, index, &id);
                s.focus_pane(pane)
            });
        }
        self.focus_root(window, cx);
    }

    /// ⌘W in a terminal with splits: closes the focused split. The session ends under the rules
    /// of every close (it asks when something runs; the Undo toast brings it back); a daemon
    /// that cannot close sessions only takes the split out of the tab.
    pub(crate) fn close_focused_split(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((_, _, split)) = self.state.active_term_split() else {
            return false;
        };
        let Some(session) = split.focused_session().map(str::to_owned) else {
            return false;
        };
        self.close_split_of(&session, window, cx);
        true
    }

    /// Closes the split of `session` (see [`close_focused_split`](Self::close_focused_split)).
    pub(crate) fn close_split_of(
        &mut self,
        session: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(id) = session.parse::<SessionId>() else {
            return;
        };
        if self.can_close_sessions(cx) {
            self.close_session(id, window, cx);
        } else {
            self.close_session_tabs(id, cx);
            self.focus_root(window, cx);
        }
    }

    /// ⌘⇧W in a terminal: with splits, closes the whole tab (what the × does: detach, or close
    /// every session when `terminal.tab_close` says so); a single terminal is stopped, as ⌘⇧W
    /// always did.
    pub(crate) fn close_terminal_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((pane, index, split)) = self.state.active_term_split() else {
            if let Some(session) = self.focused_session() {
                self.stop_session(session, window, cx);
            }
            return;
        };
        let sessions: Vec<SessionId> = split
            .sessions()
            .into_iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        if self.terminal_settings.tab_close == crate::settings::TabClose::CloseSession
            && self.can_close_sessions(cx)
        {
            for session in sessions {
                self.close_session(session, window, cx);
            }
        } else {
            self.close_tab_with_fade(pane, index, window, cx);
        }
    }

    /// *Move Split to New Tab*: the split of `session` becomes a tab of its own.
    pub(crate) fn move_split_to_new_tab(
        &mut self,
        session: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.try_mutate(cx, |s| s.move_split_to_tab(session));
        self.focus_root(window, cx);
    }

    /// A tab dropped on an edge zone of a pane (`leaf`: of one of the splits of its tab of
    /// splits). A terminal tab dropped on the edge of another terminal merges into it as a
    /// split; ⌥ keeps the pane split of before, as does any other kind of tab.
    pub(crate) fn drop_on_pane_zone(
        &mut self,
        dragged: &DraggedItem,
        target: DropTarget,
        leaf: Option<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let DropTarget::Split { pane, edge } = &target
            && let ItemSource::Pane {
                pane: from_pane,
                index: from_index,
            } = dragged.source
            && matches!(dragged.item, ItemRef::Terminal { .. })
            && !window.modifiers().alt
            && let Some(index) = self.state.pane(*pane).and_then(|p| {
                matches!(
                    p.active_item(),
                    Some(ItemRef::Terminal { .. } | ItemRef::TerminalSplit { .. })
                )
                .then_some(p.active)
            })
            && self
                .try_mutate(cx, |s| {
                    s.merge_terminal_tab((from_pane, from_index), (*pane, index), leaf, *edge)
                })
                .is_some()
        {
            self.focus_root(window, cx);
            return;
        }
        self.drop_item(dragged, target, window, cx);
    }

    // ---- context menu -----------------------------------------------------------------------

    /// The context menu of a terminal: *Split Right*, *Split Down*, and for a split of a tab of
    /// splits *Move Split to New Tab* and *Close Split*.
    pub(crate) fn terminal_menu(&self, session: &str) -> Vec<MenuEntry> {
        let in_splits = self
            .state
            .find_session_tab(session)
            .and_then(|(pane, index)| self.state.pane(pane)?.items.get(index))
            .is_some_and(|item| matches!(item, ItemRef::TerminalSplit { .. }));
        let right = session.to_owned();
        let down = session.to_owned();
        let mut entries = vec![
            Self::entry("Split Right", move |this, window, cx| {
                this.split_terminal_of(&right, Edge::Right, window, cx);
            }),
            Self::entry("Split Down", move |this, window, cx| {
                this.split_terminal_of(&down, Edge::Bottom, window, cx);
            }),
        ];
        if in_splits {
            let moved = session.to_owned();
            let closed = session.to_owned();
            entries.push(Self::entry(
                "Move Split to New Tab",
                move |this, window, cx| this.move_split_to_new_tab(&moved, window, cx),
            ));
            entries.push(Self::entry("Close Split", move |this, window, cx| {
                this.close_split_of(&closed, window, cx);
            }));
        }
        entries
    }

    // ---- drawing ----------------------------------------------------------------------------

    /// The body of a tab of terminal splits.
    pub(crate) fn render_terminal_split(
        &mut self,
        pane: PaneId,
        split: &TermSplit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if split.zoomed {
            return self.render_split_leaf(pane, split, split.focus, window, cx);
        }
        let mut dividers = 0;
        self.render_split_node(
            pane,
            split,
            &split.tree,
            Vec::new(),
            &mut dividers,
            window,
            cx,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_split_node(
        &mut self,
        pane: PaneId,
        split: &TermSplit,
        node: &PaneTree,
        path: Vec<usize>,
        dividers: &mut usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (axis, ratios, children) = match node {
            PaneTree::Pane(leaf) => {
                return self.render_split_leaf(pane, split, leaf.id, window, cx);
            }
            PaneTree::Split {
                axis,
                ratios,
                children,
            } => (*axis, ratios, children),
        };
        let horizontal = axis == super::model::Axis::Horizontal;
        let (accent, border) = (self.theme.accent, self.theme.border);
        let bounds_cell: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
        let mut container = div()
            .relative()
            .size_full()
            .flex()
            .when(!horizontal, |s| s.flex_col())
            .child({
                // Records the container's painted bounds so a divider drag can turn mouse
                // pixels into a share.
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
            let element =
                self.render_split_node(pane, split, child, child_path, dividers, window, cx);
            // One hairline between two splits, owned by the second: the boundary, never a focus
            // outline.
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
            let number = *dividers;
            *dividers += 1;
            let cell = Rc::clone(&bounds_cell);
            let (down_path, click_path) = (path.clone(), path.clone());
            let group = gpui::SharedString::from(format!("term-divider-{pane}-{path:?}-{divider}"));
            let handle = div()
                .id(element_id(format!(
                    "term-divider-{pane}-{path:?}-{divider}"
                )))
                .absolute()
                .group(group.clone())
                .child(super::render::divider_line(&group, horizontal, accent))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        let bounds = cell.get();
                        let (origin, extent) = if horizontal {
                            (f32::from(bounds.origin.x), f32::from(bounds.size.width))
                        } else {
                            (f32::from(bounds.origin.y), f32::from(bounds.size.height))
                        };
                        this.resizing = Some(Resizing::TermSplit {
                            pane,
                            path: down_path.clone(),
                            divider,
                            horizontal,
                            origin,
                            extent,
                        });
                        cx.stop_propagation();
                    }),
                )
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    // A double-click on a divider shares the split equally.
                    if event.click_count() == 2 {
                        this.mutate(cx, |s| {
                            let _ = s.equalize_term_split(pane, &click_path);
                        });
                    }
                }))
                .child(probe(&self.regions, Region::TermDivider(number)));
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

    /// One split: its terminal, dimmed while another split has the keyboard, with the focused
    /// split's top edge in the accent colour (not colour alone: the text of the others dims).
    fn render_split_leaf(
        &mut self,
        pane: PaneId,
        split: &TermSplit,
        leaf: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(session) = split.session_of(leaf) else {
            return div().size_full().into_any_element();
        };
        let Ok(id) = session.parse::<SessionId>() else {
            return self.placeholder_body(format!("Invalid session id {session}"), cx);
        };
        let theme = self.theme;
        let focused = split.focus == leaf;
        let pane_focused = self.state.focus == FocusTarget::Pane(pane);
        let dim = !focused && !split.zoomed && self.terminal_settings.dim_unfocused_splits;
        let body = self.render_terminal_item(pane, session, window, cx);
        // While a tab is dragged: the four edges of this split take it as a split of its own.
        let accent = theme.accent;
        let zones: Vec<AnyElement> = if cx.has_active_drag() {
            [
                ("top", Edge::Top, (0.0, 0.0, 1.0, 0.25)),
                ("bottom", Edge::Bottom, (0.0, 0.75, 1.0, 0.25)),
                ("left", Edge::Left, (0.0, 0.25, 0.25, 0.5)),
                ("right", Edge::Right, (0.75, 0.25, 0.25, 0.5)),
            ]
            .into_iter()
            .map(|(name, edge, (l, t, w, h))| {
                div()
                    .id(element_id(format!("term-drop-{id}-{name}")))
                    .absolute()
                    .left(relative(l))
                    .top(relative(t))
                    .w(relative(w))
                    .h(relative(h))
                    .drag_over::<DraggedItem>(move |s, _, _, _| {
                        s.bg(accent.opacity(0.25)).border_2().border_color(accent)
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                        this.drop_on_pane_zone(
                            d,
                            DropTarget::Split { pane, edge },
                            Some(leaf),
                            window,
                            cx,
                        );
                    }))
                    .into_any_element()
            })
            .collect()
        } else {
            Vec::new()
        };
        div()
            .id(element_id(format!("term-split-{id}")))
            .relative()
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.focus_split(id, window, cx);
                }),
            )
            .child(body)
            // The others lose a few percent of their contrast; the overlay takes no clicks.
            .children(dim.then(|| {
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(theme.bg.opacity(DIM))
            }))
            .children((focused && !split.zoomed).then(|| {
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(FOCUS_EDGE))
                    .bg(theme.accent.opacity(if pane_focused { 0.6 } else { 0.3 }))
            }))
            .children(zones)
            .child(probe(&self.regions, Region::TermSplit(id)))
            .into_any_element()
    }

    /// The `⊞ 3` badge of a tab that holds splits.
    /// The words of the badge of the tab at `index` of `pane` (`⊞ 3`, `⊞ 3 · zoomed`); `None`
    /// for a tab without splits.
    pub fn split_badge_text(&self, pane: PaneId, index: usize) -> Option<String> {
        let item = self.state.pane(pane)?.items.get(index)?;
        item.as_term_split().map(split_badge_label)
    }

    pub(crate) fn split_count_badge(
        &self,
        pane: PaneId,
        index: usize,
        item: &ItemRef,
    ) -> Option<AnyElement> {
        let split = item.as_term_split()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        Some(
            div()
                .id(element_id(format!("tab-splits-{pane}-{index}")))
                .flex_none()
                .text_size(px(tokens.secondary_font_size))
                // A zoomed split hides the others: the badge says so, in the accent.
                .text_color(if split.zoomed {
                    theme.accent
                } else {
                    theme.muted
                })
                .child(SharedString::from(split_badge_label(split)))
                .child(probe(&self.regions, Region::TabSplits(pane, index)))
                .into_any_element(),
        )
    }
}
