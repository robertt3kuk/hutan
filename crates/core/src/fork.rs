//! Fork a chat from any turn (ADR 0023): how the cut is made, what a harness can do, and where a
//! fork came from. Pure data; the daemon decides and does.

use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, TurnId};

/// How a fork cuts the parent's conversation (the request's `mode`, and what a finished fork
/// records). Hutan never substitutes one for another: a harness that cannot make the cut asked
/// for refuses, and the user chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkMode {
    /// The conversation as it was after the chosen turn (the harness cuts natively, or the
    /// journal is copied).
    #[default]
    Exact,
    /// The conversation as it is now, whatever turn was named: the harness forks only at its
    /// end (OMP over ACP).
    Head,
    /// A fresh conversation seeded with a bounded digest of the turns up to the chosen one.
    /// Lossy, and said so.
    Summary,
}

impl ForkMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Head => "head",
            Self::Summary => "summary",
        }
    }
}

/// What a chat's agent can do for a fork, as it advertised (`ChatInfo.fork.support`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkSupport {
    /// Cuts at any turn: direct chats, Codex, Claude Code when its adapter takes a fork point.
    Exact,
    /// Forks at its current end only (OMP over ACP).
    Head,
    /// Cannot fork natively: only a summary seeds a new conversation.
    Summary,
    /// Cannot be forked at all (an assistant, a chat that has no conversation yet).
    None,
}

impl ForkSupport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Head => "head",
            Self::Summary => "summary",
            Self::None => "none",
        }
    }

    /// Whether a fork asked for with `mode` can be made. `at_head`: the turn asked for is the
    /// parent's newest.
    pub const fn allows(self, mode: ForkMode, at_head: bool) -> bool {
        match (self, mode) {
            (Self::None, _) => false,
            (_, ForkMode::Summary) => true,
            (Self::Summary, _) => false,
            (Self::Exact, _) => true,
            (Self::Head, ForkMode::Head) => true,
            (Self::Head, ForkMode::Exact) => at_head,
        }
    }
}

/// Where a chat was forked from: the parent, the turn the conversation was cut after, when, and
/// how. Stored with the chat (`ChatSpec.fork`) and shown as `SessionInfo.forked_from`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkOrigin {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    /// The turn's place among the parent's turns, counted from 1 (what "turns 1–N" says).
    #[serde(default)]
    pub ordinal: u32,
    pub mode: ForkMode,
    pub at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_support_level_allows() {
        use ForkMode::{Exact, Head, Summary};
        use ForkSupport as S;
        // Exact cuts anywhere; head only at the end; summary is always possible but for `none`.
        assert!(S::Exact.allows(Exact, false));
        assert!(S::Exact.allows(Head, true));
        assert!(S::Exact.allows(Summary, false));
        assert!(!S::Head.allows(Exact, false));
        assert!(S::Head.allows(Exact, true));
        assert!(S::Head.allows(Head, false));
        assert!(S::Head.allows(Summary, false));
        assert!(!S::Summary.allows(Exact, true));
        assert!(!S::Summary.allows(Head, true));
        assert!(S::Summary.allows(Summary, false));
        for mode in [Exact, Head, Summary] {
            assert!(!S::None.allows(mode, true));
        }
    }

    #[test]
    fn modes_and_support_use_their_wire_names() {
        assert_eq!(serde_json::to_string(&ForkMode::Head).unwrap(), "\"head\"");
        assert_eq!(
            serde_json::from_str::<ForkSupport>("\"none\"").unwrap(),
            ForkSupport::None
        );
        assert_eq!(ForkMode::default(), ForkMode::Exact);
    }
}
