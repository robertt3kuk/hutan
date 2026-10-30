//! The agents board (protocol 0.63, capability `agents.board`, ADR 0025): DTOs of `agents.board`,
//! one bounded, read-only, redacted answer to "what are my agents doing and who waits for
//! whom", across workspaces (`docs/protocol.md` § Agents board).
//!
//! The daemon builds it with the same reducer as the desktop's Agents page (`hutan-agents-view`),
//! so a model, the CLI and the window cannot disagree. Every row says where its facts come from
//! ([`BoardSource`]): a chat's journal is exact, a terminal agent's hooks and OSC 7501 only say
//! its current state. Nothing here is a count of something that was not recorded: a missing
//! model, time or counter stays absent, and an agent whose state is not known is
//! [`BoardState::Unconfirmed`], never idle.

use hutan_core::{SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// The most agents one board lists.
pub const BOARD_MAX_AGENTS: usize = 200;
/// The most bytes of JSON one board takes (R8 of the supervisor spike): agents are added in the
/// board's order until the next would not fit.
pub const BOARD_MAX_BYTES: usize = 24 * 1024;
/// The most characters of any text of the board; longer text ends in `…`.
pub const BOARD_TEXT_CHARS: usize = 200;

/// `agents.board`: the agents of every workspace, or of one. Read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentsBoardRequest {
    /// Only this workspace, with the projects in it; a project; or Home. Absent: every
    /// workspace. `not_found` for an unknown id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// List agents that have finished too (they are always counted). Default: not listed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_finished: bool,
}

/// The board. `counts` and `coverage` are over **every** agent in scope; `agents` is the part
/// of them that is listed (see `hidden_finished` and `omitted`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsBoard {
    pub generated_at_ms: u64,
    pub counts: BoardCounts,
    pub coverage: BoardCoverage,
    /// In the board's order: *Requests* first (the oldest wait of the most urgent kind), then
    /// *Failed*, *Ready for review*, *Working*, *Unconfirmed*, *Idle*, *Finished*; inside the
    /// other groups the latest activity first, unknown last.
    pub agents: Vec<BoardAgent>,
    /// Finished agents in scope that were not listed because `include_finished` was not set.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hidden_finished: usize,
    /// Agents that would have been listed but did not fit [`BOARD_MAX_AGENTS`] or
    /// [`BOARD_MAX_BYTES`]: the least urgent ones.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// How many agents are in each group, over everything in scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BoardCounts {
    pub agents: usize,
    /// Agents in *Requests*: an approval, a question or a plan waits for the user.
    pub requests: usize,
    pub failed: usize,
    /// Ready for review: a finished task waits for a reviewer.
    pub review: usize,
    pub working: usize,
    /// No evidence of what the agent does. Never counted as idle.
    pub unconfirmed: usize,
    pub idle: usize,
    pub finished: usize,
}

/// How the agents in scope are seen (§ 2.1 of the supervisor spike): what each number of the
/// board is worth depends on the row it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BoardCoverage {
    /// Hutan chats: a journal of turns, requests and tools exists ([`BoardSource::Journal`]).
    pub chats: usize,
    /// Terminal agents that report through hooks ([`BoardSource::Hook`]): their current state
    /// and message, no history.
    pub terminal_reported: usize,
    /// Terminal agents recognised only (OSC 7501 or the program's name,
    /// [`BoardSource::Recognised`]): state and title only.
    pub terminal_recognised: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardRole {
    Agent,
    /// Mawas, the assistant.
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardKind {
    /// A conversation the daemon runs (Codex, OMP or Claude Code over ACP, a direct provider).
    Chat,
    /// An agent in a terminal.
    Terminal,
}

/// Where the facts of a row come from: what its numbers are worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardSource {
    /// A chat session: the daemon runs the agent and journals every turn, request and tool.
    Journal,
    /// A terminal agent that reports through an installed hook: its current state, message and
    /// subagents; no history.
    Hook,
    /// A terminal agent Hutan recognised (OSC 7501, the program's name): state and title only.
    Recognised,
}

/// The activity of an agent (one axis; the group is the other).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardState {
    Working,
    /// Waits for a decision.
    Blocked,
    Idle,
    /// No evidence of what the agent is doing.
    Unconfirmed,
    Completed,
    Failed,
    Stopped,
    Interrupted,
    Disconnected,
}

/// The group of the Agents page the agent is in, in the page's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardGroup {
    Requests,
    Failed,
    Review,
    Working,
    Unconfirmed,
    Idle,
    /// Snoozed by the user in a window; the daemon never says it.
    Snoozed,
    Finished,
}

/// What an agent waits for. "On whom" is only ever the user: Hutan cannot know that an agent
/// waits on something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardWaitKind {
    /// A permission request (`approval.list`).
    Approval,
    /// A question (`question.list`).
    Question,
    /// A plan to approve (a question that opens a plan).
    Plan,
    /// A terminal agent says it waits for the user (its report or OSC 7501).
    TerminalPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardWaiting {
    pub kind: BoardWaitKind,
    /// What is asked (`run cargo test`, the question, the plan's name, the agent's message).
    pub title: String,
    /// When the oldest wait of this kind began, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// The approval or question id (`approval.respond` / `question.answer` take it; a model
    /// cannot answer: those are user-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardRef {
    pub id: WorkspaceId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardCheckout {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// A git worktree of the project, not its main checkout.
    pub worktree: bool,
    /// Another live agent works in the same checkout.
    pub shared: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BoardSubagents {
    pub live: usize,
    pub blocked: usize,
    pub finished: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardAgent {
    pub session_id: SessionId,
    pub title: String,
    pub role: BoardRole,
    pub kind: BoardKind,
    /// What a person reads: `OMP`, `Claude Code`, `Codex`.
    pub harness: String,
    /// The model as reported (never inferred).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The project the session is in (or Home).
    pub project: BoardRef,
    /// The workspace the project is in, when it is in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<BoardRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<BoardCheckout>,
    pub state: BoardState,
    pub group: BoardGroup,
    /// Why the state is what it is, when the reducer has more to say (`Exited with code 1`,
    /// `This harness has not reported its current state.`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// Everything it waits for, the oldest wait first; empty when it waits for nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting: Vec<BoardWaiting>,
    /// What it does right now, on one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// A chat turn is in progress.
    pub turn_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ms: Option<u64>,
    /// The last semantic activity (a report, a node change); never the creation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_ms: Option<u64>,
    /// When the agent last reported its state; absent for a chat the daemon runs itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_ms: Option<u64>,
    pub subagents: BoardSubagents,
    pub source: BoardSource,
}
