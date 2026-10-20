//! The two worktree commands of the ⌘K palette (protocol 0.14, `docs/orchestration.md`):
//! "Worktree: New Terminal in Worktree…" (a branch name; the worktree is made when the branch
//! has none) and "Worktree: Remove…" (a picker of the worktrees of the active workspace, then a
//! confirmation). Everything else about worktrees is the CLI's.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{Context, SharedString, Window};
use hutan_client::ConnectionState;
use hutan_core::WorkspaceId;
use hutan_protocol::worktree::{WorktreeCreate, WorktreeInfo, WorktreeRemove};

use super::modal::{Modal, PromptSpec};
use super::view::Workbench;
use super::workspaces::{PickerAction, PickerEntry, PickerKind, PickerState, tilde};
use crate::sessions::SessionsStore;

/// A worktree picked for removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeChoice {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub dirty: bool,
}

impl Workbench {
    /// The workspace worktree commands act on: the active one (Home when none is shown).
    fn worktree_workspace(&self, store: &SessionsStore) -> Option<WorkspaceId> {
        self.target_workspace(store).map(|w| w.id)
    }

    fn worktrees_unavailable(&mut self, cx: &mut Context<Self>) -> bool {
        let store = self.sessions.read(cx);
        if store.connection_state() != ConnectionState::Connected {
            self.set_notice("Not connected to hutand", cx);
            return true;
        }
        if !store.connection().supports_worktrees() {
            self.set_notice(
                "This hutand does not offer worktrees; restart it to update",
                cx,
            );
            return true;
        }
        false
    }

    /// "Worktree: New Terminal in Worktree…": asks for a branch, makes (or reuses) its
    /// worktree next to the repository of the active workspace, and opens a terminal in it.
    pub fn new_terminal_in_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.worktrees_unavailable(cx) {
            return;
        }
        let Some(workspace) = self.worktree_workspace(self.sessions.read(cx)) else {
            return;
        };
        self.open_prompt(
            PromptSpec {
                title: "New terminal in worktree",
                message: "Branch (an existing branch is checked out, a new one starts from HEAD):",
                initial: "",
                confirm_label: "Open",
            },
            move |this, branch, window, cx| {
                this.start_in_worktree(workspace, branch.trim().to_owned(), window, cx);
            },
            window,
            cx,
        );
    }

    fn start_in_worktree(
        &mut self,
        workspace: WorkspaceId,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let listing = self
            .sessions
            .update(cx, |store, cx| store.list_worktrees(workspace, cx));
        cx.spawn_in(window, async move |this, cx| {
            let failed = |this: &mut Workbench, err: String, cx: &mut Context<Workbench>| {
                this.set_notice(format!("Cannot start a terminal in a worktree: {err}"), cx);
            };
            // A branch that already has a worktree is reused; the main checkout does not count.
            let existing = listing.await.ok().and_then(|list| {
                list.into_iter()
                    .find(|w| !w.main && w.branch.as_deref() == Some(branch.as_str()))
            });
            let path = match existing {
                Some(worktree) => worktree.path,
                None => {
                    let creating = this.update(cx, |this, cx| {
                        this.sessions.update(cx, |store, cx| {
                            store.create_worktree(
                                WorktreeCreate {
                                    workspace_id: workspace,
                                    repo: None,
                                    branch: branch.clone(),
                                    base: None,
                                    path: None,
                                },
                                cx,
                            )
                        })
                    });
                    let Ok(creating) = creating else { return };
                    match creating.await {
                        Ok(worktree) => worktree.path,
                        Err(err) => {
                            this.update(cx, |this, cx| failed(this, err, cx)).ok();
                            return;
                        }
                    }
                }
            };
            let starting = this.update(cx, |this, cx| {
                let spec = this.new_terminal_spec(Some(workspace), Some(path), cx);
                this.sessions.update(cx, |store, cx| {
                    store.create_session(Some(workspace), spec, cx)
                })
            });
            let Ok(starting) = starting else { return };
            match starting.await {
                Ok(info) => {
                    this.update_in(cx, |this, window, cx| {
                        this.open_session(info.id, window, cx);
                    })
                    .ok();
                }
                Err(err) => {
                    this.update(cx, |this, cx| failed(this, err, cx)).ok();
                }
            }
        })
        .detach();
    }

    /// "Worktree: Remove…": a picker of the worktrees of the active workspace's repositories
    /// (the main checkouts cannot be removed and are not listed).
    pub fn open_worktree_remove_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.worktrees_unavailable(cx) {
            return;
        }
        let Some(workspace) = self.worktree_workspace(self.sessions.read(cx)) else {
            return;
        };
        let mut state = PickerState::new(PickerKind::Worktrees, Vec::new());
        state.loading = true;
        self.show_picker(state, window, cx);
        let listing = self
            .sessions
            .update(cx, |store, cx| store.list_worktrees(workspace, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = listing.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(worktrees) => this.fill_worktrees(worktrees),
                    Err(err) => {
                        if let Some(picker) = &mut this.picker
                            && picker.kind == PickerKind::Worktrees
                        {
                            picker.loading = false;
                        }
                        this.set_notice(format!("Cannot list the worktrees: {err}"), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn fill_worktrees(&mut self, worktrees: Vec<WorktreeInfo>) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        if picker.kind != PickerKind::Worktrees {
            return;
        }
        picker.entries = worktrees
            .into_iter()
            .filter(|w| !w.main)
            .map(|w| {
                let mut notes: Vec<String> = Vec::new();
                if w.dirty {
                    notes.push("uncommitted changes".to_owned());
                }
                if !w.live_sessions.is_empty() {
                    notes.push(format!("{} live session(s)", w.live_sessions.len()));
                }
                if w.locked {
                    notes.push("locked".to_owned());
                }
                let place = tilde(&w.path);
                let detail = if notes.is_empty() {
                    place
                } else {
                    format!("{place} · {}", notes.join(", "))
                };
                PickerEntry {
                    icon: crate::icons::Icon::VcsWorktree,
                    title: w
                        .branch
                        .clone()
                        .unwrap_or_else(|| "(detached)".to_owned())
                        .into(),
                    detail: detail.into(),
                    action: PickerAction::RemoveWorktree(WorktreeChoice {
                        path: w.path,
                        branch: w.branch,
                        dirty: w.dirty,
                    }),
                    fixed: false,
                }
            })
            .collect();
        picker.loading = false;
        picker.refilter();
    }

    /// Asks before removing the worktree the picker chose; a checkbox also deletes its branch
    /// (only when merged: the daemon uses `git branch -d`).
    pub(crate) fn confirm_remove_worktree(
        &mut self,
        choice: WorktreeChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = if choice.dirty {
            format!(
                "{}\n\nIt has uncommitted changes that will be lost.",
                choice.path.display()
            )
        } else {
            choice.path.display().to_string()
        };
        self.open_modal(
            Modal {
                title: "Remove worktree".into(),
                message: message.into(),
                confirm_label: "Remove".into(),
                input: None,
                secure_input: false,
                choice: None,
                checkbox: Some(super::modal::ModalCheckbox {
                    label: SharedString::from("Also delete the branch if it is merged"),
                    checked: false,
                }),
                on_confirm: Rc::new(move |this, result, window, cx| {
                    this.remove_worktree(choice.clone(), result.checked, window, cx);
                }),
            },
            window,
            cx,
        );
    }

    fn remove_worktree(
        &mut self,
        choice: WorktreeChoice,
        delete_branch: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let removing = self.sessions.update(cx, |store, cx| {
            store.remove_worktree(
                WorktreeRemove {
                    path: choice.path.clone(),
                    // The dialog warned about the changes: confirming removes them.
                    force: choice.dirty,
                    delete_branch,
                },
                cx,
            )
        });
        cx.spawn_in(window, async move |this, cx| {
            let notice = match removing.await {
                Ok(removed) => match removed.branch_kept {
                    Some(reason) => format!(
                        "Removed {}; branch {} kept: {reason}",
                        choice.path.display(),
                        choice.branch.as_deref().unwrap_or("-")
                    ),
                    None => format!("Removed {}", choice.path.display()),
                },
                Err(err) => format!("Cannot remove the worktree: {err}"),
            };
            this.update(cx, |this, cx| this.set_notice(notice, cx)).ok();
        })
        .detach();
    }
}
