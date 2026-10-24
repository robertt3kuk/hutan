//! Read-only git history on the wire (protocol 0.46, capability `git.history`): `git.log`,
//! `git.blame`, `git.show` and `git.file` (`docs/protocol.md` § Git history).
//!
//! Every request names a place in a checkout by an absolute `path`: the checkout's top level, or
//! a file or folder inside it. The checkout must be one git lists for a repository of a
//! workspace (as for `worktree.diff`); the answers carry its top level as `root`, and every path
//! in an answer is relative to that, `/`-separated. Nothing here writes to the repository.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Most commits one `git.log` answers, whatever `limit` asks for.
pub const GIT_LOG_MAX_LIMIT: u32 = 200;
/// Longest commit subject in an answer, in characters.
pub const GIT_SUBJECT_MAX_CHARS: usize = 200;
/// Longest commit message `git.show` answers, in bytes.
pub const GIT_MESSAGE_MAX_BYTES: usize = 64 * 1024;
/// Most hunks one `git.blame` answers.
pub const GIT_BLAME_MAX_HUNKS: usize = 20_000;
/// A `git.show` patch (and so a file's part of it) is cut here, on a line boundary.
pub const GIT_SHOW_PATCH_CAP: usize = 2 * 1024 * 1024;
/// A `git.show` lists at most this many files.
pub const GIT_SHOW_MAX_FILES: usize = 5000;
/// `git.file` answers at most this many bytes of a file.
pub const GIT_FILE_MAX_BYTES: usize = 2 * 1024 * 1024;

fn is_false(value: &bool) -> bool {
    !*value
}

/// One commit of an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommit {
    /// The full hash.
    pub id: String,
    pub author: String,
    pub email: String,
    /// Author time, seconds since the Unix epoch.
    pub time: i64,
    /// The first line of the message, cut at [`GIT_SUBJECT_MAX_CHARS`] characters.
    pub subject: String,
    /// The commit has more than one parent.
    #[serde(default, skip_serializing_if = "is_false")]
    pub merge: bool,
    /// For the log of one file (git follows renames): the file's name at this commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// For the log of one file: the name it had before, when this commit renamed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
}

/// `git.log`: the commits that reach `rev` (default `HEAD`), newest first; limited to a file or
/// folder when `path` is not the checkout's top level (for a file git follows renames, and each
/// commit says the file's name then). Read-only, bounded, under a timeout; killed when the
/// connection closes. Answered with [`GitLogResult`].
///
/// Errors: `bad_request` (`path` not absolute, `limit` 0, a `rev` or `before` that is no commit
/// name), `not_found` (no such revision; `path` in no listed checkout; `before` not in the log),
/// `invalid_state` (`before` is farther back than the daemon reads: about 40 000 commits).
/// A repository with no commit yet answers an empty list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLog {
    pub path: PathBuf,
    /// A branch, tag or commit; absent: `HEAD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// At most [`GIT_LOG_MAX_LIMIT`] (clamped); at least 1.
    pub limit: u32,
    /// The page after this commit (a full hash, what an earlier answer's `next_before` said).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
}

/// Result of `git.log`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLogResult {
    /// The checkout's top level.
    pub root: PathBuf,
    /// Newest first.
    pub commits: Vec<GitCommit>,
    /// Older commits exist: ask again with `before = next_before`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub more: bool,
    /// The last commit of `commits`; present when `more`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
}

/// `git.blame`: which commit last changed each line of the file at `path`. Without `rev` the
/// file is the one in the working tree (lines not in `HEAD` belong to no commit: the hunk's
/// `commit` is [`GIT_UNCOMMITTED`]); with `rev`, that revision's. Whitespace changes count (no
/// `-w`), copies are not followed (`-M` is not used: a line moved inside the file is the commit
/// that moved it). Read-only, bounded, under a timeout, killed when the connection closes.
/// Answered with [`GitBlameResult`].
///
/// Errors: `bad_request` (`path` not absolute, or the checkout's top level), `not_found` (no
/// such revision or no listed checkout; the file is not tracked, or not in `rev`),
/// `invalid_state` (git refuses, e.g. the file is binary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBlame {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
}

/// The `commit` of a blame hunk (and the `id` of its [`GitBlameResult::commits`] entry) for lines
/// that are in no commit: edited on disk, or new.
pub const GIT_UNCOMMITTED: &str = "0000000000000000000000000000000000000000";

/// A run of consecutive lines last changed by one commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameHunk {
    /// The first line, counted from 1.
    pub line: u32,
    /// How many lines.
    pub count: u32,
    /// The commit's id (an entry of [`GitBlameResult::commits`]), or [`GIT_UNCOMMITTED`].
    pub commit: String,
}

/// One commit [`BlameHunk`]s refer to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameCommit {
    /// The full hash; [`GIT_UNCOMMITTED`] for lines in no commit (then `author` is what git
    /// calls them, `Not Committed Yet`, and `subject` is empty).
    pub id: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub subject: String,
    /// The file's name in that commit (differs from the current one after a rename).
    pub path: String,
    /// The commit is the start of what git shows (a root or `rev`'s boundary): its lines are
    /// older than the history.
    #[serde(default, skip_serializing_if = "is_false")]
    pub boundary: bool,
}

/// Result of `git.blame`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBlameResult {
    /// The checkout's top level.
    pub root: PathBuf,
    /// The file, relative to `root`.
    pub path: String,
    /// The commit `rev` named, as a full hash; absent for the working tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// Lines of the file git blamed.
    pub lines: u32,
    /// In line order, covering lines 1 to `lines` (or to the cut).
    pub hunks: Vec<BlameHunk>,
    /// Every commit the hunks name, once.
    pub commits: Vec<BlameCommit>,
    /// More than [`GIT_BLAME_MAX_HUNKS`] hunks: the later lines are not covered.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// `git.show`: one commit — its message and the files it changed with the unified diff against
/// its first parent (a root commit against nothing). `path` (when it is not the checkout's top
/// level) limits the files to that file or folder; `old_path` is the name a renamed file had
/// before (without it the rename shows as an addition). Without `patch` only the files and their
/// counts are answered. Bounded like `worktree.diff`: the patch is cut at
/// [`GIT_SHOW_PATCH_CAP`], the list at [`GIT_SHOW_MAX_FILES`]. Answered with [`GitShowResult`].
///
/// Errors: `bad_request` (`path` or `old_path` not absolute, a `rev` that is no commit name),
/// `not_found` (no such revision or checkout).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitShow {
    pub path: PathBuf,
    /// A commit, branch or tag.
    pub rev: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<PathBuf>,
    /// Answer the unified diff too. Default `true`.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub patch: bool,
}

fn yes() -> bool {
    true
}

fn is_yes(value: &bool) -> bool {
    *value
}

/// Result of `git.show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitShowResult {
    /// The checkout's top level.
    pub root: PathBuf,
    pub commit: GitCommit,
    /// The whole message (cut at [`GIT_MESSAGE_MAX_BYTES`]).
    pub message: String,
    /// The first parent, when the commit has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Sorted by path; `committed` is set on each (the change is in a commit).
    pub files: Vec<crate::worktree::DiffFile>,
    /// A unified diff with git's headers; empty without `patch`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub patch: String,
    /// The patch or the file list was cut.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// `git.file`: a file as it was at `rev` (read-only text, for opening an older revision).
///
/// Errors: `bad_request` (`path` not absolute or the checkout's top level, a bad `rev`),
/// `not_found` (no such revision, checkout, or no such file at `rev`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFile {
    pub path: PathBuf,
    pub rev: String,
}

/// Result of `git.file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFileResult {
    pub root: PathBuf,
    /// The file, relative to `root`.
    pub path: String,
    /// The commit `rev` named, as a full hash.
    pub rev: String,
    /// The file's size at `rev`, in bytes.
    pub size: u64,
    /// The text (invalid UTF-8 replaced); empty when `binary` or when it is over
    /// [`GIT_FILE_MAX_BYTES`].
    pub content: String,
    /// git treats the file as binary: `content` is empty.
    #[serde(default, skip_serializing_if = "is_false")]
    pub binary: bool,
    /// Larger than [`GIT_FILE_MAX_BYTES`]: `content` is empty.
    #[serde(default, skip_serializing_if = "is_false")]
    pub too_large: bool,
}
