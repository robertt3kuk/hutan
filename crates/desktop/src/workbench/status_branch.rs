//! The branch the `workspace` status item shows (FX-STATUSBAR): the branch checked out in the
//! main checkout of the project the user works in, as the daemon lists it (`worktree.list`; the
//! desktop never reads a repository). A focused session that works in a linked worktree says its
//! own branch instead: that is the checkout the user is in.
//!
//! The listing is asked for when the status bar is drawn and the project changed or the answer
//! is [`REFRESH`] old, and again after a command or a turn ended in a checkout
//! ([`CheckoutActivity`](crate::sessions::CheckoutActivity)). Nothing is polled while the window
//! does not draw.

use std::time::{Duration, Instant};

use gpui::{App, Context, Task};
use hutan_core::WorkspaceId;
use hutan_protocol::worktree::PlacementKind;

use super::view::Workbench;

/// The branch is read again when the status bar draws after this long.
const REFRESH: Duration = Duration::from_secs(15);

/// What the status bar knows of the branch of the project it shows.
#[derive(Default)]
pub(crate) struct StatusBranch {
    /// The project `branch` belongs to.
    project: Option<WorkspaceId>,
    /// The main checkout's branch; `None` while unknown, detached, or outside git.
    branch: Option<String>,
    /// When the listing was last asked for.
    tried: Option<Instant>,
    task: Option<Task<()>>,
}

impl StatusBranch {
    /// Something ran in a checkout: the next draw asks again.
    pub(crate) fn mark_stale(&mut self) {
        self.tried = None;
    }
}

impl Workbench {
    /// Called when the status bar draws: keeps the branch of the active project current.
    pub(crate) fn follow_status_branch(&mut self, cx: &mut Context<Self>) {
        let (project, listable) = {
            let store = self.sessions.read(cx);
            (
                store.active_project().map(|p| p.id),
                store.connection().supports_worktrees(),
            )
        };
        let Some(project) = project else {
            self.status_branch = StatusBranch::default();
            return;
        };
        if self.status_branch.project != Some(project) {
            self.status_branch = StatusBranch {
                project: Some(project),
                ..StatusBranch::default()
            };
        }
        let due = self
            .status_branch
            .tried
            .is_none_or(|at| at.elapsed() > REFRESH);
        if !due || !listable || !self.daemon_connected(cx) || self.status_branch.task.is_some() {
            return;
        }
        self.status_branch.tried = Some(Instant::now());
        let listing = self
            .sessions
            .update(cx, |store, cx| store.list_worktrees(project, cx));
        self.status_branch.task = Some(cx.spawn(async move |this, cx| {
            let result = listing.await;
            this.update(cx, |this, cx| {
                this.status_branch.task = None;
                if this.status_branch.project != Some(project) {
                    return;
                }
                match result {
                    Ok(list) => {
                        let branch = list.iter().find(|w| w.main).and_then(|w| w.branch.clone());
                        if branch != this.status_branch.branch {
                            this.status_branch.branch = branch;
                            cx.notify();
                        }
                    }
                    Err(error) => tracing::debug!("the status bar's worktree.list failed: {error}"),
                }
            })
            .ok();
        }));
    }

    /// The branch of the checkout the user is in: the focused session's worktree branch, else
    /// the project's main checkout.
    pub fn workspace_branch(&self, cx: &App) -> Option<String> {
        let store = self.sessions.read(cx);
        let placement = store
            .focused()
            .and_then(|id| store.session(id))
            .and_then(|session| session.placement.as_ref())
            .filter(|placement| placement.kind == PlacementKind::Worktree);
        match placement {
            Some(placement) => placement.branch.clone(),
            None => self.status_branch.branch.clone(),
        }
    }

    /// The name the `workspace` status item shows: the project the user works in, else the
    /// workspace the window shows.
    pub fn workspace_label(&self, cx: &App) -> Option<String> {
        let store = self.sessions.read(cx);
        store
            .active_project()
            .or_else(|| store.active_workspace())
            .map(|w| w.name.clone())
    }
}
