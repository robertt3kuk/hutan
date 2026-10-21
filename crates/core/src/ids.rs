//! Identifiers shared by the daemon and every client.
//!
//! Durable entities use ULIDs (sortable, stable across daemon restarts). Terminal IDs are `u64`
//! because they appear in binary frame headers (ADR 0002).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, thiserror::Error)]
#[error("invalid {kind} id `{input}`")]
pub struct ParseIdError {
    kind: &'static str,
    input: String,
}

macro_rules! ulid_id {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::generate())
            }

            pub const fn from_ulid(ulid: Ulid) -> Self {
                Self(ulid)
            }

            pub const fn as_ulid(&self) -> Ulid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_string(s).map(Self).map_err(|_| ParseIdError {
                    kind: $kind,
                    input: s.to_owned(),
                })
            }
        }
    };
}

ulid_id!(
    /// A workspace groups sessions, repositories, and tasks.
    WorkspaceId,
    "workspace"
);
ulid_id!(
    /// A session is one terminal, harness chat, or direct model chat.
    SessionId,
    "session"
);
ulid_id!(
    /// Identifies one daemon run. A new epoch means every live process may have restarted.
    Epoch,
    "epoch"
);
ulid_id!(
    /// One user-message-to-completion cycle of an agent session (local id; the provider has its own).
    TurnId,
    "turn"
);
ulid_id!(
    /// One message, reasoning block, tool call, or file change of an agent turn (local id).
    ItemId,
    "item"
);
ulid_id!(
    /// One permission request an agent session made; stable across clients and restarts.
    ApprovalId,
    "approval"
);
ulid_id!(
    /// One question an agent put to the user (protocol 0.24); stable across clients.
    QuestionId,
    "question"
);
ulid_id!(
    /// A task: a unit of work with dependencies, run as one or more sessions.
    TaskId,
    "task"
);
ulid_id!(
    /// One checkpoint of a chat session's files (ADR 0019): a turn's start and end, or a restore.
    CheckpointId,
    "checkpoint"
);

/// Daemon-assigned terminal handle, unique within an epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TerminalId(pub u64);

impl fmt::Display for TerminalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for TerminalId {
    type Err = ParseIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self).map_err(|_| ParseIdError {
            kind: "terminal",
            input: s.to_owned(),
        })
    }
}

/// Position in the daemon-wide event log of one epoch. Strictly increasing.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Seq(pub u64);

impl Seq {
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_ids_roundtrip_through_display_and_json() {
        let id = SessionId::new();
        assert_eq!(id.to_string().parse::<SessionId>().unwrap(), id);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<SessionId>(&json).unwrap(), id);
    }

    #[test]
    fn malformed_ids_are_rejected_with_kind() {
        let err = "not-a-ulid".parse::<WorkspaceId>().unwrap_err();
        assert_eq!(err.to_string(), "invalid workspace id `not-a-ulid`");
        assert!("12x".parse::<TerminalId>().is_err());
    }
}
