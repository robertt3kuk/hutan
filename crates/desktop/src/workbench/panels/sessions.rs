//! The `sessions` panel ("Projects"): user workspaces → their projects → sessions, the projects
//! in no workspace, then Home (or a flat list of projects, `sidebar.organize`); sorted, pinned,
//! filtered and searched; "Needs you" on top; every session row shows its live folder and its
//! placement badge. Workspaces and projects are managed here: the "+" menu, drag a project onto a
//! workspace (or out, to "No workspace"), ⌘-click / ⇧-click to select projects, inline rename,
//! context menus (`workbench/projects.rs` holds the verbs).

use crate::theme::DensityExt as _;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, ListAlignment, ListState, MouseButton, MouseDownEvent, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div, prelude::*,
    px,
};
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::messages::{SessionInfo, WorkspaceInfo, WorkspaceKind};

use super::common::{
    age_label, agent_badge, kind_icon, panel_header_with, placement, placement_badge,
    recovery_chip, state_dot, time_tooltip, tone_color,
};
use crate::icons::{Icon, icon};
use crate::sessions::tree::{self, Node, ProjectNode, folder_label, session_folder};
use crate::settings::{
    GroupBy, SessionsPanelSettings, SidebarSettings, WorkspaceSort, WorkspacesSettings,
};
use crate::workbench::actions::{
    CloseSessionById, DropProjectOnto, MoveWorkspaceOnto, OpenFolder, OpenSession, OpenWorkspace,
    ProjectDrop, ProjectMenuAt, RenameTo, RenameWorkspaceTo, SessionMenuAt, ShowPlusMenu,
    ShowProjectMenu, ShowSessionMenu, ShowSortMenu, ShowWorkspaceMenu, WorkspaceDrop,
    WorkspaceMenuAt,
};
use crate::workbench::actions::{LauncherOpen, OpenLauncher};
use crate::workbench::chat::vim::Outcome;
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::{TextTooltip, UiStyle};
use crate::workbench::text_field::FieldRef;
use crate::workbench::ui::{self, ButtonKind, Mascot, StyledExt as _};
use crate::workbench::vim_ui::{self, FieldEdit, VimConfig};
use crate::workbench::{BadgeSite, DragPreview, Region};

/// Row refresh for the relative ages.
const AGE_REFRESH: Duration = Duration::from_secs(30);

/// Indent per level of the tree.
const INDENT: f32 = 12.0;
/// Orca's project card: the leading status lane is 20 px wide, titles are 13 px.
const STATUS_LANE: f32 = 20.0;
const CARD_TITLE: f32 = 13.0;

/// Hover fill of a card: the foreground at 4 % in light, the left sidebar's accent at 40 % in
/// dark (Orca's workspace hover).
fn card_hover(theme: &crate::theme::Theme) -> gpui::Hsla {
    if theme.dark {
        theme.sidebar_accent.opacity(0.4)
    } else {
        theme.fg.opacity(0.04)
    }
}

/// A workspace row being dragged to a new place (manual order).
#[derive(Clone, Debug)]
pub struct DraggedWorkspace {
    pub workspace: WorkspaceId,
    pub label: SharedString,
}

/// A project row being dragged onto a workspace, a project, or the "No workspace" area.
#[derive(Clone, Debug)]
pub struct DraggedProject {
    pub project: WorkspaceId,
    pub label: SharedString,
}

/// What a listed row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// "Needs you · 2", a state label.
    Section,
    Workspace,
    Project,
    Home,
    /// The "No workspace" label above the projects that belong to none.
    NoWorkspace,
    Session,
}

/// A listed row as text (tests, inspection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub kind: RowKind,
    /// 0 at the top level; a project in a workspace is 1, its sessions 2.
    pub depth: usize,
    /// The name of a workspace, project or Home; the title of a session; the label of a
    /// section.
    pub title: String,
    /// A project's root folder, or where a session is now (last two path parts).
    pub folder: Option<String>,
    /// A session's placement badge text (`main`, `wt <branch>`).
    pub badge: Option<String>,
    /// The badge is drawn as a warning: another live agent works in the same checkout.
    pub shared: bool,
    pub pinned: bool,
    /// `3` or `1/3` (live/total) on a workspace, project or Home row.
    pub count: Option<String>,
    /// Sessions of the row (and under it) that need the user.
    pub needs: usize,
    /// The row of the container or project the window shows.
    pub active: bool,
    pub selected: bool,
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SectionKey {
    NeedsYou,
    All,
    State(String),
}

/// A workspace, project or Home row with what the layout decided about it.
#[derive(Debug, Clone, PartialEq)]
struct ContainerRow {
    info: WorkspaceInfo,
    depth: usize,
    needs: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct SessionRow {
    info: SessionInfo,
    depth: usize,
    in_needs_you: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Entry {
    Section {
        key: SectionKey,
        title: String,
    },
    Container(ContainerRow),
    NoWorkspace,
    Session(Box<SessionRow>),
    /// "No projects yet": under Home when nothing else is listed.
    EmptyHint,
}

/// What the list's rows need besides their entry, fixed for one frame.
#[derive(Clone)]
struct RowContext {
    entries: Rc<Vec<Entry>>,
    settings: Arc<SessionsPanelSettings>,
    focused: Option<SessionId>,
    active: Option<WorkspaceId>,
    active_project: Option<WorkspaceId>,
    manual: bool,
    multi: bool,
}

/// What the entries are a function of: when none of it changed, the entries are the same.
struct LayoutKey {
    /// Bumped whenever the sessions store notifies.
    store: u64,
    query: String,
    collapsed: HashSet<WorkspaceId>,
    settings: (
        Arc<SessionsPanelSettings>,
        Arc<WorkspacesSettings>,
        Arc<SidebarSettings>,
    ),
}

impl LayoutKey {
    fn same_as(&self, other: &Self) -> bool {
        self.store == other.store
            && self.query == other.query
            && self.collapsed == other.collapsed
            && Arc::ptr_eq(&self.settings.0, &other.settings.0)
            && Arc::ptr_eq(&self.settings.1, &other.settings.1)
            && Arc::ptr_eq(&self.settings.2, &other.settings.2)
    }
}

/// An inline rename in progress; losing focus confirms it.
struct Renaming {
    workspace: WorkspaceId,
    text: String,
    _blur: Subscription,
}

pub struct SessionsPanel {
    env: PanelEnv,
    query: String,
    /// Vim state of the filter field and of the rename field (`editor.vim_mode`).
    query_edit: FieldEdit,
    rename_edit: FieldEdit,
    search_focus: FocusHandle,
    rename_focus: FocusHandle,
    collapsed: HashSet<WorkspaceId>,
    /// Projects selected with ⌘-click / ⇧-click.
    selection: Vec<WorkspaceId>,
    anchor: Option<WorkspaceId>,
    renaming: Option<Renaming>,
    /// Counts the store's notifications: part of what the cached entries are keyed by.
    store_rev: u64,
    /// Drawn since the age timer last looked.
    drawn: bool,
    /// The entries of the last layout and what they were made from. A frame never rebuilds
    /// them (that clones every session of the daemon); a change of the store, the filter, the
    /// folds or the settings does.
    layout_cache: RefCell<Option<(LayoutKey, Rc<Vec<Entry>>)>>,
    layout_builds: Cell<usize>,
    /// The virtualized list: only the rows in view (and a little around) are built and laid out.
    list: ListState,
    /// The entries the list's heights are for, and the filter they were laid out under.
    listed: Rc<Vec<Entry>>,
    listed_query: String,
    rows: RowContext,
    rows_rendered: Cell<usize>,
    _ages: Task<()>,
}

impl std::fmt::Debug for SessionsPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionsPanel")
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

impl SessionsPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |this, _, cx| {
            this.store_rev += 1;
            cx.notify();
        })
        .detach();
        let ages = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AGE_REFRESH).await;
                // Nobody sees the ages of a panel that was not drawn since the last look (its
                // dock is hidden, another panel is on top): no frame for them.
                let alive = this.update(cx, |this, cx| {
                    if std::mem::take(&mut this.drawn) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            env,
            query: String::new(),
            query_edit: FieldEdit::new(),
            rename_edit: FieldEdit::new(),
            search_focus: cx.focus_handle(),
            rename_focus: cx.focus_handle(),
            collapsed: HashSet::new(),
            selection: Vec::new(),
            anchor: None,
            renaming: None,
            store_rev: 0,
            drawn: false,
            layout_cache: RefCell::new(None),
            layout_builds: Cell::new(0),
            list: ListState::new(0, ListAlignment::Top, px(300.0)),
            listed: Rc::new(Vec::new()),
            listed_query: String::new(),
            rows: RowContext {
                entries: Rc::new(Vec::new()),
                settings: Arc::new(SessionsPanelSettings::default()),
                focused: None,
                active: None,
                active_project: None,
                manual: false,
                multi: false,
            },
            rows_rendered: Cell::new(0),
            _ages: ages,
        }
    }

    /// How many times the entries were built (tests: a frame must not build them).
    pub fn layout_builds(&self) -> usize {
        self.layout_builds.get()
    }

    /// How many rows were built for the list so far (tests: only the rows in view are).
    pub fn rows_rendered(&self) -> usize {
        self.rows_rendered.get()
    }

    /// The search text (tests, inspection).
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Sets the search text.
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.query = query.to_owned();
        cx.notify();
    }

    /// Titles of the session rows currently listed, top to bottom (needs-you first).
    pub fn visible_titles(&self, cx: &gpui::App) -> Vec<String> {
        self.visible_rows(cx)
            .into_iter()
            .filter(|row| row.kind == RowKind::Session)
            .map(|row| row.title)
            .collect()
    }

    /// Names of the workspaces, projects and Home currently listed, top to bottom, whatever
    /// the mode orders them by.
    pub fn visible_workspaces(&self, cx: &gpui::App) -> Vec<String> {
        self.visible_rows(cx)
            .into_iter()
            .filter(|row| {
                matches!(
                    row.kind,
                    RowKind::Workspace | RowKind::Project | RowKind::Home
                )
            })
            .map(|row| row.title)
            .collect()
    }

    /// Every row currently listed, top to bottom.
    pub fn visible_rows(&self, cx: &gpui::App) -> Vec<SidebarRow> {
        let store = self.env.sessions.read(cx);
        let (active, project) = (store.active_workspace_id(), store.active_project_id());
        self.entries(cx)
            .iter()
            .filter_map(|entry| {
                Some(match entry {
                    Entry::EmptyHint => return None,
                    Entry::Section { title, .. } => SidebarRow {
                        kind: RowKind::Section,
                        depth: 0,
                        title: title.clone(),
                        folder: None,
                        badge: None,
                        shared: false,
                        pinned: false,
                        count: None,
                        needs: 0,
                        active: false,
                        selected: false,
                        collapsed: false,
                    },
                    Entry::NoWorkspace => SidebarRow {
                        kind: RowKind::NoWorkspace,
                        depth: 0,
                        title: "No workspace".to_owned(),
                        folder: None,
                        badge: None,
                        shared: false,
                        pinned: false,
                        count: None,
                        needs: 0,
                        active: false,
                        selected: false,
                        collapsed: false,
                    },
                    Entry::Container(row) => SidebarRow {
                        kind: match row.info.kind {
                            WorkspaceKind::Workspace => RowKind::Workspace,
                            WorkspaceKind::Project => RowKind::Project,
                            WorkspaceKind::Home => RowKind::Home,
                        },
                        depth: row.depth,
                        title: row.info.name.clone(),
                        folder: (row.info.kind == WorkspaceKind::Project)
                            .then(|| crate::workbench::workspaces::project_root_of(&row.info))
                            .flatten()
                            .map(|root| folder_label(&root)),
                        badge: None,
                        shared: false,
                        pinned: row.info.pinned,
                        count: count_text(&row.info),
                        needs: row.needs,
                        active: self.is_active(&row.info, active, project),
                        selected: self.selection.contains(&row.info.id),
                        collapsed: self.collapsed.contains(&row.info.id),
                    },
                    Entry::Session(row) => {
                        let badge = crate::sessions::placement_badge(&row.info);
                        SidebarRow {
                            kind: RowKind::Session,
                            depth: row.depth,
                            title: row.info.title.clone(),
                            folder: session_folder(&row.info).map(folder_label),
                            shared: badge.as_ref().is_some_and(|b| b.shared),
                            badge: badge.map(|b| b.text),
                            pinned: false,
                            count: None,
                            needs: 0,
                            active: false,
                            selected: store.focused() == Some(row.info.id),
                            collapsed: false,
                        }
                    }
                })
            })
            .collect()
    }

    /// Whether `info`'s row is the one the window shows: the project the user works in, else the
    /// container (a workspace without an active project of its own, Home, a project).
    fn is_active(
        &self,
        info: &WorkspaceInfo,
        container: Option<WorkspaceId>,
        project: Option<WorkspaceId>,
    ) -> bool {
        match info.kind {
            WorkspaceKind::Project => project == Some(info.id),
            WorkspaceKind::Workspace => container == Some(info.id) && project.is_none(),
            WorkspaceKind::Home => container == Some(info.id),
        }
    }

    // ---- selection and rename -------------------------------------------------------------

    /// The projects selected with ⌘-click and ⇧-click, in the order of the list.
    pub fn selected_projects(&self) -> Vec<WorkspaceId> {
        self.selection.clone()
    }

    /// Forgets the selection.
    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if !self.selection.is_empty() {
            self.selection.clear();
            cx.notify();
        }
    }

    /// ⌘-click toggles `project` in the selection, ⇧-click selects the projects between the
    /// last one clicked and `project`.
    pub fn select_project(&mut self, project: WorkspaceId, range: bool, cx: &mut Context<Self>) {
        let order = self.project_order(cx);
        match (range, self.anchor.filter(|a| order.contains(a))) {
            (true, Some(anchor)) => {
                let (from, to) = (
                    order.iter().position(|p| *p == anchor),
                    order.iter().position(|p| *p == project),
                );
                if let (Some(from), Some(to)) = (from, to) {
                    let (low, high) = (from.min(to), from.max(to));
                    self.selection = order[low..=high].to_vec();
                }
            }
            _ => {
                if let Some(at) = self.selection.iter().position(|p| *p == project) {
                    self.selection.remove(at);
                } else {
                    self.selection.push(project);
                    self.selection
                        .sort_by_key(|p| order.iter().position(|o| o == p));
                }
                self.anchor = Some(project);
            }
        }
        cx.notify();
    }

    /// The listed projects, top to bottom.
    fn project_order(&self, cx: &gpui::App) -> Vec<WorkspaceId> {
        self.entries(cx)
            .iter()
            .filter_map(|entry| match entry {
                Entry::Container(row) if row.info.kind == WorkspaceKind::Project => {
                    Some(row.info.id)
                }
                _ => None,
            })
            .collect()
    }

    /// Starts editing the name of `workspace`'s row in place; `false` when the row is not
    /// listed. Enter confirms, Escape cancels, and so does moving focus elsewhere… which
    /// confirms (the way Finder does).
    pub fn begin_rename(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(name) = self
            .env
            .sessions
            .read(cx)
            .workspace(workspace)
            .map(|w| w.name.clone())
        else {
            return false;
        };
        let blur = cx.on_focus_out(&self.rename_focus, window, |this, _, window, cx| {
            this.finish_rename(true, window, cx);
        });
        self.rename_edit.reset_selected(&name);
        self.renaming = Some(Renaming {
            workspace,
            text: name,
            _blur: blur,
        });
        cx.defer_in(window, |this, window, cx| {
            if this.renaming.is_some() {
                window.focus(&this.rename_focus, cx);
            }
        });
        cx.notify();
        true
    }

    /// The row being renamed and the text typed so far.
    pub fn renaming(&self) -> Option<(WorkspaceId, &str)> {
        self.renaming
            .as_ref()
            .map(|r| (r.workspace, r.text.as_str()))
    }

    /// Whether the inline rename opened with the whole old name selected and nothing changed it
    /// yet.
    pub fn rename_selected(&self) -> bool {
        self.renaming
            .as_ref()
            .is_some_and(|r| self.rename_edit.selected(&r.text))
    }

    /// Ends the inline rename: `confirm` sends the new name, else nothing changes.
    fn finish_rename(&mut self, confirm: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        let name = renaming.text.trim().to_owned();
        let changed = self
            .env
            .sessions
            .read(cx)
            .workspace(renaming.workspace)
            .is_none_or(|w| w.name != name);
        // The workbench takes the focus back either way (an empty name only does that).
        let name = if confirm && changed {
            name
        } else {
            String::new()
        };
        window.dispatch_action(
            Box::new(RenameWorkspaceTo(RenameTo {
                workspace: renaming.workspace.to_string(),
                name,
            })),
            cx,
        );
        cx.notify();
    }

    fn on_rename_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let vim = VimConfig::of(&self.env.settings);
        let Some(renaming) = &mut self.renaming else {
            return;
        };
        // The old name starts selected: typing or pasting replaces it, Backspace clears it, ⌘A
        // selects it again. Typed text arrives through the field's platform text input.
        if self
            .rename_edit
            .key(&mut renaming.text, event, vim, true, cx)
            == Outcome::Handled
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Enter and Escape (in Normal mode, under Vim mode) fall through to here.
        match event.keystroke.key.as_str() {
            "enter" => self.finish_rename(true, window, cx),
            "escape" => self.finish_rename(false, window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    // ---- the list -----------------------------------------------------------------------------

    fn matches(&self, info: &SessionInfo) -> bool {
        let query = self.query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let haystack = format!(
            "{} {} {} {} {}",
            info.title,
            info.command.join(" "),
            placement(info),
            crate::sessions::placement_badge(info)
                .map(|badge| badge.text)
                .unwrap_or_default(),
            session_folder(info)
                .map(|c| c.display().to_string())
                .unwrap_or_default()
        )
        .to_lowercase();
        query.split_whitespace().all(|word| haystack.contains(word))
    }

    /// The entries in display order (see [`build_layout`](Self::build_layout)), from the cache
    /// while nothing they are made from changed.
    fn entries(&self, cx: &gpui::App) -> Rc<Vec<Entry>> {
        let key = LayoutKey {
            store: self.store_rev,
            query: self.query.clone(),
            collapsed: self.collapsed.clone(),
            settings: (
                self.env.settings.get::<SessionsPanelSettings>(),
                self.env.settings.get::<WorkspacesSettings>(),
                self.env.settings.get::<SidebarSettings>(),
            ),
        };
        if let Some((cached, entries)) = &*self.layout_cache.borrow()
            && cached.same_as(&key)
        {
            return entries.clone();
        }
        let entries = Rc::new(self.build_layout(&key.settings.0, cx));
        self.layout_builds.set(self.layout_builds.get() + 1);
        *self.layout_cache.borrow_mut() = Some((key, entries.clone()));
        entries
    }

    /// Entries in display order, after the filters, the search and the grouping.
    fn build_layout(&self, settings: &SessionsPanelSettings, cx: &gpui::App) -> Vec<Entry> {
        let store = self.env.sessions.read(cx);
        let ws_settings = self.env.settings.get::<WorkspacesSettings>();
        let organize = self.env.settings.get::<SidebarSettings>().organize;
        let (active, project) = (store.active_workspace_id(), store.active_project_id());
        let needs: Vec<SessionId> = store.needs_you();
        let attention = store.needs_you_by_workspace();
        // A session is listed by the search when it matches itself or sits in a project or
        // workspace whose name or folder does.
        let listed = |s: &SessionInfo| {
            self.matches(s)
                || store.workspace(s.workspace_id).is_some_and(|w| {
                    crate::sessions::workspaces::matches_query(w, &self.query)
                        || w.parent_id
                            .and_then(|p| store.workspace(p))
                            .is_some_and(|p| {
                                crate::sessions::workspaces::matches_query(p, &self.query)
                            })
                })
        };
        let mut entries = Vec::new();
        let (needing, rest): (Vec<&SessionInfo>, Vec<&SessionInfo>) = if settings.needs_you {
            store
                .sessions()
                .iter()
                .filter(|s| listed(s))
                .partition(|s| needs.contains(&s.id))
        } else {
            (
                Vec::new(),
                store.sessions().iter().filter(|s| listed(s)).collect(),
            )
        };
        // The sessions the tree decides about itself (it knows which containers the search
        // matches); the ones "Needs you" took are not among them.
        let in_tree: Vec<&SessionInfo> = store
            .sessions()
            .iter()
            .filter(|s| !(settings.needs_you && needs.contains(&s.id)))
            .collect();
        if !needing.is_empty() {
            entries.push(Entry::Section {
                key: SectionKey::NeedsYou,
                title: format!("Needs you · {}", needing.len()),
            });
            entries.extend(needing.into_iter().map(|info| {
                Entry::Session(Box::new(SessionRow {
                    info: info.clone(),
                    depth: 0,
                    in_needs_you: true,
                }))
            }));
        }
        match settings.group_by {
            GroupBy::None => {
                if !rest.is_empty() {
                    entries.push(Entry::Section {
                        key: SectionKey::All,
                        title: String::new(),
                    });
                    entries.extend(rest.into_iter().map(|info| {
                        Entry::Session(Box::new(SessionRow {
                            info: info.clone(),
                            depth: 0,
                            in_needs_you: false,
                        }))
                    }));
                }
            }
            GroupBy::State => {
                let mut labels: Vec<String> = Vec::new();
                for info in &rest {
                    let label = store.presentation(info).label;
                    if !labels.contains(&label) {
                        labels.push(label);
                    }
                }
                for label in labels {
                    let rows: Vec<&&SessionInfo> = rest
                        .iter()
                        .filter(|s| store.presentation(s).label == label)
                        .collect();
                    entries.push(Entry::Section {
                        key: SectionKey::State(label.clone()),
                        title: format!("{label} · {}", rows.len()),
                    });
                    entries.extend(rows.into_iter().map(|info| {
                        Entry::Session(Box::new(SessionRow {
                            info: (*info).clone(),
                            depth: 0,
                            in_needs_you: false,
                        }))
                    }));
                }
            }
            GroupBy::Workspace => {
                let query = self.query.clone();
                let by_itself = |s: &SessionInfo| self.matches(s);
                let built = tree::build(&tree::Inputs {
                    workspaces: store.workspaces(),
                    sessions: &in_tree,
                    organize,
                    sort: ws_settings.sort,
                    filters: &ws_settings,
                    query: &query,
                    active: [active, project],
                    needs: &attention,
                    session_matches: &by_itself,
                });
                self.flatten(&built.nodes, &mut entries);
            }
        }
        let only_home = store.sessions().is_empty()
            && store
                .workspaces()
                .iter()
                .all(|w| w.kind == WorkspaceKind::Home);
        if only_home && self.query.trim().is_empty() && !entries.is_empty() {
            entries.push(Entry::EmptyHint);
        }
        entries
    }

    /// Brings the list's row heights in line with the entries: a filter that changed starts the
    /// list over (from the top); otherwise only the rows that differ are measured again, so
    /// that the reader keeps their place while sessions come and go.
    fn sync_list(&mut self, entries: &Rc<Vec<Entry>>) {
        if Rc::ptr_eq(&self.listed, entries) {
            return;
        }
        let old = std::mem::replace(&mut self.listed, entries.clone());
        if self.listed_query != self.query {
            self.listed_query.clone_from(&self.query);
            self.list.reset(entries.len());
            return;
        }
        let prefix = old
            .iter()
            .zip(entries.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let room = old.len().min(entries.len()) - prefix;
        let suffix = old
            .iter()
            .rev()
            .zip(entries.iter().rev())
            .take(room)
            .take_while(|(a, b)| a == b)
            .count();
        let (removed, added) = (old.len() - prefix - suffix, entries.len() - prefix - suffix);
        if removed > 0 || added > 0 {
            self.list.splice(prefix..prefix + removed, added);
        }
    }

    /// The rows of the tree, honouring what the user folded.
    fn flatten(&self, nodes: &[Node], entries: &mut Vec<Entry>) {
        let session = |entries: &mut Vec<Entry>, list: &[SessionInfo], depth: usize| {
            entries.extend(list.iter().map(|info| {
                Entry::Session(Box::new(SessionRow {
                    info: info.clone(),
                    depth,
                    in_needs_you: false,
                }))
            }));
        };
        let project = |entries: &mut Vec<Entry>, node: &ProjectNode, depth: usize| {
            entries.push(Entry::Container(ContainerRow {
                info: node.info.clone(),
                depth,
                needs: node.needs,
            }));
            if !self.collapsed.contains(&node.info.id) {
                session(entries, &node.sessions, depth + 1);
            }
        };
        for node in nodes {
            match node {
                Node::Workspace(workspace) => {
                    entries.push(Entry::Container(ContainerRow {
                        info: workspace.info.clone(),
                        depth: 0,
                        needs: workspace.needs,
                    }));
                    if !self.collapsed.contains(&workspace.info.id) {
                        for node in &workspace.projects {
                            project(entries, node, 1);
                        }
                        session(entries, &workspace.sessions, 1);
                    }
                }
                Node::Project(node) | Node::Home(node) => project(entries, node, 0),
                Node::NoWorkspace => entries.push(Entry::NoWorkspace),
            }
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let vim = VimConfig::of(&self.env.settings);
        // Vim's commands and the editing keys; typed text arrives through the field's platform
        // text input (`search_field`).
        if self.query_edit.key(&mut self.query, event, vim, true, cx) == Outcome::Handled {
            cx.stop_propagation();
            cx.notify();
        } else if event.keystroke.key == "escape" {
            // Escape (in Normal mode, under Vim mode) clears the filter.
            self.query.clear();
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// How the filter's element reaches its text: the platform's text input and the mouse.
    fn search_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.search_focus.clone(),
            |this, cx, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                this.query_edit.with(&mut this.query, vim_on, false, f);
                cx.notify();
            },
        )
    }

    /// How the rename's element reaches its text.
    fn rename_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.rename_focus.clone(),
            |this, cx, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                if let Some(renaming) = &mut this.renaming {
                    this.rename_edit.with(&mut renaming.text, vim_on, false, f);
                }
                cx.notify();
            },
        )
    }

    /// A project row was dropped on `target` (a workspace or project row, or `None` for the "No
    /// workspace" area); the whole selection moves when the row is part of it.
    fn project_dropped(
        &mut self,
        dragged: &DraggedProject,
        target: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let projects = if self.selection.len() > 1 && self.selection.contains(&dragged.project) {
            self.selection.clone()
        } else {
            vec![dragged.project]
        };
        for project in projects {
            window.dispatch_action(
                Box::new(DropProjectOnto(ProjectDrop {
                    dragged: project.to_string(),
                    target: target.map(|t| t.to_string()),
                })),
                cx,
            );
        }
    }
}

/// `3`, or `1/3` when only some of a row's sessions are live; nothing for a row without
/// sessions.
fn count_text(info: &WorkspaceInfo) -> Option<String> {
    match (info.live_sessions, info.total_sessions) {
        (_, 0) => None,
        (live, total) if live == total => Some(total.to_string()),
        (live, total) => Some(format!("{live}/{total}")),
    }
}

impl Focusable for SessionsPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.search_focus.clone()
    }
}

impl Render for SessionsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let settings = self.env.settings.get::<SessionsPanelSettings>();
        let ws_settings = self.env.settings.get::<WorkspacesSettings>();
        self.drawn = true;
        let entries = self.entries(cx);
        self.sync_list(&entries);
        let (focused, active, active_project) = {
            let store = self.env.sessions.read(cx);
            (
                store.focused(),
                store.active_workspace_id(),
                store.active_project_id(),
            )
        };
        // The selection and the rename only name rows that still exist.
        {
            let store = self.env.sessions.read(cx);
            let before = self.selection.len();
            self.selection.retain(|id| store.workspace(*id).is_some());
            if self.selection.len() != before {
                cx.notify();
            }
        }
        let searching = self.search_focus.is_focused(window);
        let hover = theme.surface_hover;
        let manual = ws_settings.sort == WorkspaceSort::Manual;
        let hint_query = !self.query.trim().is_empty();
        self.rows = RowContext {
            entries: entries.clone(),
            settings,
            focused,
            active,
            active_project,
            manual,
            multi: self.selection.len() > 1,
        };

        let list = div()
            .id("sessions-list")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col();
        let list = if entries.is_empty() {
            list.child(
                ui::empty_state(
                    Mascot::Empty,
                    if hint_query {
                        "Nothing matches the filter."
                    } else {
                        "Nothing here yet. ⌘T starts a terminal."
                    },
                    None,
                    &theme,
                    &tokens,
                )
                .h(px(260.0)),
            )
        } else {
            // Only the rows in view are built: a frame costs what the window shows, not what the
            // daemon has.
            list.child(gpui::list(self.list.clone(), cx.processor(Self::render_entry)).size_full())
        };

        let sort_button = div()
            .id("workspaces-sort")
            .px(px(tokens.spacing * 1.5))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .hover(move |s| s.bg(hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|event, window, cx| {
                window.dispatch_action(Box::new(ShowSortMenu(event.position())), cx);
            })
            .flex()
            .items_center()
            .gap(px(4.0))
            .min_w_0()
            .child(icon(Icon::ArrowUpDown, 12.0, theme.muted))
            .child(
                div()
                    .min_w_0()
                    .max_w(px(80.0))
                    .truncate()
                    .child(ws_settings.sort.label().to_string()),
            )
            .child(icon(Icon::ChevronDown, 12.0, theme.muted))
            .into_any_element();
        let plus_button = div()
            .id("sessions-plus")
            .relative()
            .flex_none()
            .px(px(tokens.spacing * 1.5))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .text_color(theme.muted)
            .hover(move |s| s.bg(hover).text_color(theme.fg))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|event, window, cx| {
                window.dispatch_action(Box::new(ShowPlusMenu(event.position())), cx);
            })
            .child(icon(Icon::Plus, 14.0, theme.muted))
            .child(probe(&self.env.regions, Region::SidebarControl("plus")))
            .into_any_element();
        let start_button = div()
            .id("sessions-start")
            .relative()
            .flex_none()
            .whitespace_nowrap()
            .px(px(tokens.spacing * 1.5))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .text_color(theme.accent)
            .hover(move |s| s.bg(theme.accent_soft))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(OpenLauncher(LauncherOpen::default())), cx);
            })
            .child("▶ Start")
            .child(probe(&self.env.regions, Region::SidebarControl("start")))
            .into_any_element();

        let search = div()
            .id("sessions-search")
            .relative()
            .key_context("SessionsSearch")
            .track_focus(&self.search_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .h(px(tokens.sidebar_row))
            .flex()
            .items_center()
            .rounded(px(tokens.radius))
            .bg(theme.surface)
            .border_1()
            .border_color(if searching {
                theme.accent
            } else {
                theme.border
            })
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, window, cx| {
                    // The dock re-focuses the workbench on this same mouse down; take focus
                    // after the event has been handled.
                    cx.defer_in(window, |this, window, cx| {
                        window.focus(&this.search_focus, cx);
                    });
                }),
            )
            .children(vim_ui::string_field(
                "sessions-search-text",
                &self.query_edit,
                &self.query,
                VimConfig::of(&self.env.settings).enabled,
                "⌕ Filter projects and sessions",
                searching,
                false,
                self.search_field(cx),
                &theme,
                &tokens,
            ))
            .child(probe(&self.env.regions, Region::SidebarControl("filter")));

        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(panel_header_with(
                "Projects",
                "panels.sessions",
                vec![start_button, plus_button, sort_button],
                cx,
            ))
            .child(search)
            .child(list)
    }
}

impl SessionsPanel {
    /// One row of the list: what entry `ix` of the frame's entries is.
    fn render_entry(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.rows_rendered.set(self.rows_rendered.get() + 1);
        let rows = self.rows.clone();
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let Some(entry) = rows.entries.get(ix) else {
            return div().into_any_element();
        };
        match entry {
            Entry::Section { key, title } => {
                if title.is_empty() {
                    return div().into_any_element();
                }
                let color = if *key == SectionKey::NeedsYou {
                    theme.warning
                } else {
                    theme.muted
                };
                div()
                    .id(SharedString::from(format!("section-{key:?}")))
                    .flex()
                    .items_center()
                    .h(px(tokens.sidebar_row))
                    .px(px(tokens.spacing * 2.0))
                    .mt(px(tokens.spacing))
                    .text_size(px(tokens.section_font_size))
                    .medium()
                    .text_color(color)
                    .child(title.clone())
                    .into_any_element()
            }
            Entry::NoWorkspace => self.render_no_workspace(cx),
            Entry::Container(row) => {
                let current = self.is_active(&row.info, rows.active, rows.active_project);
                let renaming = self
                    .renaming
                    .as_ref()
                    .filter(|r| r.workspace == row.info.id)
                    .map(|r| r.text.clone());
                self.render_container(
                    row,
                    self.collapsed.contains(&row.info.id),
                    current,
                    rows.multi && self.selection.contains(&row.info.id),
                    rows.manual,
                    renaming,
                    cx,
                )
            }
            Entry::Session(row) => {
                self.render_row(row, &rows.settings, rows.focused == Some(row.info.id), cx)
            }
            Entry::EmptyHint => ui::empty_state(
                Mascot::Empty,
                "No projects yet. Open a folder, or press ⌘T for a terminal.",
                Some(
                    ui::button(
                        "sessions-empty-open",
                        "Open Folder…",
                        ButtonKind::Secondary,
                        true,
                        &theme,
                        &tokens,
                    )
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx))
                    .into_any_element(),
                ),
                &theme,
                &tokens,
            )
            .h(px(260.0))
            .into_any_element(),
        }
    }

    /// The "No workspace" label above the projects that belong to none: a project dropped on
    /// it leaves its workspace.
    fn render_no_workspace(&self, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let accent = theme.accent;
        div()
            .id("section-no-workspace")
            .relative()
            .flex()
            .items_center()
            .h(px(tokens.sidebar_row))
            .px(px(tokens.spacing * 2.0))
            .mt(px(tokens.spacing))
            .text_size(px(tokens.section_font_size))
            .medium()
            .text_color(theme.muted)
            .drag_over::<DraggedProject>(move |s, _, _, _| s.bg(accent.opacity(0.14)))
            .on_drop(cx.listener(|this, dragged: &DraggedProject, window, cx| {
                this.project_dropped(dragged, None, window, cx);
            }))
            .child("No workspace")
            .child(probe(&self.env.regions, Region::NoWorkspace))
            .into_any_element()
    }

    /// `▾ ★ NAME  ······  ● 2  3` for a workspace; `▾ ◧ ★ NAME …` for a project (repository or
    /// folder), `▾ ⌂ Home …` for Home. Click shows it, the chevron folds its sessions,
    /// right-click opens its menu, ⌘-click / ⇧-click select projects, and a row can be dragged:
    /// a project onto a workspace (or "No workspace"), a workspace to reorder (manual order).
    #[allow(clippy::too_many_arguments)] // one call site; a struct would only rename them
    fn render_container(
        &self,
        row: &ContainerRow,
        collapsed: bool,
        current: bool,
        selected: bool,
        manual: bool,
        renaming: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let (hover, accent) = (card_hover(&theme), theme.accent);
        let info = &row.info;
        let id = info.id;
        let kind = info.kind;
        let id_text = id.to_string();
        let open_id = id_text.clone();
        let menu_id = id_text.clone();
        let drop_id = id_text.clone();
        let is_project = kind == WorkspaceKind::Project;
        let icon = match kind {
            WorkspaceKind::Home => Some("⌂"),
            WorkspaceKind::Project => Some(
                if info
                    .project
                    .as_ref()
                    .is_some_and(|p| p.common_dir.is_some())
                {
                    "◧"
                } else {
                    "▭"
                },
            ),
            WorkspaceKind::Workspace => None,
        };
        let name: AnyElement = match renaming {
            Some(text) => div()
                .id(SharedString::from(format!("rename-{id_text}")))
                .key_context("SidebarRename")
                .track_focus(&self.rename_focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    this.on_rename_key(event, window, cx);
                }))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .h(px(tokens.sidebar_row - 6.0))
                .px(px(tokens.spacing))
                .rounded(px(tokens.radius))
                .bg(theme.bg)
                .border_1()
                .border_color(accent)
                .text_color(theme.fg)
                .children(vim_ui::string_field(
                    SharedString::from(format!("rename-text-{id_text}")),
                    &self.rename_edit,
                    &text,
                    VimConfig::of(&self.env.settings).enabled,
                    "",
                    true,
                    false,
                    self.rename_field(cx),
                    &theme,
                    &tokens,
                ))
                .into_any_element(),
            None => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(SharedString::from(info.name.clone()))
                .into_any_element(),
        };
        let mut header = div()
            .id(SharedString::from(format!("workspace-{id_text}")))
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .h(px(tokens.sidebar_row))
            .mx(px(tokens.spacing * 1.5))
            .pl(px(tokens.spacing * 1.5 + row.depth as f32 * INDENT))
            .pr(px(tokens.spacing * 1.5))
            .rounded(px(tokens.radius + 4.0))
            .text_size(px(CARD_TITLE))
            .medium()
            .text_color(if current || selected {
                theme.fg
            } else {
                theme.muted
            })
            .cursor_pointer()
            .when(current || selected, move |s| s.bg(theme.sidebar_accent))
            .when(selected, move |s| s.border_1().border_color(accent))
            .when(!(current || selected), move |s| {
                s.hover(move |s| s.bg(hover))
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                let modifiers = event.modifiers();
                if is_project && (modifiers.platform || modifiers.shift) {
                    this.select_project(id, modifiers.shift && !modifiers.platform, cx);
                    return;
                }
                if is_project {
                    this.selection.clear();
                    this.anchor = Some(id);
                    cx.notify();
                }
                window.dispatch_action(Box::new(OpenWorkspace(open_id.clone())), cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if is_project {
                        let selection = if this.selection.contains(&id) {
                            this.selection.iter().map(ToString::to_string).collect()
                        } else {
                            vec![menu_id.clone()]
                        };
                        window.dispatch_action(
                            Box::new(ShowProjectMenu(ProjectMenuAt {
                                project: menu_id.clone(),
                                selection,
                                position: event.position,
                            })),
                            cx,
                        );
                    } else {
                        window.dispatch_action(
                            Box::new(ShowWorkspaceMenu(WorkspaceMenuAt {
                                workspace: menu_id.clone(),
                                position: event.position,
                            })),
                            cx,
                        );
                    }
                }),
            );
        // What can be dropped on the row.
        header = match kind {
            WorkspaceKind::Workspace => header
                .drag_over::<DraggedWorkspace>(move |s, _, _, _| {
                    s.border_t_2().border_color(accent)
                })
                .drag_over::<DraggedProject>(move |s, _, _, _| s.bg(accent.opacity(0.14)))
                .on_drop(
                    cx.listener(move |_, dragged: &DraggedWorkspace, window, cx| {
                        window.dispatch_action(
                            Box::new(MoveWorkspaceOnto(WorkspaceDrop {
                                dragged: dragged.workspace.to_string(),
                                target: drop_id.clone(),
                            })),
                            cx,
                        );
                    }),
                )
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedProject, window, cx| {
                        this.project_dropped(dragged, Some(id), window, cx);
                    }),
                ),
            WorkspaceKind::Project => header
                .drag_over::<DraggedProject>(move |s, _, _, _| s.border_t_2().border_color(accent))
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedProject, window, cx| {
                        this.project_dropped(dragged, Some(id), window, cx);
                    }),
                ),
            WorkspaceKind::Home => header,
        };
        let group = SharedString::from(format!("ws-row-{id_text}"));
        header = header.group(group.clone());
        match kind {
            WorkspaceKind::Workspace if manual => {
                let dragged = DraggedWorkspace {
                    workspace: id,
                    label: info.name.clone().into(),
                };
                header = header.on_drag(dragged, |dragged: &DraggedWorkspace, _, _, cx| {
                    let style = UiStyle::get(cx);
                    let label = dragged.label.clone();
                    cx.new(|_| DragPreview { label, style })
                });
            }
            WorkspaceKind::Project => {
                let dragged = DraggedProject {
                    project: id,
                    label: info.name.clone().into(),
                };
                header = header.on_drag(dragged, |dragged: &DraggedProject, _, _, cx| {
                    let style = UiStyle::get(cx);
                    let label = dragged.label.clone();
                    cx.new(|_| DragPreview { label, style })
                });
            }
            _ => {}
        }
        header
            .child(
                div()
                    .id(SharedString::from(format!("workspace-fold-{id_text}")))
                    .flex_none()
                    .w(px(12.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&id) {
                            this.collapsed.insert(id);
                        }
                        cx.notify();
                    }))
                    .child(if collapsed { "▸" } else { "▾" }),
            )
            .children(icon.map(|icon| div().flex_none().child(icon)))
            .when(info.pinned, |s| {
                s.child(div().flex_none().text_color(theme.accent).child("★"))
            })
            .child(name)
            .when(row.needs > 0, |s| {
                s.child(
                    div()
                        .flex_none()
                        .text_color(theme.warning)
                        .child(format!("● {}", row.needs)),
                )
            })
            .children(count_text(info).map(|count| ui::counter(count, &theme, &tokens)))
            .when(
                matches!(kind, WorkspaceKind::Project | WorkspaceKind::Home),
                |s| {
                    let start_id = id_text.clone();
                    s.child(
                        div()
                            .id(SharedString::from(format!("workspace-start-{id_text}")))
                            .relative()
                            .flex_none()
                            .px(px(tokens.spacing))
                            .rounded(px(tokens.radius))
                            .cursor_pointer()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.accent)
                            .opacity(if current { 1.0 } else { 0.0 })
                            .group_hover(group.clone(), |s| s.opacity(1.0))
                            .hover(move |s| s.bg(theme.accent_soft))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, window, cx| {
                                window.dispatch_action(
                                    Box::new(OpenLauncher(LauncherOpen {
                                        project: Some(start_id.clone()),
                                        queue: false,
                                    })),
                                    cx,
                                );
                                cx.stop_propagation();
                            })
                            .child("▶")
                            .child(probe(&self.env.regions, Region::ProjectStart(id))),
                    )
                },
            )
            .child(probe(&self.env.regions, Region::WorkspaceRow(id)))
            .into_any_element()
    }

    /// Two lines: the state dot, kind icon, title, agent badge and age; under them the live
    /// folder (last two path parts, `muted`, the full path in its tooltip) and the placement
    /// badge. A session that needs you adds a third line with what it waits for.
    fn render_row(
        &self,
        row: &SessionRow,
        settings: &SessionsPanelSettings,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let info = &row.info;
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let presentation = self.env.sessions.read(cx).presentation(info);
        let unread = self.env.sessions.read(cx).unread(info.id).is_some();
        let hover = card_hover(&theme);
        let id = info.id.to_string();
        let open_id = id.clone();
        let menu_id = id.clone();
        let badge = self.env.sessions.read(cx).agent_badge(info.id);
        let mut badge = agent_badge(
            &theme,
            &badge,
            BadgeSite::Row,
            info.id,
            &self.env.regions,
            cx,
        );
        let compact_chips = self.env.sessions.read(cx).large_sidebar();
        let mut recovery = hutan_client::recovery::chip(info).map(|chip| {
            recovery_chip(
                &theme,
                &chip,
                BadgeSite::Row,
                info.id,
                compact_chips,
                &self.env.regions,
                cx,
            )
        });
        let has = |field: &str| settings.row.iter().any(|f| f == field);

        let mut line = div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 1.5))
            .h(px(tokens.sidebar_row))
            .w_full()
            .min_w_0();
        for field in &settings.row {
            line = match field.as_str() {
                "state" => line.child(
                    // Orca's 20 px status lane: the state dot, and for an unread session an
                    // amber dot with a ring in the color of the sidebar on top of it.
                    div()
                        .relative()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(STATUS_LANE))
                        .child(state_dot(&theme, &presentation))
                        .when(unread, |s| {
                            s.child(
                                div()
                                    .absolute()
                                    .top(px(-2.0))
                                    .right(px(1.0))
                                    .size(px(6.0))
                                    .rounded_full()
                                    .bg(theme.warning)
                                    .border_2()
                                    .border_color(theme.sidebar),
                            )
                        }),
                ),
                "kind" => line.child(
                    div()
                        .flex_none()
                        .text_color(theme.muted)
                        .child(kind_icon(info.kind)),
                ),
                "title" => line
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(CARD_TITLE))
                            .when(unread, |s| s.font_weight(gpui::FontWeight::MEDIUM))
                            .child(SharedString::from(info.title.clone())),
                    )
                    .children(badge.take())
                    .children(recovery.take()),
                "age" => line.child(
                    div()
                        .id(SharedString::from(format!("row-age-{}", info.id)))
                        .flex_none()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .tooltip(time_tooltip(info.created_at_ms))
                        .child(SharedString::from(age_label(info.created_at_ms))),
                ),
                _ => line,
            };
        }
        // The hover × closes the session (needs a daemon with `session.close`).
        let row_group = SharedString::from(format!("session-group-{id}"));
        if self.env.sessions.read(cx).supports_close() {
            let close_id = id.clone();
            let session = info.id;
            line = line.child(
                div()
                    .id(SharedString::from(format!("session-close-{id}")))
                    .relative()
                    .flex_none()
                    .px(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .text_color(theme.muted)
                    .opacity(0.0)
                    .group_hover(row_group.clone(), |s| s.opacity(1.0))
                    .hover(move |s| s.bg(theme.accent_soft).text_color(theme.fg))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(CloseSessionById(close_id.clone())), cx);
                        cx.stop_propagation();
                    })
                    .child("×")
                    .child(probe(&self.env.regions, Region::SessionClose(session))),
            );
        }

        // Where the session is now, and which checkout it works in.
        let folder = has("folder")
            .then(|| session_folder(info).map(|path| (folder_label(path), path.to_path_buf())))
            .flatten();
        let placement = has("placement")
            .then(|| crate::sessions::placement_badge(info))
            .flatten();
        let place_line = (folder.is_some() || placement.is_some()).then(|| {
            let tip: Option<SharedString> = folder
                .as_ref()
                .map(|(_, path): &(String, PathBuf)| path.display().to_string().into());
            div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 1.5))
                .pl(px(STATUS_LANE + tokens.spacing * 1.5))
                .pb(px(tokens.spacing))
                .text_size(px(tokens.secondary_font_size))
                .children(folder.map(|(label, _)| {
                    let element = div()
                        .id(SharedString::from(format!("session-folder-{id}")))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.muted)
                        .child(SharedString::from(label));
                    match tip {
                        Some(tip) => element
                            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                            .into_any_element(),
                        None => element.into_any_element(),
                    }
                }))
                .children(placement.map(|badge| {
                    placement_badge(
                        &theme,
                        &badge,
                        BadgeSite::Row,
                        info.id,
                        &self.env.regions,
                        cx,
                    )
                }))
        });

        let mut element = div()
            .id(SharedString::from(format!("session-row-{id}")))
            .group(row_group)
            .flex()
            .flex_col()
            .mx(px(tokens.spacing * 1.5))
            .my(px(1.0))
            .pl(px(tokens.spacing * 1.5 + row.depth as f32 * INDENT))
            .pr(px(tokens.spacing * 1.5))
            .rounded(px(tokens.radius + 4.0))
            .border_1()
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .when(selected, move |s| {
                s.bg(theme.sidebar_accent).border_color(theme.border)
            })
            .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
            .on_click(cx.listener(move |_, _, window, cx| {
                window.dispatch_action(Box::new(OpenSession(open_id.clone())), cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                    window.dispatch_action(
                        Box::new(ShowSessionMenu(SessionMenuAt {
                            session: menu_id.clone(),
                            position: event.position,
                        })),
                        cx,
                    );
                }),
            )
            .child(line)
            .children(place_line);
        if row.in_needs_you {
            let subtitle = if presentation.detail.is_empty() {
                presentation.label.clone()
            } else {
                format!("{} · {}", presentation.label, presentation.detail)
            };
            element = element.child(
                div()
                    .min_w_0()
                    .truncate()
                    .pl(px(STATUS_LANE + tokens.spacing * 1.5))
                    .pb(px(tokens.spacing))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(tone_color(&theme, presentation.tone))
                    .child(SharedString::from(subtitle)),
            );
        }
        element.into_any_element()
    }
}
