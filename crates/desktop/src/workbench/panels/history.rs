//! The `history` panel: **Agent Session History** (docs/history-index.md § The History panel).
//!
//! While indexing is off the panel is one card that explains what turning it on reads, lets the
//! user pick the sources and offers **Enable**. While it is on, the panel lists the daemon's
//! session catalog (`history.sessions`, protocol 0.29): grouped by project, folder or agent,
//! scoped to the workspace, the project or everything, with a live filter, a scan strip and the
//! `X of Y sessions` bar. A typed query that is not a `repo:`/`path:` filter is also searched in
//! the transcripts (`history.search`) after a quiet 250 ms or on Return; the ranked sessions
//! replace the list, ungrouped, with the matching words marked. A daemon that predates the
//! catalog gets the search alone, with a note.
//!
//! Click a row to read more of it; Resume runs the agent's resume command in a new terminal tab
//! in the session's project. The panel only sends `history.*` requests; the actions that start
//! sessions, open files and folders (`ResumeHistoryHit`, `OpenHistoryHit`, `OpenHistoryFile`,
//! `OpenHistoryDirectory`, `RevealHistoryFile`) are the workbench's. Rendering is in
//! `history_view.rs`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    Context, FocusHandle, Focusable, KeyDownEvent, ListAlignment, ListOffset, ListState, Pixels,
    Point, SharedString, Task, Window, px,
};
use hutan_client::{ClientError, ConnectionState};
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::history::{
    HistoryHit, HistoryHits, HistoryScope, HistorySessionSort, HistorySessions,
    HistorySessionsRequest, HistorySource, HistoryStatusInfo, SESSIONS_DEFAULT_LIMIT,
    SESSIONS_MAX_LIMIT,
};
use serde_json::Value as Json;

use super::history_catalog::{
    Entry, GroupBy, ItemKey, NARROW_WIDTH, REFRESH_EVERY, RowAction, SCAN_POLL, SEARCH_DEBOUNCE,
    SEARCH_LIMIT, SessionItem, actions, entries, flat_entries, group_items, has_qualifier,
    session_id_text,
};
use super::history_model::{Filters, HutanTarget, SOURCES, shell_join};
use crate::sessions::StoreChanged;
use crate::settings::{HistoryScope as SettingsScope, HistorySettings};
use crate::workbench::actions::{
    CopyHistoryCommand, Notify, OpenHistoryDirectory, OpenHistoryFile, OpenHistoryHit,
    ReopenSessionById, ResumeHistoryHit, RevealHistoryFile,
};
use crate::workbench::chat::vim::Outcome;
use crate::workbench::registry::PanelEnv;
use crate::workbench::text_field::FieldRef;
use crate::workbench::vim_ui::{FieldEdit, VimConfig};

pub(crate) use super::history_view::pill;

/// How often the index status is read while indexing settles after Enable or Rebuild.
const SETTLE_POLL: Duration = Duration::from_millis(500);
/// The polls stop after the row count has not changed for this long once it is above zero …
const SETTLE_QUIET: Duration = Duration::from_secs(2);
/// … or this long while it is still zero (a first scan of large directories takes a while).
const SETTLE_QUIET_EMPTY: Duration = Duration::from_secs(10);
/// Longest a settle poll runs.
const SETTLE_MAX: Duration = Duration::from_secs(60);

/// What the panel knows about the index.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Index {
    /// Not asked yet, or the daemon is not reachable.
    Unknown,
    /// The connected daemon predates `history.*` (protocol 0.8).
    Unsupported,
    Known(HistoryStatusInfo),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Results {
    /// No full-text results: the catalog is shown.
    Idle,
    Hits(Vec<HistoryHit>),
    Failed(String),
}

/// A destructive action waiting for its confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Clear,
    Rebuild,
}

/// What the list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The session catalog, grouped.
    Catalog,
    /// Ranked full-text results, ungrouped.
    Search,
}

/// What identifies a list line across rebuilds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryId {
    Row(ItemKey),
    Group(String),
}

/// A menu that is open over the panel.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Menu {
    GroupBy(Point<Pixels>),
    Sources(Point<Pixels>),
    Sort(Point<Pixels>),
    Row(ItemKey, Point<Pixels>),
}

pub struct HistoryPanel {
    pub(super) env: PanelEnv,
    pub(super) search_focus: FocusHandle,
    /// Drawn since the refresh timer last looked.
    drawn: bool,
    pub(super) query: String,
    /// Vim state of the search field (`editor.vim_mode`).
    pub(super) query_edit: FieldEdit,
    pub(super) filters: Filters,
    pub(super) scope: HistoryScope,
    pub(super) group_by: GroupBy,
    pub(super) sort: HistorySessionSort,
    /// Rows the catalog asks for ("Show more" raises it).
    limit: u32,
    pub(super) index: Index,
    /// The last `history.sessions` answer.
    pub(super) catalog: Option<HistorySessions>,
    pub(super) catalog_error: Option<String>,
    /// A catalog request is on its way (the old rows stay until it answers).
    pub(super) loading: bool,
    pub(super) results: Results,
    /// A full-text search (debounce included) has not answered yet.
    searching: bool,
    // The list as drawn: items (catalog rows or hits), their groups, the flat entries.
    pub(super) items: Vec<SessionItem>,
    pub(super) groups: Vec<super::history_catalog::Group>,
    pub(super) entries: Vec<Entry>,
    pub(super) list: ListState,
    /// The list starts at the top the next time it is rebuilt (the query, scope or order changed).
    top_wanted: bool,
    pub(super) expanded: HashSet<ItemKey>,
    pub(super) collapsed: HashSet<String>,
    pub(super) selected: Option<ItemKey>,
    /// The session to select (and open) once the results arrive (⌘J `?` fallback).
    pick: Option<ItemKey>,
    pub(super) scan_open: bool,
    pub(super) menu: Option<Menu>,
    /// The off-state card's checkboxes, in [`SOURCES`] order (unticked until the user ticks
    /// them: consent is never pre-given), and what the daemon has now.
    pub(super) card_sources: [bool; 4],
    card_daemon: Option<[bool; 4]>,
    /// The card shows its sources (the user pressed *Set up history…*); until then it is one
    /// short line and a button.
    pub(super) card_open: bool,
    confirm: Option<Confirm>,
    /// What a running request is called ("Enabling…"); the buttons wait for it.
    pub(super) busy: Option<&'static str>,
    /// The last failed request, shown in the index details.
    pub(super) error: Option<String>,
    was_connected: bool,
    /// Times the panel was rendered (tests).
    renders: usize,
    /// What each row shows that takes work to make, by item index, as of the last rebuild (see
    /// [`RowText`](super::history_view::RowText)).
    pub(super) row_cache: RefCell<HashMap<usize, super::history_view::RowText>>,
    /// The user's home directory, read once (rows shorten paths against it).
    pub(super) home: Option<PathBuf>,
    /// `history.show_snippets` and the time, as of this frame: rows do not read them again.
    pub(super) show_snippets: bool,
    pub(super) render_ms: u64,
    /// The container and project the workbench showed when the catalog was last asked.
    last_ids: (Option<WorkspaceId>, Option<WorkspaceId>),
    /// Focus the search field once it is on screen.
    focus_wanted: bool,
    search_task: Option<Task<()>>,
    catalog_task: Option<Task<()>>,
    rescan_task: Option<Task<()>>,
    status_task: Option<Task<()>>,
    settle_task: Option<Task<()>>,
    card_task: Option<Task<()>>,
    _timer: Task<()>,
}

impl std::fmt::Debug for HistoryPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryPanel")
            .field("query", &self.query)
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

pub(super) fn now_ms() -> u64 {
    hutan_client::time::now_ms()
}

fn is_disabled(error: &ClientError) -> bool {
    matches!(error, ClientError::Remote(body) if body.code == ErrorCode::InvalidState)
}

fn index_rows(info: &HistoryStatusInfo) -> u64 {
    info.sources.iter().map(|s| s.rows).sum()
}

impl HistoryPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        // The panel is a function of the connection, of the workspace and project in view
        // (its scope), of their names and of which sessions exist; a notification about
        // anything else (a session's title or state, the focus, attention) is not its business.
        cx.subscribe(
            &env.sessions,
            |this, sessions, change: &StoreChanged, cx| {
                if !change.intersects(
                    StoreChanged::CONNECTION
                        | StoreChanged::ACTIVE
                        | StoreChanged::WORKSPACES
                        | StoreChanged::SESSIONS,
                ) {
                    return;
                }
                let store = sessions.read(cx);
                let connected = store.connection_state() == ConnectionState::Connected;
                let ids = (store.active_workspace_id(), store.active_project_id());
                if connected && !this.was_connected {
                    this.was_connected = true;
                    this.refresh_status(cx);
                    this.reload(false, cx);
                }
                this.was_connected = connected;
                if ids != this.last_ids {
                    this.last_ids = ids;
                    // The workspace or project in view moved: a scoped list is another list.
                    if this.scope != HistoryScope::All {
                        this.top_wanted = true;
                        this.reload(false, cx);
                        this.search(false, cx);
                    }
                }
                cx.notify();
            },
        )
        .detach();
        let timer = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH_EVERY).await;
                // A panel nobody saw since the last look is not worth a request or a frame.
                let alive = this.update(cx, |this, cx| {
                    if std::mem::take(&mut this.drawn) {
                        this.tick(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let settings = env.settings.get::<HistorySettings>();
        let filters = Filters::from_settings(&settings);
        let (workspace, project, was_connected) = {
            let store = env.sessions.read(cx);
            (
                store.active_workspace_id(),
                store.active_project_id(),
                store.connection_state() == ConnectionState::Connected,
            )
        };
        let mut panel = Self {
            env,
            search_focus: cx.focus_handle(),
            drawn: false,
            query: String::new(),
            query_edit: FieldEdit::new(),
            filters,
            scope: match settings.project {
                SettingsScope::Workspace => HistoryScope::Workspace,
                SettingsScope::All => HistoryScope::All,
            },
            group_by: GroupBy::default(),
            sort: HistorySessionSort::default(),
            limit: SESSIONS_DEFAULT_LIMIT,
            index: Index::Unknown,
            catalog: None,
            catalog_error: None,
            loading: false,
            results: Results::Idle,
            searching: false,
            items: Vec::new(),
            groups: Vec::new(),
            entries: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.0)),
            top_wanted: true,
            expanded: HashSet::new(),
            collapsed: HashSet::new(),
            selected: None,
            pick: None,
            scan_open: false,
            menu: None,
            card_sources: [false; 4],
            card_daemon: None,
            card_open: false,
            confirm: None,
            busy: None,
            error: None,
            was_connected,
            renders: 0,
            row_cache: RefCell::new(HashMap::new()),
            home: std::env::home_dir(),
            show_snippets: true,
            render_ms: now_ms(),
            last_ids: (workspace, project),
            focus_wanted: false,
            search_task: None,
            catalog_task: None,
            rescan_task: None,
            status_task: None,
            settle_task: None,
            card_task: None,
            _timer: timer,
        };
        if was_connected {
            panel.refresh_status(cx);
        }
        panel
    }

    // ---- read access (tests, the workbench) ------------------------------------------------

    /// The search text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The sessions the last full-text search found, best first (empty while the catalog is
    /// shown).
    pub fn hits(&self) -> &[HistoryHit] {
        match &self.results {
            Results::Hits(hits) => hits,
            _ => &[],
        }
    }

    /// What the list shows now.
    pub fn mode(&self) -> Mode {
        match self.results {
            Results::Idle => Mode::Catalog,
            _ => Mode::Search,
        }
    }

    /// The rows of the last `history.sessions` answer, in the daemon's order.
    pub fn sessions(&self) -> &[hutan_protocol::history::HistorySessionRow] {
        self.catalog.as_ref().map_or(&[], |c| c.sessions.as_slice())
    }

    /// The last `history.sessions` answer.
    pub fn catalog(&self) -> Option<&HistorySessions> {
        self.catalog.as_ref()
    }

    /// The error of the last `history.sessions` request.
    pub fn catalog_error(&self) -> Option<&str> {
        self.catalog_error.as_deref()
    }

    /// The sessions the list draws, in order (catalog rows, or hits in search mode).
    pub fn items(&self) -> &[SessionItem] {
        &self.items
    }

    /// The group headers the list draws, with their row counts (catalog mode).
    pub fn group_headers(&self) -> Vec<(String, usize, bool)> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Header {
                    label,
                    count,
                    collapsed,
                    ..
                } => Some((label.clone(), *count, *collapsed)),
                _ => None,
            })
            .collect()
    }

    /// The index status as the daemon last reported it; `None` while unknown or unsupported.
    pub fn status(&self) -> Option<&HistoryStatusInfo> {
        match &self.index {
            Index::Known(info) => Some(info),
            _ => None,
        }
    }

    /// True while a full-text search (debounce included) has not answered yet.
    pub fn searching(&self) -> bool {
        self.searching
    }

    /// True while a catalog request has not answered yet.
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// The filter chips as they are now.
    pub fn filters(&self) -> &Filters {
        &self.filters
    }

    pub fn scope(&self) -> HistoryScope {
        self.scope
    }

    pub fn group_by(&self) -> GroupBy {
        self.group_by
    }

    pub fn sort(&self) -> HistorySessionSort {
        self.sort
    }

    /// The row (an index into [`items`](Self::items)) that is selected.
    pub fn selected(&self) -> Option<usize> {
        let key = self.selected.as_ref()?;
        self.items.iter().position(|item| item.item_key() == *key)
    }

    /// True when the row of `item` shows its details.
    pub fn is_expanded(&self, item: usize) -> bool {
        self.items
            .get(item)
            .is_some_and(|i| self.expanded.contains(&i.item_key()))
    }

    /// The confirmation that is pending, if any.
    pub fn pending_confirmation(&self) -> Option<Confirm> {
        self.confirm
    }

    /// The text of the last failed request.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The scan strip's per-source breakdown is open.
    pub fn scan_open(&self) -> bool {
        self.scan_open
    }

    /// The name of the open menu (`group`, `sources`, `sort`, `row`).
    pub fn menu_open(&self) -> Option<&'static str> {
        self.menu.as_ref().map(|m| match m {
            Menu::GroupBy(_) => "group",
            Menu::Sources(_) => "sources",
            Menu::Sort(_) => "sort",
            Menu::Row(..) => "row",
        })
    }

    /// The off-state card's checkboxes: Hutan, Claude, Codex, OMP.
    pub fn card_sources(&self) -> [bool; 4] {
        self.card_sources
    }

    /// Whether the off-state card shows its sources (after *Set up history…*).
    pub fn card_open(&self) -> bool {
        self.card_open
    }

    /// True while the panel is narrower than [`NARROW_WIDTH`] (as of the last frame).
    pub fn is_narrow(&self) -> bool {
        self.env
            .regions
            .get(crate::workbench::Region::History("panel"))
            .is_some_and(|b| f32::from(b.size.width) < NARROW_WIDTH)
    }

    /// The header's title: "Agent Session History", or "History" in a narrow panel.
    pub fn header_title(&self) -> &'static str {
        if self.is_narrow() {
            "History"
        } else {
            "Agent Session History"
        }
    }

    /// The list line at the top of the view (the scroll position).
    pub fn first_visible_entry(&self) -> usize {
        self.list.logical_scroll_top().item_ix
    }

    /// Scrolls the list so that `entry` is at its top.
    pub fn scroll_to_entry(&self, entry: usize) {
        self.list.scroll_to(ListOffset {
            item_ix: entry,
            offset_in_item: px(0.0),
        });
    }

    /// The lines of the list: group headers, rows and "Show more".
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// True when the daemon answers `history.sessions` (protocol 0.29).
    pub fn catalog_supported(&self, cx: &gpui::App) -> bool {
        self.env
            .sessions
            .read(cx)
            .connection()
            .supports_history_sessions()
    }

    /// Focuses the search field; while it is not on screen yet (the index state is still
    /// unknown, or indexing is off) the focus moves there as soon as it appears. The index
    /// state is read again, so a `hutan index enable` made meanwhile shows up.
    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_status(cx);
        if self.status().is_some_and(|info| info.enabled) {
            self.focus_wanted = false;
            window.focus(&self.search_focus, cx);
        } else {
            self.focus_wanted = true;
        }
    }

    pub(super) fn take_focus_wanted(&mut self) -> bool {
        std::mem::take(&mut self.focus_wanted)
    }

    pub(super) fn mark_drawn(&mut self) {
        self.drawn = true;
        self.renders += 1;
    }

    /// How many times the panel was rendered (tests).
    pub fn renders(&self) -> usize {
        self.renders
    }

    // ---- searching and filtering -----------------------------------------------------------

    /// Sets the search text: the catalog filter follows at once, the full-text search after the
    /// debounce.
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.query != query {
            self.query = query.to_owned();
            self.results = Results::Idle;
            self.search_task = None;
            self.searching = false;
            self.limit = SESSIONS_DEFAULT_LIMIT;
            self.top_wanted = true;
            self.reload(false, cx);
            self.search(true, cx);
            self.rebuild(cx);
        }
        cx.notify();
    }

    /// Replaces the search text, searches at once and selects (and opens) the hit of `source` /
    /// `key` when the answer has it (the ⌘J `?` fallback).
    pub fn show_hit(
        &mut self,
        query: &str,
        source: HistorySource,
        key: &str,
        cx: &mut Context<Self>,
    ) {
        self.query = query.to_owned();
        self.pick = Some(ItemKey {
            source,
            key: key.to_owned(),
        });
        self.top_wanted = true;
        self.reload(false, cx);
        self.search(false, cx);
        cx.notify();
    }

    /// Replaces a filter chip and asks again.
    pub fn edit_filters(&mut self, edit: impl FnOnce(&mut Filters), cx: &mut Context<Self>) {
        edit(&mut self.filters);
        self.top_wanted = true;
        self.reload(false, cx);
        self.search(false, cx);
        cx.notify();
    }

    /// Workspace, Project or All. Project needs a project in view.
    pub fn set_scope(&mut self, scope: HistoryScope, cx: &mut Context<Self>) {
        if scope == HistoryScope::Project && !self.project_known(cx) {
            return;
        }
        if self.scope != scope {
            self.scope = scope;
            self.top_wanted = true;
            self.reload(false, cx);
            self.search(false, cx);
        }
        cx.notify();
    }

    pub fn set_group_by(&mut self, by: GroupBy, cx: &mut Context<Self>) {
        self.group_by = by;
        self.top_wanted = true;
        self.menu = None;
        self.rebuild(cx);
        cx.notify();
    }

    pub fn set_sort(&mut self, sort: HistorySessionSort, cx: &mut Context<Self>) {
        self.menu = None;
        if self.sort != sort {
            self.sort = sort;
            self.top_wanted = true;
            self.reload(false, cx);
        }
        cx.notify();
    }

    /// Adds `source` to the sources the list shows, or removes it; the last one stays.
    pub fn toggle_source(&mut self, source: HistorySource, cx: &mut Context<Self>) {
        let mut chosen: Vec<HistorySource> = if self.filters.sources.is_empty() {
            SOURCES.to_vec()
        } else {
            self.filters.sources.clone()
        };
        match chosen.iter().position(|s| *s == source) {
            Some(at) if chosen.len() > 1 => {
                chosen.remove(at);
            }
            Some(_) => return,
            None => chosen.push(source),
        }
        let all = SOURCES.iter().all(|s| chosen.contains(s));
        self.edit_filters(
            |f| {
                f.sources = if all {
                    Vec::new()
                } else {
                    SOURCES.into_iter().filter(|s| chosen.contains(s)).collect()
                };
            },
            cx,
        );
    }

    /// True when `source` is among the sources the list shows.
    pub fn source_shown(&self, source: HistorySource) -> bool {
        self.filters.sources.is_empty() || self.filters.sources.contains(&source)
    }

    /// Raises the rows asked for by one page (the "Show more" row).
    pub fn show_more(&mut self, cx: &mut Context<Self>) {
        self.limit = (self.limit + SESSIONS_DEFAULT_LIMIT).min(SESSIONS_MAX_LIMIT);
        self.reload(false, cx);
        cx.notify();
    }

    /// The refresh button: the daemon looks at the transcript files again; a full-text search is
    /// asked again.
    pub fn refresh_now(&mut self, cx: &mut Context<Self>) {
        self.refresh_status(cx);
        if self.mode() == Mode::Search || self.searching {
            self.search(false, cx);
        }
        self.reload(true, cx);
        cx.notify();
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.status().is_some_and(|s| s.enabled) {
            self.reload(false, cx);
        }
        cx.notify();
    }

    fn project_known(&self, cx: &gpui::App) -> bool {
        self.env.sessions.read(cx).active_project_id().is_some()
    }

    /// Whether the Project segment can be chosen.
    pub fn project_scope_available(&self, cx: &gpui::App) -> bool {
        self.project_known(cx)
    }

    /// The scope and container the requests name: Workspace and Project fall back to All while
    /// the workbench shows no such container.
    fn request_scope(&self, cx: &gpui::App) -> (HistoryScope, Option<WorkspaceId>) {
        let store = self.env.sessions.read(cx);
        match self.scope {
            HistoryScope::Workspace => match store.active_workspace_id() {
                Some(id) => (HistoryScope::Workspace, Some(id)),
                None => (HistoryScope::All, None),
            },
            HistoryScope::Project => match store.active_project_id() {
                Some(id) => (HistoryScope::Project, Some(id)),
                None => (HistoryScope::All, None),
            },
            HistoryScope::All => (HistoryScope::All, None),
        }
    }

    /// The folder a full-text search is limited to, for the scope.
    fn scope_folder(&self, cx: &gpui::App) -> Option<std::path::PathBuf> {
        let store = self.env.sessions.read(cx);
        let workspace = match self.scope {
            HistoryScope::All => return None,
            HistoryScope::Workspace => store.active_workspace().or_else(|| store.home_workspace()),
            HistoryScope::Project => store.active_project(),
        };
        workspace.and_then(|w| w.roots.first().cloned())
    }

    // ---- the catalog -----------------------------------------------------------------------

    fn catalog_ready(&self, cx: &gpui::App) -> bool {
        let store = self.env.sessions.read(cx);
        store.connection_state() == ConnectionState::Connected
            && store.connection().supports_history_sessions()
            && matches!(&self.index, Index::Known(info) if info.enabled)
    }

    fn catalog_request(&self, refresh: bool, cx: &gpui::App) -> HistorySessionsRequest {
        let (scope, workspace_id) = self.request_scope(cx);
        HistorySessionsRequest {
            scope,
            workspace_id,
            filter: self.query.trim().to_owned(),
            sort: self.sort,
            sources: self.filters.sources.clone(),
            limit: self.limit,
            refresh,
        }
    }

    /// Asks the daemon for the catalog (`refresh`: look at the transcript files first). An
    /// earlier request that has not answered is dropped; the rows stay until the answer.
    pub fn reload(&mut self, refresh: bool, cx: &mut Context<Self>) {
        if !self.catalog_ready(cx) {
            return;
        }
        let request = self.catalog_request(refresh, cx);
        let connection = self.env.sessions.read(cx).connection().clone();
        self.loading = true;
        self.catalog_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_sessions(request).await })
                .await;
            this.update(cx, |this, cx| this.finish_catalog(result, cx))
                .ok();
        }));
        cx.notify();
    }

    fn finish_catalog(
        &mut self,
        result: Result<HistorySessions, ClientError>,
        cx: &mut Context<Self>,
    ) {
        self.loading = false;
        self.rescan_task = None;
        match result {
            Ok(answer) => {
                self.catalog_error = None;
                let scanning = answer.scan.scanning;
                self.catalog = Some(answer);
                self.rebuild(cx);
                if scanning {
                    // The daemon is still reading transcripts: ask again soon.
                    self.rescan_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SCAN_POLL).await;
                        this.update(cx, |this, cx| this.reload(false, cx)).ok();
                    }));
                }
            }
            Err(error) if is_disabled(&error) => {
                // Indexing was switched off behind our back.
                self.catalog = None;
                self.rebuild(cx);
                self.refresh_status(cx);
            }
            Err(ClientError::Disconnected) => {}
            Err(error) => self.catalog_error = Some(error.to_string()),
        }
        cx.notify();
    }

    // ---- the full-text search --------------------------------------------------------------

    /// True when the typed query is waiting for its full-text results.
    fn search_pending(&self) -> bool {
        let query = self.query.trim();
        !query.is_empty() && !has_qualifier(query) && matches!(self.results, Results::Idle)
    }

    /// Runs the full-text search for the current query and filters (after the debounce when
    /// `debounce`); an earlier search that has not answered is dropped. A `repo:` / `path:`
    /// query stays a catalog filter.
    fn search(&mut self, debounce: bool, cx: &mut Context<Self>) {
        self.search_task = None;
        let query = self.query.trim().to_owned();
        if query.is_empty() || has_qualifier(&query) {
            self.results = Results::Idle;
            self.searching = false;
            self.rebuild(cx);
            return;
        }
        if matches!(&self.index, Index::Known(info) if !info.enabled) {
            self.searching = false;
            return;
        }
        self.searching = true;
        let folder = self.scope_folder(cx);
        let mut filters = self.filters.clone();
        filters.scope = if folder.is_some() {
            SettingsScope::Workspace
        } else {
            SettingsScope::All
        };
        let request = filters.request(&query, SEARCH_LIMIT, now_ms(), folder.as_deref());
        let connection = self.env.sessions.read(cx).connection().clone();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            }
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_search(request).await })
                .await;
            this.update(cx, |this, cx| this.finish_search(result, cx))
                .ok();
        }));
    }

    fn finish_search(&mut self, result: Result<HistoryHits, ClientError>, cx: &mut Context<Self>) {
        self.searching = false;
        match result {
            Ok(found) => {
                self.results = Results::Hits(found.hits);
                self.rebuild(cx);
                if let Some(pick) = self.pick.take()
                    && self.items.iter().any(|i| i.item_key() == pick)
                {
                    self.expanded.insert(pick.clone());
                    self.selected = Some(pick);
                    self.rebuild(cx);
                }
                // The index details' counts come from `history.status`; records indexed since
                // it was read (new terminal commands, a finished scan) are in these hits, so
                // read it again. The settle loop reads it on its own while it runs.
                if self.settle_task.is_none() {
                    self.refresh_status(cx);
                }
            }
            Err(error) if is_disabled(&error) => {
                // Indexing was switched off behind our back.
                self.results = Results::Idle;
                self.rebuild(cx);
                self.refresh_status(cx);
            }
            Err(error) => {
                self.results = Results::Failed(error.to_string());
                self.rebuild(cx);
            }
        }
        cx.notify();
    }

    // ---- the list --------------------------------------------------------------------------

    /// Builds the items, groups and flat entries from what the daemon answered, and tells the
    /// list; the scroll position stays unless the query, scope or order changed.
    pub(super) fn rebuild(&mut self, cx: &mut Context<Self>) {
        // What the top of the view shows now, so that the list can be put back there.
        let anchor = (!std::mem::take(&mut self.top_wanted)).then(|| {
            let top = self.list.logical_scroll_top();
            (self.entry_id(top.item_ix), top)
        });
        self.row_cache.borrow_mut().clear();
        let home = self.home.clone();
        match &self.results {
            Results::Hits(hits) => {
                self.items = hits.iter().map(SessionItem::from_hit).collect();
                self.groups = Vec::new();
                self.entries = flat_entries(self.items.len());
            }
            _ => {
                let rows = self.catalog.as_ref().map_or(&[][..], |c| &c.sessions[..]);
                self.items = rows.iter().map(SessionItem::from_row).collect();
                let store = self.env.sessions.read(cx);
                self.groups = group_items(&self.items, self.group_by, home.as_deref(), |id| {
                    store.workspace(id).map(|w| w.name.clone())
                });
                let remaining = self
                    .catalog
                    .as_ref()
                    .map_or(0, |c| c.matched.saturating_sub(c.sessions.len() as u64));
                let remaining = if self.limit >= SESSIONS_MAX_LIMIT {
                    0
                } else {
                    remaining
                };
                self.entries = entries(&self.groups, &self.collapsed, remaining);
            }
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|key| !self.items.iter().any(|i| i.item_key() == *key))
        {
            self.selected = None;
        }
        let top = match anchor {
            None => ListOffset {
                item_ix: 0,
                offset_in_item: px(0.0),
            },
            // The same line when it is still there (rows came or went above it), else the same
            // place counted in lines.
            Some((Some(id), top)) => ListOffset {
                item_ix: (0..self.entries.len())
                    .find(|&ix| self.entry_id(ix).as_ref() == Some(&id))
                    .unwrap_or(top.item_ix),
                offset_in_item: top.offset_in_item,
            },
            Some((None, top)) => top,
        };
        self.list.reset(self.entries.len());
        self.list.scroll_to(top);
        cx.notify();
    }

    /// What identifies list line `ix` across rebuilds: a row's session, a group's key.
    fn entry_id(&self, ix: usize) -> Option<EntryId> {
        match self.entries.get(ix)? {
            Entry::Header { key, .. } => Some(EntryId::Group(key.clone())),
            Entry::Row(item) => self.items.get(*item).map(|i| EntryId::Row(i.item_key())),
            Entry::More { .. } => None,
        }
    }

    /// The session of list line `entry`, when it is a row.
    pub fn entry_session(&self, entry: usize) -> Option<ItemKey> {
        match self.entry_id(entry)? {
            EntryId::Row(key) => Some(key),
            EntryId::Group(_) => None,
        }
    }

    /// Opens or closes the details of the row of `item`.
    pub fn toggle_expanded(&mut self, item: usize, cx: &mut Context<Self>) {
        let Some(key) = self.items.get(item).map(SessionItem::item_key) else {
            return;
        };
        if !self.expanded.remove(&key) {
            self.expanded.insert(key.clone());
        }
        self.selected = Some(key);
        self.rebuild(cx);
    }

    /// Folds or unfolds the group `key`.
    pub fn toggle_group(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_owned());
        }
        self.rebuild(cx);
    }

    /// The scan strip opens its per-source breakdown and the index controls, or closes them.
    pub fn toggle_scan(&mut self, cx: &mut Context<Self>) {
        self.scan_open = !self.scan_open;
        cx.notify();
    }

    /// Moves the selection by `delta` rows (the arrow keys), over the rows that are visible.
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows: Vec<(usize, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(entry, e)| match e {
                Entry::Row(item) => Some((entry, *item)),
                _ => None,
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        let current = self
            .selected()
            .and_then(|item| rows.iter().position(|(_, i)| *i == item));
        let next = match current {
            None if delta >= 0 => 0,
            None => rows.len() - 1,
            Some(at) => at.saturating_add_signed(delta).min(rows.len() - 1),
        };
        let (entry, item) = rows[next];
        self.selected = self.items.get(item).map(SessionItem::item_key);
        self.list.scroll_to_reveal_item(entry);
        cx.notify();
    }

    // ---- menus -----------------------------------------------------------------------------

    pub(super) fn open_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = if self.menu.as_ref() == Some(&menu) {
            None
        } else {
            Some(menu)
        };
        cx.notify();
    }

    /// Opens the group-by dropdown (tests; the header button does the same).
    pub fn open_group_menu(&mut self, cx: &mut Context<Self>) {
        self.open_menu(Menu::GroupBy(Point::default()), cx);
    }

    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    // ---- row actions -----------------------------------------------------------------------

    fn session_exists(&self, item: &SessionItem, cx: &gpui::App) -> bool {
        item.hutan_target()
            .and_then(|target| target.session.parse::<SessionId>().ok())
            .is_some_and(|id| self.env.sessions.read(cx).session(id).is_some())
    }

    /// The entries of the row menu of `item`, and whether the Hutan session it names exists.
    pub(super) fn row_actions(
        &self,
        item: &SessionItem,
        cx: &gpui::App,
    ) -> (Vec<super::history_catalog::ActionEntry>, bool) {
        let exists = self.session_exists(item, cx);
        (actions(item, exists), exists)
    }

    /// What Return does on the selected row: resume it, or show the Hutan session.
    fn primary(&self, item: &SessionItem, cx: &gpui::App) -> Option<RowAction> {
        let exists = self.session_exists(item, cx);
        actions(item, exists)
            .into_iter()
            .next()
            .filter(|e| e.enabled)
            .map(|e| e.action)
    }

    /// Runs a row's button or menu entry: the workbench actions do the work.
    pub fn run_action(
        &mut self,
        action: RowAction,
        item: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.items.get(item).cloned() else {
            return;
        };
        self.menu = None;
        run_row_action(action, &item, window, cx);
        cx.notify();
    }

    // ---- the index -------------------------------------------------------------------------

    /// Reads `history.status`.
    pub fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let store = self.env.sessions.read(cx);
        let connection = store.connection().clone();
        if store.connection_state() != ConnectionState::Connected {
            return;
        }
        if !connection.supports_history() {
            self.index = Index::Unsupported;
            cx.notify();
            return;
        }
        self.status_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_status().await })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(info) => this.status_arrived(info, cx),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn status_arrived(&mut self, info: HistoryStatusInfo, cx: &mut Context<Self>) {
        let before = self.status().map(|s| (s.enabled, index_rows(s)));
        let now = (info.enabled, index_rows(&info));
        self.index = Index::Known(info);
        if before.is_some_and(|(enabled, _)| enabled) && !now.0 {
            // Indexing was switched off elsewhere (the settings, the CLI).
            self.results = Results::Idle;
            self.catalog = None;
            self.selected = None;
            self.rebuild(cx);
        }
        if now.0 && before != Some(now) {
            // Newly enabled, or records were indexed since the last look.
            self.reload(false, cx);
            if before.is_none_or(|(enabled, _)| !enabled) {
                self.search(false, cx);
            }
        }
        // Nothing indexed yet: the first scan is probably still running.
        if now.0 && now.1 == 0 && self.settle_task.is_none() {
            self.settle(cx);
        }
    }

    /// *Set up history…*: the short card shows the sources to tick.
    pub fn open_card(&mut self, cx: &mut Context<Self>) {
        self.card_open = true;
        cx.notify();
    }

    /// *Not now*: back to the short card (the ticks stay).
    pub fn close_card(&mut self, cx: &mut Context<Self>) {
        self.card_open = false;
        cx.notify();
    }

    /// **Enable**: writes the sources the card ticked to `daemon.toml` where they differ (all
    /// four while the daemon's values are not known), turns indexing on
    /// (`history.set_enabled`), then follows the first scan. Nothing happens until a source is
    /// ticked.
    pub fn enable_indexing(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() || self.card_sources.iter().all(|on| !on) {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let wanted = self.card_sources;
        let have = self
            .card_daemon
            .filter(|_| connection.supports_daemon_settings());
        let write_all = have.is_none() && connection.supports_daemon_settings();
        self.busy = Some("Enabling…");
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if have.is_some() || write_all {
                        for (i, source) in SOURCES.iter().enumerate() {
                            if write_all || have.is_some_and(|have| wanted[i] != have[i]) {
                                connection
                                    .daemon_setting_set(
                                        &format!("history.{}", source.as_str()),
                                        Some(Json::Bool(wanted[i])),
                                    )
                                    .await?;
                            }
                        }
                    }
                    connection.history_set_enabled(true).await
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(info) => {
                        this.card_daemon = Some(wanted);
                        this.status_arrived(info, cx);
                        this.settle(cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("Cannot enable indexing: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// **Disable**: `history.set_enabled` off. The daemon stops reading transcripts and keeps
    /// the index file.
    pub fn disable_indexing(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        self.busy = Some("Disabling…");
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_set_enabled(false).await })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(info) => {
                        this.index = Index::Known(info);
                        this.settle_task = None;
                        this.search_task = None;
                        this.catalog_task = None;
                        this.rescan_task = None;
                        this.searching = false;
                        this.loading = false;
                        this.results = Results::Idle;
                        this.catalog = None;
                        this.selected = None;
                        this.scan_open = false;
                        this.card_daemon = None;
                        this.card_open = false;
                        this.rebuild(cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("Cannot disable indexing: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ticks or unticks a source on the off-state card.
    pub fn toggle_card_source(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(on) = self.card_sources.get_mut(index) {
            *on = !*on;
            cx.notify();
        }
    }

    /// Reads the sources `daemon.toml` has, once: Enable writes only what differs from them.
    /// The checkboxes are not set from them: they start unticked whatever the file says.
    pub(super) fn load_card(&mut self, cx: &mut Context<Self>) {
        if self.card_daemon.is_some() || self.card_task.is_some() {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        if !connection.supports_daemon_settings() {
            return;
        }
        self.card_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.daemon_settings().await })
                .await;
            this.update(cx, |this, cx| {
                this.card_task = None;
                if let Ok(settings) = result {
                    let mut have = [true; 4];
                    for (i, source) in SOURCES.iter().enumerate() {
                        let key = format!("history.{}", source.as_str());
                        if let Some(entry) = settings.entries.iter().find(|e| e.key == key) {
                            have[i] = entry.value.as_bool().unwrap_or(true);
                        }
                    }
                    this.card_daemon = Some(have);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Asks before running `what`.
    pub fn ask(&mut self, what: Confirm, cx: &mut Context<Self>) {
        self.confirm = Some(what);
        cx.notify();
    }

    pub fn cancel_confirmation(&mut self, cx: &mut Context<Self>) {
        self.confirm = None;
        cx.notify();
    }

    /// Runs the confirmed action.
    pub fn confirmed(&mut self, cx: &mut Context<Self>) {
        match self.confirm.take() {
            Some(Confirm::Clear) => self.clear(cx),
            Some(Confirm::Rebuild) => self.rebuild_index(cx),
            None => {}
        }
        cx.notify();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        self.busy = Some("Clearing…");
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_clear().await })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(()) => {
                        this.results = Results::Idle;
                        this.catalog = None;
                        this.selected = None;
                        this.rebuild(cx);
                        this.refresh_status(cx);
                        this.reload(false, cx);
                        this.settle(cx);
                        this.search(false, cx);
                    }
                    Err(error) => this.error = Some(format!("Cannot clear the index: {error}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn rebuild_index(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        self.busy = Some("Rebuilding…");
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_rebuild().await })
                .await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(()) => {
                        this.reload(false, cx);
                        this.settle(cx);
                    }
                    Err(error) => {
                        this.error = Some(format!("Cannot rebuild the index: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Follows the index while it is being (re)built: reads the status every
    /// [`SETTLE_POLL`], asks again when the rows changed, and stops once they stay put.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let connection = self.env.sessions.read(cx).connection().clone();
        self.settle_task = Some(cx.spawn(async move |this, cx| {
            let mut rows_before: Option<u64> = None;
            let mut quiet = Duration::ZERO;
            let mut elapsed = Duration::ZERO;
            while elapsed < SETTLE_MAX {
                let asked = connection.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move { asked.history_status().await })
                    .await;
                let Ok(info) = result else { break };
                let rows = index_rows(&info);
                let changed = rows_before != Some(rows);
                if changed {
                    quiet = Duration::ZERO;
                }
                let enabled = info.enabled;
                let alive = this.update(cx, |this, cx| {
                    this.index = Index::Known(info);
                    if changed && rows_before.is_some() {
                        this.reload(false, cx);
                        if !matches!(this.results, Results::Idle) {
                            this.search(false, cx);
                        }
                    }
                    cx.notify();
                });
                let limit = if rows == 0 {
                    SETTLE_QUIET_EMPTY
                } else {
                    SETTLE_QUIET
                };
                if alive.is_err() || !enabled || quiet >= limit {
                    break;
                }
                rows_before = Some(rows);
                cx.background_executor().timer(SETTLE_POLL).await;
                quiet += SETTLE_POLL;
                elapsed += SETTLE_POLL;
            }
            this.update(cx, |this, _| this.settle_task = None).ok();
        }));
    }

    // ---- the search field ------------------------------------------------------------------

    /// How the search box's element reaches its text: the platform's text input and the mouse.
    pub(super) fn search_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.search_focus.clone(),
            |this, cx, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                let mut query = this.query.clone();
                this.query_edit.with(&mut query, vim_on, false, f);
                this.set_query(&query, cx);
            },
        )
    }

    pub(super) fn on_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vim = VimConfig::of(&self.env.settings);
        // Vim's commands and the editing keys; typed text arrives through the box's platform
        // text input (`search_field`).
        let mut query = self.query.clone();
        if self.query_edit.key(&mut query, event, vim, true, cx) == Outcome::Handled {
            self.set_query(&query, cx);
            cx.stop_propagation();
            return;
        }
        // Enter, ↑ ↓ and Escape (in Normal mode, under Vim mode) are the panel's own.
        let ks = &event.keystroke;
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
            return;
        }
        match ks.key.as_str() {
            "escape" => {
                if self.menu.is_some() {
                    self.close_menu(cx);
                } else {
                    self.set_query("", cx);
                }
            }
            "down" => self.move_selection(1, cx),
            "up" => self.move_selection(-1, cx),
            "enter" => {
                if self.search_pending() {
                    // Return does not wait for the debounce.
                    self.search(false, cx);
                    cx.notify();
                } else if let Some(item) = self.selected().and_then(|i| self.items.get(i)).cloned()
                    && let Some(action) = self.primary(&item, cx)
                {
                    run_row_action(action, &item, window, cx);
                } else {
                    cx.notify();
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

/// Runs `action` for `item` by dispatching the workbench action that does it.
fn run_row_action(action: RowAction, item: &SessionItem, window: &mut Window, cx: &mut gpui::App) {
    let dispatch = |window: &mut Window, cx: &mut gpui::App, a: Box<dyn gpui::Action>| {
        window.dispatch_action(a, cx);
    };
    match action {
        RowAction::Resume => {
            if let Some(resume) = item.resume() {
                dispatch(window, cx, Box::new(ResumeHistoryHit(resume)));
            }
        }
        RowAction::Reopen => {
            if let Some(id) = item.session_id {
                dispatch(window, cx, Box::new(ReopenSessionById(id.to_string())));
            }
        }
        RowAction::OpenSession => {
            if let Some(target) = item.hutan_target() {
                let target: HutanTarget = target;
                dispatch(window, cx, Box::new(OpenHistoryHit(target)));
            }
        }
        RowAction::CopyResumeCommand => {
            if let Some(resume) = item.resume() {
                dispatch(
                    window,
                    cx,
                    Box::new(CopyHistoryCommand(shell_join(&resume.argv))),
                );
            }
        }
        RowAction::OpenLog => {
            if let Some(path) = item.transcript() {
                let path = path.to_string_lossy().into_owned();
                dispatch(window, cx, Box::new(OpenHistoryFile(path)));
            }
        }
        RowAction::RevealLog => {
            if let Some(path) = item.transcript() {
                let path = path.to_string_lossy().into_owned();
                dispatch(window, cx, Box::new(RevealHistoryFile(path)));
            }
        }
        RowAction::OpenDirectory => {
            if let Some(cwd) = item.cwd.clone().filter(|c| !c.is_empty()) {
                dispatch(window, cx, Box::new(OpenHistoryDirectory(cwd)));
            }
        }
        RowAction::CopySessionId => {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(session_id_text(item)));
            dispatch(
                window,
                cx,
                Box::new(Notify("Copied the session id".to_owned())),
            );
        }
        RowAction::CopyLogPath => {
            if let Some(path) = item.transcript() {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                    path.to_string_lossy().into_owned(),
                ));
                dispatch(
                    window,
                    cx,
                    Box::new(Notify("Copied the log path".to_owned())),
                );
            }
        }
    }
}

impl Focusable for HistoryPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.search_focus.clone()
    }
}

/// A label that cannot be taken apart: shared between the header and the strips.
pub(super) fn shared(text: impl Into<String>) -> SharedString {
    SharedString::from(text.into())
}
