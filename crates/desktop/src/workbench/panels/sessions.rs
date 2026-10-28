//! The `sessions` panel ("Projects"): user workspaces → their projects → sessions, the projects
//! in no workspace, then Home (or a flat list of projects, `sidebar.organize`); sorted, pinned,
//! filtered and searched; the groups of attention (Requests, Failed, Ready for review) on top.
//!
//! A shell is one line: lane, name, age. A coding agent is a *card* (SIDEBAR-CARDS): the lane
//! carries its harness glyph with the state's badge on it, the session's name comes first, the
//! age is the only datum on the right (the pointer swaps it for the actions, × close), and a
//! second *news line* exists only when something is going on — a question waits, it failed,
//! the tool it runs, the first line of a reply nobody has read (`sessions::card`). The first
//! agent card no hook speaks for says once what installing the hooks adds. Where a session is
//! (a worktree, a folder it left its checkout for) is muted text after the name; the warning
//! that another live agent works in the same checkout is one chip on the project or worktree row
//! of that checkout, never on the session rows. Workspaces and projects are managed here: the
//! "+" menu, drag a project onto a workspace (or out, to "No workspace"), ⌘-click / ⇧-click to
//! select projects, inline rename, context menus (`workbench/projects.rs` holds the verbs).

use crate::theme::DensityExt as _;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
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
use hutan_protocol::worktree::{Placement, PlacementKind};

use super::common::{
    age_label, agent_badge, panel_header_controls, placement, placement_badge, recovery_chip,
    session_icon, shared_checkout_chip, time_tooltip,
};
use crate::icons::{Icon, icon};
use crate::sessions::PlacementBadge;
use crate::sessions::card::{self, News, NewsKind};
use crate::sessions::tree::{
    self, Node, ProjectNode, Subtitle, SubtitleInputs, folder_label, session_folder,
};
use crate::sessions::{Attention, StoreChanged};
use crate::settings::{
    GroupBy, SessionsPanelSettings, SidebarSettings, WorkspaceSort, WorkspacesSettings,
};
use crate::workbench::actions::{CheckoutOpen, OpenCheckout};
use crate::workbench::actions::{
    CloseSessionById, DropProjectOnto, MoveWorkspaceOnto, OpenFolder, OpenSession, OpenWorkspace,
    ProjectDrop, ProjectMenuAt, RenameSessionTo, RenameTo, RenameWorkspaceTo, ReviewChangesAt,
    SessionMenuAt, SessionRenameTo, ShowPlusMenu, ShowProjectMenu, ShowSessionMenu, ShowSortMenu,
    ShowWorkspaceMenu, WorkspaceDrop, WorkspaceMenuAt,
};
use crate::workbench::actions::{LauncherOpen, OpenLauncher, OpenSettingsFor};
use crate::workbench::chat::vim::Outcome;
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::{TextTooltip, UiStyle};
use crate::workbench::text_field::FieldRef;
use crate::workbench::ui::BadgeState;
use crate::workbench::ui::{self, FocusRingExt as _, StyledExt as _};
use crate::workbench::vim_ui::{self, FieldEdit, VimConfig};
use crate::workbench::{BadgeSite, DragPreview, Region};

/// Row refresh for the relative ages.
/// What a row of the list is tall before it is measured, in pixels: the scrollbar's thumb and
/// its drags use it for the rows nobody has scrolled to yet, and the real heights replace it as
/// they are measured.
const ROW_ESTIMATE: f32 = 34.0;
const AGE_REFRESH: Duration = Duration::from_secs(30);

/// What the actions of a card (× close) take on the right: the age shrinks into the same place.
const CARD_ACTIONS: f32 = 20.0;

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
    /// "Requests · 2", a state label.
    Section,
    Workspace,
    Project,
    /// A linked worktree of the project above it: `⎇ branch`, a checkout of its own.
    Worktree,
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
    /// A project's root folder; for a session the live folder (last two path parts), only when
    /// it has left the checkout the session belongs to.
    pub folder: Option<String>,
    /// A session's placement badge text (`wt <branch>`), only off the main checkout.
    pub badge: Option<String>,
    /// The project or worktree row wears the shared-checkout chip: another live agent works in
    /// that checkout. Said once, there; session rows are never `shared`.
    pub shared: bool,
    pub pinned: bool,
    /// `3` or `1/3` (live/total) on a workspace, project or Home row.
    pub count: Option<String>,
    /// Sessions of the row (and under it) with a request waiting.
    pub needs: usize,
    /// The row of the container or project the window shows.
    pub active: bool,
    pub selected: bool,
    /// The row is drawn with the list's one selected fill: the focused session, or the active
    /// project while no session under it is the focused one.
    pub filled: bool,
    pub collapsed: bool,
    /// A session's second line; `None` is a one-line row.
    pub subtitle: Option<Subtitle>,
    /// A session's state word when its lane shows a glyph (`None` while idle: the lane is
    /// empty, the room stays).
    pub state: Option<String>,
    /// The session's notifications are muted (it wears the muted marker).
    pub muted: bool,
    /// Live sessions of a container or worktree row (and under it) that run a turn.
    pub working: usize,
    /// A repository row's main-checkout branch, a worktree row's branch; `None` for a folder
    /// outside git (never an empty label) and for a detached HEAD.
    pub branch: Option<String>,
    /// A session's lane badge ([`BadgeState::Idle`] draws none).
    pub lane: BadgeState,
    /// A session's second line; `None` is a one-line row (shells always are).
    pub news: Option<News>,
    /// `+12 −3` on a project or worktree row, only with `panels.sessions.show_diffstat`.
    pub diffstat: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SectionKey {
    Attention(Attention),
    All,
    State(String),
}

/// A workspace, project or Home row with what the layout decided about it.
#[derive(Debug, Clone, PartialEq)]
struct ContainerRow {
    info: WorkspaceInfo,
    depth: usize,
    needs: usize,
    /// Live sessions of the row (and under it) running a turn.
    working: usize,
    /// The branch of a repository's main checkout (`None` outside git).
    branch: Option<String>,
    /// Files its checkouts have uncommitted, as last counted; only for a project the window is
    /// not working in (the Changes panel says it for the active one).
    changed: usize,
    /// The lines those files added and removed, with `panels.sessions.show_diffstat`.
    diffstat: Option<(u64, u64)>,
    /// Another live agent works in the project's main checkout.
    shared: Option<SharedChip>,
    /// Its sessions are listed under it: the container the window shows, or whatever the user
    /// opened with the chevron; every container with `sidebar.show_all_sessions`.
    expanded: bool,
}

/// Another live agent works in the checkout at `root`: what the project or worktree row says
/// once, instead of every session row repeating it.
#[derive(Debug, Clone, PartialEq)]
struct SharedChip {
    root: PathBuf,
    badge: PlacementBadge,
}

/// A linked worktree listed under its project.
#[derive(Debug, Clone, PartialEq)]
struct WorktreeRow {
    project: WorkspaceId,
    path: PathBuf,
    branch: Option<String>,
    depth: usize,
    needs: usize,
    working: usize,
    /// The worktree the user picked: Files and Search follow it.
    active: bool,
    /// Files it has uncommitted, and the lines they added and removed, with
    /// `panels.sessions.show_diffstat`.
    diffstat: Option<(u64, u64)>,
    /// Another live agent works in this worktree.
    shared: Option<SharedChip>,
}

#[derive(Debug, Clone, PartialEq)]
struct SessionRow {
    info: SessionInfo,
    depth: usize,
    /// What follows the title: the project of a row away from its project row, the placement.
    subtitle: Option<Subtitle>,
    /// The card's second line, `None` for the one-line row.
    news: Option<News>,
}

#[derive(Debug, Clone, PartialEq)]
enum Entry {
    Section {
        key: SectionKey,
        title: String,
    },
    Container(ContainerRow),
    Worktree(WorktreeRow),
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
    toggled: HashSet<WorkspaceId>,
    /// The container and project the window shows, and the worktree picked in it.
    active: (Option<WorkspaceId>, Option<WorkspaceId>, Option<PathBuf>),
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
            && self.toggled == other.toggled
            && self.active == other.active
            && Arc::ptr_eq(&self.settings.0, &other.settings.0)
            && Arc::ptr_eq(&self.settings.1, &other.settings.1)
            && Arc::ptr_eq(&self.settings.2, &other.settings.2)
    }
}

/// What an inline rename edits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RenameTarget {
    Workspace(WorkspaceId),
    Session(SessionId),
}

/// An inline rename in progress; losing focus confirms it.
struct Renaming {
    target: RenameTarget,
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
    /// Containers whose fold the user flipped from its default (a container is expanded by
    /// default while it is the active one, or while every session is listed).
    toggled: HashSet<WorkspaceId>,
    /// Projects selected with ⌘-click / ⇧-click.
    selection: Vec<WorkspaceId>,
    anchor: Option<WorkspaceId>,
    renaming: Option<Renaming>,
    /// Counts the store's notifications: part of what the cached entries are keyed by.
    store_rev: u64,
    /// Times the panel was rendered (tests: a store notification that changes nothing the
    /// panel shows must not render it).
    renders: usize,
    /// Drawn since the age timer last looked.
    drawn: bool,
    /// The entries of the last layout and what they were made from. A frame never rebuilds
    /// them (that clones every session of the daemon); a change of the store, the filter, the
    /// folds or the settings does.
    layout_cache: RefCell<Option<(LayoutKey, Rc<Vec<Entry>>)>>,
    layout_builds: Cell<usize>,
    /// The virtualized list: only the rows in view (and a little around) are built and laid out.
    list: ListState,
    bar: hutan_ui::Scrollbar,
    /// The entries the list's heights are for, and the filter they were laid out under.
    listed: Rc<Vec<Entry>>,
    listed_query: String,
    rows: RowContext,
    rows_rendered: Cell<usize>,
    _ages: Task<()>,
}

impl SidebarRow {
    /// A row of `kind` with nothing else to say.
    fn blank(kind: RowKind, depth: usize, title: String) -> Self {
        Self {
            kind,
            depth,
            title,
            folder: None,
            badge: None,
            shared: false,
            pinned: false,
            count: None,
            needs: 0,
            active: false,
            selected: false,
            filled: false,
            collapsed: false,
            subtitle: None,
            state: None,
            muted: false,
            working: 0,
            branch: None,
            lane: BadgeState::Idle,
            news: None,
            diffstat: None,
        }
    }
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
        // Only what the list is made of rebuilds it; a move of the focus repaints the
        // highlight; the rest (answers given here, flags nothing lists) is not its business.
        cx.subscribe(&env.sessions, |this, _, change: &StoreChanged, cx| {
            if change.intersects(StoreChanged::LISTED) {
                this.store_rev += 1;
                cx.notify();
            } else if change.intersects(StoreChanged::FOCUS) {
                cx.notify();
            }
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
        let list = ListState::new(0, ListAlignment::Top, px(300.0));
        Self {
            env,
            query: String::new(),
            query_edit: FieldEdit::new(),
            rename_edit: FieldEdit::new(),
            search_focus: cx.focus_handle(),
            rename_focus: cx.focus_handle(),
            toggled: HashSet::new(),
            selection: Vec::new(),
            anchor: None,
            renaming: None,
            store_rev: 0,
            renders: 0,
            drawn: false,
            layout_cache: RefCell::new(None),
            layout_builds: Cell::new(0),
            bar: hutan_ui::Scrollbar::for_list(&list, ROW_ESTIMATE),
            list,
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

    /// How many times the panel was rendered (tests).
    pub fn renders(&self) -> usize {
        self.renders
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

    /// The chevron of a container row: lists its sessions when they are folded and folds them
    /// when they are listed, whatever the default (the container in use, or every one with
    /// `sidebar.show_all_sessions`) says.
    pub fn toggle_container(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        if !self.toggled.remove(&id) {
            self.toggled.insert(id);
        }
        cx.notify();
    }

    /// Every row currently listed, top to bottom.
    pub fn visible_rows(&self, cx: &gpui::App) -> Vec<SidebarRow> {
        let store = self.env.sessions.read(cx);
        let (active, project) = (store.active_workspace_id(), store.active_project_id());
        let entries = self.entries(cx);
        let focused = store.focused();
        let focus_listed = focused.is_some_and(|f| {
            entries
                .iter()
                .any(|e| matches!(e, Entry::Session(s) if s.info.id == f))
        });
        entries
            .iter()
            .enumerate()
            .filter_map(|(ix, entry)| {
                Some(match entry {
                    Entry::EmptyHint => return None,
                    Entry::Section { title, .. } => {
                        SidebarRow::blank(RowKind::Section, 0, title.clone())
                    }
                    Entry::NoWorkspace => {
                        SidebarRow::blank(RowKind::NoWorkspace, 0, "No workspace".to_owned())
                    }
                    Entry::Container(row) => {
                        let current = self.is_active(&row.info, active, project);
                        let selected = self.selection.contains(&row.info.id);
                        let child_selected =
                            current && selected_session_below(&entries, ix, row.depth, focused);
                        SidebarRow {
                            folder: (row.info.kind == WorkspaceKind::Project)
                                .then(|| crate::workbench::workspaces::project_root_of(&row.info))
                                .flatten()
                                .map(|root| folder_label(&root)),
                            pinned: row.info.pinned,
                            count: count_text(&row.info),
                            needs: row.needs,
                            working: row.working,
                            branch: row.branch.clone(),
                            shared: row.shared.is_some(),
                            diffstat: row.diffstat.map(diffstat_text),
                            active: current,
                            selected,
                            filled: selected || (current && !child_selected),
                            collapsed: !row.expanded,
                            ..SidebarRow::blank(
                                match row.info.kind {
                                    WorkspaceKind::Workspace => RowKind::Workspace,
                                    WorkspaceKind::Project => RowKind::Project,
                                    WorkspaceKind::Home => RowKind::Home,
                                },
                                row.depth,
                                row.info.name.clone(),
                            )
                        }
                    }
                    Entry::Worktree(row) => SidebarRow {
                        folder: Some(folder_label(&row.path)),
                        needs: row.needs,
                        working: row.working,
                        branch: row.branch.clone(),
                        shared: row.shared.is_some(),
                        diffstat: row.diffstat.map(diffstat_text),
                        active: row.active,
                        filled: row.active && !focus_listed,
                        ..SidebarRow::blank(
                            RowKind::Worktree,
                            row.depth,
                            row.branch
                                .clone()
                                .unwrap_or_else(|| folder_label(&row.path)),
                        )
                    },
                    Entry::Session(row) => {
                        let badge = row.subtitle.as_ref().and_then(|s| s.badge.as_ref());
                        let presentation = store.presentation(&row.info);
                        SidebarRow {
                            folder: row.subtitle.as_ref().and_then(|s| s.folder.clone()),
                            badge: badge.map(|b| b.text.clone()),
                            selected: focused == Some(row.info.id),
                            filled: focused == Some(row.info.id),
                            subtitle: row.subtitle.clone(),
                            state: lane_word(&presentation),
                            lane: card::badge_state(
                                &row.info,
                                &presentation,
                                store.unread(row.info.id),
                            ),
                            news: row.news.clone(),
                            muted: row.info.muted,
                            ..SidebarRow::blank(RowKind::Session, row.depth, row.info.title.clone())
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
        self.start_rename(RenameTarget::Workspace(workspace), name, window, cx);
        true
    }

    /// Starts editing the title of `session`'s row in place (the row must be listed: the
    /// workbench asks the region first); the whole title starts selected.
    pub fn begin_session_rename(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(title) = self
            .env
            .sessions
            .read(cx)
            .session(session)
            .map(|s| s.title.clone())
        else {
            return false;
        };
        self.start_rename(RenameTarget::Session(session), title, window, cx);
        true
    }

    fn start_rename(
        &mut self,
        target: RenameTarget,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let blur = cx.on_focus_out(&self.rename_focus, window, |this, _, window, cx| {
            this.finish_rename(true, window, cx);
        });
        self.rename_edit.reset_selected(&text);
        self.renaming = Some(Renaming {
            target,
            text,
            _blur: blur,
        });
        cx.defer_in(window, |this, window, cx| {
            if this.renaming.is_some() {
                window.focus(&this.rename_focus, cx);
            }
        });
        cx.notify();
    }

    /// The row being renamed and the text typed so far.
    pub fn renaming(&self) -> Option<(WorkspaceId, &str)> {
        self.renaming.as_ref().and_then(|r| match r.target {
            RenameTarget::Workspace(id) => Some((id, r.text.as_str())),
            RenameTarget::Session(_) => None,
        })
    }

    /// The session whose title is being edited in place and the text typed so far.
    pub fn renaming_session(&self) -> Option<(SessionId, &str)> {
        self.renaming.as_ref().and_then(|r| match r.target {
            RenameTarget::Session(id) => Some((id, r.text.as_str())),
            RenameTarget::Workspace(_) => None,
        })
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
        let changed = {
            let store = self.env.sessions.read(cx);
            match renaming.target {
                RenameTarget::Workspace(id) => store.workspace(id).is_none_or(|w| w.name != name),
                RenameTarget::Session(id) => store.session(id).is_none_or(|s| s.title != name),
            }
        };
        // The workbench takes the focus back either way (an empty name only does that).
        let name = if confirm && changed {
            name
        } else {
            String::new()
        };
        let action: Box<dyn gpui::Action> = match renaming.target {
            RenameTarget::Workspace(id) => Box::new(RenameWorkspaceTo(RenameTo {
                workspace: id.to_string(),
                name,
            })),
            RenameTarget::Session(id) => Box::new(RenameSessionTo(SessionRenameTo {
                session: id.to_string(),
                title: name,
            })),
        };
        window.dispatch_action(action, cx);
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
            toggled: self.toggled.clone(),
            active: {
                let store = self.env.sessions.read(cx);
                (
                    store.active_workspace_id(),
                    store.active_project_id(),
                    store.active_checkout(),
                )
            },
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
        let needs: HashMap<Attention, Vec<SessionId>> = Attention::ALL
            .into_iter()
            .map(|group| (group, store.in_attention(group)))
            .collect();
        let attention = store.requests_by_workspace();
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
        // The second line of a row (`tree::subtitle`): `away` is a row that is not under its
        // project row (the groups of attention, the flat lists).
        let has = |field: &str| settings.row.iter().any(|f| f == field);
        let (show_folder, show_placement) = (has("folder"), has("placement"));
        let subtitle_of = |info: &SessionInfo, away: bool| {
            let container = store.workspace(info.workspace_id);
            tree::subtitle(
                info,
                SubtitleInputs {
                    away_from_parent: away,
                    container_name: container.map(|w| w.name.as_str()),
                    project_root: container
                        .filter(|w| w.kind == WorkspaceKind::Project)
                        .and_then(crate::workbench::workspaces::project_root_of)
                        .as_deref(),
                    show_folder,
                    show_placement,
                },
            )
        };
        let mut entries = Vec::new();
        // The groups of attention on top, each listing the sessions that are in it (a search
        // narrows them like every other row).
        let (needing, rest): (Vec<(Attention, Vec<&SessionInfo>)>, Vec<&SessionInfo>) = if settings
            .needs_you
        {
            let groups = Attention::ALL
                .into_iter()
                .map(|group| {
                    let rows: Vec<&SessionInfo> = store
                        .sessions()
                        .iter()
                        .filter(|s| {
                            listed(s) && needs.get(&group).is_some_and(|ids| ids.contains(&s.id))
                        })
                        .collect();
                    (group, rows)
                })
                .filter(|(_, rows)| !rows.is_empty())
                .collect();
            let rest = store
                .sessions()
                .iter()
                .filter(|s| listed(s) && !needs.values().any(|ids| ids.contains(&s.id)))
                .collect();
            (groups, rest)
        } else {
            (
                Vec::new(),
                store.sessions().iter().filter(|s| listed(s)).collect(),
            )
        };
        // The sessions the tree decides about itself (it knows which containers the search
        // matches); the ones the groups of attention took are not among them.
        let in_tree: Vec<&SessionInfo> = store
            .sessions()
            .iter()
            .filter(|s| !(settings.needs_you && needs.values().any(|ids| ids.contains(&s.id))))
            .collect();
        for (group, rows) in needing {
            entries.push(Entry::Section {
                key: SectionKey::Attention(group),
                title: format!("{} · {}", group.label(), rows.len()),
            });
            entries.extend(rows.into_iter().map(|info| {
                Entry::Session(Box::new(SessionRow {
                    subtitle: subtitle_of(info, true),
                    info: info.clone(),
                    depth: 0,
                    news: None,
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
                            subtitle: subtitle_of(info, true),
                            info: info.clone(),
                            depth: 0,
                            news: None,
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
                            subtitle: subtitle_of(info, true),
                            info: (*info).clone(),
                            depth: 0,
                            news: None,
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
                self.flatten(&built.nodes, &subtitle_of, &mut entries, cx);
            }
        }
        // The cards' second lines are decided here, with the entries, so that a row whose line
        // changes is a different entry and the list measures it again. The first agent no hook
        // speaks for says what installing the hooks adds, when it has no news of its own.
        let agents = store.agent_presentation();
        let mut hinted = false;
        for entry in &mut entries {
            let Entry::Session(row) = entry else {
                continue;
            };
            row.news = card::news(
                &row.info,
                &store.presentation(&row.info),
                agents.get(row.info.id),
                store.unread(row.info.id),
                store.reply_line(row.info.id),
            );
            if !hinted && card::without_hooks(&row.info) {
                hinted = true;
                row.news.get_or_insert_with(|| News {
                    kind: NewsKind::HooksHint,
                    text: card::HOOKS_HINT.to_owned(),
                    model: None,
                });
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
            self.list
                .reset_with_uniform_height(entries.len(), px(ROW_ESTIMATE));
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
            self.list.clone().with_uniform_item_height(px(ROW_ESTIMATE));
        }
    }

    /// The rows of the tree. A container lists its sessions while it is expanded: by default
    /// the one the window shows (the workspace, the project in it, a project of no workspace,
    /// Home) and, with `sidebar.show_all_sessions` or a search, every one; the chevron flips
    /// that for one container. A repository's expanded project lists its linked worktrees under
    /// its own sessions, each with the sessions that work in it.
    fn flatten(
        &self,
        nodes: &[Node],
        subtitle_of: &dyn Fn(&SessionInfo, bool) -> Option<Subtitle>,
        entries: &mut Vec<Entry>,
        cx: &gpui::App,
    ) {
        let store = self.env.sessions.read(cx);
        let (active, active_project) = (store.active_workspace_id(), store.active_project_id());
        let active_checkout = store.active_checkout();
        let diffstat_on = self
            .env
            .settings
            .get::<SessionsPanelSettings>()
            .show_diffstat;
        let everything = self.env.settings.get::<SidebarSettings>().show_all_sessions
            || !self.query.trim().is_empty();
        let working = store.working_by_workspace();
        let expanded = |id: WorkspaceId, by_default: bool| {
            (everything || by_default) != self.toggled.contains(&id)
        };
        // A workspace's count includes its projects'.
        let rolled = |info: &WorkspaceInfo| {
            let own = working.get(&info.id).copied().unwrap_or(0);
            let members: usize = store
                .workspaces()
                .iter()
                .filter(|w| w.parent_id == Some(info.id))
                .map(|w| working.get(&w.id).copied().unwrap_or(0))
                .sum();
            own + members
        };
        let session = |entries: &mut Vec<Entry>, list: &[&SessionInfo], depth: usize| {
            entries.extend(list.iter().map(|info| {
                Entry::Session(Box::new(SessionRow {
                    subtitle: subtitle_of(info, false),
                    info: (*info).clone(),
                    depth,
                    news: None,
                }))
            }));
        };
        let project = |entries: &mut Vec<Entry>, node: &ProjectNode, depth: usize| {
            let info = &node.info;
            let is_expanded = expanded(
                info.id,
                active_project == Some(info.id) || active == Some(info.id),
            );
            let is_project = info.kind == WorkspaceKind::Project;
            entries.push(Entry::Container(ContainerRow {
                info: info.clone(),
                depth,
                needs: node.needs,
                working: rolled(info),
                branch: is_project.then(|| store.main_branch(info)).flatten(),
                changed: if is_project {
                    store.changed_files_of(info)
                } else {
                    0
                },
                diffstat: (diffstat_on && is_project)
                    .then(|| store.diffstat_of(info))
                    .flatten(),
                shared: is_project
                    .then(|| {
                        shared_chip(
                            store
                                .sessions()
                                .iter()
                                .filter(|s| s.workspace_id == info.id),
                            |p| p.kind == PlacementKind::Main,
                        )
                    })
                    .flatten(),
                expanded: is_expanded,
            }));
            if !is_expanded {
                return;
            }
            let checkouts: Vec<crate::sessions::Checkout> = if is_project {
                store
                    .checkouts_of(info)
                    .into_iter()
                    .filter(|c| !c.main)
                    .collect()
            } else {
                Vec::new()
            };
            let in_checkout = |s: &SessionInfo, path: &PathBuf| {
                s.placement.as_ref().is_some_and(|p| p.root == *path)
            };
            let own: Vec<&SessionInfo> = node
                .sessions
                .iter()
                .filter(|s| !checkouts.iter().any(|c| in_checkout(s, &c.path)))
                .collect();
            session(entries, &own, depth + 1);
            for checkout in &checkouts {
                let listed: Vec<&SessionInfo> = node
                    .sessions
                    .iter()
                    .filter(|s| in_checkout(s, &checkout.path))
                    .collect();
                // A search lists only the worktrees whose sessions it finds.
                if !self.query.trim().is_empty() && listed.is_empty() {
                    continue;
                }
                let (needs, busy) = store
                    .sessions()
                    .iter()
                    .filter(|s| s.workspace_id == info.id && in_checkout(s, &checkout.path))
                    .fold((0, 0), |(needs, busy), s| {
                        (
                            needs + usize::from(store.presentation(s).is_request()),
                            busy + usize::from(store.is_working(s)),
                        )
                    });
                entries.push(Entry::Worktree(WorktreeRow {
                    project: info.id,
                    path: checkout.path.clone(),
                    branch: checkout.branch.clone(),
                    depth: depth + 1,
                    needs,
                    working: busy,
                    active: active_checkout.as_ref() == Some(&checkout.path),
                    diffstat: diffstat_on
                        .then(|| store.diffstat_at(&checkout.path))
                        .flatten(),
                    shared: shared_chip(
                        store
                            .sessions()
                            .iter()
                            .filter(|s| s.workspace_id == info.id),
                        |p| p.root == checkout.path,
                    ),
                }));
                session(entries, &listed, depth + 2);
            }
        };
        for node in nodes {
            match node {
                Node::Workspace(workspace) => {
                    let is_expanded =
                        expanded(workspace.info.id, active == Some(workspace.info.id));
                    entries.push(Entry::Container(ContainerRow {
                        info: workspace.info.clone(),
                        depth: 0,
                        needs: workspace.needs,
                        working: rolled(&workspace.info),
                        branch: None,
                        changed: 0,
                        diffstat: None,
                        shared: None,
                        expanded: is_expanded,
                    }));
                    if is_expanded {
                        for node in &workspace.projects {
                            project(entries, node, 1);
                        }
                        let direct: Vec<&SessionInfo> = workspace.sessions.iter().collect();
                        session(entries, &direct, 1);
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

/// Whether what the window shows is listed under the row at `ix` (of depth `depth`): the
/// focused session, or the worktree the user picked, among the entries after it up to the next
/// row that is not below it.
fn selected_session_below(
    entries: &[Entry],
    ix: usize,
    depth: usize,
    focused: Option<SessionId>,
) -> bool {
    entries[ix + 1..]
        .iter()
        .take_while(|entry| match entry {
            Entry::Session(_) => true,
            Entry::Container(next) => next.depth > depth,
            Entry::Worktree(next) => next.depth > depth,
            _ => false,
        })
        .any(|entry| match entry {
            Entry::Session(session) => Some(session.info.id) == focused,
            Entry::Worktree(worktree) => worktree.active,
            _ => false,
        })
}

/// The chip of the checkout of the first of `sessions` that works in a checkout `here` accepts
/// and shares it with another live agent; `None` when no session there does.
fn shared_chip<'a>(
    mut sessions: impl Iterator<Item = &'a SessionInfo>,
    here: impl Fn(&Placement) -> bool,
) -> Option<SharedChip> {
    sessions.find_map(|session| {
        let placement = session
            .placement
            .as_ref()
            .filter(|p| !p.shared_with.is_empty() && here(p))?;
        Some(SharedChip {
            root: placement.root.clone(),
            badge: crate::sessions::placement_badge(session)?,
        })
    })
}

/// `+12 −3`: the lines a checkout's changed files added and removed.
fn diffstat_text((added, removed): (u64, u64)) -> String {
    format!("+{added} −{removed}")
}

/// `+12 −3` in the diff colours, muted in size: the lines a checkout's changed files added and
/// removed (`panels.sessions.show_diffstat`).
fn diffstat_view(
    (added, removed): (u64, u64),
    theme: &crate::theme::Theme,
    tokens: &crate::theme::Tokens,
) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .gap(px(tokens.spacing))
        .text_size(px(tokens.secondary_font_size))
        .font_weight(gpui::FontWeight::NORMAL)
        .child(
            div()
                .text_color(theme.diff_added)
                .child(SharedString::from(format!("+{added}"))),
        )
        .child(
            div()
                .text_color(theme.diff_removed)
                .child(SharedString::from(format!("−{removed}"))),
        )
}

/// The word of a session's state when its lane draws a glyph; an idle session draws nothing
/// (the hollow circle every idle row used to start with said nothing).
fn lane_word(presentation: &crate::sessions::Presentation) -> Option<String> {
    (presentation.label != "idle").then(|| {
        presentation
            .state_word()
            .map_or_else(|| presentation.label.clone(), str::to_owned)
    })
}

/// The sidebar's text-only empty state: one muted line and one ghost button.
fn open_folder_empty(theme: &crate::theme::Theme, tokens: &crate::theme::Tokens) -> gpui::Div {
    ui::EmptyState::new("No projects yet.")
        .action(
            ui::Button::new("sessions-empty-open", "Open folder…", theme, tokens)
                .ghost()
                .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)),
        )
        .render(theme, tokens)
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
        self.env.regions.rendered(cx.entity_id());
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let settings = self.env.settings.get::<SessionsPanelSettings>();
        let ws_settings = self.env.settings.get::<WorkspacesSettings>();
        self.drawn = true;
        self.renders += 1;
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
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col();
        let list = if entries.is_empty() {
            // A sidebar says it in one muted line (§ Empty states of the clean-aesthetic spike).
            list.child(if hint_query {
                ui::EmptyState::new("Nothing matches the filter.")
                    .render(&theme, &tokens)
                    .h(px(120.0))
            } else {
                open_folder_empty(&theme, &tokens).h(px(120.0))
            })
        } else {
            // Only the rows in view are built: a frame costs what the window shows, not what the
            // daemon has.
            list.child(gpui::list(self.list.clone(), cx.processor(Self::render_entry)).size_full())
                .child(self.bar.element())
        };

        // `⋯`: sort, filters, organize and the panel's settings (the sort menu).
        let more_button = ui::IconButton::new(
            "sessions-more",
            Icon::ActionMore,
            "Sort, filter and organize",
            &theme,
            &tokens,
        )
        .compact()
        .on_click(|event, window, cx| {
            window.dispatch_action(Box::new(ShowSortMenu(event.position())), cx);
        })
        .child(probe(&self.env.regions, Region::SidebarControl("more")))
        .into_any_element();
        // `＋`: Open folder…, New workspace…
        let plus_button = ui::IconButton::new(
            "sessions-plus",
            Icon::ActionAdd,
            "Open folder or new workspace",
            &theme,
            &tokens,
        )
        .compact()
        .on_click(|event, window, cx| {
            window.dispatch_action(Box::new(ShowPlusMenu(event.position())), cx);
        })
        .child(probe(&self.env.regions, Region::SidebarControl("plus")))
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
            .outlined(&theme, &tokens)
            .focus_ring(searching, &theme, &tokens)
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
                "Filter projects and sessions",
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
            .child(panel_header_controls(
                "Projects",
                vec![plus_button, more_button],
                cx,
            ))
            .child(search)
            .child(list)
    }
}

impl SessionsPanel {
    /// One row of the list: what entry `ix` of the frame's entries is.
    /// One row of the virtualized list. A list lays an item out as a root with the list's width
    /// only *available*: an element that does not say otherwise shrinks to its content, so the
    /// row cards (and the hover × at their end) would be as wide as their title. The wrapper
    /// takes the list's width and stretches the row in it.
    fn render_entry(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .w_full()
            .child(self.render_row_entry(ix, window, cx))
            .into_any_element()
    }

    fn render_row_entry(
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
                // A group label: small, tracked capitals with a muted count at the far end;
                // the attention is the rows'.
                let (label, count) = ui::split_count(title);
                div()
                    .id(SharedString::from(format!("section-{key:?}")))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .h(px(tokens.sidebar_row))
                    .mx(px(tokens.sidebar_inset))
                    .px(px(tokens.spacing * 1.5))
                    .mt(px(tokens.spacing))
                    .child(ui::group_header(
                        label,
                        count.map(|c| SharedString::from(c.to_owned())),
                        &theme,
                        &tokens,
                    ))
                    .into_any_element()
            }
            Entry::NoWorkspace => self.render_no_workspace(cx),
            Entry::Container(row) => {
                let current = self.is_active(&row.info, rows.active, rows.active_project);
                // One selection fill per list: while a session under the row is the selected
                // one, the row only keeps its text weight and colour.
                let child_selected =
                    current && selected_session_below(&rows.entries, ix, row.depth, rows.focused);
                let renaming = self
                    .renaming
                    .as_ref()
                    .filter(|r| r.target == RenameTarget::Workspace(row.info.id))
                    .map(|r| r.text.clone());
                self.render_container(
                    row,
                    !row.expanded,
                    current,
                    !child_selected,
                    rows.multi && self.selection.contains(&row.info.id),
                    rows.manual,
                    renaming,
                    cx,
                )
            }
            Entry::Worktree(row) => {
                let focus_listed = rows.focused.is_some_and(|f| {
                    rows.entries
                        .iter()
                        .any(|e| matches!(e, Entry::Session(s) if s.info.id == f))
                });
                self.render_worktree(row, row.active && !focus_listed, cx)
            }
            Entry::Session(row) => {
                self.render_row(row, &rows.settings, rows.focused == Some(row.info.id), cx)
            }
            Entry::EmptyHint => open_folder_empty(&theme, &tokens)
                .h(px(120.0))
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
            .mx(px(tokens.sidebar_inset))
            .px(px(tokens.spacing * 1.5))
            .mt(px(tokens.spacing))
            .rounded(px(tokens.row_radius))
            .drag_over::<DraggedProject>(move |s, _, _, _| s.bg(accent.opacity(0.14)))
            .on_drop(cx.listener(|this, dragged: &DraggedProject, window, cx| {
                this.project_dropped(dragged, None, window, cx);
            }))
            .child(ui::group_label("No workspace", &theme, &tokens))
            .child(probe(&self.env.regions, Region::NoWorkspace))
            .into_any_element()
    }

    /// The in-place name field of a rename: `id_text` keys its elements, `text` is what was
    /// typed.
    fn rename_editor(&self, id_text: &str, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        div()
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
            .border_color(theme.border)
            .focus_ring(true, &theme, &tokens)
            .text_color(theme.fg)
            .children(vim_ui::string_field(
                SharedString::from(format!("rename-text-{id_text}")),
                &self.rename_edit,
                text,
                VimConfig::of(&self.env.settings).enabled,
                "",
                true,
                false,
                self.rename_field(cx),
                &theme,
                &tokens,
            ))
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
        filled: bool,
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
            WorkspaceKind::Home => Some(Icon::NavHome),
            WorkspaceKind::Project => Some(
                if info
                    .project
                    .as_ref()
                    .is_some_and(|p| p.common_dir.is_some())
                {
                    Icon::NavProjectRepository
                } else {
                    Icon::NavProjectFolder
                },
            ),
            WorkspaceKind::Workspace => None,
        };
        let name: AnyElement = match renaming {
            Some(text) => self.rename_editor(&id_text, &text, cx),
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
            .mx(px(tokens.sidebar_inset))
            .pl(px(
                tokens.spacing * 1.5 + row.depth as f32 * tokens.sidebar_indent
            ))
            .pr(px(tokens.spacing * 1.5))
            .rounded(px(tokens.row_radius))
            .text_size(px(tokens.row_font_size))
            .medium()
            .text_color(if current || selected {
                theme.fg
            } else {
                theme.muted
            })
            .cursor_pointer()
            // The current project is the dock's selected fill; a project picked for a bulk action
            // (⌘- or ⇧-click) is the list selection's `accent_soft` fill. No outline.
            .when(current && filled && !selected, move |s| {
                s.bg(theme.sidebar_accent)
            })
            .when(selected, move |s| s.bg(theme.accent_soft))
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
                if is_project {
                    // The project row is the project's own folder, not one of its worktrees.
                    this.env.sessions.update(cx, |store, cx| {
                        store.set_active_checkout(id, None, cx);
                    });
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
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_container(id, cx)))
                    .child(crate::icons::glyph(if collapsed {
                        crate::icons::Icon::ActionDisclose
                    } else {
                        crate::icons::Icon::ActionExpand
                    })),
            )
            .children(icon.map(crate::icons::glyph))
            .when(info.pinned, |s| {
                s.child(
                    div()
                        .flex_none()
                        .text_color(theme.accent)
                        .child(crate::icons::glyph(crate::icons::Icon::ActionPin)),
                )
            })
            .child(name)
            .children(row.branch.clone().map(|branch| {
                div()
                    .flex_none()
                    .max_w(px(96.0))
                    .flex()
                    .items_center()
                    .gap(px(2.0))
                    .text_size(px(11.0))
                    .font_weight(gpui::FontWeight::NORMAL)
                    .text_color(theme.muted)
                    .child(crate::icons::icon(Icon::VcsBranch, 11.0, theme.muted))
                    .child(div().min_w_0().truncate().child(SharedString::from(branch)))
            }))
            // The Requests group above already says which sessions wait: an expanded row only
            // repeats it while that group is off; a collapsed container always says what waits
            // and what works inside it.
            .when(
                row.needs > 0
                    && (collapsed || !self.env.settings.get::<SessionsPanelSettings>().needs_you),
                |s| {
                    s.child(
                        div()
                            .flex_none()
                            .text_color(theme.warning)
                            .flex()
                            .items_center()
                            .gap(px(2.0))
                            .child(crate::icons::glyph(Icon::StatusAttention))
                            .child(row.needs.to_string()),
                    )
                },
            )
            .when(collapsed && row.working > 0, |s| {
                s.child(
                    div()
                        .flex_none()
                        .text_color(theme.accent)
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .child(crate::icons::glyph(Icon::StateWorking))
                        .child(row.working.to_string()),
                )
            })
            .when(row.changed > 0, |s| {
                let tip = SharedString::from(format!(
                    "{} changed file{} in this project",
                    row.changed,
                    if row.changed == 1 { "" } else { "s" }
                ));
                s.child(
                    div()
                        .id(SharedString::from(format!("workspace-changed-{id_text}")))
                        .flex_none()
                        .text_color(theme.accent)
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
                        .child(crate::icons::glyph(Icon::VcsDirty))
                        .child(row.changed.to_string())
                        .child(probe(&self.env.regions, Region::ProjectChanged(id))),
                )
            })
            .children(
                row.diffstat
                    .map(|stat| diffstat_view(stat, &theme, &tokens)),
            )
            .children(row.shared.as_ref().map(|chip| {
                shared_checkout_chip(&theme, &tokens, &chip.badge, &chip.root, &self.env.regions)
            }))
            .children(count_text(info).map(|count| ui::counter(count, &theme, &tokens)))
            .when(
                matches!(kind, WorkspaceKind::Project | WorkspaceKind::Home),
                |s| {
                    let start_id = id_text.clone();
                    s.child(
                        div()
                            .flex_none()
                            .opacity(0.0)
                            .group_hover(group.clone(), |s| s.opacity(1.0))
                            .child(
                                ui::IconButton::new(
                                    SharedString::from(format!("workspace-start-{id_text}")),
                                    Icon::ActionStart,
                                    "Start an agent here",
                                    &theme,
                                    &tokens,
                                )
                                .compact()
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
                                .child(probe(&self.env.regions, Region::ProjectStart(id))),
                            ),
                    )
                },
            )
            .child(probe(&self.env.regions, Region::WorkspaceRow(id)))
            .into_any_element()
    }

    /// `⧉ feature/x  ······  ⚠ 1  ● 2`: a linked worktree of the project above it, one step in
    /// from it. Click picks it (Files and Search follow it); its sessions are the rows below.
    fn render_worktree(
        &self,
        row: &WorktreeRow,
        filled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let hover = card_hover(&theme);
        let (project, path) = (row.project.to_string(), row.path.display().to_string());
        let label = row
            .branch
            .clone()
            .unwrap_or_else(|| folder_label(&row.path));
        div()
            .id(SharedString::from(format!(
                "worktree-{}",
                row.path.display()
            )))
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .h(px(tokens.sidebar_row))
            .mx(px(tokens.sidebar_inset))
            .pl(px(tokens.spacing * 1.5
                + row.depth as f32 * tokens.sidebar_indent
                + 12.0
                + tokens.spacing))
            .pr(px(tokens.spacing * 1.5))
            .rounded(px(tokens.row_radius))
            .text_size(px(tokens.row_font_size))
            .medium()
            .text_color(if row.active { theme.fg } else { theme.muted })
            .cursor_pointer()
            .when(filled, |s| s.bg(theme.sidebar_accent))
            .when(!row.active, move |s| s.hover(move |s| s.bg(hover)))
            .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
                window.dispatch_action(
                    Box::new(OpenCheckout(CheckoutOpen {
                        project: project.clone(),
                        path: path.clone(),
                    })),
                    cx,
                );
            }))
            .child(crate::icons::glyph(Icon::VcsWorktree))
            .child({
                let tip = SharedString::from(row.path.display().to_string());
                div()
                    .id(SharedString::from(format!(
                        "worktree-label-{}",
                        row.path.display()
                    )))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
                    .child(SharedString::from(label))
            })
            .children(
                row.diffstat
                    .map(|stat| diffstat_view(stat, &theme, &tokens)),
            )
            .children(row.shared.as_ref().map(|chip| {
                shared_checkout_chip(&theme, &tokens, &chip.badge, &chip.root, &self.env.regions)
            }))
            .when(row.needs > 0, |s| {
                s.child(
                    div()
                        .flex_none()
                        .text_color(theme.warning)
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .child(crate::icons::glyph(Icon::StatusAttention))
                        .child(row.needs.to_string()),
                )
            })
            .when(row.working > 0, |s| {
                s.child(
                    div()
                        .flex_none()
                        .text_color(theme.accent)
                        .flex()
                        .items_center()
                        .gap(px(2.0))
                        .child(crate::icons::glyph(Icon::StateWorking))
                        .child(row.working.to_string()),
                )
            })
            .child(probe(&self.env.regions, Region::worktree(&row.path)))
            .into_any_element()
    }

    /// One line: the state lane (a glyph unless the session is idle), kind icon, title, the
    /// project of a row away from its project row, the placement badge, agent badge,
    /// restart glyph, muted marker, age and the hover ×. Never a second line.
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
        // A failure the user has looked at stays listed but no longer wears the unread dot.
        let unread_state = self.env.sessions.read(cx).unread(info.id);
        let unread = unread_state.is_some_and(crate::sessions::Unread::is_unseen);
        let live = info.state.is_live();
        let hover = card_hover(&theme);
        let id = info.id.to_string();
        let open_id = id.clone();
        let menu_id = id.clone();
        let mut badge = self.env.sessions.read(cx).agent_badge(info.id);
        if presentation.is_request() {
            // The lane already says a request waits; its ▲ does not say it twice.
            badge.blocked.clear();
        }
        let mut badge = agent_badge(
            &theme,
            &badge,
            BadgeSite::Row,
            info.id,
            &self.env.regions,
            cx,
        );
        // A restart is a glyph with its sentence in the tooltip, never a word that eats the
        // title; the summary card has the words (`recovery_summary.rs`).
        let mut recovery = hutan_client::recovery::chip(info).map(|chip| {
            recovery_chip(
                &theme,
                &chip,
                BadgeSite::Row,
                info.id,
                true,
                &self.env.regions,
                cx,
            )
        });
        // The marker of a session whose notifications are muted (`notify.mute`).
        let mut muted = info.muted.then(|| {
            div()
                .id(SharedString::from(format!("session-muted-{id}")))
                .relative()
                .flex_none()
                .tooltip(|_, cx| {
                    cx.new(|_| TextTooltip::new("Notifications muted".into()))
                        .into()
                })
                .child(icon(Icon::StateMuted, 12.0, theme.muted))
                .child(probe(&self.env.regions, Region::SessionMuted(info.id)))
        });
        // Seen work waiting for review: the row keeps its state (idle draws nothing in the
        // lane) and wears a small link to the checkout's changes, never a state word.
        let mut review = self
            .env
            .sessions
            .read(cx)
            .changes_to_review(info.id)
            .then(|| {
                info.placement
                    .as_ref()
                    .map(|p| p.root.display().to_string())
            })
            .flatten()
            .map(|root| {
                ui::IconButton::new(
                    SharedString::from(format!("session-review-{id}")),
                    Icon::StateReview,
                    crate::sessions::agents_text::CHANGES_TO_REVIEW,
                    &theme,
                    &tokens,
                )
                .compact()
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(ReviewChangesAt(root.clone())), cx);
                })
                .child(probe(&self.env.regions, Region::SessionReview(info.id)))
            });
        let renaming = self
            .renaming
            .as_ref()
            .filter(|r| r.target == RenameTarget::Session(info.id))
            .map(|r| r.text.clone());
        // The lane: the harness glyph (a shell's prompt, an agent's monogram) with the state's
        // badge on it. An idle or read session has no badge; the room stays, it is the spine
        // the names align to.
        let lane_state = card::badge_state(info, &presentation, unread_state);
        let lane_tip = lane_word(&presentation)
            .or_else(|| lane_state.word().map(str::to_owned))
            .map(|word| {
                SharedString::from(if presentation.detail.is_empty() {
                    word
                } else {
                    format!("{word} · {}", presentation.detail)
                })
            });
        let ground = if selected {
            theme.sidebar_accent
        } else {
            theme.sidebar
        };
        let mut lane = Some(
            div()
                .id(SharedString::from(format!("row-state-{id}")))
                .relative()
                .flex_none()
                .when_some(lane_tip, |s, tip| {
                    s.tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
                })
                .child(ui::row_lane(
                    SharedString::from(format!("row-lane-{id}")),
                    session_icon(info),
                    lane_state,
                    ground,
                    selected,
                    &theme,
                ))
                .when(unread, |s| {
                    s.child(probe(&self.env.regions, Region::SessionUnread(info.id)))
                }),
        );
        let gap = tokens.spacing * 1.5;
        // The width of the row's lead lane (a pack metric).
        let lane_width = crate::theme::metrics().sidebar_lane;

        // One line, always. A row away from its project row (Requests, Ready for review) says
        // whose it is in muted text after its title; a session off the main checkout wears its
        // placement badge there too. A folder outside the checkout is in the title's tooltip.
        let mut whose: Vec<AnyElement> = Vec::new();
        if let Some(sub) = row.subtitle.as_ref() {
            whose.extend(sub.container.clone().map(|container| {
                div()
                    .id(SharedString::from(format!("session-project-{id}")))
                    .flex_none()
                    .max_w(px(96.0))
                    .truncate()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(SharedString::from(container))
                    .into_any_element()
            }));
            whose.extend(sub.badge.as_ref().map(|badge| {
                placement_badge(
                    &theme,
                    badge,
                    BadgeSite::Row,
                    info.id,
                    &self.env.regions,
                    cx,
                )
            }));
        }
        let mut line = div()
            .flex()
            .items_center()
            .gap(px(gap))
            .h(px(tokens.sidebar_row))
            .w_full()
            .min_w_0();
        // The pointer swaps the age for the actions (× close; ⑂ fork joins it with CHAT-FORK-B).
        let row_group = SharedString::from(format!("session-group-{id}"));
        let mut actions = self.env.sessions.read(cx).supports_close().then(|| {
            let close_id = id.clone();
            let session = info.id;
            div()
                .flex_none()
                .flex()
                .items_center()
                .opacity(0.0)
                .group_hover(row_group.clone(), |s| s.opacity(1.0))
                .child(
                    ui::IconButton::new(
                        SharedString::from(format!("session-close-{id}")),
                        Icon::ActionClose,
                        "Close session",
                        &theme,
                        &tokens,
                    )
                    .compact()
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(CloseSessionById(close_id.clone())), cx);
                        cx.stop_propagation();
                    })
                    .child(probe(&self.env.regions, Region::SessionClose(session))),
                )
        });
        for field in &settings.row {
            line =
                match field.as_str() {
                    "state" | "kind" => line.children(lane.take()),
                    "title" => line
                        .child(match renaming.as_deref() {
                            Some(text) => self.rename_editor(&id, text, cx),
                            None => {
                                let title = div()
                                    .id(SharedString::from(format!("session-title-{id}")))
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(tokens.row_font_size))
                                    .when(unread, |s| s.font_weight(gpui::FontWeight::SEMIBOLD))
                                    .when(!live, |s| s.text_color(theme.faint))
                                    .child(SharedString::from(info.title.clone()));
                                // The name says what the session is; the prompt behind a short title
                                // and where the session is, in full, are one hover away.
                                let prompt = self
                                    .env
                                    .sessions
                                    .read(cx)
                                    .tasks()
                                    .prompt_of(info.id, &info.title);
                                let tip = match (prompt, session_folder(info)) {
                                    (Some(prompt), Some(path)) => {
                                        Some(format!("{prompt}\n{}", path.display()))
                                    }
                                    (Some(prompt), None) => Some(prompt),
                                    (None, Some(path)) => Some(path.display().to_string()),
                                    (None, None) => None,
                                };
                                match tip.map(SharedString::from) {
                                    Some(tip) => title
                                        .tooltip(move |_, cx| {
                                            cx.new(|_| TextTooltip::new(tip.clone())).into()
                                        })
                                        .into_any_element(),
                                    None => title.into_any_element(),
                                }
                            }
                        })
                        .children(std::mem::take(&mut whose))
                        .children(badge.take())
                        .children(recovery.take())
                        .children(review.take())
                        .children(muted.take()),
                    "age" => {
                        // Since something last happened in it, not since it was made.
                        let active_ms = self.env.sessions.read(cx).last_activity_ms(info);
                        let swaps = actions.is_some();
                        let age = div()
                            .id(SharedString::from(format!("row-age-{}", info.id)))
                            .flex_none()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.faint)
                            .when(swaps, |s| {
                                s.group_hover(row_group.clone(), |s| s.opacity(0.0))
                            })
                            .tooltip(time_tooltip(active_ms))
                            .child(SharedString::from(age_label(active_ms)));
                        line.child(
                            div()
                                .relative()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_end()
                                .when(swaps, |s| s.min_w(px(CARD_ACTIONS)))
                                .child(age)
                                .children(actions.take().map(|actions| {
                                    actions.absolute().top_0().bottom_0().right_0()
                                })),
                        )
                    }
                    _ => line,
                };
        }
        // A row without an age field still has its actions, at its end.
        if let Some(actions) = actions.take() {
            line = line.child(actions);
        }

        let news = row
            .news
            .as_ref()
            .map(|news| self.news_line(info, news, selected, gap + lane_width, cx));
        div()
            .id(SharedString::from(format!("session-row-{id}")))
            .group(row_group)
            .relative()
            .flex()
            .flex_col()
            .mx(px(tokens.sidebar_inset))
            .my(px(1.0))
            .pl(px(gap + row.depth as f32 * tokens.sidebar_indent))
            .pr(px(gap))
            .rounded(px(tokens.row_radius))
            .cursor_pointer()
            // The selected row is the sidebar's selected fill, with no outline.
            .when(selected, move |s| s.bg(theme.sidebar_accent))
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
            .children(news)
            .child(probe(&self.env.regions, Region::SessionRow(info.id)))
            .into_any_element()
    }

    /// The second line of an agent card, under its name: what is going on, one line, and the
    /// model as a mono chip at its end. A request is `warning` and a failure `danger` (the
    /// selected row keeps both in its ink: state colours are not AA on that fill); the tool it
    /// runs, the unread reply and the agent's own title are muted. The first card no hook speaks
    /// for has what hooks add and the link to *Agent integrations…* here instead.
    fn news_line(
        &self,
        info: &SessionInfo,
        news: &News,
        selected: bool,
        indent: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let color = super::common::state_word_color(&theme, news.kind.tone(), selected);
        let hint = news.kind == NewsKind::HooksHint;
        let region = if hint {
            Region::HooksHint(info.id)
        } else {
            Region::SessionNote(info.id)
        };
        let text = SharedString::from(news.text.clone());
        let tip = if hint {
            SharedString::from(card::HOOKS_HINT_TIP)
        } else {
            text.clone()
        };
        div()
            .id(SharedString::from(format!("session-note-{}", info.id)))
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .h(px(tokens.sidebar_card - tokens.sidebar_row))
            .pl(px(indent))
            .min_w_0()
            .text_size(px(tokens.secondary_font_size))
            .text_color(color)
            .child(
                div()
                    .id(SharedString::from(format!("session-note-text-{}", info.id)))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(tip.clone())).into())
                    .child(text),
            )
            .when(hint, |s| {
                s.child(
                    div()
                        .id(SharedString::from(format!("hooks-hint-link-{}", info.id)))
                        .flex_none()
                        .cursor_pointer()
                        .text_color(theme.link)
                        .hover(move |s| s.text_color(theme.fg))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(|_, window, cx| {
                            window.dispatch_action(
                                Box::new(OpenSettingsFor("integrations".to_owned())),
                                cx,
                            );
                            cx.stop_propagation();
                        })
                        .child("Agent integrations…"),
                )
            })
            .children(
                news.model
                    .as_ref()
                    .map(|model| ui::chip_mono(SharedString::from(model.clone()), &theme, &tokens)),
            )
            .child(probe(&self.env.regions, region))
            .into_any_element()
    }
}
