//! The `changes` panel, the right sidebar's **Changes** tab: the checkouts of the workspace you
//! are in (the main checkout and every worktree), the ones with uncommitted changes first. A
//! click opens that checkout's full Changes view (`ReviewChangesAt`) as a tab of the center.
//!
//! The daemon lists them (`worktree.list`); the panel asks while it is drawn (when it opens,
//! when the workspace or the connection changes, and again after [`REFRESH_AFTER`]) and has the
//! Refresh button for the rest. It never reads a repository itself.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use hutan_client::ConnectionState;
use hutan_core::WorkspaceId;
use hutan_protocol::messages::WorkspaceKind;
use hutan_protocol::worktree::{DiffFile, WorktreeDiffRequest, WorktreeInfo};

use crate::icons::Icon;
use crate::sessions::{CheckoutActivity, StoreChanged};
use crate::settings::{ChangesScope, ChangesSettings};
use crate::theme::{DensityExt as _, Theme, Tokens};
use crate::workbench::Region;
use crate::workbench::actions::{
    ChangedFileAt, FinishChangesAt, RerunWorktreeSetup, ReviewChangesAt, ReviewFileAt,
    ReviewWorktreeSetup, ShowWorktreeSetupOutput, TrackHomeChanges,
};
use crate::workbench::chat::{Look, counts_of};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::UiStyle;
use crate::workbench::ui::{self, Mascot};
use crate::workbench::workspaces::tilde;
use crate::workbench::worktree_setup::{SetupAction, SetupTone, setup_line};

/// A drawn panel lists the checkouts again after this long.
pub const REFRESH_AFTER: Duration = Duration::from_secs(15);

/// Checkouts whose changes are counted for their headers; the rest show no count.
const MAX_COUNTED: usize = 12;

/// Files listed under a group's header; the rest is "N more files…".
const MAX_FILES_SHOWN: usize = 40;

/// What a checkout changed, for its row: files and lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub files: usize,
    pub added: usize,
    pub removed: usize,
}

impl Counts {
    /// The totals of a listing's files.
    pub fn of(files: &[DiffFile]) -> Self {
        files.iter().fold(
            Self {
                files: files.len(),
                added: 0,
                removed: 0,
            },
            |sum, f| Self {
                added: sum.added + f.additions as usize,
                removed: sum.removed + f.deletions as usize,
                ..sum
            },
        )
    }

    /// `1 file` / `4 files`.
    pub fn files_text(&self) -> String {
        if self.files == 1 {
            "1 file".to_owned()
        } else {
            format!("{} files", self.files)
        }
    }
}

/// What the panel knows about the checkouts.
#[derive(Debug, Clone, PartialEq)]
pub enum Checkouts {
    /// Nothing asked yet, or no workspace to ask about.
    Idle,
    Loading,
    Ready(Vec<WorktreeInfo>),
    Failed(String),
}

pub struct ChangesPanel {
    env: PanelEnv,
    checkouts: Checkouts,
    /// The workspace and connection state the listing describes.
    asked: Option<(WorkspaceId, ConnectionState)>,
    loaded_at: Option<Instant>,
    /// When the listing on screen was read, in Unix milliseconds.
    refreshed_ms: Option<u64>,
    /// Why the last refresh failed while the listing on screen is older.
    refresh_error: Option<String>,
    /// A listing is in flight; an automatic trigger meanwhile asks for one more afterwards.
    loading: bool,
    reload_again: bool,
    generation: u64,
    task: Option<Task<()>>,
    /// What each listed checkout changed (`worktree.diff`, the file list only), read after every
    /// listing; a checkout that could not be read has no entry.
    diffs: HashMap<PathBuf, Vec<DiffFile>>,
    /// Groups folded to their header.
    collapsed: HashSet<PathBuf>,
    /// *This checkout* or *All in workspace*, as chosen with the chip.
    changes_scope: ChangesScope,
    /// `changes.default_scope` as last seen: a change of the setting moves the chip.
    default_scope: ChangesScope,
    /// The checkout the window worked in when the files were last read.
    counted_for: Option<PathBuf>,
    counts_task: Option<Task<()>>,
    /// The list of groups scrolls on its own; its scrollbar.
    scroll: gpui::ScrollHandle,
    bar: hutan_ui::Scrollbar,
    _subscriptions: Vec<gpui::Subscription>,
}

impl std::fmt::Debug for ChangesPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangesPanel")
            .field("checkouts", &self.checkouts)
            .finish_non_exhaustive()
    }
}

/// The repository a checkout belongs to, by its folder's name.
fn repo_name(worktree: &WorktreeInfo) -> String {
    worktree.repo_root.file_name().map_or_else(
        || tilde(&worktree.repo_root),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The rows of a listing: checkouts that still exist, the ones with uncommitted changes first,
/// the main checkout before the worktrees, then by branch.
pub fn ordered(mut worktrees: Vec<WorktreeInfo>) -> Vec<WorktreeInfo> {
    worktrees.retain(|w| !w.prunable);
    worktrees.sort_by(|a, b| {
        b.dirty
            .cmp(&a.dirty)
            .then(b.main.cmp(&a.main))
            .then(a.branch.cmp(&b.branch))
            .then(a.path.cmp(&b.path))
    });
    worktrees
}

impl ChangesPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        let sessions = env.sessions.clone();
        let subscriptions = vec![
            cx.observe(&sessions, |this, _, cx| this.sync(cx)),
            // Something may have changed in a checkout: an agent's turn or a command ended.
            cx.subscribe(&sessions, |this, _, _: &CheckoutActivity, cx| {
                this.refresh_soon(cx);
            }),
            // `worktree.changed`: a worktree was made or removed, or its setup moved on.
            cx.subscribe(&sessions, |this, _, change: &StoreChanged, cx| {
                if change.intersects(StoreChanged::CHECKOUTS) {
                    this.refresh_soon(cx);
                }
            }),
        ];
        let default_scope = env.settings.get::<ChangesSettings>().default_scope;
        let env_regions = env.regions.clone();
        let scroll = gpui::ScrollHandle::new();
        let mut panel = Self {
            env,
            checkouts: Checkouts::Idle,
            asked: None,
            loaded_at: None,
            refreshed_ms: None,
            refresh_error: None,
            loading: false,
            reload_again: false,
            generation: 0,
            task: None,
            diffs: HashMap::new(),
            collapsed: HashSet::new(),
            changes_scope: default_scope,
            default_scope,
            counted_for: None,
            counts_task: None,
            bar: {
                let regions = env_regions.clone();
                hutan_ui::Scrollbar::new(scroll.clone(), hutan_ui::Axis::Vertical).on_paint(
                    move |bounds, window| {
                        regions.record(
                            window.current_view(),
                            Region::Changes("panel-vbar"),
                            bounds,
                        );
                    },
                )
            },
            scroll,
            _subscriptions: subscriptions,
        };
        panel.sync(cx);
        panel
    }

    /// Lists the checkouts again whenever `window` becomes the active window.
    pub fn follow_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._subscriptions
            .push(cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.refresh_soon(cx);
                }
            }));
    }

    /// [`refresh`](Self::refresh) for a trigger nobody asked for: it waits for a listing in
    /// flight and asks once more afterwards.
    pub fn refresh_soon(&mut self, cx: &mut Context<Self>) {
        if matches!(self.checkouts, Checkouts::Idle) {
            return;
        }
        if self.loading {
            self.reload_again = true;
        } else {
            self.refresh(cx);
        }
    }

    /// Why the last refresh failed while the listing on screen is older.
    pub fn refresh_error(&self) -> Option<&str> {
        self.refresh_error.as_deref()
    }

    /// When the listing on screen was read (Unix milliseconds).
    pub fn refreshed_ms(&self) -> Option<u64> {
        self.refreshed_ms
    }

    /// What the panel lists (tests).
    pub fn checkouts(&self) -> &Checkouts {
        &self.checkouts
    }

    /// What `checkout` changed, once counted (tests).
    pub fn counts(&self, checkout: &std::path::Path) -> Option<Counts> {
        self.diffs.get(checkout).map(|files| Counts::of(files))
    }

    /// What the panel is about: the container the window shows, or Mawas's scope while Mawas
    /// Home is the focused center tab (the chip of its header).
    pub fn scope(&self, cx: &gpui::App) -> Option<crate::sessions::ScopeLabel> {
        self.env.sessions.read(cx).followed_scope()
    }

    /// The listed checkouts, in the order the rows have.
    pub fn rows(&self) -> &[WorktreeInfo] {
        match &self.checkouts {
            Checkouts::Ready(list) => list,
            _ => &[],
        }
    }

    /// Asks again when the workspace or the connection is not the one listed.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let (workspace, state, supported) = {
            let store = self.env.sessions.read(cx);
            (
                store.scope_container_id(),
                store.connection_state(),
                store.connection().supports_worktree_review(),
            )
        };
        let wanted = workspace.map(|w| (w, state));
        if wanted == self.asked {
            // The window may work in another checkout of the same workspace now: its files are
            // read too.
            if let Checkouts::Ready(list) = &self.checkouts {
                let current = self.current_in(list, cx).map(|w| w.path.clone());
                if current != self.counted_for {
                    let list = list.clone();
                    self.count(&list, cx);
                }
            }
            return;
        }
        self.asked = wanted;
        match wanted {
            Some((workspace, ConnectionState::Connected)) if supported => {
                self.load(workspace, cx);
            }
            _ => {
                self.generation += 1;
                self.task = None;
                self.loading = false;
                self.reload_again = false;
                self.checkouts = Checkouts::Idle;
                self.diffs.clear();
                self.counts_task = None;
                self.refreshed_ms = None;
                self.refresh_error = None;
                cx.notify();
            }
        }
    }

    /// Lists the checkouts of `workspace` again (the Refresh button).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let workspace = self.env.sessions.read(cx).scope_container_id();
        if let Some(workspace) = workspace {
            self.load(workspace, cx);
        }
    }

    fn load(&mut self, workspace: WorkspaceId, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        self.reload_again = false;
        if !matches!(self.checkouts, Checkouts::Ready(_)) {
            self.checkouts = Checkouts::Loading;
        }
        self.loaded_at = Some(Instant::now());
        let listing = self
            .env
            .sessions
            .update(cx, |store, cx| store.list_worktrees(workspace, cx));
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = listing.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(list) => {
                        let list = ordered(list);
                        this.count(&list, cx);
                        this.checkouts = Checkouts::Ready(list);
                        this.refreshed_ms = Some(hutan_client::time::now_ms());
                        this.refresh_error = None;
                    }
                    // The list on screen stays; the panel says it is not current.
                    Err(error) if matches!(this.checkouts, Checkouts::Ready(_)) => {
                        this.refresh_error = Some(error);
                    }
                    Err(error) => this.checkouts = Checkouts::Failed(error),
                }
                if std::mem::take(&mut this.reload_again) {
                    this.refresh(cx);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Reads what each checkout that may have changes changed (the dirty ones and the branches
    /// of worktrees; the checkout the window works in first), at most [`MAX_COUNTED`], for the
    /// counts on the headers and the files under them. A checkout that cannot be read is left
    /// out; its header stays as it was.
    fn count(&mut self, list: &[WorktreeInfo], cx: &mut Context<Self>) {
        let current = self.current_in(list, cx).map(|w| w.path.clone());
        self.counted_for.clone_from(&current);
        let mut wanted: Vec<PathBuf> = list
            .iter()
            .filter(|w| w.dirty || !w.main || Some(&w.path) == current.as_ref())
            .map(|w| w.path.clone())
            .collect();
        if let Some(current) = current
            && let Some(at) = wanted.iter().position(|p| *p == current)
        {
            let first = wanted.remove(at);
            wanted.insert(0, first);
        }
        wanted.truncate(MAX_COUNTED);
        self.diffs.retain(|path, _| wanted.contains(path));
        let generation = self.generation;
        let reads: Vec<_> = wanted
            .iter()
            .map(|path| {
                let request = WorktreeDiffRequest {
                    path: path.clone(),
                    base: None,
                    include_uncommitted: true,
                };
                let read = self
                    .env
                    .sessions
                    .update(cx, |store, cx| store.worktree_diff(request, cx));
                (path.clone(), read)
            })
            .collect();
        self.counts_task = Some(cx.spawn(async move |this, cx| {
            for (path, read) in reads {
                let result = read.await;
                let alive = this
                    .update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        match result {
                            Ok(diff) => {
                                this.diffs.insert(path, diff.files);
                            }
                            Err(_) => {
                                this.diffs.remove(&path);
                            }
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    return;
                }
            }
        }));
    }

    /// The checkout the window works in, among `list`: the worktree the user picked, else the
    /// main checkout of the project they work in, else the first main checkout.
    fn current_in<'a>(&self, list: &'a [WorktreeInfo], cx: &gpui::App) -> Option<&'a WorktreeInfo> {
        let store = self.env.sessions.read(cx);
        // Mawas's folder while her page is in front: the Home folder, or her workspace's project.
        if store.mawas_focus().is_some() {
            let folder = store.files_root();
            return folder
                .and_then(|root| list.iter().find(|w| w.path == root))
                .or_else(|| list.iter().find(|w| w.main));
        }
        if let Some(picked) = store.active_checkout()
            && let Some(found) = list.iter().find(|w| w.path == picked)
        {
            return Some(found);
        }
        if let Some(root) = store
            .active_project()
            .and_then(|p| p.project.as_ref())
            .map(|p| &p.root)
            && let Some(found) = list.iter().find(|w| w.main && w.repo_root == *root)
        {
            return Some(found);
        }
        list.iter().find(|w| w.main)
    }

    /// Whether the workspace has more than one place that changes: several repositories, or a
    /// worktree with changes. Only then is there a choice between the scopes.
    pub fn all_available(&self) -> bool {
        let rows = self.rows();
        let repos: HashSet<&PathBuf> = rows.iter().map(|w| &w.repo_root).collect();
        repos.len() > 1
            || rows
                .iter()
                .any(|w| !w.main && (w.dirty || self.counts(&w.path).is_some_and(|c| c.files > 0)))
    }

    /// The scope the panel shows: the chosen one while [`all_available`](Self::all_available),
    /// else the checkout's.
    pub fn shown_scope(&self) -> ChangesScope {
        if self.all_available() {
            self.changes_scope
        } else {
            ChangesScope::Checkout
        }
    }

    /// The scope the user chose (the chip), whether or not it applies right now.
    pub fn changes_scope(&self) -> ChangesScope {
        self.changes_scope
    }

    /// Switches the chip: *This checkout* or *All in workspace*.
    pub fn set_changes_scope(&mut self, scope: ChangesScope, cx: &mut Context<Self>) {
        if self.changes_scope != scope {
            self.changes_scope = scope;
            cx.notify();
        }
    }

    /// The rows (indexes into [`rows`](Self::rows)) the panel draws: the checkout the window
    /// works in (and any worktree with a setup to tell about), or every checkout of the
    /// workspace.
    pub fn shown(&self, cx: &gpui::App) -> Vec<usize> {
        let rows = self.rows();
        match self.shown_scope() {
            ChangesScope::Workspace => (0..rows.len()).collect(),
            ChangesScope::Checkout => {
                let current = self
                    .current_in(rows, cx)
                    .and_then(|current| rows.iter().position(|w| w.path == current.path));
                // A worktree whose setup runs, failed or waits for the user is news wherever
                // the window works: its row stays in the list until the setup is done.
                (0..rows.len())
                    .filter(|&i| Some(i) == current || setup_line(&rows[i]).is_some())
                    .collect()
            }
        }
    }

    /// The files `checkout` changed, as last read (empty while unread or clean).
    pub fn files_of(&self, checkout: &std::path::Path) -> &[DiffFile] {
        self.diffs.get(checkout).map_or(&[], Vec::as_slice)
    }

    /// Whether the group of `checkout` is folded to its header.
    pub fn is_folded(&self, checkout: &std::path::Path) -> bool {
        self.collapsed.contains(checkout)
    }

    /// Folds or unfolds the group of `checkout`.
    pub fn toggle_folded(&mut self, checkout: &std::path::Path, cx: &mut Context<Self>) {
        if !self.collapsed.remove(checkout) {
            self.collapsed.insert(checkout.to_owned());
        }
        cx.notify();
    }

    /// A checkout of the listing as a group: its header (the checkout, what it changed, Finish…)
    /// and, unless folded, its changed files.
    fn group(&self, index: usize, worktree: &WorktreeInfo, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let files = self.files_of(&worktree.path);
        let folded = self.collapsed.contains(&worktree.path);
        let mut group = div()
            .flex()
            .flex_col()
            .flex_none()
            .child(self.header(index, worktree, cx));
        if !folded {
            let path = worktree.path.display().to_string();
            group = group
                .children(
                    files
                        .iter()
                        .take(MAX_FILES_SHOWN)
                        .enumerate()
                        .map(|(i, file)| self.file_row(index, i, &path, file, cx)),
                )
                .when(files.len() > MAX_FILES_SHOWN, |s| {
                    let rest = files.len() - MAX_FILES_SHOWN;
                    let open = path.clone();
                    s.child(
                        div()
                            .id(SharedString::from(format!("changes-more-{path}")))
                            .relative()
                            .flex_none()
                            .pl(px(tokens.spacing * 2.0 + 40.0))
                            .py(px(tokens.spacing))
                            .cursor_pointer()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .on_click(move |_, window, cx| {
                                window.dispatch_action(Box::new(ReviewChangesAt(open.clone())), cx);
                            })
                            .child(SharedString::from(format!("{rest} more files…")))
                            .child(probe(&self.env.regions, Region::Changes("panel-more"))),
                    )
                });
        }
        group.into_any_element()
    }

    /// `▾ ⑂ repo · branch   4 files · +4 −0   [Finish…]` over `~/path · main checkout`. A click
    /// opens the checkout's Changes view; the chevron folds the group.
    fn header(&self, index: usize, worktree: &WorktreeInfo, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let hover = theme.sidebar_accent;
        let look = Look::of(cx);
        let counted = self.counts(&worktree.path).filter(|c| c.files > 0);
        let path = worktree.path.clone();
        let click_path = path.display().to_string();
        let finish_path = click_path.clone();
        let folded = self.collapsed.contains(&path);
        let fold_path = path.clone();
        let title = format!(
            "{} · {}",
            repo_name(worktree),
            worktree
                .branch
                .clone()
                .unwrap_or_else(|| "(detached)".to_owned())
        );
        let mut notes: Vec<String> = Vec::new();
        if worktree.main {
            notes.push("main checkout".to_owned());
        }
        if worktree.dirty {
            notes.push("uncommitted changes".to_owned());
        }
        let live = worktree.live_sessions.len();
        if live > 0 {
            notes.push(format!("{live} live"));
        }
        let detail = if notes.is_empty() {
            tilde(&path)
        } else {
            format!("{} · {}", tilde(&path), notes.join(" · "))
        };
        let can_finish = counted.is_some()
            && worktree.branch.is_some()
            && self.env.sessions.read(cx).connection().supports_finish();
        let foldable = counted.is_some();
        let diffstat = self
            .env
            .settings
            .get::<crate::settings::ChangesPanelSettings>()
            .show_diffstat;
        div()
            .id(SharedString::from(format!("changes-row-{click_path}")))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .mx(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing * 1.5))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(move |_, window, cx| {
                window.dispatch_action(Box::new(ReviewChangesAt(click_path.clone())), cx);
            })
            .child(
                div()
                    .id(SharedString::from(format!("changes-fold-{index}")))
                    .flex_none()
                    .w(px(20.0))
                    .flex()
                    .justify_center()
                    .text_color(if worktree.dirty {
                        theme.accent
                    } else {
                        theme.muted
                    })
                    .when(foldable, |s| {
                        s.on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_folded(&fold_path, cx);
                                cx.stop_propagation();
                            }))
                    })
                    .child(crate::icons::glyph(if !foldable {
                        if worktree.dirty {
                            Icon::VcsDirty
                        } else {
                            Icon::VcsClean
                        }
                    } else if folded {
                        Icon::ActionDisclose
                    } else {
                        Icon::ActionExpand
                    }))
                    .child(probe(&self.env.regions, Region::ChangesFold(index))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(tokens.spacing * 2.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.0))
                                    .text_color(theme.fg)
                                    .child(SharedString::from(title)),
                            )
                            .children(counted.map(|c| {
                                div()
                                    .id(SharedString::from(format!("changes-counts-{index}")))
                                    .relative()
                                    .flex_none()
                                    .flex()
                                    .gap(px(tokens.spacing))
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(if diffstat {
                                        format!("{} ·", c.files_text())
                                    } else {
                                        c.files_text()
                                    }))
                                    .when(diffstat, |s| {
                                        s.child(counts_of(c.added, c.removed, &look)).child(probe(
                                            &self.env.regions,
                                            Region::ChangesPanelCounts(index),
                                        ))
                                    })
                            })),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(detail)),
                    )
                    .children(setup_line(worktree).map(|line| {
                        let tone = match line.tone {
                            SetupTone::Busy => theme.accent,
                            SetupTone::Warn => theme.warning,
                            SetupTone::Failed => theme.danger,
                        };
                        let action_path = path.display().to_string();
                        let action = line.action.map(|(label, action)| {
                            ui::Button::new(
                                SharedString::from(format!("changes-setup-action-{index}")),
                                label,
                                &theme,
                                &tokens,
                            )
                            .link()
                            .compact()
                            .on_click(move |_, window, cx| {
                                let path = action_path.clone();
                                match action {
                                    SetupAction::ShowOutput => window.dispatch_action(
                                        Box::new(ShowWorktreeSetupOutput(path)),
                                        cx,
                                    ),
                                    SetupAction::Review => window
                                        .dispatch_action(Box::new(ReviewWorktreeSetup(path)), cx),
                                    SetupAction::RunAgain => window
                                        .dispatch_action(Box::new(RerunWorktreeSetup(path)), cx),
                                }
                            })
                            .child(probe(&self.env.regions, Region::ChangesSetupAction(index)))
                        });
                        div()
                            .id(SharedString::from(format!("changes-setup-{index}")))
                            .flex()
                            .items_center()
                            .gap(px(tokens.spacing * 2.0))
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(tone)
                            .child(SharedString::from(line.text))
                            .children(action)
                            .child(probe(&self.env.regions, Region::ChangesSetup(index)))
                    })),
            )
            .when(can_finish, |s| {
                s.child(
                    ui::Button::new(
                        SharedString::from(format!("changes-finish-{index}")),
                        "Finish…",
                        &theme,
                        &tokens,
                    )
                    .ghost()
                    .compact()
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(FinishChangesAt(finish_path.clone())), cx);
                        cx.stop_propagation();
                    })
                    .child(probe(&self.env.regions, Region::ChangesFinish(index))),
                )
            })
            .child(probe(&self.env.regions, Region::ChangesCheckout(index)))
            .into_any_element()
    }

    /// `M  src/lib.rs   +3 −1`: one changed file of a group; a click opens the checkout's
    /// Changes view on it.
    fn file_row(
        &self,
        group: usize,
        index: usize,
        checkout: &str,
        file: &DiffFile,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let hover = theme.sidebar_accent;
        let look = Look::of(cx);
        let (letter, color) = crate::workbench::changes::status_mark(file.status, &theme);
        let (name, dir) = match file.path.rsplit_once('/') {
            Some((dir, name)) => (name.to_owned(), Some(dir.to_owned())),
            None => (file.path.clone(), None),
        };
        let home = self.env.sessions.read(cx).home_folder();
        let hint = crate::workbench::changes::memory_files_hint(
            home.as_deref(),
            &[std::path::Path::new(checkout)],
            &file.path,
        );
        let action = ChangedFileAt {
            checkout: checkout.to_owned(),
            file: file.path.clone(),
        };
        let tip = SharedString::from(file.path.clone());
        div()
            .id(SharedString::from(format!(
                "changes-file-{checkout}-{}",
                file.path
            )))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .mx(px(tokens.spacing * 2.0))
            .pl(px(tokens.spacing * 2.0 + 20.0 + tokens.spacing * 2.0))
            .pr(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .tooltip(move |_, cx| {
                cx.new(|_| crate::workbench::style::TextTooltip::new(tip.clone()))
                    .into()
            })
            .on_click(move |_, window, cx| {
                window.dispatch_action(Box::new(ReviewFileAt(action.clone())), cx);
            })
            .child(
                div()
                    .flex_none()
                    .w(px(12.0))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(color)
                    .child(letter),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .gap(px(tokens.spacing))
                    .text_size(px(tokens.font_size))
                    .child(
                        div()
                            .flex_none()
                            .max_w_full()
                            .truncate()
                            .child(SharedString::from(name)),
                    )
                    .children(
                        hint.map(|hint| {
                            div()
                                .flex_none()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(hint)
                                .child(probe(
                                    &self.env.regions,
                                    Region::ChangesPanelMemoryHint(group, index),
                                ))
                        })
                        .or_else(|| {
                            dir.map(|dir| {
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(dir))
                            })
                        }),
                    ),
            )
            .children(if file.binary {
                Some(
                    div()
                        .flex_none()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child("binary")
                        .into_any_element(),
                )
            } else {
                self.env
                    .settings
                    .get::<crate::settings::ChangesPanelSettings>()
                    .show_diffstat
                    .then(|| {
                        div()
                            .relative()
                            .flex_none()
                            .child(counts_of(
                                file.additions as usize,
                                file.deletions as usize,
                                &look,
                            ))
                            .child(probe(
                                &self.env.regions,
                                Region::ChangesPanelFileCounts(group, index),
                            ))
                            .into_any_element()
                    })
            })
            .child(probe(
                &self.env.regions,
                Region::ChangesPanelFile(group, index),
            ))
            .into_any_element()
    }

    /// *This checkout · All in workspace*: only when the workspace has more than one place
    /// that changes.
    fn scope_switch(&self, theme: &Theme, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let shown = self.shown_scope();
        let segment = |id: &'static str, label: &'static str, scope: ChangesScope| {
            ui::segment(id, label, shown == scope, theme, tokens)
                .on_click(cx.listener(move |this, _, _, cx| this.set_changes_scope(scope, cx)))
                .child(probe(&self.env.regions, Region::Changes(id)))
        };
        div()
            .flex()
            .flex_none()
            .px(px(tokens.spacing * 3.0))
            .pb(px(tokens.spacing * 2.0))
            .child(
                ui::segmented(theme, tokens)
                    .child(segment(
                        "panel-scope-checkout",
                        "This checkout",
                        ChangesScope::Checkout,
                    ))
                    .child(segment(
                        "panel-scope-workspace",
                        "All in workspace",
                        ChangesScope::Workspace,
                    )),
            )
            .into_any_element()
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
        let default_scope = self.env.settings.get::<ChangesSettings>().default_scope;
        if default_scope != self.default_scope {
            self.default_scope = default_scope;
            self.changes_scope = default_scope;
        }
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        // Drawn means visible: that is when stale checkouts are listed again.
        if self
            .loaded_at
            .is_some_and(|at| at.elapsed() > REFRESH_AFTER)
            && matches!(self.checkouts, Checkouts::Ready(_) | Checkouts::Failed(_))
        {
            self.refresh(cx);
        }

        let refresh = ui::IconButton::new(
            "changes-refresh",
            Icon::ActionRefresh,
            "Refresh",
            &theme,
            &tokens,
        )
        .compact()
        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
        .child(probe(&self.env.regions, Region::Changes("panel-refresh")))
        .into_any_element();

        let shown = self.shown(cx);
        let all = self.all_available();
        let body: AnyElement = match &self.checkouts {
            Checkouts::Ready(list) if !shown.is_empty() => div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(probe(&self.env.regions, Region::Changes("panel-list")))
                .child(
                    div()
                        .id("changes-list")
                        .size_full()
                        .overflow_y_scroll()
                        .restrict_scroll_to_axis()
                        .track_scroll(&self.scroll)
                        .flex()
                        .flex_col()
                        .children(shown.iter().map(|&i| self.group(i, &list[i], cx))),
                )
                .child(self.bar.element())
                .into_any_element(),
            other => {
                // At Home, an empty listing means the Home folder is not a repository: say so
                // and offer to track it (a confirm sheet follows).
                let track = matches!(other, Checkouts::Ready(_))
                    && self.env.sessions.read(cx).can_track_home()
                    && self
                        .env
                        .sessions
                        .read(cx)
                        .scope_container()
                        .is_some_and(|w| w.kind == WorkspaceKind::Home);
                if track {
                    let button = ui::Button::new(
                        "changes-track-home",
                        "Track changes in this folder…",
                        &theme,
                        &tokens,
                    )
                    .primary()
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(TrackHomeChanges), cx);
                    })
                    .child(probe(&self.env.regions, Region::Changes("track-home")))
                    .into_any_element();
                    ui::empty_state(
                        Mascot::Empty,
                        "The Home folder does not track its changes yet. Track it to see what \
                         Mawas edits here and to rewind her turns.",
                        Some(button),
                        &theme,
                        &tokens,
                    )
                    .into_any_element()
                } else {
                    let text = match other {
                        Checkouts::Idle => {
                            "Connect to hutand and open a project to see its changes.".to_owned()
                        }
                        Checkouts::Loading => "Listing the checkouts…".to_owned(),
                        Checkouts::Failed(error) => format!("Cannot list the checkouts: {error}"),
                        Checkouts::Ready(_) => "This workspace has no git checkout.".to_owned(),
                    };
                    ui::empty_state(Mascot::Empty, text, None, &theme, &tokens).into_any_element()
                }
            }
        };

        // A refresh that failed leaves the list standing; say how old it is.
        let stale = self.refresh_error.as_ref().map(|error| {
            let age = self.refreshed_ms.map_or_else(String::new, |at| {
                format!(" from {}", hutan_client::time::relative_to_now(at))
            });
            let tip = SharedString::from(error.clone());
            div()
                .id("changes-stale")
                .flex_none()
                .px(px(tokens.spacing * 3.0))
                .pb(px(tokens.spacing))
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.warning)
                .tooltip(move |_, cx| {
                    cx.new(|_| crate::workbench::style::TextTooltip::new(tip.clone()))
                        .into()
                })
                .child(SharedString::from(format!(
                    "Couldn't refresh · showing the list{age}"
                )))
                .child(probe(&self.env.regions, Region::Changes("panel-stale")))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(32.0))
                    .px(px(tokens.spacing * 3.0))
                    .child(
                        div()
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap(px(tokens.spacing))
                            .children(super::common::scope_chip(
                                self.scope(cx).as_ref(),
                                "changes",
                                &self.env.regions,
                                cx,
                            ))
                            .child(refresh),
                    ),
            )
            .when(all, |s| s.child(self.scope_switch(&theme, &tokens, cx)))
            .children(stale)
            .child(body)
    }
}
