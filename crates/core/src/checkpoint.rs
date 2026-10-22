//! Checkpoints of agent edits (ADR 0019): what the daemon records about a chat turn's files.
//!
//! The daemon takes a snapshot of the session's git checkout when a turn starts and when it
//! ends, keeps the end in one hidden ref per turn and describes it with a [`CheckpointInfo`].
//! A turn that changed nothing, or whose checkout cannot have checkpoints, is still described:
//! its [`CheckpointState`] says why there is nothing to restore.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{CheckpointId, SessionId, TurnId};

/// What a checkpoint is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    /// An agent turn: its start and its end.
    Turn,
    /// A restore of earlier checkpoints: the files before and after it (CK-4).
    Restore,
}

impl CheckpointKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Restore => "restore",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "turn" => Some(Self::Turn),
            "restore" => Some(Self::Restore),
            _ => None,
        }
    }
}

/// How a checkpoint turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointState {
    /// The turn is running: its start is recorded (or being recorded), its end is not.
    Running,
    /// Start and end recorded; the end is in a ref.
    Ok,
    /// The start took longer than its budget, so the prompt went to the agent before it was
    /// recorded: the start may already hold some of the turn's edits, and restoring the turn
    /// is refused.
    Late,
    /// The turn changed no file: nothing to restore, and no ref.
    Unchanged,
    /// A snapshot failed (`reason` says how).
    Failed,
    /// Checkpoints were off for this turn: switched off by failures or low disk, or turned
    /// off in the settings while it ran (`reason` says which).
    Off,
    /// The session's folder cannot have checkpoints: not in a git repository, the home
    /// directory, too many untracked files, no git (`reason` says which).
    Unsupported,
}

impl CheckpointState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Late => "late",
            Self::Unchanged => "unchanged",
            Self::Failed => "failed",
            Self::Off => "off",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Running,
            Self::Ok,
            Self::Late,
            Self::Unchanged,
            Self::Failed,
            Self::Off,
            Self::Unsupported,
        ]
        .into_iter()
        .find(|state| state.as_str() == text)
    }
}

/// Why a file is not in a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SkipReason {
    /// Larger than `max_file_bytes`.
    TooLarge,
    /// Past the snapshot's `snapshot_budget_bytes`.
    OverBudget,
    /// Past the snapshot's `max_files`.
    TooManyFiles,
    /// The name looks like a secret (the `exclude` list).
    SecretName,
    /// A `filter=` attribute (Git LFS and the like): its content would not round-trip as raw
    /// bytes.
    Filter { driver: String },
    /// A nested repository or a submodule: neither the gitlink nor its content is captured.
    NestedRepository,
    /// Not a regular file or a symbolic link (a FIFO, socket or device).
    NotRegular,
    /// The daemon may not read it.
    Unreadable,
}

/// A file left out of a checkpoint, relative to the checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedFile {
    pub path: String,
    #[serde(flatten)]
    pub reason: SkipReason,
    /// Its size, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// One checkpoint as the daemon describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub id: CheckpointId,
    pub session_id: SessionId,
    /// The turn; `None` until the agent started it (or when it never did).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub kind: CheckpointKind,
    pub state: CheckpointState,
    /// Why the state is what it is (`failed`, `off`, `unsupported`, `late`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_ms: u64,
    /// The checkout's top level (the session's folder when it is not in git).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<PathBuf>,
    /// Files that differ between the start and the end.
    pub files_changed: u64,
    /// Lines added and removed over those files (binary files count none).
    pub added: u64,
    pub removed: u64,
    /// New content the checkpoint wrote to the repository's object store.
    pub bytes: u64,
    /// Files left out, with the reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SkippedFile>,
    /// A `restore` checkpoint: the checkpoint it restored to before (a turn's, or an earlier
    /// restore's when it was an Undo), from its stored row (protocol 0.40). Absent for a turn
    /// and for a restore recorded before 0.40.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restores: Option<CheckpointId>,
    /// A restore of a chain of turns: the last checkpoint of the chain (`through`), absent
    /// when it restored one checkpoint (protocol 0.40).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restores_through: Option<CheckpointId>,
}

/// Whether a chat session's turns get checkpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointAvailability {
    On,
    /// Turned off in the settings, or switched off by failures or low disk.
    Off,
    /// The session's folder cannot have checkpoints.
    Unsupported,
}

/// A chat session's checkpoints right now, and why when they are not on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointStatus {
    pub state: CheckpointAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CheckpointStatus {
    pub const fn on() -> Self {
        Self {
            state: CheckpointAvailability::On,
            reason: None,
        }
    }

    pub fn off(reason: impl Into<String>) -> Self {
        Self {
            state: CheckpointAvailability::Off,
            reason: Some(reason.into()),
        }
    }

    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            state: CheckpointAvailability::Unsupported,
            reason: Some(reason.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_files_carry_their_reason_inline() {
        let lfs = SkippedFile {
            path: "big.psd".to_owned(),
            reason: SkipReason::Filter {
                driver: "lfs".to_owned(),
            },
            bytes: Some(7),
        };
        let json = serde_json::to_value(&lfs).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"path": "big.psd", "reason": "filter", "driver": "lfs", "bytes": 7})
        );
        assert_eq!(serde_json::from_value::<SkippedFile>(json).unwrap(), lfs);
        let large = SkippedFile {
            path: "a.bin".to_owned(),
            reason: SkipReason::TooLarge,
            bytes: None,
        };
        let json = serde_json::to_string(&large).unwrap();
        assert_eq!(json, r#"{"path":"a.bin","reason":"too_large"}"#);
        assert_eq!(serde_json::from_str::<SkippedFile>(&json).unwrap(), large);
    }

    #[test]
    fn states_and_kinds_parse_what_they_print() {
        for state in [
            CheckpointState::Running,
            CheckpointState::Ok,
            CheckpointState::Late,
            CheckpointState::Unchanged,
            CheckpointState::Failed,
            CheckpointState::Off,
            CheckpointState::Unsupported,
        ] {
            assert_eq!(CheckpointState::parse(state.as_str()), Some(state));
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::json!(state.as_str())
            );
        }
        for kind in [CheckpointKind::Turn, CheckpointKind::Restore] {
            assert_eq!(CheckpointKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(CheckpointState::parse("done"), None);
    }
}
