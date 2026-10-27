//! The `files` panel (E-13a, EX-1): the active project's folder as a tree, read through the daemon
//! (`fs.list`, one folder at a time when it is opened), so it works for a remote window too and
//! the daemon decides what may be read.
//!
//! - **Ignore-aware.** `fs.list` flags what `.gitignore` / `.ignore` rules match (and `.git`);
//!   the tree hides those unless `panels.files.show_ignored` (the header's ⋯) shows them, dimmed.
//!   Dotfiles hide with `panels.files.show_hidden` off.
//! - **Lazy and virtualized.** A folder is listed when it is opened; the rows are a flat list of
//!   what is open ([`Tree`], `files_tree.rs`: natural order, spliced per folder, folded
//!   single-folder chains), drawn with `uniform_list`, so a frame builds only the rows in view
//!   however many thousands are open. The list has a scrollbar.
//! - **Live.** Open folders are watched (`fs.watch`, at most [`MAX_WATCHED_DIRS`]): an
//!   `fs.changed` lists the folder it happened in again (at most four times a second per folder,
//!   [`RELIST_EVERY`]); an `fs.generation` of the root lists the open folders that are not
//!   watched; an overflow lists every open folder.
//! - **Git status** (FX-E2E2-C, `files_git`). A file with uncommitted changes shows a letter
//!   (`M` modified, `A` added and staged, `U` untracked, `D` deleted, `R` renamed) and a tint, a
//!   folder that holds changes a dot. The marks come from the file list of `worktree.diff`
//!   (no patches), read when the root is set, on `fs.changed` and when a checkout's activity
//!   ends, one read at a time and at most one every [`GIT_READ_EVERY`]; a frame only looks them
//!   up. A root outside git has none. A deleted file has no row to decorate; its folder still
//!   gets the dot.
//! - **Per root.** The tree of a root is kept (listings, open folders, selection, scroll) when
//!   the window works somewhere else, up to [`MAX_CACHED_ROOTS`] roots; the open folders of a root
//!   are also remembered across restarts (`desktop-state.json`, [`FilesMemory`]).
//! - **Keyboard.** ↑↓ move, Home / End / PgUp / PgDn jump, → opens a folder (or steps into it),
//!   ← closes it (or steps to its parent), ⌘→ opens every folder in view (not beyond 2 000 rows
//!   or ten folders still to list), ⌘← closes every folder, Return opens a file and moves the
//!   keyboard to it, Space shows it and keeps the keyboard in the tree, Esc hands the keyboard
//!   back to the editor. A click shows a file (the keyboard stays), a double click opens it.
//! - **Reveal.** The workbench reveals the focused tab's file (`panels.files.auto_reveal`, and
//!   *Files: Reveal Active File*): its folders open, its row is selected and scrolled to.
//! - **Operations** (EX-7). New file / New folder (the header's buttons, ⌘N / ⌥⌘N, the row menu)
//!   open a name field first inside the folder; F2 turns a row's name into the field with its stem
//!   selected (`edit.rs`). ⌘⌫ asks (a sheet that lists what goes, the tabs with unsaved changes and
//!   an agent working there) and moves to the Trash; ⌘D duplicates (`name copy`); ⌘C / ⌘X / ⌘V
//!   copy, cut and paste, with the tree's own clipboard and the files Finder copied; a row dragged
//!   to a folder moves there (⌥ copies), the folder under the pointer opens after a moment and
//!   the tree scrolls at its edges (`drag.rs`); ⌘Z / ⇧⌘Z undo and redo, a batch is one step
//!   (`ops.rs`). Every change is a request to the daemon, which never overwrites; the workbench
//!   re-keys the tabs of what was renamed or moved and marks the tabs of what went to the Trash.

mod drag;
mod edit;
mod ops;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, ClickEvent, Context, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Pixels, Point, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, Task, UniformListScrollHandle, Window, div, prelude::*, px,
    uniform_list,
};
use hutan_client::{ClientError, Notice};
use hutan_protocol::fs::{FsList, FsListing};
use hutan_protocol::worktree::WorktreeDiffRequest;
use tokio::sync::broadcast::error::RecvError;

use super::common::{panel_toolbar, scope_chip};
use super::files_git::GitStatus;
use super::files_names::Journal;
use super::files_tree::{ExpandRefusal, file_icon};
pub use super::files_tree::{Row, RowKind, Tree, TreeOptions};
use crate::icons::{Icon, icon};
use crate::sessions::CheckoutActivity;
use crate::settings::{FilesPanelSettings, IndentGuides};
use crate::theme::DensityExt as _;
use crate::workbench::Region;
use crate::workbench::actions::{
    ChosenFile, FilesMenuAt, FocusEditor, Notify, OpenChosenFile, ShowFilesMenu, ShowFilesMoreMenu,
};
use crate::workbench::files::FilePlace;
use crate::workbench::persistence::{self, FilesExpanded};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::{TextTooltip, UiStyle};
use crate::workbench::ui;
use crate::workbench::vim_ui::FieldEdit;
pub use drag::{
    AUTOSCROLL_EVERY as FILES_AUTOSCROLL_EVERY, DraggedRows, SPRING_AFTER as FILES_SPRING_AFTER,
};
use edit::NameEdit;
pub use edit::{EditKind, NameEditView};
pub use ops::FilesCommand;
use ops::{PathClipboard, error_message};

/// Open folders watched at once; folders opened beyond it are listed again on the root's
/// `fs.generation` instead (the connection may watch 256 paths, open files included).
pub const MAX_WATCHED_DIRS: usize = 64;
/// A folder is listed again at most this often while its files change (a burst of `fs.changed`
/// is one listing now and one after this).
pub const RELIST_EVERY: Duration = Duration::from_millis(250);
/// The git marks are read at most this often.
pub const GIT_READ_EVERY: Duration = Duration::from_secs(2);
/// Roots whose tree is kept while the window works elsewhere.
pub const MAX_CACHED_ROOTS: usize = 8;
/// Indentation per level down to [`INDENT_FULL_LEVELS`], then [`INDENT_DEEP`], never more than
/// [`INDENT_MAX`] in all: depth never takes the width of the body.
const INDENT: f32 = 12.0;
const INDENT_FULL_LEVELS: usize = 6;
const INDENT_DEEP: f32 = 6.0;
const INDENT_MAX: f32 = 100.0;

/// How far in from the row's left edge level `depth` starts.
fn indent_px(depth: usize) -> f32 {
    let full = depth.min(INDENT_FULL_LEVELS) as f32 * INDENT;
    let deep = depth.saturating_sub(INDENT_FULL_LEVELS) as f32 * INDENT_DEEP;
    (full + deep).min(INDENT_MAX)
}

/// The open folders the panel remembers per root, shared with the workbench that saves them in
/// `desktop-state.json` (`files_expanded`). Cheap to clone.
#[derive(Clone)]
pub struct FilesMemory {
    roots: Rc<RefCell<FilesExpanded>>,
    changed: Rc<dyn Fn(&mut App)>,
}

impl std::fmt::Debug for FilesMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesMemory")
            .field("roots", &self.roots.borrow().len())
            .finish_non_exhaustive()
    }
}

impl FilesMemory {
    /// The remembered folders `initial`; `changed` runs (deferred by the caller if it has to)
    /// whenever the panel changed them, so the owner saves.
    pub fn new(initial: FilesExpanded, changed: impl Fn(&mut App) + 'static) -> Self {
        Self {
            roots: Rc::new(RefCell::new(initial)),
            changed: Rc::new(changed),
        }
    }

    /// A memory nobody saves.
    pub fn detached() -> Self {
        Self::new(FilesExpanded::new(), |_| {})
    }

    /// The open folders remembered for `root`, shallowest first.
    pub fn dirs_of(&self, root: &Path) -> Vec<PathBuf> {
        let key = root.display().to_string();
        self.roots
            .borrow()
            .iter()
            .find(|r| r.root == key)
            .map(|r| r.dirs.iter().map(PathBuf::from).collect())
            .unwrap_or_default()
    }

    /// What is remembered, for saving.
    pub fn snapshot(&self) -> FilesExpanded {
        self.roots.borrow().clone()
    }

    fn remember(&self, root: &Path, dirs: Vec<String>, cx: &mut App) {
        let changed = persistence::note_files_expanded(
            &mut self.roots.borrow_mut(),
            &root.display().to_string(),
            dirs,
        );
        if changed {
            (self.changed)(cx);
        }
    }
}

/// The tree of a root the window left, kept for when it comes back.
struct CachedRoot {
    tree: Tree,
    selected: Option<PathBuf>,
    offset: Point<Pixels>,
}

/// The Files panel.
pub struct FilesPanel {
    env: PanelEnv,
    focus: FocusHandle,
    tree: Option<Tree>,
    /// The root before the last switch of container: its cached tree is drawn (not interactive)
    /// until the new root's first listing arrives, so a switch never flashes an empty panel.
    stale_root: Option<PathBuf>,
    /// Trees of the roots the window left, most recent first, at most [`MAX_CACHED_ROOTS`].
    cache: Vec<CachedRoot>,
    selected: Option<PathBuf>,
    /// The file of the focused tab, as last revealed.
    active_file: Option<PathBuf>,
    /// A file to select once the folders it is in are listed.
    pending_reveal: Option<PathBuf>,
    scroll: UniformListScrollHandle,
    bar: ui::Scrollbar,
    /// Where the scrollbar's thumb was painted last frame (tests); `None` while everything fits.
    thumb: Rc<Cell<Option<gpui::Bounds<Pixels>>>>,
    /// The pointer is over the tree (indent guides on hover).
    hovered: bool,
    /// `fs.list` in flight, per folder; a newer one replaces (cancels) the older.
    listing: HashMap<PathBuf, Task<()>>,
    /// Open folders remembered from the last run that are not listed yet: one that cannot be
    /// listed any more is closed quietly instead of showing an error.
    restored: HashSet<PathBuf>,
    /// The open folders watched for this panel (through the store's shared watches).
    watched: HashSet<PathBuf>,
    listener: Option<Task<()>>,
    /// Folders an `fs.changed` touched that are not listed again yet.
    dirty: HashSet<PathBuf>,
    /// While set, no folder is listed again for a change: what changes meanwhile waits for it.
    relist_cooldown: Option<Task<()>>,
    /// Rows built so far (tests: only the rows in view are).
    rows_rendered: Rc<Cell<usize>>,
    /// Indent guides drawn so far (tests).
    guides_drawn: Rc<Cell<usize>>,
    /// Listings answered so far (tests wait on it).
    listings_done: usize,
    /// Listings asked for so far (tests count them).
    lists_issued: usize,
    /// The git marks of the root (`worktree.diff`, the file list), refreshed on `fs.changed`
    /// and when a checkout's activity ends; a frame only looks them up.
    git: GitStatus,
    /// A `worktree.diff` is in flight; a trigger meanwhile asks for one more afterwards.
    git_task: Option<Task<()>>,
    git_again: bool,
    /// While set, no new read starts: a trigger meanwhile asks for one more when it ends.
    git_cooldown: Option<Task<()>>,
    /// The root and connection state the git status was last asked for: it is asked again when
    /// either changes (a window that connects after the root was set has no marks yet).
    git_asked: Option<(PathBuf, hutan_client::ConnectionState)>,
    /// Reads of the git status answered so far (tests wait on it).
    git_reads: usize,
    /// The name field of a new file, a new folder or a rename, when one is open.
    editing: Option<NameEdit>,
    name_edit: FieldEdit,
    name_focus: FocusHandle,
    /// A path to select (and, with `true`, to rename) once its folder is listed again.
    follow: Option<(PathBuf, bool)>,
    /// A path whose name field opens at the next frame (its row was not listed when asked).
    rename_when_listed: Option<PathBuf>,
    /// After a row went (Trash), the row to select in its place once the folder is listed
    /// again: `(folder, row index)`.
    hint: Option<(PathBuf, usize)>,
    /// What ⌘C and ⌘X remembered.
    clipboard: Option<PathClipboard>,
    /// The undo and redo stacks of this window's file operations.
    journal: Journal,
    /// Operations whose answer is on its way.
    ops: HashMap<u64, Task<()>>,
    next_op: u64,
    ops_done: usize,
    /// The sheet of the first drag was confirmed this session.
    drag_confirmed: bool,
    /// The folder rows are being dragged over, where its row is, and the wait to open it.
    spring: Option<(PathBuf, gpui::Bounds<Pixels>, Option<Task<()>>)>,
    /// Pixels per tick the tree scrolls while a drag is near its edge (0: not scrolling).
    edge_speed: f32,
    autoscroll: Option<Task<()>>,
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
        let scroll = UniformListScrollHandle::new();
        let thumb = Rc::new(Cell::new(None));
        let bar = ui::Scrollbar::new(
            scroll.0.borrow().base_handle.clone(),
            ui::ScrollAxis::Vertical,
        )
        .on_paint({
            let thumb = Rc::clone(&thumb);
            move |bounds, _| thumb.set(Some(bounds))
        });
        let mut panel = Self {
            env,
            focus: cx.focus_handle(),
            tree: None,
            stale_root: None,
            cache: Vec::new(),
            selected: None,
            active_file: None,
            pending_reveal: None,
            scroll,
            bar,
            thumb,
            hovered: false,
            listing: HashMap::new(),
            restored: HashSet::new(),
            watched: HashSet::new(),
            listener: None,
            dirty: HashSet::new(),
            relist_cooldown: None,
            rows_rendered: Rc::new(Cell::new(0)),
            guides_drawn: Rc::new(Cell::new(0)),
            listings_done: 0,
            lists_issued: 0,
            git: GitStatus::default(),
            git_task: None,
            git_again: false,
            git_cooldown: None,
            git_asked: None,
            git_reads: 0,
            editing: None,
            name_edit: FieldEdit::new(),
            name_focus: cx.focus_handle(),
            follow: None,
            rename_when_listed: None,
            hint: None,
            clipboard: None,
            journal: Journal::default(),
            ops: HashMap::new(),
            next_op: 0,
            ops_done: 0,
            drag_confirmed: false,
            spring: None,
            edge_speed: 0.0,
            autoscroll: None,
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

    /// The tree itself.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// The rows the tree shows now, top to bottom.
    pub fn rows(&self) -> &[Row] {
        self.tree.as_ref().map_or(&[], Tree::rows)
    }

    /// The rows to draw: the tree's, or while a switch of container waits for its first listing
    /// the ones of the tree before it.
    fn shown_rows(&self) -> &[Row] {
        match (&self.stale_root, self.rows()) {
            (Some(root), []) => self
                .cache
                .iter()
                .find(|c| c.tree.root() == root)
                .map_or(&[], |c| c.tree.rows()),
            (_, rows) => rows,
        }
    }

    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// Selects the row of `path` when the tree shows it (without opening anything).
    pub fn select_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(index) = self.tree.as_ref().and_then(|t| t.index_of(path)) {
            self.select_index(index);
            cx.notify();
        }
    }

    /// The file of the focused tab, as last revealed.
    pub fn active_file(&self) -> Option<&Path> {
        self.active_file.as_deref()
    }

    /// Rows built for frames so far.
    pub fn rows_rendered(&self) -> usize {
        self.rows_rendered.get()
    }

    /// Indent guides drawn for frames so far.
    pub fn guides_drawn(&self) -> usize {
        self.guides_drawn.get()
    }

    /// `fs.list` answers taken so far.
    pub fn listings_done(&self) -> usize {
        self.listings_done
    }

    /// `fs.list` requests made so far.
    pub fn lists_issued(&self) -> usize {
        self.lists_issued
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

    /// No git status read is in flight or waiting.
    pub fn git_settled(&self) -> bool {
        self.git_task.is_none() && !self.git_again
    }

    /// The folders watched for the tree.
    pub fn watched_dirs(&self) -> usize {
        self.watched.len()
    }

    /// The roots whose tree is kept, most recent first.
    pub fn cached_roots(&self) -> Vec<PathBuf> {
        self.cache
            .iter()
            .map(|c| c.tree.root().to_owned())
            .collect()
    }

    /// Whether the keyboard is in the tree.
    pub fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// Where the scrollbar's thumb was painted in the last frame; `None` while the rows fit.
    pub fn scrollbar_thumb(&self) -> Option<gpui::Bounds<Pixels>> {
        self.thumb.get()
    }

    /// Whether the indent guides are drawn at this moment.
    pub fn guides_shown(&self) -> bool {
        match self.env.settings.get::<FilesPanelSettings>().indent_guides {
            IndentGuides::Always => true,
            IndentGuides::Hover => self.hovered,
            IndentGuides::Off => false,
        }
    }

    // ---- the root ---------------------------------------------------------------------------

    fn tree_options(&self) -> TreeOptions {
        let settings = self.env.settings.get::<FilesPanelSettings>();
        TreeOptions {
            show_ignored: settings.show_ignored,
            show_hidden: settings.show_hidden,
            compact: settings.compact_folders,
        }
    }

    /// Follows the project the window works in: a new root starts a tree (or brings its kept
    /// one back).
    fn follow_root(&mut self, cx: &mut Context<Self>) {
        let root = self.env.sessions.read(cx).files_root();
        if root.as_deref() == self.root() {
            return;
        }
        self.unwatch_all(cx);
        self.listing.clear();
        self.dirty.clear();
        self.relist_cooldown = None;
        self.restored.clear();
        self.git = GitStatus::default();
        self.git_task = None;
        self.git_again = false;
        self.git_cooldown = None;
        self.git_asked = None;
        self.pending_reveal = None;
        self.editing = None;
        self.follow = None;
        self.rename_when_listed = None;
        self.hint = None;
        self.spring = None;
        self.stale_root = None;
        let offset = self.scroll.0.borrow().base_handle.offset();
        let selected = self.selected.take();
        if let Some(tree) = self.tree.take() {
            // The tree that leaves is drawn until the new root's first listing, when there is a
            // new root and the old one showed something.
            if root.is_some() && !tree.rows().is_empty() {
                self.stale_root = Some(tree.root().to_owned());
            }
            self.cache.retain(|c| c.tree.root() != tree.root());
            self.cache.insert(
                0,
                CachedRoot {
                    tree,
                    selected,
                    offset,
                },
            );
            self.cache.truncate(MAX_CACHED_ROOTS);
        }
        let options = self.tree_options();
        let mut offset = Point::default();
        self.tree = root.map(|root| {
            if let Some(i) = self.cache.iter().position(|c| c.tree.root() == root) {
                // Back to a root seen before: its rows are drawn at once, every open folder is
                // listed again.
                let CachedRoot {
                    mut tree,
                    selected,
                    offset: kept,
                } = self.cache.remove(i);
                tree.set_options(options);
                self.selected = selected;
                offset = kept;
                self.stale_root = None;
                tree
            } else {
                let dirs = self.env.files_memory.dirs_of(&root);
                self.restored = dirs.iter().cloned().collect();
                Tree::with_expanded(root, options, dirs)
            }
        });
        self.scroll.0.borrow().base_handle.set_offset(offset);
        match self.tree.as_ref().map(|t| t.expanded_dirs()) {
            Some(dirs) => {
                self.listen(cx);
                for dir in dirs {
                    self.list(&dir, cx);
                    self.watch(&dir, cx);
                }
                if let Some(file) = self.active_file.clone() {
                    self.reveal(&file, cx);
                }
            }
            None => self.listener = None,
        }
        cx.notify();
    }

    /// Takes in what the settings say of the tree (the header's ⋯ writes them): the rows are
    /// built again when a filter or the folding changed.
    fn sync_options(&mut self, cx: &mut Context<Self>) {
        let options = self.tree_options();
        let Some(tree) = &mut self.tree else {
            return;
        };
        if tree.options() == options {
            return;
        }
        tree.set_options(options);
        self.after_tree_change(cx);
    }

    /// After the tree changed under its own power: lists and watches the folders folding opened,
    /// keeps the selection on a row that exists and remembers the open folders.
    fn after_tree_change(&mut self, cx: &mut Context<Self>) {
        let opened = self
            .tree
            .as_mut()
            .map(Tree::take_opened)
            .unwrap_or_default();
        for dir in opened {
            self.list(&dir, cx);
            self.watch(&dir, cx);
        }
        self.keep_selection();
        self.remember(cx);
    }

    /// Hands the open folders to the memory that survives a restart.
    fn remember(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = &self.tree else {
            return;
        };
        let dirs: Vec<String> = tree
            .expanded_dirs()
            .into_iter()
            .filter(|d| d != tree.root())
            .map(|d| d.display().to_string())
            .collect();
        self.env.files_memory.remember(tree.root(), dirs, cx);
    }

    // ---- listing and watching -------------------------------------------------------------

    fn list(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        tree.set_loading(dir);
        self.lists_issued += 1;
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
        let root = tree.root().to_owned();
        let mut gone = Vec::new();
        match answer {
            Ok(listing) => {
                self.restored.remove(dir);
                gone = tree.set_listing(dir, listing);
            }
            // A folder remembered from the last run that is not there any more is closed
            // without a word.
            Err(_) if dir != root && self.restored.remove(dir) => gone = tree.forget(dir),
            Err(error) => tree.set_failed(dir, crate::workbench::files::error_text(&error)),
        }
        for dir in gone {
            self.unwatch(&dir, cx);
        }
        if root == dir {
            self.stale_root = None;
        }
        if let Some(file) = self.pending_reveal.clone() {
            self.reveal(&file, cx);
        }
        self.apply_follow();
        if let Some((folder, index)) = self.hint.clone()
            && folder == dir
        {
            self.hint = None;
            let selected_is_row = self
                .selected
                .as_deref()
                .is_some_and(|s| self.tree.as_ref().is_some_and(|t| t.index_of(s).is_some()));
            let count = self.rows().len();
            if !selected_is_row && count > 0 {
                self.select_index(index.min(count - 1));
            }
        }
        self.after_tree_change(cx);
        cx.notify();
    }

    /// Lists the open folders again, shallowest first (the Refresh button, *Files: Refresh*),
    /// and reads the git marks now.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = &self.tree else {
            return;
        };
        for dir in tree.expanded_dirs() {
            self.list(&dir, cx);
        }
        self.git_again = false;
        self.git_cooldown = None;
        if self.git_task.is_some() {
            self.git_again = true;
        } else {
            self.start_git_read(cx);
        }
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

    /// Reads which files of the root are changed (`worktree.diff` without patches): one read at
    /// a time, at most one every [`GIT_READ_EVERY`]; a trigger meanwhile is one read when the
    /// time is up. A root outside git, or a daemon without worktree review, has no marks.
    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        if self.tree.is_none() {
            return;
        }
        if self.git_task.is_some() || self.git_cooldown.is_some() {
            self.git_again = true;
            return;
        }
        self.start_git_read(cx);
    }

    fn start_git_read(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.tree.as_ref().map(|t| t.root().to_owned()) else {
            return;
        };
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
        self.git_cooldown = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(GIT_READ_EVERY).await;
            this.update(cx, |this, cx| {
                this.git_cooldown = None;
                if this.git_task.is_none() && std::mem::take(&mut this.git_again) {
                    this.refresh_git(cx);
                }
            })
            .ok();
        }));
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
                if this.git_cooldown.is_none() && std::mem::take(&mut this.git_again) {
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

    /// An `fs.changed` or `fs.generation`: the open folders it touched are listed again (not
    /// more than every [`RELIST_EVERY`]) and the git marks read again (not more than every
    /// [`GIT_READ_EVERY`]).
    pub fn on_notice(&mut self, notice: Notice, cx: &mut Context<Self>) {
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
        self.relist(again, cx);
        self.refresh_git(cx);
    }

    /// Lists `dirs` again: now when nothing was listed in the last [`RELIST_EVERY`], else with
    /// whatever else changed in the meantime when that time is up.
    fn relist(&mut self, dirs: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.dirty.extend(dirs);
        if self.relist_cooldown.is_none() {
            self.flush_relist(cx);
        }
    }

    fn flush_relist(&mut self, cx: &mut Context<Self>) {
        let dirs: Vec<PathBuf> = std::mem::take(&mut self.dirty).into_iter().collect();
        let open: Vec<PathBuf> = match &self.tree {
            Some(tree) => dirs.into_iter().filter(|d| tree.is_expanded(d)).collect(),
            None => Vec::new(),
        };
        if open.is_empty() {
            return;
        }
        for dir in open {
            self.list(&dir, cx);
        }
        self.relist_cooldown = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RELIST_EVERY).await;
            this.update(cx, |this, cx| {
                this.relist_cooldown = None;
                if !this.dirty.is_empty() {
                    this.flush_relist(cx);
                }
            })
            .ok();
        }));
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
            self.after_tree_change(cx);
        } else {
            self.open_dir(path, cx);
        }
        cx.notify();
    }

    fn open_dir(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        tree.expand(path);
        // A folder closed for a while is listed again: what shows meanwhile is what it held.
        self.list(path, cx);
        self.watch(path, cx);
        self.after_tree_change(cx);
    }

    /// Closes every folder but the root (⌘←, the header's button, *Files: Collapse All*).
    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        for dir in tree.collapse_all() {
            self.unwatch(&dir, cx);
        }
        self.after_tree_change(cx);
        cx.notify();
    }

    /// Opens every folder in view (⌘→, the header's ⋯ menu); says why when it would be too much.
    pub fn expand_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        match tree.expand_all() {
            Ok(unlisted) => {
                let open = tree.expanded_dirs();
                for dir in unlisted {
                    self.list(&dir, cx);
                }
                for dir in open {
                    self.watch(&dir, cx);
                }
                self.after_tree_change(cx);
                cx.notify();
            }
            Err(refusal) => {
                let why = match refusal {
                    ExpandRefusal::TooManyRows => {
                        "Expand all would show more than 2 000 rows: open the folders you need"
                    }
                    ExpandRefusal::TooManyUnlisted => {
                        "Expand all would have to list more than 10 folders first: open some, then try again"
                    }
                };
                window.dispatch_action(Box::new(Notify(why.to_owned())), cx);
            }
        }
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

    /// How many rows a page holds: what PgUp / PgDn move by.
    fn page_rows(&self, cx: &App) -> usize {
        let row = UiStyle::get(cx).density.ui().sidebar_row - 4.0;
        let height: f32 = self
            .scroll
            .0
            .borrow()
            .base_handle
            .bounds()
            .size
            .height
            .into();
        ((height / row.max(1.0)) as usize).saturating_sub(1).max(1)
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.editing.is_some() {
            // The name field has the keyboard; what it does not use is nobody's.
            return false;
        }
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.platform && !(modifiers.control || modifiers.alt || modifiers.shift) {
            match keystroke.key.as_str() {
                "left" => self.collapse_all(cx),
                "right" => self.expand_all(window, cx),
                _ => return false,
            }
            self.keep_selection();
            cx.notify();
            return true;
        }
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.shift {
            return false;
        }
        if keystroke.key == "escape" {
            // The keyboard goes back to what the focused tab shows; the panel stays open.
            window.dispatch_action(Box::new(FocusEditor), cx);
            return true;
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
            "home" => self.select_index(0),
            "end" => self.select_index(count - 1),
            "pagedown" => {
                let page = self.page_rows(cx);
                self.select_index(
                    current.map_or(page.min(count - 1), |i| (i + page).min(count - 1)),
                );
            }
            "pageup" => {
                let page = self.page_rows(cx);
                self.select_index(current.map_or(0, |i| i.saturating_sub(page)));
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

    /// Writes one `panels.files.*` setting (the header's ⋯) and takes it in.
    pub fn set_option(&mut self, key: &str, value: toml::Value, cx: &mut Context<Self>) {
        if let Err(err) = self
            .env
            .settings
            .write_user(&format!("panels.files.{key}"), value)
        {
            tracing::warn!("cannot save panels.files.{key}: {err}");
        }
        self.sync_options(cx);
        cx.notify();
    }

    // ---- rendering --------------------------------------------------------------------------

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let hover = theme.sidebar_hover;
        let focused_row = self.selected.clone();
        let active = self.active_file.clone();
        let guides = self.guides_shown();
        let guide_color = theme.faint.opacity(0.35);
        // The new entry's field is a row of its own among the rows drawn: the tree's rows after
        // it are one further down the list than they are in the tree.
        let slot = self.new_row_slot();
        let rows: Vec<(usize, Row)> = range
            .clone()
            .filter(|v| Some(*v) != slot.map(|s| s.0))
            .map(|v| {
                if slot.is_some_and(|s| v > s.0) {
                    v - 1
                } else {
                    v
                }
            })
            .filter_map(|i| self.shown_rows().get(i).cloned().map(|row| (i, row)))
            .collect();
        self.rows_rendered
            .set(self.rows_rendered.get() + rows.len());
        let drawn = Rc::clone(&self.guides_drawn);
        let accent = theme.accent;
        let mut elements: Vec<AnyElement> = rows
            .into_iter()
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
                    RowKind::File => Some(file_icon(&row.name)),
                    RowKind::Link => Some(Icon::ItemFile),
                    RowKind::Failed => Some(Icon::StateWarning),
                    RowKind::More => None,
                };
                let lead = tokens.spacing * 2.0;
                // The tooltip is the whole path: a folded chain ends in its last folder.
                let tip: Option<SharedString> =
                    matches!(row.kind, RowKind::Dir | RowKind::File | RowKind::Link)
                        .then(|| row.folder().display().to_string().into());
                // A folded chain: the folders in front are muted and give way first, the last
                // name stays whole.
                let name: AnyElement = match row.name.rsplit_once('/') {
                    Some((front, last)) if row.tail.is_some() => div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.muted)
                                .child(SharedString::from(format!("{front}/"))),
                        )
                        .child(div().flex_none().child(SharedString::from(last.to_owned())))
                        .into_any_element(),
                    _ => div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(row.kind == RowKind::Link, |s| s.italic())
                        .child(row.name.clone())
                        .into_any_element(),
                };
                // A rename turns the name (the last folder of a folded chain) into the field.
                let name: AnyElement = if self.renaming(row.folder()) {
                    let editor = self.name_editor(cx);
                    match row.name.rsplit_once('/') {
                        Some((front, _)) if row.tail.is_some() => div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .child(
                                div()
                                    .flex_none()
                                    .max_w_1_2()
                                    .truncate()
                                    .text_color(theme.muted)
                                    .child(SharedString::from(format!("{front}/"))),
                            )
                            .child(editor)
                            .into_any_element(),
                        _ => editor,
                    }
                } else {
                    name
                };
                let cut = self.is_cut(row.folder());
                let is_dir = row.kind == RowKind::Dir;
                let can_drag = !matches!(row.kind, RowKind::More | RowKind::Failed);
                let target = row.folder().to_owned();
                let dragged = self.dragged_by(index);
                let drop_target = target.clone();
                let drop_listener = cx.listener(move |this, rows: &DraggedRows, window, cx| {
                    let copy = window.modifiers().alt;
                    this.dropped(
                        rows,
                        Some((drop_target.as_path(), is_dir)),
                        copy,
                        window,
                        cx,
                    );
                });
                let drag_listener = cx.listener(
                    move |this, event: &gpui::DragMoveEvent<DraggedRows>, window, cx| {
                        if event.bounds.contains(&event.event.position) {
                            let over = is_dir.then(|| (target.clone(), event.bounds));
                            this.drag_over_row(over, window, cx);
                        }
                    },
                );
                div()
                    .id(("files-row", index))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .h(px(tokens.sidebar_row - 4.0))
                    .pl(px(lead + indent_px(row.depth)))
                    .pr(px(tokens.spacing * 2.0))
                    .mx(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .text_size(px(tokens.font_size))
                    .text_color(color)
                    .when(is_active, |s| s.font_weight(gpui::FontWeight::MEDIUM))
                    .when(cut, |s| s.opacity(0.5))
                    .when(selected, |s| s.bg(theme.sidebar_accent))
                    .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
                    .when(guides, |row_el| {
                        // One line per open ancestor, under the middle of its chevron.
                        let mut row_el = row_el;
                        for level in 0..row.depth {
                            drawn.set(drawn.get() + 1);
                            row_el = row_el.child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left(px(lead + indent_px(level) + 6.0))
                                    .w(px(1.0))
                                    .bg(guide_color),
                            );
                        }
                        row_el
                    })
                    .when_some(tip, |s, tip| {
                        s.tooltip(move |_, cx| cx.new(|_| TextTooltip(tip.clone())).into())
                    })
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            let Some(row) = this.rows().get(index).cloned() else {
                                return;
                            };
                            if matches!(row.kind, RowKind::More | RowKind::Failed) {
                                return;
                            }
                            this.selected = Some(row.path.clone());
                            window.dispatch_action(
                                Box::new(ShowFilesMenu(FilesMenuAt {
                                    path: row.path.display().to_string(),
                                    file: matches!(row.kind, RowKind::File | RowKind::Link),
                                    position: event.position,
                                })),
                                cx,
                            );
                            cx.notify();
                        }),
                    )
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
                    .when_some(dragged, |s, payload| {
                        s.on_drag(payload, |dragged: &DraggedRows, _, _, cx| {
                            let style = UiStyle::get(cx);
                            let label = dragged.label.clone();
                            cx.new(|_| crate::workbench::view::DragPreview { label, style })
                        })
                    })
                    .when(is_dir, |s| {
                        s.drag_over::<DraggedRows>(move |s, _, _, _| s.bg(accent.opacity(0.18)))
                    })
                    .when(can_drag, |s| {
                        s.on_drop(drop_listener)
                            .on_drag_move::<DraggedRows>(drag_listener)
                    })
                    .child(
                        div()
                            .flex_none()
                            .w(px(12.0))
                            .children(chevron.map(|c| icon(c, 12.0, theme.muted))),
                    )
                    .children(glyph.map(|g| div().flex_none().child(icon(g, 14.0, color))))
                    .child(name)
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
                                    cx.new(|_| TextTooltip("Contains changes".into())).into()
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
                            .tooltip(move |_, cx| cx.new(|_| TextTooltip(m.words().into())).into())
                    }))
                    .into_any_element()
            })
            .collect();
        if let Some((at, depth)) = slot
            && let Some(position) = at.checked_sub(range.start)
            && position <= elements.len()
        {
            let folder = self.editing.as_ref().is_some_and(|e| e.is_dir_entry());
            let glyph = if folder {
                Icon::ItemFolder
            } else {
                Icon::ItemFile
            };
            let lead = tokens.spacing * 2.0;
            let row = div()
                .id("files-new-row")
                .flex()
                .items_center()
                .gap(px(4.0))
                .h(px(tokens.sidebar_row - 4.0))
                .pl(px(lead + indent_px(depth)))
                .pr(px(tokens.spacing * 2.0))
                .mx(px(tokens.spacing))
                .child(div().flex_none().w(px(12.0)))
                .child(div().flex_none().child(icon(glyph, 14.0, theme.muted)))
                .child(self.name_editor(cx))
                .into_any_element();
            elements.insert(position, row);
        }
        elements
    }
}

impl Focusable for FilesPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FilesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        self.sync_options(cx);
        if let Some(path) = self.rename_when_listed.take() {
            self.begin_rename_of(path, window, cx);
        }
        self.keep_edit_visible();
        self.thumb.set(None);
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let new_file = ui::IconButton::new(
            "files-new-file",
            Icon::ActionNewFile,
            "New File",
            &theme,
            &tokens,
        )
        .compact()
        .on_click(cx.listener(|this, _, window, cx| this.begin_new(false, window, cx)))
        .into_any_element();
        let new_folder = ui::IconButton::new(
            "files-new-folder",
            Icon::ActionNewFolder,
            "New Folder",
            &theme,
            &tokens,
        )
        .compact()
        .on_click(cx.listener(|this, _, window, cx| this.begin_new(true, window, cx)))
        .into_any_element();
        let refresh = ui::IconButton::new(
            "files-refresh",
            Icon::ActionRefresh,
            "Refresh the tree",
            &theme,
            &tokens,
        )
        .compact()
        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
        .into_any_element();
        let collapse = ui::IconButton::new(
            "files-collapse-all",
            Icon::ActionCollapseAll,
            "Collapse all folders",
            &theme,
            &tokens,
        )
        .compact()
        .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx)))
        .into_any_element();
        let more = ui::IconButton::new("files-more", Icon::ActionMore, "More", &theme, &tokens)
            .compact()
            .on_click(|event: &ClickEvent, window, cx| {
                window.dispatch_action(Box::new(ShowFilesMoreMenu(event.position())), cx);
            })
            .into_any_element();
        let scope = self.scope(cx);
        let chip = scope_chip(scope.as_ref(), "files", &self.env.regions, cx);
        // The chip names the project: the root's name is said under it only when there is no
        // chip (a folder that is not a project), never twice.
        let name_shown = chip.is_none();
        let lead: Vec<AnyElement> = chip.into_iter().collect();
        // The dock tab says Files: the header keeps the scope chip and the controls.
        let header = panel_toolbar(
            "panels.files",
            lead,
            vec![new_file, new_folder, refresh, collapse, more],
            cx,
        );
        let name: Option<SharedString> = self.root().and_then(|root| {
            root.file_name()
                .map(|n| SharedString::from(n.to_string_lossy().into_owned()))
        });
        let count = self.shown_rows().len() + usize::from(self.new_row_slot().is_some());
        let body: AnyElement = match (&self.tree, count) {
            (None, _) => empty_state(
                "No project here",
                "Open a project (⌘O) to see its files.",
                &theme,
                &tokens,
            ),
            (Some(_), 0) if !self.settled() => empty_state("Listing…", "", &theme, &tokens),
            (Some(_), 0) => empty_state("Nothing here", "", &theme, &tokens),
            (Some(_), _) => div()
                .id("files-tree-area")
                .relative()
                .flex_1()
                .min_h_0()
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    if this.hovered != *hovered {
                        this.hovered = *hovered;
                        if this.env.settings.get::<FilesPanelSettings>().indent_guides
                            == IndentGuides::Hover
                        {
                            cx.notify();
                        }
                    }
                }))
                .on_drag_move::<DraggedRows>(cx.listener(
                    |this, event: &gpui::DragMoveEvent<DraggedRows>, window, cx| {
                        this.drag_near_edge(event.event.position, event.bounds, window, cx);
                    },
                ))
                // Rows dropped on the empty part of the tree go to its root.
                .on_drop(cx.listener(|this, rows: &DraggedRows, window, cx| {
                    let copy = window.modifiers().alt;
                    this.dropped(rows, None, copy, window, cx);
                }))
                .child(
                    uniform_list(
                        "files-tree",
                        count,
                        cx.processor(|this, range: Range<usize>, _window, cx| {
                            this.render_rows(range, cx)
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full(),
                )
                .child(self.bar.element(&theme))
                .child(probe(&self.env.regions, Region::Files("tree")))
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
            .children(name.filter(|_| name_shown).map(|name| {
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
            .children(self.name_error_strip(cx))
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

    #[test]
    fn indentation_is_twelve_pixels_a_level_then_six_and_never_over_a_hundred() {
        assert_eq!(indent_px(0), 0.0);
        assert_eq!(indent_px(1), 12.0);
        assert_eq!(indent_px(6), 72.0);
        assert_eq!(indent_px(7), 78.0);
        assert_eq!(indent_px(10), 96.0);
        assert_eq!(indent_px(11), 100.0, "the cap");
        assert_eq!(indent_px(50), 100.0);
    }
}
