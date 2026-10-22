//! The `search` panel (E-13b): find, and replace, across the active project's files through
//! the daemon's `fs.search` (the same search `hutan fs search` runs).
//!
//! - **Query.** A query field with three toggles — *Match case* (`Aa`; off is smart case),
//!   *Whole word* (`ab`) and *Regular expression* (`.*`), kept in `panels.search` — and two glob
//!   fields: files to include and to exclude (comma-separated, relative to the project). Every
//!   change cancels the search that runs (`fs.search.cancel`) and starts a new one after
//!   `panels.search.debounce_ms`; Return starts it at once.
//! - **Results.** While the daemon searches, the status line counts the files looked at and the
//!   hits found (`fs.search.progress`); the answer lists one row per match, grouped under its
//!   file with the file's match count. The match is drawn semibold and underlined on a soft
//!   fill (never colour alone). The list is virtualized. A search that hit
//!   `panels.search.max_results` says "stopped at N results".
//! - **Keyboard.** ↑↓ move between matches from the fields and the list, Return opens the
//!   selected match in the focused pane (the caret on it); in the list `n` / `N` do the same as
//!   ↓ / ↑ under Vim mode, Space ticks a match while replacing, Escape goes back to the query;
//!   Tab cycles the fields and the list. A click opens the match and leaves the keyboard here.
//! - **Replace.** ⌘⇧H (or the header's ⇄) shows the replace field; every match and every file
//!   has a checkbox, and each match row previews the replacement. *Replace All* first asks
//!   "Replace 42 matches in 7 files?"; the workbench then applies one transaction per file
//!   (`search_replace.rs`) and the panel lists what it skipped.
//! - **The version the search saw.** When the answer comes the panel asks `fs.stat` of every
//!   file listed: that version is what a clean file must still be when it is replaced.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use crate::workbench::chat::vim::Outcome;
use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, Focusable, HighlightStyle, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Render, ScrollStrategy,
    SharedString, StatefulInteractiveElement, Styled, StyledText, Task, UnderlineStyle,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use hutan_client::{ConnectionState, Notice};
use hutan_protocol::fs::{FileVersion, FsSearch, FsSearchResult, SearchHit};
use tokio::sync::broadcast::error::RecvError;

use super::common::panel_header_with;
use crate::icons::{Icon, icon};
use crate::settings::SearchPanelSettings;
use crate::theme::{DensityExt as _, Theme, Tokens};
use crate::workbench::Region;
use crate::workbench::actions::{ChosenFile, OpenChosenFile, ReplaceInFiles};
use crate::workbench::files::FilePlace;
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::search_replace::{
    ReplaceFile, ReplaceLine, ReplaceOutcome, ReplaceRequest, SearchSpec, SkipReason, Skipped,
    plural, replacement_at,
};
use crate::workbench::style::UiStyle;
use crate::workbench::text_field::FieldRef;
use crate::workbench::ui::{self, FocusRingExt as _, StyledExt as _};
use crate::workbench::vim_ui::{self, FieldEdit, VimConfig};

/// Height of every row of the list (file headers and matches alike: the list is uniform).
const ROW_HEIGHT: f32 = 22.0;
/// A match further into its line than this many characters is shown with the line's start cut.
const PREVIEW_LEAD: usize = 24;
/// Skipped files the outcome lists under its summary.
const SKIPPED_SHOWN: usize = 8;

/// The fields of the panel, in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchField {
    Query,
    Replace,
    Include,
    Exclude,
}

/// What the search is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchStatus {
    /// Nothing to search for.
    Idle,
    /// No project is active (Home).
    NoProject,
    /// Not connected, or the daemon has no file service.
    Unavailable,
    /// Waiting for the typing to pause.
    Waiting,
    /// The daemon searches.
    Running {
        files_searched: u64,
        hits: u32,
    },
    /// The answer is listed.
    Done {
        files_searched: u64,
        truncated: bool,
    },
    Failed(String),
}

/// The matching lines of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultFile {
    /// Relative to the project.
    pub path: String,
    pub hits: Vec<SearchHit>,
    /// Why the last *Replace All* left it alone.
    pub skipped: Option<String>,
}

impl ResultFile {
    /// Match rows: one per matched range, one for a line whose matches lie past the cut.
    pub fn match_count(&self) -> usize {
        self.hits.iter().map(|h| h.matched.len().max(1)).sum()
    }
}

/// A row of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRow {
    File {
        file: usize,
    },
    /// `range`: the index into the hit's `matched`; `None` for a line whose matches lie past
    /// the shown text (listed, not replaceable).
    Match {
        file: usize,
        hit: usize,
        range: Option<usize>,
    },
}

/// A search on its way: its id (for progress and cancel) and what it asked for.
#[derive(Debug, Clone)]
struct Running {
    id: String,
    root: PathBuf,
    spec: SearchSpec,
}

/// What the replace preview asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplaceCount {
    pub matches: usize,
    pub files: usize,
}

impl ReplaceCount {
    pub fn question(self) -> String {
        format!(
            "Replace {} in {}?",
            plural(self.matches, "match", "matches"),
            plural(self.files, "file", "files")
        )
    }
}

/// The Search panel.
pub struct SearchPanel {
    env: PanelEnv,
    query_focus: FocusHandle,
    replace_focus: FocusHandle,
    include_focus: FocusHandle,
    exclude_focus: FocusHandle,
    list_focus: FocusHandle,
    query: String,
    query_edit: FieldEdit,
    replacement: String,
    replace_edit: FieldEdit,
    include: String,
    include_edit: FieldEdit,
    exclude: String,
    exclude_edit: FieldEdit,
    show_replace: bool,
    /// The project searched (the active one).
    root: Option<PathBuf>,
    /// The options the list was searched with (`panels.search`): a change searches again.
    options: Option<(bool, bool, bool, u32)>,
    status: SearchStatus,
    /// What the listed answer was searched for.
    searched: Option<Running>,
    files: Vec<ResultFile>,
    rows: Vec<SearchRow>,
    /// Matches unticked for *Replace All*: `(file, hit, range)`.
    excluded: HashSet<(usize, usize, usize)>,
    /// The selected row (always a match row).
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    /// Bumped by every change of the query: answers of an older search are dropped.
    generation: u64,
    running: Option<Running>,
    task: Option<Task<()>>,
    listener: Option<Task<()>>,
    /// `fs.stat` of every listed file, by relative path: its canonical path and version.
    versions: HashMap<String, Result<(String, FileVersion), String>>,
    versions_task: Option<Task<()>>,
    /// *Replace All* asks before it runs.
    confirm: Option<ReplaceCount>,
    /// The *Replace All* sent to the workbench, and the files the panel skipped itself.
    replacing: Option<(u64, Vec<Skipped>)>,
    last_replace: Option<ReplaceOutcome>,
    next_id: u64,
    // ---- counters for tests ----
    searches: u64,
    answers: u64,
    renders: u64,
    progress: u64,
    rows_rendered: Rc<Cell<usize>>,
}

impl std::fmt::Debug for SearchPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchPanel")
            .field("query", &self.query)
            .field("status", &self.status)
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

impl SearchPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |this, _, cx| this.follow_root(cx))
            .detach();
        let mut panel = Self {
            env,
            query_focus: cx.focus_handle(),
            replace_focus: cx.focus_handle(),
            include_focus: cx.focus_handle(),
            exclude_focus: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            query: String::new(),
            query_edit: FieldEdit::new(),
            replacement: String::new(),
            replace_edit: FieldEdit::new(),
            include: String::new(),
            include_edit: FieldEdit::new(),
            exclude: String::new(),
            exclude_edit: FieldEdit::new(),
            show_replace: false,
            root: None,
            options: None,
            status: SearchStatus::Idle,
            searched: None,
            files: Vec::new(),
            rows: Vec::new(),
            excluded: HashSet::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            generation: 0,
            running: None,
            task: None,
            listener: None,
            versions: HashMap::new(),
            versions_task: None,
            confirm: None,
            replacing: None,
            last_replace: None,
            next_id: 0,
            searches: 0,
            answers: 0,
            renders: 0,
            progress: 0,
            rows_rendered: Rc::new(Cell::new(0)),
        };
        panel.root = panel.env.sessions.read(cx).files_root();
        panel.options = Some(panel.option_values());
        panel
    }

    // ---- read access (also for tests) -----------------------------------------------------

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn replacement(&self) -> &str {
        &self.replacement
    }

    pub fn status(&self) -> &SearchStatus {
        &self.status
    }

    /// A search waits for the typing to pause or runs.
    pub fn searching(&self) -> bool {
        matches!(
            self.status,
            SearchStatus::Waiting | SearchStatus::Running { .. }
        )
    }

    pub fn files(&self) -> &[ResultFile] {
        &self.files
    }

    pub fn rows(&self) -> &[SearchRow] {
        &self.rows
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn replace_shown(&self) -> bool {
        self.show_replace
    }

    /// `fs.search` requests sent so far.
    pub fn searches(&self) -> u64 {
        self.searches
    }

    /// `fs.search` answers listed so far.
    pub fn answers(&self) -> u64 {
        self.answers
    }

    /// Frames the panel drew.
    pub fn renders(&self) -> u64 {
        self.renders
    }

    /// `fs.search.progress` reports taken for the running search.
    pub fn progress_reports(&self) -> u64 {
        self.progress
    }

    /// Rows built for frames so far (only the rows in view are).
    pub fn rows_rendered(&self) -> usize {
        self.rows_rendered.get()
    }

    /// Every listed file's version is known (or failed to be).
    pub fn versions_ready(&self) -> bool {
        self.versions_task.is_none()
            && self
                .files
                .iter()
                .all(|f| self.versions.contains_key(&f.path))
    }

    /// The question *Replace All* asks, while it is shown.
    pub fn confirm(&self) -> Option<ReplaceCount> {
        self.confirm
    }

    /// A *Replace All* is under way.
    pub fn replacing(&self) -> bool {
        self.replacing.is_some()
    }

    pub fn last_replace(&self) -> Option<&ReplaceOutcome> {
        self.last_replace.as_ref()
    }

    fn settings(&self) -> SearchPanelSettings {
        *self.env.settings.get::<SearchPanelSettings>()
    }

    fn option_values(&self) -> (bool, bool, bool, u32) {
        let s = self.settings();
        (s.case_sensitive, s.whole_word, s.regex, s.max_results)
    }

    /// What the query and the toggles ask for.
    pub fn spec(&self) -> SearchSpec {
        let s = self.settings();
        SearchSpec {
            query: self.query.clone(),
            case_sensitive: s.case_sensitive,
            whole_word: s.whole_word,
            regex: s.regex,
        }
    }

    /// The include and exclude fields as `fs.search` globs (`!` excludes).
    pub fn globs(&self) -> Vec<String> {
        let split = |text: &str| -> Vec<String> {
            text.split(',')
                .map(str::trim)
                .filter(|g| !g.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let mut globs = split(&self.include);
        globs.extend(split(&self.exclude).into_iter().map(|g| format!("!{g}")));
        globs
    }

    // ---- editing the fields ---------------------------------------------------------------

    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.query != query {
            self.query = query.to_owned();
            self.schedule(true, cx);
        }
    }

    pub fn set_replacement(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.replacement != text {
            self.replacement = text.to_owned();
            self.confirm = None;
            cx.notify();
        }
    }

    pub fn set_include(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.include != text {
            self.include = text.to_owned();
            self.schedule(true, cx);
        }
    }

    pub fn set_exclude(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.exclude != text {
            self.exclude = text.to_owned();
            self.schedule(true, cx);
        }
    }

    fn text_of(&self, field: SearchField) -> &str {
        match field {
            SearchField::Query => &self.query,
            SearchField::Replace => &self.replacement,
            SearchField::Include => &self.include,
            SearchField::Exclude => &self.exclude,
        }
    }

    fn set_text(&mut self, field: SearchField, text: &str, cx: &mut Context<Self>) {
        match field {
            SearchField::Query => self.set_query(text, cx),
            SearchField::Replace => self.set_replacement(text, cx),
            SearchField::Include => self.set_include(text, cx),
            SearchField::Exclude => self.set_exclude(text, cx),
        }
    }

    fn edit_of(&mut self, field: SearchField) -> &mut FieldEdit {
        match field {
            SearchField::Query => &mut self.query_edit,
            SearchField::Replace => &mut self.replace_edit,
            SearchField::Include => &mut self.include_edit,
            SearchField::Exclude => &mut self.exclude_edit,
        }
    }

    fn focus_of(&self, field: SearchField) -> &FocusHandle {
        match field {
            SearchField::Query => &self.query_focus,
            SearchField::Replace => &self.replace_focus,
            SearchField::Include => &self.include_focus,
            SearchField::Exclude => &self.exclude_focus,
        }
    }

    /// Flips one of `panels.search`'s toggles (`case_sensitive`, `whole_word`, `regex`) and
    /// searches again.
    pub fn toggle_option(&mut self, key: &'static str, cx: &mut Context<Self>) {
        let s = self.settings();
        let now = match key {
            "case_sensitive" => s.case_sensitive,
            "whole_word" => s.whole_word,
            _ => s.regex,
        };
        if let Err(error) = self
            .env
            .settings
            .write_user(&format!("panels.search.{key}"), toml::Value::Boolean(!now))
        {
            tracing::warn!("cannot save panels.search.{key}: {error}");
        }
        self.follow_options(cx);
    }

    /// Shows or hides the replace field.
    pub fn set_replace_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.show_replace = shown;
        self.confirm = None;
        cx.notify();
    }

    /// ⌘⇧F / ⌘⇧H: the keyboard in the query with its text selected (`seed`, a selection of the
    /// editor, replaces it), the replace field shown with `replace`.
    pub fn reveal(
        &mut self,
        seed: Option<String>,
        replace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if replace {
            self.show_replace = true;
        }
        if let Some(seed) = seed {
            self.set_query(&seed, cx);
        }
        self.query_edit.select_all(&self.query);
        window.focus(&self.query_focus, cx);
        cx.notify();
    }

    // ---- searching ------------------------------------------------------------------------

    /// Follows the project the window works in: another project is searched afresh.
    fn follow_root(&mut self, cx: &mut Context<Self>) {
        let root = self.env.sessions.read(cx).files_root();
        if root == self.root {
            return;
        }
        self.root = root;
        self.schedule(false, cx);
    }

    /// Searches again when a toggle or the result bound changed (here or in Settings).
    fn follow_options(&mut self, cx: &mut Context<Self>) {
        let options = self.option_values();
        if self.options == Some(options) {
            return;
        }
        self.options = Some(options);
        self.schedule(false, cx);
    }

    /// Cancels the search that runs and drops the listed answer's pending reads.
    fn cancel_running(&mut self, cx: &mut Context<Self>) {
        self.task = None;
        if let Some(running) = self.running.take() {
            let connection = self.env.sessions.read(cx).connection().clone();
            cx.background_spawn(async move {
                if let Err(error) = connection.fs_search_cancel(&running.id).await {
                    tracing::debug!(%error, "fs.search.cancel failed");
                }
            })
            .detach();
        }
    }

    /// The query changed: the search that runs stops, a new one starts after the debounce
    /// (`debounce`) or at once.
    fn schedule(&mut self, debounce: bool, cx: &mut Context<Self>) {
        self.cancel_running(cx);
        self.generation += 1;
        self.confirm = None;
        let generation = self.generation;
        if self.query.is_empty() {
            self.clear_results();
            self.status = SearchStatus::Idle;
            cx.notify();
            return;
        }
        if self.root.is_none() {
            self.clear_results();
            self.status = SearchStatus::NoProject;
            cx.notify();
            return;
        }
        let wait = if debounce {
            Duration::from_millis(self.settings().debounce_ms)
        } else {
            Duration::ZERO
        };
        self.status = SearchStatus::Waiting;
        self.task = Some(cx.spawn(async move |this, cx| {
            if !wait.is_zero() {
                cx.background_executor().timer(wait).await;
            }
            let Ok(Some((connection, request))) =
                this.update(cx, |this, cx| this.begin(generation, cx))
            else {
                return;
            };
            let answer = cx
                .background_spawn(async move { connection.fs_search(request).await })
                .await;
            this.update(cx, |this, cx| this.answered(generation, answer, cx))
                .ok();
        }));
        cx.notify();
    }

    /// Return in a field: a search waiting for the debounce starts now.
    fn search_now(&mut self, cx: &mut Context<Self>) {
        if self.status == SearchStatus::Waiting {
            self.schedule(false, cx);
        }
    }

    /// The request of the search `generation`, now that the typing paused.
    fn begin(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Option<(hutan_client::Connection, FsSearch)> {
        if generation != self.generation {
            return None;
        }
        let root = self.root.clone()?;
        let (state, supported, connection) = {
            let store = self.env.sessions.read(cx);
            (
                store.connection_state(),
                store.connection().supports_fs(),
                store.connection().clone(),
            )
        };
        if state != ConnectionState::Connected || !supported {
            self.status = SearchStatus::Unavailable;
            cx.notify();
            return None;
        }
        self.listen(cx);
        let spec = self.spec();
        let pattern = spec.pattern();
        self.next_id += 1;
        let id = format!("search-{}-{}", cx.entity_id(), self.next_id);
        let request = FsSearch {
            search_id: id.clone(),
            root: root.clone(),
            pattern: pattern.pattern,
            regex: pattern.regex,
            case_sensitive: pattern.case_sensitive,
            globs: self.globs(),
            limit: Some(
                self.settings()
                    .max_results
                    .clamp(1, hutan_protocol::fs::SEARCH_MAX),
            ),
        };
        self.running = Some(Running { id, root, spec });
        self.status = SearchStatus::Running {
            files_searched: 0,
            hits: 0,
        };
        self.searches += 1;
        cx.notify();
        Some((connection, request))
    }

    /// Takes the connection's progress reports of this panel's searches.
    fn listen(&mut self, cx: &mut Context<Self>) {
        if self.listener.is_some() {
            return;
        }
        let mut notices = self.env.sessions.read(cx).connection().notices();
        self.listener = Some(cx.spawn(async move |this, cx| {
            loop {
                let (id, files, hits) = match notices.recv().await {
                    Ok(Notice::FsSearchProgress {
                        search_id,
                        files_searched,
                        hits,
                    }) => (search_id, files_searched, hits),
                    Ok(_) | Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                };
                let alive = this.update(cx, |this, cx| {
                    if this.running.as_ref().is_some_and(|r| r.id == id)
                        && matches!(this.status, SearchStatus::Running { .. })
                    {
                        this.status = SearchStatus::Running {
                            files_searched: files,
                            hits,
                        };
                        this.progress += 1;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    fn clear_results(&mut self) {
        self.files.clear();
        self.rows.clear();
        self.excluded.clear();
        self.selected = None;
        self.versions.clear();
        self.versions_task = None;
        self.searched = None;
    }

    fn answered(
        &mut self,
        generation: u64,
        answer: Result<FsSearchResult, hutan_client::ClientError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        let running = self.running.take();
        self.task = None;
        self.answers += 1;
        match answer {
            Ok(result) => {
                self.clear_results();
                self.files = group(result.hits);
                self.rebuild_rows();
                self.searched = running;
                self.status = SearchStatus::Done {
                    files_searched: result.files_searched,
                    truncated: result.truncated,
                };
                self.selected = self.first_match_row();
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                self.read_versions(cx);
            }
            Err(error) => {
                self.clear_results();
                self.status = SearchStatus::Failed(crate::workbench::files::error_text(&error));
            }
        }
        cx.notify();
    }

    /// Asks `fs.stat` of every listed file: the version the search saw.
    fn read_versions(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.searched.as_ref().map(|s| s.root.clone()) else {
            return;
        };
        let paths: Vec<String> = self.files.iter().map(|f| f.path.clone()).collect();
        if paths.is_empty() {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let generation = self.generation;
        let read = cx.background_spawn(async move {
            let mut found = Vec::with_capacity(paths.len());
            for path in paths {
                let entry = connection.fs_stat(&root.join(&path), false).await;
                let version = entry
                    .map(|e| {
                        (
                            e.path.display().to_string(),
                            FileVersion {
                                mtime_ns: e.mtime_ns,
                                size: e.size,
                                ino: e.ino,
                            },
                        )
                    })
                    .map_err(|e| crate::workbench::files::error_text(&e));
                found.push((path, version));
            }
            found
        });
        self.versions_task = Some(cx.spawn(async move |this, cx| {
            let found = read.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.versions.extend(found);
                this.versions_task = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (f, file) in self.files.iter().enumerate() {
            self.rows.push(SearchRow::File { file: f });
            for (h, hit) in file.hits.iter().enumerate() {
                if hit.matched.is_empty() {
                    self.rows.push(SearchRow::Match {
                        file: f,
                        hit: h,
                        range: None,
                    });
                } else {
                    for r in 0..hit.matched.len() {
                        self.rows.push(SearchRow::Match {
                            file: f,
                            hit: h,
                            range: Some(r),
                        });
                    }
                }
            }
        }
    }

    fn first_match_row(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(r, SearchRow::Match { .. }))
    }

    // ---- moving and opening -----------------------------------------------------------------

    /// Moves the selection `delta` matches down (up when negative), wrapping around.
    pub fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let matches: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, SearchRow::Match { .. }))
            .map(|(i, _)| i)
            .collect();
        if matches.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|s| matches.iter().position(|&m| m == s));
        let next = match at {
            None if delta >= 0 => 0,
            None => matches.len() - 1,
            Some(at) => {
                let len = matches.len() as isize;
                (at as isize + delta).rem_euclid(len) as usize
            }
        };
        self.select(matches[next], cx);
    }

    fn select(&mut self, row: usize, cx: &mut Context<Self>) {
        self.selected = Some(row);
        self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// The file and position of row `row`: `(absolute path, line, column)`, 1-based, the
    /// column in characters.
    fn target_of(&self, row: usize) -> Option<(String, u32, u32)> {
        let root = self.searched.as_ref()?.root.clone();
        let (file, hit, range) = match *self.rows.get(row)? {
            SearchRow::File { file } => (file, 0, Some(0)),
            SearchRow::Match { file, hit, range } => (file, hit, range),
        };
        let file = self.files.get(file)?;
        let hit = file.hits.get(hit)?;
        let start = range
            .and_then(|r| hit.matched.get(r))
            .map_or(0, |m| m[0] as usize);
        let column = hit.text.get(..start).map_or(0, |s| s.chars().count()) as u32 + 1;
        Some((
            root.join(&file.path).display().to_string(),
            hit.line,
            column,
        ))
    }

    /// Opens row `row` at its match: in the focused pane with the keyboard (`focus`), or
    /// leaving the keyboard here.
    pub fn open_row(
        &mut self,
        row: usize,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((path, line, column)) = self.target_of(row) else {
            return;
        };
        if matches!(self.rows.get(row), Some(SearchRow::Match { .. })) {
            self.selected = Some(row);
        }
        window.dispatch_action(
            Box::new(OpenChosenFile(ChosenFile {
                path,
                line: Some(line),
                column: Some(column),
                place: if focus {
                    FilePlace::Focused
                } else {
                    FilePlace::Preview
                },
            })),
            cx,
        );
        cx.notify();
    }

    /// Puts the keyboard in the list, on a match.
    pub fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            self.selected = self.first_match_row();
        }
        window.focus(&self.list_focus, cx);
        cx.notify();
    }

    // ---- ticking matches --------------------------------------------------------------------

    fn match_keys(&self, file: usize) -> Vec<(usize, usize, usize)> {
        self.files.get(file).map_or_else(Vec::new, |f| {
            f.hits
                .iter()
                .enumerate()
                .flat_map(|(h, hit)| (0..hit.matched.len()).map(move |r| (file, h, r)))
                .collect()
        })
    }

    /// Ticks or unticks row `row`: a match, or every match of a file.
    pub fn toggle_row(&mut self, row: usize, cx: &mut Context<Self>) {
        match self.rows.get(row).copied() {
            Some(SearchRow::Match {
                file,
                hit,
                range: Some(range),
            }) => {
                let key = (file, hit, range);
                if !self.excluded.remove(&key) {
                    self.excluded.insert(key);
                }
            }
            Some(SearchRow::File { file }) => {
                let keys = self.match_keys(file);
                let all_in = keys.iter().all(|k| !self.excluded.contains(k));
                for key in keys {
                    if all_in {
                        self.excluded.insert(key);
                    } else {
                        self.excluded.remove(&key);
                    }
                }
            }
            _ => return,
        }
        self.confirm = None;
        cx.notify();
    }

    /// How much of file `file` is ticked: `Some(true)` all, `Some(false)` none, `None` some.
    fn file_ticked(&self, file: usize) -> Option<bool> {
        let keys = self.match_keys(file);
        let out = keys.iter().filter(|k| self.excluded.contains(k)).count();
        if out == 0 {
            Some(true)
        } else if out == keys.len() {
            Some(false)
        } else {
            None
        }
    }

    /// What *Replace All* would change now.
    pub fn replace_count(&self) -> ReplaceCount {
        let mut count = ReplaceCount {
            matches: 0,
            files: 0,
        };
        for f in 0..self.files.len() {
            let n = self
                .match_keys(f)
                .iter()
                .filter(|k| !self.excluded.contains(k))
                .count();
            if n > 0 {
                count.matches += n;
                count.files += 1;
            }
        }
        count
    }

    /// *Replace All*: asks first ("Replace 42 matches in 7 files?").
    pub fn ask_replace_all(&mut self, cx: &mut Context<Self>) {
        if !self.can_replace() {
            return;
        }
        let count = self.replace_count();
        if count.matches > 0 {
            self.confirm = Some(count);
            cx.notify();
        }
    }

    fn can_replace(&self) -> bool {
        self.show_replace
            && self.replacing.is_none()
            && matches!(self.status, SearchStatus::Done { .. })
            && self.versions_ready()
    }

    pub fn cancel_replace(&mut self, cx: &mut Context<Self>) {
        self.confirm = None;
        cx.notify();
    }

    /// The question was answered yes: the workbench replaces, one transaction per file.
    pub fn confirm_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.take().is_none() || !self.can_replace() {
            cx.notify();
            return;
        }
        let Some(searched) = self.searched.clone() else {
            return;
        };
        self.next_id += 1;
        let id = self.next_id;
        let mut files = Vec::new();
        let mut skipped = Vec::new();
        for (f, file) in self.files.iter().enumerate() {
            let mut lines = Vec::new();
            for (h, hit) in file.hits.iter().enumerate() {
                let starts: Vec<u32> = hit
                    .matched
                    .iter()
                    .enumerate()
                    .filter(|(r, _)| !self.excluded.contains(&(f, h, *r)))
                    .map(|(_, m)| m[0])
                    .collect();
                if !starts.is_empty() {
                    lines.push(ReplaceLine {
                        line: hit.line,
                        text: hit.text.clone(),
                        starts,
                    });
                }
            }
            if lines.is_empty() {
                continue;
            }
            let absolute = searched.root.join(&file.path).display().to_string();
            match self.versions.get(&file.path) {
                Some(Ok((path, seen))) => files.push(ReplaceFile {
                    path: path.clone(),
                    relative: file.path.clone(),
                    seen: *seen,
                    lines,
                }),
                Some(Err(message)) => skipped.push(Skipped {
                    path: absolute,
                    relative: file.path.clone(),
                    reason: SkipReason::Unreadable(message.clone()),
                }),
                None => skipped.push(Skipped {
                    path: absolute,
                    relative: file.path.clone(),
                    reason: SkipReason::Changed,
                }),
            }
        }
        self.replacing = Some((id, skipped));
        window.dispatch_action(
            Box::new(ReplaceInFiles(ReplaceRequest {
                id,
                spec: searched.spec,
                replacement: self.replacement.clone(),
                files,
            })),
            cx,
        );
        cx.notify();
    }

    /// The workbench finished a *Replace All*: the replaced matches leave the list, a skipped
    /// file says why on its row, and the outcome stays under the fields.
    pub fn replace_finished(&mut self, mut outcome: ReplaceOutcome, cx: &mut Context<Self>) {
        let Some((id, skipped)) = self.replacing.take() else {
            return;
        };
        if id != outcome.id {
            self.replacing = Some((id, skipped));
            return;
        }
        outcome.skipped.extend(skipped);
        let reasons: BTreeMap<&str, String> = outcome
            .skipped
            .iter()
            .map(|s| (s.relative.as_str(), s.reason.text()))
            .collect();
        let files = std::mem::take(&mut self.files);
        let excluded = std::mem::take(&mut self.excluded);
        let mut kept = Vec::new();
        for (f, mut file) in files.into_iter().enumerate() {
            if let Some(reason) = reasons.get(file.path.as_str()) {
                file.skipped = Some(reason.clone());
                kept.push((f, file, None));
                continue;
            }
            // Only the unticked matches are left.
            let mut hits = Vec::new();
            for (h, mut hit) in std::mem::take(&mut file.hits).into_iter().enumerate() {
                let left: Vec<[u32; 2]> = hit
                    .matched
                    .iter()
                    .enumerate()
                    .filter(|(r, _)| excluded.contains(&(f, h, *r)))
                    .map(|(_, m)| *m)
                    .collect();
                if !left.is_empty() || hit.matched.is_empty() {
                    hit.matched = left;
                    hits.push(hit);
                }
            }
            if !hits.is_empty() {
                file.hits = hits;
                kept.push((f, file, Some(())));
            }
        }
        // Skipped files keep their unticked matches unticked.
        let mut remap = HashMap::new();
        self.files = kept
            .into_iter()
            .enumerate()
            .map(|(new, (old, file, replaced))| {
                if replaced.is_none() {
                    remap.insert(old, new);
                }
                file
            })
            .collect();
        self.excluded = excluded
            .into_iter()
            .filter_map(|(f, h, r)| remap.get(&f).map(|&n| (n, h, r)))
            .collect();
        self.rebuild_rows();
        self.selected = self.first_match_row();
        self.last_replace = Some(outcome);
        cx.notify();
    }

    // ---- the keyboard -----------------------------------------------------------------------

    fn next_field(&self, from: Option<SearchField>, back: bool) -> Option<SearchField> {
        let mut order = vec![SearchField::Query];
        if self.show_replace {
            order.push(SearchField::Replace);
        }
        order.extend([SearchField::Include, SearchField::Exclude]);
        // `None` is the list, between the last field and the first.
        let mut ring: Vec<Option<SearchField>> = order.into_iter().map(Some).collect();
        ring.push(None);
        let at = ring.iter().position(|f| *f == from).unwrap_or(0);
        let len = ring.len();
        let next = if back {
            (at + len - 1) % len
        } else {
            (at + 1) % len
        };
        ring[next]
    }

    fn focus_field(
        &mut self,
        field: Option<SearchField>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match field {
            Some(field) => {
                let focus = self.focus_of(field).clone();
                window.focus(&focus, cx);
            }
            None => self.focus_list(window, cx),
        }
        cx.notify();
    }

    fn field_key(
        &mut self,
        field: SearchField,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vim = VimConfig::of(&self.env.settings);
        let mut text = self.text_of(field).to_owned();
        if self.edit_of(field).key(&mut text, event, vim, true, cx) == Outcome::Handled {
            self.set_text(field, &text, cx);
            cx.stop_propagation();
            return;
        }
        let ks = &event.keystroke;
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
            return;
        }
        match ks.key.as_str() {
            "tab" => {
                let next = self.next_field(Some(field), ks.modifiers.shift);
                self.focus_field(next, window, cx);
            }
            "down" => self.move_selection(1, cx),
            "up" => self.move_selection(-1, cx),
            "escape" if self.confirm.is_some() => self.cancel_replace(cx),
            "enter" if self.confirm.is_some() => self.confirm_replace(window, cx),
            "enter" if field == SearchField::Replace => self.ask_replace_all(cx),
            "enter" if self.status == SearchStatus::Waiting => self.search_now(cx),
            "enter" => {
                if let Some(row) = self.selected {
                    self.open_row(row, true, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn list_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.alt {
            return;
        }
        let vim = VimConfig::of(&self.env.settings).enabled;
        match (ks.key.as_str(), ks.modifiers.shift) {
            ("tab", back) => {
                let next = self.next_field(None, back);
                self.focus_field(next, window, cx);
            }
            ("down", false) => self.move_selection(1, cx),
            ("up", false) => self.move_selection(-1, cx),
            ("n", false) if vim => self.move_selection(1, cx),
            ("n", true) if vim => self.move_selection(-1, cx),
            ("space", false) if self.show_replace => {
                if let Some(row) = self.selected {
                    self.toggle_row(row, cx);
                }
            }
            ("enter", false) if self.confirm.is_some() => self.confirm_replace(window, cx),
            ("enter", false) => {
                if let Some(row) = self.selected {
                    self.open_row(row, true, window, cx);
                }
            }
            ("escape", false) if self.confirm.is_some() => self.cancel_replace(cx),
            ("escape", false) => self.focus_field(Some(SearchField::Query), window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// How a field's element reaches its text: the platform's text input and the mouse.
    fn field_ref(&self, field: SearchField, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.focus_of(field).clone(),
            move |this: &mut Self, cx, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                let mut text = this.text_of(field).to_owned();
                this.edit_of(field).with(&mut text, vim_on, false, f);
                this.set_text(field, &text, cx);
            },
        )
    }

    // ---- rendering --------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn render_field(
        &self,
        field: SearchField,
        id: &'static str,
        placeholder: &str,
        trailing: Vec<AnyElement>,
        window: &Window,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus = self.focus_of(field).clone();
        let focused = focus.is_focused(window);
        let edit = match field {
            SearchField::Query => &self.query_edit,
            SearchField::Replace => &self.replace_edit,
            SearchField::Include => &self.include_edit,
            SearchField::Exclude => &self.exclude_edit,
        };
        div()
            .id(id)
            .key_context("SearchField")
            .track_focus(&focus)
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                this.field_key(field, event, window, cx);
            }))
            .relative()
            .flex_1()
            .min_w_0()
            .px(px(tokens.spacing * 2.0))
            .h(px(28.0))
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .outlined(theme, tokens)
            .focus_ring(focused, theme, tokens)
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, _: &MouseDownEvent, window, cx| {
                    // The dock takes focus on this same mouse down; take it afterwards.
                    cx.defer_in(window, move |this, window, cx| {
                        let focus = this.focus_of(field).clone();
                        window.focus(&focus, cx);
                    });
                }),
            )
            .children(vim_ui::string_field(
                SharedString::from(format!("{id}-text")),
                edit,
                self.text_of(field),
                VimConfig::of(&self.env.settings).enabled,
                placeholder,
                focused,
                false,
                self.field_ref(field, cx),
                theme,
                tokens,
            ))
            .children(trailing)
            .child(probe(
                &self.env.regions,
                Region::Search(match field {
                    SearchField::Query => "query",
                    SearchField::Replace => "replace",
                    SearchField::Include => "include",
                    SearchField::Exclude => "exclude",
                }),
            ))
            .into_any_element()
    }

    /// A toggle of the query field: filled and semibold while on (not colour alone).
    #[allow(clippy::too_many_arguments)] // the toggle's words and state, one call per toggle
    fn render_toggle(
        &self,
        key: &'static str,
        label: &'static str,
        tip: &'static str,
        on: bool,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let region = match key {
            "case_sensitive" => "case",
            "whole_word" => "word",
            _ => "regex",
        };
        ui::Button::new(
            SharedString::from(format!("search-toggle-{key}")),
            label,
            theme,
            tokens,
        )
        .compact()
        .ghost()
        .selected(on)
        .tooltip(format!("{tip}: {}", if on { "on" } else { "off" }))
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_option(key, cx)))
        .child(probe(&self.env.regions, Region::Search(region)))
        .into_any_element()
    }

    fn status_line(&self) -> Option<String> {
        let count = |files: &[ResultFile]| {
            let matches: usize = files.iter().map(ResultFile::match_count).sum();
            format!(
                "{} in {}",
                plural(matches, "match", "matches"),
                plural(files.len(), "file", "files")
            )
        };
        match &self.status {
            SearchStatus::Idle => None,
            SearchStatus::NoProject => Some("Open a project (⌘O) to search its files.".into()),
            SearchStatus::Unavailable => {
                Some("Searching needs a connected hutand with the file service.".into())
            }
            SearchStatus::Waiting => Some("Searching…".into()),
            SearchStatus::Running {
                files_searched,
                hits,
            } => Some(format!(
                "Searching… {} looked at, {} so far",
                plural(*files_searched as usize, "file", "files"),
                plural(*hits as usize, "line", "lines")
            )),
            SearchStatus::Done {
                files_searched,
                truncated,
            } => {
                if self.files.is_empty() {
                    return Some(format!(
                        "No results in {}",
                        plural(*files_searched as usize, "file", "files")
                    ));
                }
                let mut text = count(&self.files);
                if *truncated {
                    let shown: usize = self.files.iter().map(|f| f.hits.len()).sum();
                    text.push_str(&format!(" · stopped at {shown} results"));
                }
                Some(text)
            }
            SearchStatus::Failed(message) => Some(message.clone()),
        }
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let hover = theme.sidebar_hover;
        let focused = self.list_focus.is_focused(window);
        let replacing = self.show_replace && !self.replacement.is_empty();
        let preview = replacing.then(|| {
            self.searched
                .as_ref()
                .and_then(|s| s.spec.compile().ok().map(|re| (re, s.spec.regex)))
        });
        let preview = preview.flatten();
        let rows: Vec<(usize, SearchRow)> = range
            .filter_map(|i| self.rows.get(i).copied().map(|r| (i, r)))
            .collect();
        self.rows_rendered
            .set(self.rows_rendered.get() + rows.len());
        rows.into_iter()
            .map(|(index, row)| {
                let selected = self.selected == Some(index);
                let check = self.show_replace.then(|| match row {
                    SearchRow::File { file } => self.file_ticked(file),
                    SearchRow::Match {
                        file,
                        hit,
                        range: Some(r),
                    } => Some(!self.excluded.contains(&(file, hit, r))),
                    SearchRow::Match { range: None, .. } => Some(false),
                });
                let checkable = !matches!(row, SearchRow::Match { range: None, .. });
                let checkbox = check.map(|state| {
                    let glyph = match state {
                        Some(true) => Icon::StateCompleted,
                        Some(false) => Icon::StateTodo,
                        None => Icon::StateWorking,
                    };
                    div()
                        .id(("search-check", index))
                        .relative()
                        .flex_none()
                        .text_color(if checkable { theme.fg } else { theme.faint })
                        .when(checkable, |s| {
                            s.cursor_pointer()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_row(index, cx);
                                }))
                        })
                        .child(crate::icons::glyph(glyph))
                        .child(probe(&self.env.regions, Region::SearchCheck(index)))
                        .into_any_element()
                });
                let body: AnyElement = match row {
                    SearchRow::File { file } => {
                        let Some(file) = self.files.get(file) else {
                            return div().h(px(ROW_HEIGHT)).into_any_element();
                        };
                        let name = Path::new(&file.path).file_name().map_or_else(
                            || file.path.clone(),
                            |n| n.to_string_lossy().into_owned(),
                        );
                        let dir = Path::new(&file.path)
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(tokens.spacing * 1.5))
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.fg)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(dir)),
                            )
                            .children(file.skipped.clone().map(|reason| {
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.0))
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.warning)
                                    .child(crate::icons::glyph(Icon::StateWarning))
                                    .child(SharedString::from(format!("skipped: {reason}")))
                            }))
                            .child(
                                div()
                                    .flex_none()
                                    .px(px(5.0))
                                    .rounded(px(crate::theme::pill_radius()))
                                    .bg(theme.surface_hover)
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(file.match_count().to_string())),
                            )
                            .into_any_element()
                    }
                    SearchRow::Match { file, hit, range } => {
                        let Some(hit) = self.files.get(file).and_then(|f| f.hits.get(hit)) else {
                            return div().h(px(ROW_HEIGHT)).into_any_element();
                        };
                        let matched = range
                            .and_then(|r| hit.matched.get(r))
                            .map(|m| m[0] as usize..m[1] as usize);
                        let replacement = match (&preview, &matched) {
                            (Some((re, expand)), Some(m)) => {
                                replacement_at(re, &hit.text, m.start, &self.replacement, *expand)
                                    .map(|(_, text)| text)
                            }
                            _ => None,
                        };
                        let text = match_preview(&hit.text, matched, replacement, &theme);
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(tokens.spacing * 1.5))
                            .child(
                                div()
                                    .flex_none()
                                    .min_w(px(24.0))
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.faint)
                                    .child(SharedString::from(hit.line.to_string())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.fg)
                                    .child(text),
                            )
                            .into_any_element()
                    }
                };
                let indent = match row {
                    SearchRow::File { .. } => tokens.spacing * 2.0,
                    SearchRow::Match { .. } => tokens.spacing * 5.0,
                };
                let line = div()
                    .id(("search-row", index))
                    .relative()
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 1.5))
                    .pl(px(indent))
                    .pr(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .text_size(px(tokens.font_size))
                    .when(selected && focused, |s| s.bg(theme.sidebar_accent))
                    .when(selected && !focused, |s| s.bg(theme.surface_hover))
                    .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        // A double click moves the keyboard into the file.
                        this.open_row(index, event.click_count() >= 2, window, cx);
                    }))
                    .children(checkbox)
                    .child(body)
                    .child(probe(&self.env.regions, Region::SearchRow(index)));
                div()
                    .w_full()
                    .px(px(tokens.spacing))
                    .child(line)
                    .into_any_element()
            })
            .collect()
    }

    fn render_outcome(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let outcome = self.last_replace.as_ref()?;
        let root = self.searched.as_ref().map(|s| s.root.clone());
        let mut lines: Vec<AnyElement> = vec![
            div()
                .text_color(theme.fg)
                .child(SharedString::from(outcome.summary()))
                .into_any_element(),
        ];
        for (n, skipped) in outcome.skipped.iter().take(SKIPPED_SHOWN).enumerate() {
            let path = root.as_ref().map_or_else(
                || skipped.path.clone(),
                |r| r.join(&skipped.relative).display().to_string(),
            );
            let hover = theme.surface_hover;
            lines.push(
                div()
                    .id(("search-skipped", n))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .text_color(theme.muted)
                    .on_click(cx.listener(move |_, _, window, cx| {
                        window.dispatch_action(
                            Box::new(OpenChosenFile(ChosenFile {
                                path: path.clone(),
                                line: None,
                                column: None,
                                place: FilePlace::Focused,
                            })),
                            cx,
                        );
                    }))
                    .child(
                        div()
                            .text_color(theme.warning)
                            .child(crate::icons::glyph(Icon::StateWarning)),
                    )
                    .child(div().min_w_0().truncate().child(SharedString::from(format!(
                        "{} — {}",
                        skipped.relative,
                        skipped.reason.text()
                    ))))
                    .into_any_element(),
            );
        }
        if outcome.skipped.len() > SKIPPED_SHOWN {
            lines.push(
                div()
                    .text_color(theme.faint)
                    .child(SharedString::from(format!(
                        "and {} more",
                        outcome.skipped.len() - SKIPPED_SHOWN
                    )))
                    .into_any_element(),
            );
        }
        Some(
            div()
                .flex_none()
                .mx(px(tokens.spacing * 3.0))
                .mb(px(tokens.spacing))
                .p(px(tokens.spacing * 1.5))
                .rounded(px(tokens.radius))
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border)
                .flex()
                .flex_col()
                .gap(px(2.0))
                .text_size(px(tokens.secondary_font_size))
                .children(lines)
                .into_any_element(),
        )
    }
}

/// The hits of an answer, file by file (the daemon lists them in path order).
pub fn group(hits: Vec<SearchHit>) -> Vec<ResultFile> {
    let mut files: Vec<ResultFile> = Vec::new();
    for hit in hits {
        match files.last_mut() {
            Some(file) if file.path == hit.path => file.hits.push(hit),
            _ => files.push(ResultFile {
                path: hit.path.clone(),
                hits: vec![hit],
                skipped: None,
            }),
        }
    }
    files
}

/// What a match row shows: the text, the match's range in it, and the replacement's (shown
/// after the match) when there is one. A match far into the line cuts the line's start ("…");
/// leading blanks are left out.
pub fn preview_text(
    line: &str,
    matched: Option<Range<usize>>,
    replacement: Option<&str>,
) -> (String, Option<Range<usize>>, Option<Range<usize>>) {
    let trimmed = line.len() - line.trim_start().len();
    let Some(m) = matched.filter(|m| {
        m.start <= m.end
            && m.end <= line.len()
            && line.is_char_boundary(m.start)
            && line.is_char_boundary(m.end)
    }) else {
        return (line.trim_start().to_owned(), None, None);
    };
    // Where the shown text starts: the first non-blank, or a few characters before the match.
    let mut from = trimmed.min(m.start);
    let lead = line[from..m.start].chars().count();
    let mut prefix = "";
    if lead > PREVIEW_LEAD {
        let skip = lead - PREVIEW_LEAD;
        from = line[from..]
            .char_indices()
            .nth(skip)
            .map_or(m.start, |(i, _)| from + i);
        prefix = "…";
    }
    let mut text = String::with_capacity(line.len() + 8);
    text.push_str(prefix);
    text.push_str(&line[from..m.start]);
    let start = text.len();
    text.push_str(&line[m.clone()]);
    let end = text.len();
    let new = replacement.map(|new| {
        text.push_str(new);
        end..text.len()
    });
    text.push_str(&line[m.end..]);
    (text, Some(start..end), new)
}

/// [`preview_text`] drawn: the match semibold and underlined on a soft fill; with a
/// replacement, the match struck through on the removed tint and the new text semibold and
/// underlined on the added tint (never colour alone).
fn match_preview(
    line: &str,
    matched: Option<Range<usize>>,
    replacement: Option<String>,
    theme: &Theme,
) -> StyledText {
    let (text, matched, new) = preview_text(line, matched, replacement.as_deref());
    let underline = UnderlineStyle {
        thickness: px(1.0),
        color: Some(theme.fg),
        wavy: false,
    };
    let mut highlights = Vec::new();
    match (matched, new) {
        (Some(matched), Some(new)) => {
            highlights.push((
                matched,
                HighlightStyle {
                    color: Some(theme.muted),
                    strikethrough: Some(gpui::StrikethroughStyle {
                        thickness: px(1.0),
                        color: Some(theme.muted),
                    }),
                    background_color: Some(theme.diff_removed_bg),
                    ..HighlightStyle::default()
                },
            ));
            highlights.push((
                new,
                HighlightStyle {
                    font_weight: Some(gpui::FontWeight::SEMIBOLD),
                    underline: Some(underline),
                    background_color: Some(theme.diff_added_bg),
                    ..HighlightStyle::default()
                },
            ));
        }
        (Some(matched), None) => highlights.push((
            matched,
            HighlightStyle {
                font_weight: Some(gpui::FontWeight::SEMIBOLD),
                underline: Some(underline),
                background_color: Some(theme.accent_soft),
                ..HighlightStyle::default()
            },
        )),
        _ => {}
    }
    StyledText::new(SharedString::from(text)).with_highlights(highlights)
}

impl Focusable for SearchPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.query_focus.clone()
    }
}

impl Render for SearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        self.renders += 1;
        // A toggle may have changed in Settings (or settings.toml).
        self.follow_options(cx);
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let settings = self.settings();
        let show_replace = self.show_replace;
        let replace_toggle = ui::Button::new("search-toggle-replace", "⇄ Replace", &theme, &tokens)
            .compact()
            .selected(show_replace)
            .tooltip(if show_replace {
                "Hide the replace field"
            } else {
                "Replace (⌘⇧H)"
            })
            .on_click(cx.listener(move |this, _, _, cx| this.set_replace_shown(!show_replace, cx)))
            .child(probe(&self.env.regions, Region::Search("toggle-replace")))
            .into_any_element();
        let header = panel_header_with("Search", "panels.search", vec![replace_toggle], cx);

        let toggles = vec![
            self.render_toggle(
                "case_sensitive",
                "Aa",
                "Match case",
                settings.case_sensitive,
                &theme,
                &tokens,
                cx,
            ),
            self.render_toggle(
                "whole_word",
                "ab",
                "Whole word",
                settings.whole_word,
                &theme,
                &tokens,
                cx,
            ),
            self.render_toggle(
                "regex",
                ".*",
                "Regular expression",
                settings.regex,
                &theme,
                &tokens,
                cx,
            ),
        ];
        let mut query_parts = vec![icon(Icon::ActionSearch, 14.0, theme.muted).into_any_element()];
        let query_field = self.render_field(
            SearchField::Query,
            "search-query",
            "Search",
            toggles,
            window,
            &theme,
            &tokens,
            cx,
        );
        query_parts.push(query_field);
        let row = |children: Vec<AnyElement>| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 1.5))
                .mx(px(tokens.spacing * 3.0))
                .mb(px(tokens.spacing))
                .children(children)
                .into_any_element()
        };
        let mut fields = vec![row(query_parts)];
        if self.show_replace {
            let count = self.replace_count();
            let enabled = self.can_replace() && count.matches > 0;
            let button = ui::Button::new("search-replace-all", "Replace all", &theme, &tokens)
                .compact()
                .disabled(!enabled)
                .tooltip("Replace every ticked match (asks first)")
                .on_click(cx.listener(|this, _, _, cx| this.ask_replace_all(cx)))
                .child(probe(&self.env.regions, Region::Search("replace-all")))
                .into_any_element();
            let field = self.render_field(
                SearchField::Replace,
                "search-replace",
                "Replace",
                Vec::new(),
                window,
                &theme,
                &tokens,
                cx,
            );
            fields.push(row(vec![
                div().flex_none().w(px(14.0)).into_any_element(),
                field,
                button,
            ]));
        }
        let include = self.render_field(
            SearchField::Include,
            "search-include",
            "Files to include (src/**/*.rs, *.md)",
            Vec::new(),
            window,
            &theme,
            &tokens,
            cx,
        );
        let exclude = self.render_field(
            SearchField::Exclude,
            "search-exclude",
            "Files to exclude",
            Vec::new(),
            window,
            &theme,
            &tokens,
            cx,
        );
        fields.push(row(vec![
            div().flex_none().w(px(14.0)).into_any_element(),
            include,
        ]));
        fields.push(row(vec![
            div().flex_none().w(px(14.0)).into_any_element(),
            exclude,
        ]));

        let status = self.status_line().map(|text| {
            let failed = matches!(self.status, SearchStatus::Failed(_));
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.0))
                .px(px(tokens.spacing * 3.0))
                .pb(px(tokens.spacing))
                .text_size(px(tokens.secondary_font_size))
                .text_color(if failed { theme.danger } else { theme.muted })
                .when(failed, |s| s.child(crate::icons::glyph(Icon::StateFailed)))
                .child(div().min_w_0().truncate().child(SharedString::from(text)))
        });
        let waiting_versions = self.show_replace
            && matches!(self.status, SearchStatus::Done { .. })
            && !self.files.is_empty()
            && !self.versions_ready();
        let versions_note = waiting_versions.then(|| {
            div()
                .flex_none()
                .px(px(tokens.spacing * 3.0))
                .pb(px(tokens.spacing))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.faint)
                .child("Reading the files' versions…")
        });
        let confirm = self.confirm.map(|count| {
            let yes = ui::Button::new("search-confirm", "Replace", &theme, &tokens)
                .compact()
                .primary()
                .on_click(cx.listener(|this, _, window, cx| this.confirm_replace(window, cx)))
                .child(probe(&self.env.regions, Region::Search("confirm")));
            let no = ui::Button::new("search-cancel", "Cancel", &theme, &tokens)
                .compact()
                .ghost()
                .on_click(cx.listener(|this, _, _, cx| this.cancel_replace(cx)))
                .child(probe(&self.env.regions, Region::Search("cancel")));
            div()
                .flex_none()
                .mx(px(tokens.spacing * 3.0))
                .mb(px(tokens.spacing))
                .p(px(tokens.spacing * 1.5))
                .rounded(px(tokens.radius))
                .border_1()
                .border_color(theme.border)
                .bg(theme.accent_soft)
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(
                    div()
                        .text_color(theme.fg)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(SharedString::from(count.question())),
                )
                .child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child("Each file changes in one step: ⌘Z in its tab takes it back. Unsaved files change in their tab only."),
                )
                .child(div().flex().gap(px(tokens.spacing)).child(no).child(yes))
        });
        let replacing_note = self.replacing.is_some().then(|| {
            div()
                .flex_none()
                .px(px(tokens.spacing * 3.0))
                .pb(px(tokens.spacing))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child("Replacing…")
        });
        let outcome = self.render_outcome(&theme, &tokens, cx);

        let count = self.rows.len();
        let body: AnyElement = if count > 0 {
            uniform_list(
                "search-results",
                count,
                cx.processor(|this, range: Range<usize>, window, cx| {
                    this.render_rows(range, window, cx)
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        } else {
            let hint = match self.status {
                SearchStatus::Idle => Some("Type to search the project's files. ⌘⇧H replaces."),
                _ => None,
            };
            div()
                .flex_1()
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing * 2.0))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.faint)
                .children(hint)
                .into_any_element()
        };
        let list_focused = self.list_focus.is_focused(window);
        let list = div()
            .id("search-list")
            .key_context("SearchList")
            .track_focus(&self.list_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.list_key(event, window, cx);
            }))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(list_focused, |s| s.border_t_1().border_color(theme.focus))
            .child(body);

        div()
            .id("search-panel")
            .key_context("SearchPanel")
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(header)
            .children(fields)
            .children(status)
            .children(versions_note)
            .children(confirm)
            .children(replacing_note)
            .children(outcome)
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn hit(path: &str, line: u32, text: &str, matched: &[[u32; 2]]) -> SearchHit {
        SearchHit {
            path: path.into(),
            line,
            text: text.into(),
            matched: matched.to_vec(),
        }
    }

    #[test]
    fn hits_group_by_file_in_the_order_they_came() {
        let files = group(vec![
            hit("a.rs", 1, "needle needle", &[[0, 6], [7, 13]]),
            hit("a.rs", 4, "needle", &[[0, 6]]),
            hit("b/c.rs", 2, "far", &[]),
        ]);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].match_count(), 3);
        assert_eq!(
            files[1].match_count(),
            1,
            "a line with its match past the cut counts once"
        );
    }

    #[test]
    fn the_preview_marks_the_match_and_cuts_a_long_lead() {
        let line = format!("    {}needle tail", "x".repeat(60));
        let start = 4 + 60;
        let (shown, matched, new) = preview_text(&line, Some(start..start + 6), None);
        assert!(shown.starts_with('…'), "{shown}");
        assert!(shown.ends_with("needle tail"));
        assert_eq!(&shown[matched.unwrap()], "needle");
        assert!(new.is_none());
        let (short, matched, new) = preview_text("  let needle = 1;", Some(6..12), Some("pin"));
        assert_eq!(short, "let needlepin = 1;");
        assert_eq!(&short[matched.unwrap()], "needle");
        assert_eq!(&short[new.unwrap()], "pin");
        let (plain, matched, _) = preview_text("\tfar", None, None);
        assert_eq!((plain.as_str(), matched), ("far", None));
    }

    #[test]
    fn the_question_counts_matches_and_files() {
        let count = ReplaceCount {
            matches: 42,
            files: 7,
        };
        assert_eq!(count.question(), "Replace 42 matches in 7 files?");
        let one = ReplaceCount {
            matches: 1,
            files: 1,
        };
        assert_eq!(one.question(), "Replace 1 match in 1 file?");
    }
}
