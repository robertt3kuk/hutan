//! `ui.open` (protocol 0.13): the daemon asks this window to show a session, because the user
//! chose "Open" on a notification intent.

use gpui::{Context, Window};

use super::model::{DockMode, DockPosition, LayoutPreset, PanelId};
use super::panels::AssistantPanel;
use super::view::Workbench;
use crate::sessions::UiOpenRequested;

impl Workbench {
    /// Whether Mawas Home is on screen.
    fn assistant_visible(&self) -> bool {
        self.page_visible("assistant")
    }

    /// Whether the page `panel` is on screen: the active tab of a pane outside the overview (or,
    /// when the user moved it there, the active panel of a visible dock).
    fn page_visible(&self, panel: &str) -> bool {
        let id = PanelId::new(panel);
        let docked = [
            DockPosition::Left,
            DockPosition::Right,
            DockPosition::Bottom,
        ]
        .into_iter()
        .any(|pos| {
            self.dock_mode(pos) == DockMode::Expanded
                && self.state.docks.get(pos).active.as_ref() == Some(&id)
        });
        docked
            || (self.state.preset != LayoutPreset::OVERVIEW
                && self
                    .state
                    .panes()
                    .into_iter()
                    .filter_map(|pane| pane.active_item())
                    .any(|item| item.as_panel() == Some(&id)))
    }

    /// Tells the sessions store whether the user is typing into the Assistant panel's composer
    /// (`client.focus`): the panel is on screen, the window is active, and the composer of the
    /// scope's session holds the keyboard. Runs every frame, so moving the panel to a pane or
    /// closing it, changing its scope, and the window losing or gaining activation are all seen
    /// without a hook of their own; the store only acts on a change.
    pub(crate) fn sync_assistant_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.assistant_visible();
        // A page that is not on screen does no work: it hears of the store only when shown.
        if let Some(panel) = self.panel::<AssistantPanel>("assistant") {
            panel.update(cx, |panel, cx| panel.set_visible(visible, cx));
        }
        let memory_visible = self.page_visible("mawas-memory");
        if let Some(panel) = self.panel::<super::panels::MemoryPanel>("mawas-memory") {
            panel.update(cx, |panel, cx| panel.set_visible(memory_visible, cx));
        }
        let session = (window.is_window_active() && visible)
            .then(|| self.panel::<AssistantPanel>("assistant"))
            .flatten()
            .and_then(|panel| {
                let panel = panel.read(cx);
                let session = panel.session(cx)?;
                let view = panel.chat_view(session)?;
                view.read(cx)
                    .focus_handle_is_focused(window)
                    .then_some(session)
            });
        self.sessions
            .update(cx, |store, cx| store.set_assistant_focused(session, cx));
    }

    /// Brings the window forward and focuses the session's tab (its workspace first), with the
    /// same item machinery as every other way of opening a session. A session that is gone or
    /// no longer live is not opened; the window says so instead.
    pub(crate) fn on_ui_open(
        &mut self,
        event: &UiOpenRequested,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let live = self
            .sessions
            .read(cx)
            .session(event.session_id)
            .map(|info| info.state.is_live());
        match live {
            Some(true) => {
                self.bring_forward(window, cx);
                self.open_session(event.session_id, window, cx);
            }
            Some(false) => {
                self.bring_forward(window, cx);
                self.set_notice("That session is no longer running", cx);
            }
            None => self.set_notice("That session no longer exists", cx),
        }
    }
}
