//! Docks as the user and the window leave them (DK-1, `docs/design.md` § Docks).
//!
//! The model (`model/ops.rs`) owns what the user chose: each dock is Expanded, Compact (down to
//! its rail) or Hidden. The window may show less than that for a while, never more: with
//! `layout.auto_collapse` a narrow window shows an expanded side dock as its rail
//! (`model/responsive.rs`). This file joins the two for the shell: what is shown, the commands
//! that change it, the drag past a dock's minimum that collapses it, and the keyboard focus that
//! must never stay in something that is no longer drawn.

use gpui::{Context, Window};

use super::model::{
    AutoState, DockMode, DockPosition, FocusTarget, MIN_CENTER_WIDTH, PanelId, Responsive,
};
use super::view::{Resizing, Workbench};

/// Dragging the edge of a side dock this far below its minimum width arms the collapse to its
/// rail; releasing the mouse then commits it, Escape cancels.
pub const COLLAPSE_DRAG: f32 = 24.0;

/// Key context of the workbench while a dock edge is dragged: Escape cancels the drag.
pub const RESIZE_CONTEXT: &str = "DockResize";

impl Workbench {
    /// What is on screen of dock `pos` now: the user's mode, but an expanded side dock a narrow
    /// window collapsed shows as [`DockMode::Compact`].
    pub fn dock_mode(&self, pos: DockPosition) -> DockMode {
        self.responsive.effective(&self.state.docks, pos)
    }

    /// What the window itself is doing to side dock `pos`.
    pub fn dock_auto_state(&self, pos: DockPosition) -> AutoState {
        self.responsive.auto_state(pos)
    }

    /// The width side dock `pos` takes now (its rail, its body, both, or nothing).
    pub fn dock_extent(&self, pos: DockPosition) -> f32 {
        Responsive::extent(&self.state.docks, pos, self.dock_mode(pos))
    }

    /// The focus handle around what dock `pos` draws: the keyboard is inside the dock while it
    /// (or something under it) is focused.
    pub fn dock_focus_handle(&self, pos: DockPosition) -> gpui::FocusHandle {
        let index = DockPosition::ALL
            .iter()
            .position(|p| *p == pos)
            .unwrap_or_default();
        self.dock_focus[index].clone()
    }

    /// Runs at the start of every render: re-evaluates the window's auto-collapse for its current
    /// width and takes the keyboard out of every dock that is not drawn as a body any more.
    pub(crate) fn sync_docks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width = f32::from(window.viewport_size().width);
        let enabled = self.layout_seen.auto_collapse;
        self.responsive.evaluate(&self.state.docks, width, enabled);
        for (index, pos) in DockPosition::ALL.into_iter().enumerate() {
            if self.dock_mode(pos) == DockMode::Expanded {
                continue;
            }
            if self.state.focus == FocusTarget::Dock(pos) {
                self.state.focus = FocusTarget::Pane(self.state.active_pane_id());
            }
            if self.dock_focus[index].contains_focused(window, cx) {
                self.focus_root(window, cx);
            }
        }
    }

    /// After every model change: a dock the user just opened, or put the keyboard in, stays open
    /// in a window that would collapse it, until the window has grown.
    pub(crate) fn note_dock_modes(&mut self) {
        for (index, pos) in DockPosition::ALL.into_iter().enumerate() {
            let now = self.state.docks.get(pos).mode();
            let opened =
                now == DockMode::Expanded && self.dock_modes_seen[index] != DockMode::Expanded;
            let entered = self.state.focus == FocusTarget::Dock(pos)
                && self.focus_dock_seen != Some(pos)
                && now == DockMode::Expanded;
            if opened || entered {
                self.responsive.reveal(&self.state.docks, pos);
            }
            self.dock_modes_seen[index] = now;
        }
        self.focus_dock_seen = match self.state.focus {
            FocusTarget::Dock(pos) => Some(pos),
            FocusTarget::Pane(_) => None,
        };
    }

    /// ⌘B, ⌘L, ⌘⌥J and the status bar buttons: the working toggle of `pos`
    /// (`WorkbenchState::toggle_dock`). A side dock the window collapsed opens instead.
    pub(crate) fn toggle_dock_cmd(&mut self, pos: DockPosition, cx: &mut Context<Self>) {
        if self.responsive.auto_state(pos) == AutoState::Collapsed {
            self.responsive.open(pos);
            cx.notify();
            return;
        }
        self.mutate(cx, |s| s.toggle_dock(pos));
    }

    /// Puts `pos` in `mode` as the user's choice (the titlebar menus, "Hide Left Sidebar", "Hide
    /// Right Rail"). Opening a dock the window collapsed keeps it open until the window grows.
    pub(crate) fn set_dock_mode_cmd(
        &mut self,
        pos: DockPosition,
        mode: DockMode,
        cx: &mut Context<Self>,
    ) {
        if mode == DockMode::Expanded && self.responsive.auto_state(pos) == AutoState::Collapsed {
            self.responsive.open(pos);
        }
        self.mutate(cx, |s| s.set_dock_mode(pos, mode));
    }

    /// The titlebar's sidebar button: hides the sidebar, or brings it back in the mode it was
    /// hidden from.
    pub(crate) fn toggle_dock_hidden_cmd(&mut self, pos: DockPosition, cx: &mut Context<Self>) {
        if self.dock_mode(pos) == DockMode::Hidden {
            self.mutate(cx, |s| s.toggle_dock_hidden(pos));
            self.responsive.reveal(&self.state.docks, pos);
        } else {
            self.mutate(cx, |s| s.set_dock_mode(pos, DockMode::Hidden));
        }
    }

    /// A click on the icon of `panel` in the rail of `pos`: the icon of the panel the open dock
    /// shows collapses it to the rail, any other icon expands the dock on that panel.
    pub(crate) fn click_rail_icon(
        &mut self,
        pos: DockPosition,
        panel: &PanelId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shown = self.dock_mode(pos);
        if shown != DockMode::Expanded {
            self.responsive.open(pos);
        }
        let panel = panel.clone();
        self.mutate(cx, |s| {
            s.click_rail_icon(pos, &panel, shown);
        });
        self.focus_root(window, cx);
    }

    // ---- drag past the minimum ---------------------------------------------------------------

    /// The size a drag of the edge of `pos` is at: `raw` clamped to what the dock may be, and
    /// whether `raw` is far enough below the minimum to arm the collapse to the rail.
    pub(crate) fn drag_size(&self, pos: DockPosition, raw: f32, window: &Window) -> (f32, bool) {
        let (min, mut max) = pos.size_range();
        let viewport = window.viewport_size();
        match pos {
            DockPosition::Bottom => {
                max = max.min(f32::from(viewport.height) * 0.6);
            }
            DockPosition::Left | DockPosition::Right => {
                let other = match pos {
                    DockPosition::Left => DockPosition::Right,
                    _ => DockPosition::Left,
                };
                let rail = if pos == DockPosition::Right {
                    pos.rail_width()
                } else {
                    0.0
                };
                let room =
                    f32::from(viewport.width) - self.dock_extent(other) - rail - MIN_CENTER_WIDTH;
                max = max.min(room.max(min));
            }
        }
        let armed = pos != DockPosition::Bottom && raw <= min - COLLAPSE_DRAG;
        (raw.clamp(min, max), armed)
    }

    /// Mouse released: a drag that armed the collapse commits it, and the dock goes down to its
    /// rail with the size it had before the drag below the minimum.
    pub(crate) fn finish_resize(&mut self, cx: &mut Context<Self>) {
        let Some(resizing) = self.resizing.take() else {
            return;
        };
        if let Resizing::Dock {
            pos,
            start_size,
            armed: true,
            ..
        } = resizing
        {
            // The width the dock had before the drag is what it opens at again.
            self.mutate(cx, |s| {
                s.resize_dock(pos, start_size);
                s.collapse_dock(pos);
            });
        } else {
            cx.notify();
        }
    }

    /// Escape during a drag: the dock goes back to the size it had when the drag began.
    pub(crate) fn cancel_resize(&mut self, cx: &mut Context<Self>) {
        let Some(resizing) = self.resizing.take() else {
            return;
        };
        if let Resizing::Dock {
            pos, start_size, ..
        } = resizing
        {
            self.mutate(cx, |s| s.resize_dock(pos, start_size));
        } else {
            cx.notify();
        }
    }

    /// The side dock whose drag is armed to collapse on release.
    pub(crate) fn armed_collapse(&self) -> Option<DockPosition> {
        match self.resizing {
            Some(Resizing::Dock {
                pos, armed: true, ..
            }) => Some(pos),
            _ => None,
        }
    }
}
