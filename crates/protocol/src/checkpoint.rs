//! Checkpoints of agent edits on the wire (protocol 0.37, capability `checkpoint`, ADR 0019):
//! `checkpoint.list`, `checkpoint.diff`, `checkpoint.remove`, the events `checkpoint.created`,
//! `checkpoint.removed` and `checkpoint.state`, and `ChatInfo.checkpoints`.
//!
//! The daemon takes a snapshot of an agent chat's git checkout when a turn starts and when it
//! ends ([`CheckpointInfo`]); these requests read them and remove them. Nothing here restores
//! files (that is CK-4's `checkpoint.preview` / `checkpoint.restore`, user-only).

use std::fmt;

use hutan_core::{CheckpointId, SessionId, WorkspaceId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use hutan_core::{
    CheckpointAvailability, CheckpointInfo, CheckpointKind, CheckpointState, CheckpointStatus,
    SkipReason, SkippedFile,
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
            other => other.parse().map(Self::Checkpoint).map_err(|_| {
                format!("`{other}` is neither `end`, `now` nor a checkpoint id")
            }),
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
}
