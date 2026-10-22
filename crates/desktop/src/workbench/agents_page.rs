//! Workbench behavior around the Agents page (AG-1): opening the one retained tab (from the
//! left nav, the compact rail, the right panel, Today and the palette), the menus of its rows,
//! and the commands those menus run. The page itself is `panels/agents_page.rs`; it only
//! collects decisions, the workbench carries them out.
//!
//! The page is one entity: the nav, the rail and the Agents overview (⌘⇧O, a reversible focus
//! layout, `model/preset.rs`) all show the same one, with the same selection and scope.

use std::path::PathBuf;

use gpui::{Context, Entity, Focusable as _, Pixels, Point, Window};
use hutan_core::SessionId;
use hutan_protocol::messages::WorkspaceKind;

use super::actions;
use super::model::{AGENTS_PAGE, DockMode, DockPosition, ItemRef, PanelId};
use super::panels::{AgentsPage, AgentsPanel, AgentsRequest};
use super::view::{MenuEntry, Workbench};
use crate::sessions::agents_text::{self, Avail, Caps};

impl Workbench {
    /// The Agents page entity, made when it is first asked for.
    pub(crate) fn agents_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<AgentsPage>> {
        self.panel_view(&PanelId::new(AGENTS_PAGE), window, cx)?
            .downcast::<AgentsPage>()
            .ok()
    }

    /// Shows the Agents page (a tab of the center, one instance; clicking again focuses the
    /// existing tab and never closes anything), applies `request` to it and puts the keyboard
    /// in its list.
    pub fn open_agents_page(
        &mut self,
        request: AgentsRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_panel(AGENTS_PAGE, cx);
        if let Some(page) = self.agents_page(window, cx) {
            page.update(cx, |page, cx| page.apply_request(request, cx));
            window.focus(&page.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Whether the Agents page is on screen: the active tab of a pane (the Agents overview
    /// included) or the active panel of a visible dock.
    pub(crate) fn agents_page_visible(&self) -> bool {
        let id = PanelId::new(AGENTS_PAGE);
        let docked = DockPosition::ALL.into_iter().any(|pos| {
            self.dock_mode(pos) == DockMode::Expanded
                && self.state.docks.get(pos).active.as_ref() == Some(&id)
        });
        docked
            || self
                .state
                .panes()
                .into_iter()
                .filter_map(|pane| pane.active_item())
                .any(|item| item == &ItemRef::panel(AGENTS_PAGE))
    }

    /// Tells the page whether it is on screen and whether a menu is open (it freezes the order
    /// of its rows meanwhile). Runs every frame; the page only acts on a change, and a page that
    /// is not on screen hears of the store only when it is shown again.
    pub(crate) fn sync_agents_page(&mut self, cx: &mut Context<Self>) {
        let visible = self.agents_page_visible();
        let menu_open = self.context_menu.is_some();
        if let Some(page) = self.panel::<AgentsPage>(AGENTS_PAGE) {
            page.update(cx, |page, cx| {
                page.set_visible(visible, cx);
                page.set_menu_open(menu_open, cx);
            });
        }
    }

    /// What the connection lets a row do.
    pub(crate) fn agent_caps(&self, cx: &gpui::App) -> Caps {
        let store = self.sessions.read(cx);
        Caps {
            online: store.connection_state() == hutan_client::ConnectionState::Connected,
            chat: store.connection().supports_chat(),
        }
    }

    /// The menu of a row: Open session, Reveal session, Open worktree, Show changes, Open
    /// transcript, Copy session ID, Interrupt turn, Stop session…. What cannot be done is
    /// greyed out and says why in its label; nothing here acts on a child by acting on its
    /// parent.
    pub(crate) fn agent_menu(&self, session: SessionId, cx: &gpui::App) -> Vec<MenuEntry> {
        let presentation = self.sessions.read(cx).agent_presentation();
        let Some(record) = presentation.get(session) else {
            return Vec::new();
        };
        let caps = self.agent_caps(cx);
        let gated = |label: &str, avail: Avail, entry: MenuEntry| -> MenuEntry {
            match avail {
                Avail::Yes => entry,
                Avail::No(why) => MenuEntry {
                    label: format!("{label} ({why})").into(),
                    disabled: true,
                    ..entry
                },
            }
        };
        let mut entries = vec![
            Self::entry("Open session", move |this, window, cx| {
                this.open_session_from_overview(session, window, cx);
            }),
            Self::entry("Reveal session", move |this, window, cx| {
                this.reveal_session(session, window, cx);
            }),
        ];
        entries.push(if record.checkout.is_some() {
            Self::entry("Open worktree", move |this, _, cx| {
                this.open_worktree_of(session, cx);
            })
        } else {
            MenuEntry {
                label: "Open worktree (this session is not in a git checkout)".into(),
                disabled: true,
                ..Self::entry("Open worktree", |_, _, _| {})
            }
        });
        entries.push(gated(
            "Show changes",
            agents_text::changes(record),
            Self::entry("Show changes", move |this, window, cx| {
                this.show_agent_changes(session, window, cx);
            }),
        ));
        entries.push(gated(
            "Open transcript",
            agents_text::transcript(record),
            Self::entry("Open transcript", move |this, _, cx| {
                this.open_transcript_of(session, cx);
            }),
        ));
        // Agents page → session ⋯ → Checkpoints… (CK-5): each turn that changed files, with
        // its Rewind….
        entries.extend(self.checkpoints_menu_entry(session, cx));
        entries.push(Self::entry("Copy session ID", move |this, _, cx| {
            this.copy_session_id(session, cx);
        }));
        entries.extend(self.attention_entries(session, cx));
        entries.push(gated(
            "Interrupt turn",
            agents_text::interrupt(record, caps),
            Self::entry("Interrupt turn", move |this, window, cx| {
                this.interrupt_agent(session, window, cx);
            }),
        ));
        entries.push(gated(
            "Stop session…",
            agents_text::stop(record, caps),
            Self::entry("Stop session…", move |this, window, cx| {
                this.stop_session(session, window, cx);
            }),
        ));
        entries
    }

    /// Opens the row menu of `session` at `position` (the More button, a right click,
    /// Shift-F10).
    pub(crate) fn show_agent_menu(
        &mut self,
        session: SessionId,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.agent_menu(session, cx);
        if !entries.is_empty() {
            self.open_menu(position, entries, window, cx);
        }
    }

    /// The entries of the page's scope: All workspaces, then every workspace, project and Home;
    /// the one shown is checked. `prefix` heads each label (`Scope: ` inside the folded ⋯ menu).
    fn agents_scope_entries(
        &self,
        page: &Entity<AgentsPage>,
        prefix: &str,
        cx: &gpui::App,
    ) -> Vec<MenuEntry> {
        let current = page.read(cx).scope();
        let store = self.sessions.read(cx);
        let mut choices = vec![("All workspaces".to_owned(), crate::sessions::Scope::All)];
        for info in store.workspaces() {
            // A project inside a workspace is part of its workspace's scope.
            if info.kind == WorkspaceKind::Project && info.parent_id.is_some() {
                continue;
            }
            choices.push((
                info.name.clone(),
                crate::sessions::Scope::Workspace(info.id),
            ));
        }
        choices
            .into_iter()
            .map(|(label, scope)| {
                let page = page.clone();
                let mut entry = Self::entry(format!("{prefix}{label}"), move |_, _, cx| {
                    page.update(cx, |page, cx| page.set_scope(scope, cx));
                });
                entry.checked = scope == current;
                entry
            })
            .collect()
    }

    /// The menu of the page's scope button.
    pub(crate) fn show_agents_scope_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = self.agents_page(window, cx) else {
            return;
        };
        let entries = self.agents_scope_entries(&page, "", cx);
        self.open_menu(position, entries, window, cx);
    }

    /// The page's ⋯ menu: *Group by workspace* and *Show finished* (checked while on), and,
    /// while the toolbar is folded below 640 pt, the scope first (`Scope: All workspaces`).
    pub(crate) fn show_agents_page_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(page) = self.agents_page(window, cx) else {
            return;
        };
        let settings = self.settings.get::<crate::settings::AgentsSettings>();
        let mut entries = if page.read(cx).folded() {
            self.agents_scope_entries(&page, "Scope: ", cx)
        } else {
            Vec::new()
        };
        let group = page.clone();
        let mut by_workspace = Self::entry("Group by workspace", move |_, _, cx| {
            group.update(cx, |page, cx| page.toggle_group_by_workspace(cx));
        });
        by_workspace.checked = settings.page_group_by_workspace;
        let finished = page.clone();
        let mut show_finished = Self::entry("Show finished", move |_, _, cx| {
            finished.update(cx, |page, cx| page.toggle_show_finished(cx));
        });
        show_finished.checked = settings.page_show_finished;
        entries.push(by_workspace);
        entries.push(show_finished);
        self.open_menu(position, entries, window, cx);
    }

    /// The right Agents panel's ⋯ menu: *Show finished* and *Blocked only* (checked while on),
    /// *All agents* (the page) and the panel's settings.
    pub(crate) fn show_agents_panel_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.panel::<AgentsPanel>("agents") else {
            return;
        };
        let settings = self.settings.get::<crate::settings::AgentsSettings>();
        let finished = panel.clone();
        let mut show_finished = Self::entry("Show finished", move |_, _, cx| {
            finished.update(cx, |panel, cx| panel.toggle_show_finished(cx));
        });
        show_finished.checked = settings.show_finished;
        let blocked = panel.clone();
        let mut only_blocked = Self::entry("Blocked only", move |_, _, cx| {
            blocked.update(cx, |panel, cx| panel.toggle_only_blocked(cx));
        });
        only_blocked.checked = settings.only_blocked;
        let entries = vec![
            show_finished,
            only_blocked,
            Self::entry("All agents", |_, window, cx| {
                window.dispatch_action(Box::new(actions::ShowAgentsPage), cx);
            }),
            Self::entry("Agents settings…", |_, window, cx| {
                window.dispatch_action(Box::new(actions::OpenSettingsFor("agents".to_owned())), cx);
            }),
        ];
        self.open_menu(position, entries, window, cx);
    }

    /// *Show changes* / *Review changes*: the diff of the session's checkout.
    pub(crate) fn show_agent_changes(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self
            .sessions
            .read(cx)
            .agent_presentation()
            .get(session)
            .and_then(|r| r.checkout.as_ref().map(|c| c.root.clone()));
        match root {
            Some(root) => self.open_changes(root, window, cx),
            None => self.set_notice("This session is not in a git checkout", cx),
        }
    }

    /// *Interrupt turn*: `chat.interrupt` for a chat the daemon runs. Nothing else is
    /// interrupted by this, and an agent in a terminal has no such command here.
    pub(crate) fn interrupt_agent(
        &mut self,
        session: SessionId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let allowed = {
            let caps = self.agent_caps(cx);
            self.sessions
                .read(cx)
                .agent_presentation()
                .get(session)
                .map(|record| agents_text::interrupt(record, caps))
        };
        match allowed {
            Some(Avail::Yes) => {}
            Some(Avail::No(why)) => {
                self.set_notice(format!("Cannot interrupt: {why}"), cx);
                return;
            }
            None => return,
        }
        let connection = self.sessions.read(cx).connection().clone();
        cx.spawn(async move |this, cx| {
            if let Err(error) = connection.chat_interrupt(session).await {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Could not interrupt: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// *Reveal session*: the Projects sidebar at the session's workspace.
    pub(crate) fn reveal_session(
        &mut self,
        session: SessionId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_workspace_of(session, cx);
        self.open_panel("sessions", cx);
    }

    /// *Open worktree*: the checkout's folder in Finder.
    pub(crate) fn open_worktree_of(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let root = self
            .sessions
            .read(cx)
            .agent_presentation()
            .get(session)
            .and_then(|r| r.checkout.as_ref().map(|c| c.root.clone()));
        match root {
            Some(root) => (self.daemon.reveal.clone())(&root, cx),
            None => self.set_notice("This session is not in a git checkout", cx),
        }
    }

    /// *Open transcript*: the main agent's own transcript, in Finder.
    pub(crate) fn open_transcript_of(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let path = self
            .sessions
            .read(cx)
            .agent_presentation()
            .get(session)
            .and_then(|r| r.transcript.clone());
        match path {
            Some(path) => self.reveal_transcript(PathBuf::from(path), cx),
            None => self.set_notice("This agent did not report a transcript", cx),
        }
    }
}

/// The action a row's main button runs, for the page and the panel alike.
pub fn primary_action(session: SessionId, primary: agents_text::Primary) -> Box<dyn gpui::Action> {
    use agents_text::Primary;
    match primary {
        // The decision itself is taken on the page: the request, with its buttons, expanded.
        Primary::ReviewRequest => Box::new(actions::OpenAgentsPageWith(AgentsRequest {
            session: Some(session.to_string()),
            review: true,
            ..AgentsRequest::default()
        })),
        Primary::AnswerQuestion | Primary::OpenSession | Primary::OpenMawas => {
            Box::new(actions::OpenSessionFromOverview(session.to_string()))
        }
        Primary::ReviewChanges => Box::new(actions::ShowAgentChanges(session.to_string())),
    }
}
