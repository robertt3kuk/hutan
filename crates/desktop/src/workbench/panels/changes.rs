//! The `changes` panel, the right sidebar's **Changes** tab: the checkouts of the workspace you
//! are in (the main checkout and every worktree), the ones with uncommitted changes first. A
//! click opens that checkout's full Changes view (`ReviewChangesAt`) as a tab of the center.
//!
//! The daemon lists them (`worktree.list`); the panel asks while it is drawn (when it opens,
//! when the workspace or the connection changes, and again after [`REFRESH_AFTER`]) and has the
//! Refresh button for the rest. It never reads a repository itself.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, div, px,
};
use hutan_client::ConnectionState;
use hutan_core::WorkspaceId;
use hutan_protocol::worktree::WorktreeInfo;

use crate::theme::DensityExt as _;
use crate::workbench::Region;
use crate::workbench::actions::ReviewChangesAt;
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::UiStyle;
use crate::workbench::ui::{self, Mascot};
use crate::workbench::workspaces::tilde;

/// A drawn panel lists the checkouts again after this long.
pub const REFRESH_AFTER: Duration = Duration::from_secs(15);

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
    generation: u64,
    task: Option<Task<()>>,
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
        cx.observe(&env.sessions, |this, _, cx| this.sync(cx))
            .detach();
        let mut panel = Self {
            env,
            checkouts: Checkouts::Idle,
            asked: None,
            loaded_at: None,
            generation: 0,
            task: None,
        };
        panel.sync(cx);
        panel
    }

    /// What the panel lists (tests).
    pub fn checkouts(&self) -> &Checkouts {
        &self.checkouts
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
                self.checkouts = Checkouts::Idle;
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
                this.checkouts = match result {
                    Ok(list) => Checkouts::Ready(ordered(list)),
                    Err(error) => Checkouts::Failed(error),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn row(&self, index: usize, worktree: &WorktreeInfo, cx: &mut Context<Self>) -> AnyElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let hover = theme.sidebar_accent;
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
                    .child(if worktree.dirty { "●" } else { "±" }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.0))
                            .text_color(theme.fg)
                            .child(SharedString::from(title)),
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
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let hover = theme.surface_hover;
        // Drawn means visible: that is when stale checkouts are listed again.
        if self
            .loaded_at
            .is_some_and(|at| at.elapsed() > REFRESH_AFTER)
            && matches!(self.checkouts, Checkouts::Ready(_) | Checkouts::Failed(_))
        {
            self.refresh(cx);
        }

        let refresh = div()
            .id("changes-refresh")
            .relative()
            .px(px(tokens.spacing * 1.5))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .text_color(theme.muted)
            .hover(move |s| s.bg(hover).text_color(theme.fg))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
            .child("↻")
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
        };

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
                            .text_size(px(12.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.muted)
                            .child("Changes"),
                    )
                    .child(refresh),
            )
            .child(body)
    }
}
