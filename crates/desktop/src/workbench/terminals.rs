//! Workbench behavior around live sessions: terminal items, session lifecycle commands, focus
//! and attention, and the hot-reload reactions of settings and keymap.

use crate::theme::DensityExt as _;
use std::collections::HashSet;
use std::path::PathBuf;

use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, Focusable as _, IntoElement,
    ParentElement, Styled, Window, div, prelude::*, px,
};
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::messages::{SessionInfo, TerminalSpec};
use hutan_settings::{ChangeSet, KEYMAP_NAMESPACE};
use hutan_terminal_view::{
    DefaultColors, SharedEvent, SharedTerminal, TerminalView, TerminalViewEvent, ViewMode,
};

use super::actions;
use super::modal::PromptSpec;
use super::model::{Edge, FocusTarget, ItemRef, PaneId};
use super::panels::SessionsPanel;
use super::registry::StatusBarLayout;
use super::style::{TerminalStyle, UiStyle};
use super::ui::{self, Mascot};
use super::view::{MenuEntry, Region, Workbench};
use crate::sessions::TerminalSession;
use crate::settings::{AppearanceSettings, KeymapSettings, LayoutSettings, TerminalSettings};
use crate::theme::Theme;

/// The view configuration of a terminal: the `terminal` namespace with the pack's palette and
/// block roles, the pack's font defaults where the user set none, plus what follows the density
/// (padding, block gutter).
pub(crate) fn terminal_style(
    settings: &TerminalSettings,
    terminal: &hutan_settings::TerminalTheme,
    pack: &hutan_settings::themes::ResolvedTheme,
    mono_family: String,
    explicit: crate::settings::TerminalExplicit,
    density: hutan_settings::Density,
) -> hutan_terminal_view::TerminalViewSettings {
    let defaults = crate::settings::TerminalPackDefaults::of(pack, mono_family);
    let base = settings.view_settings(terminal, &defaults, explicit);
    hutan_terminal_view::TerminalViewSettings {
        padding: density.ui().terminal_padding,
        blocks: crate::theme::block_settings(
            terminal,
            density,
            base.blocks.enabled,
            !super::ui::motion::reduce_motion(),
        ),
        find: hutan_terminal_view::FindSettings::default().with_colors(&pack.editor()),
        ..base
    }
}

/// The pack's monospace family: the first of its `[fonts] mono` list that is installed.
pub(crate) fn mono_family(cx: &gpui::App, pack: &hutan_settings::themes::ResolvedTheme) -> String {
    ui::pick_family(
        cx,
        pack.fonts().role(hutan_settings::themes::FontRole::Mono),
    )
    .to_string()
}

/// A terminal tab's view; the attachment it draws from lives in [`Workbench::held`].
#[derive(Debug)]
pub(crate) struct TerminalItem {
    pub view: Entity<TerminalView>,
    /// What the view tells the workbench ("Send to agent").
    pub(crate) _events: gpui::Subscription,
}

/// An open find bar and what ends it.
#[derive(Debug)]
pub(crate) struct FindItem {
    pub find: Entity<super::terminal_find::TerminalFind>,
    pub(crate) _closed: gpui::Subscription,
}

/// The session an item shows; for a tab of terminal splits, the focused split's.
pub fn session_of_item(item: &ItemRef) -> Option<SessionId> {
    item.session().and_then(|session| session.parse().ok())
}

/// Every session a terminal tab shows (one per split).
pub fn terminal_sessions_of_item(item: &ItemRef) -> Vec<SessionId> {
    match item {
        ItemRef::Terminal { .. } | ItemRef::TerminalSplit { .. } => item
            .sessions()
            .into_iter()
            .filter_map(|session| session.parse().ok())
            .collect(),
        _ => Vec::new(),
    }
}

impl Workbench {
    // ---- items ------------------------------------------------------------------------------

    /// Every `(pane, session)` that has a terminal tab, in tree order.
    fn open_terminal_items(&self) -> Vec<(PaneId, SessionId)> {
        self.state
            .center
            .panes()
            .into_iter()
            .flat_map(|pane| {
                pane.items.iter().flat_map(move |item| {
                    terminal_sessions_of_item(item)
                        .into_iter()
                        .map(move |session| (pane.id, session))
                })
            })
            .collect()
    }

    /// The session of the focused item (a terminal tab in the focused pane), if any.
    pub fn focused_session(&self) -> Option<SessionId> {
        match self.state.focus {
            FocusTarget::Pane(_) => self
                .state
                .active_pane()
                .and_then(|p| p.active_item())
                .and_then(session_of_item),
            FocusTarget::Dock(_) => None,
        }
    }

    /// The checkout of the Changes view in the focused pane's active tab, if that is one.
    fn focused_changes_path(&self) -> Option<std::path::PathBuf> {
        match self.state.focus {
            FocusTarget::Pane(_) => match self.state.active_pane()?.active_item()? {
                ItemRef::Changes { path } => Some(std::path::PathBuf::from(path)),
                _ => None,
            },
            FocusTarget::Dock(_) => None,
        }
    }

    /// Attaches every open terminal item whose session exists and is not attached yet (restoring
    /// a layout re-attaches all of them), and detaches the ones no tab shows anymore.
    pub(crate) fn attach_open_items(&mut self, cx: &mut Context<Self>) {
        let items = self.open_terminal_items();
        // Sessions hidden in the overview's stash stay attached (paced at zero cost).
        let mut wanted: HashSet<SessionId> = items.iter().map(|(_, s)| *s).collect();
        if let Some(saved) = &self.state.stashed {
            wanted.extend(
                saved
                    .center
                    .panes()
                    .into_iter()
                    .flat_map(|p| p.items.iter().flat_map(terminal_sessions_of_item)),
            );
        }
        self.held.retain(|session, _| wanted.contains(session));
        self.collapse_watch
            .retain(|session, _| wanted.contains(session));
        let keep: HashSet<(PaneId, SessionId)> = items.into_iter().collect();
        let gone: Vec<(PaneId, SessionId)> = self
            .terminals
            .keys()
            .filter(|key| !keep.contains(*key))
            .copied()
            .collect();
        for key in gone {
            self.terminal_finds.remove(&key);
            if let Some(item) = self.terminals.remove(&key) {
                // Focus may still sit on it; `rescue_orphaned_focus` checks before it is dropped.
                self.pruned.push(item.view);
            }
        }
        for session in wanted {
            self.ensure_held(session, cx);
        }
        self.sync_chats(cx);
        self.sync_files(cx);
        self.sync_history_views();
    }

    fn ensure_held(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalSession>> {
        if let Some(held) = self.held.get(&session) {
            return Some(held.clone());
        }
        let attached = self
            .sessions
            .update(cx, |store, cx| store.terminal_session(session, cx))?;
        // Images pasted into the terminal are saved under the data folder.
        let data_folder = self
            .settings
            .paths()
            .and_then(|paths| paths.user.parent().map(std::path::Path::to_path_buf));
        if let Some(data_folder) = data_folder {
            attached.update(cx, |attached, _| {
                attached.set_scratch_root(Some(data_folder))
            });
        }
        self.held.insert(session, attached.clone());
        self.watch_collapsed(session, &attached, cx);
        Some(attached)
    }

    /// Restores the folded blocks saved for `attached`'s terminal and saves the changes the
    /// user makes from now on.
    fn watch_collapsed(
        &mut self,
        session: SessionId,
        attached: &Entity<TerminalSession>,
        cx: &mut Context<Self>,
    ) {
        let (shared, terminal) = {
            let attached = attached.read(cx);
            (attached.shared().clone(), attached.terminal())
        };
        if let Some(ids) = terminal.and_then(|t| self.collapsed_blocks.get(&t.to_string())) {
            let ids: std::collections::BTreeSet<u64> = ids.iter().copied().collect();
            shared.update(cx, |shared, cx| shared.set_collapsed(ids, cx));
        }
        let watch = cx.subscribe(&shared, move |this, shared, event: &SharedEvent, cx| {
            if *event == SharedEvent::Collapsed {
                this.collapsed_changed(session, &shared, cx);
            }
        });
        self.collapse_watch.insert(session, watch);
    }

    fn collapsed_changed(
        &mut self,
        session: SessionId,
        shared: &Entity<SharedTerminal>,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.held.get(&session).and_then(|h| h.read(cx).terminal()) else {
            return;
        };
        let ids: Vec<u64> = shared
            .read(cx)
            .model()
            .collapsed()
            .iter()
            .copied()
            .collect();
        let key = terminal.to_string();
        let changed = if ids.is_empty() {
            self.collapsed_blocks.remove(&key).is_some()
        } else {
            self.collapsed_blocks.insert(key, ids.clone()).as_ref() != Some(&ids)
        };
        if changed {
            self.schedule_save(cx);
        }
    }

    /// Forgets the folds of terminals the daemon no longer has (once it has said what it has).
    fn prune_collapsed(&mut self, cx: &mut Context<Self>) {
        let live: HashSet<String> = {
            let store = self.sessions.read(cx);
            if !store.synced()
                || store.connection_state() != hutan_client::ConnectionState::Connected
            {
                return;
            }
            store
                .sessions()
                .iter()
                .filter_map(|s| s.terminal_id)
                .map(|t| t.to_string())
                .collect()
        };
        let before = self.collapsed_blocks.len();
        self.collapsed_blocks.retain(|key, _| live.contains(key));
        if self.collapsed_blocks.len() != before {
            self.schedule_save(cx);
        }
    }

    /// The view of `session` in `pane`, created on first use.
    pub(crate) fn terminal_view(
        &mut self,
        pane: PaneId,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalView>> {
        if let Some(item) = self.terminals.get(&(pane, session)) {
            return Some(item.view.clone());
        }
        let attached = self.ensure_held(session, cx)?;
        let (shared, backend) = {
            let attached = attached.read(cx);
            (attached.shared().clone(), attached.backend())
        };
        let settings = self.terminal_style.clone();
        let view = cx.new(|cx| {
            TerminalView::with_shared(shared, backend, settings, ViewMode::Interactive, window, cx)
        });
        let events = cx.subscribe_in(
            &view,
            window,
            move |this, _, event, window, cx| match event {
                TerminalViewEvent::SendBlockToAgent { block } => {
                    this.send_block_to_agent(session, *block, window, cx);
                }
                TerminalViewEvent::OpenLink(target) => this.open_terminal_link(target, window, cx),
                TerminalViewEvent::Notice(text) => this.set_notice(text.clone(), cx),
                TerminalViewEvent::ImageSaved(path) => this.show_paste_toast(path.clone(), cx),
                TerminalViewEvent::TitleChanged(_) => {}
            },
        );
        self.terminals.insert(
            (pane, session),
            TerminalItem {
                view: view.clone(),
                _events: events,
            },
        );
        Some(view)
    }

    /// The view of the focused pane's active tab, if it is a terminal.
    pub fn focused_terminal_view(&self) -> Option<Entity<TerminalView>> {
        let FocusTarget::Pane(_) = self.state.focus else {
            return None;
        };
        let pane = self.state.active_pane_id();
        let session = self.focused_session()?;
        self.terminals.get(&(pane, session)).map(|t| t.view.clone())
    }

    /// ⌘F with the keyboard in a terminal: opens its find bar (or gives the open one the keyboard
    /// again). Returns whether a terminal had the keyboard.
    pub(crate) fn open_terminal_find(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let FocusTarget::Pane(_) = self.state.focus else {
            return false;
        };
        let pane = self.state.active_pane_id();
        let Some(session) = self.focused_session() else {
            return false;
        };
        let Some(view) = self.terminals.get(&(pane, session)).map(|t| t.view.clone()) else {
            return false;
        };
        let key = (pane, session);
        if let Some(item) = self.terminal_finds.get(&key) {
            let find = item.find.clone();
            find.update(cx, |find, cx| find.open(window, cx));
            return true;
        }
        let Some(attached) = self.held.get(&session).cloned() else {
            return false;
        };
        // One bar per session: a second pane showing it takes the bar over.
        self.terminal_finds.retain(|(_, s), _| *s != session);
        let settings = self.settings.clone();
        let find =
            cx.new(|cx| super::terminal_find::TerminalFind::new(view, attached, settings, cx));
        find.update(cx, |find, _| find.remember(self.last_terminal_find.clone()));
        let closed = cx.subscribe(&find, move |this, _, event, cx| match event {
            super::terminal_find::TerminalFindEvent::Closed(memory) => {
                this.last_terminal_find = memory.clone();
                this.terminal_finds.remove(&key);
                cx.notify();
            }
        });
        self.terminal_finds.insert(
            key,
            FindItem {
                find: find.clone(),
                _closed: closed,
            },
        );
        find.update(cx, |find, cx| find.open(window, cx));
        cx.notify();
        true
    }

    /// The find bar of the focused pane's terminal, when it is open.
    pub fn focused_terminal_find(&self) -> Option<Entity<super::terminal_find::TerminalFind>> {
        let FocusTarget::Pane(_) = self.state.focus else {
            return None;
        };
        let pane = self.state.active_pane_id();
        let session = self.focused_session()?;
        self.terminal_finds
            .get(&(pane, session))
            .map(|item| item.find.clone())
    }

    /// How many find bars are open.
    pub fn terminal_find_count(&self) -> usize {
        self.terminal_finds.len()
    }

    /// ⌘G / ⌘⇧G: the next / previous match of the open find bar.
    pub(crate) fn step_terminal_find(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(find) = self.focused_terminal_find() {
            find.update(cx, |find, cx| find.step(forward, cx));
        }
    }

    /// The view of `session` in any pane (tests, inspection).
    pub fn terminal_view_of(&self, session: SessionId) -> Option<Entity<TerminalView>> {
        self.terminals
            .iter()
            .find(|((_, s), _)| *s == session)
            .map(|(_, t)| t.view.clone())
    }

    /// Whether `session` is attached because some tab holds it.
    pub fn holds(&self, session: SessionId) -> bool {
        self.held.contains_key(&session)
    }

    /// Moves keyboard focus to what the focused pane shows: its terminal, else the workbench.
    pub(crate) fn focus_root(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let FocusTarget::Pane(_) = self.state.focus
            && self.focus_chat(window, cx)
        {
            return;
        }
        if let FocusTarget::Pane(_) = self.state.focus
            && let Some(view) = self.focused_file_view()
        {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        if let FocusTarget::Pane(_) = self.state.focus
            && let Some(session) = self.focused_session()
        {
            let pane = self.state.active_pane_id();
            if let Some(view) = self.terminal_view(pane, session, window, cx) {
                window.focus(&view.focus_handle(cx), cx);
                return;
            }
        }
        window.focus(&self.focus_handle, cx);
    }

    /// A terminal view that had focus but is no longer drawn (its tab was switched away, its pane
    /// closed, the overview replaced the center) leaves keystrokes without a dispatch path.
    /// Moves focus to whatever the focused pane shows now.
    pub(crate) fn rescue_orphaned_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(focused) = window.focused(cx) else {
            // Nothing holds the keyboard: actions panels dispatch (row menus, …) start from the
            // focused element, and without one they would never reach the workbench.
            self.focus_root(window, cx);
            return;
        };
        let orphaned = self.terminals.iter().any(|(key, item)| {
            !self.rendered_terminals.contains(key) && item.view.focus_handle(cx) == focused
        }) || self
            .pruned
            .iter()
            .any(|view| view.focus_handle(cx) == focused)
            || self.chats.iter().any(|(key, view)| {
                !self.rendered_chats.contains(key) && view.focus_handle(cx) == focused
            })
            || self
                .pruned_chats
                .iter()
                .any(|view| view.focus_handle(cx) == focused)
            || self.file_views.iter().any(|(key, view)| {
                !self.rendered_files.contains(key) && view.focus_handle(cx) == focused
            })
            || self
                .pruned_files
                .iter()
                .any(|view| view.focus_handle(cx) == focused);
        self.pruned.clear();
        self.pruned_chats.clear();
        self.pruned_files.clear();
        if orphaned {
            self.focus_root(window, cx);
        }
    }

    // ---- open / new -------------------------------------------------------------------------

    /// The tab kind that shows `session`: a chat for an agent conversation, a terminal else.
    fn item_for(&self, session: SessionId, cx: &gpui::App) -> ItemRef {
        if self.is_chat(session, cx) {
            ItemRef::Chat {
                session: session.to_string(),
            }
        } else {
            ItemRef::Terminal {
                session: session.to_string(),
            }
        }
    }

    /// Shows `session` in the focused pane: focuses its tab when open, else adds a tab (a pane
    /// that only holds a placeholder gives the placeholder's place to the session). One session
    /// is one tab: a Mawas conversation is shown on Mawas Home (its only tab, whichever surface
    /// asked), never in a chat tab of its own.
    pub fn open_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.show_assistant_session(session, window, cx) {
            return;
        }
        let item = self.item_for(session, cx);
        // A session of another workspace brings that workspace's layout along.
        self.show_workspace_of(session, cx);
        let id = session.to_string();
        if let Some((pane, index)) = self
            .state
            .find_item(&item)
            .or_else(|| self.state.find_session_tab(&id))
        {
            self.try_mutate(cx, |s| {
                s.set_active_item(pane, index)?;
                // A split of a tab of splits takes the keyboard inside its tab.
                let _ = s.focus_term_split(pane, index, &id);
                s.focus_pane(pane)
            });
        } else {
            let pane = self.state.active_pane_id();
            let placeholder = self
                .state
                .pane(pane)
                .and_then(|p| match p.items.as_slice() {
                    [only @ ItemRef::Placeholder { .. }] => Some(only.clone()),
                    _ => None,
                });
            self.try_mutate(cx, |s| {
                if let Some(placeholder) = &placeholder
                    && s.replace_item(placeholder, item.clone())
                {
                    return s.focus_pane(pane);
                }
                s.add_item(pane, item.clone()).map(|_| ())
            });
        }
        self.focus_root(window, cx);
    }

    /// Opens `session` in a new split at `edge` of the focused pane.
    pub fn open_session_in_split(
        &mut self,
        session: SessionId,
        edge: Edge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A Mawas conversation has no split of its own: Home is its tab.
        if self.show_assistant_session(session, window, cx) {
            return;
        }
        self.show_workspace_of(session, cx);
        let pane = self.state.active_pane_id();
        let item = self.item_for(session, cx);
        self.try_mutate(cx, |s| s.split_pane(pane, edge, item));
        self.focus_root(window, cx);
    }

    /// Leaves the overview (restoring the layout it replaced) and focuses `session`.
    pub fn open_session_from_overview(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.preset == crate::workbench::model::LayoutPreset::OVERVIEW {
            self.toggle_overview(cx);
        }
        self.open_session(session, window, cx);
    }

    /// What a new terminal in `workspace` starts with: `cwd`, and the `terminal.shell` and
    /// `terminal.env` settings as resolved for that project (its `.hutan/settings.toml` over
    /// its workspace's over the user's), which need not be the one on screen.
    pub(crate) fn new_terminal_spec(
        &self,
        workspace: Option<WorkspaceId>,
        cwd: Option<PathBuf>,
        cx: &gpui::App,
    ) -> TerminalSpec {
        let (workspace_root, project_root) = workspace.map_or((None, None), |id| {
            super::workspaces::layer_roots_of(self.sessions.read(cx), id)
        });
        let mut spec = TerminalSpec {
            cwd,
            ..TerminalSpec::default()
        };
        self.settings
            .get_for_roots::<TerminalSettings>(workspace_root.as_deref(), project_root.as_deref())
            .apply_to_spec(&mut spec);
        spec
    }

    /// Starts a terminal in the active workspace (Home when none is shown), in its first root
    /// or `$HOME`, and opens it.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (workspace, cwd) = {
            let store = self.sessions.read(cx);
            let workspace = self.target_workspace(store);
            let cwd = workspace
                .and_then(|w| w.roots.first().cloned())
                .or_else(std::env::home_dir);
            (workspace.map(|w| w.id), cwd)
        };
        self.start_terminal_in(workspace, cwd, window, cx);
    }

    /// Starts a terminal of `workspace` in `cwd` and opens it.
    pub(crate) fn start_terminal_in(
        &mut self,
        workspace: Option<WorkspaceId>,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let spec = self.new_terminal_spec(workspace, cwd, cx);
        let task = self
            .sessions
            .update(cx, |store, cx| store.create_session(workspace, spec, cx));
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(info) => {
                this.update_in(cx, |this, window, cx| {
                    this.open_session(info.id, window, cx)
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

    /// Where a terminal started from `pane` begins: the pane's terminal session's current
    /// directory (it follows OSC 7) in its workspace; for any other item the active
    /// workspace's root (Home when none is shown), else `$HOME`.
    fn split_target(
        &self,
        pane: PaneId,
        cx: &gpui::App,
    ) -> (Option<hutan_core::WorkspaceId>, Option<PathBuf>) {
        let session = self
            .state
            .pane(pane)
            .and_then(|p| p.active_item())
            .and_then(session_of_item);
        self.session_split_target(session, cx)
    }

    /// Where a terminal started next to `session` begins: its current directory (it follows
    /// OSC 7) in its workspace; without a known session the active workspace's root (Home when
    /// none is shown), else `$HOME`.
    pub(crate) fn session_split_target(
        &self,
        session: Option<SessionId>,
        cx: &gpui::App,
    ) -> (Option<hutan_core::WorkspaceId>, Option<PathBuf>) {
        let store = self.sessions.read(cx);
        let home_or_root = |workspace: Option<&hutan_protocol::messages::WorkspaceInfo>| {
            workspace
                .and_then(|w| w.roots.first().cloned())
                .or_else(std::env::home_dir)
        };
        let info = session.and_then(|id| store.session(id));
        match info {
            Some(info) => {
                let workspace = store.workspace(info.workspace_id);
                (
                    Some(info.workspace_id),
                    info.cwd.clone().or_else(|| home_or_root(workspace)),
                )
            }
            None => {
                let workspace = self.target_workspace(store);
                (workspace.map(|w| w.id), home_or_root(workspace))
            }
        }
    }

    /// Splits `pane` at `edge` and starts a new terminal in the new pane (iTerm / Ghostty
    /// style). Without a daemon connection the new pane is an empty placeholder and a toast says
    /// why.
    pub fn split_pane_with_terminal(
        &mut self,
        pane: PaneId,
        edge: Edge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_pane_with_terminal_in(pane, edge, None, window, cx);
    }

    /// [`split_pane_with_terminal`](Self::split_pane_with_terminal), the terminal starting in
    /// `dir` when given (Vim's `:term` from a file: the file's folder).
    pub(crate) fn split_pane_with_terminal_in(
        &mut self,
        pane: PaneId,
        edge: Edge,
        dir: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sessions.read(cx).connection_state() != hutan_client::ConnectionState::Connected {
            self.split_with_placeholder(pane, edge, cx);
            self.set_notice("Not connected to hutand: opened an empty pane", cx);
            return;
        }
        let (workspace, cwd) = self.split_target(pane, cx);
        let spec = self.new_terminal_spec(workspace, dir.or(cwd), cx);
        let task = self
            .sessions
            .update(cx, |store, cx| store.create_session(workspace, spec, cx));
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(info) => {
                this.update_in(cx, |this, window, cx| {
                    // The pane may have closed meanwhile; then split the focused one.
                    let pane = if this.state.pane(pane).is_some() {
                        pane
                    } else {
                        this.state.active_pane_id()
                    };
                    if this.state.focus != FocusTarget::Pane(pane) {
                        this.try_mutate(cx, |s| s.focus_pane(pane));
                    }
                    this.open_session_in_split(info.id, edge, window, cx);
                })
                .ok();
            }
            Err(err) => {
                this.update(cx, |this, cx| {
                    if this.state.pane(pane).is_some() {
                        this.split_with_placeholder(pane, edge, cx);
                    }
                    this.set_notice(format!("Cannot start a terminal: {err}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    // ---- lifecycle --------------------------------------------------------------------------

    fn info_of(&self, session: SessionId, cx: &gpui::App) -> Option<SessionInfo> {
        self.sessions.read(cx).session(session).cloned()
    }

    fn report(
        &mut self,
        what: &'static str,
        task: gpui::Task<Result<SessionInfo, String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            if let Err(err) = task.await {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot {what}: {err}"), cx)
                })
                .ok();
            }
        })
        .detach();
    }

    /// Stops `session`; asks first while it is running.
    pub fn stop_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.info_of(session, cx) else {
            return;
        };
        if !info.state.is_live() {
            self.set_notice("That session already ended", cx);
            return;
        }
        let title = info.title.clone();
        self.open_destructive(
            "Stop session?",
            format!(
                "“{title}” is running. Stopping ends its process; closing the tab only detaches."
            ),
            "Stop",
            move |this, window, cx| {
                let task = this
                    .sessions
                    .update(cx, |store, cx| store.stop(session, cx));
                this.report("stop the session", task, window, cx);
            },
            window,
            cx,
        );
    }

    pub fn restart_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.restart(session, cx));
        self.report("restart the session", task, window, cx);
    }

    /// `session.rename`: the answer to the sidebar's inline edit.
    pub fn rename_session_to(
        &mut self,
        session: SessionId,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The inline editor is gone: the workbench takes the keyboard back.
        self.focus_root(window, cx);
        let title = title.trim().to_owned();
        if title.is_empty() {
            return;
        }
        let task = self
            .sessions
            .update(cx, |store, cx| store.rename(session, title, cx));
        self.report("rename the session", task, window, cx);
    }

    pub fn rename_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.info_of(session, cx) else {
            return;
        };
        // Where the session's row is on screen, its title becomes a field right there.
        if self.region_bounds(Region::SessionRow(session)).is_some()
            && let Some(panel) = self.panel::<SessionsPanel>("sessions")
            && panel.update(cx, |panel, cx| {
                panel.begin_session_rename(session, window, cx)
            })
        {
            return;
        }
        self.open_prompt(
            PromptSpec {
                title: "Rename session",
                message: "New title:",
                initial: &info.title,
                confirm_label: "Rename",
            },
            move |this, title, window, cx| {
                let task = this.sessions.update(cx, |store, cx| {
                    store.rename(session, title.trim().to_owned(), cx)
                });
                this.report("rename the session", task, window, cx);
            },
            window,
            cx,
        );
        self.select_modal_input(cx);
    }

    pub fn copy_session_id(&mut self, session: SessionId, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(session.to_string()));
        self.set_notice("Copied the session id", cx);
    }

    /// Shows `session`, scrolls its terminal to `block` and highlights it (the inspector's
    /// block list).
    pub fn jump_to_block(
        &mut self,
        session: &str,
        block: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(session) = session.parse::<SessionId>() else {
            return;
        };
        self.open_session(session, window, cx);
        if let Some(view) = self.terminal_view_of(session) {
            view.update(cx, |view, cx| {
                view.jump_to_block(block, cx);
            });
        }
    }

    /// Focuses the next session that wants the user: a request first, then a failure, then work
    /// to review.
    pub fn jump_to_next_attention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.focused_session();
        let next = self.sessions.read(cx).next_needing_attention(current);
        match next {
            Some(session) => self.open_session(session, window, cx),
            None => self.set_notice("No requests, failures or work to review", cx),
        }
    }

    /// *Mark as unread* / *Mark as read* and *Snooze until it changes* / *Unsnooze*, the two
    /// entries of the row `More` menu and the sidebar context menu (`UX-ATTENTION-CONTROLS`).
    /// A session that does not need the user cannot be snoozed, and the entry says so. With a
    /// daemon that has `notify.mute` (protocol 0.34) a third: *Mute notifications* /
    /// *Unmute notifications*.
    pub(crate) fn attention_entries(&self, session: SessionId, cx: &gpui::App) -> Vec<MenuEntry> {
        let store = self.sessions.read(cx);
        let unread = store.unread(session).is_some();
        let snoozed = store.is_snoozed(session);
        let wants_you = store
            .session(session)
            .is_some_and(|info| store.presentation(info).attention.is_some());
        let read = if unread {
            Self::entry("Mark as read", move |this, _, cx| {
                this.sessions
                    .update(cx, |store, cx| store.mark_read(session, cx));
            })
        } else {
            Self::entry("Mark as unread", move |this, _, cx| {
                this.sessions.update(cx, |store, cx| {
                    store.mark_unread(session, cx);
                });
            })
        };
        let snooze = if snoozed {
            Self::entry("Unsnooze", move |this, _, cx| {
                this.sessions
                    .update(cx, |store, cx| store.unsnooze(session, cx));
            })
        } else if wants_you {
            Self::entry("Snooze until it changes", move |this, _, cx| {
                this.sessions.update(cx, |store, cx| {
                    store.snooze(session, cx);
                });
            })
        } else {
            MenuEntry {
                label: "Snooze until it changes (nothing waits for you here)".into(),
                disabled: true,
                ..Self::entry("Snooze until it changes", |_, _, _| {})
            }
        };
        let mut entries = vec![read, snooze];
        if store.supports_mute() {
            let muted = store.is_muted(session);
            let label = if muted {
                "Unmute notifications"
            } else {
                "Mute notifications"
            };
            entries.push(Self::entry(label, move |this, _, cx| {
                this.set_session_muted(session, !muted, cx);
            }));
        }
        entries
    }

    /// *Mute notifications* / *Unmute notifications* (`notify.mute`); a refusal is said in the
    /// status line.
    pub(crate) fn set_session_muted(
        &mut self,
        session: SessionId,
        muted: bool,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.set_muted(session, muted, cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Could not change the notifications: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Saves what the store holds of the attention marks (`attention` in the state file) when it
    /// differs from the last saved copy.
    pub(crate) fn sync_attention_marks(&mut self, cx: &mut Context<Self>) {
        let marks = self.sessions.read(cx).marks();
        if marks != self.attention_marks {
            self.attention_marks = marks;
            self.schedule_save(cx);
        }
    }

    /// Context menu of a session (sessions panel, overview card, tab).
    pub(crate) fn session_menu(&self, session: SessionId, cx: &gpui::App) -> Vec<MenuEntry> {
        let mut entries = vec![
            Self::entry("Open", move |this, window, cx| {
                this.open_session(session, window, cx);
            }),
            Self::entry("Open in Split Right", move |this, window, cx| {
                this.open_session_in_split(session, Edge::Right, window, cx);
            }),
            Self::entry("Open in Split Down", move |this, window, cx| {
                this.open_session_in_split(session, Edge::Bottom, window, cx);
            }),
            Self::entry("Rename…", move |this, window, cx| {
                this.rename_session(session, window, cx);
            }),
            Self::entry("Restart", move |this, window, cx| {
                this.restart_session(session, window, cx);
            }),
            Self::entry("Stop…", move |this, window, cx| {
                this.stop_session(session, window, cx);
            }),
            Self::entry("Close session…", move |this, window, cx| {
                this.close_session(session, window, cx);
            }),
            Self::entry("Show in Resources", move |this, window, cx| {
                this.show_in_resources(session, window, cx);
            }),
            Self::entry("Copy Session ID", move |this, _, cx| {
                this.copy_session_id(session, cx);
            }),
        ];
        if let Some(review) = self.review_entry(session, cx) {
            entries.insert(entries.len() - 1, review);
        }
        entries.insert(entries.len() - 1, Self::open_file_entry());
        // A chat's checkpoints (or, once, why it has none), before Copy Session ID.
        if let Some(checkpoints) = self.checkpoints_menu_entry(session, cx) {
            entries.insert(entries.len() - 1, checkpoints);
        }
        // After the openers, before Rename…: what the user can do to the queue.
        for (at, entry) in self.attention_entries(session, cx).into_iter().enumerate() {
            entries.insert(3 + at, entry);
        }
        entries
    }

    // ---- attention & focus ------------------------------------------------------------------

    /// Tells the sessions store which session the user is looking at (a focused terminal tab in
    /// an active window): that session's unread attention is read. The workspace whose settings
    /// apply is the active one (see `workspaces.rs`).
    pub(crate) fn sync_session_focus(&mut self, cx: &mut Context<Self>) {
        let focused = if self.window_active {
            self.focused_session()
        } else {
            None
        };
        // A Changes view on screen shows the work of the sessions of its checkout.
        let changes = self
            .window_active
            .then(|| self.focused_changes_path())
            .flatten();
        // The other splits of a tab of splits are on screen too: seen as well.
        let visible: Vec<SessionId> = match (self.window_active, self.state.focus) {
            (true, FocusTarget::Pane(_)) => self
                .state
                .active_pane()
                .and_then(|p| p.active_item())
                .map(|item| {
                    item.visible_sessions()
                        .into_iter()
                        .filter_map(|s| s.parse().ok())
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        self.note_focused_chat(cx);
        self.sessions.update(cx, |store, cx| {
            if !self.window_active {
                // The panel's composer keeps its keyboard focus in a window nobody uses.
                store.set_assistant_focused(None, cx);
            }
            store.set_focused(focused, cx);
            if let Some(session) = focused {
                store.mark_looked_at(session, cx);
            }
            for session in visible {
                if Some(session) != focused {
                    store.mark_looked_at(session, cx);
                }
            }
            if let Some(root) = &changes {
                store.mark_checkout_looked_at(root, cx);
            }
        });
        self.sync_workspace(cx);
        self.sync_mawas_focus(cx);
    }

    pub(crate) fn on_sessions_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.held.len();
        self.resolve_active_workspace(cx);
        self.merge_session_tabs(window, cx);
        self.attach_open_items(cx);
        self.prune_collapsed(cx);
        self.sync_session_focus(cx);
        let nothing_focused = window.focused(cx).is_none() || self.focus_handle.is_focused(window);
        if nothing_focused && self.held.len() != before {
            self.focus_root(window, cx);
        }
        self.on_daemon_state_changed(cx);
        self.check_harness_installs(cx);
        self.on_approvals_changed(cx);
        // A Rewind sheet that waited on a running turn asks again once it ended.
        self.recheck_rewind_busy(cx);
        cx.notify();
    }

    /// The root whose `.hutan/settings.toml` is the workspace layer: the first project of the
    /// active workspace, or Home's folder; none while a project in no workspace is shown.
    pub fn workspace_root(&self) -> Option<&PathBuf> {
        self.workspace_root.as_ref()
    }

    /// The root whose `.hutan/settings.toml` is the project layer: the project the user works in.
    pub fn project_root(&self) -> Option<&PathBuf> {
        self.project_root.as_ref()
    }

    // ---- rendering --------------------------------------------------------------------------

    /// Body of a terminal tab: the terminal view with Restart / Close when it ended, or a
    /// placeholder while the session is unknown.
    pub(crate) fn render_terminal_item(
        &mut self,
        pane: PaneId,
        session: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let Ok(id) = session.parse::<SessionId>() else {
            return self.placeholder_body(format!("Invalid session id {session}"), cx);
        };
        let (known, synced, live) = {
            let store = self.sessions.read(cx);
            let info = store.session(id);
            (
                info.is_some(),
                store.synced(),
                info.is_some_and(|i| i.state.is_live()),
            )
        };
        self.rendered_terminals.insert((pane, id));
        let Some(view) = self.terminal_view(pane, id, window, cx) else {
            let stopped = self.daemon.stopped;
            let text = if stopped {
                "Daemon stopped"
            } else if known {
                "Starting…"
            } else if synced {
                "This session no longer exists. Close the tab."
            } else {
                "Connecting to hutand…"
            };
            let mascot = if stopped {
                Mascot::Sleep
            } else if known || !synced {
                Mascot::Thinking
            } else {
                Mascot::Empty
            };
            // While the daemon is stopped the status bar says so and starts it (one message).
            let action = if stopped {
                None
            } else if !known && synced {
                Some(
                    self.action_button(
                        "ended-close",
                        "Close",
                        ui::ButtonKind::Secondary,
                        cx,
                        move |this, _, cx| {
                            this.close_session_tabs(id, cx);
                        },
                    )
                    .into_any_element(),
                )
            } else {
                None
            };
            return ui::empty_state(mascot, text, action, &theme, &tokens).into_any_element();
        };
        let ended = self.held.get(&id).is_some_and(|h| {
            matches!(
                h.read(cx).status(),
                hutan_terminal_view::ViewStatus::Ended(_)
            )
        });
        let session_id = session.to_owned();
        let menu_view = view.clone();
        let mut root = div()
            .relative()
            .size_full()
            // Right-click: Split Right / Split Down / Close Split. A program that handles the
            // mouse itself keeps the press (⇧ takes it back).
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    if menu_view.read(cx).reports_mouse(event.modifiers) {
                        return;
                    }
                    let entries = this.terminal_menu(&session_id);
                    this.open_menu(event.position, entries, window, cx);
                }),
            )
            .child(super::render::cached_view(&self.regions, view));
        // A stopped daemon is said once, by the status bar's `Stopped · Start`; a terminal does
        // not repeat it with a banner of its own.
        if ended && !self.daemon.stopped {
            let mut bar = div()
                .absolute()
                .top(px(tokens.spacing * 2.0))
                .right(px(tokens.spacing * 3.0))
                .flex()
                .gap(px(tokens.spacing * 2.0));
            bar = bar.child(self.action_button(
                "ended-close",
                "Close",
                ui::ButtonKind::Secondary,
                cx,
                move |this, _, cx| {
                    this.close_session_tabs(id, cx);
                },
            ));
            if known && !live {
                bar = bar.child(self.action_button(
                    "ended-restart",
                    "Restart",
                    ui::ButtonKind::Primary,
                    cx,
                    move |this, window, cx| {
                        this.restart_session(id, window, cx);
                    },
                ));
            }
            root = root.child(bar);
        }
        if let Some(item) = self.terminal_finds.get(&(pane, id)) {
            // Below the Close / Restart buttons of an ended session.
            let top = if ended && !self.daemon.stopped {
                tokens.spacing * 2.0 + 36.0
            } else {
                tokens.spacing * 2.0
            };
            root = root.child(
                div()
                    .absolute()
                    .top(px(top))
                    .right(px(tokens.spacing * 4.0))
                    .child(item.find.clone()),
            );
        }
        if !self.daemon.stopped
            && let Some(bar) = self.render_terminal_recovery(id, ended, cx)
        {
            root = root.child(bar);
        }
        if !self.daemon.stopped
            && let Some(strip) = self.render_terminal_untracked(id, cx)
        {
            root = root.child(strip);
        }
        root.into_any_element()
    }

    /// What the last daemon restart did to this terminal, until the user dismisses it or types:
    /// the chip, one sentence, and Dismiss. Below the Restart / Close bar of an ended session.
    fn render_terminal_recovery(
        &mut self,
        id: SessionId,
        ended: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let info = self.sessions.read(cx).session(id).cloned()?;
        let chip = hutan_client::recovery::chip(&info)?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let chip_element = crate::workbench::panels::recovery_chip(
            &theme,
            &chip,
            crate::workbench::BadgeSite::Tab,
            id,
            false,
            &self.regions,
            cx,
        );
        let dismiss = self
            .action_button(
                "recovery-dismiss",
                "Dismiss",
                ui::ButtonKind::Ghost,
                cx,
                move |this, _, cx| {
                    let ack = this
                        .sessions
                        .update(cx, |store, cx| store.ack_recovery(Some(id), cx));
                    cx.spawn(async move |_, _| {
                        // The session's next update says it; a failure leaves the chip.
                        let _ = ack.await;
                    })
                    .detach();
                },
            )
            .compact()
            .child(crate::workbench::render::probe(
                &self.regions,
                crate::workbench::Region::Recovery("terminal-dismiss"),
            ));
        Some(
            div()
                .absolute()
                .top(px(tokens.spacing * 2.0 + if ended { 36.0 } else { 0.0 }))
                .right(px(tokens.spacing * 3.0))
                .max_w(px(560.0))
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 2.0))
                .py(px(tokens.spacing))
                .rounded(px(tokens.radius))
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border)
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(chip_element)
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(gpui::SharedString::from(chip.detail.clone())),
                )
                .child(dismiss)
                .child(crate::workbench::render::probe(
                    &self.regions,
                    crate::workbench::Region::Recovery("terminal"),
                ))
                .into_any_element(),
        )
    }

    /// A quiet line at the foot of a terminal in which a known agent (`claude`, `codex`, `omp`)
    /// runs without having reported: Hutan cannot tell what it does, so it is not in *Requests*.
    /// *Track as <agent>* reports it through `agent.report` (`UX-ATTENTION-CONTROLS`).
    fn render_terminal_untracked(
        &mut self,
        id: SessionId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let found = self.sessions.read(cx).untracked_agent(id)?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let offer = self.hooks_offer(found.agent, cx);
        let label = gpui::SharedString::from(format!("Track as {}", found.agent));
        let track = ui::Button::new("untracked-track", label, &theme, &tokens)
            .compact()
            .ghost()
            .on_click(cx.listener(move |this, _, _, cx| {
                let tracking = this
                    .sessions
                    .update(cx, |store, cx| store.track_agent(id, cx));
                cx.spawn(async move |this, cx| {
                    if let Err(error) = tracking.await {
                        this.update(cx, |this, cx| {
                            this.set_notice(format!("Could not track the agent: {error}"), cx);
                        })
                        .ok();
                    }
                })
                .detach();
            }))
            .child(crate::workbench::render::probe(
                &self.regions,
                crate::workbench::Region::Untracked("track"),
            ));
        let (offer_buttons, offer_detail) = match offer {
            Some(offer) => (offer.buttons, offer.detail),
            None => (Vec::new(), None),
        };
        Some(
            div()
                .absolute()
                .bottom(px(tokens.spacing * 2.0))
                .right(px(tokens.spacing * 3.0))
                .max_w(px(560.0))
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .px(px(tokens.spacing * 2.0))
                .py(px(tokens.spacing))
                .rounded(px(tokens.radius))
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border)
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing * 2.0))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(gpui::SharedString::from(format!(
                                    "Agent not recognised · looks like {} but never reported to Hutan",
                                    found.agent
                                ))),
                        )
                        .child(track)
                        .children(offer_buttons),
                )
                .children(offer_detail)
                .child(crate::workbench::render::probe(
                    &self.regions,
                    crate::workbench::Region::Untracked("strip"),
                ))
                .into_any_element(),
        )
    }

    fn action_button(
        &self,
        id: &'static str,
        label: &'static str,
        kind: ui::ButtonKind,
        cx: &mut Context<Self>,
        run: impl Fn(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
    ) -> ui::Button {
        let style = UiStyle::get(cx);
        ui::Button::new(id, label, &style.theme, &style.density.ui())
            .kind(kind)
            .on_click(cx.listener(move |this, _, window, cx| run(this, window, cx)))
    }

    /// Closes (detaches) every tab showing `session`; in a tab of terminal splits only its split
    /// goes.
    pub(crate) fn close_session_tabs(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let id = session.to_string();
        if self.state.find_session_tab(&id).is_some() {
            self.mutate(cx, |s| {
                s.remove_session_tabs(&id);
            });
        }
    }

    // ---- hot reload -------------------------------------------------------------------------

    /// Re-reads the keymap (the `keymap.base` table + `keymap.toml`) and rebinds every key.
    pub(crate) fn rebind_keys(&mut self, cx: &mut Context<Self>) {
        let base = self.settings.get::<KeymapSettings>().base;
        let mut load = self.settings.keymap(&actions::default_bindings(base));
        if !self.assistant_enabled() {
            // Off: a key the user bound no longer opens the assistant.
            let open = <actions::OpenAssistant as gpui::Action>::name_for_type();
            load.bindings.retain(|b| b.action != open);
        }
        actions::apply_keymap(cx, &load);
        cx.notify();
    }

    /// ⌘= / ⌘-: grows or shrinks the terminal font by `delta` points and saves it as the user's
    /// `terminal.font_size` (the range the terminal view accepts).
    pub(crate) fn change_font_size(&mut self, delta: f64, cx: &mut Context<Self>) {
        let current = self.settings.get::<TerminalSettings>().font_size;
        let size = (current + delta).clamp(
            TerminalSettings::MIN_FONT_SIZE,
            TerminalSettings::MAX_FONT_SIZE,
        );
        self.save_font_size(size, cx);
    }

    /// ⌘0: back to the default `terminal.font_size`: the user's own value is removed, so the
    /// active pack's default applies.
    pub(crate) fn reset_font_size(&mut self, cx: &mut Context<Self>) {
        if let Err(err) =
            self.settings
                .remove(hutan_settings::SettingsLayer::User, "terminal", "font_size")
        {
            tracing::warn!("cannot reset terminal.font_size: {err}");
            self.set_notice("Could not reset the font size", cx);
        }
    }

    fn save_font_size(&mut self, size: f64, cx: &mut Context<Self>) {
        if let Err(err) = self
            .settings
            .write_user("terminal.font_size", toml::Value::Float(size))
        {
            tracing::warn!("cannot save terminal.font_size: {err}");
            self.set_notice("Could not save the font size", cx);
        }
    }

    /// The status bar of the current preset.
    pub fn status_bar(&self) -> StatusBarLayout {
        self.preset_defs
            .iter()
            .find(|d| d.layout.name == self.state.preset)
            .map(|d| d.status_bar.clone())
            .unwrap_or_default()
    }

    /// Re-resolves the theme settings against the catalog: the chrome pack of the current
    /// polarity (with the user's overrides), the terminal palette `terminal.theme` selects, and
    /// the problems with either. The style is published only when what the user sees changed:
    /// an edit of a pack that is not in use changes the catalog (pickers, diagnostics) and
    /// nothing else.
    pub(crate) fn refresh_theme(&mut self, cx: &mut Context<Self>) {
        let appearance = self.settings.get::<AppearanceSettings>();
        let terminal = self.settings.get::<TerminalSettings>();
        let snapshot = self.themes.snapshot();
        let mut frame = crate::theme::resolve_frame(
            &self.themes,
            &snapshot,
            &appearance,
            &terminal.theme,
            crate::theme::polarity(self.chrome, self.appearance),
        );
        let (icons, icon_notes) = self.themes.select_icons(
            &appearance.icon_theme,
            crate::theme::polarity(self.chrome, self.appearance),
        );
        frame
            .diagnostics
            .extend(self.themes.icon_snapshot().diagnostics.iter().cloned());
        frame.diagnostics.extend(icon_notes);
        frame.diagnostics.extend(self.theme_watch_note.clone());
        if crate::icons::install(icons) {
            self.invalidate_cached_views(cx);
            cx.notify();
        }
        if frame.diagnostics != self.theme_notes {
            self.settings.set_notes("themes", frame.diagnostics.clone());
            self.theme_notes = frame.diagnostics.clone();
        }
        if *frame.resolved != *self.resolved_theme {
            crate::theme::install(&frame.resolved);
            ui::apply_fonts(cx, frame.resolved.fonts());
            self.resolved_theme = frame.resolved.clone();
            self.theme = Theme::from_resolved(&frame.resolved);
            self.publish_style(cx);
        }
        self.terminal_theme_name = frame.terminal.name.clone();
        self.apply_terminal_style(&frame.terminal, terminal, cx);
        // An open picker's preview reads the catalog and the user's overrides: keep it current.
        self.sync_pack_preview();
        // File tabs take the new pack's editor colors (no reparse).
        self.refresh_code_style(cx);
    }

    /// Same as [`refresh_theme`](Self::refresh_theme): the terminal's colors and fonts come
    /// from the pack, so there is nothing to refresh on its own.
    pub(crate) fn refresh_terminal_style(&mut self, cx: &mut Context<Self>) {
        self.refresh_theme(cx);
    }

    /// Pushes the terminal view settings to every view when they changed; the daemon learns the
    /// new default colors once per change, for every attached terminal.
    fn apply_terminal_style(
        &mut self,
        theme: &hutan_settings::TerminalTheme,
        settings: std::sync::Arc<TerminalSettings>,
        cx: &mut Context<Self>,
    ) {
        let explicit = crate::settings::TerminalExplicit::of(&self.settings);
        let style = terminal_style(
            &settings,
            theme,
            &self.resolved_theme,
            mono_family(cx, &self.resolved_theme),
            explicit,
            self.density,
        );
        self.terminal_explicit = explicit;
        self.terminal_settings = settings;
        if style == self.terminal_style {
            return;
        }
        self.terminal_style = style.clone();
        cx.set_global(TerminalStyle(style.clone()));
        for item in self.terminals.values() {
            item.view
                .update(cx, |view, cx| view.update_settings(style.clone(), cx));
        }
        let colors = DefaultColors::from(&style.palette);
        self.sessions
            .update(cx, |store, cx| store.set_terminal_colors(colors, cx));
        cx.notify();
    }

    pub(crate) fn on_settings_changed(
        &mut self,
        changes: &ChangeSet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if changes.contains(KEYMAP_NAMESPACE) {
            self.rebind_keys(cx);
        }
        if changes.contains("appearance") {
            let appearance = self.settings.get::<AppearanceSettings>();
            self.chrome = appearance.appearance;
            self.density = appearance.density;
            super::ui::motion::set_setting(cx, appearance.reduce_motion);
            self.refresh_theme(cx);
            self.publish_style(cx);
        }
        if changes.contains("terminal")
            || crate::settings::TerminalExplicit::of(&self.settings) != self.terminal_explicit
        {
            self.refresh_terminal_style(cx);
        }
        if changes.contains("assistant") {
            self.apply_assistant_enabled(cx);
        }
        if changes.contains("layout") {
            self.apply_layout_settings(cx);
        }
        if changes.contains("app") {
            self.sync_menus(cx);
        }
        if changes.contains("editor") {
            self.refresh_code_style(cx);
            // `editor.vim_mode` is live in file tabs too (safe in the middle of a command).
            for view in self.file_views.values() {
                view.update(cx, |view, cx| view.sync_vim(cx));
                view.update(cx, |view, cx| view.sync_blame_setting(cx));
            }
            // `editor.agent_highlight` too.
            let highlight = self
                .settings
                .get::<crate::settings::EditorSettings>()
                .agent_highlight;
            let buffers: Vec<_> = self
                .buffers
                .buffers
                .values()
                .map(|s| s.buffer.clone())
                .collect();
            for buffer in buffers {
                buffer.update(cx, |b, cx| b.set_agent_highlight(highlight, cx));
            }
        }
        if changes.contains("integrations") {
            self.on_integrations_setting_changed(cx);
        }
        self.on_mac_settings_changed(changes, window, cx);
        self.invalidate_cached_views(cx);
        cx.notify();
    }

    fn apply_layout_settings(&mut self, cx: &mut Context<Self>) {
        let previous = self.layout_seen.clone();
        let before = self
            .preset_defs
            .iter()
            .find(|d| d.layout.name == self.state.preset)
            .cloned();
        self.refresh_layout_defs(cx);
        let layout = self.layout_seen.clone();
        if layout.preset != previous.preset {
            self.apply_preset_named(&layout.preset.clone(), cx);
        } else if let Some(before) = before
            && let Some(after) = self
                .preset_defs
                .iter()
                .find(|d| d.layout.name == before.layout.name)
                .cloned()
            && after != before
        {
            let preset = after.layout;
            self.mutate(cx, |s| s.apply_preset(&preset));
        }
    }

    /// Reads `layout` again: the presets (and the status bars they carry), the palette's
    /// commands and the View menu. Nothing is applied: a change the window made itself
    /// (`customize.rs`) calls this after writing, so the settings' own notification that
    /// follows finds nothing new and leaves the arrangement alone.
    pub(crate) fn refresh_layout_defs(&mut self, cx: &mut Context<Self>) {
        let layout = self.settings.get::<LayoutSettings>();
        self.layout_seen = layout.clone();
        self.preset_defs = layout.preset_defs();
        self.presets = self.preset_defs.iter().map(|d| d.layout.clone()).collect();
        self.commands = std::rc::Rc::new(actions::commands(
            &self.presets,
            &self.panels,
            self.assistant_enabled(),
        ));
        self.refresh_menus(cx);
    }

    /// Opens a split of an existing tab: used by the pane header menus of terminal tabs.
    pub(crate) fn tab_session_menu(&self, pane: PaneId, index: usize) -> Vec<MenuEntry> {
        let session = self
            .state
            .pane(pane)
            .and_then(|p| p.items.get(index))
            .and_then(session_of_item);
        match session {
            Some(session) => {
                let mut entries = self.tab_menu(pane, index);
                let mut extra = vec![
                    Self::entry("Rename…", move |this, window, cx| {
                        this.rename_session(session, window, cx);
                    }),
                    Self::entry("Restart", move |this, window, cx| {
                        this.restart_session(session, window, cx);
                    }),
                    Self::entry("Stop…", move |this, window, cx| {
                        this.stop_session(session, window, cx);
                    }),
                    Self::entry("Close session…", move |this, window, cx| {
                        this.close_session(session, window, cx);
                    }),
                    Self::entry("Show in Resources", move |this, window, cx| {
                        this.show_in_resources(session, window, cx);
                    }),
                    Self::entry("Copy Session ID", move |this, _, cx| {
                        this.copy_session_id(session, cx);
                    }),
                ];
                // "Close" (detach) stays last.
                let close = entries.pop();
                entries.append(&mut extra);
                entries.extend(close);
                entries
            }
            None => self.tab_menu(pane, index),
        }
    }
}
