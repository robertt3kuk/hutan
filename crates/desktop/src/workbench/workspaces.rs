//! Workbench behavior around projects and workspaces (M12): the active workspace and its
//! layout, the ⌘O project picker, the ⌘J jump palette (⌘P's Sessions section, `quick_open.rs`),
//! the workspace menus and commands.
//!
//! One workspace is *active*: its layout is the live one in the model (the others are parked,
//! see `model/workspace.rs`), its first root layers its `.hutan/settings.toml` over the user
//! settings, and new sessions start in it. Opening a session of another workspace from
//! anywhere (sidebar, ⌘J, attention, a restored tab) switches to that workspace first.
//!
//! **Terminals of a parked workspace are detached**, not kept attached: only what the live
//! layout shows is attached (`attach_open_items`), so nothing outside the window renders or is
//! acked. Coming back re-attaches, and the daemon answers with a snapshot of the screen
//! (measured in `tests/it/workspaces_gpui.rs`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, Context, ElementId, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, PathPromptOptions, SharedString, StatefulInteractiveElement, Styled, Window,
    div, prelude::*, px,
};
use hutan_client::ConnectionState;
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::history::HistorySource;
use hutan_protocol::messages::{RecentProject, WorkspaceInfo, WorkspaceKind};

use super::actions::palette;
use super::palette::fuzzy_score;
use super::panels::HistoryResume;
use super::terminals::session_of_item;
use super::ui::StyledExt as _;
use super::ui::motion::{self, PresentExt as _};
use super::ui::scroll::ScrollExt as _;
use super::view::{MenuEntry, Workbench};
use super::vim_ui::OverlayField;
use crate::settings::{SidebarOrganize, SidebarSettings, WorkspaceSort, WorkspacesSettings};
use crate::theme::DensityExt as _;

/// How many projects the ⌘O picker asks the daemon for.
const RECENT_LIMIT: u32 = 50;

/// Which list the picker shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    /// ⌘O: "Open Folder…" and the recent projects.
    Projects,
    /// ⌘J: every workspace and session, hidden workspaces included.
    Jump,
    /// ⌘P: the files of the active project (`quick_open.rs`), then the rows of [`Self::Jump`].
    QuickOpen,
    /// ⌘K "Review approvals": the permission requests of every chat that wait for an answer.
    Approvals,
    /// ⌘K "Worktree: Remove…": the removable worktrees of the active workspace.
    Worktrees,
    /// ⌘K "Worktree: Review Changes…": the checkouts of the active workspace's repositories.
    Changes,
}

/// What confirming a picker row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PickerAction {
    OpenFolder,
    RemoveWorktree(super::worktrees::WorktreeChoice),
    /// Show the Changes view of this checkout.
    ReviewChanges(PathBuf),
    OpenProject(PathBuf),
    Workspace(WorkspaceId),
    Session(SessionId),
    /// A history hit that can be resumed.
    ResumeHit(HistoryResume),
    /// A history hit without a resume command: show it in the History panel.
    ShowHit {
        query: String,
        source: HistorySource,
        key: String,
    },
    /// A file of ⌘P's Files section.
    OpenFile(super::quick_open::FileHit),
}

/// One row of the picker.
#[derive(Debug, Clone)]
pub struct PickerEntry {
    pub icon: crate::icons::Icon,
    pub title: SharedString,
    pub detail: SharedString,
    pub(crate) action: PickerAction,
    /// Stays listed whatever the query says ("Open Folder…").
    pub fixed: bool,
}

/// Where the ⌘J `?` history search stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HistoryState {
    /// Nothing typed after the `?`.
    #[default]
    Idle,
    Searching,
    Done,
    /// Indexing is off.
    Disabled,
    /// The daemon predates `history.*`.
    Unsupported,
    Failed(String),
}

/// An open ⌘O picker or ⌘J palette: the query, the entries, the filtered rows (indices into
/// `entries`, best first) and the highlighted row. A ⌘J query that starts with `?` searches the
/// session history instead: the rows are then the hits (`history`), best first.
#[derive(Debug, Clone)]
pub struct PickerState {
    pub kind: PickerKind,
    pub query: String,
    pub entries: Vec<PickerEntry>,
    pub matches: Vec<usize>,
    pub selected: usize,
    /// The project list is still on its way from the daemon.
    pub loading: bool,
    /// The hits of the `?` search.
    pub history: Vec<PickerEntry>,
    pub history_state: HistoryState,
    /// ⌘P's Files section (its rows are the first `files.rows` entries).
    pub files: super::quick_open::FileFinder,
}

impl PickerState {
    pub(super) fn new(kind: PickerKind, entries: Vec<PickerEntry>) -> Self {
        let mut state = Self {
            kind,
            query: String::new(),
            entries,
            matches: Vec::new(),
            selected: 0,
            loading: false,
            history: Vec::new(),
            history_state: HistoryState::Idle,
            files: super::quick_open::FileFinder::default(),
        };
        state.refilter();
        state
    }

    /// The text after the `?` of a ⌘J / ⌘P history search; `None` for every other query.
    pub fn history_query(&self) -> Option<&str> {
        match self.kind {
            PickerKind::Jump | PickerKind::QuickOpen => self.query.strip_prefix('?').map(str::trim),
            PickerKind::Projects
            | PickerKind::Approvals
            | PickerKind::Worktrees
            | PickerKind::Changes => None,
        }
    }

    /// The rows `matches` indexes: the hits in a history search, else the files (⌘P) and the
    /// workspaces and sessions.
    fn shown(&self) -> &[PickerEntry] {
        if self.history_query().is_some() {
            &self.history
        } else {
            &self.entries
        }
    }

    /// Recomputes the filtered rows: fixed rows first (⌘P's files, which the daemon matched),
    /// then the fuzzy matches, titles before details. In a history search every hit is a row,
    /// in the order the index ranked them.
    pub(super) fn refilter(&mut self) {
        if self.history_query().is_some() {
            self.matches = (0..self.history.len()).collect();
            self.selected = self.selected.min(self.matches.len().saturating_sub(1));
            return;
        }
        let mut fixed = Vec::new();
        let mut scored: Vec<(i32, usize)> = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.fixed {
                fixed.push(index);
                continue;
            }
            // Titles match fuzzily; details (folders, workspace names) only by whole words, so
            // a short query does not hit random letters of a long path.
            let detail = entry.detail.to_lowercase();
            let score = fuzzy_score(&self.query, &entry.title)
                .map(|s| s + 1000)
                .or_else(|| {
                    self.query
                        .split_whitespace()
                        .all(|word| detail.contains(&word.to_lowercase()))
                        .then_some(0)
                });
            if let Some(score) = score {
                scored.push((score, index));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.matches = fixed;
        self.matches.extend(scored.into_iter().map(|(_, i)| i));
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    /// The row at position `row` of the filtered list.
    pub(super) fn entry_at(&self, row: usize) -> Option<&PickerEntry> {
        self.matches
            .get(row)
            .and_then(|index| self.shown().get(*index))
    }

    fn selected_entry(&self) -> Option<&PickerEntry> {
        self.entry_at(self.selected)
    }
}

/// `~/proj/x` for paths under the user's home folder.
pub(super) fn tilde(path: &Path) -> String {
    match std::env::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_owned)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

impl Workbench {
    // ---- the active container ----------------------------------------------------------------

    /// The container whose layout is shown: the workspace the user is in, else the project in
    /// no workspace, else Home.
    pub fn active_workspace(&self) -> Option<WorkspaceId> {
        self.state.workspace().and_then(|id| id.parse().ok())
    }

    /// Where new sessions start: the project the user works in (the active project, else the
    /// first project of the active workspace, else the workspace itself), the active project
    /// or Home.
    pub(crate) fn target_workspace<'a>(
        &self,
        store: &'a crate::sessions::SessionsStore,
    ) -> Option<&'a WorkspaceInfo> {
        let container = self.active_workspace().and_then(|id| store.workspace(id));
        match container {
            Some(container) if container.kind == WorkspaceKind::Workspace => store
                .active_project()
                .filter(|p| p.parent_id == Some(container.id))
                .or_else(|| store.state().projects_in(container.id).next())
                .or(Some(container)),
            Some(container) => Some(container),
            None => store.home_workspace(),
        }
    }

    /// Shows the container of `workspace` (a project, a workspace or Home): its layout becomes
    /// the live one, its settings apply, and the terminals of the layout that was shown detach.
    pub fn switch_workspace(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_workspace(workspace, cx);
        self.focus_root(window, cx);
    }

    /// The containers the keyboard cycles through, in the sidebar's order under its filters: a
    /// workspace's projects (the workspace itself when it has none), the projects of no
    /// workspace, Home. Switching never closes a session.
    pub fn container_cycle(&self, cx: &gpui::App) -> Vec<WorkspaceId> {
        use crate::sessions::tree::{self, Node};
        let store = self.sessions.read(cx);
        let filters = self.settings.get::<WorkspacesSettings>();
        let sessions: Vec<&hutan_protocol::messages::SessionInfo> =
            store.sessions().iter().collect();
        let needs = store.requests_by_workspace();
        let built = tree::build(&tree::Inputs {
            workspaces: store.workspaces(),
            sessions: &sessions,
            organize: self.settings.get::<SidebarSettings>().organize,
            sort: filters.sort,
            filters: &filters,
            query: "",
            active: [store.active_workspace_id(), store.active_project_id()],
            needs: &needs,
            session_matches: &|_| true,
        });
        let mut order = Vec::new();
        for node in &built.nodes {
            match node {
                Node::Workspace(workspace) if workspace.projects.is_empty() => {
                    order.push(workspace.info.id);
                }
                Node::Workspace(workspace) => {
                    order.extend(workspace.projects.iter().map(|p| p.info.id));
                }
                Node::Project(node) | Node::Home(node) => order.push(node.info.id),
                Node::NoWorkspace => {}
            }
        }
        order
    }

    /// Shows the container `delta` places after the one the window shows in
    /// [`container_cycle`](Self::container_cycle), wrapping around (⌃⌥↓ / ⌃⌥↑).
    pub fn cycle_container(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let order = self.container_cycle(cx);
        if order.len() < 2 {
            return;
        }
        let current = {
            let store = self.sessions.read(cx);
            store.active_project_id().or(store.active_workspace_id())
        };
        let at = current
            .and_then(|id| order.iter().position(|c| *c == id))
            .map_or(0, |i| i as isize);
        let next = (at + delta).rem_euclid(order.len() as isize) as usize;
        self.switch_workspace(order[next], window, cx);
    }

    /// Shows `project` and picks its worktree at `path`: Files and Search follow it, and the
    /// sidebar marks its row. The center keeps the layout of the project.
    pub fn open_checkout(
        &mut self,
        project: WorkspaceId,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.switch_workspace(project, window, cx);
        self.sessions.update(cx, |store, cx| {
            store.set_active_checkout(project, Some(path), cx);
        });
    }

    /// The model part of [`Self::switch_workspace`]. The layout belongs to the container the
    /// user switches to: the workspace when `workspace` is a project inside one, else the
    /// project, else Home. A no-op for the container already shown (the project chosen inside
    /// it still changes).
    pub(crate) fn show_workspace(&mut self, workspace: WorkspaceId, cx: &mut Context<Self>) {
        self.resolve_active_workspace(cx);
        let (container, project) = {
            let store = self.sessions.read(cx);
            let state = store.state();
            let container = state.workspace_of(workspace);
            // The project chosen so far stays the workspace's choice for when it comes back.
            if let Some(chosen) = self.chosen_project {
                let of = state.workspace_of(chosen);
                if of != chosen {
                    self.remembered_projects.insert(of, chosen);
                }
            }
            // An id the store does not know yet is a project (or workspace) whose events are on
            // their way; the choice is checked against the store when it is used.
            let project = match store.workspace(workspace) {
                Some(w) if w.kind == WorkspaceKind::Project => Some(w.id),
                Some(_) => None,
                None => Some(workspace),
            };
            // A workspace keeps the project the user had chosen in it.
            let kept = self
                .remembered_projects
                .get(&container)
                .copied()
                .filter(|chosen| state.workspace_of(*chosen) == container);
            (container, project.or(kept))
        };
        self.chosen_project = project;
        let key = container.to_string();
        if self.state.workspace() == Some(key.as_str()) {
            self.sync_workspace(cx);
            return;
        }
        self.mutate(cx, |s| {
            s.switch_workspace(&key);
        });
    }

    /// Switches to the container `session` belongs to (it is about to be shown), and makes its
    /// project the one the user works in.
    pub(crate) fn show_workspace_of(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let workspace = self
            .sessions
            .read(cx)
            .session(session)
            .map(|s| s.workspace_id);
        if let Some(workspace) = workspace {
            self.show_workspace(workspace, cx);
        }
    }

    /// The container shown and the project worked in, whose `.hutan/settings.toml` files
    /// layer over the user's settings (see [`layer_roots`]): the shown workspace or project
    /// and, inside a workspace, the project the user chose.
    pub(crate) fn layer_containers(
        &self,
        cx: &gpui::App,
    ) -> (Option<WorkspaceInfo>, Option<WorkspaceInfo>) {
        let store = self.sessions.read(cx);
        let container = self.active_workspace().and_then(|id| store.workspace(id));
        let project = match container {
            Some(container) if container.kind == WorkspaceKind::Project => Some(container),
            Some(container) if container.kind == WorkspaceKind::Workspace => self
                .chosen_project
                .and_then(|id| store.workspace(id))
                .filter(|p| p.parent_id == Some(container.id)),
            _ => None,
        };
        (container.cloned(), project.cloned())
    }

    /// Tells the sessions store which container and project are active, and layers their
    /// `.hutan/settings.toml` files over the user settings: the first project root of the
    /// workspace (or Home's folder) is the workspace layer, the root of the project the user
    /// works in the project layer (a project in no workspace layers only its own). The settings
    /// store re-reads and hot-reloads both while they stay the active ones.
    pub(crate) fn sync_workspace(&mut self, cx: &mut Context<Self>) {
        let active = self.active_workspace();
        // Focusing a session of another project of the workspace moves the active project (not
        // when the focus only changed because the layout did: a project row that was clicked
        // chose its project, whatever tab the restored layout had focused).
        let focused = self.sessions.read(cx).focused();
        if focused != self.focus_seen {
            self.focus_seen = focused;
            if self.synced_container == active {
                let store = self.sessions.read(cx);
                let project = focused
                    .and_then(|id| store.session(id))
                    .and_then(|s| store.workspace(s.workspace_id))
                    .filter(|w| w.kind == WorkspaceKind::Project)
                    .map(|w| w.id);
                if project.is_some() {
                    self.chosen_project = project;
                }
            }
        }
        self.synced_container = active;
        let (project, workspace_root, project_root) = {
            let (container, project) = self.layer_containers(cx);
            let (workspace_root, project_root) = layer_roots(container.as_ref(), project.as_ref());
            (project.map(|p| p.id), workspace_root, project_root)
        };
        self.sessions
            .update(cx, |store, cx| store.set_active(active, project, cx));
        if workspace_root != self.workspace_root {
            self.workspace_root.clone_from(&workspace_root);
            self.settings.set_workspace_root(workspace_root);
        }
        if project_root != self.project_root {
            self.project_root.clone_from(&project_root);
            self.settings.set_project_root(project_root);
        }
    }

    /// After the daemon told us what it has: a layout from before workspaces existed (or a first
    /// start) belongs to the container of its first session, else Home; a project that joined a
    /// workspace hands its layout to it; a container that is gone hands the window to Home;
    /// parked layouts of vanished containers are forgotten.
    pub(crate) fn resolve_active_workspace(&mut self, cx: &mut Context<Self>) {
        let (known, parents, home, owner) = {
            let store = self.sessions.read(cx);
            if !store.synced() || store.connection_state() != ConnectionState::Connected {
                return;
            }
            let known: HashSet<String> = store
                .workspaces()
                .iter()
                .map(|w| w.id.to_string())
                .collect();
            let parents: Vec<(String, String)> = store
                .workspaces()
                .iter()
                .filter_map(|w| Some((w.id.to_string(), w.parent_id?.to_string())))
                .collect();
            let owner = self
                .state
                .center
                .panes()
                .into_iter()
                .flat_map(|pane| pane.items.iter())
                .filter_map(session_of_item)
                .find_map(|session| store.session(session))
                .map(|session| store.state().workspace_of(session.workspace_id));
            (known, parents, store.home_workspace().map(|w| w.id), owner)
        };
        let mut changed = false;
        match self.state.workspace().map(str::to_owned) {
            None => {
                if let Some(owner) = owner.or(home) {
                    self.state.adopt_workspace(&owner.to_string());
                    changed = true;
                }
            }
            Some(active) if known.contains(&active) => {
                if self.pending_workspace.map(|p| p.to_string()) == Some(active) {
                    self.pending_workspace = None;
                }
            }
            Some(active) => {
                let pending = self.pending_workspace.map(|p| p.to_string()) == Some(active);
                if !pending && let Some(home) = home {
                    self.state.switch_workspace(&home.to_string());
                    changed = true;
                }
            }
        }
        for (project, workspace) in &parents {
            changed |= self.state.merge_workspace(project, workspace);
        }
        let gone: Vec<String> = self
            .state
            .parked_workspaces()
            .filter(|id| !known.contains(*id))
            .map(str::to_owned)
            .collect();
        for id in gone {
            self.state.forget_workspace(&id);
            changed = true;
        }
        if changed {
            self.schedule_save(cx);
        }
    }

    // ---- opening projects --------------------------------------------------------------------

    /// `workspace.open` for `path`, then shows the workspace (creating it when it is new).
    pub fn open_project_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.open_workspace(path, cx));
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(opened) => {
                this.update_in(cx, |this, window, cx| {
                    let id = opened.workspace.id;
                    // Its `workspace.updated` may still be on its way.
                    this.pending_workspace = Some(id);
                    this.switch_workspace(id, window, cx);
                    this.project_opened(id, window, cx);
                })
                .ok();
            }
            Err(err) => {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot open the folder: {err}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Asks for a folder with the system's open panel; `then` runs with the choice.
    pub(super) fn choose_folder(
        &mut self,
        prompt: &'static str,
        then: impl FnOnce(&mut Workbench, PathBuf, &mut Window, &mut Context<Workbench>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                this.update_in(cx, |this, window, cx| then(this, path, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Like [`Self::choose_folder`] for several folders at once; `then` runs with the choice
    /// when it is not empty.
    pub(super) fn choose_folders(
        &mut self,
        prompt: &'static str,
        then: impl FnOnce(&mut Workbench, Vec<PathBuf>, &mut Window, &mut Context<Workbench>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            if !paths.is_empty() {
                this.update_in(cx, |this, window, cx| then(this, paths, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// "Open Folder…": the system folder panel, then `workspace.open`. Before the first project
    /// the panel starts at `~` (the system's own remembers some other place, the Desktop say).
    pub fn open_folder_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first_run = !self
            .sessions
            .read(cx)
            .workspaces()
            .iter()
            .any(|w| w.kind == WorkspaceKind::Project);
        let start = first_run
            .then(|| {
                std::env::home_dir().or_else(|| {
                    self.sessions
                        .read(cx)
                        .home_workspace()
                        .and_then(|home| home.roots.first().cloned())
                })
            })
            .flatten();
        let Some(start) = start else {
            self.choose_folder(
                "Open",
                |this, path, window, cx| this.open_project_path(path, window, cx),
                window,
                cx,
            );
            return;
        };
        let panel = self.mac.open_panel.clone();
        cx.spawn_in(window, async move |this, cx| {
            // AppKit is asked outside of any update of the window, as GPUI does with its own.
            let path = match panel.choose_folder(&start) {
                Some(chosen) => chosen.await.ok().flatten(),
                None => {
                    let Ok(chosen) = cx.update(|_, cx| {
                        cx.prompt_for_paths(PathPromptOptions {
                            files: false,
                            directories: true,
                            multiple: false,
                            prompt: Some("Open".into()),
                        })
                    }) else {
                        return;
                    };
                    match chosen.await {
                        Ok(Ok(Some(paths))) => paths.into_iter().next(),
                        _ => None,
                    }
                }
            };
            if let Some(path) = path {
                this.update_in(cx, |this, window, cx| {
                    this.open_project_path(path, window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    // ---- workspace verbs ---------------------------------------------------------------------

    pub(super) fn workspace_info(&self, id: WorkspaceId, cx: &gpui::App) -> Option<WorkspaceInfo> {
        self.sessions.read(cx).workspace(id).cloned()
    }

    /// The daemon's description of the active workspace.
    pub(crate) fn active_workspace_info(&self, cx: &gpui::App) -> Option<WorkspaceInfo> {
        self.active_workspace()
            .and_then(|id| self.workspace_info(id, cx))
    }

    /// Shows "Cannot `what`: reason" when the request in `task` failed.
    pub(super) fn report_unit<T: 'static>(
        &mut self,
        what: &'static str,
        task: gpui::Task<Result<T, String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            if let Err(err) = task.await {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot {what}: {err}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    // ---- sorting and filters -----------------------------------------------------------------

    /// Sets how the sidebar sorts workspaces and keeps it in `workspaces.sort`.
    pub fn set_workspace_sort(&mut self, mode: WorkspaceSort, cx: &mut Context<Self>) {
        self.write_workspace_setting("workspaces.sort", toml::Value::String(mode.key().into()));
        cx.notify();
    }

    /// Flips the boolean filter `key` (`hide_empty`, `hide_sleeping`, `hide_cli_created`,
    /// `always_show_active`) of the `workspaces` namespace.
    pub fn toggle_workspace_filter(&mut self, key: &'static str, cx: &mut Context<Self>) {
        let current = self.settings.get::<WorkspacesSettings>();
        let value = match key {
            "hide_empty" => current.hide_empty,
            "hide_sleeping" => current.hide_sleeping,
            "hide_cli_created" => current.hide_cli_created,
            "always_show_active" => current.always_show_active,
            _ => return,
        };
        self.write_workspace_setting(&format!("workspaces.{key}"), toml::Value::Boolean(!value));
        cx.notify();
    }

    /// Organizes the sidebar by workspaces or by projects and keeps it in `sidebar.organize`.
    pub fn set_sidebar_organize(&mut self, mode: SidebarOrganize, cx: &mut Context<Self>) {
        self.write_workspace_setting("sidebar.organize", toml::Value::String(mode.key().into()));
        cx.notify();
    }

    /// Lists every container's sessions (the old tree) or only the active container's
    /// (`sidebar.show_all_sessions`).
    pub fn toggle_show_all_sessions(&mut self, cx: &mut Context<Self>) {
        let all = self.settings.get::<SidebarSettings>().show_all_sessions;
        self.write_workspace_setting("sidebar.show_all_sessions", toml::Value::Boolean(!all));
        cx.notify();
    }

    fn write_workspace_setting(&mut self, key: &str, value: toml::Value) {
        if let Err(err) = self.settings.write_user(key, value) {
            tracing::warn!("cannot save `{key}`: {err}");
        }
    }

    /// The sort, filter and organize menu of the sidebar header.
    pub(crate) fn sort_menu(&self) -> Vec<MenuEntry> {
        let settings = self.settings.get::<WorkspacesSettings>();
        let mut entries: Vec<MenuEntry> = WorkspaceSort::ALL
            .into_iter()
            .map(|mode| {
                let mut entry = Self::entry(mode.label(), move |this, _, cx| {
                    this.set_workspace_sort(mode, cx);
                });
                entry.checked = settings.sort == mode;
                entry
            })
            .collect();
        for (key, label, on) in [
            ("hide_empty", "Hide Empty Projects", settings.hide_empty),
            (
                "hide_sleeping",
                "Hide Sleeping Projects",
                settings.hide_sleeping,
            ),
            (
                "hide_cli_created",
                "Hide CLI-Created Projects",
                settings.hide_cli_created,
            ),
            (
                "always_show_active",
                "Always Show Where You Are",
                settings.always_show_active,
            ),
        ] {
            let mut entry = Self::entry(label, move |this, _, cx| {
                this.toggle_workspace_filter(key, cx);
            });
            entry.checked = on;
            entries.push(entry);
        }
        let organize = self.settings.get::<SidebarSettings>().organize;
        for mode in [SidebarOrganize::Workspaces, SidebarOrganize::Projects] {
            let mut entry =
                Self::entry(format!("Organize: {}", mode.label()), move |this, _, cx| {
                    this.set_sidebar_organize(mode, cx)
                });
            entry.checked = organize == mode;
            entries.push(entry);
        }
        let mut all = Self::entry("Show All Sessions", |this, _, cx| {
            this.toggle_show_all_sessions(cx);
        });
        all.checked = self.settings.get::<SidebarSettings>().show_all_sessions;
        entries.push(all);
        // The Projects header has no ⚙ of its own any more: its settings are here.
        entries.push(Self::entry(
            "Projects Sidebar Settings…",
            |_, window, cx| {
                window.dispatch_action(
                    Box::new(super::actions::OpenSettingsFor(
                        "panels.sessions".to_owned(),
                    )),
                    cx,
                );
            },
        ));
        entries
    }

    /// Opens the sort and filter menu at `position`.
    pub fn show_sort_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut entries = self.sort_menu();
        // The panel's own placement (Move to ▸, Hide); its settings are *Projects Sidebar
        // Settings…* above.
        entries.extend(
            self.panel_menu(&super::model::PanelId::new("sessions"), position)
                .into_iter()
                .filter(|entry| entry.label != "Panel settings…"),
        );
        entries.push(Self::submenu_entry(
            "Sidebar sections",
            move |this, window, cx| {
                let entries = this.left_checklist(position);
                this.open_menu(position, entries, window, cx);
            },
        ));
        self.open_menu(position, entries, window, cx);
    }

    // ---- the picker (⌘O project picker, ⌘J jump palette) -------------------------------------

    /// The open picker, if any.
    pub fn picker(&self) -> Option<&PickerState> {
        self.picker.as_ref()
    }

    /// Titles of the picker rows currently shown, in order.
    pub fn picker_titles(&self) -> Vec<SharedString> {
        self.picker
            .as_ref()
            .map(|p| {
                (0..p.matches.len())
                    .filter_map(|row| p.entry_at(row))
                    .map(|e| e.title.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn show_picker(
        &mut self,
        state: PickerState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette = None;
        self.context_menu = None;
        if self
            .picker
            .replace(state)
            .is_some_and(|old| old.kind == PickerKind::QuickOpen)
        {
            self.quick_open_closed(cx);
        }
        self.edits.picker.reset();
        window.focus(&self.picker_focus, cx);
        cx.notify();
    }

    pub fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker_search = None;
        if let Some(old) = self.picker.take() {
            if old.kind == PickerKind::QuickOpen {
                self.quick_open_closed(cx);
            }
            self.leave_picker(old, cx);
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    /// ⌘O: "Open Folder…" and the recent projects (`workspace.recent`).
    pub fn open_project_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open_folder = PickerEntry {
            icon: crate::icons::Icon::ActionOpenWorktree,
            title: "Open Folder…".into(),
            detail: "Choose a folder in Finder".into(),
            action: PickerAction::OpenFolder,
            fixed: true,
        };
        let mut state = PickerState::new(PickerKind::Projects, vec![open_folder]);
        state.loading = true;
        self.show_picker(state, window, cx);
        let task = self
            .sessions
            .update(cx, |store, cx| store.recent_projects(RECENT_LIMIT, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let notice = match result {
                    Ok(projects) => {
                        this.fill_projects(projects);
                        None
                    }
                    Err(err) => {
                        if let Some(picker) = &mut this.picker
                            && picker.kind == PickerKind::Projects
                        {
                            picker.loading = false;
                        }
                        Some(format!("Cannot list the recent projects: {err}"))
                    }
                };
                if let Some(notice) = notice {
                    this.set_notice(notice, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn fill_projects(&mut self, projects: Vec<RecentProject>) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        if picker.kind != PickerKind::Projects {
            return;
        }
        picker.entries.truncate(1);
        picker
            .entries
            .extend(projects.into_iter().map(|project| PickerEntry {
                icon: if project.workspace_id.is_some() {
                    crate::icons::Icon::NavWorkspace
                } else {
                    crate::icons::Icon::NavProjectFolder
                },
                title: project.name.into(),
                detail: tilde(&project.path).into(),
                action: PickerAction::OpenProject(project.path),
                fixed: false,
            }));
        picker.loading = false;
        picker.refilter();
    }

    /// ⌘J: every workspace, project (hidden ones too, but not stale ones) and session, in the
    /// sidebar's order; Enter shows it. A workspace or project row shows its container's
    /// layout; a session its tab.
    pub fn open_jump_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.jump_entries(cx);
        self.show_picker(PickerState::new(PickerKind::Jump, entries), window, cx);
    }

    /// The rows of the ⌘J palette (and ⌘P's Sessions section). The filters of the sidebar do
    /// not apply, except that a project that never had a session is **stale** (a folder opened
    /// by mistake and left): it is not offered unless it is pinned, in a workspace or the one
    /// the window is in. ⌘O's recent projects still list it.
    pub(super) fn jump_entries(&self, cx: &gpui::App) -> Vec<PickerEntry> {
        use crate::sessions::tree::{self, Node, ProjectNode, folder_label, session_folder};
        let store = self.sessions.read(cx);
        let settings = self.settings.get::<WorkspacesSettings>();
        let everything = WorkspacesSettings {
            hide_empty: true,
            hide_sleeping: false,
            hide_cli_created: false,
            always_show_active: true,
            ..(*settings).clone()
        };
        let sessions: Vec<&hutan_protocol::messages::SessionInfo> =
            store.sessions().iter().collect();
        let needs = store.requests_by_workspace();
        let tree = tree::build(&tree::Inputs {
            workspaces: store.workspaces(),
            sessions: &sessions,
            organize: self.settings.get::<SidebarSettings>().organize,
            sort: settings.sort,
            filters: &everything,
            query: "",
            active: [store.active_workspace_id(), store.active_project_id()],
            needs: &needs,
            session_matches: &|_| true,
        });
        let project_entry = |project: &ProjectNode, workspace: Option<&str>| {
            let info = &project.info;
            let icon = match info.kind {
                WorkspaceKind::Home => crate::icons::Icon::NavHome,
                _ if info
                    .project
                    .as_ref()
                    .is_some_and(|p| p.common_dir.is_some()) =>
                {
                    crate::icons::Icon::NavProjectRepository
                }
                _ => crate::icons::Icon::NavProjectFolder,
            };
            let folder = project_root_of(info)
                .map(|root| tilde(&root))
                .unwrap_or_default();
            PickerEntry {
                icon,
                title: info.name.clone().into(),
                detail: match workspace {
                    Some(workspace) => format!("{workspace} › {folder}"),
                    None => folder,
                }
                .into(),
                action: PickerAction::Workspace(info.id),
                fixed: false,
            }
        };
        let mut entries = Vec::new();
        // Sessions follow the containers, grouped the way the sidebar lists them.
        let mut session_rows: Vec<(String, &hutan_protocol::messages::SessionInfo)> = Vec::new();
        let mut push_sessions =
            |label: String, list: &'_ [hutan_protocol::messages::SessionInfo]| {
                for session in list {
                    if let Some(found) = store.session(session.id) {
                        session_rows.push((label.clone(), found));
                    }
                }
            };
        for node in &tree.nodes {
            match node {
                Node::Workspace(workspace) => {
                    let projects = workspace.projects.len();
                    entries.push(PickerEntry {
                        icon: crate::icons::Icon::NavWorkspace,
                        title: workspace.info.name.clone().into(),
                        detail: match projects {
                            0 => "no projects".to_owned(),
                            1 => "1 project".to_owned(),
                            n => format!("{n} projects"),
                        }
                        .into(),
                        action: PickerAction::Workspace(workspace.info.id),
                        fixed: false,
                    });
                    for project in &workspace.projects {
                        entries.push(project_entry(project, Some(&workspace.info.name)));
                        push_sessions(
                            format!("{} › {}", workspace.info.name, project.info.name),
                            &project.sessions,
                        );
                    }
                    push_sessions(workspace.info.name.clone(), &workspace.sessions);
                }
                Node::Project(project) => {
                    let workspace = project
                        .info
                        .parent_id
                        .and_then(|id| store.workspace(id))
                        .map(|w| w.name.as_str());
                    entries.push(project_entry(project, workspace));
                    push_sessions(
                        match workspace {
                            Some(workspace) => format!("{workspace} › {}", project.info.name),
                            None => project.info.name.clone(),
                        },
                        &project.sessions,
                    );
                }
                Node::Home(home) => {
                    entries.push(project_entry(home, None));
                    push_sessions(home.info.name.clone(), &home.sessions);
                }
                Node::NoWorkspace => {}
            }
        }
        for (label, session) in session_rows {
            let folder = session_folder(session)
                .map(folder_label)
                .unwrap_or_default();
            let badge = crate::sessions::placement_badge(session)
                .map(|badge| format!(" · {}", badge.text))
                .unwrap_or_default();
            // How long ago the session started tells two namesakes apart when nothing else does.
            let age = super::panels::age_label(session.created_at_ms);
            let command = match session.command.join(" ") {
                command if command.is_empty() => String::new(),
                command => format!(" · {command}"),
            };
            entries.push(PickerEntry {
                icon: super::panels::session_icon(session),
                title: session.title.clone().into(),
                detail: format!("{label} · {folder}{badge} · {age}{command}").into(),
                action: PickerAction::Session(session.id),
                fixed: false,
            });
        }
        entries
    }

    /// ⌘K "Review approvals": every pending permission request, oldest first; Enter shows the
    /// chat it belongs to, where its card has the buttons.
    pub fn open_approvals_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries: Vec<PickerEntry> = {
            let store = self.sessions.read(cx);
            store
                .approvals()
                .iter()
                .map(|approval| {
                    let session = store.session(approval.session_id);
                    let options: Vec<String> = approval
                        .options
                        .iter()
                        .enumerate()
                        .map(|(i, o)| format!("{} {}", i + 1, o.label))
                        .collect();
                    PickerEntry {
                        icon: crate::icons::Icon::StateApproval,
                        title: approval.title.clone().into(),
                        detail: format!(
                            "{} · {}",
                            session.map_or("a session that is gone", |s| s.title.as_str()),
                            options.join(" · ")
                        )
                        .into(),
                        action: PickerAction::Session(approval.session_id),
                        fixed: false,
                    }
                })
                .collect()
        };
        self.show_picker(PickerState::new(PickerKind::Approvals, entries), window, cx);
    }

    pub fn set_picker_query(&mut self, query: &str, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        picker.query = query.to_owned();
        picker.selected = 0;
        picker.refilter();
        let searching_history = picker.history_query().is_some();
        let quick_open = picker.kind == PickerKind::QuickOpen;
        if searching_history {
            self.search_history_in_picker(cx);
        } else {
            self.picker_search = None;
        }
        if quick_open {
            self.quick_open_find(cx);
        }
        cx.notify();
    }

    pub(crate) fn picker_select(&mut self, next: bool, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.picker {
            let last = picker.matches.len().saturating_sub(1);
            picker.selected = if next {
                (picker.selected + 1).min(last)
            } else {
                picker.selected.saturating_sub(1)
            };
            cx.notify();
        }
    }

    /// Closes the picker and runs the highlighted row (or the one at `index`).
    pub fn picker_confirm(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker_confirm_at(index, super::files::FilePlace::Focused, window, cx);
    }

    /// [`Self::picker_confirm`]; `place` says where a file (or a session, in a split) opens.
    /// On ⌘P's top row, while the answer to what was typed is still on its way, the Return
    /// waits for it: the top row may be about to change.
    pub fn picker_confirm_at(
        &mut self,
        index: Option<usize>,
        place: super::files::FilePlace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        if index.is_none()
            && picker.kind == PickerKind::QuickOpen
            && picker.files.searching
            && picker.selected == 0
        {
            picker.files.pending_confirm = Some(place);
            return;
        }
        let (line, column) = (picker.files.line, picker.files.column);
        let entry = match index {
            Some(row) => picker.entry_at(row),
            None => picker.selected_entry(),
        };
        let Some(action) = entry.map(|e| e.action.clone()) else {
            return;
        };
        self.close_picker(window, cx);
        let split = place == super::files::FilePlace::Split;
        match action {
            PickerAction::OpenFolder => self.open_folder_dialog(window, cx),
            PickerAction::RemoveWorktree(choice) => {
                self.confirm_remove_worktree(choice, window, cx);
            }
            PickerAction::ReviewChanges(path) => self.open_changes(path, window, cx),
            PickerAction::OpenProject(path) => self.open_project_path(path, window, cx),
            PickerAction::Workspace(workspace) => self.switch_workspace(workspace, window, cx),
            PickerAction::Session(session) if split => {
                self.open_session_in_split(session, super::model::Edge::Right, window, cx);
            }
            PickerAction::Session(session) => self.open_session(session, window, cx),
            PickerAction::ResumeHit(resume) => self.resume_history_hit(resume, window, cx),
            PickerAction::ShowHit { query, source, key } => {
                self.show_history_hit(&query, source, &key, window, cx);
            }
            PickerAction::OpenFile(hit) => {
                let target = super::files::FileOpen {
                    path: hit.path,
                    line,
                    column,
                    picked: true,
                };
                self.open_file_placed(target, place, window, cx);
            }
        }
    }

    pub(crate) fn render_picker(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.picker.is_some() {
            self.exits.picker.cancel();
            return self.build_picker(true, cx);
        }
        let now = cx.background_executor().now();
        motion::while_leaving(
            self,
            now,
            |this| &mut this.picker,
            |this| &mut this.exits.picker,
            |this| this.build_picker(false, cx),
        )
        .flatten()
    }

    /// The picker at its current state; `wanted` is false while it leaves (inert, the field
    /// drawn as plain text).
    fn build_picker(&mut self, wanted: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.picker.clone()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        const VISIBLE_ROWS: usize = 12;
        let start = picker.selected.saturating_sub(VISIBLE_ROWS - 1);
        // ⌘P's sections: the files, then the jump list.
        let sections = picker.kind == PickerKind::QuickOpen && picker.history_query().is_none();
        let file_rows = if sections { picker.files.rows } else { 0 };
        let header = |label: &'static str| {
            div()
                .flex()
                .items_end()
                .h(px(tokens.secondary_font_size + 10.0))
                .px(px(tokens.spacing * 2.0))
                .text_size(px(tokens.secondary_font_size))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.muted)
                .child(label)
                .into_any_element()
        };
        let marked = |text: SharedString, marks: &[std::ops::Range<usize>]| -> AnyElement {
            if marks.is_empty() {
                return text.into_any_element();
            }
            let style = gpui::HighlightStyle {
                color: Some(theme.accent),
                font_weight: Some(gpui::FontWeight::SEMIBOLD),
                ..Default::default()
            };
            gpui::StyledText::new(text)
                .with_highlights(marks.iter().map(|r| (r.clone(), style)))
                .into_any_element()
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        for row in (0..picker.matches.len()).skip(start).take(VISIBLE_ROWS) {
            let Some(entry) = picker.entry_at(row) else {
                continue;
            };
            if sections && row == 0 && file_rows > 0 {
                rows.push(header("Files"));
            }
            if sections && row == file_rows {
                rows.push(header("Sessions"));
            }
            let selected = row == picker.selected;
            let hover = theme.surface_hover;
            let (title, detail) = match &entry.action {
                PickerAction::OpenFile(hit) => (
                    marked(entry.title.clone(), &hit.name_marks),
                    marked(entry.detail.clone(), &hit.dir_marks),
                ),
                _ => (
                    entry.title.clone().into_any_element(),
                    entry.detail.clone().into_any_element(),
                ),
            };
            rows.push(
                div()
                    .id(ElementId::from(SharedString::from(format!(
                        "picker-row-{row}"
                    ))))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .h(px(tokens.sidebar_row))
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .when(selected, |s| s.bg(theme.accent_soft))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.picker_confirm(Some(row), window, cx);
                    }))
                    .child(
                        div()
                            .w(px(14.0))
                            .text_color(theme.muted)
                            .child(crate::icons::glyph(entry.icon)),
                    )
                    .child(div().flex_none().max_w(px(260.0)).truncate().child(title))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted)
                            .text_size(px(tokens.secondary_font_size))
                            .child(detail),
                    )
                    .into_any_element(),
            );
        }
        let hint: Option<SharedString> = match (
            picker.history_query().is_some(),
            picker.matches.is_empty(),
            picker.loading,
        ) {
            (_, _, true) if matches!(picker.kind, PickerKind::Worktrees | PickerKind::Changes) => {
                Some("Loading worktrees…".into())
            }
            (_, _, true) => Some("Loading recent projects…".into()),
            (true, true, false) => Some(match &picker.history_state {
                HistoryState::Idle => "Type to search the session history".into(),
                HistoryState::Searching => "Searching…".into(),
                HistoryState::Done => "No session matches".into(),
                HistoryState::Disabled => {
                    "History indexing is off. Enable it in the History panel (command palette → Search History)"
                        .into()
                }
                HistoryState::Unsupported => {
                    "This hutand does not offer history search; restart it".into()
                }
                HistoryState::Failed(error) => format!("History search failed: {error}").into(),
            }),
            (true, false, false) => None,
            (false, true, false) if picker.kind == PickerKind::QuickOpen && picker.files.searching => {
                Some("Searching files…".into())
            }
            (false, true, false) if picker.kind == PickerKind::Projects => {
                Some("No matching project".into())
            }
            (false, true, false) if picker.kind == PickerKind::Approvals => {
                Some(if picker.entries.is_empty() {
                    "No approvals are waiting".into()
                } else {
                    "No approval matches".into()
                })
            }
            (false, true, false) if picker.kind == PickerKind::Worktrees => {
                Some(if picker.entries.is_empty() {
                    "No worktrees to remove".into()
                } else {
                    "No worktree matches".into()
                })
            }
            (false, true, false) if picker.kind == PickerKind::Changes => {
                Some(if picker.entries.is_empty() {
                    "No checkouts to review".into()
                } else {
                    "No checkout matches".into()
                })
            }
            (false, true, false) => Some(match &picker.files.failed {
                Some(error) if picker.kind == PickerKind::QuickOpen => {
                    format!("Cannot search the files: {error}").into()
                }
                _ => "Nothing matches".into(),
            }),
            (false, false, false) => None,
        };
        let prompt = match (picker.kind, picker.history_query().is_some()) {
            (PickerKind::Projects, _) => "Open project ›",
            (PickerKind::Jump | PickerKind::QuickOpen, true) => "Search history ›",
            (PickerKind::Jump, false) => "Jump to ›",
            (PickerKind::QuickOpen, false) => "Go to ›",
            (PickerKind::Approvals, _) => "Review approvals ›",
            (PickerKind::Worktrees, _) => "Remove worktree ›",
            (PickerKind::Changes, _) => "Review changes of ›",
        };

        Some(
            div()
                .id("picker-backdrop")
                .absolute()
                .inset_0()
                .when(wanted, |s| s.shield_behind())
                .flex()
                .justify_center()
                .items_start()
                .pt(px(80.0))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_picker(window, cx)),
                )
                .child(
                    div()
                        .id("picker")
                        .key_context(super::vim_ui::vim_key_context(
                            "Picker",
                            self.overlay_vim_mode(super::vim_ui::OverlayField::Picker),
                        ))
                        .when(wanted, |s| s.track_focus(&self.picker_focus))
                        .w(px(560.0))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing))
                        .popover(&theme, &tokens)
                        .p(px(tokens.spacing * 2.0))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            // Typed text arrives through the field's platform text input.
                            if this.overlay_key(OverlayField::Picker, event, cx) {
                                cx.stop_propagation();
                            }
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectNext, _, cx| {
                            this.picker_select(true, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectPrev, _, cx| {
                            this.picker_select(false, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::Confirm, window, cx| {
                            this.picker_confirm(None, window, cx);
                        }))
                        .on_action(
                            cx.listener(|this, _: &palette::ConfirmInSplit, window, cx| {
                                this.picker_confirm_at(
                                    None,
                                    super::files::FilePlace::Split,
                                    window,
                                    cx,
                                );
                            }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .h(px(tokens.sidebar_row + 6.0))
                                .px(px(tokens.spacing * 2.0))
                                .border_b_1()
                                .border_color(theme.border)
                                .child(div().text_color(theme.muted).child(format!("{prompt} ")))
                                .children(if wanted {
                                    self.overlay_parts(OverlayField::Picker, "", cx)
                                } else {
                                    vec![div().child(picker.query.clone()).into_any_element()]
                                }),
                        )
                        .children(rows)
                        .when_some(hint, |s, hint| {
                            s.child(
                                div()
                                    .px(px(tokens.spacing * 2.0))
                                    .text_color(theme.muted)
                                    .child(hint),
                            )
                        })
                        .presented(
                            "picker-motion",
                            motion::Kind::Popover,
                            wanted,
                            motion::Slide::Drop,
                        )
                        .inert(!wanted),
                )
                .into_any_element(),
        )
    }
}

/// Why `workspace` cannot be closed, if it cannot.
pub(super) fn close_blocker(workspace: &WorkspaceInfo) -> Option<String> {
    if workspace.kind == WorkspaceKind::Home {
        return Some("Home cannot be closed".to_owned());
    }
    match workspace.live_sessions {
        0 => None,
        1 => Some("1 live session".to_owned()),
        n => Some(format!("{n} live sessions")),
    }
}

/// The root of a project: its repository's main checkout, or its folder.
pub(crate) fn project_root_of(info: &WorkspaceInfo) -> Option<PathBuf> {
    info.project
        .as_ref()
        .map(|p| p.root.clone())
        .or_else(|| info.roots.first().cloned())
}

/// The roots whose `.hutan/settings.toml` files layer over the user's settings for `container`
/// (what is shown) and `project` (the one worked in, inside a workspace): `(workspace layer,
/// project layer)`. A workspace's layer is its first project's root and Home's is its folder;
/// a project in no workspace layers only its own file; the project layer is left out when it
/// would read the file the workspace layer already reads.
pub(crate) fn layer_roots(
    container: Option<&WorkspaceInfo>,
    project: Option<&WorkspaceInfo>,
) -> (Option<PathBuf>, Option<PathBuf>) {
    let Some(container) = container else {
        return (None, None);
    };
    let (workspace, project) = match container.kind {
        WorkspaceKind::Workspace => (
            container.roots.first().cloned(),
            project.and_then(project_root_of),
        ),
        WorkspaceKind::Project => (None, project_root_of(container)),
        WorkspaceKind::Home => (container.roots.first().cloned(), None),
    };
    let project = project.filter(|root| Some(root) != workspace.as_ref());
    (workspace, project)
}

/// [`layer_roots`] for the container a new session of `id` belongs to.
pub(crate) fn layer_roots_of(
    store: &crate::sessions::SessionsStore,
    id: WorkspaceId,
) -> (Option<PathBuf>, Option<PathBuf>) {
    match store.workspace(id) {
        Some(project) if project.kind == WorkspaceKind::Project => {
            let parent = project.parent_id.and_then(|parent| store.workspace(parent));
            match parent {
                Some(parent) => layer_roots(Some(parent), Some(project)),
                None => layer_roots(Some(project), None),
            }
        }
        other => layer_roots(other, None),
    }
}
