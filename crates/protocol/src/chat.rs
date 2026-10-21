//! Agent chat sessions and approvals (protocol 0.9, capability `chat.codex`; direct chats since
//! 0.11, capability `chat.direct`): DTOs of
//! `chat.*`, `approval.*` and their events. The event itself is the core
//! [`AgentEvent`](hutan_core::AgentEvent).

use std::path::PathBuf;

use hutan_core::agent::{PermissionKind, PermissionOption};
use hutan_core::{
    AgentEvent, ApprovalId, ApprovalPolicy, ModelInfo, QuestionId, QuestionResponse, QuestionSpec,
    SandboxMode, SessionId, TurnId, WorkspaceId,
};
use serde::{Deserialize, Serialize};

use crate::assistant::Attachment;

/// Most events one `chat.history` returns, whatever `limit` asks for.
pub const HISTORY_MAX_LIMIT: u32 = 1000;
/// `limit` when a request leaves it out.
pub const HISTORY_DEFAULT_LIMIT: u32 = 200;
/// Longest accepted `chat.send` text, in bytes.
pub const SEND_MAX_BYTES: usize = 256 * 1024;

fn default_limit() -> u32 {
    HISTORY_DEFAULT_LIMIT
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// `chat.create`: opens an agent conversation as a new session; answered with `SessionResult`
/// once the agent is ready (state `idle`) or with an error when it could not start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCreate {
    /// Defaults to the workspace containing `cwd`, else Home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// An existing directory, absolute: the agent works there.
    pub cwd: PathBuf,
    /// `codex`; `direct` (protocol 0.11): the model gateway, no agent in between; `omp` or
    /// `claude` (protocol 0.17, capability `harness.acp`): a harness over ACP.
    pub agent: String,
    /// A model from `chat.models` (`provider.models` for `direct`); absent takes the agent
    /// catalog's default (the provider's `default_model` for `direct`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Absent means `on-request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ApprovalPolicy>,
    /// Absent means `workspace-write`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Protocol 0.17 (capability `harness.acp`), harness agents only: the session mode the agent
    /// starts in (`plan`, from the agent's offered modes); absent keeps the agent's default.
    /// `bad_request` when the agent does not offer it, or the agent is not a harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Protocol 0.11 (capability `chat.direct`), `agent: "direct"` only: the provider to talk
    /// to (`provider.list`); required there, ignored for other agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Protocol 0.11, `agent: "direct"` only: sent to the model ahead of the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// `chat.send`: the user's message; the turn's events follow as `chat.event`.
///
/// Protocol 0.12 (capability `assistant`): `attachments` is read-only context the daemon
/// resolves, redacts, bounds and journals with the message (`docs/assistant.md`).
/// `forbidden` when the daemon's `[assistant.context]` does not allow one of them, when a path
/// is outside the workspace roots and the user did not `pick` it (or an agent sent it), or when
/// an agent (a client running inside an agent's session) sends an `image` or a `pasted_text`;
/// `not_found` when it names something that does not exist.
///
/// Protocol 0.23 (capability `chat.rich_input`): every chat takes attachments (Codex too),
/// including `image` and `pasted_text` (`docs/agents.md` § Rich input). `bad_request` for an
/// attachment out of bounds, a directory, an image that is not PNG, JPEG, GIF or WebP, or a
/// file that is not text; `unsupported` when the chat's backend or model cannot take images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSend {
    pub session_id: SessionId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

/// `chat.retry` (protocol 0.34, capability `chat.retry`): sends the conversation's last user
/// message again, as a new turn, **without journaling it again**: the journal keeps one user
/// message however often it is retried. A direct chat sends the conversation up to and including
/// that message (what a failed or cancelled turn said after it is left out of the request); a
/// harness chat gets the message's text again (attachments are not sent again). Answered with
/// [`ChatSent`]; `invalid_state` while a turn runs or when the chat has no user message yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRetry {
    pub session_id: SessionId,
}

/// Result of `chat.send`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSent {
    pub turn_id: TurnId,
}

/// `chat.history`: the journal of a session. Events come oldest first; to page back pass the
/// first `seq` of the previous page as `before_seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatHistoryRequest {
    pub session_id: SessionId,
    /// Only events with a smaller `seq`; absent starts at the newest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_seq: Option<u64>,
    /// At most [`HISTORY_MAX_LIMIT`]; default [`HISTORY_DEFAULT_LIMIT`].
    #[serde(default = "default_limit")]
    pub limit: u32,
}

/// Result of `chat.history`. Events are marked `replay`. Streaming deltas are not journaled,
/// so `seq` has gaps; the `*_end` events carry the complete text.
///
/// Protocol 0.15 (capability `chat.provenance`): a message or reasoning block that is still
/// streaming appears as its latest `message_partial` / `reasoning_partial`, which includes
/// every delta with a smaller `seq` (the newest page carries a fresh snapshot stamped with a
/// `seq` of its own, no greater than `last_seq`); the live deltas that follow have a greater
/// `seq`. Once the item ends, its partial is gone and `message_end` / `reasoning_end` stand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatHistory {
    pub events: Vec<AgentEvent>,
    /// Older events exist before the first one returned.
    pub has_more: bool,
    /// The newest `seq` the session has used (journaled or not): live events with a greater
    /// `seq` are not in `events`.
    pub last_seq: u64,
}

/// `chat.models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatModelsRequest {
    pub agent: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatModels {
    pub agent: String,
    pub models: Vec<ModelInfo>,
}

/// `approval.list`: permission requests waiting for an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApprovalListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// A permission request waiting for an answer. Pending approvals live in the daemon: they
/// survive client disconnects, and expire when their turn ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalInfo {
    pub id: ApprovalId,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub kind: PermissionKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What the agent offered; only these can be chosen.
    pub options: Vec<PermissionOption>,
    pub requested_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalList {
    pub approvals: Vec<ApprovalInfo>,
}

/// `approval.respond`: `option_id` must be the `id` of one of the approval's options
/// (otherwise `option_not_offered`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRespond {
    pub approval_id: ApprovalId,
    pub option_id: String,
}

/// `question.list` (protocol 0.24, capability `chat.questions`): questions waiting for an
/// answer, oldest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct QuestionListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// A question waiting for an answer. Pending questions live in the daemon: they survive client
/// disconnects and expire when their turn ends (`expires: turn`) or the agent goes away.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionInfo {
    pub id: QuestionId,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(flatten)]
    pub spec: QuestionSpec,
    pub requested_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionList {
    pub questions: Vec<QuestionInfo>,
}

/// `question.answer`: `action` is `submit` (with `values`, by field id, checked against the
/// question's fields: `bad_request` for an unknown field, a missing required one or a value of
/// the wrong type), `decline` or `cancel`. `not_found` when the question is not pending
/// (answered, or its turn ended); `invalid_state` when another client answered first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub question_id: QuestionId,
    #[serde(flatten)]
    pub response: QuestionResponse,
}

/// What a chat session is, as part of its `SessionInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatInfo {
    pub agent: String,
    pub model: String,
    pub approval_policy: ApprovalPolicy,
    pub sandbox: SandboxMode,
    /// A turn is in progress.
    #[serde(default)]
    pub turn_active: bool,
    /// Permission requests waiting for an answer.
    #[serde(default)]
    pub pending_approvals: u32,
    /// Protocol 0.24 (capability `chat.questions`): questions of the agent waiting for an
    /// answer. A session with one counts as waiting for the user like one with an approval.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pending_questions: u32,
    /// Protocol 0.11: the provider of a direct chat (`agent: "direct"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Protocol 0.17: the agent's own id of the conversation once it has one (Codex thread, OMP
    /// or Claude Code session): what `omp --resume <id>` / `codex resume <id>` continue in a
    /// terminal. Absent for direct and assistant chats and until the agent is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    /// Protocol 0.20 (capability `assistant.backend`): this chat is an assistant that runs on
    /// a harness (`agent` is then `omp`, `claude` or `codex`) with its tools restricted; the
    /// provider-backed assistant keeps `agent: "assistant"`. See [`ChatInfo::is_assistant`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub assistant: bool,
    /// Protocol 0.23 (capability `chat.rich_input`): whether the chat's backend takes `image`
    /// attachments. `true` or `false` when the daemon knows (an ACP harness after its
    /// handshake: `promptCapabilities.image`); absent when only a send can tell (Codex, and the
    /// model of a direct chat, which the provider's model list answers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<bool>,
}

impl ChatInfo {
    /// Whether this is an assistant session: the provider-backed one (`agent: "assistant"`) or
    /// a harness assistant (protocol 0.20).
    pub fn is_assistant(&self) -> bool {
        self.assistant || self.agent == crate::assistant::AGENT_ASSISTANT
    }
}
