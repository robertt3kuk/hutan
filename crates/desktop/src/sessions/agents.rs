//! The window's side of the agent monitor: the artwork of the vocabulary `hutan-agents-view`
//! decides — the icon of a node, of a state and of a presentation, the badge's blocked marker —
//! and the event the store emits when a node becomes blocked. What each state *means* is the
//! crate's (GPUI-free, shared with the daemon); which icon says it is the theme's.

use hutan_agents_view::agents::AgentBadge;
use hutan_agents_view::attention::{Presentation, Tone};
use hutan_agents_view::presentation::State;
use hutan_core::{AgentNode, NodeOutcome, NodeState, SessionId};

use crate::icons::Icon;

/// Emitted by [`SessionsStore`](super::SessionsStore) when a node of a session becomes blocked.
/// M6 turns it into a macOS notification; until then it only moves the status bar's attention
/// count (the store folds blocked nodes into every session's [`Presentation`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAttentionChanged {
    pub session_id: SessionId,
    /// Every node of the session that is blocked now, by node id, in tree order.
    pub blocked_nodes: Vec<String>,
}

/// The state glyph of a node. State is never color alone: every state and outcome has its own
/// shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    pub icon: Icon,
    pub tone: Tone,
    /// Working nodes pulse (unless the user asked to reduce motion).
    pub animated: bool,
    /// What the glyph means, for tooltips and tests.
    pub meaning: &'static str,
}

/// The glyph of `node`: its outcome when it has finished, else its state.
pub fn glyph(node: &AgentNode) -> Glyph {
    let g = |icon, tone, animated, meaning| Glyph {
        icon,
        tone,
        animated,
        meaning,
    };
    match (node.outcome, node.state) {
        (Some(NodeOutcome::Completed), _) => {
            g(Icon::StateCompleted, Tone::Success, false, "completed")
        }
        (Some(NodeOutcome::Failed), _) => g(Icon::StateFailed, Tone::Error, false, "failed"),
        (Some(NodeOutcome::Interrupted), _) => {
            g(Icon::StateInterrupted, Tone::Muted, false, "interrupted")
        }
        (Some(NodeOutcome::Disconnected), _) => {
            g(Icon::StateDisconnected, Tone::Muted, false, "disconnected")
        }
        (None, NodeState::Working) => g(Icon::StateWorking, Tone::Accent, true, "working"),
        (None, NodeState::Blocked) => g(Icon::StateBlocked, Tone::Warning, false, "blocked"),
        (None, NodeState::Idle) => g(Icon::StateIdle, Tone::Muted, false, "idle"),
        (None, NodeState::Unknown) => {
            g(Icon::StateUnknown, Tone::Muted, false, "state unavailable")
        }
    }
}

/// The state's own shape: state is never color alone.
pub const fn state_icon(state: State) -> Icon {
    match state {
        State::Working => Icon::StateWorking,
        State::Blocked => Icon::StateBlocked,
        State::Idle => Icon::StateIdle,
        State::Unconfirmed => Icon::StateUnknown,
        State::Completed => Icon::StateCompleted,
        State::Failed => Icon::StateFailed,
        State::Stopped => Icon::StateStopped,
        State::Interrupted => Icon::StateInterrupted,
        State::Disconnected => Icon::StateDisconnected,
    }
}

/// The icon of a presentation. The state word remains the accessible meaning; artwork is
/// independently replaceable.
pub fn presentation_icon(presentation: &Presentation) -> Icon {
    match presentation.label.as_str() {
        "working" | "starting" | "running" => Icon::StateWorking,
        "blocked" => Icon::StateBlocked,
        "approval" => Icon::StateApproval,
        "question" => Icon::StateQuestion,
        "plan" => Icon::StatePlan,
        "done" | "completed" | "exited" => Icon::StateCompleted,
        "failed" => Icon::StateFailed,
        "stopped" => Icon::StateStopped,
        "interrupted" => Icon::StateInterrupted,
        "disconnected" => Icon::StateDisconnected,
        "review" => Icon::StateReview,
        "idle" => Icon::StateIdle,
        _ => Icon::StateUnknown,
    }
}

/// The blocked marker of a badge; `None` while nothing is blocked.
pub fn blocked_icon(badge: &AgentBadge) -> Option<Icon> {
    (!badge.blocked.is_empty()).then_some(Icon::StateBlocked)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use hutan_core::{Liveness, NodeKind};

    use super::*;

    fn node(id: &str) -> AgentNode {
        AgentNode {
            pid: None,
            node_id: id.into(),
            parent: None,
            kind: NodeKind::Main,
            source: "test".into(),
            agent: "omp".into(),
            name: Some(id.into()),
            agent_type: None,
            state: NodeState::Working,
            outcome: None,
            liveness: Liveness::Alive,
            model: None,
            started_at_ms: Some(1),
            ended_at_ms: None,
            last_activity_ms: 1,
            activity: None,
            usage: None,
            transcript_path: None,
        }
    }

    #[test]
    fn every_state_and_outcome_has_its_own_glyph() {
        let mut seen = HashSet::new();
        let mut n = node("a");
        for state in [
            NodeState::Working,
            NodeState::Blocked,
            NodeState::Idle,
            NodeState::Unknown,
        ] {
            n.state = state;
            assert!(seen.insert(glyph(&n).icon), "{state:?}");
        }
        for outcome in [
            NodeOutcome::Completed,
            NodeOutcome::Failed,
            NodeOutcome::Interrupted,
            NodeOutcome::Disconnected,
        ] {
            n.outcome = Some(outcome);
            assert!(seen.insert(glyph(&n).icon), "{outcome:?}");
        }
        n.outcome = Some(NodeOutcome::Completed);
        assert_eq!(
            (glyph(&n).icon, glyph(&n).tone),
            (Icon::StateCompleted, Tone::Success)
        );
        n.outcome = Some(NodeOutcome::Failed);
        assert_eq!(
            (glyph(&n).icon, glyph(&n).tone),
            (Icon::StateFailed, Tone::Error)
        );
        n.outcome = None;
        n.state = NodeState::Unknown;
        assert_eq!(
            (glyph(&n).icon, glyph(&n).tone),
            (Icon::StateUnknown, Tone::Muted)
        );
        n.state = NodeState::Blocked;
        assert_eq!(glyph(&n).tone, Tone::Warning);
        n.state = NodeState::Working;
        assert!(glyph(&n).animated);
    }

    #[test]
    fn a_badge_with_a_blocked_node_shows_the_blocked_marker() {
        let mut badge = AgentBadge::default();
        assert_eq!(blocked_icon(&badge), None);
        badge.blocked.push(hutan_agents_view::agents::BlockedNode {
            node_id: "a".into(),
            label: "reviewer".into(),
            main: false,
        });
        assert_eq!(blocked_icon(&badge), Some(Icon::StateBlocked));
    }
}
