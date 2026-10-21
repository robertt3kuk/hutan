//! Worktrees and session placement (protocol 0.14, capability `worktree`): DTOs of
//! `worktree.*`, the `worktree.changed` event and [`Placement`] on `SessionInfo`
//! (`docs/protocol.md` § Worktrees and placement, `docs/orchestration.md`).
//!
//! git is the source of truth: the daemon lists worktrees with `git worktree list` and only
//! remembers which ones it created itself.

use std::path::PathBuf;

use hutan_core::{SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// `worktree.create`: a checkout of `branch` next to the repository. Answered with
/// [`WorktreeCreated`].
///
/// `branch` is created from `base` when it does not exist, else it is checked out. Errors:
/// `bad_request` (not a valid branch name, no git repository, `path` not absolute), `conflict`
/// (the path exists, or the branch is checked out in another worktree; the message names
/// where), `not_found` (workspace), `invalid_state` with git's message when git refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCreate {
    pub workspace_id: WorkspaceId,
    /// A directory inside the repository; absent: the first root of the workspace that is a git
    /// repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    pub branch: String,
    /// The ref a new branch starts from; absent: `HEAD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Absolute and not existing; absent: `<parent of repo>/<repo name>-wt/<branch with / as ->`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// Result of `worktree.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCreated {
    pub worktree: WorktreeInfo,
}

/// `worktree.list`: the worktrees of the repositories of one workspace, or of every workspace.
/// Answered with [`WorktreeList`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorktreeListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// Result of `worktree.list`: per repository its main worktree first, then the others in
/// git's order; repositories in workspace order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeList {
    pub worktrees: Vec<WorktreeInfo>,
}

/// One worktree as git reports it, plus what Hutan knows about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    /// Canonical directory.
    pub path: PathBuf,
    /// The repository's main worktree (equal to `path` for the main worktree itself).
    pub repo_root: PathBuf,
    /// Absent for a detached HEAD (or a bare repository).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The commit checked out (full hash).
    pub head: String,
    /// The repository's main worktree, which cannot be removed.
    pub main: bool,
    /// Hutan made it (`worktree.create`).
    pub created_by_hutan: bool,
    /// Uncommitted changes (tracked, or untracked and not ignored).
    pub dirty: bool,
    /// Live sessions whose checkout is this worktree.
    pub live_sessions: Vec<SessionId>,
    /// `git worktree lock` was used.
    pub locked: bool,
    /// The directory is gone; `git worktree prune` would drop the entry.
    pub prunable: bool,
}

/// `worktree.remove`: removes the worktree directory, optionally its branch. Answered with
/// [`WorktreeRemoved`].
///
/// Errors: `invalid_state` (with the reason) for the main worktree, a worktree with live
/// sessions inside it, or uncommitted changes without `force`; `not_found` for a path git does
/// not list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRemove {
    pub path: PathBuf,
    /// Remove even with uncommitted changes (never with live sessions).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    /// Also delete the branch with `git branch -d` (never `-D`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delete_branch: bool,
}

/// Result of `worktree.remove`: `{}` unless a branch was asked to go and stayed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorktreeRemoved {
    /// `delete_branch` was set and git kept the branch (typically: it is not merged); git's
    /// reason. The worktree is removed anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_kept: Option<String>,
}

/// Where a session works: the git checkout containing its cwd (`SessionInfo.placement`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub kind: PlacementKind,
    /// The checkout's top level (canonical).
    pub root: PathBuf,
    /// Absent for a detached HEAD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Other live sessions with an agent in this checkout, while this session has one too:
    /// two agents writing to one checkout.
    #[serde(default)]
    pub shared_with: Vec<SessionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementKind {
    /// The repository's main checkout.
    Main,
    /// A linked worktree.
    Worktree,
}

// Review and merge (protocol 0.22, capability `worktree.review`) -------------------------------

fn yes() -> bool {
    true
}

fn is_yes(value: &bool) -> bool {
    *value
}

/// `worktree.diff`: what changed in the checkout at `path` (a worktree git lists, the main
/// checkout included) against the point where it left `base`. Answered with [`WorktreeDiff`].
///
/// The comparison is `git diff --merge-base <base>`: the commits since the checkout's branch
/// left `base`, plus (with `include_uncommitted`) staged, unstaged and untracked-but-not-ignored
/// changes. Read-only; never takes a lock. Errors: `bad_request` (`path` not absolute, a `base`
/// that is not a branch or commit), `not_found` (`path` is no worktree git lists),
/// `invalid_state` (no base given and the repository has no default branch; git's message when
/// git refuses).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeDiffRequest {
    pub path: PathBuf,
    /// A branch or commit; absent: the repository's default branch (`origin/HEAD`'s branch,
    /// else `main`, else `master`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Include changes not committed yet. Default `true`.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub include_uncommitted: bool,
}

/// Result of `worktree.diff`: the changed files, sorted by path.
///
/// Protocol 0.25 (capability `task.review`) says separately what a merge would take and what
/// it would not: [`commits`](Self::commits) are the commits on the branch since `merge_base`
/// (exactly what `worktree.merge` integrates), and every [`DiffFile`] says whether its change
/// is `committed`, `uncommitted`, or both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeDiff {
    /// The base that was used, as resolved (`main`, or what the request said).
    pub base: String,
    /// The commit the comparison starts from: the merge base of `base` and `HEAD`.
    pub merge_base: String,
    pub files: Vec<DiffFile>,
    /// Some patch is too large to be shown whole: a file over 2 MiB (its `worktree.file_diff`
    /// is cut), or the patches together over 20 MiB, or more files than the list holds (5000).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub patch_truncated: bool,
    /// The commits of the checkout's branch since `merge_base`, newest first (at most 200).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<CommitSummary>,
    /// More commits than `commits` holds.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub commits_truncated: bool,
}

/// One commit of [`WorktreeDiff::commits`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitSummary {
    /// The full hash.
    pub id: String,
    /// The first line of the message.
    pub subject: String,
}

/// One changed file of [`WorktreeDiff`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFile {
    /// Relative to the checkout's top level, `/`-separated (invalid UTF-8 is replaced).
    pub path: String,
    /// Where a renamed file came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: DiffStatus,
    /// Added and removed lines; `0` for a binary file.
    pub additions: u32,
    pub deletions: u32,
    /// git treats the file as binary: there are no line counts and no hunks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
    /// Protocol 0.25: the file changed in the commits since the merge base (a merge takes it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub committed: bool,
    /// Protocol 0.25: the file has staged, unstaged or untracked changes not in any commit (a
    /// merge does not take them). Only set when the request included uncommitted changes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub uncommitted: bool,
    /// Protocol 0.34 (capability `worktree.index`): the file has changes in git's index (what a
    /// commit would take now), against `HEAD`. Only set when the request included uncommitted
    /// changes. Read-only: Hutan never stages or unstages.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub staged: bool,
    /// Protocol 0.34 (capability `worktree.index`): the working tree differs from the index
    /// (changes not staged), or the file is untracked. A file can be both `staged` and
    /// `unstaged` (staged, then edited again).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unstaged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffStatus {
    /// New, including a file git does not track yet.
    Added,
    /// Changed content or mode, or a type change.
    Modified,
    Deleted,
    Renamed,
}

/// `worktree.file_diff`: the unified diff of one file of [`WorktreeDiff`], for the same `base`
/// and `include_uncommitted`. Answered with [`WorktreeFileDiff`]. Errors as `worktree.diff`,
/// plus `bad_request` for a `file` that is absolute or leaves the checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeFileDiffRequest {
    pub path: PathBuf,
    /// The file, as [`DiffFile::path`].
    pub file: String,
    /// A renamed file's [`DiffFile::old_path`]: without it the diff shows the file as new.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub include_uncommitted: bool,
}

/// Result of `worktree.file_diff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeFileDiff {
    /// A unified diff with git's headers (`diff --git`, `---`, `+++`, hunks); empty when the file
    /// did not change. For a binary file, git's `Binary files … differ` line.
    pub patch: String,
    /// The patch was cut at 2 MiB (on a line boundary).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
}

/// `worktree.merge`: brings the branch checked out at `path` into `into` in the repository's
/// **main checkout**, with a local commit; never pushes. Answered with [`WorktreeMerged`].
///
/// Refused, with nothing changed: `invalid_state` for the main checkout itself, a detached HEAD,
/// an unknown or not-checked-out target (the message names what the main checkout has), a dirty
/// main checkout, a merge or rebase already in progress there, a branch with nothing to merge,
/// and (`rebase`) uncommitted changes or live sessions in the worktree; `conflict` when the
/// branches conflict — the attempt is aborted and `data.conflicts` lists the files
/// ([`conflict_files`]); `bad_request` for a message with `rebase`, or names that are no
/// branches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeMerge {
    pub path: PathBuf,
    /// The branch to merge into; it must be the one checked out in the main checkout. Absent:
    /// the repository's default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into: Option<String>,
    #[serde(default)]
    pub strategy: MergeStrategy,
    /// The commit message (`merge`, `squash`); absent: git's own for a merge, `Squash merge
    /// branch '<branch>'` for a squash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// `git merge --no-ff`: a merge commit.
    #[default]
    Merge,
    /// `git merge --squash` and one commit.
    Squash,
    /// The branch is rebased onto the target (in its own worktree), then the target moves to it
    /// with `git merge --ff-only`: linear history, no new commit message.
    Rebase,
}

/// Result of `worktree.merge`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeMerged {
    /// The target's new tip (full hash).
    pub commit: String,
    pub target: String,
    /// The branch that was merged.
    pub branch: String,
    pub strategy: MergeStrategy,
    /// The worktree still has uncommitted changes, which are not part of the merge; removing it
    /// then needs `force`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_dirty: bool,
}

/// The files of a `worktree.merge` refused with `conflict` (`data.conflicts`); empty for any
/// other error.
pub fn conflict_files(error: &crate::control::ErrorBody) -> Vec<String> {
    error
        .data
        .as_ref()
        .and_then(|data| data.get("conflicts"))
        .and_then(|files| serde_json::from_value(files.clone()).ok())
        .unwrap_or_default()
}

/// `worktree.commit` (protocol 0.25, capability `task.review`): stages everything in the
/// worktree at `path` (`git add -A`: tracked changes and untracked files that are not ignored)
/// and commits it with `message`, so a following merge takes it. Hooks do not run. Answered
/// with [`WorktreeCommitted`].
///
/// Errors: `bad_request` (`path` not absolute, an empty message), `not_found` (no such
/// worktree), `invalid_state` for the main checkout (commit there yourself), a detached HEAD, a
/// merge or rebase in progress, nothing to commit, or git's own refusal (e.g. no author
/// identity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCommit {
    pub path: PathBuf,
    pub message: String,
}

/// Result of `worktree.commit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCommitted {
    /// The new commit (full hash).
    pub commit: String,
    /// The branch it is on.
    pub branch: String,
    /// Files the commit changed.
    pub files: u32,
}
