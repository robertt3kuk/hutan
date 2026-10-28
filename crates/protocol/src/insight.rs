//! Agent insight (protocol 0.59, capability `agent.insight`): DTOs of `agent.insight`,
//! `agent.measure_context` and the `agent.insight_updated` event (`docs/protocol.md` § Agent
//! insight, `docs/agents.md` § Insight).
//!
//! The record itself, [`AgentInsight`](hutan_core::insight::AgentInsight), lives in `core`: what
//! a session's context holds, what each turn used, tools, files, time, and where each block
//! came from. It carries counts, tool names and file paths, never message text, tool output or
//! a diff.

use hutan_core::insight::AgentInsight;
use hutan_core::{SessionId, TurnId};
use serde::{Deserialize, Serialize};

/// `agent.insight`: the insight of a chat session; answered with [`AgentInsightResult`].
/// Read-only. `not_found` for an unknown session, or for a `node_id` the session has no such
/// agent for; a session nothing has reported on answers with `sources: []` and no blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInsightRequest {
    pub session_id: SessionId,
    /// An agent of the session's tree. Absent: the session's main agent. Only the main agent
    /// has insight of a chat (subagents report no usage of their own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInsightResult {
    pub insight: AgentInsight,
}

/// `agent.measure_context`: asks the harness to print its context by category (`/context` of
/// OMP and Claude Code) and reads the answer into the insight's breakdown. It is a turn of the
/// conversation (it appears in the journal as the message `/context` and its answer) that costs
/// no tokens, so it is only ever sent because a client asked. Answered with
/// [`AgentContextMeasured`] once the turn ended. Mutate (it sends a message to the agent).
///
/// Errors: `unsupported` for a harness without such a command (Codex, direct chats),
/// `invalid_state` while a turn runs or the session is not running, `not_found`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMeasureContext {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentContextMeasured {
    /// The turn that carried the command.
    pub turn_id: TurnId,
    /// The harness answered with a report that was read. `false`: it said something else (no
    /// model selected, a changed format) and the insight's breakdown is what it was before.
    pub measured: bool,
    /// The insight after the measurement.
    pub insight: AgentInsight,
}
