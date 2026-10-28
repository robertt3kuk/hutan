//! `chat.fork` (protocol 0.56; where the fork works, 0.58): the part that needs more than the
//! registry — a new worktree with the files of the turn, or the files of this folder put back —
//! and the parent's archiving.
//!
//! Everything that can be refused is decided first (`ChatManager::plan_fork`, the turn's
//! checkpoint, the restore's preview), so a refusal changes nothing. Then, in this order: the
//! worktree is made (or the files restored), the forked session is started in it, and the parent
//! is closed when asked. A fork that cannot start takes the worktree away again, or undoes the
//! restore through its own checkpoint, and says so.

use std::path::{Path, PathBuf};

use hutan_core::{ForkSupport, RestoreAction, TurnId};
use hutan_protocol::chat::{ChatFork, ChatForked, ForkDone, ForkWhere, ForkWorktree};
use hutan_protocol::checkpoint::{CheckpointPreviewRequest, CheckpointRestore, CheckpointRestored};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::SessionClose;
use tracing::warn;

use super::{ConnHandle, Router};
use crate::chat::ForkParent;
use crate::resources::{Actor, audit};
use crate::worktrees::ForkWorktreeSpec;

/// The folder a fork works in inside its worktree: the same place below the checkout's top
/// level as the parent's folder, when the worktree has it, else the worktree's top level.
fn folder_in(parent_cwd: &Path, checkout: &Path, worktree: &Path) -> PathBuf {
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let inside = canonical(parent_cwd)
        .strip_prefix(canonical(checkout))
        .ok()
        .map(Path::to_owned)
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| worktree.join(relative))
        .filter(|folder| folder.is_dir());
    inside.unwrap_or_else(|| worktree.to_owned())
}

/// `fork/<slug of the name>-t<N>`: the branch a fork's worktree gets unless it is named.
fn default_branch(name: &str, ordinal: u32) -> String {
    let named = name.chars().any(|c| c.is_ascii_alphanumeric());
    let slug = if named {
        hutan_core::task::slug(name)
    } else {
        "chat".to_owned()
    };
    format!("fork/{slug}-t{ordinal}")
}

impl Router {
    /// `chat.fork`.
    pub(super) async fn fork_chat(
        &self,
        conn: &ConnHandle,
        req: ChatFork,
    ) -> Result<ChatForked, ErrorBody> {
        let chats = self.registry.chats();
        let parent = self.registry.fork_parent(req.session_id).await?;
        let plan = chats.plan_fork(&parent, &req).await?;
        let origin = plan.origin.clone();
        let support = plan.support;
        let mut done = ForkDone {
            mode: origin.mode,
            support: support.unwrap_or(ForkSupport::None),
            turn_id: origin.turn_id,
            ordinal: origin.ordinal,
            copied_events: plan.copied_events,
            worktree: None,
            restored: None,
            parent_archived: false,
            notes: Vec::new(),
        };

        // What changes outside the new session, once nothing else can refuse.
        let mut cwd = None;
        let mut made: Option<(PathBuf, PathBuf, String)> = None;
        match req.where_ {
            ForkWhere::NewWorktree => {
                let files = chats
                    .checkpoints()
                    .turn_files(parent.id, origin.turn_id)
                    .await?;
                let name = req.title.as_deref().unwrap_or(&parent.title);
                let spec = ForkWorktreeSpec {
                    workspace_id: parent.workspace_id,
                    checkout: files.checkout.clone(),
                    branch: req
                        .branch
                        .clone()
                        .unwrap_or_else(|| default_branch(name, origin.ordinal)),
                    default_branch: req.branch.is_none(),
                    base: files.head.clone(),
                    files: files.commit.clone(),
                };
                let created = self.worktrees.create_for_fork(spec).await?;
                let path = created.worktree.path.clone();
                // The worktree's setup comes first, like it does for a task.
                self.worktrees.wait_for_setup(&path).await;
                cwd = Some(folder_in(&parent.cwd, &files.checkout, &path));
                made = Some((created.main.clone(), path.clone(), created.branch.clone()));
                done.worktree = Some(ForkWorktree {
                    path,
                    branch: created.branch,
                    base: created.base,
                    files: files.from,
                    checkpoint_id: files.checkpoint,
                    skipped: files.skipped,
                    setup: created.setup,
                });
            }
            ForkWhere::ThisFolder if req.restore_files => {
                done.restored = self.restore_files(conn, &parent, origin.turn_id).await?;
            }
            ForkWhere::ThisFolder => {}
        }

        let session = match self
            .registry
            .start_fork(parent.id, req.title.clone(), plan, cwd)
            .await
        {
            Ok(session) => session,
            Err(mut error) => {
                if let Some((main, path, branch)) = &made
                    && !self
                        .worktrees
                        .discard_fork_worktree(main, path, branch)
                        .await
                {
                    error.message = format!(
                        "{}; the worktree {} (branch {branch}) was made for it and could not be \
                         removed",
                        error.message,
                        path.display()
                    );
                }
                if let Some(restored) = &done.restored {
                    let undone = self.undo_restore(conn, restored).await;
                    error.message = format!(
                        "{}; the files of {} {}",
                        error.message,
                        parent.cwd.display(),
                        if undone {
                            "were put back to how they were (the restore was undone)"
                        } else {
                            "were restored for it and the restore could not be undone: `hutan \
                             checkpoint undo` does it"
                        }
                    );
                }
                return Err(error);
            }
        };

        // What the new agent said it can do, else what the parent's did.
        done.support = session
            .chat
            .as_ref()
            .and_then(|chat| chat.fork.as_ref())
            .map(|fork| fork.support)
            .or(support)
            .unwrap_or(ForkSupport::None);

        if req.archive_parent {
            let close = SessionClose {
                session_id: parent.id,
                stop_running: false,
            };
            let closed = self.registry.close_session(close).await;
            audit(
                Actor::of(conn),
                "session.close",
                &parent.id.to_string(),
                &closed,
            );
            match closed {
                Ok(()) => done.parent_archived = true,
                Err(error) => done.notes.push(format!(
                    "the original chat could not be closed: {}",
                    error.message
                )),
            }
        }
        Ok(ChatForked {
            session,
            fork: done,
        })
    }

    /// Brings the parent's folder back to the files turn `turn` left, through the restore of
    /// `checkpoint.restore` over the turns after it. `None`: no turn after it changed a file.
    /// `conflict` (with the preview as its data) when a file changed since in a way that is not
    /// the agent's: that is for the user to look at, not for a fork to overwrite.
    async fn restore_files(
        &self,
        conn: &ConnHandle,
        parent: &ForkParent,
        turn: TurnId,
    ) -> Result<Option<CheckpointRestored>, ErrorBody> {
        let checkpoints = self.registry.chats().checkpoints();
        let Some((first, last)) = checkpoints.restore_span(parent.id, turn).await? else {
            return Ok(None);
        };
        let through = (last != first).then_some(last);
        let ctx = self.restore_context().await;
        let preview = checkpoints
            .preview(
                CheckpointPreviewRequest {
                    checkpoint_id: first,
                    through,
                    paths: Vec::new(),
                },
                &ctx,
            )
            .await?;
        if preview
            .entries
            .iter()
            .any(|entry| entry.state.is_conflict())
        {
            let mut error = ErrorBody::new(
                ErrorCode::Conflict,
                "some files changed after that turn in a way the turns do not explain: restoring \
                 them would overwrite those changes. Nothing was changed; review them (`hutan \
                 checkpoint restore`) or fork without `restore_files`",
            );
            error.data = serde_json::to_value(&preview).ok();
            return Err(error);
        }
        if preview
            .entries
            .iter()
            .all(|entry| entry.action == RestoreAction::Skip)
        {
            return Ok(None);
        }
        let restore = CheckpointRestore {
            checkpoint_id: first,
            through,
            paths: Vec::new(),
            accept_conflicts: Vec::new(),
            plan_digest: preview.plan_digest,
        };
        let restored = checkpoints.restore(restore, ctx).await;
        audit(
            Actor::of(conn),
            "checkpoint.restore",
            &first.to_string(),
            &restored,
        );
        restored.map(Some)
    }

    /// Undo of a restore a fork made, when the fork then failed. Whether it worked.
    async fn undo_restore(&self, conn: &ConnHandle, restored: &CheckpointRestored) -> bool {
        let checkpoints = self.registry.chats().checkpoints();
        let id = restored.checkpoint.id;
        let ctx = self.restore_context().await;
        let outcome = async {
            let preview = checkpoints
                .preview(
                    CheckpointPreviewRequest {
                        checkpoint_id: id,
                        through: None,
                        paths: Vec::new(),
                    },
                    &ctx,
                )
                .await?;
            let undone = checkpoints
                .restore(
                    CheckpointRestore {
                        checkpoint_id: id,
                        through: None,
                        paths: Vec::new(),
                        accept_conflicts: Vec::new(),
                        plan_digest: preview.plan_digest,
                    },
                    ctx.clone(),
                )
                .await;
            audit(
                Actor::of(conn),
                "checkpoint.restore",
                &id.to_string(),
                &undone,
            );
            undone.map(|_| ())
        }
        .await;
        if let Err(error) = &outcome {
            warn!(checkpoint = %id, message = %error.message, "the restore of a failed fork was not undone");
        }
        outcome.is_ok()
    }
}
