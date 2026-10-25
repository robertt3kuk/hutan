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
    /// Protocol 0.49 (capability `worktree.setup`): do not copy files or run the setup of
    /// `.hutan/worktree.toml` (`hutan wt create --no-setup`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_setup: bool,
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
    /// Protocol 0.49 (capability `worktree.setup`): where the setup of this worktree stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<WorktreeSetup>,
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
/// With `staged_only` (protocol 0.53, capability `worktree.stage`, **user-only**) nothing is
/// staged: the commit takes exactly what git's index holds, and the main checkout may be
/// committed too (as Finish does); the rest stays where it is.
///
/// Errors: `bad_request` (`path` not absolute, an empty message), `not_found` (no such
/// worktree), `invalid_state` for the main checkout (commit there yourself; not with
/// `staged_only`), a detached HEAD, a merge or rebase in progress, nothing to commit (nothing
/// staged, with `staged_only`), or git's own refusal (e.g. no author identity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCommit {
    pub path: PathBuf,
    pub message: String,
    /// Commit only what is staged; default `false`: everything, as before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub staged_only: bool,
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

// Staging (protocol 0.53, capability `worktree.stage`, ADR 0020 amendment) -------------------

/// `worktree.hunks`: the hunks of the files of a checkout that can be staged, unstaged or
/// discarded, with their ids. Answered with [`WorktreeHunks`]. Read-only.
///
/// Hunks come from `git diff` (the working tree against the index: **unstaged**) and
/// `git diff --cached` (the index against `HEAD`: **staged**), both without rename detection, so
/// a renamed file appears as its two paths. An untracked file has one unstaged hunk (all of it).
/// A hunk's id is a digest of its side, path, position and text: it is the same for as long as
/// that side of the file is unchanged, and a request naming an id that no longer exists answers
/// `conflict`. Errors as `worktree.diff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeHunksRequest {
    /// The checkout (a worktree git lists, the main checkout included).
    pub path: PathBuf,
    /// Only these files, relative to the checkout; empty: every changed file (at most 5000).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// Result of `worktree.hunks`: sorted by path; files without hunks are left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeHunks {
    pub files: Vec<FileHunks>,
    /// Some file was left out or cut: more than 5000 files, or a file's diff over 2 MiB.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// The hunks of one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHunks {
    /// Relative to the checkout's top level, `/`-separated.
    pub path: String,
    /// git treats the file as binary: it can be staged, unstaged or discarded whole, never by
    /// hunk (`bad_request`), and it lists no hunks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
    /// Working tree against the index (what *Stage hunk* and *Discard hunk* act on).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unstaged: Vec<DiffHunk>,
    /// Index against `HEAD` (what *Unstage hunk* acts on).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub staged: Vec<DiffHunk>,
}

/// One hunk of a unified diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    /// 16 hex digits; see [`WorktreeHunksRequest`].
    pub id: String,
    /// The `@@ -a,b +c,d @@ …` line.
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// The hunk's lines after the header, each with its ` `, `+`, `-` or `\` prefix, joined by
    /// `\n` (invalid UTF-8 is replaced). A line's number in this list (from 0) is what
    /// [`HunkRef::lines`] counts.
    pub body: String,
}

/// A hunk named by a request, as `worktree.hunks` listed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkRef {
    /// The file, as [`FileHunks::path`].
    pub path: String,
    /// [`DiffHunk::id`].
    pub hunk_id: String,
    /// Only these changed lines of the hunk (their numbers in [`DiffHunk::body`], from 0; each
    /// must be a `+` or `-` line); empty: the whole hunk. The others stay as they are.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<u32>,
}

/// `worktree.stage` and `worktree.unstage` (**user-only**: an agent-originated connection gets
/// `forbidden` whatever `[control]` says). Answered with [`WorktreeIndexed`].
///
/// `stage` copies working-tree state into git's index — whole files (`paths`: `git add -A`, so a
/// deleted file is staged as deleted and an untracked one is added; a renamed file needs both of
/// its paths) or hunks (`hunks`: the unstaged hunk applied to the index with `git apply
/// --cached`). `unstage` restores index entries from `HEAD` (`paths`) or applies a staged hunk
/// in reverse to the index (`hunks`). **The working tree is never touched**, and no hook runs.
/// `all` is every path of the checkout (`git add -A` / `git reset`).
///
/// Everything is checked before the first change: the request is applied whole or not at all.
/// Errors: `bad_request` (`checkout` not absolute; nothing named; a path that leaves the
/// checkout; a hunk of a binary file; a line that is not a changed line), `not_found` (no such
/// checkout or file), `conflict` (a hunk id that no longer exists, a patch that no longer
/// applies), `invalid_state` (a merge or rebase in progress, git's refusal).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorktreeIndexChange {
    pub checkout: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hunks: Vec<HunkRef>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all: bool,
}

/// Result of `worktree.stage` and `worktree.unstage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorktreeIndexed {
    /// Whole files named (`all`: `0`).
    pub paths: u32,
    /// Hunks applied.
    pub hunks: u32,
}

/// `worktree.discard` (**user-only**): throws working-tree changes away, for good unless the
/// checkpoint it takes first is restored (Undo). Answered with [`WorktreeDiscarded`].
///
/// `paths` make a file's working tree equal to the index (an untracked file is deleted); with
/// `staged` they make file and index equal to `HEAD` (a file new in the index is deleted).
/// `hunks` are unstaged hunks, applied in reverse to the working tree. The index is not touched
/// by hunks, nor by paths without `staged`.
///
/// Before anything is written the daemon takes a checkpoint of the checkout (kind `restore`,
/// the one `checkpoint.restore` calls Undo when restored) and a second one afterwards; the
/// changes are announced in `fs.changed` with `origin: {kind: "restore"}`. Refused with
/// `invalid_state`, nothing changed: an agent session in the checkout has a turn running, or
/// checkpoints are off or unsupported there (Undo would not exist). Other errors as
/// [`WorktreeIndexChange`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorktreeDiscard {
    pub checkout: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hunks: Vec<HunkRef>,
    /// Also discard what is staged for `paths` (back to `HEAD`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub staged: bool,
}

/// Result of `worktree.discard`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeDiscarded {
    pub paths: u32,
    pub hunks: u32,
    /// The discard's checkpoint (kind `restore`): `checkpoint.preview` and `checkpoint.restore`
    /// with its id put everything back.
    pub checkpoint: hutan_core::CheckpointInfo,
}

/// `worktree.finish` (protocol 0.35, capability `finish`, ADR 0020): the user's **Finish** of a
/// checkout, in up to three steps, each reported in [`WorktreeFinished`]:
///
/// 1. **commit** — everything uncommitted in `checkout` (`git add -A`, as `worktree.commit`;
///    hooks do not run), on its branch. A main checkout is committed in place.
/// 2. **merge** (a worktree only) — its branch into `into` in the main checkout, as
///    `worktree.merge` with `strategy`.
/// 3. **push** — the branch that now holds the work (the main checkout's branch, or the merge
///    target) to its upstream: `git push <remote> refs/heads/<branch>:<upstream>`, with the
///    user's own credential helper and SSH agent from the login environment. Never forced, never
///    rebased: a read-only `git fetch` of the upstream first says how far the branch is ahead and
///    behind, and a branch behind its upstream is not pushed (`rejected`).
///
/// A step that fails leaves what the steps before it did (a failed push keeps the commit) and
/// the steps after it are `skipped` (`earlier_step_failed`). **User-only** (ADR 0016): an
/// agent-originated connection gets `forbidden` whatever `[control]` says.
///
/// Refused before anything changes: `bad_request` (`checkout` not absolute; an empty message
/// while there is something to commit; an `into` that is no valid branch name), `not_found` (no
/// such checkout, or no branch `into`), `invalid_state` (a detached HEAD, a merge or rebase in
/// progress, and for a worktree the refusals of `worktree.merge` that do not depend on the
/// commit: the target not checked out in the main checkout, a dirty main checkout, an operation
/// in progress there, and for `rebase` live sessions in the worktree).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeFinish {
    /// The checkout to finish: a worktree, or a repository's main checkout.
    pub checkout: PathBuf,
    /// The message of the commit step (and of a `squash` merge's commit). May be empty when
    /// there is nothing to commit.
    #[serde(default)]
    pub message: String,
    /// Push after committing (and merging). Absent: `[finish] push` of `daemon.toml` (default
    /// `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<bool>,
    /// A worktree's merge target; absent: the repository's default branch. Ignored for a main
    /// checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into: Option<String>,
    /// How a worktree's branch is merged. Ignored for a main checkout.
    #[serde(default)]
    pub strategy: MergeStrategy,
    /// Protocol 0.53 (capability `worktree.stage`): the commit step takes only what is staged
    /// (as `worktree.commit` with `staged_only`); the rest stays uncommitted in the checkout.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub staged_only: bool,
}

/// Result of `worktree.finish`: one entry per step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeFinished {
    pub commit: FinishCommit,
    /// Absent for a main checkout (there is nothing to merge it into).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge: Option<FinishMerge>,
    /// Absent when no push was asked for (`push: false`, or `[finish] push = false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<FinishPush>,
}

impl WorktreeFinished {
    /// Whether some step failed.
    pub fn failed(&self) -> bool {
        self.commit.status == StepStatus::Failed
            || self
                .merge
                .as_ref()
                .is_some_and(|m| m.status == StepStatus::Failed)
            || self
                .push
                .as_ref()
                .is_some_and(|p| p.status == StepStatus::Failed)
    }
}

/// How a step of `worktree.finish` went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Ok,
    /// Not run; [`StepReason`] says why.
    Skipped,
    /// Run and failed; [`StepReason`] and the message say why.
    Failed,
}

/// Why a step of `worktree.finish` was skipped or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepReason {
    /// Skipped commit: the checkout had no uncommitted change.
    NothingToCommit,
    /// Skipped merge: the target already contains the branch.
    NothingToMerge,
    /// Skipped push: the upstream already has every commit of the branch.
    UpToDate,
    /// Skipped: a step before it failed.
    EarlierStepFailed,
    /// Failed merge: the branches conflict; the merge was aborted ([`FinishMerge::conflicts`]).
    Conflict,
    /// Failed push: the branch has no upstream (or tracks a local branch). Skipped push: the
    /// repository has no remote at all, so the Finish is commit-only (protocol 0.43).
    NoUpstream,
    /// Failed push: not a fast-forward — the upstream has commits the branch lacks (seen by the
    /// fetch before pushing, [`FinishPush::behind`]), or the remote refused the update (a hook,
    /// a protected branch). Hutan never forces.
    Rejected,
    /// Failed push (or its fetch): git could not authenticate (no credential, a refused key, no
    /// terminal to ask on).
    Auth,
    /// Failed otherwise (git's own refusal, the network, a timeout); the message has git's words.
    Error,
}

/// The commit step of [`WorktreeFinished`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishCommit {
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StepReason>,
    /// Why the step was skipped or failed, in words (git's own for a failure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The branch committed on.
    pub branch: String,
    /// The new commit (full hash), when one was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Files the commit changed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub files: u32,
}

/// The merge step of [`WorktreeFinished`] (a worktree only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishMerge {
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StepReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The worktree's branch.
    pub branch: String,
    /// The branch it was merged into.
    pub target: String,
    pub strategy: MergeStrategy,
    /// The target's new tip (full hash), when the merge was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The files of a `conflict`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
    /// As [`WorktreeMerged::source_dirty`]: removing the worktree afterwards needs `force`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_dirty: bool,
}

/// The push step of [`WorktreeFinished`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishPush {
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<StepReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The local branch pushed (or that would have been).
    pub branch: String,
    /// The upstream as git names it (`origin/main`), when the branch has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// The commit the upstream has now (full hash), after a push that went through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Commits of the branch the upstream did not have, as the fetch before pushing saw it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u32>,
    /// Commits of the upstream the branch does not have; more than zero is not pushed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

// Setup, teardown and retention (protocol 0.49, capability `worktree.setup`) ------------------

/// Where a worktree's setup stands (`WorktreeInfo.setup`). Setup is what the repository's
/// `.hutan/worktree.toml` (or the user's `[worktrees.projects.<name>]`) asks for after
/// `worktree.create`: copy gitignored files, then run commands in a terminal session named
/// *Setup* (`docs/orchestration.md` § Setup and cleanup). The state is the daemon's memory of
/// this run: it is not kept across a daemon restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeSetupState {
    /// The files were copied and the *Setup* session is running its commands.
    Running,
    /// The commands come from the repository and the user has not confirmed exactly these
    /// yet (never, or the file changed): nothing ran. `worktree.setup_trust` with
    /// [`WorktreeSetup::hash`] confirms them and runs the setup.
    NeedsTrust,
    /// A command failed, or the session was stopped; [`WorktreeSetup::message`] says how and
    /// the session (still listed) has the output.
    Failed,
    /// Everything ran and succeeded.
    Done,
}

/// What `worktree.list` and `worktree.create` say about the setup of one worktree; absent when
/// no setup was asked for (no config, or `no_setup`) or the daemon has not run one since it
/// started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSetup {
    pub state: WorktreeSetupState,
    /// The *Setup* terminal session: running, or ended (`failed`, `done`) with its output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// The commands of `setup`, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    /// The commands of `teardown`, in order (they run before the worktree is removed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teardown: Vec<String>,
    /// For `needs_trust`: the content hash of the repository's commands (sha-256, hex) that
    /// `worktree.setup_trust` must repeat, so the user confirms what they were shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// How many gitignored files were copied from the main checkout.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub copied: u32,
    /// Why the setup failed, or what was left out (copy limits, unknown keys of the config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `worktree.setup`: runs the setup of an existing worktree again (copies what is missing, runs
/// the commands in a new *Setup* session). A running setup is `invalid_state`. Answered with
/// [`WorktreeSetupResult`]. Errors: `bad_request` (`path` not absolute), `not_found` (no such
/// worktree), `invalid_state` (the main worktree; a running setup; nothing configured).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSetupRequest {
    pub path: PathBuf,
}

/// `worktree.setup_trust`: the user confirms the repository's setup and teardown commands of the
/// worktree at `path` (and, with them, of every worktree of the repository while the commands
/// stay the same), then the setup runs. `hash` must equal [`WorktreeSetup::hash`] now (the
/// config may have changed since it was shown: `conflict`). The decision is remembered by the
/// content hash; editing the commands asks again. **User-only**: an agent gets `forbidden`.
/// Answered with [`WorktreeSetupResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSetupTrust {
    pub path: PathBuf,
    pub hash: String,
}

/// Result of `worktree.setup` and `worktree.setup_trust`: the worktree as it is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSetupResult {
    pub worktree: WorktreeInfo,
}

/// `[worktrees] cleanup`: which worktrees `worktree.cleanup` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCleanup {
    /// None (the default).
    #[default]
    Never,
    /// Worktrees whose branch got commits of its own and is merged into the default branch.
    Merged,
    /// Worktrees nothing has touched for `idle_days`.
    Idle,
}

impl WorktreeCleanup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Merged => "merged",
            Self::Idle => "idle",
        }
    }
}

/// `worktree.cleanup`: the worktrees that retention would remove now. Read-only. Answered with
/// [`WorktreeCleanupList`].
///
/// Only worktrees Hutan created qualify, and only when no session is live in them, nothing in
/// them is uncommitted (untracked files count), they are not locked, and no setup is running.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCleanupRequest {
    /// Only the repositories of this workspace; absent: every workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// Absent: `[worktrees] cleanup`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<WorktreeCleanup>,
    /// Absent: `[worktrees] cleanup_idle_days`. Only for `idle`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_days: Option<u64>,
}

/// Why a worktree is listed by `worktree.cleanup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupReason {
    /// Its branch is merged into the repository's default branch.
    Merged,
    /// Nothing touched it for [`WorktreeCleanupList::idle_days`] days.
    Idle,
}

/// One worktree of [`WorktreeCleanupList`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupCandidate {
    pub path: PathBuf,
    pub repo_root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub reason: CleanupReason,
    /// Whole days since anything touched it (the later of its newest commit and the last change
    /// of its git state).
    pub idle_days: u64,
    /// Removing it runs these teardown commands first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teardown: Vec<String>,
}

/// Result of `worktree.cleanup`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCleanupList {
    /// The policy that was applied (the request's, else the setting).
    pub mode: WorktreeCleanup,
    /// The idle limit that was applied.
    pub idle_days: u64,
    pub candidates: Vec<CleanupCandidate>,
}

/// `worktree.prune`: removes the worktrees `worktree.cleanup` lists with the same `workspace_id`,
/// `mode` and `idle_days` (looked at again now), or only those among them whose path is in
/// `paths`. Each one's teardown runs first; one that fails stays and is reported. A merged
/// branch is deleted with `git branch -d`. **User-only**: nothing is removed by an agent.
/// Answered with [`WorktreePruned`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreePrune {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<WorktreeCleanup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_days: Option<u64>,
    /// Empty: every candidate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PathBuf>,
}

/// Result of `worktree.prune`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreePruned {
    pub removed: Vec<PathBuf>,
    pub failed: Vec<PruneFailure>,
}

/// A worktree `worktree.prune` kept, and why (a failing teardown, git's refusal, a worktree
/// that stopped being a candidate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneFailure {
    pub path: PathBuf,
    pub reason: String,
}
