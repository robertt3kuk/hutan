//! The `files` panel (E-13a): the active project's folder as a tree, read through the daemon
//! (`fs.list`, one folder at a time when it is opened), so it works for a remote window too and
//! the daemon decides what may be read.
//!
//! - **Ignore-aware.** `fs.list` flags what `.gitignore` / `.ignore` rules match (and `.git`);
//!   the tree hides those unless `panels.files.show_ignored` (the header's eye) shows them,
//!   dimmed.
//! - **Lazy and virtualized.** A folder is listed when it is opened; the rows are a flat list of
//!   what is open ([`Tree`]), drawn with `uniform_list`, so a frame builds only the rows in view
//!   however many thousands are open.
//! - **Live.** Open folders are watched (`fs.watch`, at most [`MAX_WATCHED_DIRS`]): an
//!   `fs.changed` lists the folder it happened in again; an `fs.generation` of the root lists
//!   the open folders that are not watched; an overflow lists every open folder.
//! - **Git status** (FX-E2E2-C, `files_git`). A file with uncommitted changes shows a letter
//!   (`M` modified, `A` added and staged, `U` untracked, `D` deleted, `R` renamed) and a tint, a
//!   folder that holds changes a dot. The marks come from the file list of `worktree.diff`
//!   (no patches), read when the root is set, on `fs.changed` and when a checkout's activity
//!   ends, one read at a time; a frame only looks them up. A root outside git has none. The
//!   icon theme has one file icon, so file icons are not told apart by type. A deleted file has
//!   no row to decorate; its folder still gets the dot.
//! - **Keyboard.** ↑↓ move, → opens a folder (or steps into it), ← closes it (or steps to its
//!   parent), Return opens a file and moves the keyboard to it, Space shows it and keeps the
//!   keyboard in the tree. A click shows a file (the keyboard stays), a double click opens it.
//! - **Reveal.** The workbench reveals the focused tab's file (`panels.files.auto_reveal`, and
//!   *Files: Reveal Active File*): its folders open, its row is selected and scrolled to.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, ScrollStrategy, SharedString, StatefulInteractiveElement,
    Styled, Task, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use hutan_client::{ClientError, Notice};
use hutan_protocol::fs::{FsEntry, FsKind, FsList, FsListing};
use hutan_protocol::worktree::WorktreeDiffRequest;
use tokio::sync::broadcast::error::RecvError;

use super::common::{panel_header_with, scope_chip};
use super::files_git::GitStatus;
use crate::icons::{Icon, icon};
use crate::sessions::CheckoutActivity;
use crate::settings::FilesPanelSettings;
use crate::theme::DensityExt as _;
use crate::workbench::actions::{ChosenFile, OpenChosenFile};
use crate::workbench::files::FilePlace;
use crate::workbench::registry::PanelEnv;
use crate::workbench::style::UiStyle;
use crate::workbench::ui;

/// Open folders watched at once; folders opened beyond it are listed again on the root's
/// `fs.generation` instead (the connection may watch 256 paths, open files included).
pub const MAX_WATCHED_DIRS: usize = 64;
/// Indentation per level.
const INDENT: f32 = 12.0;

/// What a row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Dir,
    File,
    /// A symbolic link: what it points at is asked when it is opened.
    Link,
    /// The folder has more entries than `fs.list` gives.
    More,
    /// The folder could not be listed.
    Failed,
}

/// One drawn row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: PathBuf,
    pub name: SharedString,
    pub depth: usize,
    pub kind: RowKind,
    pub ignored: bool,
    pub expanded: bool,
    /// An open folder whose listing is on its way.
    pub loading: bool,
}

/// What is known of a folder.
#[derive(Debug, Clone)]
enum Listing {
    Loading,
    Listed {
        entries: Vec<FsEntry>,
        truncated: bool,
    },
    Failed(String),
}

/// The tree of one root: the listings of the folders seen, which are open, and the flat rows
/// of what shows (rebuilt when any of that changes, never per frame).
#[derive(Debug, Clone)]
pub struct Tree {
    root: PathBuf,
    listings: HashMap<PathBuf, Listing>,
    expanded: HashSet<PathBuf>,
    rows: Vec<Row>,
    show_ignored: bool,
}

impl Tree {
    /// The root, open and not listed yet.
    pub fn new(root: PathBuf, show_ignored: bool) -> Self {
        let mut tree = Self {
            listings: HashMap::new(),
            expanded: HashSet::from([root.clone()]),
            root,
            rows: Vec::new(),
            show_ignored,
        };
        tree.listings.insert(tree.root.clone(), Listing::Loading);
        tree
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Whether `dir` is open (the root always is).
    pub fn is_expanded(&self, dir: &Path) -> bool {
        self.expanded.contains(dir)
    }

    /// The open folders, the root first.
    pub fn expanded_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        dirs.sort_by_key(|d| d.components().count());
        dirs
    }

    /// Whether `dir` was listed (or is being listed).
    pub fn knows(&self, dir: &Path) -> bool {
        self.listings.contains_key(dir)
    }

    pub fn set_show_ignored(&mut self, show: bool) {
        if self.show_ignored != show {
            self.show_ignored = show;
            self.rebuild();
        }
    }

    /// `dir` is being listed (again): a folder seen for the first time shows a loading row.
    pub fn set_loading(&mut self, dir: &Path) {
        self.listings
            .entry(dir.to_owned())
            .or_insert(Listing::Loading);
    }

    /// The daemon's listing of `dir`. Canonical paths are the daemon's business: the entries are
    /// filed under the folder the tree asked for.
    pub fn set_listing(&mut self, dir: &Path, listing: FsListing) {
        let entries = listing
            .entries
            .into_iter()
            .map(|mut entry| {
                entry.path = dir.join(&entry.name);
                entry
            })
            .collect();
        self.listings.insert(
            dir.to_owned(),
            Listing::Listed {
                entries,
                truncated: listing.truncated,
            },
        );
        self.rebuild();
    }

    pub fn set_failed(&mut self, dir: &Path, message: String) {
        self.listings
            .insert(dir.to_owned(), Listing::Failed(message));
        self.rebuild();
    }

    /// Opens `dir`; true when it has to be listed first.
    pub fn expand(&mut self, dir: &Path) -> bool {
        self.expanded.insert(dir.to_owned());
        let needs = !self.listings.contains_key(dir);
        if needs {
            self.listings.insert(dir.to_owned(), Listing::Loading);
        }
        self.rebuild();
        needs
    }

    /// Closes `dir` and every folder inside it; returns the folders closed (for unwatching).
    pub fn collapse(&mut self, dir: &Path) -> Vec<PathBuf> {
        if dir == self.root {
            return Vec::new();
        }
        let closed: Vec<PathBuf> = self
            .expanded
            .iter()
            .filter(|d| d.starts_with(dir))
            .cloned()
            .collect();
        for d in &closed {
            self.expanded.remove(d);
        }
        self.rebuild();
        closed
    }

    /// The folders between the root (included) and `path` (excluded), outermost first; `None`
    /// when `path` is not below the root.
    pub fn ancestors_of(&self, path: &Path) -> Option<Vec<PathBuf>> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let mut dirs = vec![self.root.clone()];
        let mut at = self.root.clone();
        let components: Vec<_> = relative.components().collect();
        for component in components.iter().take(components.len().saturating_sub(1)) {
            at.push(component);
            dirs.push(at.clone());
        }
        Some(dirs)
    }

    /// Where `path` is among the rows.
    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|row| row.path == path)
    }

    /// The parent row of the row at `index`.
    pub fn parent_index(&self, index: usize) -> Option<usize> {
        let depth = self.rows.get(index)?.depth;
        (0..index).rev().find(|i| self.rows[*i].depth < depth)
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        self.push_rows(&self.root.clone(), 0, &mut rows);
        self.rows = rows;
    }

    fn push_rows(&self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        match self.listings.get(dir) {
            None | Some(Listing::Loading) => {}
            Some(Listing::Failed(message)) => rows.push(Row {
                path: dir.join("\u{0}failed"),
                name: message.clone().into(),
                depth,
                kind: RowKind::Failed,
                ignored: false,
                expanded: false,
                loading: false,
            }),
            Some(Listing::Listed { entries, truncated }) => {
                for entry in entries {
                    if entry.ignored && !self.show_ignored {
                        continue;
                    }
                    let kind = match entry.kind {
                        FsKind::Dir => RowKind::Dir,
                        FsKind::Symlink => RowKind::Link,
                        FsKind::File | FsKind::Other => RowKind::File,
                    };
                    let expanded = kind == RowKind::Dir && self.expanded.contains(&entry.path);
                    rows.push(Row {
                        path: entry.path.clone(),
                        name: entry.name.clone().into(),
                        depth,
                        kind,
                        ignored: entry.ignored,
                        expanded,
                        loading: expanded
                            && matches!(
                                self.listings.get(&entry.path),
                                None | Some(Listing::Loading)
                            ),
                    });
                    if expanded {
                        self.push_rows(&entry.path, depth + 1, rows);
                    }
                }
                if *truncated {
                    rows.push(Row {
                        path: dir.join("\u{0}more"),
                        name: "More entries than the tree lists".into(),
                        depth,
                        kind: RowKind::More,
                        ignored: false,
                        expanded: false,
                        loading: false,
                    });
                }
            }
        }
    }
}

/// The Files panel.
pub struct FilesPanel {
    env: PanelEnv,
    focus: FocusHandle,
    tree: Option<Tree>,
    /// The tree of the root before the last switch of container: drawn (not interactive) until
    /// the new root's first listing arrives, so a switch never flashes an empty panel.
    stale: Option<Tree>,
    selected: Option<PathBuf>,
    /// The file of the focused tab, as last revealed.
    active_file: Option<PathBuf>,
    /// A file to select once the folders it is in are listed.
    pending_reveal: Option<PathBuf>,
    scroll: UniformListScrollHandle,
    /// `fs.list` in flight, per folder; a newer one replaces (cancels) the older.
    listing: HashMap<PathBuf, Task<()>>,
    /// The open folders watched for this panel (through the store's shared watches).
    watched: HashSet<PathBuf>,
    listener: Option<Task<()>>,
    /// Rows built so far (tests: only the rows in view are).
    rows_rendered: std::rc::Rc<Cell<usize>>,
    /// Listings answered so far (tests wait on it).
    listings_done: usize,
    /// The git marks of the root (`worktree.diff`, the file list), refreshed on `fs.changed`
    /// and when a checkout's activity ends; a frame only looks them up.
    git: GitStatus,
    /// A `worktree.diff` is in flight; a trigger meanwhile asks for one more afterwards.
    git_task: Option<Task<()>>,
    git_again: bool,
    /// The root and connection state the git status was last asked for: it is asked again when
    /// either changes (a window that connects after the root was set has no marks yet).
    git_asked: Option<(PathBuf, hutan_client::ConnectionState)>,
    /// Reads of the git status answered so far (tests wait on it).
    git_reads: usize,
    _subscriptions: Vec<gpui::Subscription>,
}

impl std::fmt::Debug for FilesPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesPanel")
            .field("root", &self.tree.as_ref().map(Tree::root))
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl FilesPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe(&env.sessions, |this, _, cx| {
                this.follow_root(cx);
                this.sync_git(cx);
            }),
            // A command or an agent's turn ended in a checkout: its files may have changed.
            cx.subscribe(&env.sessions, |this, _, _: &CheckoutActivity, cx| {
                this.refresh_git(cx);
            }),
        ];
        let mut panel = Self {
            env,
            focus: cx.focus_handle(),
            tree: None,
            stale: None,
            selected: None,
            active_file: None,
            pending_reveal: None,
            scroll: UniformListScrollHandle::new(),
            listing: HashMap::new(),
            watched: HashSet::new(),
            listener: None,
            rows_rendered: std::rc::Rc::new(Cell::new(0)),
            listings_done: 0,
            git: GitStatus::default(),
            git_task: None,
            git_again: false,
            git_asked: None,
            git_reads: 0,
            _subscriptions: subscriptions,
        };
        panel.follow_root(cx);
        panel.sync_git(cx);
        panel
    }

    // ---- read access (also for tests) -----------------------------------------------------

    /// The folder the tree shows; `None` at Home or before the daemon says what is active.
    pub fn root(&self) -> Option<&Path> {
        self.tree.as_ref().map(Tree::root)
    }

    /// What the panel is about: the worktree picked in the sidebar, else the project the window
    /// works in (the chip of its header).
    pub fn scope(&self, cx: &gpui::App) -> Option<crate::sessions::ScopeLabel> {
        self.env.sessions.read(cx).project_scope()
    }
    /// The rows the tree shows now, top to bottom.
    pub fn rows(&self) -> &[Row] {
        self.tree.as_ref().map_or(&[], Tree::rows)
    }

    /// The rows to draw: the tree's, or while a switch of container waits for its first listing
    /// the ones of the tree before it.
    fn shown_rows(&self) -> &[Row] {
        match (&self.stale, self.rows()) {
            (Some(stale), []) => stale.rows(),
            (_, rows) => rows,
        }
    }

    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// The file of the focused tab, as last revealed.
    pub fn active_file(&self) -> Option<&Path> {
        self.active_file.as_deref()
    }

    /// Rows built for frames so far.
    pub fn rows_rendered(&self) -> usize {
        self.rows_rendered.get()
    }

    /// `fs.list` answers taken so far.
    pub fn listings_done(&self) -> usize {
        self.listings_done
    }

    /// No `fs.list` is in flight.
    pub fn settled(&self) -> bool {
        self.listing.is_empty()
    }

    /// The git marks of the root's files and folders.
    pub fn git(&self) -> &GitStatus {
        &self.git
    }

    /// Reads of the git status answered so far.
    pub fn git_reads(&self) -> usize {
        self.git_reads
    }

    /// No git status read is in flight.
    pub fn git_settled(&self) -> bool {
        self.git_task.is_none() && !self.git_again
    }

    /// The folders watched for the tree.
    pub fn watched_dirs(&self) -> usize {
        self.watched.len()
    }

    // ---- the root ---------------------------------------------------------------------------

    /// Follows the project the window works in: a new root starts a new tree.
    fn follow_root(&mut self, cx: &mut Context<Self>) {
        let root = self.env.sessions.read(cx).files_root();
        if root.as_deref() == self.root() {
            return;
        }
        self.unwatch_all(cx);
        self.listing.clear();
        self.git = GitStatus::default();
        self.git_task = None;
        self.git_again = false;
        self.git_asked = None;
        self.selected = None;
        self.pending_reveal = None;
        let show_ignored = self.env.settings.get::<FilesPanelSettings>().show_ignored;
        let old = self.tree.take();
        self.stale = old.filter(|tree| !tree.rows().is_empty() && root.is_some());
        self.tree = root.map(|root| Tree::new(root, show_ignored));
        match self.tree.as_ref().map(|t| t.root().to_owned()) {
            Some(root) => {
                self.listen(cx);
                self.list(&root, cx);
                self.watch(&root, cx);
                if let Some(file) = self.active_file.clone() {
                    self.reveal(&file, cx);
                }
            }
            None => self.listener = None,
        }
        cx.notify();
    }

    // ---- listing and watching -------------------------------------------------------------

    fn list(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        tree.set_loading(dir);
        let connection = self.env.sessions.read(cx).connection().clone();
        let request = FsList {
            dir: dir.to_owned(),
            ignore: true,
            limit: None,
            picked: false,
        };
        let answer = cx.background_spawn(async move { connection.fs_list(request).await });
        let key = dir.to_owned();
        let task = cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |this, cx| this.listed(&key, answer, cx))
                .ok();
        });
        self.listing.insert(dir.to_owned(), task);
    }

    fn listed(
        &mut self,
        dir: &Path,
        answer: Result<FsListing, ClientError>,
        cx: &mut Context<Self>,
    ) {
        self.listing.remove(dir);
        self.listings_done += 1;
        let Some(tree) = &mut self.tree else {
            return;
        };
        if !tree.knows(dir) {
            return;
        }
        match answer {
            Ok(listing) => tree.set_listing(dir, listing),
            Err(error) => tree.set_failed(dir, crate::workbench::files::error_text(&error)),
        }
        if tree.root() == dir {
            self.stale = None;
        }
        if let Some(file) = self.pending_reveal.clone() {
            self.reveal(&file, cx);
        }
        self.keep_selection();
        cx.notify();
    }

    /// Asks for the git status when the root or the connection is not what it was asked for.
    fn sync_git(&mut self, cx: &mut Context<Self>) {
        let wanted = self.root().map(|root| {
            (
                root.to_owned(),
                self.env.sessions.read(cx).connection_state(),
            )
        });
        if wanted == self.git_asked {
            return;
        }
        self.git_asked = wanted.clone();
        if matches!(wanted, Some((_, hutan_client::ConnectionState::Connected))) {
            self.refresh_git(cx);
        }
    }

    /// Reads which files of the root are changed (`worktree.diff` without patches), one read at a
    /// time. A root outside git, or a daemon without worktree review, has no marks.
    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.tree.as_ref().map(|t| t.root().to_owned()) else {
            return;
        };
        if self.git_task.is_some() {
            self.git_again = true;
            return;
        }
        let store = self.env.sessions.read(cx);
        if !store.connection().supports_worktree_review() {
            return;
        }
        let read = self.env.sessions.update(cx, |store, cx| {
            store.worktree_diff(
                WorktreeDiffRequest {
                    path: root.clone(),
                    base: None,
                    include_uncommitted: true,
                },
                cx,
            )
        });
        self.git_task = Some(cx.spawn(async move |this, cx| {
            let answer = read.await;
            this.update(cx, |this, cx| {
                this.git_task = None;
                this.git_reads += 1;
                if this.root() != Some(root.as_path()) {
                    return;
                }
                let status = match answer {
                    Ok(diff) => GitStatus::from_files(&root, &diff.files),
                    // Not a git checkout (or unreadable): no decorations.
                    Err(_) => GitStatus::default(),
                };
                if status != this.git {
                    this.git = status;
                    cx.notify();
                }
                if std::mem::take(&mut this.git_again) {
                    this.refresh_git(cx);
                }
            })
            .ok();
        }));
    }

    fn watch(&mut self, dir: &Path, cx: &mut Context<Self>) {
        if self.watched.len() >= MAX_WATCHED_DIRS || !self.watched.insert(dir.to_owned()) {
            return;
        }
        self.env
            .sessions
            .update(cx, |store, cx| store.watch_dir(dir, cx));
    }

    fn unwatch(&mut self, dir: &Path, cx: &mut Context<Self>) {
        if self.watched.remove(dir) {
            self.env
                .sessions
                .update(cx, |store, cx| store.unwatch_dir(dir, cx));
        }
    }

    fn unwatch_all(&mut self, cx: &mut Context<Self>) {
        for dir in std::mem::take(&mut self.watched) {
            self.env
                .sessions
                .update(cx, |store, cx| store.unwatch_dir(&dir, cx));
        }
    }

    /// Takes the connection's `fs.changed` and `fs.generation` while there is a root.
    fn listen(&mut self, cx: &mut Context<Self>) {
        if self.listener.is_some() {
            return;
        }
        let mut notices = self.env.sessions.read(cx).connection().notices();
        self.listener = Some(cx.spawn(async move |this, cx| {
            loop {
                let notice = match notices.recv().await {
                    Ok(notice @ (Notice::FsChanged { .. } | Notice::FsGeneration { .. })) => notice,
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => Notice::FsChanged {
                        changes: Vec::new(),
                        overflow: true,
                    },
                    Err(RecvError::Closed) => break,
                };
                if this
                    .update(cx, |this, cx| this.on_notice(notice, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn on_notice(&mut self, notice: Notice, cx: &mut Context<Self>) {
        let Some(tree) = &self.tree else {
            return;
        };
        let open = tree.expanded_dirs();
        let again: Vec<PathBuf> = match notice {
            Notice::FsChanged { overflow: true, .. } => open,
            Notice::FsChanged { changes, .. } => {
                let mut dirs = HashSet::new();
                for change in &changes {
                    for path in std::iter::once(&change.path).chain(change.from.as_ref()) {
                        if let Some(parent) = path.parent()
                            && tree.is_expanded(parent)
                        {
                            dirs.insert(parent.to_owned());
                        }
                    }
                }
                open.into_iter().filter(|d| dirs.contains(d)).collect()
            }
            Notice::FsGeneration { root, .. } => {
                let ours = tree.root();
                if !(ours.starts_with(&root) || root.starts_with(ours)) {
                    return;
                }
                // The watched folders hear of their changes themselves.
                open.into_iter()
                    .filter(|d| !self.watched.contains(d))
                    .collect()
            }
            _ => return,
        };
        for dir in again {
            self.list(&dir, cx);
        }
        self.refresh_git(cx);
    }

    // ---- what the user does ---------------------------------------------------------------

    /// Opens or closes the folder at `path`.
    pub fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        if tree.is_expanded(path) {
            for dir in tree.collapse(path) {
                self.unwatch(&dir, cx);
            }
            self.keep_selection();
        } else {
            self.open_dir(path, cx);
        }
        cx.notify();
    }

    fn open_dir(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        let needs = tree.expand(path);
        if needs {
            self.list(path, cx);
        }
        self.watch(path, cx);
    }

    /// Shows the file (or opens the folder) at row `index`: Return (`focus`) moves the keyboard
    /// to the file, Space and a click (`!focus`) keep it here.
    fn activate(&mut self, index: usize, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows().get(index).cloned() else {
            return;
        };
        self.selected = Some(row.path.clone());
        match row.kind {
            RowKind::Dir => self.toggle(&row.path, cx),
            RowKind::File | RowKind::Link => {
                let place = if focus {
                    FilePlace::Focused
                } else {
                    FilePlace::Preview
                };
                window.dispatch_action(
                    Box::new(OpenChosenFile(ChosenFile {
                        path: row.path.display().to_string(),
                        line: None,
                        column: None,
                        place,
                    })),
                    cx,
                );
            }
            RowKind::More | RowKind::Failed => {}
        }
        cx.notify();
    }

    fn selected_index(&self) -> Option<usize> {
        let tree = self.tree.as_ref()?;
        tree.index_of(self.selected.as_deref()?)
    }

    fn select_index(&mut self, index: usize) {
        if let Some(row) = self.rows().get(index) {
            self.selected = Some(row.path.clone());
            self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
        }
    }

    /// A selected row that went (its folder closed, the file was removed) hands the selection
    /// to its nearest open folder.
    fn keep_selection(&mut self) {
        let Some(tree) = &self.tree else {
            return;
        };
        let Some(selected) = self.selected.clone() else {
            return;
        };
        if tree.index_of(&selected).is_some() {
            return;
        }
        self.selected = selected
            .ancestors()
            .skip(1)
            .find(|a| tree.index_of(a).is_some())
            .map(Path::to_owned);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.platform
            || keystroke.modifiers.control
            || keystroke.modifiers.alt
            || keystroke.modifiers.shift
        {
            return false;
        }
        let count = self.rows().len();
        if count == 0 {
            return false;
        }
        let current = self.selected_index();
        match keystroke.key.as_str() {
            "down" => {
                let next = current.map_or(0, |i| (i + 1).min(count - 1));
                self.select_index(next);
            }
            "up" => {
                let next = current.map_or(0, |i| i.saturating_sub(1));
                self.select_index(next);
            }
            "right" => {
                let Some(index) = current else {
                    self.select_index(0);
                    cx.notify();
                    return true;
                };
                let row = self.rows()[index].clone();
                if row.kind == RowKind::Dir {
                    if row.expanded {
                        if self
                            .rows()
                            .get(index + 1)
                            .is_some_and(|r| r.depth > row.depth)
                        {
                            self.select_index(index + 1);
                        }
                    } else {
                        self.open_dir(&row.path, cx);
                    }
                }
            }
            "left" => {
                let Some(index) = current else {
                    return true;
                };
                let row = self.rows()[index].clone();
                if row.kind == RowKind::Dir && row.expanded {
                    self.toggle(&row.path, cx);
                } else if let Some(parent) = self.tree.as_ref().and_then(|t| t.parent_index(index))
                {
                    self.select_index(parent);
                }
            }
            "enter" => {
                if let Some(index) = current {
                    self.activate(index, true, window, cx);
                }
            }
            "space" => {
                if let Some(index) = current {
                    self.activate(index, false, window, cx);
                }
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// Selects `file` in the tree, opening the folders it is in (each listed first when it has
    /// not been) and scrolling to it.
    pub fn reveal(&mut self, file: &Path, cx: &mut Context<Self>) {
        self.active_file = Some(file.to_owned());
        let Some(tree) = &self.tree else {
            return;
        };
        let Some(dirs) = tree.ancestors_of(file) else {
            self.pending_reveal = None;
            return;
        };
        let mut waiting = false;
        for dir in dirs {
            let known = self.tree.as_ref().is_some_and(|t| t.is_expanded(&dir));
            if !known {
                self.open_dir(&dir, cx);
            }
            if self.listing.contains_key(&dir) {
                waiting = true;
            }
        }
        if waiting {
            self.pending_reveal = Some(file.to_owned());
        } else {
            self.pending_reveal = None;
            if let Some(index) = self.tree.as_ref().and_then(|t| t.index_of(file)) {
                self.selected = Some(file.to_owned());
                self.scroll.scroll_to_item(index, ScrollStrategy::Center);
            }
        }
        cx.notify();
    }

    /// Puts the keyboard in the tree.
    pub fn focus_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.selected.is_none() && !self.rows().is_empty() {
            self.select_index(0);
        }
        cx.notify();
    }

    fn toggle_ignored(&mut self, cx: &mut Context<Self>) {
        let show = !self.env.settings.get::<FilesPanelSettings>().show_ignored;
        if let Err(err) = self
            .env
            .settings
            .write_user("panels.files.show_ignored", toml::Value::Boolean(show))
        {
            tracing::warn!("cannot save panels.files.show_ignored: {err}");
        }
        if let Some(tree) = &mut self.tree {
            tree.set_show_ignored(show);
        }
        self.keep_selection();
        cx.notify();
    }

    // ---- rendering --------------------------------------------------------------------------

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let hover = theme.sidebar_hover;
        let focused_row = self.selected.clone();
        let active = self.active_file.clone();
        let rows: Vec<(usize, Row)> = range
            .filter_map(|i| self.shown_rows().get(i).cloned().map(|row| (i, row)))
            .collect();
        self.rows_rendered
            .set(self.rows_rendered.get() + rows.len());
        rows.into_iter()
            .map(|(index, row)| {
                let selected = focused_row.as_ref() == Some(&row.path);
                let is_active = active.as_ref() == Some(&row.path);
                // Git: a changed file gets a letter and a tint, a folder with changes a dot (never
                // for ignored rows, which git does not track).
                let mark = (!row.ignored && matches!(row.kind, RowKind::File | RowKind::Link))
                    .then(|| self.git.mark(&row.path))
                    .flatten();
                let dotted =
                    !row.ignored && row.kind == RowKind::Dir && self.git.has_changes_in(&row.path);
                let color = match row.kind {
                    RowKind::More | RowKind::Failed => theme.muted,
                    _ if row.ignored => theme.faint,
                    _ => mark.map_or(theme.fg, |m| m.color(&theme)),
                };
                let chevron = match row.kind {
                    RowKind::Dir if row.expanded => Some(Icon::ActionExpand),
                    RowKind::Dir => Some(Icon::ActionDisclose),
                    _ => None,
                };
                let glyph = match row.kind {
                    RowKind::Dir if row.expanded => Some(Icon::ItemFolderOpen),
                    RowKind::Dir => Some(Icon::ItemFolder),
                    RowKind::File | RowKind::Link => Some(Icon::ItemFile),
                    RowKind::Failed => Some(Icon::StateWarning),
                    RowKind::More => None,
                };
                div()
                    .id(("files-row", index))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .h(px(tokens.sidebar_row - 4.0))
                    .pl(px(tokens.spacing * 2.0 + row.depth as f32 * INDENT))
                    .pr(px(tokens.spacing * 2.0))
                    .mx(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .text_size(px(tokens.font_size))
                    .text_color(color)
                    .when(is_active, |s| s.font_weight(gpui::FontWeight::MEDIUM))
                    .when(selected, |s| s.bg(theme.sidebar_accent))
                    .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        window.focus(&this.focus, cx);
                        let double = event.click_count() >= 2;
                        let kind = this.rows().get(index).map(|r| r.kind);
                        match kind {
                            Some(RowKind::Dir) if double => {}
                            Some(_) => this.activate(index, double, window, cx),
                            None => {}
                        }
                    }))
                    .child(
                        div()
                            .flex_none()
                            .w(px(12.0))
                            .children(chevron.map(|c| icon(c, 12.0, theme.muted))),
                    )
                    .children(glyph.map(|g| div().flex_none().child(icon(g, 14.0, color))))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(row.kind == RowKind::Link, |s| s.italic())
                            .child(row.name.clone()),
                    )
                    .when(row.loading, |s| {
                        s.child(
                            div()
                                .flex_none()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.faint)
                                .child("…"),
                        )
                    })
                    .when(dotted, |s| {
                        s.child(
                            div()
                                .id(("files-git-dot", index))
                                .flex_none()
                                .size(px(6.0))
                                .rounded_full()
                                .bg(theme.diff_modified)
                                .tooltip(|_, cx| {
                                    cx.new(|_| {
                                        crate::workbench::style::TextTooltip(
                                            "Contains changes".into(),
                                        )
                                    })
                                    .into()
                                }),
                        )
                    })
                    .children(mark.map(|m| {
                        div()
                            .id(("files-git-mark", index))
                            .flex_none()
                            .w(px(12.0))
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(m.color(&theme))
                            .child(m.letter())
                            .tooltip(move |_, cx| {
                                cx.new(|_| crate::workbench::style::TextTooltip(m.words().into()))
                                    .into()
                            })
                    }))
                    .into_any_element()
            })
            .collect()
    }
}

impl Focusable for FilesPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let show_ignored = self.env.settings.get::<FilesPanelSettings>().show_ignored;
        if let Some(tree) = &mut self.tree {
            tree.set_show_ignored(show_ignored);
        }
        let eye = ui::IconButton::new(
            "files-show-ignored",
            Icon::StateReview,
            if show_ignored {
                "Hide ignored files"
            } else {
                "Show ignored files"
            },
            &theme,
            &tokens,
        )
        .compact()
        .toggled(show_ignored)
        .on_click(cx.listener(|this, _, _, cx| this.toggle_ignored(cx)))
        .into_any_element();
        let scope = self.scope(cx);
        let controls: Vec<AnyElement> = scope_chip(scope.as_ref(), "files", &self.env.regions, cx)
            .into_iter()
            .chain([eye])
            .collect();
        let header = panel_header_with("Files", "panels.files", controls, cx);
        let name: Option<SharedString> = self.root().and_then(|root| {
            root.file_name()
                .map(|n| SharedString::from(n.to_string_lossy().into_owned()))
        });
        let count = self.shown_rows().len();
        let body: AnyElement = match (&self.tree, count) {
            (None, _) => empty_state(
                "No project here",
                "Open a project (⌘O) to see its files.",
                &theme,
                &tokens,
            ),
            (Some(_), 0) if !self.settled() => empty_state("Listing…", "", &theme, &tokens),
            (Some(_), 0) => empty_state("Nothing here", "", &theme, &tokens),
            (Some(_), _) => uniform_list(
                "files-tree",
                count,
                cx.processor(|this, range: Range<usize>, _window, cx| this.render_rows(range, cx)),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h_0()
            .into_any_element(),
        };
        div()
            .id("files-panel")
            .key_context("FilesPanel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.fg)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .children(name.map(|name| {
                div()
                    .flex_none()
                    .px(px(tokens.spacing * 3.0))
                    .pb(px(tokens.spacing))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .truncate()
                    .child(name)
            }))
            .child(body)
    }
}

fn empty_state(
    title: &'static str,
    detail: &'static str,
    theme: &crate::theme::Theme,
    tokens: &crate::theme::Tokens,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(tokens.spacing))
        .px(px(tokens.spacing * 3.0))
        .py(px(tokens.spacing * 2.0))
        .text_size(px(tokens.font_size))
        .child(div().text_color(theme.muted).child(title))
        .when(!detail.is_empty(), |s| {
            s.child(
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.faint)
                    .child(detail),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(dir: &str, name: &str, kind: FsKind, ignored: bool) -> FsEntry {
        FsEntry {
            name: name.to_owned(),
            path: PathBuf::from(dir).join(name),
            kind,
            size: 0,
            mtime_ns: 0,
            ino: 0,
            ignored,
        }
    }

    fn listing(dir: &str, entries: Vec<FsEntry>) -> FsListing {
        FsListing {
            dir: PathBuf::from(dir),
            entries,
            truncated: false,
        }
    }

    fn names(tree: &Tree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    }

    #[test]
    fn folders_open_lazily_and_ignored_entries_hide_until_asked_for() {
        let mut tree = Tree::new(PathBuf::from("/repo"), false);
        assert!(tree.rows().is_empty(), "the root is not listed yet");
        tree.set_listing(
            Path::new("/repo"),
            listing(
                "/repo",
                vec![
                    entry("/repo", "src", FsKind::Dir, false),
                    entry("/repo", "target", FsKind::Dir, true),
                    entry("/repo", "Cargo.toml", FsKind::File, false),
                ],
            ),
        );
        assert_eq!(names(&tree), ["src", "Cargo.toml"]);
        assert!(tree.expand(Path::new("/repo/src")), "listed on first open");
        assert!(tree.rows()[0].loading);
        tree.set_listing(
            Path::new("/repo/src"),
            listing(
                "/repo/src",
                vec![entry("/repo/src", "main.rs", FsKind::File, false)],
            ),
        );
        assert_eq!(names(&tree), ["src", "  main.rs", "Cargo.toml"]);
        tree.set_show_ignored(true);
        assert_eq!(names(&tree), ["src", "  main.rs", "target", "Cargo.toml"]);
        assert!(tree.rows()[2].ignored);

        // Closing keeps the listing: opening again needs no new one.
        tree.collapse(Path::new("/repo/src"));
        assert_eq!(names(&tree), ["src", "target", "Cargo.toml"]);
        assert!(!tree.expand(Path::new("/repo/src")));
        assert_eq!(tree.parent_index(1), Some(0));
        assert_eq!(tree.index_of(Path::new("/repo/src/main.rs")), Some(1));
    }

    #[test]
    fn the_ancestors_of_a_file_run_from_the_root_down() {
        let tree = Tree::new(PathBuf::from("/repo"), false);
        assert_eq!(
            tree.ancestors_of(Path::new("/repo/src/bin/tool.rs")),
            Some(vec![
                PathBuf::from("/repo"),
                PathBuf::from("/repo/src"),
                PathBuf::from("/repo/src/bin"),
            ])
        );
        assert_eq!(
            tree.ancestors_of(Path::new("/repo/README.md")),
            Some(vec![PathBuf::from("/repo")])
        );
        assert_eq!(tree.ancestors_of(Path::new("/elsewhere/x.rs")), None);
    }

    #[test]
    fn closing_a_folder_closes_the_folders_inside_it() {
        let mut tree = Tree::new(PathBuf::from("/r"), false);
        tree.expand(Path::new("/r/a"));
        tree.expand(Path::new("/r/a/b"));
        tree.expand(Path::new("/r/c"));
        let mut closed = tree.collapse(Path::new("/r/a"));
        closed.sort();
        assert_eq!(closed, [PathBuf::from("/r/a"), PathBuf::from("/r/a/b")]);
        assert!(
            tree.collapse(Path::new("/r")).is_empty(),
            "the root stays open"
        );
        assert_eq!(
            tree.expanded_dirs(),
            [PathBuf::from("/r"), PathBuf::from("/r/c")]
        );
    }
}
