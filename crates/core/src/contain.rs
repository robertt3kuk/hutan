//! Contain (protocol 0.52, capability `sandbox`, ADR 0021 decisions 3 and 8): Hutan's own
//! Seatbelt boundary around a session whose harness has no OS sandbox of its own.
//!
//! These are the two wire types: what a client asks for ([`Contain`], on `session.create`'s
//! terminal spec, `chat.create` and task runs) and what the daemon says it did
//! ([`Containment`], `SessionInfo.containment`). The daemon turns the request into a Seatbelt
//! profile (`hutan-daemon`'s `contain` module); nothing here does I/O.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A request to run a session inside Hutan's boundary. Everything is optional: the daemon
/// derives the writable folders from the session's checkout and fills the rest from its
/// `[sandbox]` settings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Contain {
    /// Whether the session may reach the network. Absent: the daemon's `[sandbox] network`
    /// (on unless the user turned it off). `false` also closes every unix socket, the
    /// daemon's own included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    /// Further absolute folders the session may write to, on top of its checkout, the
    /// repository's git directory, the temp folders and `[sandbox] extra_write`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_write: Vec<PathBuf>,
}

/// What enforces a [`Containment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainKind {
    /// macOS Seatbelt (`sandbox-exec`) around the session's process tree.
    Seatbelt,
    /// A container runtime (reserved for a later slice; no daemon reports it yet).
    Container,
}

impl ContainKind {
    /// What a person reads.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Seatbelt => "Seatbelt",
            Self::Container => "container",
        }
    }
}

/// The boundary a session **actually runs in** (`SessionInfo.containment`), read from what the
/// daemon applied to the spawn, never from what was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Containment {
    pub kind: ContainKind,
    /// Whether the session can reach the network.
    pub network: bool,
    /// The folders the session can write to, canonical and absolute.
    pub write_roots: Vec<PathBuf>,
    /// The process runs inside the boundary. `false`: containment was asked for and the
    /// session was not started uncontained; `reason` says why.
    pub applied: bool,
    /// Why the boundary is not applied, in a sentence. Absent when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The one honest line a client shows of a boundary (ADR 0021 decision 6): what it holds back
/// and what it leaves open. Reads are open and the network is a plain on or off, so neither is
/// ever hidden. The Start sheet shows it before the session exists, the chip afterwards.
pub fn summary_line(network: bool) -> String {
    format!(
        "writes limited to this worktree · can read your files · network {}",
        if network { "on" } else { "off" }
    )
}

impl Containment {
    /// [`summary_line`] of what was applied; a boundary that is not there says why.
    pub fn summary(&self) -> String {
        if !self.applied {
            return match &self.reason {
                Some(reason) => format!("not contained: {reason}"),
                None => "not contained".to_owned(),
            };
        }
        summary_line(self.network)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_request_is_an_empty_object() {
        assert_eq!(
            serde_json::to_value(Contain::default()).unwrap(),
            serde_json::json!({})
        );
        let off: Contain = serde_json::from_str(r#"{"network":false}"#).unwrap();
        assert_eq!(off.network, Some(false));
        assert!(off.extra_write.is_empty());
    }

    #[test]
    fn the_summary_names_the_boundary_and_its_holes() {
        let applied = Containment {
            kind: ContainKind::Seatbelt,
            network: false,
            write_roots: vec!["/repo".into()],
            applied: true,
            reason: None,
        };
        assert_eq!(
            applied.summary(),
            "writes limited to this worktree · can read your files · network off"
        );
        let on = Containment {
            network: true,
            ..applied.clone()
        };
        assert!(on.summary().ends_with("network on"));
        let refused = Containment {
            applied: false,
            reason: Some("sandbox-exec is missing".into()),
            ..applied
        };
        assert_eq!(refused.summary(), "not contained: sandbox-exec is missing");
    }
}
