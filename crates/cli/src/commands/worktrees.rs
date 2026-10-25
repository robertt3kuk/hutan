//! `hutan wt new|ls|rm|diff|merge|commit`, `hutan finish` and the worktree step of `hutan new
//! --worktree`.

use std::path::{Path, PathBuf};

use hutan_client::Client;
use hutan_core::WorkspaceId;
use hutan_protocol::messages::{CreatedBy, Request, WorkspaceKind, WorkspaceOpen, WorkspaceOpened};
use hutan_protocol::worktree::{
    CleanupReason, DiffFile, DiffStatus, FinishCommit, FinishMerge, FinishPush, MergeStrategy,
    StepReason, StepStatus, WorktreeCleanup, WorktreeCleanupRequest, WorktreeCommit,
    WorktreeCreate, WorktreeCreated, WorktreeDiff, WorktreeDiffRequest, WorktreeFileDiffRequest,
    WorktreeFinish, WorktreeFinished, WorktreeInfo, WorktreeList, WorktreeMerge, WorktreeMerged,
    WorktreePrune, WorktreeRemove, WorktreeSetup, WorktreeSetupRequest, WorktreeSetupState,
    WorktreeSetupTrust, conflict_files,
};
use serde_json::json;

use super::Ctx;
use super::sessions::with_source;
use super::workspaces::workspace_target;
use crate::args::{
    FinishArgs, WtCommand, WtCommitArgs, WtDiffArgs, WtLsArgs, WtMergeArgs, WtNewArgs, WtPruneArgs,
    WtRmArgs, WtSetupArgs,
};
use crate::error::CliError;

/// Where `hutan new --worktree` starts its session.
pub(super) struct Placed {
    pub workspace: WorkspaceId,
    pub path: PathBuf,
}

impl Ctx {
    pub(super) async fn wt(&self, command: WtCommand) -> Result<(), CliError> {
        match command {
            WtCommand::New(args) => self.wt_new(args).await,
            WtCommand::Ls(args) => self.wt_ls(args).await,
            WtCommand::Rm(args) => self.wt_rm(args).await,
            WtCommand::Diff(args) => self.wt_diff(args).await,
            WtCommand::Merge(args) => self.wt_merge(args).await,
            WtCommand::Commit(args) => self.wt_commit(args).await,
            WtCommand::Setup(args) => self.wt_setup(args).await,
            WtCommand::Prune(args) => self.wt_prune(args).await,
        }
    }

    /// The worktree `target` names among those of the workspace the command is scoped to.
    async fn wt_target(
        &self,
        client: &Client,
        target: &str,
        workspace: Option<String>,
    ) -> Result<WorktreeInfo, CliError> {
        let scope = self.wt_scope(client, workspace).await?.map(|s| s.0);
        let list = client
            .worktree_list(scope)
            .await
            .map_err(|e| self.fail(e))?;
        resolve_worktree(&list, target, &super::paths::here()?).cloned()
    }

    async fn wt_diff(&self, args: WtDiffArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let target = self
            .wt_target(&client, &args.target, args.workspace)
            .await?;
        let include_uncommitted = !args.committed;
        let diff = client
            .worktree_diff(WorktreeDiffRequest {
                path: target.path.clone(),
                base: args.base.clone(),
                include_uncommitted,
            })
            .await
            .map_err(|e| self.fail(e))?;
        let mut patch = String::new();
        let mut cut: Vec<&str> = Vec::new();
        if !args.stat {
            for file in &diff.files {
                let one = client
                    .worktree_file_diff(WorktreeFileDiffRequest {
                        path: target.path.clone(),
                        file: file.path.clone(),
                        old_path: file.old_path.clone(),
                        base: args.base.clone(),
                        include_uncommitted,
                    })
                    .await
                    .map_err(|e| self.fail(e))?;
                if one.truncated {
                    cut.push(&file.path);
                }
                patch.push_str(&one.patch);
                if one.truncated && !patch.ends_with('\n') {
                    patch.push('\n');
                }
            }
        }
        if !self.json && !cut.is_empty() {
            eprintln!("note: the patch of {} is cut at 2 MiB", cut.join(", "));
        }
        if !self.json && diff.patch_truncated && cut.is_empty() {
            eprintln!("note: the diff is larger than the daemon shows (20 MiB or 5000 files)");
        }
        let json = json!({
            "path": target.path,
            "base": diff.base,
            "merge_base": diff.merge_base,
            "files": diff.files,
            "patch_truncated": diff.patch_truncated,
            "commits": diff.commits,
            "commits_truncated": diff.commits_truncated,
            "patch": (!args.stat).then_some(&patch),
        });
        self.print(&json, || {
            if args.stat {
                stat_text(&diff, target.branch.as_deref().unwrap_or("<branch>"))
            } else {
                patch.trim_end_matches('\n').to_owned()
            }
        })
    }

    async fn wt_commit(&self, args: WtCommitArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let target = self
            .wt_target(&client, &args.target, args.workspace)
            .await?;
        let committed = client
            .worktree_commit(WorktreeCommit {
                path: target.path.clone(),
                message: args.message,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&committed, || {
            format!(
                "committed {} file(s) on {} ({})",
                committed.files,
                committed.branch,
                committed.commit.chars().take(7).collect::<String>()
            )
        })
    }

    async fn wt_merge(&self, args: WtMergeArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let target = self
            .wt_target(&client, &args.target, args.workspace)
            .await?;
        let strategy = match (args.squash, args.rebase) {
            (true, _) => MergeStrategy::Squash,
            (_, true) => MergeStrategy::Rebase,
            _ => MergeStrategy::Merge,
        };
        let merged = client
            .worktree_merge(WorktreeMerge {
                path: target.path.clone(),
                into: args.into,
                strategy,
                message: args.message,
            })
            .await
            .map_err(|e| self.fail_merge(e))?;
        self.print(&merged, || merged_text(&target, &merged))
    }

    /// A refused merge: for a conflict the files are listed one per line after the reason.
    fn fail_merge(&self, error: hutan_client::ClientError) -> CliError {
        if let hutan_client::ClientError::Remote(body) = &error {
            let files = conflict_files(body);
            if !files.is_empty() {
                let mut message = body.message.clone();
                for file in files {
                    message.push_str(&format!("\n  conflict: {file}"));
                }
                return CliError::Failed(message);
            }
        }
        self.fail(error)
    }

    /// `hutan finish`: one line per step; exit 1 when a step failed (what the steps before it
    /// did stays).
    pub(super) async fn finish(&self, args: FinishArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let scope = self.wt_scope(&client, args.workspace).await?.map(|s| s.0);
        let list = client
            .worktree_list(scope)
            .await
            .map_err(|e| self.fail(e))?;
        let cwd = super::paths::here()?;
        let checkout = match &args.target {
            Some(target) => resolve_worktree(&list, target, &cwd)?.clone(),
            None => checkout_of(&list, &cwd)?.clone(),
        };
        let strategy = match (args.squash, args.rebase) {
            (true, _) => MergeStrategy::Squash,
            (_, true) => MergeStrategy::Rebase,
            _ => MergeStrategy::Merge,
        };
        let finished = client
            .worktree_finish(WorktreeFinish {
                checkout: checkout.path.clone(),
                message: args.message.unwrap_or_default(),
                push: args.no_push.then_some(false),
                into: args.into,
                strategy,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&finished, || finished_text(&checkout, &finished))?;
        if finished.failed() {
            return Err(CliError::Reported);
        }
        Ok(())
    }

    async fn wt_new(&self, args: WtNewArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let (workspace, from_cwd) = self
            .wt_scope(&client, args.workspace)
            .await?
            .ok_or_else(no_workspace)?;
        // The daemon resolves paths from its own working directory, not ours.
        let repo = match args.repo {
            Some(path) => Some(super::paths::absolute(path)?),
            None if from_cwd => Some(super::paths::here()?),
            None => None,
        };
        let path = args.path.map(super::paths::absolute).transpose()?;
        let created = client
            .worktree_create(WorktreeCreate {
                workspace_id: workspace,
                repo,
                branch: args.branch,
                base: args.base,
                path,
                no_setup: args.no_setup,
            })
            .await
            .map_err(|e| self.fail(e))?;
        if let Some(setup) = &created.setup {
            self.note(setup_note(setup, created.branch.as_deref()));
        }
        let path = created.path.display().to_string();
        self.print(&WorktreeCreated { worktree: created }, || path)
    }

    /// `hutan wt setup`: runs the setup again; the repository's commands are shown and asked
    /// about when they are new.
    async fn wt_setup(&self, args: WtSetupArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_worktree_setup() {
            return Err(CliError::Failed(
                "this hutand cannot run worktree setup (it is older than protocol 0.49); restart \
                 it to update"
                    .to_owned(),
            ));
        }
        let target = self
            .wt_target(&client, &args.target, args.workspace)
            .await?;
        let mut result = client
            .worktree_setup(WorktreeSetupRequest {
                path: target.path.clone(),
            })
            .await
            .map_err(|e| self.fail(e))?;
        if let Some(setup) = result.worktree.setup.clone()
            && setup.state == WorktreeSetupState::NeedsTrust
        {
            let hash = setup.hash.clone().unwrap_or_default();
            confirm_setup(&setup, &target.path, args.yes)?;
            result = client
                .worktree_setup_trust(WorktreeSetupTrust {
                    path: target.path.clone(),
                    hash,
                })
                .await
                .map_err(|e| self.fail(e))?;
        }
        self.print(&result, || {
            result.worktree.setup.as_ref().map_or_else(
                || "no setup".to_owned(),
                |setup| setup_note(setup, target.branch.as_deref()),
            )
        })
    }

    /// `hutan wt prune`: lists what the cleanup policy removes, asks, and removes it.
    async fn wt_prune(&self, args: WtPruneArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_worktree_setup() {
            return Err(CliError::Failed(
                "this hutand cannot clean up worktrees (it is older than protocol 0.49); restart \
                 it to update"
                    .to_owned(),
            ));
        }
        let workspace = self.wt_scope(&client, args.workspace).await?.map(|s| s.0);
        let (mode, idle_days) = match (args.merged, args.idle_days) {
            (true, _) => (Some(WorktreeCleanup::Merged), None),
            (_, Some(days)) => (Some(WorktreeCleanup::Idle), Some(days)),
            _ => (None, None),
        };
        let listed = client
            .worktree_cleanup(WorktreeCleanupRequest {
                workspace_id: workspace,
                mode,
                idle_days,
            })
            .await
            .map_err(|e| self.fail(e))?;
        if args.dry_run {
            return self.print(&listed, || cleanup_text(&listed));
        }
        if listed.candidates.is_empty() {
            return self.print(
                &hutan_protocol::worktree::WorktreePruned {
                    removed: Vec::new(),
                    failed: Vec::new(),
                },
                || cleanup_text(&listed),
            );
        }
        if !self.json {
            println!("{}", cleanup_text(&listed));
        }
        confirm_prune(listed.candidates.len(), args.yes)?;
        let pruned = client
            .worktree_prune(WorktreePrune {
                workspace_id: workspace,
                mode,
                idle_days,
                paths: listed.candidates.iter().map(|c| c.path.clone()).collect(),
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&pruned, || {
            let mut lines: Vec<String> = pruned
                .removed
                .iter()
                .map(|path| format!("removed {}", path.display()))
                .collect();
            lines.extend(
                pruned
                    .failed
                    .iter()
                    .map(|f| format!("kept {}: {}", f.path.display(), f.reason)),
            );
            lines.join("\n")
        })?;
        if pruned.failed.is_empty() {
            Ok(())
        } else {
            Err(CliError::Reported)
        }
    }

    async fn wt_ls(&self, args: WtLsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let workspace = self.wt_scope(&client, args.workspace).await?.map(|s| s.0);
        let worktrees = client
            .worktree_list(workspace)
            .await
            .map_err(|e| self.fail(e))?;
        let list = WorktreeList { worktrees };
        self.print(&list, || worktree_table(&list.worktrees))
    }

    async fn wt_rm(&self, args: WtRmArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let workspace = self.wt_scope(&client, args.workspace).await?.map(|s| s.0);
        let list = client
            .worktree_list(workspace)
            .await
            .map_err(|e| self.fail(e))?;
        let target = resolve_worktree(&list, &args.target, &super::paths::here()?)?.clone();
        let removed = client
            .worktree_remove(WorktreeRemove {
                path: target.path.clone(),
                force: args.force,
                delete_branch: args.delete_branch,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(
            &json!({
                "path": target.path,
                "removed": true,
                "branch_kept": removed.branch_kept,
            }),
            || {
                let mut text = format!("removed {}", target.path.display());
                if let Some(reason) = &removed.branch_kept {
                    text.push_str(&format!(
                        "\nbranch {} kept: {reason}",
                        target.branch.as_deref().unwrap_or("-")
                    ));
                }
                text
            },
        )
    }

    /// The directory and workspace of `hutan new --worktree <branch>`: the branch's existing
    /// worktree, or a new one at the default path.
    pub(super) async fn worktree_for_session(
        &self,
        client: &Client,
        workspace_flag: Option<String>,
        branch: &str,
        cwd: &Path,
    ) -> Result<Placed, CliError> {
        let (workspace, from_cwd) = self
            .wt_scope(client, workspace_flag)
            .await?
            .ok_or_else(no_workspace)?;
        let hint = from_cwd.then_some(cwd);
        let list = client
            .worktree_list(Some(workspace))
            .await
            .map_err(|e| self.fail(e))?;
        if let Some(existing) = reusable(&list, branch, hint)? {
            self.note(format!("using worktree {}", existing.path.display()));
            return Ok(Placed {
                workspace,
                path: existing.path.clone(),
            });
        }
        let created = client
            .worktree_create(WorktreeCreate {
                workspace_id: workspace,
                repo: hint.map(Path::to_path_buf),
                branch: branch.to_owned(),
                base: None,
                path: None,
                no_setup: false,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.note(format!("created worktree {}", created.path.display()));
        if let Some(setup) = &created.setup {
            self.note(setup_note(setup, created.branch.as_deref()));
        }
        Ok(Placed {
            workspace,
            path: created.path,
        })
    }

    /// A line for people on stderr; nothing in `--json` mode.
    fn note(&self, text: String) {
        if !self.json {
            eprintln!("{text}");
        }
    }

    /// The workspace the worktree and task commands act on and whether the current directory
    /// chose it: `--ws`, else `$HUTAN_WORKSPACE`, else the project of the current directory (the
    /// daemon finds it through the repository, so a linked worktree belongs to the project of
    /// its main checkout, and makes it when the directory has none yet). Home is no project.
    pub(super) async fn wt_scope(
        &self,
        client: &Client,
        flag: Option<String>,
    ) -> Result<Option<(WorkspaceId, bool)>, CliError> {
        if let Some((target, source)) = workspace_target(flag) {
            let id = self
                .resolve_workspace_id(client, &target)
                .await
                .map_err(|error| with_source(error, &target, source))?;
            return Ok(Some((id, false)));
        }
        let opened: WorkspaceOpened = self
            .call(
                client,
                Request::WorkspaceOpen(WorkspaceOpen {
                    path: super::paths::here()?,
                    created_by: Some(CreatedBy::Cli),
                }),
            )
            .await?;
        Ok(
            (opened.workspace.kind == WorkspaceKind::Project)
                .then_some((opened.workspace.id, true)),
        )
    }
}

pub(super) fn no_workspace() -> CliError {
    CliError::Usage(
        "the current directory belongs to no project (it is Home or the home directory); pass \
         --ws, set $HUTAN_WORKSPACE, or run the command in a repository or folder"
            .to_owned(),
    )
}

/// The checkout `cwd` is in: the deepest listed checkout containing it.
fn checkout_of<'a>(list: &'a [WorktreeInfo], cwd: &Path) -> Result<&'a WorktreeInfo, CliError> {
    let dir = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    list.iter()
        .filter(|w| dir.starts_with(&w.path))
        .max_by_key(|w| w.path.components().count())
        .ok_or_else(|| {
            CliError::Failed(format!(
                "{} is in no git checkout of this project; run `hutan finish` inside one or \
                 name it (`hutan wt ls`)",
                cwd.display()
            ))
        })
}

fn short(hash: &str) -> String {
    hash.chars().take(7).collect()
}

fn files_text(n: u32) -> String {
    if n == 1 {
        "1 file".to_owned()
    } else {
        format!("{n} files")
    }
}

/// Why a step did not run or failed, in a few words.
fn reason_text(reason: Option<StepReason>) -> &'static str {
    match reason {
        Some(StepReason::NothingToCommit) => "nothing to commit",
        Some(StepReason::NothingToMerge) => "nothing to merge",
        Some(StepReason::UpToDate) => "up to date",
        Some(StepReason::EarlierStepFailed) => "skipped: an earlier step failed",
        Some(StepReason::Conflict) => "conflict",
        Some(StepReason::NoUpstream) => "no upstream",
        Some(StepReason::Rejected) => "rejected",
        Some(StepReason::Auth) => "authentication",
        Some(StepReason::Error) | None => "error",
    }
}

fn commit_line(step: &FinishCommit) -> String {
    match step.status {
        StepStatus::Ok => format!(
            "commit: committed {} on {} ({})",
            files_text(step.files),
            step.branch,
            short(step.commit.as_deref().unwrap_or_default())
        ),
        StepStatus::Skipped => format!("commit: {}", reason_text(step.reason)),
        StepStatus::Failed => format!(
            "commit failed: {}",
            step.message.as_deref().unwrap_or("git refused")
        ),
    }
}

fn merge_line(step: &FinishMerge) -> String {
    match step.status {
        StepStatus::Ok => format!(
            "merge:  {} {} into {} ({})",
            match step.strategy {
                MergeStrategy::Merge => "merged",
                MergeStrategy::Squash => "squash-merged",
                MergeStrategy::Rebase => "rebased and merged",
            },
            step.branch,
            step.target,
            short(step.commit.as_deref().unwrap_or_default())
        ),
        StepStatus::Skipped => match &step.message {
            Some(message) => format!("merge:  {message}"),
            None => format!("merge:  {}", reason_text(step.reason)),
        },
        StepStatus::Failed => {
            let mut text = format!(
                "merge failed ({}): {}",
                reason_text(step.reason),
                step.message.as_deref().unwrap_or_default()
            );
            for file in &step.conflicts {
                text.push_str(&format!("\n  conflict: {file}"));
            }
            text
        }
    }
}

fn push_line(step: Option<&FinishPush>) -> String {
    let Some(step) = step else {
        return "push:   not pushed (--no-push, or `[finish] push = false` in daemon.toml)"
            .to_owned();
    };
    let upstream = step.upstream.as_deref().unwrap_or("its upstream");
    match step.status {
        StepStatus::Ok => format!(
            "push:   pushed {} to {upstream}{} ({})",
            step.branch,
            match step.ahead {
                Some(1) => " (1 commit)".to_owned(),
                Some(n) => format!(" ({n} commits)"),
                None => String::new(),
            },
            short(step.commit.as_deref().unwrap_or_default())
        ),
        StepStatus::Skipped => match &step.message {
            Some(message) => format!("push:   {message}"),
            None => format!("push:   {}", reason_text(step.reason)),
        },
        StepStatus::Failed => format!(
            "push failed ({}): {}",
            reason_text(step.reason),
            step.message.as_deref().unwrap_or_default()
        ),
    }
}

/// What `hutan finish` did, one line per step, then the worktree cleanup a merge allows.
fn finished_text(checkout: &WorktreeInfo, finished: &WorktreeFinished) -> String {
    let mut lines = vec![commit_line(&finished.commit)];
    if let Some(merge) = &finished.merge {
        lines.push(merge_line(merge));
    }
    lines.push(push_line(finished.push.as_ref()));
    if finished
        .merge
        .as_ref()
        .is_some_and(|m| m.status == StepStatus::Ok)
    {
        lines.push(format!(
            "remove the worktree with `hutan wt rm {} --delete-branch`",
            checkout.path.display()
        ));
    }
    lines.join("\n")
}

/// The worktree `target` names: a path (as written, or relative to `cwd`), else a branch.
fn resolve_worktree<'a>(
    list: &'a [WorktreeInfo],
    target: &str,
    cwd: &Path,
) -> Result<&'a WorktreeInfo, CliError> {
    let written = cwd.join(target);
    let canonical = std::fs::canonicalize(&written).ok();
    if let Some(found) = list
        .iter()
        .find(|w| w.path == written || canonical.as_deref() == Some(w.path.as_path()))
    {
        return Ok(found);
    }
    let by_branch: Vec<&WorktreeInfo> = list
        .iter()
        .filter(|w| w.branch.as_deref() == Some(target))
        .collect();
    match by_branch.as_slice() {
        [one] => Ok(one),
        [] => Err(CliError::Failed(format!(
            "no worktree matches `{target}`; `hutan wt ls` lists them"
        ))),
        many => {
            let mut message = format!(
                "`{target}` is checked out in {} worktrees; use a path:",
                many.len()
            );
            for w in many {
                message.push_str(&format!("\n  {}", w.path.display()));
            }
            Err(CliError::Usage(message))
        }
    }
}

/// The existing worktree `hutan new --worktree <branch>` should use, if any. `hint` is a
/// directory of the repository the user is in; without it every repository of the workspace
/// counts.
fn reusable<'a>(
    list: &'a [WorktreeInfo],
    branch: &str,
    hint: Option<&Path>,
) -> Result<Option<&'a WorktreeInfo>, CliError> {
    let hint = hint.map(|dir| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()));
    // The repository the directory is in: the deepest checkout containing it.
    let repo = hint.as_deref().and_then(|dir| {
        list.iter()
            .filter(|w| dir.starts_with(&w.path))
            .max_by_key(|w| w.path.components().count())
            .map(|w| w.repo_root.as_path())
    });
    let matching: Vec<&WorktreeInfo> = list
        .iter()
        .filter(|w| w.branch.as_deref() == Some(branch))
        .filter(|w| repo.is_none_or(|repo| w.repo_root == repo))
        .collect();
    match matching.as_slice() {
        [] => Ok(None),
        [one] if one.main => Err(CliError::Failed(format!(
            "branch `{branch}` is checked out in the main checkout {}; a worktree needs a branch \
             that is not (start the session there with `hutan new --cwd`)",
            one.path.display()
        ))),
        [one] => Ok(Some(one)),
        many => {
            let mut message = format!(
                "branch `{branch}` has worktrees in {} repositories; run from inside one:",
                many.len()
            );
            for w in many {
                message.push_str(&format!("\n  {}", w.path.display()));
            }
            Err(CliError::Usage(message))
        }
    }
}

/// `git diff --stat`-like lines: path, `+adds -dels`, a summary, then what a merge takes (the
/// commits) and what it leaves behind (uncommitted files).
fn stat_text(diff: &WorktreeDiff, branch: &str) -> String {
    if diff.files.is_empty() && diff.commits.is_empty() {
        return format!("no changes against {}", diff.base);
    }
    let name = |file: &DiffFile| match (&file.old_path, file.status) {
        (Some(old), DiffStatus::Renamed) => format!("{old} => {}", file.path),
        _ => file.path.clone(),
    };
    let width = diff
        .files
        .iter()
        .map(|f| name(f).chars().count())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<String> = diff
        .files
        .iter()
        .map(|file| {
            let status = match file.status {
                DiffStatus::Added => 'A',
                DiffStatus::Modified => 'M',
                DiffStatus::Deleted => 'D',
                DiffStatus::Renamed => 'R',
            };
            let counts = if file.binary {
                "binary".to_owned()
            } else {
                format!("+{} -{}", file.additions, file.deletions)
            };
            let scope = match (file.committed, file.uncommitted) {
                (true, true) => "  (also uncommitted)",
                (false, true) => "  (uncommitted)",
                _ => "",
            };
            format!("{status} {:<width$}  {counts}{scope}", name(file))
        })
        .collect();
    let additions: u64 = diff.files.iter().map(|f| u64::from(f.additions)).sum();
    let deletions: u64 = diff.files.iter().map(|f| u64::from(f.deletions)).sum();
    lines.push(format!(
        "{} file(s) changed, {additions} insertion(s)(+), {deletions} deletion(s)(-) against {}",
        diff.files.len(),
        diff.base
    ));
    lines.push(format!(
        "a merge takes {} commit(s){}:",
        diff.commits.len(),
        if diff.commits_truncated {
            " (the newest are listed)"
        } else {
            ""
        }
    ));
    for commit in &diff.commits {
        lines.push(format!(
            "  {} {}",
            commit.id.chars().take(7).collect::<String>(),
            commit.subject
        ));
    }
    let uncommitted = diff.files.iter().filter(|f| f.uncommitted).count();
    if uncommitted > 0 {
        lines.push(format!(
            "{uncommitted} file(s) with uncommitted changes: a merge leaves them behind \
             (`hutan wt commit {branch} -m MSG` commits them)"
        ));
    }
    lines.join("\n")
}

fn merged_text(source: &WorktreeInfo, merged: &WorktreeMerged) -> String {
    let how = match merged.strategy {
        MergeStrategy::Merge => "merged",
        MergeStrategy::Squash => "squash-merged",
        MergeStrategy::Rebase => "rebased and merged",
    };
    let mut text = format!(
        "{how} {} into {} ({})",
        merged.branch,
        merged.target,
        merged.commit.chars().take(7).collect::<String>()
    );
    if merged.source_dirty {
        text.push_str(&format!(
            "\n{} still has uncommitted changes: they were not part of the merge and removing \
             the worktree would discard them (`hutan wt rm --force` does)",
            source.path.display()
        ));
    }
    text.push_str(&format!(
        "\nremove the worktree with `hutan wt rm {} --delete-branch`{}",
        source.path.display(),
        if merged.source_dirty {
            " (after committing or deliberately discarding them)"
        } else {
            ""
        }
    ));
    text
}

fn branch_text(worktree: &WorktreeInfo) -> String {
    match &worktree.branch {
        Some(branch) => branch.clone(),
        None => format!(
            "(detached {})",
            worktree.head.chars().take(7).collect::<String>()
        ),
    }
}

/// One line for what a setup is doing or did.
fn setup_note(setup: &WorktreeSetup, branch: Option<&str>) -> String {
    let copied = match setup.copied {
        0 => String::new(),
        1 => "copied 1 file; ".to_owned(),
        n => format!("copied {n} files; "),
    };
    let tail = setup
        .message
        .as_deref()
        .map(|m| format!(" ({m})"))
        .unwrap_or_default();
    match setup.state {
        WorktreeSetupState::Running => format!(
            "{copied}setup is running{}{tail}",
            setup
                .session_id
                .map(|id| format!(" in session {id}"))
                .unwrap_or_default()
        ),
        WorktreeSetupState::NeedsTrust => format!(
            "{copied}setup did not run: its commands come from .hutan/worktree.toml and need your \
             confirmation; run `hutan wt setup {}`{tail}",
            branch.unwrap_or("<worktree>")
        ),
        WorktreeSetupState::Failed => format!("{copied}setup failed{tail}"),
        WorktreeSetupState::Done => format!("{copied}setup done{tail}"),
    }
}

/// Shows the repository's commands and asks; anything but `y` or `yes` is No.
fn confirm_setup(setup: &WorktreeSetup, path: &Path, yes: bool) -> Result<(), CliError> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        ".hutan/worktree.toml asks to run these commands in the worktree {}:",
        path.display()
    )?;
    for command in &setup.commands {
        writeln!(stderr, "  setup     $ {command}")?;
    }
    for command in &setup.teardown {
        writeln!(
            stderr,
            "  teardown  $ {command}  (before the worktree is removed)"
        )?;
    }
    if yes {
        return Ok(());
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(CliError::Usage(
            "nothing ran: stdin is not a terminal; pass --yes to confirm these commands".to_owned(),
        ));
    }
    write!(
        stderr,
        "Run them, and again whenever they stay the same? [y/N] "
    )?;
    stderr.flush()?;
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliError::Failed(
            "nothing ran: the commands were not confirmed".to_owned(),
        ))
    }
}

fn confirm_prune(count: usize, yes: bool) -> Result<(), CliError> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};
    if yes {
        return Ok(());
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(CliError::Usage(
            "nothing was removed: stdin is not a terminal; pass --yes to remove them".to_owned(),
        ));
    }
    let mut stderr = std::io::stderr().lock();
    write!(
        stderr,
        "Remove {count} worktree(s), running their teardown first? [y/N] "
    )?;
    stderr.flush()?;
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliError::Failed("nothing was removed".to_owned()))
    }
}

fn cleanup_text(list: &hutan_protocol::worktree::WorktreeCleanupList) -> String {
    if list.candidates.is_empty() {
        return match list.mode {
            WorktreeCleanup::Never => {
                "cleanup is never: set `[worktrees] cleanup` in daemon.toml, \
                                      or pass --merged or --idle-days N"
                    .to_owned()
            }
            WorktreeCleanup::Merged => "no merged worktree to clean up".to_owned(),
            WorktreeCleanup::Idle => format!("no worktree untouched for {} day(s)", list.idle_days),
        };
    }
    let rows = list
        .candidates
        .iter()
        .map(|c| {
            vec![
                c.branch.clone().unwrap_or_else(|| "(detached)".to_owned()),
                c.path.display().to_string(),
                match c.reason {
                    CleanupReason::Merged => "merged".to_owned(),
                    CleanupReason::Idle => format!("idle {} d", c.idle_days),
                },
                if c.teardown.is_empty() {
                    String::new()
                } else {
                    format!("teardown: {}", c.teardown.join("; "))
                },
            ]
        })
        .collect();
    super::workspaces::table(&["BRANCH", "PATH", "WHY", "TEARDOWN"], rows)
}

fn state_text(worktree: &WorktreeInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if worktree.main {
        parts.push("main".to_owned());
    }
    if worktree.created_by_hutan {
        parts.push("hutan".to_owned());
    }
    if worktree.dirty {
        parts.push("dirty".to_owned());
    }
    if worktree.locked {
        parts.push("locked".to_owned());
    }
    if worktree.prunable {
        parts.push("prunable".to_owned());
    }
    match worktree.live_sessions.len() {
        0 => {}
        1 => parts.push("1 session".to_owned()),
        n => parts.push(format!("{n} sessions")),
    }
    match worktree.setup.as_ref().map(|s| s.state) {
        Some(WorktreeSetupState::Running) => parts.push("setting up".to_owned()),
        Some(WorktreeSetupState::NeedsTrust) => parts.push("setup needs confirmation".to_owned()),
        Some(WorktreeSetupState::Failed) => parts.push("setup failed".to_owned()),
        Some(WorktreeSetupState::Done) | None => {}
    }
    if parts.is_empty() {
        "-".to_owned()
    } else {
        parts.join(", ")
    }
}

fn worktree_table(worktrees: &[WorktreeInfo]) -> String {
    if worktrees.is_empty() {
        return "no worktrees".to_owned();
    }
    let rows: Vec<[String; 3]> = worktrees
        .iter()
        .map(|w| [branch_text(w), w.path.display().to_string(), state_text(w)])
        .collect();
    let header = ["BRANCH", "PATH", "STATE"].map(str::to_owned);
    let mut widths = [0; 3];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    std::iter::once(&header)
        .chain(&rows)
        .map(|row| {
            format!(
                "{:<w0$}  {:<w1$}  {}",
                row[0],
                row[1],
                row[2],
                w0 = widths[0],
                w1 = widths[1]
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wt(path: &str, repo: &str, branch: Option<&str>, main: bool) -> WorktreeInfo {
        WorktreeInfo {
            path: PathBuf::from(path),
            repo_root: PathBuf::from(repo),
            branch: branch.map(str::to_owned),
            head: "3f786850e387550fdab836ed7e6dc881de23001b".to_owned(),
            main,
            created_by_hutan: false,
            dirty: false,
            live_sessions: vec![],
            locked: false,
            prunable: false,
            setup: None,
        }
    }

    fn fixture() -> Vec<WorktreeInfo> {
        vec![
            wt("/w/a", "/w/a", Some("main"), true),
            wt("/w/a-wt/x", "/w/a", Some("feature/x"), false),
            wt("/w/b", "/w/b", Some("main"), true),
            wt("/w/b-wt/x", "/w/b", Some("feature/x"), false),
            wt("/w/b-wt/bisect", "/w/b", None, false),
        ]
    }

    #[test]
    fn a_target_is_a_path_first_then_a_branch() {
        let list = fixture();
        let cwd = Path::new("/w/b");
        assert_eq!(
            resolve_worktree(&list, "/w/a-wt/x", cwd).unwrap().path,
            Path::new("/w/a-wt/x")
        );
        assert!(
            resolve_worktree(&list, "../b-wt/x", cwd).is_err(),
            "relative paths are joined, not normalised: no such entry"
        );
        assert_eq!(
            resolve_worktree(&list, "b-wt/bisect", Path::new("/w"))
                .unwrap()
                .path,
            Path::new("/w/b-wt/bisect")
        );
        // A branch in one repository only.
        let list = &list[..3];
        assert_eq!(
            resolve_worktree(list, "feature/x", cwd).unwrap().path,
            Path::new("/w/a-wt/x")
        );
    }

    #[test]
    fn an_ambiguous_branch_is_a_usage_error_and_an_unknown_one_a_failure() {
        let list = fixture();
        let error = resolve_worktree(&list, "feature/x", Path::new("/")).unwrap_err();
        assert!(matches!(error, CliError::Usage(_)), "{error:?}");
        assert!(error.to_string().contains("/w/a-wt/x") && error.to_string().contains("/w/b-wt/x"));
        let error = resolve_worktree(&list, "nope", Path::new("/")).unwrap_err();
        assert!(matches!(error, CliError::Failed(_)));
    }

    #[test]
    fn an_existing_worktree_is_reused_and_the_main_checkout_is_refused() {
        let list = fixture();
        assert!(reusable(&list, "unknown", None).unwrap().is_none());
        // Two repositories have the branch: the directory decides.
        assert!(matches!(
            reusable(&list, "feature/x", None),
            Err(CliError::Usage(_))
        ));
        assert_eq!(
            reusable(&list, "feature/x", Some(Path::new("/w/b/src")))
                .unwrap()
                .unwrap()
                .path,
            Path::new("/w/b-wt/x")
        );
        // From inside a worktree the repository is the same.
        assert_eq!(
            reusable(&list, "feature/x", Some(Path::new("/w/a-wt/x/deep")))
                .unwrap()
                .unwrap()
                .path,
            Path::new("/w/a-wt/x")
        );
        let error = reusable(&list, "main", Some(Path::new("/w/a"))).unwrap_err();
        assert!(matches!(error, CliError::Failed(_)), "{error:?}");
        assert!(error.to_string().contains("main checkout"));
    }

    #[test]
    fn the_table_shows_branch_path_and_state() {
        let mut list = fixture();
        list[1].created_by_hutan = true;
        list[1].dirty = true;
        list[1].live_sessions = vec!["01K7F3B5N2Q9W8E4R6T1Y0V3KM".parse().unwrap()];
        list[4].locked = true;
        list[4].prunable = true;
        let table = worktree_table(&list);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 6);
        assert!(lines[0].starts_with("BRANCH") && lines[0].ends_with("STATE"));
        assert!(
            lines[1].contains("main") && lines[1].ends_with("main"),
            "{table}"
        );
        assert!(lines[2].ends_with("hutan, dirty, 1 session"), "{table}");
        assert!(
            lines[5].starts_with("(detached 3f78685)") && lines[5].ends_with("locked, prunable"),
            "{table}"
        );
        assert_eq!(worktree_table(&[]), "no worktrees");
    }

    #[test]
    fn finish_names_the_checkout_of_the_directory() {
        let list = fixture();
        assert_eq!(
            checkout_of(&list, Path::new("/w/a-wt/x/src")).unwrap().path,
            Path::new("/w/a-wt/x")
        );
        assert_eq!(
            checkout_of(&list, Path::new("/w/b")).unwrap().path,
            Path::new("/w/b")
        );
        assert!(checkout_of(&list, Path::new("/elsewhere")).is_err());
    }

    #[test]
    fn finish_says_what_each_step_did() {
        let worktree = wt("/w/a-wt/x", "/w/a", Some("feature/x"), false);
        let finished = WorktreeFinished {
            commit: FinishCommit {
                status: StepStatus::Ok,
                reason: None,
                message: None,
                branch: "feature/x".into(),
                commit: Some("7a6b5c4d3e2f1098".into()),
                files: 2,
            },
            merge: Some(FinishMerge {
                status: StepStatus::Ok,
                reason: None,
                message: None,
                branch: "feature/x".into(),
                target: "main".into(),
                strategy: MergeStrategy::Squash,
                commit: Some("9d8c7b6a5f4e3d2c".into()),
                conflicts: vec![],
                source_dirty: false,
            }),
            push: Some(FinishPush {
                status: StepStatus::Failed,
                reason: Some(StepReason::Rejected),
                message: Some("origin/main has 1 commit that main does not have".into()),
                branch: "main".into(),
                upstream: Some("origin/main".into()),
                commit: None,
                ahead: Some(1),
                behind: Some(1),
            }),
        };
        let text = finished_text(&worktree, &finished);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "commit: committed 2 files on feature/x (7a6b5c4)");
        assert_eq!(
            lines[1],
            "merge:  squash-merged feature/x into main (9d8c7b6)"
        );
        assert_eq!(
            lines[2],
            "push failed (rejected): origin/main has 1 commit that main does not have"
        );
        assert!(lines[3].contains("hutan wt rm /w/a-wt/x --delete-branch"));

        let local = WorktreeFinished {
            merge: None,
            push: None,
            ..finished
        };
        let text = finished_text(&worktree, &local);
        assert!(
            text.ends_with("not pushed (--no-push, or `[finish] push = false` in daemon.toml)")
        );
    }
}
