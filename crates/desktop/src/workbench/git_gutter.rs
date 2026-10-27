//! Git change marks in file tabs (EDITOR-POLISH): the workbench fetches the file the marks are
//! drawn against once — `git.file` of `HEAD`, and the file's staged hunks of `worktree.hunks`
//! applied to it, which is the index — and gives it to the buffer
//! ([`FileBuffer::set_git_base`]); the buffer diffs its own text against it after every edit.
//! The base is fetched again when the file loads or saves, when its tab is shown again, after
//! *Stage hunk*, and when `editor.git_gutter` is turned on.
//!
//! A file git does not know (outside a checkout, untracked, not in `HEAD`, binary, over the
//! `git.file` limit) has no base and no marks.
//!
//! *Stage hunk* of the peek: the file is saved first when the buffer has unsaved changes
//! (`worktree.stage` works on what the disk holds), then the unstaged hunks are read again and
//! the change blocks that overlap the clicked lines are staged by their lines
//! (`HunkRef::lines`), so a neighbouring change in the same git hunk stays unstaged.

use std::path::{Path, PathBuf};

use gpui::Context;
use hutan_code_view::git_gutter::{GitBase, GitBaseKind, PatchHunk, apply_hunks, match_change};
use hutan_protocol::git::GitFile;
use hutan_protocol::worktree::{HunkRef, WorktreeHunksRequest, WorktreeIndexChange};

use super::view::Workbench;

/// A *Stage hunk* waiting for the file to be saved.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingStage {
    pub first_line: usize,
    /// Equal to `first_line` for removed lines.
    pub end_line: usize,
}

/// What the daemon knows of the file: its checkout and its name there.
struct Fetched {
    root: PathBuf,
    rel: String,
    base: GitBase,
}

impl Workbench {
    /// Brings the git base of `path`'s buffer up to date: asks the daemon, or drops what the
    /// buffer holds when the marks are off or the daemon cannot say.
    pub(crate) fn refresh_git_base(&mut self, path: &str, cx: &mut Context<Self>) {
        let on = self
            .settings
            .get::<crate::settings::EditorSettings>()
            .git_gutter;
        let Some(stored) = self.buffers.buffers.get_mut(path) else {
            return;
        };
        let buffer = stored.buffer.clone();
        let store = self.sessions.read(cx);
        let supported = store.connection().supports_git_history();
        if !on || !supported {
            stored.git_task = None;
            if buffer.read(cx).git_base().is_some() {
                buffer.update(cx, |b, cx| b.set_git_base(None, cx));
            }
            return;
        }
        if !buffer.read(cx).is_ready() {
            // Asked again when the text arrives (`loaded`).
            return;
        }
        let stage_hunks = store.connection().supports_worktree_stage();
        let file = Path::new(path).to_owned();
        let head = self.sessions.update(cx, |store, cx| {
            store.git_file(
                GitFile {
                    path: file.clone(),
                    rev: "HEAD".to_owned(),
                },
                cx,
            )
        });
        let key = path.to_owned();
        let task = cx.spawn(async move |this, cx| {
            let fetched = match head.await {
                Ok(head) if !head.binary && !head.too_large => {
                    let staged = if stage_hunks {
                        let request = WorktreeHunksRequest {
                            path: head.root.clone(),
                            files: vec![head.path.clone()],
                        };
                        let asking = this
                            .update(cx, |this, cx| {
                                this.sessions
                                    .update(cx, |store, cx| store.worktree_hunks(request, cx))
                            })
                            .ok();
                        match asking {
                            Some(asking) => asking.await.ok(),
                            None => return,
                        }
                    } else {
                        None
                    };
                    let staged = staged
                        .and_then(|listed| listed.files.into_iter().find(|f| f.path == head.path))
                        .map(|f| f.staged)
                        .unwrap_or_default();
                    let base = if staged.is_empty() {
                        GitBase::new(&head.content, GitBaseKind::Head)
                    } else {
                        let patch: Vec<PatchHunk<'_>> = staged
                            .iter()
                            .map(|h| PatchHunk {
                                old_start: h.old_start,
                                old_lines: h.old_lines,
                                body: &h.body,
                            })
                            .collect();
                        match apply_hunks(&hutan_code_view::agent::normalize(&head.content), &patch)
                        {
                            Some(index) => GitBase::new(&index, GitBaseKind::Index),
                            // The staged hunks are of another text (the index changed under
                            // us): the next refresh gets it right; HEAD is the honest base now.
                            None => GitBase::new(&head.content, GitBaseKind::Head),
                        }
                    };
                    Some(Fetched {
                        root: head.root,
                        rel: head.path,
                        base,
                    })
                }
                // Not in a checkout, not in HEAD, binary, too large: no marks.
                _ => None,
            };
            this.update(cx, |this, cx| {
                let Some(stored) = this.buffers.buffers.get_mut(&key) else {
                    return;
                };
                stored.git_task = None;
                stored.git_file = fetched.as_ref().map(|f| (f.root.clone(), f.rel.clone()));
                let buffer = stored.buffer.clone();
                buffer.update(cx, |b, cx| b.set_git_base(fetched.map(|f| f.base), cx));
                this.sync_git_stage_availability(&key, cx);
            })
            .ok();
        });
        if let Some(stored) = self.buffers.buffers.get_mut(path) {
            stored.git_task = Some(task);
        }
    }

    /// Every open file's base again (the setting changed, the connection came back).
    pub(crate) fn refresh_git_bases(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<String> = self.buffers.buffers.keys().cloned().collect();
        for path in paths {
            self.refresh_git_base(&path, cx);
        }
    }

    /// The views of `path` offer *Stage hunk* while the daemon can stage and knows the file's
    /// checkout.
    pub(crate) fn sync_git_stage_availability(&mut self, path: &str, cx: &mut Context<Self>) {
        let available = self
            .sessions
            .read(cx)
            .connection()
            .supports_worktree_stage()
            && self
                .buffers
                .buffers
                .get(path)
                .is_some_and(|s| s.git_file.is_some());
        let views: Vec<_> = self
            .file_views
            .iter()
            .filter(|((_, p), _)| p == path)
            .map(|(_, view)| view.clone())
            .collect();
        for view in views {
            view.update(cx, |view, cx| view.set_git_stage_available(available, cx));
        }
    }

    /// *Stage hunk* in the peek of `path`'s tab: the change at lines `first_line..end_line`.
    pub(crate) fn stage_git_hunk(
        &mut self,
        path: &str,
        change: PendingStage,
        cx: &mut Context<Self>,
    ) {
        let Some(stored) = self.buffers.buffers.get_mut(path) else {
            return;
        };
        if stored.git_file.is_none()
            || !self
                .sessions
                .read(cx)
                .connection()
                .supports_worktree_stage()
        {
            self.set_notice("This daemon cannot stage hunks. Restart it to update.", cx);
            return;
        }
        let (dirty, editable) = {
            let b = stored.buffer.read(cx);
            (b.is_dirty(), b.is_editable())
        };
        if dirty && editable {
            // `worktree.stage` works on the file as the disk holds it: save, then stage.
            stored.stage_after_save = Some(change);
            self.save_file(path, None, cx);
            return;
        }
        self.stage_saved_hunk(path, change, cx);
    }

    /// Stages the change of the saved file: reads the unstaged hunks, finds the change blocks
    /// that overlap `change`, and stages exactly their lines.
    pub(crate) fn stage_saved_hunk(
        &mut self,
        path: &str,
        change: PendingStage,
        cx: &mut Context<Self>,
    ) {
        let Some((root, rel)) = self
            .buffers
            .buffers
            .get(path)
            .and_then(|s| s.git_file.clone())
        else {
            return;
        };
        let request = WorktreeHunksRequest {
            path: root.clone(),
            files: vec![rel.clone()],
        };
        let listing = self
            .sessions
            .update(cx, |store, cx| store.worktree_hunks(request, cx));
        let key = path.to_owned();
        cx.spawn(async move |this, cx| {
            let outcome: Result<WorktreeIndexChange, String> = async {
                let listed = listing.await?;
                let file = listed
                    .files
                    .into_iter()
                    .find(|f| f.path == rel)
                    .ok_or_else(|| "Nothing of this file is unstaged.".to_owned())?;
                let hunks: Vec<(u32, u32, &str)> = file
                    .unstaged
                    .iter()
                    .map(|h| (h.new_start, h.new_lines, h.body.as_str()))
                    .collect();
                let (index, lines) = match_change(&hunks, change.first_line, change.end_line)
                    .ok_or_else(|| {
                        "That change is no longer in the saved file; nothing was staged.".to_owned()
                    })?;
                Ok(WorktreeIndexChange {
                    checkout: root,
                    hunks: vec![HunkRef {
                        path: rel,
                        hunk_id: file.unstaged[index].id.clone(),
                        lines,
                    }],
                    ..WorktreeIndexChange::default()
                })
            }
            .await;
            let staging = match outcome {
                Ok(request) => this
                    .update(cx, |this, cx| {
                        this.sessions
                            .update(cx, |store, cx| store.change_index(request, true, cx))
                    })
                    .ok(),
                Err(error) => {
                    this.update(cx, |this, cx| this.set_notice(error, cx)).ok();
                    return;
                }
            };
            let Some(staging) = staging else {
                return;
            };
            let answer = staging.await;
            this.update(cx, |this, cx| {
                match answer {
                    Ok(_) => this.set_notice("Staged the change.", cx),
                    Err(error) => {
                        this.set_notice(format!("Could not stage the change: {error}"), cx)
                    }
                }
                // The index moved: the marks are against the new one.
                this.refresh_git_base(&key, cx);
            })
            .ok();
        })
        .detach();
    }
}
