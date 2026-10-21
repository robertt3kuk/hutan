//! Review and merge (protocol 0.22, capability `worktree.review`): `worktree.diff`,
//! `worktree.file_diff` and `worktree.merge`.
//!
//! Diffs are read-only: every call runs `git --no-pager --literal-pathspecs diff --no-ext-diff
//! --no-textconv --no-color` (so no external program is started by a repository's attributes and
//! user configuration cannot change the output format), without optional locks and under a
//! timeout, against the merge base of the requested base and the checkout's `HEAD`. Output is
//! bounded: 2 MiB per file, 20 MiB in all, 5000 files; the reading git is killed at the bound.
//! Files git does not track yet are listed from the file system and diffed with `--no-index`,
//! nothing is staged or written to the repository.
//!
//! A merge runs in the repository's **main checkout** and leaves it as it was whenever it does
//! not succeed: every refusal is decided before the first change, and a merge that conflicts or
//! fails half way is aborted (`git merge --abort`, `git reset --merge` for a squash, `git rebase
//! --abort`). Nothing is pushed or fetched.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Component, Path};
use std::time::Duration;

use hutan_core::WorkspaceId;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::worktree::{
    CommitSummary, DiffFile, DiffStatus, MergeStrategy, WorktreeCommit, WorktreeCommitted,
    WorktreeDiff, WorktreeDiffRequest, WorktreeFileDiff, WorktreeFileDiffRequest, WorktreeMerge,
    WorktreeMerged,
};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{
    READ_TIMEOUT, RawWorktree, WORKTREE_MERGE_TIMEOUT, WorktreeService, bad_request, dirty,
    git_command, git_failed, git_timed_out, git_unavailable, invalid_state, list_raw, run_git,
    sessions_inside, validate_branch, workspaces_of_repo,
};

/// One file's patch is cut here.
pub(super) const FILE_PATCH_CAP: usize = 2 * 1024 * 1024;
/// Patches of one diff together are not read beyond this.
pub(super) const TOTAL_PATCH_CAP: usize = 20 * 1024 * 1024;
/// A diff lists at most this many files.
pub(super) const MAX_FILES: usize = 5000;
/// Diffing a large repository is slower than a status.
const DIFF_TIMEOUT: Duration = Duration::from_secs(60);
/// stderr is only read for the message of a failure.
const STDERR_CAP: usize = 64 * 1024;

// Capped git ----------------------------------------------------------------------------------

/// What a git process printed, stdout cut at a bound.
#[derive(Debug)]
struct Capped {
    stdout: Vec<u8>,
    /// More output was available than `cap`; git was killed.
    truncated: bool,
    code: Option<i32>,
    stderr: String,
}

/// Reads up to `cap` bytes; the flag says more was coming.
async fn read_capped<R: AsyncRead + Unpin>(mut reader: R, cap: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut chunk = vec![0_u8; 32 * 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return (out, false),
            Ok(n) => {
                let room = cap + 1 - out.len();
                out.extend_from_slice(&chunk[..n.min(room)]);
                if out.len() > cap {
                    out.truncate(cap);
                    return (out, true);
                }
            }
        }
    }
}

/// Like [`run_git`], but stdout is read only up to `cap` bytes: git is killed when it has more.
async fn run_git_capped(
    cwd: &Path,
    args: &[&OsStr],
    limit: Duration,
    cap: usize,
) -> Result<Capped, ErrorBody> {
    let mut command = git_command(cwd, args);
    let mut child = command.spawn().map_err(|e| git_unavailable(&e))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(ErrorBody::new(
            ErrorCode::Internal,
            "git was started without pipes",
        ));
    };
    let run = async {
        let errors = tokio::spawn(async move { read_capped(stderr, STDERR_CAP).await.0 });
        let (out, truncated) = read_capped(stdout, cap).await;
        if truncated {
            let _ = child.start_kill();
        }
        let status = child.wait().await;
        let err = errors.await.unwrap_or_default();
        (out, truncated, status, err)
    };
    match tokio::time::timeout(limit, run).await {
        Ok((stdout, truncated, status, err)) => Ok(Capped {
            stdout,
            truncated,
            code: status.map_err(|e| git_unavailable(&e))?.code(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
        }),
        Err(_) => Err(git_timed_out(args, limit)),
    }
}

/// `run_git` for string arguments.
pub(super) async fn git(
    cwd: &Path,
    args: &[&str],
    limit: Duration,
) -> Result<super::GitOutput, ErrorBody> {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    run_git(cwd, &args, limit).await
}

fn os_refs(args: &[OsString]) -> Vec<&OsStr> {
    args.iter().map(OsString::as_os_str).collect()
}

// The service -----------------------------------------------------------------------------------

impl WorktreeService {
    pub async fn diff(&self, req: WorktreeDiffRequest) -> Result<WorktreeDiff, ErrorBody> {
        let (main, entry) = self.checkout(&req.path).await?;
        let (base, merge_base) = resolve_base(&main, &entry, req.base.as_deref()).await?;
        let (mut files, patch_truncated) =
            collect_diff(&entry.path, &merge_base, req.include_uncommitted).await?;
        let (commits, commits_truncated) = commits_since(&entry.path, &merge_base).await?;
        if req.include_uncommitted {
            let scope = scope_of(&entry.path, &merge_base).await?;
            for file in &mut files {
                file.committed = scope.committed.contains(&file.path)
                    || file
                        .old_path
                        .as_ref()
                        .is_some_and(|old| scope.committed.contains(old));
                file.staged = scope.staged.contains(&file.path);
                file.unstaged = scope.unstaged.contains(&file.path);
                file.uncommitted = file.staged || file.unstaged;
            }
        } else {
            for file in &mut files {
                file.committed = true;
            }
        }
        Ok(WorktreeDiff {
            base,
            merge_base,
            files,
            patch_truncated,
            commits,
            commits_truncated,
        })
    }

    pub async fn file_diff(
        &self,
        req: WorktreeFileDiffRequest,
    ) -> Result<WorktreeFileDiff, ErrorBody> {
        validate_relative(&req.file)?;
        if let Some(old) = &req.old_path {
            validate_relative(old)?;
        }
        let (main, entry) = self.checkout(&req.path).await?;
        let (_, merge_base) = resolve_base(&main, &entry, req.base.as_deref()).await?;

        let mut files = vec![req.file.as_str()];
        files.extend(req.old_path.as_deref());
        let args = diff_args(&[], &merge_base, req.include_uncommitted, &files);
        let tracked =
            run_git_capped(&entry.path, &os_refs(&args), DIFF_TIMEOUT, FILE_PATCH_CAP).await?;
        if tracked.code != Some(0) && !tracked.truncated {
            return Err(invalid_state(format!(
                "git diff failed: {}",
                tracked.stderr.trim()
            )));
        }
        let mut capped = tracked;
        if capped.stdout.is_empty()
            && req.include_uncommitted
            && self.untracked(&entry.path, &req.file).await?
        {
            let args: Vec<OsString> = [
                "--no-pager",
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--",
                "/dev/null",
            ]
            .iter()
            .map(OsString::from)
            .chain([OsString::from(&req.file)])
            .collect();
            capped =
                run_git_capped(&entry.path, &os_refs(&args), DIFF_TIMEOUT, FILE_PATCH_CAP).await?;
            // `--no-index` exits 1 when the files differ.
            if !matches!(capped.code, Some(0 | 1)) && !capped.truncated {
                return Err(invalid_state(format!(
                    "git diff failed: {}",
                    capped.stderr.trim()
                )));
            }
        }
        let truncated = capped.truncated;
        let mut patch = capped.stdout;
        if truncated {
            // Whole lines only.
            let keep = patch.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
            patch.truncate(keep);
        }
        let patch = String::from_utf8_lossy(&patch).into_owned();
        let binary = patch.lines().any(|line| line.starts_with("Binary files "));
        Ok(WorktreeFileDiff {
            patch,
            truncated,
            binary,
        })
    }

    /// Whether `file` is a file of the checkout that git does not track and does not ignore.
    async fn untracked(&self, root: &Path, file: &str) -> Result<bool, ErrorBody> {
        let listed = git(
            root,
            &[
                "--literal-pathspecs",
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                file,
            ],
            READ_TIMEOUT,
        )
        .await?;
        Ok(listed.success && !listed.stdout.is_empty())
    }

    /// The listed worktree at `path` and its repository's main checkout; a directory that is
    /// gone has nothing to show.
    pub(super) async fn checkout(
        &self,
        path: &Path,
    ) -> Result<(std::path::PathBuf, RawWorktree), ErrorBody> {
        if !path.is_absolute() {
            return Err(bad_request(format!(
                "`{}` is not an absolute path",
                path.display()
            )));
        }
        let wanted = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let (main, entry) = self.find(&workspaces, &wanted, path).await?;
        if entry.prunable || entry.bare {
            return Err(invalid_state(format!(
                "{} is not a usable checkout (its directory is gone, or it is bare)",
                entry.path.display()
            )));
        }
        Ok((main, entry))
    }

    /// Merges the worktree's branch; besides the result, the workspaces of the repository (the
    /// task service marks the work of a task on that branch `merged`).
    pub async fn merge(
        &self,
        req: WorktreeMerge,
    ) -> Result<(WorktreeMerged, Vec<WorkspaceId>), ErrorBody> {
        let _writing = self.writes.lock().await;
        self.merge_locked(req).await
    }

    /// [`merge`](Self::merge), for a caller that holds the write lock.
    pub(super) async fn merge_locked(
        &self,
        req: WorktreeMerge,
    ) -> Result<(WorktreeMerged, Vec<WorkspaceId>), ErrorBody> {
        let message = req
            .message
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty());
        if req.strategy == MergeStrategy::Rebase && message.is_some() {
            return Err(bad_request(
                "a rebase has no commit message of its own; drop the message or merge/squash",
            ));
        }
        let (main, entry) = self.checkout(&req.path).await?;
        let (branch, target) = self.merge_target(&main, &entry, req.into.clone()).await?;
        let head_ref = format!("refs/heads/{branch}");
        let target_ref = format!("refs/heads/{target}");
        if git_ok_in(
            &main,
            &["merge-base", "--is-ancestor", &head_ref, &target_ref],
        )
        .await?
        {
            return Err(invalid_state(format!(
                "`{branch}` has nothing to merge: `{target}` already contains it"
            )));
        }
        let source_dirty = dirty(&entry.path).await?;
        if req.strategy == MergeStrategy::Rebase {
            if source_dirty {
                return Err(invalid_state(format!(
                    "{} has uncommitted changes; a rebase needs them committed or stashed",
                    entry.path.display()
                )));
            }
            if let Some(operation) = operation_in_progress(&entry.path).await? {
                return Err(invalid_state(format!(
                    "{operation} is in progress in {}; finish or abort it first",
                    entry.path.display()
                )));
            }
            self.refuse_live_sessions(&entry.path).await?;
        }
        self.run_merge(
            &main,
            &entry.path,
            &branch,
            &target,
            req.strategy,
            message,
            source_dirty,
        )
        .await
    }

    /// A rebase rewrites the worktree's files under any session working in it.
    pub(super) async fn refuse_live_sessions(&self, root: &Path) -> Result<(), ErrorBody> {
        let sessions = self.live_sessions().await?;
        if sessions_inside(&sessions, root).is_empty() {
            Ok(())
        } else {
            Err(invalid_state(format!(
                "live sessions work in {}; a rebase rewrites their files — stop them first",
                root.display()
            )))
        }
    }

    /// The branch of the worktree `entry` and the branch of the main checkout `main` it merges
    /// into, with every refusal of a merge that no commit in the worktree could change: the main
    /// checkout itself, a detached HEAD, a target that is no branch or not the one checked out
    /// in the main checkout, an operation in progress or uncommitted changes there.
    pub(super) async fn merge_target(
        &self,
        main: &Path,
        entry: &RawWorktree,
        into: Option<String>,
    ) -> Result<(String, String), ErrorBody> {
        if entry.is_main {
            return Err(invalid_state(format!(
                "{} is the main checkout of {}; there is nothing to merge it into",
                entry.path.display(),
                main.display()
            )));
        }
        let Some(branch) = entry.branch.clone() else {
            return Err(invalid_state(format!(
                "{} has a detached HEAD; check a branch out there first (`git switch -c <name>`)",
                entry.path.display()
            )));
        };

        // Everything that can refuse, before the first change.
        let target = match into {
            Some(into) => into,
            None => default_branch(main).await.ok_or_else(|| {
                invalid_state("the repository has no default branch (main, master); pass `into`")
            })?,
        };
        if target.starts_with('-') || target.contains('\0') {
            return Err(bad_request(format!(
                "`{target}` is not a valid branch name"
            )));
        }
        validate_branch(main, &target).await?;
        if !branch_exists(main, &target).await? {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("there is no local branch `{target}` to merge into"),
            ));
        }
        if target == branch {
            return Err(invalid_state(format!(
                "`{branch}` is the target itself; there is nothing to merge"
            )));
        }
        let listed = list_raw(main).await?;
        let main_entry = listed
            .iter()
            .find(|w| w.is_main)
            .ok_or_else(|| invalid_state(format!("{} has no main checkout", main.display())))?;
        if main_entry.branch.as_deref() != Some(target.as_str()) {
            let elsewhere = listed
                .iter()
                .find(|w| w.branch.as_deref() == Some(target.as_str()))
                .map_or_else(String::new, |w| {
                    format!("; it is checked out at {}", w.path.display())
                });
            return Err(invalid_state(format!(
                "`{target}` is not checked out in the main checkout {} (which has {}){elsewhere}; \
                 check it out there first",
                main_entry.path.display(),
                main_entry
                    .branch
                    .as_deref()
                    .map_or_else(|| "a detached HEAD".to_owned(), |b| format!("`{b}`"))
            )));
        }
        if let Some(operation) = operation_in_progress(main).await? {
            return Err(invalid_state(format!(
                "{operation} is in progress in the main checkout {}; finish or abort it first",
                main.display()
            )));
        }
        if dirty(main).await? {
            return Err(invalid_state(format!(
                "the main checkout {} has uncommitted changes; commit or stash them first",
                main.display()
            )));
        }
        Ok((branch, target))
    }

    /// The git steps of a merge whose refusals are all decided; the main checkout is left as it
    /// was when one fails.
    #[allow(clippy::too_many_arguments)] // the decided merge, field by field
    async fn run_merge(
        &self,
        main: &Path,
        worktree: &Path,
        branch: &str,
        target: &str,
        strategy: MergeStrategy,
        message: Option<&str>,
        source_dirty: bool,
    ) -> Result<(WorktreeMerged, Vec<WorkspaceId>), ErrorBody> {
        let head_ref = format!("refs/heads/{branch}");
        let target_ref = format!("refs/heads/{target}");
        match strategy {
            MergeStrategy::Merge => {
                let mut args = vec!["merge", "--no-ff", "--no-edit", "--no-verify"];
                if let Some(message) = message {
                    args.extend(["-m", message]);
                }
                // The branch name, not its full ref: it is in the default message.
                args.push(branch);
                run_merge_step(main, &args, target, branch, Abort::Merge).await?;
            }
            MergeStrategy::Squash => {
                run_merge_step(
                    main,
                    &["merge", "--squash", "--no-verify", branch],
                    target,
                    branch,
                    Abort::Reset,
                )
                .await?;
                let default = format!("Squash merge branch '{branch}'");
                let committed = git(
                    main,
                    &[
                        "commit",
                        "--no-verify",
                        "--cleanup=strip",
                        "-m",
                        message.unwrap_or(&default),
                    ],
                    WORKTREE_MERGE_TIMEOUT,
                )
                .await;
                match committed {
                    Ok(out) if out.success => {}
                    Ok(out) => {
                        let error = git_failed("commit", &out);
                        return Err(abort(main, Abort::Reset, error).await);
                    }
                    Err(error) => return Err(abort(main, Abort::Reset, error).await),
                }
            }
            MergeStrategy::Rebase => {
                run_merge_step(
                    worktree,
                    &["rebase", "--no-verify", &target_ref],
                    target,
                    branch,
                    Abort::Rebase,
                )
                .await?;
                let forwarded = git(
                    main,
                    &["merge", "--ff-only", "--no-verify", &head_ref],
                    WORKTREE_MERGE_TIMEOUT,
                )
                .await;
                match forwarded {
                    Ok(out) if out.success => {}
                    Ok(out) => {
                        let error = git_failed("merge --ff-only", &out);
                        return Err(abort(main, Abort::Merge, ErrorBody::new(
                            error.code,
                            format!(
                                "`{branch}` was rebased onto `{target}`, but `{target}` could not \
                                 move to it: {}",
                                error.message
                            ),
                        ))
                        .await);
                    }
                    Err(error) => return Err(abort(main, Abort::Merge, error).await),
                }
            }
        }

        let tip = git(main, &["rev-parse", "--verify", "HEAD"], READ_TIMEOUT).await?;
        if !tip.success {
            return Err(git_failed("rev-parse", &tip));
        }
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let affected = workspaces_of_repo(&workspaces, main).await;
        self.changed(&affected);
        Ok((
            WorktreeMerged {
                commit: String::from_utf8_lossy(&tip.stdout).trim().to_owned(),
                target: target.to_owned(),
                branch: branch.to_owned(),
                strategy,
                source_dirty: strategy != MergeStrategy::Rebase && source_dirty,
            },
            affected,
        ))
    }

    /// `worktree.commit`: everything in a worktree into one commit, so a merge takes it.
    pub async fn commit(&self, req: WorktreeCommit) -> Result<WorktreeCommitted, ErrorBody> {
        let _writing = self.writes.lock().await;
        let message = req.message.trim();
        if message.is_empty() {
            return Err(bad_request("a commit needs a message"));
        }
        let (_, entry) = self.checkout(&req.path).await?;
        if entry.is_main {
            return Err(invalid_state(format!(
                "{} is the main checkout; commit there yourself",
                entry.path.display()
            )));
        }
        let Some(branch) = entry.branch.clone() else {
            return Err(invalid_state(format!(
                "{} has a detached HEAD; check a branch out there first (`git switch -c <name>`)",
                entry.path.display()
            )));
        };
        if let Some(operation) = operation_in_progress(&entry.path).await? {
            return Err(invalid_state(format!(
                "{operation} is in progress in {}; finish or abort it first",
                entry.path.display()
            )));
        }
        if !dirty(&entry.path).await? {
            return Err(invalid_state(format!(
                "{} has nothing to commit",
                entry.path.display()
            )));
        }
        let (commit, files) = commit_all(&entry.path, message).await?;
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        if let Ok((main, _)) = self.checkout(&req.path).await {
            self.changed(&workspaces_of_repo(&workspaces, &main).await);
        }
        Ok(WorktreeCommitted {
            commit,
            branch,
            files,
        })
    }
}

/// Stages everything in the checkout `dir` (`git add -A`) and commits it with `message`, hooks
/// off; the new commit's hash and how many files it changed. When git refuses, the index is
/// left as it was.
pub(super) async fn commit_all(dir: &Path, message: &str) -> Result<(String, u32), ErrorBody> {
    let staged = git(dir, &["add", "-A"], WORKTREE_MERGE_TIMEOUT).await?;
    if !staged.success {
        return Err(git_failed("add", &staged));
    }
    let committed = git(
        dir,
        &["commit", "--no-verify", "--cleanup=strip", "-m", message],
        WORKTREE_MERGE_TIMEOUT,
    )
    .await;
    match committed {
        Ok(out) if out.success => {}
        failed => {
            // Leave the index as it was: nothing the caller did not ask for stays staged.
            let _ = git(dir, &["reset", "--quiet"], WORKTREE_MERGE_TIMEOUT).await;
            return Err(match failed {
                Ok(out) => git_failed("commit", &out),
                Err(error) => error,
            });
        }
    }
    let tip = git(dir, &["rev-parse", "--verify", "HEAD"], READ_TIMEOUT).await?;
    if !tip.success {
        return Err(git_failed("rev-parse", &tip));
    }
    let changed = git(
        dir,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "-z",
            "--root",
            "HEAD",
        ],
        READ_TIMEOUT,
    )
    .await?;
    let files = changed
        .stdout
        .split(|b| *b == 0)
        .filter(|name| !name.is_empty())
        .count();
    Ok((
        String::from_utf8_lossy(&tip.stdout).trim().to_owned(),
        u32::try_from(files).unwrap_or(u32::MAX),
    ))
}

// Merging -----------------------------------------------------------------------------------

/// How an interrupted step is undone.
#[derive(Debug, Clone, Copy)]
enum Abort {
    /// `git merge --abort`.
    Merge,
    /// `git reset --merge`: a squash leaves no `MERGE_HEAD` to abort.
    Reset,
    /// `git rebase --abort`.
    Rebase,
}

/// Runs the git step that changes `dir`; when it fails the step is undone, and conflicts come
/// back as `conflict` with the files in `data.conflicts`.
async fn run_merge_step(
    dir: &Path,
    args: &[&str],
    target: &str,
    branch: &str,
    undo: Abort,
) -> Result<(), ErrorBody> {
    let output = match git(dir, args, WORKTREE_MERGE_TIMEOUT).await {
        Ok(output) => output,
        // A timeout killed git half way.
        Err(error) => return Err(abort(dir, undo, error).await),
    };
    if output.success {
        return Ok(());
    }
    let conflicts = unmerged_files(dir).await;
    let failure = if conflicts.is_empty() {
        git_failed(args.first().copied().unwrap_or("merge"), &output)
    } else {
        let mut error = ErrorBody::new(
            ErrorCode::Conflict,
            format!(
                "merging `{branch}` into `{target}` conflicts in {} file(s): {}; nothing was changed",
                conflicts.len(),
                conflicts.join(", ")
            ),
        );
        error.data = Some(serde_json::json!({ "conflicts": conflicts }));
        error
    };
    Err(abort(dir, undo, failure).await)
}

/// Undoes a failed step in `dir` and returns `error`, with a word when the undo failed too.
async fn abort(dir: &Path, undo: Abort, mut error: ErrorBody) -> ErrorBody {
    let args: &[&str] = match undo {
        Abort::Merge => &["merge", "--abort"],
        Abort::Reset => &["reset", "--merge"],
        Abort::Rebase => &["rebase", "--abort"],
    };
    let state_present = match undo {
        Abort::Merge => path_in_git_dir(dir, "MERGE_HEAD").await,
        Abort::Rebase => {
            path_in_git_dir(dir, "rebase-merge").await || path_in_git_dir(dir, "rebase-apply").await
        }
        Abort::Reset => true,
    };
    if state_present {
        match git(dir, args, READ_TIMEOUT).await {
            Ok(out) if out.success => {}
            Ok(out) => {
                error.message = format!(
                    "{} (and `git {}` failed in {}: {}; resolve it by hand)",
                    error.message,
                    args.join(" "),
                    dir.display(),
                    out.message()
                );
            }
            Err(other) => {
                error.message = format!(
                    "{} (and `git {}` could not run in {}: {})",
                    error.message,
                    args.join(" "),
                    dir.display(),
                    other.message
                );
            }
        }
    }
    error
}

/// The files with unresolved conflicts in `dir`, sorted.
async fn unmerged_files(dir: &Path) -> Vec<String> {
    let Ok(out) = git(
        dir,
        &["diff", "--name-only", "--diff-filter=U", "-z"],
        READ_TIMEOUT,
    )
    .await
    else {
        return Vec::new();
    };
    let mut files: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    files.sort();
    files.dedup();
    files
}

/// Whether git keeps `name` in the private directory of the checkout `dir`.
async fn path_in_git_dir(dir: &Path, name: &str) -> bool {
    match git(dir, &["rev-parse", "--git-path", name], READ_TIMEOUT).await {
        Ok(out) if out.success => {
            let printed = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            let path = Path::new(&printed);
            if path.is_absolute() {
                path.exists()
            } else {
                dir.join(path).exists()
            }
        }
        _ => false,
    }
}

/// What git is in the middle of in `dir`, if anything.
pub(super) async fn operation_in_progress(dir: &Path) -> Result<Option<&'static str>, ErrorBody> {
    for (name, what) in [
        ("MERGE_HEAD", "a merge"),
        ("rebase-merge", "a rebase"),
        ("rebase-apply", "a rebase"),
        ("CHERRY_PICK_HEAD", "a cherry-pick"),
        ("REVERT_HEAD", "a revert"),
        ("BISECT_LOG", "a bisect"),
    ] {
        if path_in_git_dir(dir, name).await {
            return Ok(Some(what));
        }
    }
    Ok(None)
}

pub(super) async fn git_ok_in(dir: &Path, args: &[&str]) -> Result<bool, ErrorBody> {
    Ok(git(dir, args, READ_TIMEOUT).await?.success)
}

async fn branch_exists(dir: &Path, branch: &str) -> Result<bool, ErrorBody> {
    git_ok_in(
        dir,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await
}

// Bases -----------------------------------------------------------------------------------------

/// The repository's default branch: the branch `origin/HEAD` points to (its local branch when
/// there is one), else `main`, else `master`.
async fn default_branch(main: &Path) -> Option<String> {
    if let Ok(out) = git(
        main,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
        READ_TIMEOUT,
    )
    .await
        && out.success
    {
        let remote = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if let Some(local) = remote.strip_prefix("origin/")
            && !local.is_empty()
            && branch_exists(main, local).await.unwrap_or(false)
        {
            return Some(local.to_owned());
        }
        if !remote.is_empty()
            && git_ok_in(
                main,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{remote}^{{commit}}"),
                ],
            )
            .await
            .unwrap_or(false)
        {
            return Some(remote);
        }
    }
    for candidate in ["main", "master"] {
        if branch_exists(main, candidate).await.unwrap_or(false) {
            return Some(candidate.to_owned());
        }
    }
    None
}

/// The base to compare against and the merge base of it and the checkout's `HEAD`.
async fn resolve_base(
    main: &Path,
    entry: &RawWorktree,
    base: Option<&str>,
) -> Result<(String, String), ErrorBody> {
    let base = match base {
        Some(base) => {
            if base.is_empty() || base.starts_with('-') || base.contains('\0') {
                return Err(bad_request(format!("`{base}` is not a valid base")));
            }
            if !git_ok_in(
                main,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{base}^{{commit}}"),
                ],
            )
            .await?
            {
                return Err(bad_request(format!("`{base}` is not a branch or commit")));
            }
            base.to_owned()
        }
        None => default_branch(main).await.ok_or_else(|| {
            invalid_state("the repository has no default branch (main, master); pass `base`")
        })?,
    };
    let found = git(
        &entry.path,
        &["merge-base", &format!("{base}^{{commit}}"), "HEAD"],
        READ_TIMEOUT,
    )
    .await?;
    let merge_base = String::from_utf8_lossy(&found.stdout).trim().to_owned();
    if !found.success || merge_base.is_empty() {
        return Err(invalid_state(format!(
            "`{base}` and the checkout at {} share no history",
            entry.path.display()
        )));
    }
    Ok((base, merge_base))
}

/// A file name inside a checkout: relative, no `..`, no NUL.
fn validate_relative(file: &str) -> Result<(), ErrorBody> {
    let path = Path::new(file);
    let plain = !file.is_empty()
        && !file.contains('\0')
        && path.components().all(|c| matches!(c, Component::Normal(_)));
    if plain {
        Ok(())
    } else {
        Err(bad_request(format!(
            "`{file}` is not a path inside the checkout"
        )))
    }
}

// Diffs -----------------------------------------------------------------------------------------

/// `git diff` of the checkout against `merge_base`: `mode` picks the output (`--numstat -z`,
/// none for a patch), `files` limit it. Without `include_uncommitted` the comparison ends at
/// `HEAD`.
fn diff_args(
    mode: &[&str],
    merge_base: &str,
    include_uncommitted: bool,
    files: &[&str],
) -> Vec<OsString> {
    let mut args = diff_prefix();
    args.extend(mode.iter().map(OsString::from));
    args.push(OsString::from(merge_base));
    if !include_uncommitted {
        args.push(OsString::from("HEAD"));
    }
    args.push(OsString::from("--"));
    args.extend(files.iter().map(OsString::from));
    args
}

/// `git diff` and the options every diff of the review uses (no pager, no external diff or
/// text conversion, fixed prefixes, renames found).
fn diff_prefix() -> Vec<OsString> {
    [
        "--no-pager",
        "--literal-pathspecs",
        "-c",
        "core.fsmonitor=false",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--submodule=short",
        "-M",
    ]
    .iter()
    .map(OsString::from)
    .collect()
}

/// The changed files of the checkout at `root` and whether some patch is too big to show whole.
async fn collect_diff(
    root: &Path,
    merge_base: &str,
    include_uncommitted: bool,
) -> Result<(Vec<DiffFile>, bool), ErrorBody> {
    let names_args = diff_args(
        &["--name-status", "-z"],
        merge_base,
        include_uncommitted,
        &[],
    );
    let stats_args = diff_args(&["--numstat", "-z"], merge_base, include_uncommitted, &[]);
    let patch_args = diff_args(&[], merge_base, include_uncommitted, &[]);
    let untracked_args: Vec<&str> = vec!["ls-files", "--others", "--exclude-standard", "-z"];
    let (names_refs, stats_refs, patch_refs) = (
        os_refs(&names_args),
        os_refs(&stats_args),
        os_refs(&patch_args),
    );
    let (names, stats, patch, untracked) = tokio::join!(
        run_git(root, &names_refs, DIFF_TIMEOUT),
        run_git(root, &stats_refs, DIFF_TIMEOUT),
        run_git_capped(root, &patch_refs, DIFF_TIMEOUT, TOTAL_PATCH_CAP),
        async {
            if include_uncommitted {
                git(root, &untracked_args, READ_TIMEOUT).await.map(Some)
            } else {
                Ok(None)
            }
        },
    );
    let names = names?;
    if !names.success {
        return Err(git_failed("diff --name-status", &names));
    }
    let stats = stats?;
    if !stats.success {
        return Err(git_failed("diff --numstat", &stats));
    }
    let patch = patch?;
    if patch.code != Some(0) && !patch.truncated {
        return Err(invalid_state(format!(
            "git diff failed: {}",
            patch.stderr.trim()
        )));
    }

    let counts: HashMap<String, NumStat> = parse_numstat(&stats.stdout)
        .into_iter()
        .map(|stat| (stat.path.clone(), stat))
        .collect();
    let mut files: Vec<DiffFile> = parse_name_status(&names.stdout)
        .into_iter()
        .map(|entry| {
            let stat = counts.get(&entry.path);
            DiffFile {
                additions: stat.map_or(0, |s| s.additions),
                deletions: stat.map_or(0, |s| s.deletions),
                binary: stat.is_some_and(|s| s.binary),
                path: entry.path,
                old_path: entry.old_path,
                status: entry.status,
                committed: false,
                uncommitted: false,
                staged: false,
                unstaged: false,
            }
        })
        .collect();

    let mut truncated = patch.truncated;
    let mut total = patch.stdout.len();
    if patch_sizes(&patch.stdout)
        .iter()
        .any(|size| *size > FILE_PATCH_CAP)
    {
        truncated = true;
    }
    if let Some(listed) = untracked? {
        if !listed.success {
            return Err(git_failed("ls-files", &listed));
        }
        let names: Vec<String> = listed
            .stdout
            .split(|b| *b == 0)
            .filter(|n| !n.is_empty() && !n.ends_with(b"/"))
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .collect();
        let root = root.to_path_buf();
        let (added, sizes) = tokio::task::spawn_blocking(move || untracked_files(&root, names))
            .await
            .map_err(super::join_failed)?;
        total += sizes.iter().sum::<usize>();
        if sizes.iter().any(|size| *size > FILE_PATCH_CAP) {
            truncated = true;
        }
        files.extend(added);
    }
    if total > TOTAL_PATCH_CAP {
        truncated = true;
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    if files.len() > MAX_FILES {
        files.truncate(MAX_FILES);
        truncated = true;
    }
    Ok((files, truncated))
}

/// The most commits `worktree.diff` lists.
const MAX_COMMITS: usize = 200;
/// Longest commit subject sent to clients, in characters.
const SUBJECT_MAX_CHARS: usize = 200;

/// The commits of `HEAD` since `merge_base`, newest first, and whether there are more than the
/// list holds: what a merge of the branch integrates.
async fn commits_since(
    root: &Path,
    merge_base: &str,
) -> Result<(Vec<CommitSummary>, bool), ErrorBody> {
    let limit = format!("--max-count={}", MAX_COMMITS + 1);
    let range = format!("{merge_base}..HEAD");
    let log = git(
        root,
        &[
            "log",
            "--no-color",
            "--no-show-signature",
            "--format=%H%x1f%s%x1e",
            &limit,
            &range,
            "--",
        ],
        READ_TIMEOUT,
    )
    .await?;
    if !log.success {
        return Err(git_failed("log", &log));
    }
    let mut commits = parse_log(&log.stdout);
    let truncated = commits.len() > MAX_COMMITS;
    commits.truncate(MAX_COMMITS);
    Ok((commits, truncated))
}

/// Parses `git log --format=%H%x1f%s%x1e`.
fn parse_log(output: &[u8]) -> Vec<CommitSummary> {
    String::from_utf8_lossy(output)
        .split('\x1e')
        .filter_map(|record| {
            let (id, subject) = record.trim_start_matches('\n').split_once('\x1f')?;
            (!id.is_empty()).then(|| CommitSummary {
                id: id.to_owned(),
                subject: subject.chars().take(SUBJECT_MAX_CHARS).collect(),
            })
        })
        .collect()
}

/// What the changes of a checkout are made of, by path (renames under their new path).
#[derive(Debug, Default)]
struct Scope {
    /// Changed by the commits since the merge base.
    committed: HashSet<String>,
    /// In the index: what a commit would take now (`git diff --cached HEAD`).
    staged: HashSet<String>,
    /// In the working tree but not the index (`git diff`), and untracked, not ignored files.
    unstaged: HashSet<String>,
}

/// The paths changed by commits since `merge_base`, the staged ones and the unstaged ones
/// (protocol 0.34, `worktree.index`); a path in neither of the last two is not uncommitted.
async fn scope_of(root: &Path, merge_base: &str) -> Result<Scope, ErrorBody> {
    let committed_args = diff_args(&["--name-status", "-z"], merge_base, false, &[]);
    let staged_args = diff_args(&["--cached", "--name-status", "-z"], "HEAD", true, &[]);
    let mut unstaged_args = diff_prefix();
    unstaged_args.extend(["--name-status", "-z", "--"].map(OsString::from));
    let untracked_args: Vec<&str> = vec!["ls-files", "--others", "--exclude-standard", "-z"];
    let (committed_refs, staged_refs, unstaged_refs) = (
        os_refs(&committed_args),
        os_refs(&staged_args),
        os_refs(&unstaged_args),
    );
    let (committed, staged, unstaged, untracked) = tokio::join!(
        run_git(root, &committed_refs, DIFF_TIMEOUT),
        run_git(root, &staged_refs, DIFF_TIMEOUT),
        run_git(root, &unstaged_refs, DIFF_TIMEOUT),
        git(root, &untracked_args, READ_TIMEOUT),
    );
    let (committed, staged, unstaged, untracked) = (committed?, staged?, unstaged?, untracked?);
    for (what, output) in [
        ("diff --name-status", &committed),
        ("diff --cached --name-status", &staged),
        ("diff --name-status", &unstaged),
    ] {
        if !output.success {
            return Err(git_failed(what, output));
        }
    }
    if !untracked.success {
        return Err(git_failed("ls-files", &untracked));
    }
    let paths = |stdout: &[u8]| -> HashSet<String> {
        parse_name_status(stdout)
            .into_iter()
            .map(|entry| entry.path)
            .collect()
    };
    let mut scope = Scope {
        committed: paths(&committed.stdout),
        staged: paths(&staged.stdout),
        unstaged: paths(&unstaged.stdout),
    };
    scope.unstaged.extend(
        untracked
            .stdout
            .split(|b| *b == 0)
            .filter(|n| !n.is_empty() && !n.ends_with(b"/"))
            .map(|n| String::from_utf8_lossy(n).into_owned()),
    );
    Ok(scope)
}

/// One entry of `git diff --name-status -z`.
#[derive(Debug, PartialEq, Eq)]
struct NameStatus {
    path: String,
    old_path: Option<String>,
    status: DiffStatus,
}

/// Parses `git diff --name-status -z -M`: `X NUL path NUL`, and `Rnn NUL old NUL new NUL`
/// (`Cnn` likewise) for renames and copies.
fn parse_name_status(output: &[u8]) -> Vec<NameStatus> {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let mut fields = output.split(|b| *b == 0);
    let mut entries = Vec::new();
    while let Some(code) = fields.next() {
        let Some(letter) = code.first() else {
            continue;
        };
        let moved = matches!(letter, b'R' | b'C');
        // A path is never empty: the empty field after the last NUL is the end of the output.
        let Some(first) = fields.next().filter(|f| !f.is_empty()) else {
            break;
        };
        let (old_path, path) = if moved {
            let Some(second) = fields.next().filter(|f| !f.is_empty()) else {
                break;
            };
            (Some(text(first)), text(second))
        } else {
            (None, text(first))
        };
        let status = match letter {
            b'A' | b'C' => DiffStatus::Added,
            b'D' => DiffStatus::Deleted,
            b'R' => DiffStatus::Renamed,
            // M, T (type change), U and whatever git adds.
            _ => DiffStatus::Modified,
        };
        // A copy is a new file as far as this view is concerned.
        let old_path = if *letter == b'C' { None } else { old_path };
        entries.push(NameStatus {
            path,
            old_path,
            status,
        });
    }
    entries
}

/// One entry of `git diff --numstat -z`.
#[derive(Debug, PartialEq, Eq)]
struct NumStat {
    path: String,
    additions: u32,
    deletions: u32,
    binary: bool,
}

/// Parses `git diff --numstat -z -M`: `adds TAB dels TAB path NUL`, and for renames
/// `adds TAB dels TAB NUL old NUL new NUL`; binary files count `-`.
fn parse_numstat(output: &[u8]) -> Vec<NumStat> {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let mut fields = output.split(|b| *b == 0);
    let mut entries = Vec::new();
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(3, |b| *b == b'\t');
        let (Some(adds), Some(dels), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let path = if path.is_empty() {
            // A rename: the old and the new name follow as fields of their own.
            let (Some(_old), Some(new)) = (fields.next(), fields.next()) else {
                break;
            };
            text(new)
        } else {
            text(path)
        };
        let binary = adds == b"-" || dels == b"-";
        let number = |digits: &[u8]| text(digits).parse::<u32>().unwrap_or(0);
        entries.push(NumStat {
            path,
            additions: if binary { 0 } else { number(adds) },
            deletions: if binary { 0 } else { number(dels) },
            binary,
        });
    }
    entries
}

/// The byte size of each file's part of a unified diff (split at `diff --git ` header lines).
fn patch_sizes(patch: &[u8]) -> Vec<usize> {
    const HEADER: &[u8] = b"diff --git ";
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0;
    while at < patch.len() {
        if patch[at..].starts_with(HEADER) {
            starts.push(at);
        }
        match patch[at..].iter().position(|b| *b == b'\n') {
            Some(end) => at += end + 1,
            None => break,
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, start)| starts.get(i + 1).copied().unwrap_or(patch.len()) - start)
        .collect()
}

/// Untracked files as added ones: line counts read from the file system (a symlink counts as
/// the one line git shows for it), and the size their patches would have.
fn untracked_files(root: &Path, names: Vec<String>) -> (Vec<DiffFile>, Vec<usize>) {
    let mut files = Vec::new();
    let mut sizes = Vec::new();
    for name in names {
        let path = root.join(&name);
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let (additions, binary, size) = if meta.file_type().is_symlink() {
            (1, false, usize::try_from(meta.len()).unwrap_or(usize::MAX))
        } else if meta.is_file() {
            let mut head = Vec::new();
            let read = fs::File::open(&path)
                .and_then(|file| file.take(FILE_PATCH_CAP as u64 + 1).read_to_end(&mut head));
            if read.is_err() {
                continue;
            }
            // git's own test: a NUL in the first 8000 bytes.
            let binary = head.iter().take(8000).any(|b| *b == 0);
            let lines = if binary {
                0
            } else {
                let newlines = head.iter().filter(|b| **b == b'\n').count();
                newlines + usize::from(head.last().is_some_and(|b| *b != b'\n'))
            };
            (
                u32::try_from(lines).unwrap_or(u32::MAX),
                binary,
                usize::try_from(meta.len()).unwrap_or(usize::MAX),
            )
        } else {
            continue;
        };
        files.push(DiffFile {
            path: name,
            old_path: None,
            status: DiffStatus::Added,
            additions,
            deletions: 0,
            binary,
            committed: false,
            uncommitted: false,
            staged: false,
            unstaged: false,
        });
        sizes.push(size);
    }
    (files, sizes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_status_reads_every_status_and_renames_with_both_paths() {
        let raw = b"M\0src/a.rs\0A\0new.txt\0D\0gone.txt\0R100\0old name.txt\0new name.txt\0\
                    T\0link\0C75\0orig\0copy\0";
        let parsed = parse_name_status(raw);
        assert_eq!(
            parsed,
            vec![
                NameStatus {
                    path: "src/a.rs".into(),
                    old_path: None,
                    status: DiffStatus::Modified
                },
                NameStatus {
                    path: "new.txt".into(),
                    old_path: None,
                    status: DiffStatus::Added
                },
                NameStatus {
                    path: "gone.txt".into(),
                    old_path: None,
                    status: DiffStatus::Deleted
                },
                NameStatus {
                    path: "new name.txt".into(),
                    old_path: Some("old name.txt".into()),
                    status: DiffStatus::Renamed
                },
                NameStatus {
                    path: "link".into(),
                    old_path: None,
                    status: DiffStatus::Modified
                },
                NameStatus {
                    path: "copy".into(),
                    old_path: None,
                    status: DiffStatus::Added
                },
            ]
        );
        assert!(parse_name_status(b"").is_empty());
        // A cut-off record is dropped, not misread.
        assert!(parse_name_status(b"R90\0only-old\0").is_empty());
    }

    #[test]
    fn numstat_reads_counts_binary_and_renames() {
        let raw = b"3\t1\tsrc/a.rs\0-\t-\timg.png\0\x30\t0\t\0old.txt\0new.txt\0\
                    10\t0\tpath with\ttab.txt\0";
        let parsed = parse_numstat(raw);
        assert_eq!(parsed.len(), 4);
        assert_eq!((parsed[0].additions, parsed[0].deletions), (3, 1));
        assert!(parsed[1].binary && parsed[1].additions == 0);
        assert_eq!(parsed[2].path, "new.txt");
        assert_eq!(parsed[3].path, "path with\ttab.txt");
        assert_eq!(parsed[3].additions, 10);
    }

    #[test]
    fn patch_sizes_split_at_header_lines_only() {
        let patch = b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+diff --git inside\ndiff --git a/y b/y\nnew file mode 100644\n";
        let sizes = patch_sizes(patch);
        assert_eq!(sizes.len(), 2, "{sizes:?}");
        assert_eq!(sizes.iter().sum::<usize>(), patch.len());
        assert!(patch_sizes(b"").is_empty());
    }

    #[test]
    fn file_names_must_stay_inside_the_checkout() {
        assert!(validate_relative("src/a.rs").is_ok());
        assert!(validate_relative("a b/c d").is_ok());
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a\0b", "./a/.."] {
            assert!(validate_relative(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn diff_arguments_pin_the_output_and_end_at_head_without_uncommitted() {
        let with = diff_args(&["--numstat"], "abc", true, &["f"]);
        let without = diff_args(&[], "abc", false, &[]);
        let text = |args: &[OsString]| {
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let with = text(&with);
        assert!(with.starts_with("--no-pager --literal-pathspecs"), "{with}");
        for needle in [
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "-M --numstat abc -- f",
        ] {
            assert!(with.contains(needle), "{needle}: {with}");
        }
        assert!(
            text(&without).ends_with("-M abc HEAD --"),
            "{}",
            text(&without)
        );
    }
}
