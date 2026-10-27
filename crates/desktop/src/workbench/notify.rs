//! `ui.open` (protocol 0.13): the daemon asks this window to show a session, because the user
//! chose "Open" on a notification intent.

use gpui::{Context, Window};

use super::model::{DockPosition, ItemRef, LayoutPreset, PanelId};
use super::panels::{AssistantPanel, ScopeKey};
use super::view::Workbench;
use crate::sessions::{MawasFocus, UiOpenRequested};

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
            self.dock_body_shown(pos)
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
        // Home's own changes (its scope) reach `sync_mawas_focus` from here on: never from
        // inside a frame, where GPUI drops the notification the store would send.
        if let Some(panel) = self.panel::<AssistantPanel>("assistant")
            && self.mawas_watch.as_ref().map(|(id, _)| *id) != Some(panel.entity_id())
        {
            let watch = cx.observe(&panel, |this, _, cx| this.sync_mawas_focus(cx));
            self.mawas_watch = Some((panel.entity_id(), watch));
        }
    }

    /// Files, Search and Problems follow Mawas's scope while Mawas Home is the focused center
    /// tab (the active tab of the active pane, wherever the keyboard is): Global is the Home
    /// folder, a workspace's Mawas is that workspace's active project. Any other tab, or Mawas
    /// off, and they follow the active container again. Runs after every layout change, every
    /// session or workspace change and every change of Home itself — never during a frame
    /// (GPUI does not deliver a notification made while drawing).
    pub(crate) fn sync_mawas_focus(&mut self, cx: &mut Context<Self>) {
        let shown = self.assistant_enabled()
            && self.state.active_center_item() == Some(ItemRef::panel("assistant"));
        let focus = shown.then(|| match self.panel::<AssistantPanel>("assistant") {
            Some(panel) => match panel.read(cx).key(cx) {
                ScopeKey::Global => MawasFocus::Global,
                ScopeKey::Workspace(id) => MawasFocus::Workspace(id),
            },
            // Home is shown before its page is first drawn: it opens on the active scope.
            None => match self.sessions.read(cx).active_workspace() {
                Some(w) if w.kind != hutan_protocol::messages::WorkspaceKind::Home => {
                    MawasFocus::Workspace(w.id)
                }
                _ => MawasFocus::Global,
            },
        });
        self.sessions
            .update(cx, |store, cx| store.set_mawas_focus(focus, cx));
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
