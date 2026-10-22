//! Checkpoints of agent edits on the wire (protocol 0.37, capability `checkpoint`, ADR 0019):
//! `checkpoint.list`, `checkpoint.diff`, `checkpoint.remove`, the events `checkpoint.created`,
//! `checkpoint.removed` and `checkpoint.state`, and `ChatInfo.checkpoints`; and restoring
//! files to before turns (protocol 0.38, capability `checkpoint.restore`): `checkpoint.preview`
//! and `checkpoint.restore`, and `fs.changed` changes with `origin: {kind: "restore"}`.
//!
//! The daemon takes a snapshot of an agent chat's git checkout when a turn starts and when it
//! ends ([`CheckpointInfo`]); `list`, `diff` and `remove` read them and remove them. A preview
//! says file by file what a restore would do ([`RestoreEntry`]); a restore confirms the preview
//! by its digest, is the user's alone (an agent gets `forbidden`), and is itself a checkpoint
//! (kind `restore`), so restoring that one is Undo.

use std::fmt;
use std::path::PathBuf;

use hutan_core::{CheckpointId, SessionId, WorkspaceId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use hutan_core::{
    CheckpointAvailability, CheckpointInfo, CheckpointKind, CheckpointState, CheckpointStatus,
    RestoreAction, RestoreEntry, RestoreState, SkipReason, SkippedFile,
};

/// `checkpoint.list`'s largest page.
pub const LIST_MAX_LIMIT: u32 = 200;
/// `checkpoint.list`'s page when `limit` is absent.
pub const LIST_DEFAULT_LIMIT: u32 = 50;

/// One file's part of a `checkpoint.diff` patch is cut here (like `worktree.diff`).
pub const DIFF_FILE_PATCH_CAP: usize = 2 * 1024 * 1024;
/// A `checkpoint.diff` patch is not read beyond this.
pub const DIFF_TOTAL_PATCH_CAP: usize = 20 * 1024 * 1024;
/// A `checkpoint.diff` lists at most this many files.
pub const DIFF_MAX_FILES: usize = 5000;

/// `checkpoint.list`: the checkpoints of a session, of the sessions of a workspace (and of the
/// projects in it), or of all, newest first. Only checkpoints that hold something or say why
/// they do not are listed (`ok`, `late`, `failed`, `off`): a running turn, a turn that changed
/// nothing and a turn whose folder cannot have checkpoints are not.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CheckpointListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// At most [`LIST_MAX_LIMIT`]; [`LIST_DEFAULT_LIMIT`] when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// The page after this checkpoint: what an earlier answer's `next` said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<CheckpointId>,
}

/// The answer to `checkpoint.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointList {
    /// Newest first.
    pub checkpoints: Vec<CheckpointInfo>,
    /// More are older than the last one: ask again with `before: next`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<CheckpointId>,
}

/// What a checkpoint's start is compared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DiffAgainst {
    /// Its end: what the turn changed (`"end"`).
    #[default]
    End,
    /// The checkout as it is now (`"now"`), read through a fresh snapshot of Hutan's own index.
    Now,
    /// The end of another checkpoint of the same repository (its id): the turns from this one
    /// through that one.
    Checkpoint(CheckpointId),
}

impl DiffAgainst {
    pub const fn is_end(&self) -> bool {
        matches!(self, Self::End)
    }
}

impl fmt::Display for DiffAgainst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::End => f.write_str("end"),
            Self::Now => f.write_str("now"),
            Self::Checkpoint(id) => id.fmt(f),
        }
    }
}

impl std::str::FromStr for DiffAgainst {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "end" => Ok(Self::End),
            "now" => Ok(Self::Now),
            other => other
                .parse()
                .map(Self::Checkpoint)
                .map_err(|_| format!("`{other}` is neither `end`, `now` nor a checkpoint id")),
        }
    }
}

impl Serialize for DiffAgainst {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DiffAgainst {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// `checkpoint.diff`: what changed from a checkpoint's start to its end, to the checkout now, or
/// to the end of another checkpoint. Bounded like `worktree.diff`: 2 MiB of patch per file,
/// 20 MiB in all, 5 000 files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointDiffRequest {
    pub checkpoint_id: CheckpointId,
    #[serde(default, skip_serializing_if = "DiffAgainst::is_end")]
    pub against: DiffAgainst,
    /// Only this file or directory, relative to the checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The files only, no patch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stat: bool,
}

/// How a file differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointFileStatus {
    Added,
    Modified,
    Deleted,
}

/// One file of a `checkpoint.diff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointFile {
    /// Relative to the checkout.
    pub path: String,
    pub status: CheckpointFileStatus,
    /// Lines added and removed (a binary file counts none).
    pub added: u64,
    pub removed: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
    /// Its patch is cut (at 2 MiB, on a line boundary) or left out (past 20 MiB in all).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// The answer to `checkpoint.diff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointDiff {
    /// Sorted by path.
    pub files: Vec<CheckpointFile>,
    /// The unified diff (`a/` and `b/` prefixes); absent with `stat`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    /// Something is left out: a file's patch (see [`CheckpointFile::truncated`]) or files past
    /// the first 5 000.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// `checkpoint.remove`: one checkpoint, or every checkpoint of a session (exactly one of the
/// two). Its ref goes from the repository and its row from Hutan's database, in one batch.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CheckpointRemove {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<CheckpointId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// The answer to `checkpoint.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRemoved {
    /// The checkpoints that went.
    pub removed: Vec<CheckpointId>,
}

// Restore (protocol 0.38, capability `checkpoint.restore`) -------------------------------------

/// `checkpoint.preview`: what restoring the checkout to before a checkpoint's turn (through
/// `through`'s turn, every checkpoint of the session in between included) would do, file by
/// file. Nothing is written: the checkout now is read through a fresh snapshot of Hutan's own
/// index, never the user's. A read: an agent may ask for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointPreviewRequest {
    pub checkpoint_id: CheckpointId,
    /// A later checkpoint of the same session: rewind its turn and every one in between too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<CheckpointId>,
    /// Only these files or directories, relative to the checkout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

/// HEAD moved during the turns: a restore puts files back, never HEAD, branches or the stash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadMoved {
    /// The commit at the start of the first turn (absent: no commit yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The commit at the end of the last turn (absent: no commit, a branch without one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// The answer to `checkpoint.preview`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointPreview {
    /// What `checkpoint.restore` confirms: it changes with any file of the plan.
    pub plan_digest: String,
    /// Sorted by path. A file already as it was is left out; conflicts (`after`, `gap`,
    /// `during`) are `skip` until named in `accept_conflicts` (`accept` says what that does);
    /// `uncaptured` files, symbolic links and submodules are never written.
    pub entries: Vec<RestoreEntry>,
    pub session_id: SessionId,
    /// The chat's agent (`codex`, `omp`, `claude`), while the session is known: it still
    /// remembers the turns ([`restore_label`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The checkout's top level.
    pub checkout: PathBuf,
    /// The checkpoints rewound, oldest first (turns and earlier restores of the session).
    pub checkpoints: Vec<CheckpointId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_moved: Option<HeadMoved>,
    /// Other live sessions with an agent in the checkout: a restore never touches a file the
    /// turns did not change, and what they changed since shows as `after`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_with: Vec<SessionId>,
    /// Agent sessions with a turn running in the checkout: a restore is refused
    /// (`invalid_state`) until they end or are interrupted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub busy: Vec<SessionId>,
}

/// `checkpoint.restore`: the preview of the same `checkpoint_id`, `through` and `paths`,
/// confirmed by its `plan_digest`, plus the conflicts the user accepts. User-only: an
/// agent-originated connection gets `forbidden` whatever `[control]` says.
///
/// Before writing, the daemon takes a snapshot of the checkout (the restore's start) and plans
/// again from it: a different digest answers `conflict` with the new preview as `data`, and
/// nothing is written. Then clean files are written back (confined to the checkout, never
/// under `.git`, atomically, raw bytes, the mode of the tree entry) or deleted (with the
/// folders that deleting emptied), each announced in `fs.changed` with `origin: {kind:
/// "restore"}`, and a second snapshot ends the restore. `invalid_state` while an agent session
/// in the checkout has a turn running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRestore {
    pub checkpoint_id: CheckpointId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<CheckpointId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Conflicts to overwrite anyway, by path (each must be a conflict of the plan).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accept_conflicts: Vec<String>,
    /// The preview's `plan_digest`.
    pub plan_digest: String,
}

/// The answer to `checkpoint.restore`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRestored {
    /// The restore's own checkpoint (kind `restore`): its start is the checkout before, its end
    /// the checkout after. Restoring it is Undo.
    pub checkpoint: CheckpointInfo,
    /// Files written back, sorted.
    pub written: Vec<String>,
    /// Files deleted, sorted.
    pub deleted: Vec<String>,
    /// Files left alone with the reason: conflicts not accepted, uncaptured files, links, and
    /// files that changed or could not be written while the restore ran.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<RestoreEntry>,
}

/// The display name of a chat's agent, for the label.
fn harness_name(agent: &str) -> &str {
    match agent {
        "codex" => "Codex",
        "omp" => "OMP",
        "claude" => "Claude Code",
        other => other,
    }
}

/// The words every preview of a restore shows (ADR 0019 Decision 9): it restores files only,
/// the agent still remembers, and git history and what lies outside the checkout stay.
/// `checkout` names the checkout: its path (the CLI), or `this checkout` where the path is shown
/// elsewhere (the desktop's sheet, in a tooltip).
pub fn restore_label(
    agent: Option<&str>,
    checkout: impl fmt::Display,
    head_moved: Option<&HeadMoved>,
) -> String {
    let harness = agent.map_or("The agent", harness_name);
    let short = |commit: &Option<String>| {
        commit
            .as_deref()
            .map_or_else(|| "no commit".to_owned(), |c| c.chars().take(10).collect())
    };
    let git = match head_moved {
        Some(moved) => format!(
            "HEAD moved from {} to {} during the turns and stays at {}: git history (commits, \
             branches, stash) and anything outside {} are not rewound.",
            short(&moved.from),
            short(&moved.to),
            short(&moved.to),
            checkout
        ),
        None => format!(
            "Git history (commits, branches, stash) and anything outside {} are not rewound.",
            checkout
        ),
    };
    format!(
        "Restores files only. {harness} still remembers this turn and may mention or redo the \
         changes. {git}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn against_is_end_now_or_an_id() {
        let id = CheckpointId::new();
        for (value, text) in [
            (DiffAgainst::End, "end".to_owned()),
            (DiffAgainst::Now, "now".to_owned()),
            (DiffAgainst::Checkpoint(id), id.to_string()),
        ] {
            let json = serde_json::to_value(value).unwrap();
            assert_eq!(json, serde_json::Value::String(text));
            assert_eq!(serde_json::from_value::<DiffAgainst>(json).unwrap(), value);
        }
        assert!(serde_json::from_str::<DiffAgainst>("\"later\"").is_err());
        let request: CheckpointDiffRequest =
            serde_json::from_value(serde_json::json!({"checkpoint_id": id})).unwrap();
        assert_eq!(request.against, DiffAgainst::End);
    }

    #[test]
    fn the_label_says_files_only_who_remembers_and_what_stays() {
        let checkout = std::path::Path::new("/work/repo");
        assert_eq!(
            restore_label(Some("claude"), checkout.display(), None),
            "Restores files only. Claude Code still remembers this turn and may mention or redo \
             the changes. Git history (commits, branches, stash) and anything outside /work/repo \
             are not rewound."
        );
        let moved = HeadMoved {
            from: Some("0123456789abcdef".to_owned()),
            to: Some("fedcba9876543210".to_owned()),
        };
        let text = restore_label(None, "this checkout", Some(&moved));
        assert!(text.contains("The agent still remembers"), "{text}");
        assert!(
            text.contains("HEAD moved from 0123456789 to fedcba9876 during the turns"),
            "{text}"
        );
        assert!(
            text.ends_with("anything outside this checkout are not rewound."),
            "{text}"
        );
    }
}
