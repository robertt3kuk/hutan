//! Workbench behavior around the resources panel (M13-D): showing and toggling it, "Show in
//! Resources" for a session, the context menu of a line, and the question before a kill. The
//! panel itself is `panels/resources.rs`.

use gpui::{Context, Window};
use hutan_core::SessionId;

use super::actions::ResourceMenuAt;
use super::model::{ItemSource, PanelId};
use super::panels::{ResourceCommand, ResourcesPanel};
use super::view::{MenuEntry, Workbench};

/// The panel's id in the registry.
const PANEL: &str = "resources";

impl Workbench {
    /// "Resources: Show Panel".
    pub(crate) fn show_resources(&mut self, cx: &mut Context<Self>) {
        self.open_panel(PANEL, cx);
    }

    /// ⌥⌘R: hides the panel when it is showing (its dock, or its tab), shows it otherwise.
    pub(crate) fn toggle_resources(&mut self, cx: &mut Context<Self>) {
        let id = PanelId::new(PANEL);
        match self.state.find_panel(&id) {
            Some(ItemSource::Dock { pos, .. })
                if self.dock_body_shown(pos)
                    && self.state.docks.get(pos).active.as_ref() == Some(&id) =>
            {
                self.toggle_dock_cmd(pos, cx);
                return;
            }
            Some(ItemSource::Pane { pane, index })
                if self.state.pane(pane).is_some_and(|p| p.active == index) =>
            {
                self.try_mutate(cx, |s| s.close_panel(&id));
                return;
            }
            _ => {}
        }
        self.open_panel(PANEL, cx);
    }

    /// "Show in Resources": reveals the panel and points it at `session`.
    pub fn show_in_resources(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_panel(PANEL, cx);
        let id = PanelId::new(PANEL);
        if let Some(panel) = self
            .panel_view(&id, window, cx)
            .and_then(|view| view.downcast::<ResourcesPanel>().ok())
        {
            panel.update(cx, |panel, cx| panel.reveal(session, cx));
        }
    }

    /// Sends `command` through the panel (inline errors on its row), after asking when it ends
    /// something for good.
    pub(crate) fn run_resource_command(
        &mut self,
        command: ResourceCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.panel::<ResourcesPanel>(PANEL) else {
            return;
        };
        let Some(ask) = panel.read(cx).confirmation(&command, cx) else {
            panel.update(cx, |panel, cx| panel.run(command, cx));
            return;
        };
        self.open_destructive(
            ask.title,
            ask.message,
            ask.confirm,
            move |this, _, cx| {
                if let Some(panel) = this.panel::<ResourcesPanel>(PANEL) {
                    panel.update(cx, |panel, cx| panel.run(command.clone(), cx));
                }
            },
            window,
            cx,
        );
    }

    /// The context menu of a line: Open Session, then what the panel offers for it. A greyed-out
    /// entry carries the reason.
    pub(crate) fn resource_menu(&self, at: &ResourceMenuAt, cx: &gpui::App) -> Vec<MenuEntry> {
        let Ok(session) = at.session.parse::<SessionId>() else {
            return Vec::new();
        };
        let Some(panel) = self.panel::<ResourcesPanel>(PANEL) else {
            return Vec::new();
        };
        let items = panel
            .read(cx)
            .items_for(session, at.pid, at.node.as_deref(), cx);
        if items.is_empty() {
            return Vec::new();
        }
        let mut entries = vec![Self::entry("Open Session", move |this, window, cx| {
            this.open_session(session, window, cx);
        })];
        for item in items {
            entries.push(match (item.command, item.disabled) {
                (_, Some(reason)) => Self::disabled_entry(format!("{} — {reason}", item.label)),
                (Some(command), None) => Self::entry(item.label, move |this, window, cx| {
                    this.run_resource_command(command.clone(), window, cx);
                }),
                (None, None) => Self::disabled_entry(item.label),
            });
        }
        entries
    }

    /// Opens the menu of a line at its position.
    pub(crate) fn show_resource_menu(
        &mut self,
        at: &ResourceMenuAt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.resource_menu(at, cx);
        if !entries.is_empty() {
            self.open_menu(at.position, entries, window, cx);
        }
    }
}
