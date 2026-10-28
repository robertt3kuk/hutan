//! Docks as the user and the window leave them (DK-1, `docs/design.md` § Docks).
//!
//! The model (`model/ops.rs`) owns what the user chose: each dock is Expanded, Compact (down to
//! its rail) or Hidden. The window may show less than that for a while, never more: with
//! `layout.auto_collapse` a narrow window shows an expanded side dock as its rail
//! (`model/responsive.rs`). This file joins the two for the shell: what is shown, the commands
//! that change it, the drag past a dock's minimum that collapses it, and the keyboard focus that
//! must never stay in something that is no longer drawn.
//!
//! **Rail peek** (EX-2, `layout.rail_peek`): where a body opened from its rail would leave the
//! center under 480 pt, the body opens as a *peek* instead — a layer floating over the center's
//! edge at the dock's width. The peek is not a dock mode and is not saved: the dock stays on
//! its rail, the center never moves, the keyboard is in the peek while it is open and goes back
//! to the center when it closes (Esc, a click outside, opening a file, the same icon, any other
//! place taking the focus, a window wide enough to push).

use gpui::{Context, Window};

use super::model::{
    AutoState, DockMode, DockPosition, FocusTarget, MIN_CENTER_WIDTH, OpenAs, PanelId, Responsive,
};
use super::view::{Resizing, Workbench};

/// An invisible element whose paint registers window-level mouse observers for the frame: while a
/// dock edge or a divider is dragged, every move and the release reach the workbench whatever is
/// under the pointer. (The workbench root's own handlers need the pointer over the root, and the
/// handle being dragged — which occludes it — is under the pointer: a slow drag stalled until the
/// pointer left the handle.)
pub fn resize_observer(view: gpui::Entity<Workbench>) -> impl gpui::IntoElement {
    use gpui::{DispatchPhase, MouseButton, MouseMoveEvent, MouseUpEvent, Styled as _, canvas};
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let moved = view.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture {
                    moved.update(cx, |this, cx| this.on_mouse_move(event, window, cx));
                }
            });
            let released = view.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    released.update(cx, |this, cx| this.finish_resize(cx));
                }
            });
        },
    )
    .absolute()
    .size_0()
}

/// Dragging the edge of a side dock this far below its minimum width arms the collapse to its
/// rail; releasing the mouse then commits it, Escape cancels.
pub const COLLAPSE_DRAG: f32 = 24.0;

/// Key context of the workbench while a dock edge is dragged: Escape cancels the drag.
pub const RESIZE_CONTEXT: &str = "DockResize";

/// Key context of the workbench while a peek is open: Escape closes it (unless something
/// deeper — a palette, a field, a menu — takes the key first).
pub const PEEK_CONTEXT: &str = "Peek";

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

    /// The side dock whose panel floats over the center now.
    pub fn peek_dock(&self) -> Option<DockPosition> {
        self.peek
    }

    /// Whether the body of `pos` is on screen: beside the center, or as the peek over it.
    pub fn dock_body_shown(&self, pos: DockPosition) -> bool {
        self.dock_mode(pos) == DockMode::Expanded || self.peek == Some(pos)
    }

    /// The width of the peek of `pos`.
    pub(crate) fn peek_width(&self, pos: DockPosition) -> f32 {
        self.responsive.peek_width(&self.state.docks, pos)
    }

    /// Whether `panel`, opened now from the rail of `pos`, would be a peek: the setting is on,
    /// the body is not on screen already as a body and the center would be under 480 pt beside
    /// it. A left page that *is* the sidebar (Projects, Tasks, Memory) is never a peek: it
    /// expands the sidebar as it always did.
    fn would_peek(&self, pos: DockPosition, panel: &PanelId) -> bool {
        let peekable = match pos {
            DockPosition::Bottom => false,
            DockPosition::Left => !matches!(
                panel.as_str(),
                "sessions" | "tasks" | super::model::MEMORY_PAGE
            ),
            DockPosition::Right => true,
        };
        peekable
            && self.state.docks.get(pos).contains(panel)
            && self.dock_mode(pos) != DockMode::Expanded
            && self
                .responsive
                .open_as(&self.state.docks, pos, self.layout_seen.rail_peek)
                == OpenAs::Peek
    }

    /// Shows `panel` of `pos` as the peek (or switches the open peek to it) and gives it the
    /// keyboard on the next frame. The dock keeps its mode.
    fn open_peek(&mut self, pos: DockPosition, panel: &PanelId, cx: &mut Context<Self>) {
        self.peek = Some(pos);
        self.peek_focus_pending = true;
        let panel = panel.clone();
        self.mutate(cx, |s| {
            let _ = s.set_dock_active(pos, &panel);
            s.focus_dock(pos);
        });
    }

    /// Ends the peek without touching the keyboard: what the model calls focus goes back to the
    /// center, and the next frame moves the real focus out of the dock (`sync_docks`).
    pub(crate) fn release_peek(&mut self) {
        let Some(pos) = self.peek.take() else {
            return;
        };
        self.peek_focus_pending = false;
        if self.state.focus == FocusTarget::Dock(pos) {
            self.state.focus = FocusTarget::Pane(self.state.active_pane_id());
        }
    }

    /// Closes the peek and hands the keyboard back to the last active item of the center.
    pub(crate) fn close_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.peek.is_none() {
            return;
        }
        self.release_peek();
        self.focus_root(window, cx);
        cx.notify();
    }

    /// Runs at the start of every render: re-evaluates the window's auto-collapse for its current
    /// width and takes the keyboard out of every dock that is not drawn as a body any more. A
    /// peek the window no longer needs (it grew, the dock is open, the panel is gone, the
    /// setting is off) ends here; a new one takes the keyboard.
    pub(crate) fn sync_docks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width = f32::from(window.viewport_size().width);
        let enabled = self.layout_seen.auto_collapse;
        self.responsive.evaluate(&self.state.docks, width, enabled);
        self.retire_rail_hint();
        if let Some(pos) = self.peek {
            let kept = self
                .state
                .docks
                .get(pos)
                .active
                .clone()
                .is_some_and(|panel| self.would_peek(pos, &panel));
            if !kept {
                self.release_peek();
            }
        }
        if self.peek_focus_pending
            && let Some(pos) = self.peek
        {
            self.peek_focus_pending = false;
            // The keyboard is where it was left (the workbench itself), or nowhere: a panel
            // that took it already keeps it.
            if window.focused(cx).is_none_or(|f| f == self.focus_handle) {
                let handle = self.dock_focus_handle(pos);
                window.focus(&handle, cx);
            }
        }
        for (index, pos) in DockPosition::ALL.into_iter().enumerate() {
            if self.dock_body_shown(pos) {
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
    /// in a window that would collapse it, until the window has grown. A peek ends when the
    /// model's focus is anywhere but in its dock (a tab, a file, another dock).
    pub(crate) fn note_dock_modes(&mut self) {
        if let Some(pos) = self.peek
            && self.state.focus != FocusTarget::Dock(pos)
        {
            self.release_peek();
        }
        for (index, pos) in DockPosition::ALL.into_iter().enumerate() {
            let now = self.state.docks.get(pos).mode();
            let opened =
                now == DockMode::Expanded && self.dock_modes_seen[index] != DockMode::Expanded;
            let entered = self.state.focus == FocusTarget::Dock(pos)
                && self.focus_dock_seen != Some(pos)
                && now == DockMode::Expanded;
            if (opened || entered) && self.peek != Some(pos) {
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
    /// (`WorkbenchState::toggle_dock`). A side dock the window collapsed opens instead — as a
    /// peek when the center has no room for its body — and ⌘L over an open peek closes it.
    pub(crate) fn toggle_dock_cmd(&mut self, pos: DockPosition, cx: &mut Context<Self>) {
        if self.peek == Some(pos) {
            self.release_peek();
            cx.notify();
            return;
        }
        if self.dock_mode(pos) == DockMode::Compact
            && let Some(panel) = self.state.docks.get(pos).active.clone()
            && self.would_peek(pos, &panel)
        {
            self.open_peek(pos, &panel, cx);
            return;
        }
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
        if self.peek == Some(pos) {
            self.release_peek();
        }
        if mode == DockMode::Expanded && self.responsive.auto_state(pos) == AutoState::Collapsed {
            self.responsive.open(pos);
        }
        self.mutate(cx, |s| s.set_dock_mode(pos, mode));
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

    /// The titlebar's sidebar button: hides the sidebar, or brings it back in the mode it was
    /// hidden from.
    pub(crate) fn toggle_dock_hidden_cmd(&mut self, pos: DockPosition, cx: &mut Context<Self>) {
        if self.peek == Some(pos) {
            self.release_peek();
        }
        if self.dock_mode(pos) == DockMode::Hidden {
            self.mutate(cx, |s| s.toggle_dock_hidden(pos));
            self.responsive.reveal(&self.state.docks, pos);
        } else {
            self.mutate(cx, |s| s.set_dock_mode(pos, DockMode::Hidden));
        }
    }

    /// A click on the icon of `panel` in the rail of `pos`: the icon of the panel the open dock
    /// shows collapses it to the rail, any other icon expands the dock on that panel. Where the
    /// body would leave the center under 480 pt it is a peek over the center instead: the icon
    /// of the panel the peek shows closes it, another icon switches the peek to that panel.
    pub(crate) fn click_rail_icon(
        &mut self,
        pos: DockPosition,
        panel: &PanelId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.peek == Some(pos) && self.state.docks.get(pos).active.as_ref() == Some(panel) {
            self.close_peek(window, cx);
            return;
        }
        if self.would_peek(pos, panel) {
            self.open_peek(pos, panel, cx);
            self.focus_peek(window, cx);
            return;
        }
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

    /// Gives the keyboard to the peek's dock (a click has the window at hand; commands let the
    /// next frame do it, `sync_docks`).
    fn focus_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pos) = self.peek {
            self.peek_focus_pending = false;
            let handle = self.dock_focus_handle(pos);
            window.focus(&handle, cx);
        }
    }

    /// Opens `panel` as a peek when it sits in a side dock whose body would squeeze the center
    /// (a panel command: ⌘⇧E, *Panel: Open …*). Returns whether it did; otherwise nothing
    /// changed and the panel opens as it always did.
    pub(crate) fn peek_panel(&mut self, panel: &PanelId, cx: &mut Context<Self>) -> bool {
        let Some(pos) = self.state.docks.find(panel) else {
            return false;
        };
        if !self.would_peek(pos, panel) {
            return false;
        }
        self.open_peek(pos, panel, cx);
        true
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
