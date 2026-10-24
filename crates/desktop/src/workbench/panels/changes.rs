//! The `changes` panel, the right sidebar's **Changes** tab: the checkouts of the workspace you
//! are in (the main checkout and every worktree), the ones with uncommitted changes first. A
//! click opens that checkout's full Changes view (`ReviewChangesAt`) as a tab of the center.
//!
//! The daemon lists them (`worktree.list`); the panel asks while it is drawn (when it opens,
//! when the workspace or the connection changes, and again after [`REFRESH_AFTER`]) and has the
//! Refresh button for the rest. It never reads a repository itself.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Task, Window, div, px,
};
use hutan_client::ConnectionState;
use hutan_core::WorkspaceId;
use hutan_protocol::messages::WorkspaceKind;
use hutan_protocol::worktree::{DiffFile, WorktreeDiffRequest, WorktreeInfo};

use crate::icons::Icon;
use crate::sessions::CheckoutActivity;
use crate::theme::DensityExt as _;
use crate::workbench::Region;
use crate::workbench::actions::{ReviewChangesAt, TrackHomeChanges};
use crate::workbench::chat::{Look, counts_of};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::UiStyle;
use crate::workbench::ui::{self, Mascot};
use crate::workbench::workspaces::tilde;

/// A drawn panel lists the checkouts again after this long.
pub const REFRESH_AFTER: Duration = Duration::from_secs(15);

/// Checkouts whose changes are counted for their rows; the rest show no count.
const MAX_COUNTED: usize = 12;

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
    /// listing; a checkout that could not be counted has no entry.
    counts: HashMap<PathBuf, Counts>,
    counts_task: Option<Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl std::fmt::Debug for ChangesPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangesPanel")
            .field("checkouts", &self.checkouts)
            .finish_non_exhaustive()
    }
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
        ];
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
            counts: HashMap::new(),
            counts_task: None,
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
        self.counts.get(checkout).copied()
    }

    /// What the panel is about: the container the window shows (the chip of its header).
    pub fn scope(&self, cx: &gpui::App) -> Option<crate::sessions::ScopeLabel> {
        self.env.sessions.read(cx).container_scope()
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
                store.active_workspace_id(),
                store.connection_state(),
                store.connection().supports_worktree_review(),
            )
        };
        let wanted = workspace.map(|w| (w, state));
        if wanted == self.asked {
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
                self.counts.clear();
                self.counts_task = None;
                self.refreshed_ms = None;
                self.refresh_error = None;
                cx.notify();
            }
        }
    }

    /// Lists the checkouts of `workspace` again (the Refresh button).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let workspace = self.env.sessions.read(cx).active_workspace_id();
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
    /// of worktrees), at most [`MAX_COUNTED`], for the counts on the rows. A count that cannot
    /// be read is left out; the row stays as it was.
    fn count(&mut self, list: &[WorktreeInfo], cx: &mut Context<Self>) {
        let wanted: Vec<PathBuf> = list
            .iter()
            .filter(|w| w.dirty || !w.main)
            .take(MAX_COUNTED)
            .map(|w| w.path.clone())
            .collect();
        self.counts.retain(|path, _| wanted.contains(path));
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
                                this.counts.insert(path, Counts::of(&diff.files));
                            }
                            Err(_) => {
                                this.counts.remove(&path);
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

    fn row(&self, index: usize, worktree: &WorktreeInfo, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let hover = theme.sidebar_accent;
        let look = Look::of(cx);
        let counted = self.counts.get(&worktree.path).filter(|c| c.files > 0);
        let path = worktree.path.clone();
        let click_path = path.display().to_string();
        let title = worktree
            .branch
            .clone()
            .unwrap_or_else(|| "(detached)".to_owned());
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
        div()
            .id(SharedString::from(format!("changes-row-{click_path}")))
            .relative()
            .flex()
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
                    .flex_none()
                    .w(px(20.0))
                    .flex()
                    .justify_center()
                    .text_color(if worktree.dirty {
                        theme.accent
                    } else {
                        theme.muted
                    })
                    .child(crate::icons::glyph(if worktree.dirty {
                        Icon::VcsDirty
                    } else {
                        Icon::VcsClean
                    })),
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
                                    .flex_none()
                                    .flex()
                                    .gap(px(tokens.spacing))
                                    .text_size(px(tokens.secondary_font_size))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(format!("{} ·", c.files_text())))
                                    .child(counts_of(c.added, c.removed, &look))
                            })),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(detail)),
                    ),
            )
            .child(probe(&self.env.regions, Region::ChangesCheckout(index)))
            .into_any_element()
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.env.regions.rendered(cx.entity_id());
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

        let body: AnyElement = match &self.checkouts {
            Checkouts::Ready(list) if !list.is_empty() => div()
                .id("changes-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(list.iter().enumerate().map(|(i, w)| self.row(i, w, cx)))
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
                        .active_workspace()
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
                    cx.new(|_| crate::workbench::style::TextTooltip(tip.clone()))
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
            .children(stale)
            .child(body)
    }
}
