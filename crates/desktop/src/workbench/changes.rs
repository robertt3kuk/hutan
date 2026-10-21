//! The Changes view (M7-C): what a worktree (or the main checkout) changed against its base,
//! read-only, with a file list and a unified diff per file, and — for a worktree — the merge
//! into the main checkout and the offer to remove the worktree afterwards.
//!
//! Everything comes from the daemon: `worktree.diff` (the file list, against the merge base of
//! the base branch and `HEAD`, uncommitted changes togglable), `worktree.file_diff` (one file's
//! patch, fetched when it is selected) and `worktree.merge`. Patches are parsed with
//! `hutan_chat_view::diff` and drawn with the chat's diff rows; each hunk folds.

mod comments;
mod delivery;
mod tray;

pub use comments::{Mark, ReviewComment, SnippetLine};
pub use delivery::{CleanupDefault, DeliveryForm};
pub use tray::{Selection, SendTarget};

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, Context, EventEmitter, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Window, canvas, div, prelude::*, px,
};
use hutan_chat_view::diff::{self, FileDiff, LineKind, MAX_LINES};
use hutan_protocol::worktree::{
    DiffFile, DiffStatus, MergeStrategy, WorktreeDiff, WorktreeDiffRequest,
    WorktreeFileDiffRequest, WorktreeInfo, WorktreeMerge, WorktreeMerged, WorktreeRemove,
};

use super::chat::vim::{Outcome, Vim};
use super::chat::{Look, TextInput, counts_of, diff_row};
use super::panels::{edit_text, field_box, pill};
use super::registry::PanelEnv;
use super::render::probe;
use super::style::UiStyle;
use super::text_field::{FieldMut, FieldRef};
use super::ui::{self, StyledExt as _};
use super::view::Region;
use super::vim_ui::{self, VimConfig};
use crate::sessions::CheckoutActivity;
use crate::theme::{DensityExt as _, Theme, Tokens};

/// Below this width the view stacks: the file list becomes a strip above the diff, so the diff
/// and the scope card keep the whole width (N6).
const NARROW_BELOW: f32 = 720.0;

/// What a Changes view asks of the workbench.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesEvent {
    /// A line for the notice area (the merge result, a removal).
    Notice(String),
    /// The worktree is gone: close the tab.
    Close,
    /// The review comments of this checkout changed: save them (all of them, every base).
    Comments(Vec<ReviewComment>),
    /// "Start a new agent with these comments": open the launcher on `cwd` with `text` as the
    /// prompt; `comments` are marked sent once the new agent has it.
    StartAgent {
        cwd: PathBuf,
        text: String,
        comments: Vec<u64>,
    },
}

/// The hunks of a file's diff: the line ranges starting at each `@@` header.
pub fn hunk_ranges(diff: &FileDiff) -> Vec<Range<usize>> {
    let starts: Vec<usize> = diff
        .lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.kind == LineKind::Hunk)
        .map(|(at, _)| at)
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(i, start)| *start..starts.get(i + 1).copied().unwrap_or(diff.lines.len()))
        .collect()
}

/// How old a read is, in the words the freshness chip uses: `just now`, `3 s ago`, then the
/// minutes, hours and days of `docs/design.md` § Time.
fn age_words(age_ms: u64) -> String {
    match age_ms / 1000 {
        0..=4 => "just now".to_owned(),
        secs @ 5..=59 => format!("{secs} s ago"),
        _ => hutan_client::time::relative_text(age_ms, 0),
    }
}

/// The chip over the file list: `updated 3 s ago`, or `couldn't refresh · showing changes from
/// 3 min ago` while the last refresh failed.
pub fn freshness_label(now_ms: u64, refreshed_ms: u64, failed: bool) -> String {
    let age = age_words(now_ms.saturating_sub(refreshed_ms));
    if failed {
        format!("couldn't refresh · showing changes from {age}")
    } else {
        format!("updated {age}")
    }
}

/// A file's patch as the view holds it.
#[derive(Debug, Clone)]
enum FilePatch {
    Loading,
    Ready {
        diff: FileDiff,
        truncated: bool,
        binary: bool,
    },
    Failed(String),
}

#[derive(Debug)]
enum Listing {
    Loading,
    Ready(WorktreeDiff),
    Failed(String),
}

#[derive(Debug)]
struct MergeForm {
    strategy: MergeStrategy,
    message: TextInput,
    /// Vim's modal state over the message (`editor.vim_mode`).
    vim: Vim,
    running: bool,
    error: Option<String>,
    conflicts: Vec<String>,
}

#[derive(Debug)]
struct Merged {
    result: WorktreeMerged,
    removing: bool,
    error: Option<String>,
}

/// Which uncommitted changes the file list shows (`worktree.index`, protocol 0.34): the header's
/// *Staged · Unstaged · All*. Read-only: Hutan never stages or unstages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexFilter {
    #[default]
    All,
    /// Files with changes in git's index (what a commit would take now).
    Staged,
    /// Files whose working tree differs from the index, and untracked files.
    Unstaged,
}

impl IndexFilter {
    /// Whether `file` is listed under this filter.
    pub fn shows(self, file: &DiffFile) -> bool {
        match self {
            Self::All => true,
            Self::Staged => file.staged,
            Self::Unstaged => file.unstaged,
        }
    }
}

/// The marker of a file's row: `staged`, `unstaged` or `both`; none for a committed-only file.
pub fn index_mark(file: &DiffFile) -> Option<&'static str> {
    match (file.staged, file.unstaged) {
        (true, true) => Some("both"),
        (true, false) => Some("staged"),
        (false, true) => Some("unstaged"),
        (false, false) => None,
    }
}

pub struct ChangesView {
    env: PanelEnv,
    path: PathBuf,
    base: Option<String>,
    include_uncommitted: bool,
    /// The header's *Staged · Unstaged · All*.
    index_filter: IndexFilter,
    info: Option<WorktreeInfo>,
    listing: Listing,
    /// Bumped by every reload; answers of older ones are dropped.
    generation: u64,
    selected: Option<String>,
    patches: HashMap<String, FilePatch>,
    collapsed: HashSet<(String, usize)>,
    merge: Option<MergeForm>,
    merged: Option<Merged>,
    message_focus: FocusHandle,
    /// The checkout's canonical path, which sessions' placements name.
    canonical: PathBuf,
    /// Line selection, review comments and the Review tray (RC-1).
    review: tray::Review,
    delivery: delivery::Delivery,
    /// The file strip of a narrow view is unfolded into the list of files.
    files_open: bool,
    /// Horizontal scroll of the diff, for lines wider than the pane.
    diff_x: ScrollHandle,
    /// When the listing was last read, in Unix milliseconds (`updated 3 s ago`).
    refreshed_ms: Option<u64>,
    /// Why the last refresh failed while the listing it left standing is older.
    refresh_error: Option<String>,
    /// A reload is in flight; an automatic trigger that arrives meanwhile asks for one more
    /// afterwards instead of racing it.
    loading: bool,
    reload_again: bool,
    /// The timer that redraws the age chip while the view is drawn.
    tick: Option<gpui::Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
    /// The width the view was last painted at (the workbench clears the shared regions before it
    /// draws, so the view keeps its own).
    width: std::rc::Rc<std::cell::Cell<Option<gpui::Pixels>>>,
}

impl EventEmitter<ChangesEvent> for ChangesView {}

impl std::fmt::Debug for ChangesView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangesView")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ChangesView {
    pub fn new(
        env: PanelEnv,
        path: PathBuf,
        base: Option<String>,
        comments: Vec<ReviewComment>,
        cx: &mut Context<Self>,
    ) -> Self {
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let mut view = Self {
            env,
            path,
            base,
            include_uncommitted: true,
            index_filter: IndexFilter::All,
            info: None,
            listing: Listing::Loading,
            generation: 0,
            selected: None,
            patches: HashMap::new(),
            collapsed: HashSet::new(),
            merge: None,
            merged: None,
            message_focus: cx.focus_handle(),
            canonical,
            review: tray::Review::new(comments, cx),
            delivery: delivery::Delivery::new(cx),
            files_open: false,
            diff_x: ScrollHandle::new(),
            width: std::rc::Rc::default(),
            refreshed_ms: None,
            refresh_error: None,
            loading: false,
            reload_again: false,
            tick: None,
            _subscriptions: Vec::new(),
        };
        let sessions = view.env.sessions.clone();
        view._subscriptions = vec![
            cx.observe(&sessions, |_, _, cx| cx.notify()),
            cx.subscribe(&sessions, |this, _, activity: &CheckoutActivity, cx| {
                if activity.touches(&this.path, &this.canonical) {
                    this.refresh_soon(cx);
                }
            }),
        ];
        view.reload(cx);
        view
    }

    // ---- read access (also for tests) -----------------------------------------------------

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `Changes · <branch>` (the directory name until the worktree is listed).
    pub fn title(&self) -> String {
        let name = self
            .info
            .as_ref()
            .and_then(|i| i.branch.clone())
            .or_else(|| {
                self.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| self.path.display().to_string());
        format!("Changes · {name}")
    }

    pub fn info(&self) -> Option<&WorktreeInfo> {
        self.info.as_ref()
    }

    /// The changed files, once listed.
    pub fn files(&self) -> &[DiffFile] {
        match &self.listing {
            Listing::Ready(diff) => &diff.files,
            _ => &[],
        }
    }

    /// The base the comparison used, once listed.
    pub fn base(&self) -> Option<&str> {
        match &self.listing {
            Listing::Ready(diff) => Some(&diff.base),
            _ => None,
        }
    }

    /// The listing failed with this.
    pub fn error(&self) -> Option<&str> {
        match &self.listing {
            Listing::Failed(error) => Some(error),
            _ => None,
        }
    }

    pub fn truncated(&self) -> bool {
        matches!(&self.listing, Listing::Ready(diff) if diff.patch_truncated)
    }

    pub fn include_uncommitted(&self) -> bool {
        self.include_uncommitted
    }

    pub fn index_filter(&self) -> IndexFilter {
        self.index_filter
    }

    /// Shows only the staged files, only the unstaged ones, or all (nothing is re-listed).
    pub fn set_index_filter(&mut self, filter: IndexFilter, cx: &mut Context<Self>) {
        if self.index_filter != filter {
            self.index_filter = filter;
            cx.notify();
        }
    }

    /// Whether the listing says what is staged: uncommitted changes are included and some file
    /// carries a marker (a daemon without `worktree.index` sets none).
    pub fn has_index_marks(&self) -> bool {
        self.include_uncommitted && self.files().iter().any(|f| index_mark(f).is_some())
    }

    /// The paths the file list shows now, under the index filter.
    pub fn visible_files(&self) -> Vec<String> {
        let filter = if self.has_index_marks() {
            self.index_filter
        } else {
            IndexFilter::All
        };
        self.files()
            .iter()
            .filter(|f| filter.shows(f))
            .map(|f| f.path.clone())
            .collect()
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The parsed diff of the selected file, once its patch arrived.
    pub fn selected_diff(&self) -> Option<&FileDiff> {
        match self.patches.get(self.selected.as_deref()?)? {
            FilePatch::Ready { diff, .. } => Some(diff),
            _ => None,
        }
    }

    /// Hunks of the selected file that are folded.
    pub fn collapsed_hunks(&self) -> Vec<usize> {
        let Some(file) = &self.selected else {
            return Vec::new();
        };
        let mut hunks: Vec<usize> = self
            .collapsed
            .iter()
            .filter(|(f, _)| f == file)
            .map(|(_, h)| *h)
            .collect();
        hunks.sort_unstable();
        hunks
    }

    pub fn merge_open(&self) -> bool {
        self.merge.is_some()
    }

    pub fn merge_strategy(&self) -> Option<MergeStrategy> {
        self.merge.as_ref().map(|m| m.strategy)
    }

    pub fn merge_error(&self) -> Option<&str> {
        self.merge.as_ref().and_then(|m| m.error.as_deref())
    }

    pub fn merge_conflicts(&self) -> &[String] {
        self.merge.as_ref().map_or(&[], |m| m.conflicts.as_slice())
    }

    /// The result of a merge done in this view, while its offer to remove the worktree stands.
    pub fn merged(&self) -> Option<&WorktreeMerged> {
        self.merged.as_ref().map(|m| &m.result)
    }

    pub fn removal_error(&self) -> Option<&str> {
        self.merged.as_ref().and_then(|m| m.error.as_deref())
    }

    /// Whether this checkout can be merged: a worktree with a branch.
    pub fn can_merge(&self) -> bool {
        self.info
            .as_ref()
            .is_some_and(|i| !i.main && i.branch.is_some())
    }

    // ---- loading --------------------------------------------------------------------------

    /// Reads the checkout again (the Refresh button, a window that came back, an agent's turn
    /// that ended). What the user is looking at stays: the selected file (while it is still
    /// changed), its patch until the new one arrives, the folds, the scroll, the line selection
    /// and an open comment box where their lines moved to. A refresh that fails keeps the list
    /// and says so in the freshness chip.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load(false, cx);
    }

    /// [`reload`](Self::reload) for a trigger nobody asked for: it waits for a reload in flight
    /// and then reads once more, so a burst of finished commands is at most two reads.
    pub fn refresh_soon(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            self.reload_again = true;
        } else {
            self.reload(cx);
        }
    }

    /// Reads the checkout again whenever `window` becomes the active window.
    pub fn follow_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._subscriptions
            .push(cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.refresh_soon(cx);
                }
            }));
    }

    /// `replace`: the question changed (uncommitted changes in or out), so nothing of the old
    /// answer is kept and a failure replaces the list.
    fn load(&mut self, replace: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        self.reload_again = false;
        if replace {
            self.patches.clear();
            self.review_reloading();
        }
        if !matches!(self.listing, Listing::Ready(_)) {
            self.listing = Listing::Loading;
        }
        let path = self.path.clone();
        let request = WorktreeDiffRequest {
            path: path.clone(),
            base: self.base.clone(),
            include_uncommitted: self.include_uncommitted,
        };
        let diff = self
            .env
            .sessions
            .update(cx, |store, cx| store.worktree_diff(request, cx));
        let worktrees = self
            .env
            .sessions
            .update(cx, |store, cx| store.list_all_worktrees(cx));
        cx.spawn(async move |this, cx| {
            let (diff, worktrees) = (diff.await, worktrees.await);
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                if let Ok(list) = worktrees {
                    this.info = list.into_iter().find(|w| w.path == path);
                }
                match diff {
                    Ok(diff) => {
                        let keep = this
                            .selected
                            .as_ref()
                            .filter(|s| diff.files.iter().any(|f| &f.path == *s))
                            .cloned();
                        this.selected = keep.or_else(|| diff.files.first().map(|f| f.path.clone()));
                        this.listing = Listing::Ready(diff);
                        this.refreshed_ms = Some(hutan_client::time::now_ms());
                        this.refresh_error = None;
                        this.refresh_patches(cx);
                        this.review_listed(cx);
                    }
                    Err(error) if !replace && matches!(this.listing, Listing::Ready(_)) => {
                        this.refresh_error = Some(error);
                    }
                    Err(error) => this.listing = Listing::Failed(error),
                }
                if std::mem::take(&mut this.reload_again) {
                    this.reload(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// After a listing: the patches of the shown file and of the files with review comments are
    /// fetched again, the old ones standing until the new ones arrive; the others are dropped
    /// (they are fetched when a file is selected).
    fn refresh_patches(&mut self, cx: &mut Context<Self>) {
        let wanted = self.refresh_files();
        let mut old = std::mem::take(&mut self.patches);
        for file in &wanted {
            if let Some(patch @ FilePatch::Ready { .. }) = old.remove(file) {
                self.patches.insert(file.clone(), patch);
            }
        }
        for file in &wanted {
            self.fetch_patch(file, cx);
        }
        if self
            .review
            .selection
            .as_ref()
            .is_some_and(|s| Some(s.file.as_str()) != self.selected.as_deref())
        {
            self.review.selection = None;
        }
    }

    /// Includes or leaves out the uncommitted changes, and lists again.
    pub fn set_include_uncommitted(&mut self, include: bool, cx: &mut Context<Self>) {
        if self.include_uncommitted != include {
            self.include_uncommitted = include;
            self.load(true, cx);
        }
    }

    /// The files whose patches a refresh reads again: the shown one first, then every listed
    /// file this diff has review comments on.
    fn refresh_files(&self) -> Vec<String> {
        let mut files: Vec<String> = self.selected.iter().cloned().collect();
        if let Some(base) = self.base() {
            for comment in self.review.comments.iter().filter(|c| c.base == base) {
                if !files.contains(&comment.file)
                    && self.files().iter().any(|f| f.path == comment.file)
                {
                    files.push(comment.file.clone());
                }
            }
        }
        files
    }

    /// When the listing was last read (Unix milliseconds), once it has been.
    pub fn refreshed_ms(&self) -> Option<u64> {
        self.refreshed_ms
    }

    /// Why the last refresh failed, while the list on screen is older than it.
    pub fn refresh_error(&self) -> Option<&str> {
        self.refresh_error.as_deref()
    }

    /// Whether a read of the checkout is in flight.
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// The freshness chip's words at `now_ms`: `updated 3 s ago`, or that the last refresh
    /// failed. Nothing before the first listing.
    pub fn freshness_text(&self, now_ms: u64) -> Option<String> {
        Some(freshness_label(
            now_ms,
            self.refreshed_ms?,
            self.refresh_error.is_some(),
        ))
    }

    /// Draws the age chip again in a second (a minute in, every 30 s) while the view is drawn.
    fn schedule_tick(&mut self, cx: &mut Context<Self>) {
        let Some(at) = self.refreshed_ms else {
            return;
        };
        if self.tick.is_some() {
            return;
        }
        let wait = if hutan_client::time::now_ms().saturating_sub(at) < 60_000 {
            1
        } else {
            30
        };
        self.tick = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(wait))
                .await;
            this.update(cx, |this, cx| {
                this.tick = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Shows a file's diff (fetched from the daemon the first time).
    pub fn select(&mut self, file: &str, cx: &mut Context<Self>) {
        if !self.files().iter().any(|f| f.path == file) {
            return;
        }
        self.review.selection = None;
        self.selected = Some(file.to_owned());
        self.files_open = false;
        self.load_patch(file, cx);
        cx.notify();
    }

    /// Whether the last frame was narrower than [`NARROW_BELOW`]: the layout that frame's
    /// width asks for (wide until the first one has been painted).
    fn narrow(&self) -> bool {
        self.width
            .get()
            .is_some_and(|width| width < px(NARROW_BELOW))
    }

    /// Unfolds or folds the file strip of a narrow view.
    pub fn toggle_files(&mut self, cx: &mut Context<Self>) {
        self.files_open = !self.files_open;
        cx.notify();
    }

    /// Whether the narrow view shows the whole list of files.
    pub fn files_open(&self) -> bool {
        self.files_open
    }

    /// How far the diff can scroll sideways: 0 while every line fits.
    pub fn diff_scroll_width(&self) -> f32 {
        f32::from(self.diff_x.max_offset().x)
    }

    /// Shows `file`'s patch: fetched the first time it is wanted.
    fn load_patch(&mut self, file: &str, cx: &mut Context<Self>) {
        if !self.patches.contains_key(file) {
            self.fetch_patch(file, cx);
        }
    }

    /// Asks the daemon for `file`'s patch. A patch already held stays on screen until its
    /// replacement arrives, so a refresh does not blank the diff or reset its scroll.
    fn fetch_patch(&mut self, file: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.files().iter().find(|f| f.path == file).cloned() else {
            return;
        };
        if !self.patches.contains_key(file) {
            self.patches.insert(file.to_owned(), FilePatch::Loading);
        }
        let generation = self.generation;
        let request = WorktreeFileDiffRequest {
            path: self.path.clone(),
            file: entry.path.clone(),
            old_path: entry.old_path.clone(),
            base: self.base.clone(),
            include_uncommitted: self.include_uncommitted,
        };
        let fetching = self
            .env
            .sessions
            .update(cx, |store, cx| store.worktree_file_diff(request, cx));
        cx.spawn(async move |this, cx| {
            let answer = fetching.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match answer {
                    Ok(answer) => {
                        let patch = FilePatch::Ready {
                            diff: diff::split_patch(&answer.patch)
                                .into_iter()
                                .next()
                                .unwrap_or_else(|| {
                                    diff::split_patch("diff --git a/x b/x\n").remove(0)
                                }),
                            truncated: answer.truncated,
                            binary: answer.binary || entry.binary,
                        };
                        let before = this.patches.insert(entry.path.clone(), patch);
                        if let Some(FilePatch::Ready { diff: before, .. }) = before {
                            this.review_patch_replaced(&entry.path, &before);
                        }
                    }
                    // A patch that is on screen stays; the chip says it is not current.
                    Err(error) => {
                        if matches!(this.patches.get(&entry.path), Some(FilePatch::Ready { .. })) {
                            this.refresh_error = Some(error);
                        } else {
                            this.patches
                                .insert(entry.path.clone(), FilePatch::Failed(error));
                        }
                    }
                }
                this.review_patch_arrived(&entry.path, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Folds or unfolds hunk `hunk` of the selected file.
    pub fn toggle_hunk(&mut self, hunk: usize, cx: &mut Context<Self>) {
        self.review.selection = None;
        if let Some(file) = self.selected.clone()
            && !self.collapsed.remove(&(file.clone(), hunk))
        {
            self.collapsed.insert((file, hunk));
        }
        cx.notify();
    }

    // ---- merging --------------------------------------------------------------------------

    /// Opens the merge form ("Merge into main…").
    pub fn open_merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_merge() || self.merged.is_some() {
            return;
        }
        self.delivery.close();
        if self.merge.is_none() {
            self.merge = Some(MergeForm {
                strategy: MergeStrategy::Merge,
                message: TextInput::new(),
                vim: Vim::new(),
                running: false,
                error: None,
                conflicts: Vec::new(),
            });
        }
        window.focus(&self.message_focus, cx);
        cx.notify();
    }

    pub fn close_merge(&mut self, cx: &mut Context<Self>) {
        self.merge = None;
        cx.notify();
    }

    pub fn set_strategy(&mut self, strategy: MergeStrategy, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.merge {
            form.strategy = strategy;
            form.error = None;
            cx.notify();
        }
    }

    pub fn set_merge_message(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.merge {
            form.message.set(text);
            cx.notify();
        }
    }

    /// Sends `worktree.merge`; the result is a notice and the offer to remove the worktree, a
    /// refusal stays in the form (a conflict lists its files).
    pub fn confirm_merge(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.merge else {
            return;
        };
        if form.running {
            return;
        }
        form.running = true;
        form.error = None;
        form.conflicts.clear();
        let message = form.message.text().trim().to_owned();
        let request = WorktreeMerge {
            path: self.path.clone(),
            into: self.base.clone(),
            strategy: form.strategy,
            message: (form.strategy != MergeStrategy::Rebase && !message.is_empty())
                .then_some(message),
        };
        let merging = self
            .env
            .sessions
            .update(cx, |store, cx| store.merge_worktree(request, cx));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = merging.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(result) => {
                        this.merge = None;
                        cx.emit(ChangesEvent::Notice(format!(
                            "Merged {} into {} ({})",
                            result.branch,
                            result.target,
                            result.commit.chars().take(7).collect::<String>()
                        )));
                        this.merged = Some(Merged {
                            result,
                            removing: false,
                            error: None,
                        });
                        this.reload(cx);
                    }
                    Err(error) => {
                        if let Some(form) = &mut this.merge {
                            form.running = false;
                            let (text, conflicts) = split_conflicts(&error);
                            form.error = Some(text);
                            form.conflicts = conflicts;
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "Remove worktree and branch": `worktree.remove` with `delete_branch` (the daemon deletes
    /// the branch with `-d`: it is merged now).
    pub fn remove_worktree(&mut self, cx: &mut Context<Self>) {
        let Some(merged) = &mut self.merged else {
            return;
        };
        if merged.removing {
            return;
        }
        merged.removing = true;
        merged.error = None;
        let request = WorktreeRemove {
            path: self.path.clone(),
            force: merged.result.source_dirty,
            delete_branch: true,
        };
        let removing = self
            .env
            .sessions
            .update(cx, |store, cx| store.remove_worktree(request, cx));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = removing.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(removed) => {
                        let branch = this
                            .merged
                            .as_ref()
                            .map(|m| m.result.branch.clone())
                            .unwrap_or_default();
                        cx.emit(ChangesEvent::Notice(match removed.branch_kept {
                            Some(reason) => {
                                format!("Removed the worktree; branch {branch} kept: {reason}")
                            }
                            None => format!("Removed the worktree and branch {branch}"),
                        }));
                        cx.emit(ChangesEvent::Close);
                    }
                    Err(error) => {
                        if let Some(merged) = &mut this.merged {
                            merged.removing = false;
                            merged.error = Some(error);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Dismisses the offer to remove the worktree.
    pub fn keep_worktree(&mut self, cx: &mut Context<Self>) {
        self.merged = None;
        cx.notify();
    }

    fn on_message_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let vim = VimConfig::of(&self.env.settings);
        match event.keystroke.key.as_str() {
            "escape" => {
                // Under Vim mode the first Escape leaves Insert (or Visual); Normal mode closes.
                if vim.enabled
                    && let Some(form) = &mut self.merge
                    && vim_ui::apply(
                        &mut form.vim,
                        &mut form.message,
                        event,
                        vim.options(false, true),
                        cx,
                    ) == Outcome::Handled
                {
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                self.close_merge(cx);
            }
            "enter" if event.keystroke.modifiers.platform => self.confirm_merge(cx),
            _ => {
                let Some(form) = &mut self.merge else {
                    return;
                };
                if !edit_text(&mut form.message, &mut form.vim, vim, event, true, cx) {
                    return;
                }
                cx.notify();
            }
        }
        cx.stop_propagation();
    }

    /// How the merge message's element reaches its text: the platform's text input and the mouse.
    fn message_field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.message_focus.clone(),
            |this, _, f| {
                let vim_on = VimConfig::of(&this.env.settings).enabled;
                if let Some(form) = &mut this.merge {
                    f(&mut FieldMut {
                        input: &mut form.message,
                        vim: vim_on.then_some(&mut form.vim),
                        multiline: true,
                    });
                }
            },
        )
    }
}

/// The reason of a refused merge and the files `conflict:` lines name.
fn split_conflicts(error: &str) -> (String, Vec<String>) {
    let mut text = Vec::new();
    let mut files = Vec::new();
    for line in error.lines() {
        match line.strip_prefix("conflict: ") {
            Some(file) => files.push(file.to_owned()),
            None => text.push(line),
        }
    }
    (text.join("\n"), files)
}

fn status_mark(status: DiffStatus, theme: &Theme) -> (&'static str, gpui::Hsla) {
    match status {
        DiffStatus::Added => ("A", theme.diff_added),
        DiffStatus::Modified => ("M", theme.diff_modified),
        DiffStatus::Deleted => ("D", theme.diff_removed),
        DiffStatus::Renamed => ("R", theme.diff_renamed),
    }
}

impl Render for ChangesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        self.schedule_tick(cx);
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let look = Look::of(cx);
        let narrow = self.narrow();

        let mut root = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .bg(theme.bg)
            .track_focus(&self.review.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.on_review_key(event, window, cx);
            }))
            .child(self.size_probe(narrow))
            .child(self.render_header(&theme, &tokens, window, cx))
            .children(self.render_delivery(&theme, &tokens, window, cx));
        if let Some(tray) = self.render_review_tray(&theme, &tokens, cx) {
            root = root.child(tray);
        }
        if let Some(merged) = &self.merged {
            root = root.child(self.render_merged(merged, &theme, &tokens, cx));
        }
        if self.merge.is_some() {
            root = root.child(self.render_merge_form(&theme, &tokens, window, cx));
        }
        root.child(match &self.listing {
            Listing::Loading => div()
                .flex_1()
                .p(px(tokens.spacing * 4.0))
                .text_color(theme.muted)
                .child("Reading the changes…")
                .into_any_element(),
            Listing::Failed(error) => ui::empty_state(
                ui::Mascot::Error,
                format!("Cannot show the changes: {error}"),
                None,
                &theme,
                &tokens,
            )
            .flex_1()
            .into_any_element(),
            Listing::Ready(diff) if diff.files.is_empty() => ui::empty_state(
                ui::Mascot::Success,
                format!(
                    "No changes against {}{}.",
                    diff.base,
                    if self.include_uncommitted {
                        ""
                    } else {
                        " (uncommitted changes left out)"
                    }
                ),
                None,
                &theme,
                &tokens,
            )
            .flex_1()
            .into_any_element(),
            // A narrow view stacks: the files are a strip above a diff that has the whole width.
            Listing::Ready(_) if narrow => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.render_file_strip(&theme, &tokens, cx))
                .child(self.render_patch(&look, window, cx))
                .into_any_element(),
            Listing::Ready(_) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.render_files(&theme, &tokens, true, cx))
                .child(self.render_patch(&look, window, cx))
                .into_any_element(),
        })
    }
}

impl ChangesView {
    /// *Staged · Unstaged · All* (protocol 0.34): shown while the listing says what is staged.
    /// It filters the list only; nothing is staged or unstaged.
    fn render_index_filter(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.has_index_marks() {
            return None;
        }
        let choice = |id: &'static str, label: &'static str, filter: IndexFilter| {
            pill(id, label, self.index_filter == filter, theme, tokens)
                .relative()
                .on_click(cx.listener(move |this, _, _, cx| this.set_index_filter(filter, cx)))
                .child(probe(&self.env.regions, Region::Changes(id)))
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing))
                .child(choice(
                    "changes-index-staged",
                    "Staged",
                    IndexFilter::Staged,
                ))
                .child(choice(
                    "changes-index-unstaged",
                    "Unstaged",
                    IndexFilter::Unstaged,
                ))
                .child(choice("changes-index-all", "All", IndexFilter::All))
                .into_any_element(),
        )
    }

    fn render_header(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let branch = self
            .info
            .as_ref()
            .and_then(|i| i.branch.clone())
            .unwrap_or_else(|| {
                self.path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            });
        let against = match &self.listing {
            Listing::Ready(diff) => format!(
                "against {} (merge base {})",
                diff.base,
                diff.merge_base.chars().take(7).collect::<String>()
            ),
            _ => String::new(),
        };
        let files = self.files();
        let (added, removed) = files.iter().fold((0usize, 0usize), |(a, r), f| {
            (a + f.additions as usize, r + f.deletions as usize)
        });
        let look = Look::of(cx);
        let merge_enabled = self.can_merge() && self.merged.is_none();
        // The title and counts on the left, the actions on the right. When the pane is too
        // narrow the actions wrap under the title (and wrap again among themselves) instead of
        // running out of the pane: Merge is always reachable.
        div()
            .flex()
            .flex_wrap()
            .flex_none()
            .items_center()
            .gap_x(px(tokens.spacing * 2.0))
            .gap_y(px(tokens.spacing))
            .min_h(px(tokens.sidebar_row + 8.0))
            .px(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w(px(120.0))
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .overflow_hidden()
                    .child(div().truncate().medium().child(SharedString::from(branch)))
                    .child(
                        div()
                            .text_color(theme.muted)
                            .text_size(px(tokens.secondary_font_size))
                            .truncate()
                            .child(SharedString::from(against)),
                    )
                    .when(!files.is_empty(), |s| {
                        s.child(
                            div()
                                .flex_none()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(SharedString::from(format!("{} files", files.len()))),
                        )
                        .child(counts_of(added, removed, &look))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .max_w_full()
                    .items_center()
                    .justify_end()
                    .gap(px(tokens.spacing * 2.0))
                    .children(self.render_review_controls(theme, tokens, cx))
                    .children(self.render_index_filter(theme, tokens, cx))
                    .child(
                        pill(
                            "changes-uncommitted",
                            "With uncommitted",
                            self.include_uncommitted,
                            theme,
                            tokens,
                        )
                        .relative()
                        .on_click(cx.listener({
                            let next = !self.include_uncommitted;
                            move |this, _, _, cx| this.set_include_uncommitted(next, cx)
                        }))
                        .child(probe(&self.env.regions, Region::Changes("toggle"))),
                    )
                    .children(self.render_freshness(theme, tokens))
                    .child(
                        ui::button(
                            "changes-refresh",
                            "Refresh",
                            ui::ButtonKind::Ghost,
                            true,
                            theme,
                            tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, _, cx| this.reload(cx)))
                        .child(probe(&self.env.regions, Region::Changes("refresh"))),
                    )
                    .child(
                        ui::button(
                            "changes-merge",
                            format!("Merge into {}…", self.base().map_or("main", |b| b)),
                            ui::ButtonKind::Primary,
                            merge_enabled,
                            theme,
                            tokens,
                        )
                        .relative()
                        .when(merge_enabled, |s| {
                            s.on_click(
                                cx.listener(|this, _, window, cx| this.open_merge(window, cx)),
                            )
                        })
                        .child(probe(&self.env.regions, Region::Changes("merge"))),
                    ),
            )
            .into_any_element()
    }

    /// The chip that says how current the list is; it turns to a warning when a refresh failed.
    /// Its tooltip has the absolute time of the last read, and the reason of a failure.
    fn render_freshness(&self, theme: &Theme, tokens: &Tokens) -> Option<AnyElement> {
        let refreshed = self.refreshed_ms?;
        let text = self.freshness_text(hutan_client::time::now_ms())?;
        let failed = self.refresh_error.is_some();
        let mut tip = format!(
            "Last read {}",
            hutan_client::time::local_time_seconds_text(refreshed)
        );
        if let Some(error) = &self.refresh_error {
            tip = format!("{error}\n{tip}");
        }
        let tip = SharedString::from(tip);
        Some(
            div()
                .id("changes-freshness")
                .relative()
                .flex_none()
                .text_size(px(tokens.secondary_font_size))
                .text_color(if failed { theme.warning } else { theme.muted })
                .tooltip(move |_, cx| cx.new(|_| super::style::TextTooltip(tip.clone())).into())
                .child(SharedString::from(text))
                .child(probe(&self.env.regions, Region::Changes("freshness")))
                .into_any_element(),
        )
    }

    fn render_merge_form(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(form) = &self.merge else {
            return div().into_any_element();
        };
        let branch = self
            .info
            .as_ref()
            .and_then(|i| i.branch.clone())
            .unwrap_or_default();
        let target = self.base().unwrap_or("main").to_owned();
        let mut strategies = div().flex().gap(px(tokens.spacing));
        for (strategy, label, key) in [
            (MergeStrategy::Merge, "Merge commit", "strategy-merge"),
            (MergeStrategy::Squash, "Squash", "strategy-squash"),
            (MergeStrategy::Rebase, "Rebase", "strategy-rebase"),
        ] {
            strategies = strategies.child(
                pill(
                    SharedString::from(format!("changes-{key}")),
                    label,
                    form.strategy == strategy,
                    theme,
                    tokens,
                )
                .relative()
                .on_click(cx.listener(move |this, _, _, cx| this.set_strategy(strategy, cx)))
                .child(probe(&self.env.regions, Region::Changes(key))),
            );
        }
        let focused = self.message_focus.is_focused(window);
        let vim = VimConfig::of(&self.env.settings).enabled;
        let mut body = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(tokens.spacing * 1.5))
            .m(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .card(theme, tokens)
            .child(
                div()
                    .medium()
                    .child(SharedString::from(format!("Merge {branch} into {target}"))),
            )
            .child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(
                        "Runs in the main checkout and makes a local commit; nothing is pushed. \
                         It is refused when the main checkout has uncommitted changes or does not \
                         have the target branch checked out, and aborted cleanly on conflicts.",
                    ),
            )
            .child(strategies)
            .child(self.render_merge_takes(theme, tokens, cx));
        if form.strategy != MergeStrategy::Rebase {
            body = body.child(
                field_box(
                    "changes-message",
                    &form.message,
                    vim.then_some(&form.vim),
                    "Commit message (empty: git's own)",
                    focused,
                    true,
                    Some(self.message_field(cx)),
                    theme,
                    tokens,
                )
                .relative()
                .track_focus(&self.message_focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    this.on_message_key(event, cx);
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        // The workbench takes focus on this same mouse down; ask again after it.
                        let handle = this.message_focus.clone();
                        cx.defer_in(window, move |_, window, cx| window.focus(&handle, cx));
                    }),
                )
                .child(probe(&self.env.regions, Region::Changes("message"))),
            );
        }
        if let Some(error) = &form.error {
            body = body.child(
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(error.clone())),
            );
        }
        if !form.conflicts.is_empty() {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .font_family(Look::of(cx).mono.clone())
                    .text_size(px(tokens.secondary_font_size))
                    .children(form.conflicts.iter().map(|file| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(crate::icons::glyph(crate::icons::Icon::StateWarning))
                            .child(SharedString::from(file.clone()))
                    })),
            );
        }
        body.child(
            div()
                .flex()
                .justify_end()
                .gap(px(tokens.spacing * 2.0))
                .child(
                    ui::button(
                        "changes-merge-cancel",
                        "Cancel",
                        ui::ButtonKind::Ghost,
                        true,
                        theme,
                        tokens,
                    )
                    .relative()
                    .on_click(cx.listener(|this, _, _, cx| this.close_merge(cx)))
                    .child(probe(&self.env.regions, Region::Changes("merge-cancel"))),
                )
                .child(
                    ui::button(
                        "changes-merge-confirm",
                        if form.running { "Merging…" } else { "Merge" },
                        ui::ButtonKind::Primary,
                        !form.running,
                        theme,
                        tokens,
                    )
                    .relative()
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_merge(cx)))
                    .child(probe(&self.env.regions, Region::Changes("merge-confirm"))),
                ),
        )
        .into_any_element()
    }

    fn render_merged(
        &self,
        merged: &Merged,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let result = &merged.result;
        let keep = CleanupDefault::of(result) == CleanupDefault::Keep;
        let mut text = format!(
            "Merged {} into {} ({}).",
            result.branch,
            result.target,
            result.commit.chars().take(7).collect::<String>()
        );
        if result.source_dirty {
            text.push_str(
                " The worktree still has uncommitted changes that were not merged; removing it \
                 would discard them, so it stays unless you say otherwise.",
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(tokens.spacing * 1.5))
            .m(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .card(theme, tokens)
            .child(
                div()
                    .text_color(theme.success)
                    .child(SharedString::from(text)),
            )
            .when_some(merged.error.clone(), |s, error| {
                s.child(
                    div()
                        .text_color(theme.danger)
                        .child(SharedString::from(error)),
                )
            })
            .child(
                div()
                    .flex()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        ui::button(
                            "changes-remove",
                            if result.source_dirty {
                                "Remove worktree and branch (discard its changes)"
                            } else {
                                "Remove worktree and branch"
                            },
                            if keep {
                                ui::ButtonKind::Secondary
                            } else {
                                ui::ButtonKind::Primary
                            },
                            !merged.removing,
                            theme,
                            tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, _, cx| this.remove_worktree(cx)))
                        .child(probe(&self.env.regions, Region::Changes("remove"))),
                    )
                    .child(
                        ui::button(
                            "changes-keep",
                            "Keep",
                            if keep {
                                ui::ButtonKind::Primary
                            } else {
                                ui::ButtonKind::Ghost
                            },
                            true,
                            theme,
                            tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, _, cx| this.keep_worktree(cx)))
                        .child(probe(&self.env.regions, Region::Changes("keep"))),
                    ),
            )
            .into_any_element()
    }

    /// Records the view's painted bounds (what [`ChangesView::narrow`] reads next frame) and asks
    /// for another frame when the width crossed [`NARROW_BELOW`] since the layout was built.
    fn size_probe(&self, narrow: bool) -> impl IntoElement {
        let regions = self.env.regions.clone();
        let width = std::rc::Rc::clone(&self.width);
        canvas(
            |bounds, _, _| bounds,
            move |_, bounds, window, cx| {
                regions.record(window.current_view(), Region::Changes("root"), bounds);
                width.set(Some(bounds.size.width));
                if (bounds.size.width < px(NARROW_BELOW)) != narrow {
                    // The layout was built for the other width: build it again.
                    cx.notify(window.current_view());
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// The narrow view's file list: the selected file as one row that unfolds into the list.
    fn render_file_strip(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let look = Look::of(cx);
        let files = self.files();
        let current = self
            .selected
            .as_deref()
            .and_then(|path| files.iter().position(|f| f.path == path));
        let mut strip = div()
            .flex()
            .flex_col()
            .flex_none()
            .border_b_1()
            .border_color(theme.border);
        let hover = theme.surface_hover;
        let label = current
            .and_then(|at| files.get(at))
            .map_or_else(|| "Choose a file".to_owned(), |f| f.path.clone());
        let (mark, color) = current
            .and_then(|at| files.get(at))
            .map_or(("", theme.muted), |f| status_mark(f.status, theme));
        let counts = current.and_then(|at| files.get(at)).map(|f| {
            if f.binary {
                div()
                    .flex_none()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child("binary")
                    .into_any_element()
            } else {
                counts_of(f.additions as usize, f.deletions as usize, &look)
            }
        });
        strip = strip.child(
            div()
                .id("changes-files-toggle")
                .relative()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 1.5))
                .mx(px(tokens.spacing))
                .my(px(tokens.spacing / 2.0))
                .px(px(tokens.spacing * 1.5))
                .h(px(tokens.sidebar_row))
                .rounded(px(tokens.radius))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_files(cx)))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.muted)
                        .child(crate::icons::glyph(if self.files_open {
                            crate::icons::Icon::ActionCollapse
                        } else {
                            crate::icons::Icon::ActionExpand
                        })),
                )
                .child(div().flex_none().w(px(12.0)).text_color(color).child(mark))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(label)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(format!(
                            "{} of {}",
                            current.map_or(0, |at| at + 1),
                            files.len()
                        ))),
                )
                .children(counts)
                .child(probe(&self.env.regions, Region::Changes("files-toggle"))),
        );
        if self.files_open {
            strip = strip.child(self.render_files(theme, tokens, false, cx));
        }
        strip.into_any_element()
    }

    fn render_files(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        wide: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let look = Look::of(cx);
        let hover = theme.surface_hover;
        let mut list = div()
            .id("changes-files")
            .flex()
            .flex_col()
            .flex_none()
            .overflow_y_scroll()
            .border_color(theme.border)
            .py(px(tokens.spacing));
        list = if wide {
            list.w(px(tokens.font_size * 24.0)).h_full().border_r_1()
        } else {
            // Unfolded under the strip: the whole width, at most eight rows tall.
            list.w_full().max_h(px(tokens.sidebar_row * 8.0))
        };
        let filter = if self.has_index_marks() {
            self.index_filter
        } else {
            IndexFilter::All
        };
        for (index, file) in self.files().iter().enumerate() {
            if !filter.shows(file) {
                continue;
            }
            let (mark, color) = status_mark(file.status, theme);
            let index_chip = index_mark(file).map(|text| {
                div()
                    .flex_none()
                    .px(px(tokens.spacing))
                    .rounded(px(crate::theme::pill_radius()))
                    .border_1()
                    .border_color(theme.border)
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(if text == "unstaged" {
                        theme.warning
                    } else {
                        theme.muted
                    })
                    .child(text)
            });
            let selected = self.selected.as_deref() == Some(file.path.as_str());
            let path = file.path.clone();
            let name = match &file.old_path {
                Some(old) => format!("{old} → {}", file.path),
                None => file.path.clone(),
            };
            list = list.child(
                div()
                    .id(SharedString::from(format!("changes-file-{index}")))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 1.5))
                    .mx(px(tokens.spacing))
                    .px(px(tokens.spacing * 1.5))
                    .h(px(tokens.sidebar_row))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .when(selected, |s| s.bg(theme.accent_soft))
                    .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(&path, cx)))
                    .child(div().flex_none().w(px(12.0)).text_color(color).child(mark))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(name)),
                    )
                    .children(index_chip)
                    .child(if file.binary {
                        div()
                            .flex_none()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child("binary")
                            .into_any_element()
                    } else {
                        counts_of(file.additions as usize, file.deletions as usize, &look)
                    })
                    .child(probe(&self.env.regions, Region::ChangesFile(index))),
            );
        }
        list.into_any_element()
    }

    fn render_patch(&self, look: &Look, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let mut pane = div()
            .id("changes-diff")
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.review.scroll)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    // The workbench takes focus on this same mouse down; ask again after it.
                    let handle = this.review.focus.clone();
                    cx.defer_in(window, move |_, window, cx| window.focus(&handle, cx));
                }),
            )
            .p(px(tokens.spacing * 2.0))
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .child(probe(&self.env.regions, Region::Changes("diff")));
        let Some(file) = self.selected.clone() else {
            return pane.into_any_element();
        };
        match self.patches.get(&file) {
            None | Some(FilePatch::Loading) => {
                pane = pane.child(div().text_color(theme.muted).child("Reading the patch…"));
            }
            Some(FilePatch::Failed(error)) => {
                pane = pane.child(div().text_color(theme.danger).child(SharedString::from(
                    format!("Cannot read the patch: {error}"),
                )));
            }
            Some(FilePatch::Ready {
                diff,
                truncated,
                binary,
            }) => {
                if *binary {
                    pane = pane.child(
                        div()
                            .text_color(theme.muted)
                            .child("A binary file: there is no text to show."),
                    );
                } else if diff.lines.is_empty() {
                    pane = pane.child(
                        div()
                            .text_color(theme.muted)
                            .child("No text changes (a mode change, or an empty file)."),
                    );
                }
                let ranges = hunk_ranges(diff);
                let mut drawn = 0usize;
                // Long lines scroll sideways inside the block, not out of the pane.
                let mut block = div()
                    .id("changes-diff-block")
                    .overflow_x_scroll()
                    .track_scroll(&self.diff_x)
                    .flex()
                    .flex_col()
                    // Rows are as wide as their text (and at least as the block): that is what
                    // gives the block something to scroll.
                    .items_start()
                    .rounded(px(tokens.radius))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.bg)
                    .font_family(look.mono.clone())
                    .text_size(px(tokens.font_size - 0.5))
                    .whitespace_nowrap()
                    .py(px(tokens.spacing));
                for (hunk, range) in ranges.iter().enumerate() {
                    let folded = self.collapsed.contains(&(file.clone(), hunk));
                    let header = &diff.lines[range.start];
                    let hidden = range.len() - 1;
                    block = block.child(
                        div()
                            .id(SharedString::from(format!("changes-hunk-{hunk}")))
                            .relative()
                            .flex()
                            .min_w_full()
                            .items_center()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_hunk(hunk, cx)))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(tokens.font_size * 1.5))
                                    .text_color(theme.muted)
                                    .child(crate::icons::glyph(if folded {
                                        crate::icons::Icon::ActionDisclose
                                    } else {
                                        crate::icons::Icon::ActionExpand
                                    })),
                            )
                            .child(diff_row(header, look))
                            .when(folded && hidden > 0, |s| {
                                s.child(
                                    div()
                                        .flex_none()
                                        .px(px(tokens.spacing * 2.0))
                                        .text_color(theme.muted)
                                        .child(SharedString::from(format!("{hidden} lines"))),
                                )
                            })
                            .child(probe(&self.env.regions, Region::ChangesHunk(hunk))),
                    );
                    if folded {
                        continue;
                    }
                    let anchors = self.anchors(&file);
                    for (offset, line) in diff.lines[range.start + 1..range.end].iter().enumerate()
                    {
                        if drawn >= MAX_LINES {
                            break;
                        }
                        block = self.review_line(
                            block,
                            &anchors,
                            &file,
                            range.start + 1 + offset,
                            line,
                            look,
                            window,
                            cx,
                        );
                        drawn += 1;
                    }
                }
                if drawn >= MAX_LINES {
                    block = block.child(
                        div()
                            .px(px(tokens.spacing * 2.0))
                            .text_color(theme.muted)
                            .child(SharedString::from(format!(
                                "… the first {MAX_LINES} lines of each file are shown"
                            ))),
                    );
                }
                if !ranges.is_empty() {
                    pane = pane.child(block);
                }
                for card in self.render_outdated(&file, look, window, cx) {
                    pane = pane.child(card);
                }
                if *truncated {
                    pane = pane.child(
                        div()
                            .text_color(theme.warning)
                            .child("This patch is larger than 2 MiB and is cut."),
                    );
                }
            }
        }
        if self.truncated() {
            pane = pane.child(
                div()
                    .text_color(theme.warning)
                    .text_size(px(tokens.secondary_font_size))
                    .child("Some patches of this diff are too large to show whole."),
            );
        }
        pane.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunks_are_the_runs_of_lines_from_each_header() {
        let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n@@ -10 +10 @@\n-d\n+e\n";
        let diff = diff::split_patch(patch).remove(0);
        assert_eq!(hunk_ranges(&diff), [0..4, 4..7]);
        assert!(
            hunk_ranges(&diff::split_patch("diff --git a/x b/x\nBinary files differ\n")[0])
                .is_empty()
        );
    }

    #[test]
    fn conflict_lines_leave_the_reason() {
        let (text, files) = split_conflicts(
            "merging `a` into `main` conflicts\nconflict: a.txt\nconflict: b/c.txt",
        );
        assert_eq!(text, "merging `a` into `main` conflicts");
        assert_eq!(files, ["a.txt", "b/c.txt"]);
        assert_eq!(split_conflicts("plain"), ("plain".to_owned(), vec![]));
    }

    #[test]
    fn the_freshness_chip_counts_seconds_then_minutes() {
        let then = 1_000_000;
        for (age_ms, words) in [
            (0, "updated just now"),
            (4_999, "updated just now"),
            (5_000, "updated 5 s ago"),
            (59_999, "updated 59 s ago"),
            (60_000, "updated 1 min ago"),
            (3 * 3_600_000, "updated 3 h ago"),
        ] {
            assert_eq!(freshness_label(then + age_ms, then, false), words);
        }
        assert_eq!(
            freshness_label(then + 180_000, then, true),
            "couldn't refresh · showing changes from 3 min ago"
        );
        // A clock that went backwards does not underflow.
        assert_eq!(freshness_label(then - 1, then, false), "updated just now");
    }
}
