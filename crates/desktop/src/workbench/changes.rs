//! The Changes view (M7-C): what a worktree (or the main checkout) changed against its base,
//! read-only, with a file list and a unified diff per file, and **Finish** (PR-FINISH, ADR
//! 0020): commit, merge a worktree into the main checkout, push — then the offer to remove a
//! merged worktree.
//!
//! Everything comes from the daemon: `worktree.diff` (the file list, against the merge base of
//! the base branch and `HEAD`, uncommitted changes togglable), `worktree.file_diff` (one file's
//! patch, fetched when it is selected) and `worktree.finish`. Patches are parsed with
//! `hutan_chat_view::diff` and drawn with the chat's diff rows; each hunk folds.

mod comments;
mod delivery;
mod finish;
mod history;
mod list;
mod staging;
mod tray;

pub use comments::{Mark, ReviewComment, SnippetLine};
pub use delivery::{CleanupDefault, DeliveryForm};
pub use finish::{StepTone, step_lines as finish_step_lines};
pub use list::ListRow;
pub use staging::HunkRow;
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
    DiffFile, DiffStatus, WorktreeDiff, WorktreeDiffRequest, WorktreeFileDiffRequest,
    WorktreeFinished, WorktreeInfo, WorktreeMerged, WorktreeRemove,
};

use super::chat::{Look, RowFlow, counts_of, diff_row_flow};
use super::registry::PanelEnv;
use super::render::probe;
use super::style::UiStyle;
use super::ui::{self, StyledExt as _};
use super::view::Region;
use crate::sessions::CheckoutActivity;
use crate::settings::ChangesSettings;
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
struct Merged {
    result: WorktreeMerged,
    removing: bool,
    error: Option<String>,
}

/// Which uncommitted changes the file list shows (`worktree.index`, protocol 0.34): the header's
/// *All · Staged · Unstaged*. Read-only: Hutan never stages or unstages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexFilter {
    #[default]
    All,
    /// Files with changes in git's index (what a commit would take now).
    Staged,
    /// Files whose working tree differs from the index, and untracked files.
    Unstaged,
}

/// What the toolbar's one segmented control picks: the committed changes alone, or the
/// uncommitted ones too under an [`IndexFilter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Leaves the uncommitted changes out of the list.
    Committed,
    /// Lists them too, narrowed by the filter.
    Index(IndexFilter),
    /// The recent commits of the checkout, each with its diff (GIT-HISTORY).
    History,
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

/// The toolbar's Finish…: the surface's primary until the Finish panel is open, which has its
/// own primary (Finish): then it is a secondary, and disabled, so the view never shows two.
fn toolbar_finish_kind(finish_open: bool) -> ui::ButtonKind {
    if finish_open {
        ui::ButtonKind::Secondary
    } else {
        ui::ButtonKind::Primary
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
    /// A file the next listing selects (the Changes panel's file rows open the view on it).
    wanted_file: Option<String>,
    /// *Finish…* was asked for before the listing it needs (the branch, whether git knows it)
    /// arrived; the form opens as soon as it has.
    finish_requested: bool,
    patches: HashMap<String, FilePatch>,
    collapsed: HashSet<(String, usize)>,
    /// The Finish form, while it is open.
    finish: Option<finish::FinishForm>,
    /// What the last Finish in this view did, step by step, until dismissed.
    finished: Option<WorktreeFinished>,
    merged: Option<Merged>,
    message_focus: FocusHandle,
    /// The checkout's canonical path, which sessions' placements name.
    canonical: PathBuf,
    /// Line selection, review comments and the Review tray (RC-1).
    review: tray::Review,
    delivery: delivery::Delivery,
    /// The History segment (`changes/history.rs`).
    history: history::HistoryState,
    /// The Stage pane: hunks of the selected file, the focused one, a discard being asked and
    /// the Undo offer after one (`changes/staging.rs`).
    staging: staging::StagingState,
    /// The file strip of a narrow view is unfolded into the list of files.
    files_open: bool,
    /// Horizontal scroll of the diff, for lines wider than the pane.
    diff_x: ScrollHandle,
    /// The scrollbars of the diff (along its lines, and down the pane) and of the file list.
    diff_bar_x: ui::Scrollbar,
    diff_bar_y: ui::Scrollbar,
    files_scroll: ScrollHandle,
    files_bar: ui::Scrollbar,
    /// The divider between the file list and the diff, and the list's width it holds; the same
    /// with the list over the diff (a narrow view), and the list's height.
    split: ui::Splitter,
    split_rows: ui::Splitter,
    /// *Wrap lines* of the diff header, for this view; `None` follows `changes.wrap_lines`.
    wrap_override: Option<bool>,
    /// The list's own toggle (flat or tree), for this view; `None` follows `changes.list`.
    list_override: Option<crate::settings::ChangesList>,
    /// Folders of the tree list that are folded.
    folded_dirs: HashSet<String>,
    /// The arrow and page keys act on the file list (after a click in it) rather than the diff.
    keys_on_list: bool,
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
    /// The same for the height.
    height: std::rc::Rc<std::cell::Cell<Option<gpui::Pixels>>>,
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
        let review = tray::Review::new(comments, cx);
        let review_scroll = review.scroll.clone();
        let diff_x = ScrollHandle::new();
        let files_scroll = ScrollHandle::new();
        // A scrollbar that tells the regions where its thumb is (tests drag it).
        let all_regions = env.regions.clone();
        let thumb = move |handle: ScrollHandle, axis: ui::ScrollAxis, name: &'static str| {
            let regions = all_regions.clone();
            ui::Scrollbar::new(handle, axis).on_paint(move |bounds, window| {
                regions.record(window.current_view(), Region::Changes(name), bounds);
            })
        };
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
            wanted_file: None,
            finish_requested: false,
            patches: HashMap::new(),
            collapsed: HashSet::new(),
            finish: None,
            finished: None,
            merged: None,
            message_focus: cx.focus_handle(),
            canonical,
            review,
            delivery: delivery::Delivery::new(cx),
            history: history::HistoryState::default(),
            staging: staging::StagingState::default(),
            files_open: true,
            diff_bar_x: thumb(diff_x.clone(), ui::ScrollAxis::Horizontal, "diff-hbar")
                .within(review_scroll.clone()),
            diff_bar_y: thumb(review_scroll, ui::ScrollAxis::Vertical, "diff-vbar"),
            files_bar: thumb(files_scroll.clone(), ui::ScrollAxis::Vertical, "files-vbar"),
            diff_x,
            files_scroll,
            split: ui::Splitter::new(ui::SplitAxis::Columns, 160.0),
            split_rows: ui::Splitter::new(ui::SplitAxis::Rows, 72.0),
            wrap_override: None,
            list_override: None,
            folded_dirs: HashSet::new(),
            keys_on_list: false,
            width: std::rc::Rc::default(),
            height: std::rc::Rc::default(),
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

    /// `Changes · <branch>` (the directory name until the worktree is listed); when another
    /// listed checkout has the same branch name (two repositories on `main`) the repository
    /// comes first: `Changes · alpha · main`.
    pub fn title(&self, cx: &gpui::App) -> String {
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
        let shared = self.info.as_ref().is_some_and(|info| {
            info.branch.is_some()
                && self
                    .env
                    .sessions
                    .read(cx)
                    .checkouts_all()
                    .iter()
                    .any(|other| other.path != info.path && other.branch == info.branch)
        });
        let repo = self
            .info
            .as_ref()
            .filter(|_| shared)
            .and_then(|info| info.repo_root.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        match repo {
            Some(repo) => format!("Changes · {repo} · {name}"),
            None => format!("Changes · {name}"),
        }
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

    /// The segment of the toolbar that is on: Committed while the uncommitted changes are left
    /// out, else the index filter in force (All while the listing says nothing of the index).
    pub fn scope(&self) -> Scope {
        if self.history.on {
            Scope::History
        } else if !self.include_uncommitted {
            Scope::Committed
        } else if self.has_index_marks() {
            Scope::Index(self.index_filter)
        } else {
            Scope::Index(IndexFilter::All)
        }
    }

    /// Picks a segment of the toolbar: what the list holds and, for the uncommitted changes,
    /// which of them.
    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        // The "Committed … Done" banner of a commit has been read once another segment is
        // picked; a merge's result (its offer to remove the worktree) and a failure stay.
        if scope != self.scope()
            && self
                .finished
                .as_ref()
                .is_some_and(|done| done.merge.is_none() && !done.failed())
        {
            self.finished = None;
        }
        if scope == Scope::History {
            self.show_history(cx);
            return;
        }
        self.hide_history(cx);
        match scope {
            Scope::Committed => self.set_include_uncommitted(false, cx),
            Scope::Index(filter) => {
                self.set_index_filter(filter, cx);
                self.set_include_uncommitted(true, cx);
            }
            Scope::History => {}
        }
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

    /// The result of a merge done in this view, while its offer to remove the worktree stands.
    pub fn merged(&self) -> Option<&WorktreeMerged> {
        self.merged.as_ref().map(|m| &m.result)
    }

    pub fn removal_error(&self) -> Option<&str> {
        self.merged.as_ref().and_then(|m| m.error.as_deref())
    }

    /// Whether this checkout is a worktree with a branch (one Finish merges).
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
        if self.history.on {
            self.reload_history(cx);
        }
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
                        let wanted = this
                            .wanted_file
                            .take()
                            .filter(|s| diff.files.iter().any(|f| &f.path == s));
                        let keep = wanted.or_else(|| {
                            this.selected
                                .as_ref()
                                .filter(|s| diff.files.iter().any(|f| &f.path == *s))
                                .cloned()
                        });
                        this.selected = keep.or_else(|| diff.files.first().map(|f| f.path.clone()));
                        this.listing = Listing::Ready(diff);
                        this.refreshed_ms = Some(hutan_client::time::now_ms());
                        this.refresh_error = None;
                        this.refresh_patches(cx);
                        this.refresh_hunks(cx);
                        this.review_listed(cx);
                        this.refill_finish(cx);
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

    /// The tooltip of the Refresh button: the action and how old the list is (`updated just
    /// now`), which the toolbar no longer draws as a chip of its own.
    fn refresh_tooltip(&self) -> SharedString {
        match self.freshness_text(hutan_client::time::now_ms()) {
            Some(age) => SharedString::from(format!("Refresh · {age}")),
            None => SharedString::from("Refresh"),
        }
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
        if self.selected.as_deref() != Some(file) {
            // Another file: its diff starts at the top left.
            self.diff_x.set_offset(gpui::point(px(0.0), px(0.0)));
            self.review.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        }
        self.selected = Some(file.to_owned());
        self.load_patch(file, cx);
        self.refresh_hunks(cx);
        cx.notify();
    }

    /// Opens the view on `file` (the Changes panel's file rows): selected now when the listing
    /// has it, else by the next listing.
    pub fn show_file(&mut self, file: &str, cx: &mut Context<Self>) {
        if !self.loading && self.files().iter().any(|f| f.path == file) {
            self.wanted_file = None;
            self.select(file, cx);
        } else {
            self.wanted_file = Some(file.to_owned());
        }
    }

    /// Whether the last frame was narrower than [`NARROW_BELOW`]: the layout that frame's
    /// width asks for (wide until the first one has been painted).
    fn narrow(&self) -> bool {
        self.width
            .get()
            .is_some_and(|width| width < px(NARROW_BELOW))
    }

    /// Folds or unfolds the file list of a narrow view. It starts unfolded (every changed file
    /// is a row under the strip) and stays as the user left it.
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

    // ---- after a merge ----------------------------------------------------------------------
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
}

/// What a Changes row says about a file Mawas keeps for itself.
pub(crate) const MEMORY_FILES_HINT: &str = "Mawas's memory files";

/// [`MEMORY_FILES_HINT`] for `rel` (relative to `checkout`) when `checkout` is the Home folder
/// and `rel` is under `.mawas/memory/`: the mirror of Mawas's memory, which a tracked Home
/// folder shows in Changes like any file (`docs/assistant.md` § Memory files).
pub(crate) fn memory_files_hint(
    home: Option<&Path>,
    checkout: &[&Path],
    rel: &str,
) -> Option<&'static str> {
    let home = home?;
    (checkout.contains(&home) && rel.starts_with(".mawas/memory/")).then_some(MEMORY_FILES_HINT)
}

pub(crate) fn status_mark(status: DiffStatus, theme: &Theme) -> (&'static str, gpui::Hsla) {
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
        self.open_requested_finish(window, cx);

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
        if let Some(offer) = self.render_undo_offer(&theme, &tokens, cx) {
            root = root.child(offer);
        }
        if let Some(done) = &self.finished {
            root = root.child(self.render_finished(done, &theme, &tokens, cx));
        }
        if let Some(merged) = &self.merged {
            root = root.child(self.render_merged(merged, &theme, &tokens, cx));
        }
        if self.finish.is_some() {
            root = root.child(self.render_finish_form(&theme, &tokens, window, cx));
        }
        if self.history.on {
            return root
                .child(self.render_history(&look, narrow, cx))
                .into_any_element();
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
                .child(self.render_diff_side(&look, &theme, &tokens, window, cx))
                .into_any_element(),
            Listing::Ready(_) => {
                let list = self.render_files(&theme, &tokens, true, cx);
                let width = self.list_width(&tokens);
                let regions = self.env.regions.clone();
                let divider =
                    self.split
                        .element("changes-split", width, &theme, move |bounds, window| {
                            regions.record(window.current_view(), Region::Changes("split"), bounds);
                        });
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(list)
                    .child(divider)
                    .child(self.render_diff_side(&look, &theme, &tokens, window, cx))
                    .into_any_element()
            }
        })
        .into_any_element()
    }
}

impl ChangesView {
    /// The one segmented control of the toolbar: *Committed · All · Staged · Unstaged*.
    /// Committed leaves the uncommitted changes out of the list; All lists them too; Staged and
    /// Unstaged (protocol 0.34, shown while the listing says what is staged) narrow the
    /// uncommitted ones. It filters the list only; nothing is staged or unstaged.
    fn render_scope_strip(
        &self,
        theme: &Theme,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.scope();
        let segment = |id: &'static str, label: &'static str, scope: Scope| {
            ui::segment(id, label, selected == scope, theme, tokens)
                .relative()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_scope(scope, cx);
                    // The list the keyboard sat in may be swapped out: the keyboard goes to
                    // the view itself, so ⌘P, ⌘⇧P and the other shortcuts keep their path.
                    window.focus(&this.review.focus, cx);
                }))
                .child(probe(&self.env.regions, Region::Changes(id)))
        };
        // Wraps onto a second line rather than run out of a narrow pane.
        let mut strip = ui::segmented(theme, tokens)
            .flex_wrap()
            .max_w_full()
            .child(segment(
                "changes-scope-committed",
                "Committed",
                Scope::Committed,
            ))
            .child(segment(
                "changes-index-all",
                "All",
                Scope::Index(IndexFilter::All),
            ));
        if self.has_index_marks() {
            strip = strip
                .child(segment(
                    "changes-index-staged",
                    "Staged",
                    Scope::Index(IndexFilter::Staged),
                ))
                .child(segment(
                    "changes-index-unstaged",
                    "Unstaged",
                    Scope::Index(IndexFilter::Unstaged),
                ));
        }
        strip = strip.child(segment("changes-scope-history", "History", Scope::History));
        strip.into_any_element()
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
        let finish_enabled = self.can_finish(cx);
        let finish_open = self.finish_open();
        // The title and counts on the left, the actions on the right. When the pane is too
        // narrow the actions wrap under the title (and wrap again among themselves) instead of
        // running out of the pane: Finish is always reachable.
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
                                .child(SharedString::from(super::search_replace::plural(
                                    files.len(),
                                    "file",
                                    "files",
                                ))),
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
                    // Right-aligned beside the title; in a narrow pane they own the line, start
                    // at its left edge and wrap among themselves, so none is cut off.
                    .when(self.narrow(), |s| s.w_full().justify_start())
                    .when(!self.narrow(), |s| s.justify_end())
                    .gap(px(tokens.spacing * 2.0))
                    .children(self.render_review_controls(theme, tokens, cx))
                    .children(self.render_staging_controls(theme, tokens, cx))
                    .child(self.render_scope_strip(theme, tokens, cx))
                    .children(self.render_freshness(theme, tokens))
                    .child(
                        ui::IconButton::new(
                            "changes-refresh",
                            crate::icons::Icon::ActionRefresh,
                            self.refresh_tooltip(),
                            theme,
                            tokens,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.reload(cx)))
                        .child(probe(&self.env.regions, Region::Changes("refresh"))),
                    )
                    .child(
                        // The Finish panel has its own Finish button: while it is open this one is
                        // a disabled secondary, so a surface never shows two primaries.
                        ui::Button::new("changes-finish", "Finish…", theme, tokens)
                            .kind(toolbar_finish_kind(finish_open))
                            .disabled(!finish_enabled || finish_open)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_finish(window, cx)),
                            )
                            .child(probe(&self.env.regions, Region::Changes("finish"))),
                    ),
            )
            .into_any_element()
    }

    /// What the toolbar says of the age of the list: nothing while it is current (the Refresh
    /// button's tooltip has it), and a warning when a refresh failed. Its tooltip has the
    /// absolute time of the last read and the reason.
    fn render_freshness(&self, theme: &Theme, tokens: &Tokens) -> Option<AnyElement> {
        let refreshed = self.refreshed_ms?;
        let error = self.refresh_error.as_ref()?;
        let text = self.freshness_text(hutan_client::time::now_ms())?;
        let tip = SharedString::from(format!(
            "{error}\nLast read {}",
            hutan_client::time::local_time_seconds_text(refreshed)
        ));
        Some(
            div()
                .id("changes-freshness")
                .relative()
                .flex_none()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.warning)
                .tooltip(move |_, cx| cx.new(|_| super::style::TextTooltip(tip.clone())).into())
                .child(SharedString::from(text))
                .child(probe(&self.env.regions, Region::Changes("freshness")))
                .into_any_element(),
        )
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
            .child({
                let remove = ui::Button::new(
                    "changes-remove",
                    if result.source_dirty {
                        "Remove worktree and branch (discard its changes)"
                    } else {
                        "Remove worktree and branch"
                    },
                    theme,
                    tokens,
                )
                .kind(if result.source_dirty {
                    ui::ButtonKind::Destructive
                } else if keep {
                    ui::ButtonKind::Secondary
                } else {
                    ui::ButtonKind::Primary
                })
                .loading(merged.removing)
                .on_click(cx.listener(|this, _, _, cx| this.remove_worktree(cx)))
                .child(probe(&self.env.regions, Region::Changes("remove")));
                let keep_button = ui::Button::new("changes-keep", "Keep", theme, tokens)
                    .kind(if keep {
                        ui::ButtonKind::Primary
                    } else {
                        ui::ButtonKind::Ghost
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.keep_worktree(cx)))
                    .child(probe(&self.env.regions, Region::Changes("keep")));
                // The default answer goes last.
                let row = div().flex().gap(px(tokens.spacing * 2.0));
                if keep {
                    row.child(remove).child(keep_button)
                } else {
                    row.child(keep_button).child(remove)
                }
            })
            .into_any_element()
    }

    /// Records the view's painted bounds (what [`ChangesView::narrow`] reads next frame) and asks
    /// for another frame when the width crossed [`NARROW_BELOW`] since the layout was built.
    fn size_probe(&self, narrow: bool) -> impl IntoElement {
        let regions = self.env.regions.clone();
        let width = std::rc::Rc::clone(&self.width);
        let height = std::rc::Rc::clone(&self.height);
        canvas(
            |bounds, _, _| bounds,
            move |_, bounds, window, cx| {
                regions.record(window.current_view(), Region::Changes("root"), bounds);
                let built_for = width.replace(Some(bounds.size.width));
                let built_tall = height.replace(Some(bounds.size.height));
                if (bounds.size.width < px(NARROW_BELOW)) != narrow
                    || built_for.is_none_or(|was| (was - bounds.size.width).abs() > px(0.5))
                    || built_tall.is_none_or(|was| (was - bounds.size.height).abs() > px(0.5))
                {
                    // The layout was built for another width (the stacked or the side-by-side
                    // one, or the file list's share of it): build it again.
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
        let label = if self.files_open {
            format!(
                "{} changed {}",
                files.len(),
                if files.len() == 1 { "file" } else { "files" }
            )
        } else {
            current
                .and_then(|at| files.get(at))
                .map_or_else(|| "Choose a file".to_owned(), |f| f.path.clone())
        };
        let (mark, color) = current
            .and_then(|at| files.get(at))
            .filter(|_| !self.files_open)
            .map_or(("", theme.muted), |f| status_mark(f.status, theme));
        let counts = current
            .and_then(|at| files.get(at))
            .filter(|_| !self.files_open)
            .map(|f| {
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
                .when(!self.files_open, |s| {
                    s.child(
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
                })
                .children(counts)
                .child(probe(&self.env.regions, Region::Changes("files-toggle"))),
        );
        if self.files_open {
            strip = strip.child(self.render_files(theme, tokens, false, cx));
            let rows = self.list_rows().len();
            let height = self.list_height(rows, tokens);
            let regions = self.env.regions.clone();
            strip = strip.child(self.split_rows.element(
                "changes-split-rows",
                height,
                theme,
                move |bounds, window| {
                    regions.record(window.current_view(), Region::Changes("split-rows"), bounds);
                },
            ));
        }
        strip.into_any_element()
    }

    /// *Open* (E-10): `file` (relative to the checkout) in a file tab at `line`.
    fn open_button(
        &self,
        id: SharedString,
        file: &str,
        line: Option<u32>,
        region: Region,
        theme: &Theme,
        tokens: &Tokens,
    ) -> AnyElement {
        let target = super::actions::FileTarget {
            path: self.canonical.join(file).display().to_string(),
            line,
            column: None,
        };
        let tip = SharedString::from(match line {
            Some(line) => format!("Open the file at line {line}"),
            None => "Open the file".to_owned(),
        });
        ui::Button::new(id, "Open", theme, tokens)
            .compact()
            .ghost()
            .tooltip(tip)
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                window.dispatch_action(Box::new(super::actions::OpenFile(target.clone())), cx);
            })
            .child(probe(&self.env.regions, region))
            .into_any_element()
    }

    /// Whether the diff of `file` wraps its long lines: the header's *Wrap lines* for this view,
    /// else `changes.wrap_lines` (`auto`: prose wraps, code scrolls sideways).
    pub fn wraps(&self, file: &str) -> bool {
        use crate::settings::WrapLines;
        self.wrap_override.unwrap_or_else(|| {
            match self.env.settings.get::<ChangesSettings>().wrap_lines {
                WrapLines::On => true,
                WrapLines::Off => false,
                WrapLines::Auto => is_prose(file),
            }
        })
    }

    /// *Wrap lines*: wraps or stops wrapping the diffs of this view (the setting is not written).
    pub fn set_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.wrap_override = Some(wrap);
        self.diff_x
            .set_offset(gpui::point(px(0.0), self.diff_x.offset().y));
        cx.notify();
    }

    /// How far the code of the diff is scrolled sideways (0 while it wraps).
    pub fn diff_offset(&self) -> f32 {
        -f32::from(self.diff_x.offset().x)
    }

    /// How far the diff pane is scrolled down, and how far it can be.
    pub fn diff_scroll_y(&self) -> (f32, f32) {
        (
            -f32::from(self.review.scroll.offset().y),
            f32::from(self.review.scroll.max_offset().y),
        )
    }

    /// How far the file list is scrolled down, and how far it can be.
    pub fn list_scroll_y(&self) -> (f32, f32) {
        (
            -f32::from(self.files_scroll.offset().y),
            f32::from(self.files_scroll.max_offset().y),
        )
    }

    /// Scrolls the diff by `(dx, dy)` pixels (positive moves the content left and up), within
    /// its limits.
    pub fn scroll_diff_by(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        scroll_by(&self.diff_x, dx, 0.0);
        scroll_by(&self.review.scroll, 0.0, dy);
        cx.notify();
    }

    /// Scrolls the file list by `dy` pixels, within its limits.
    pub fn scroll_list_by(&mut self, dy: f32, cx: &mut Context<Self>) {
        scroll_by(&self.files_scroll, 0.0, dy);
        cx.notify();
    }

    /// Page keys, Home and End on whichever of the list and the diff has the keys; whether the
    /// key was one of them.
    pub(super) fn scroll_key(&mut self, key: &str, plain: bool, cx: &mut Context<Self>) -> bool {
        if !plain {
            return false;
        }
        let (view, line) = if self.keys_on_list {
            (f32::from(self.files_scroll.bounds().size.height), 28.0)
        } else {
            (f32::from(self.review.scroll.bounds().size.height), 20.0)
        };
        let page = (view - 2.0 * line).max(line);
        let (x, y) = match key {
            "pagedown" => (0.0, page),
            "pageup" => (0.0, -page),
            "home" => (0.0, -1.0e9),
            "end" => (0.0, 1.0e9),
            "down" if !self.keys_on_list => (0.0, line),
            "up" if !self.keys_on_list => (0.0, -line),
            "right" if !self.keys_on_list => (40.0, 0.0),
            "left" if !self.keys_on_list => (-40.0, 0.0),
            "down" => {
                self.select_neighbor(true, cx);
                return true;
            }
            "up" => {
                self.select_neighbor(false, cx);
                return true;
            }
            _ => return false,
        };
        if self.keys_on_list {
            self.scroll_list_by(y, cx);
        } else {
            self.scroll_diff_by(x, y, cx);
        }
        true
    }

    fn render_patch(&self, look: &Look, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let wrap = self
            .selected
            .as_deref()
            .is_some_and(|file| self.wraps(file));
        let flow = RowFlow {
            wrap,
            scrolled: if wrap {
                px(0.0)
            } else {
                -self.diff_x.offset().x
            },
        };
        let mut pane = div()
            .id("changes-diff")
            .relative()
            .size_full()
            .overflow_y_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.review.scroll)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    // The workbench takes focus on this same mouse down; ask again after it.
                    this.keys_on_list = false;
                    let handle = this.review.focus.clone();
                    cx.defer_in(window, move |_, window, cx| window.focus(&handle, cx));
                }),
            )
            .p(px(tokens.spacing * 2.0))
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .child(probe(&self.env.regions, Region::Changes("diff")));
        if let Some(file) = self.selected.clone() {
            pane = self.fill_patch(pane, &file, flow, look, window, cx);
        }
        if self.truncated() {
            pane = pane.child(
                div()
                    .text_color(theme.warning)
                    .text_size(px(tokens.secondary_font_size))
                    .child("Some patches of this diff are too large to show whole."),
            );
        }
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(pane)
            .child(self.diff_bar_y.element(theme))
            .child(probe(&self.env.regions, Region::Changes("diff-view")))
            .into_any_element()
    }

    /// The selected file's patch (or what stands in for it) as children of `pane`.
    fn fill_patch(
        &self,
        mut pane: gpui::Stateful<gpui::Div>,
        file: &str,
        flow: RowFlow,
        look: &Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (theme, tokens) = (&look.theme, &look.tokens);
        match self.patches.get(file) {
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
                // Every row of a block is as wide as the widest (so a tint spans the whole
                // scrolled width) and at least as wide as the pane; wrapped, as the pane.
                let mut rows = div().flex().flex_col();
                rows = if flow.wrap {
                    rows.w_full()
                } else {
                    rows.min_w_full()
                };
                for (hunk, range) in ranges.iter().enumerate() {
                    let folded = self.collapsed.contains(&(file.to_owned(), hunk));
                    let header = &diff.lines[range.start];
                    let hidden = range.len() - 1;
                    let chevron = div()
                        .flex_none()
                        .w(px(tokens.font_size * 1.5))
                        .text_color(theme.muted)
                        .child(crate::icons::glyph(if folded {
                            crate::icons::Icon::ActionDisclose
                        } else {
                            crate::icons::Icon::ActionExpand
                        }))
                        .into_any_element();
                    let trail = div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .when(folded && hidden > 0, |s| {
                            s.child(
                                div()
                                    .flex_none()
                                    .px(px(tokens.spacing * 2.0))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(format!("{hidden} lines"))),
                            )
                        })
                        .child(self.open_button(
                            SharedString::from(format!("changes-hunk-open-{hunk}")),
                            file,
                            diff.changed_line_from(range.start),
                            Region::ChangesHunkOpen(hunk),
                            theme,
                            tokens,
                        ))
                        .into_any_element();
                    rows = rows.child(
                        div()
                            .id(SharedString::from(format!("changes-hunk-{hunk}")))
                            .relative()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_hunk(hunk, cx)))
                            .child(diff_row_flow(
                                header,
                                look,
                                flow,
                                Some(chevron),
                                Some(trail),
                            ))
                            .child(probe(&self.env.regions, Region::ChangesHunk(hunk))),
                    );
                    if folded {
                        continue;
                    }
                    let anchors = self.anchors(file);
                    for (offset, line) in diff.lines[range.start + 1..range.end].iter().enumerate()
                    {
                        if drawn >= MAX_LINES {
                            break;
                        }
                        rows = self.review_line(
                            rows,
                            &anchors,
                            file,
                            range.start + 1 + offset,
                            line,
                            look,
                            flow,
                            window,
                            cx,
                        );
                        drawn += 1;
                    }
                }
                if drawn >= MAX_LINES {
                    rows = rows.child(
                        self.pinned(
                            flow,
                            div()
                                .px(px(tokens.spacing * 2.0))
                                .text_color(theme.muted)
                                .child(SharedString::from(format!(
                                    "… the first {MAX_LINES} lines of each file are shown"
                                )))
                                .into_any_element(),
                        ),
                    );
                }
                if !ranges.is_empty() {
                    // Long lines scroll sideways inside the block, not out of the pane (a wheel
                    // over it moves it along its own axis only: down scrolls the pane).
                    let scrollable = self.diff_x.max_offset().x > px(0.5);
                    let block = div()
                        .id("changes-diff-block")
                        .overflow_x_scroll()
                        .restrict_scroll_to_axis()
                        .track_scroll(&self.diff_x)
                        .flex()
                        .flex_col()
                        .items_start()
                        .rounded(px(tokens.radius))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.bg)
                        .font_family(look.mono.clone())
                        .text_size(px(tokens.font_size - 0.5))
                        .when(flow.wrap, |s| s.whitespace_normal())
                        .when(!flow.wrap, |s| s.whitespace_nowrap())
                        .pt(px(tokens.spacing))
                        .pb(px(if scrollable {
                            tokens.spacing + 8.0
                        } else {
                            tokens.spacing
                        }))
                        .child(rows);
                    pane = pane.child(
                        div()
                            .relative()
                            .flex_none()
                            .w_full()
                            .child(block)
                            .child(self.diff_bar_x.element(theme)),
                    );
                }
                for card in self.render_outdated(file, look, window, cx) {
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
        pane
    }
}

/// Moves `handle` by `(dx, dy)` pixels (positive: the content goes left and up) within its
/// limits.
fn scroll_by(handle: &ScrollHandle, dx: f32, dy: f32) {
    let max = handle.max_offset();
    let mut at = handle.offset();
    at.x = (at.x - px(dx)).clamp(-max.x, px(0.0));
    at.y = (at.y - px(dy)).clamp(-max.y, px(0.0));
    handle.set_offset(at);
}

/// Whether a file is prose: its diff wraps by default (Markdown and text, where a line is a
/// sentence), unlike code (`changes.wrap_lines = "auto"`).
pub fn is_prose(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    match lower.rsplit_once('.') {
        Some((_, ext)) => matches!(
            ext,
            "md" | "markdown" | "mdx" | "txt" | "text" | "rst" | "adoc" | "org" | "tex"
        ),
        None => matches!(
            lower.as_str(),
            "readme" | "license" | "licence" | "changelog" | "authors" | "notice" | "todo"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_toolbar_finish_is_a_secondary_while_the_finish_panel_has_the_primary() {
        assert_eq!(toolbar_finish_kind(false), ui::ButtonKind::Primary);
        assert_eq!(toolbar_finish_kind(true), ui::ButtonKind::Secondary);
    }

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
