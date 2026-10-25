//! Retention of worktrees (protocol 0.48, capability `worktree.setup`): `worktree.cleanup` lists
//! the worktrees `[worktrees] cleanup` would remove, `worktree.prune` removes them.
//!
//! Nothing is removed by itself: the daemon never prunes in the background. A worktree is a
//! candidate only when Hutan created it, nobody works in it (no live session, no setup running),
//! it is not locked, and nothing in it is uncommitted (untracked files count; when git cannot say,
//! the worktree is left alone), and then, by policy:
//!
//! - `merged`: its branch got commits of its own (its reflog has more than the creation entry,
//!   so a branch that was only created and never used is not "merged" just because the default
//!   branch moved on) and every commit is reachable from the repository's default branch;
//! - `idle`: nothing touched it for `cleanup_idle_days` — the later of its newest commit, the
//!   last change of its git state (`HEAD`, `index`, the reflog) and its creation.
//!
//! Removal is `worktree.remove` without `force`: teardown runs first and a failing teardown keeps
//! the worktree; a merged branch is deleted with `git branch -d`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use hutan_protocol::control::ErrorBody;
use hutan_protocol::worktree::{
    CleanupCandidate, CleanupReason, PruneFailure, WorktreeCleanup, WorktreeCleanupList,
    WorktreeCleanupRequest, WorktreeInfo, WorktreeListRequest, WorktreePrune, WorktreePruned,
    WorktreeRemove, WorktreeSetupState,
};

use super::review::default_branch;
use super::{
    READ_TIMEOUT, RemovedWorktree, WorktreeService, bad_request, dirty_strict, git_ok, run_git,
};
use crate::registry::now_ms;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

impl WorktreeService {
    /// `worktree.cleanup`: the worktrees the policy would remove now.
    pub async fn cleanup(
        &self,
        req: WorktreeCleanupRequest,
    ) -> Result<WorktreeCleanupList, ErrorBody> {
        let settings = self.settings();
        let mode = req.mode.unwrap_or(settings.cleanup);
        let idle_days = req.idle_days.unwrap_or(settings.cleanup_idle_days);
        if idle_days == 0 {
            return Err(bad_request("idle_days must be at least 1"));
        }
        let mut list = WorktreeCleanupList {
            mode,
            idle_days,
            candidates: Vec::new(),
        };
        if mode == WorktreeCleanup::Never {
            return Ok(list);
        }
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let worktrees = self
            .list(WorktreeListRequest {
                workspace_id: req.workspace_id,
            })
            .await?
            .worktrees;
        let created: std::collections::HashMap<PathBuf, u64> = {
            let storage = self.storage.clone();
            tokio::task::spawn_blocking(move || storage.worktrees().list())
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default()
                .into_iter()
                .map(|row| (row.path, row.created_at_ms))
                .collect()
        };
        for worktree in worktrees {
            if !eligible(&worktree) {
                continue;
            }
            // Deleting data: when git cannot tell, leave it be.
            if !matches!(dirty_strict(&worktree.path).await, Ok(false)) {
                continue;
            }
            let idle = idle_days_of(&worktree, created.get(&worktree.path).copied()).await;
            let reason = match mode {
                WorktreeCleanup::Merged if is_merged(&worktree).await => CleanupReason::Merged,
                WorktreeCleanup::Idle if idle >= idle_days => CleanupReason::Idle,
                _ => continue,
            };
            let mut teardown = Vec::new();
            if let Some(workspace) = self
                .setup_workspace(&workspaces, &worktree.repo_root, &worktree.path)
                .await
                && let Ok(resolved) = self.resolve_setup(&workspace, &worktree.repo_root).await
            {
                let confirmed = match resolved.trust_hash() {
                    Some(hash) if resolved.teardown_from_file => {
                        self.setup_trusted(&worktree.repo_root, &hash).await
                    }
                    _ => true,
                };
                if confirmed {
                    teardown = resolved.teardown;
                }
            }
            list.candidates.push(CleanupCandidate {
                path: worktree.path,
                repo_root: worktree.repo_root,
                branch: worktree.branch,
                reason,
                idle_days: idle,
                teardown,
            });
        }
        Ok(list)
    }

    /// `worktree.prune`: removes the candidates (or those of them in `req.paths`), one at a time.
    /// Besides the answer, the worktrees that went (the task service marks their open work).
    pub async fn prune(
        &self,
        req: WorktreePrune,
    ) -> Result<(WorktreePruned, Vec<RemovedWorktree>), ErrorBody> {
        let listed = self
            .cleanup(WorktreeCleanupRequest {
                workspace_id: req.workspace_id,
                mode: req.mode,
                idle_days: req.idle_days,
            })
            .await?;
        let mut pruned = WorktreePruned {
            removed: Vec::new(),
            failed: Vec::new(),
        };
        let mut gone = Vec::new();
        let wanted: Vec<PathBuf> = req
            .paths
            .iter()
            .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()))
            .collect();
        for path in &wanted {
            if !listed.candidates.iter().any(|c| &c.path == path) {
                pruned.failed.push(PruneFailure {
                    path: path.clone(),
                    reason: "not a worktree the cleanup policy lists now".to_owned(),
                });
            }
        }
        for candidate in listed.candidates {
            if !wanted.is_empty() && !wanted.contains(&candidate.path) {
                continue;
            }
            let removal = self
                .remove(WorktreeRemove {
                    path: candidate.path.clone(),
                    force: false,
                    delete_branch: candidate.reason == CleanupReason::Merged,
                })
                .await;
            match removal {
                Ok((_, removed)) => {
                    pruned.removed.push(candidate.path);
                    gone.push(removed);
                }
                Err(error) => pruned.failed.push(PruneFailure {
                    path: candidate.path,
                    reason: error.message,
                }),
            }
        }
        Ok((pruned, gone))
    }
}

/// Created by Hutan, not the main worktree, not locked or prunable, nobody in it, no setup running.
fn eligible(worktree: &WorktreeInfo) -> bool {
    worktree.created_by_hutan
        && !worktree.main
        && !worktree.locked
        && !worktree.prunable
        && worktree.live_sessions.is_empty()
        && worktree
            .setup
            .as_ref()
            .is_none_or(|s| s.state != WorktreeSetupState::Running)
}

/// The branch has commits of its own and the default branch contains them all.
async fn is_merged(worktree: &WorktreeInfo) -> bool {
    let Some(branch) = worktree.branch.as_deref() else {
        return false;
    };
    let main = &worktree.repo_root;
    let Some(target) = default_branch(main).await else {
        return false;
    };
    if target == branch {
        return false;
    }
    let reflog = run_git(
        main,
        &[
            OsStr::new("reflog"),
            OsStr::new("show"),
            OsStr::new("--format=%H"),
            OsStr::new(&format!("refs/heads/{branch}")),
            OsStr::new("--"),
        ],
        READ_TIMEOUT,
    )
    .await;
    let own_commits = reflog.ok().filter(|out| out.success).is_some_and(|out| {
        out.stdout
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .count()
            > 1
    });
    own_commits
        && git_ok(
            main,
            &[
                "merge-base",
                "--is-ancestor",
                &format!("refs/heads/{branch}"),
                &target,
            ],
        )
        .await
        .unwrap_or(false)
}

/// Whole days since anything touched the worktree: the later of its newest commit, the last
/// change of its git state and its creation.
async fn idle_days_of(worktree: &WorktreeInfo, created_at_ms: Option<u64>) -> u64 {
    let mut latest = created_at_ms.unwrap_or(0);
    if let Ok(out) = run_git(
        &worktree.path,
        &[
            OsStr::new("log"),
            OsStr::new("-1"),
            OsStr::new("--format=%ct"),
            OsStr::new("HEAD"),
        ],
        READ_TIMEOUT,
    )
    .await
        && out.success
        && let Ok(seconds) = String::from_utf8_lossy(&out.stdout).trim().parse::<u64>()
    {
        latest = latest.max(seconds.saturating_mul(1000));
    }
    if let Ok(out) = run_git(
        &worktree.path,
        &[OsStr::new("rev-parse"), OsStr::new("--absolute-git-dir")],
        READ_TIMEOUT,
    )
    .await
        && out.success
    {
        let git_dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        latest = latest.max(state_changed_ms(&git_dir).await);
    }
    now_ms().saturating_sub(latest) / DAY_MS
}

/// The newest modification time of the files that change when someone works in the worktree.
async fn state_changed_ms(git_dir: &Path) -> u64 {
    let git_dir = git_dir.to_owned();
    tokio::task::spawn_blocking(move || {
        ["HEAD", "index", "logs/HEAD"]
            .iter()
            .filter_map(|name| std::fs::metadata(git_dir.join(name)).ok())
            .filter_map(|meta| meta.modified().ok())
            .filter_map(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .max()
            .unwrap_or(0)
    })
    .await
    .unwrap_or(0)
}
