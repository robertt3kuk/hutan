//! The internal agent event model (ADR 0005): what every agent adapter produces, what the
//! journal stores, and what clients receive as `chat.event`. No I/O lives here.
//!
//! An [`AgentEventDraft`] is what an adapter emits: local turn/item ids, the provider's own ids,
//! one [`AgentEventKind`], and the provider's raw payload. The daemon stamps it with the session,
//! a per-session sequence number, a timestamp and the live/replay marker, which makes it an
//! [`AgentEvent`].

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{ApprovalId, ItemId, QuestionId, SessionId, TurnId};
use crate::question::{QuestionOutcome, QuestionSpec};

/// Whether a client sees the event as it happens or from the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    #[default]
    Live,
    Replay,
}

/// The provider's identifiers of what an event is about; opaque to Hutan.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProviderIds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// The provider's id of a request it made of us (approvals).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// An agent event as an adapter emits it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEventDraft {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
    #[serde(default)]
    pub provider: ProviderIds,
    #[serde(flatten)]
    pub kind: AgentEventKind,
    /// The provider's message this event was derived from, as received (absent for streaming
    /// deltas and when it exceeds [`MAX_PAYLOAD_BYTES`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

/// A raw provider payload larger than this is not kept on the event.
pub const MAX_PAYLOAD_BYTES: usize = 128 * 1024;
/// Tool output and diffs longer than this many bytes are cut (and flagged `truncated`).
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;

impl AgentEventDraft {
    pub fn new(kind: AgentEventKind) -> Self {
        Self {
            turn_id: None,
            item_id: None,
            provider: ProviderIds::default(),
            kind,
            payload: None,
        }
    }

    /// Stamps the draft: the daemon assigns the session, `seq` and time.
    pub fn into_event(self, session_id: SessionId, seq: u64, ts_ms: u64) -> AgentEvent {
        AgentEvent {
            session_id,
            seq,
            ts_ms,
            delivery: Delivery::Live,
            turn_id: self.turn_id,
            item_id: self.item_id,
            provider: self.provider,
            kind: self.kind,
            payload: self.payload,
        }
    }
}

/// One event of an agent session, strictly ordered by `seq` within the session.
///
/// Streaming deltas (`message_delta`, `reasoning_delta`, `command_output`) consume a `seq` but
/// are not journaled; the `*_end` events carry the complete text, so a replay from the journal
/// has gaps in `seq` and loses nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub session_id: SessionId,
    pub seq: u64,
    pub ts_ms: u64,
    #[serde(default)]
    pub delivery: Delivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
    #[serde(default)]
    pub provider: ProviderIds,
    #[serde(flatten)]
    pub kind: AgentEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

impl AgentEvent {
    /// The same event marked as read from the journal.
    #[must_use]
    pub fn replayed(mut self) -> Self {
        self.delivery = Delivery::Replay;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

/// Which part of a reasoning block a delta belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningPart {
    Summary,
    Content,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// A shell command.
    Command,
    /// A patch applied to files.
    FileChange,
    /// A tool of an MCP server.
    Mcp,
    /// A tool the client registered with the provider.
    Dynamic,
    WebSearch,
    /// Anything else the provider calls an item.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Completed,
    Failed,
    /// The user (or policy) refused it.
    Declined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    Add,
    Delete,
    Update,
}

/// One file of a diff: a unified patch, or the content before/after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub kind: FileChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    Command,
    FileChange,
    /// Extra permissions (network, file system) for the turn or session.
    Permissions,
    /// An agent asks Hutan itself for something (protocol 0.19, ADR 0016): a mutating request
    /// of an agent-originated connection under `[control] agents = "ask"`.
    Control,
    Other,
}

/// What choosing an option means; clients style buttons by it, providers' own names stay in `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    /// Refuse and stop the turn.
    Cancel,
    /// A decision Hutan has no name for; the label is all there is.
    Other,
}

/// One answer the provider offered for a permission request. Only these may be chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionOption {
    /// The provider's name for the decision (`accept`, `acceptForSession`, `decline`, ...).
    pub id: String,
    pub label: String,
    pub kind: PermissionOptionKind,
    /// The decision exactly as the provider offered it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Value>,
}

/// Who answered a permission request (protocol 0.15): the kind of client, as the daemon saw it
/// in the client's `hello` (`desktop`, `cli`, `notifier`, `other`; a newer daemon may add
/// more), and its name when the client gave one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecidedBy {
    pub client_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PermissionOutcome {
    /// The user chose one of the offered options.
    Selected { option_id: String },
    /// The turn ended (or the provider withdrew the request) before anyone answered.
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub text: String,
    pub status: PlanStepStatus,
}

/// Token counts; a field the provider did not report stays `None`, never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenCounts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    /// Protocol 0.59: tokens written to the prompt cache (priced above plain input by Claude);
    /// Codex's `cacheWriteInputTokens`, an ACP prompt's `cachedWriteTokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// What happened. Internally tagged by `type` on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEventKind {
    TurnStarted,
    MessageStart {
        role: MessageRole,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    /// Streamed text; not journaled.
    MessageDelta {
        text: String,
    },
    /// The message is complete; `text` is all of it.
    MessageEnd {
        role: MessageRole,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        text: String,
    },
    /// Protocol 0.15: the text of the open assistant message `message_id` so far, journaled
    /// (replaced in place, at most every 500 ms) while it streams so that a client that
    /// joins mid-stream starts from the full text. It includes every `message_delta` of the
    /// message with a smaller `seq`; the message's `message_end` supersedes it. Never
    /// streamed live.
    MessagePartial {
        message_id: ItemId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        text: String,
    },
    ReasoningStart,
    /// Streamed reasoning text; not journaled.
    ReasoningDelta {
        part: ReasoningPart,
        text: String,
    },
    /// Protocol 0.15: like [`MessagePartial`](Self::MessagePartial) for an open reasoning
    /// block: the streamed summary and content so far.
    ReasoningPartial {
        reasoning_id: ItemId,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        content: String,
    },
    ReasoningEnd {
        #[serde(default)]
        summary: Vec<String>,
        #[serde(default)]
        content: Vec<String>,
    },
    ToolStart {
        tool: ToolKind,
        name: String,
        /// One line for a card or a log: the command, the files, the query.
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Value>,
    },
    ToolUpdate {
        message: String,
    },
    ToolEnd {
        tool: ToolKind,
        status: ToolStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// `output` was cut to [`MAX_OUTPUT_BYTES`].
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    /// Streamed output of a running command; not journaled (`tool_end` has the whole output).
    CommandOutput {
        text: String,
    },
    /// Files changed (or about to be), per file and/or as one aggregate unified patch.
    FileDiff {
        #[serde(default)]
        changes: Vec<FileChange>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patch: Option<String>,
    },
    PermissionRequested {
        approval_id: ApprovalId,
        kind: PermissionKind,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// The options the provider offered, as offered.
        options: Vec<PermissionOption>,
    },
    PermissionResolved {
        approval_id: ApprovalId,
        #[serde(flatten)]
        outcome: PermissionOutcome,
        /// Protocol 0.15: the client that answered; absent when nobody did (expiry), and for
        /// events journaled before 0.15.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decided_by: Option<DecidedBy>,
    },
    /// Protocol 0.24 (capability `chat.questions`): the agent asks the user something that is
    /// not a permission (Codex `requestUserInput`, an MCP or ACP elicitation).
    QuestionRequested {
        question_id: QuestionId,
        #[serde(flatten)]
        spec: QuestionSpec,
    },
    /// A question is over. A secret answer is never in here (`secret` names its fields).
    QuestionResolved {
        question_id: QuestionId,
        #[serde(flatten)]
        outcome: QuestionOutcome,
        /// The client that answered; absent when nobody did (expiry).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decided_by: Option<DecidedBy>,
    },
    Plan {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        explanation: Option<String>,
        #[serde(default)]
        steps: Vec<PlanStep>,
        /// A plan the agent wrote as prose.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// Cumulative (`total`) and latest-request (`last`) counts of the conversation.
    Usage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<TokenCounts>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last: Option<TokenCounts>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_window: Option<u64>,
    },
    TurnCompleted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    TurnFailed {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<String>,
    },
    TurnCancelled,
    /// How to continue this conversation with the provider after a restart.
    ResumeLocator {
        agent: String,
        thread_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Notice {
        level: NoticeLevel,
        message: String,
    },
    /// A provider message Hutan has no model for; only `provider` and `payload` are meaningful.
    Opaque {
        method: String,
    },
}

impl AgentEventKind {
    /// Streaming events are delivered live and not journaled.
    pub const fn is_streaming(&self) -> bool {
        matches!(
            self,
            Self::MessageDelta { .. } | Self::ReasoningDelta { .. } | Self::CommandOutput { .. }
        )
    }

    /// The wire name (`type`).
    pub const fn name(&self) -> &'static str {
        match self {
            Self::TurnStarted => "turn_started",
            Self::MessageStart { .. } => "message_start",
            Self::MessageDelta { .. } => "message_delta",
            Self::MessageEnd { .. } => "message_end",
            Self::MessagePartial { .. } => "message_partial",
            Self::ReasoningStart => "reasoning_start",
            Self::ReasoningDelta { .. } => "reasoning_delta",
            Self::ReasoningPartial { .. } => "reasoning_partial",
            Self::ReasoningEnd { .. } => "reasoning_end",
            Self::ToolStart { .. } => "tool_start",
            Self::ToolUpdate { .. } => "tool_update",
            Self::ToolEnd { .. } => "tool_end",
            Self::CommandOutput { .. } => "command_output",
            Self::FileDiff { .. } => "file_diff",
            Self::PermissionRequested { .. } => "permission_requested",
            Self::PermissionResolved { .. } => "permission_resolved",
            Self::QuestionRequested { .. } => "question_requested",
            Self::QuestionResolved { .. } => "question_resolved",
            Self::Plan { .. } => "plan",
            Self::Usage { .. } => "usage",
            Self::TurnCompleted { .. } => "turn_completed",
            Self::TurnFailed { .. } => "turn_failed",
            Self::TurnCancelled => "turn_cancelled",
            Self::ResumeLocator { .. } => "resume_locator",
            Self::Notice { .. } => "notice",
            Self::Opaque { .. } => "opaque",
        }
    }

    /// The turn is over.
    pub const fn ends_turn(&self) -> bool {
        matches!(
            self,
            Self::TurnCompleted { .. } | Self::TurnFailed { .. } | Self::TurnCancelled
        )
    }
}

/// When the agent asks for permission. Names are the agents' own (Codex: `untrusted`,
/// `on-request`, `never`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalPolicy {
    /// Everything but known-safe commands asks.
    Untrusted,
    /// The agent decides when to ask.
    #[default]
    OnRequest,
    /// Never asks; failures go back to the model.
    Never,
}

impl ApprovalPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }
}

impl fmt::Display for ApprovalPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ApprovalPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "untrusted" => Ok(Self::Untrusted),
            "on-request" => Ok(Self::OnRequest),
            "never" => Ok(Self::Never),
            other => Err(format!(
                "unknown approval policy `{other}` (untrusted, on-request, never)"
            )),
        }
    }
}

/// What the agent's own commands may touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    DangerFullAccess,
}

impl SandboxMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

impl fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SandboxMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read-only" => Ok(Self::ReadOnly),
            "workspace-write" => Ok(Self::WorkspaceWrite),
            "danger-full-access" => Ok(Self::DangerFullAccess),
            other => Err(format!(
                "unknown sandbox `{other}` (read-only, workspace-write, danger-full-access)"
            )),
        }
    }
}

/// One model an agent offers (`model/list` for Codex).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// What to pass as the model of a new session.
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    /// The agent's catalog default (not whatever a config file names).
    pub is_default: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn draft(kind: AgentEventKind) -> AgentEventDraft {
        AgentEventDraft::new(kind)
    }

    #[test]
    fn events_are_flat_objects_tagged_by_type() {
        let session = SessionId::new();
        let mut d = draft(AgentEventKind::MessageDelta { text: "hi".into() });
        d.provider.thread_id = Some("t1".into());
        let event = d.into_event(session, 7, 1000);
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(
            value,
            json!({
                "session_id": session,
                "seq": 7,
                "ts_ms": 1000,
                "delivery": "live",
                "provider": {"thread_id": "t1"},
                "type": "message_delta",
                "text": "hi",
            })
        );
        assert_eq!(serde_json::from_value::<AgentEvent>(value).unwrap(), event);
    }

    #[test]
    fn permission_requests_keep_the_offered_options() {
        let approval = ApprovalId::new();
        let kind = AgentEventKind::PermissionRequested {
            approval_id: approval,
            kind: PermissionKind::Command,
            title: "touch hi.txt".into(),
            detail: None,
            options: vec![PermissionOption {
                id: "accept".into(),
                label: "Allow once".into(),
                kind: PermissionOptionKind::AllowOnce,
                provider: Some(json!("accept")),
            }],
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["options"][0]["provider"], "accept");
        assert_eq!(
            serde_json::from_value::<AgentEventKind>(json).unwrap(),
            kind
        );
    }

    #[test]
    fn resolution_outcomes_flatten_into_the_event() {
        let approval = ApprovalId::new();
        let kind = AgentEventKind::PermissionResolved {
            approval_id: approval,
            outcome: PermissionOutcome::Selected {
                option_id: "accept".into(),
            },
            decided_by: Some(DecidedBy {
                client_kind: "cli".into(),
                client_name: None,
            }),
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["type"], "permission_resolved");
        assert_eq!(json["decided_by"], json!({"client_kind": "cli"}));
        assert_eq!(json["outcome"], "selected");
        assert_eq!(json["option_id"], "accept");
        assert_eq!(
            serde_json::from_value::<AgentEventKind>(json).unwrap(),
            kind
        );
        let expired = serde_json::to_value(AgentEventKind::PermissionResolved {
            approval_id: approval,
            outcome: PermissionOutcome::Expired,
            decided_by: None,
        })
        .unwrap();
        assert_eq!(expired["outcome"], "expired");
        assert!(expired.get("decided_by").is_none());
    }

    #[test]
    fn question_events_flatten_their_spec_and_outcome() {
        use crate::question::{FieldKind, QuestionExpiry, QuestionField};

        let question = QuestionId::new();
        let requested = AgentEventKind::QuestionRequested {
            question_id: question,
            spec: QuestionSpec {
                title: "Which branch?".into(),
                prompt: "Pick **one**".into(),
                fields: vec![QuestionField {
                    id: "branch".into(),
                    label: "Branch".into(),
                    description: None,
                    kind: FieldKind::Text,
                    required: true,
                    default: None,
                }],
                raw_schema: None,
                source: Some("MCP server git".into()),
                expires: QuestionExpiry::Turn,
            },
        };
        let json = serde_json::to_value(&requested).unwrap();
        assert_eq!(json["type"], "question_requested");
        assert_eq!(json["title"], "Which branch?");
        assert_eq!(json["fields"][0]["kind"]["type"], "text");
        assert_eq!(
            serde_json::from_value::<AgentEventKind>(json).unwrap(),
            requested
        );

        let resolved = AgentEventKind::QuestionResolved {
            question_id: question,
            outcome: QuestionOutcome::Declined,
            decided_by: Some(DecidedBy {
                client_kind: "cli".into(),
                client_name: None,
            }),
        };
        let json = serde_json::to_value(&resolved).unwrap();
        assert_eq!(json["type"], "question_resolved");
        assert_eq!(json["outcome"], "declined");
        assert_eq!(
            serde_json::from_value::<AgentEventKind>(json).unwrap(),
            resolved
        );
    }

    #[test]
    fn usage_never_zero_fills() {
        let kind = AgentEventKind::Usage {
            total: Some(TokenCounts {
                total_tokens: Some(5),
                ..TokenCounts::default()
            }),
            last: None,
            context_window: None,
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json, json!({"type": "usage", "total": {"total_tokens": 5}}));
    }

    #[test]
    fn streaming_and_turn_end_classification() {
        assert!(
            AgentEventKind::MessageDelta {
                text: String::new()
            }
            .is_streaming()
        );
        assert!(!AgentEventKind::TurnCancelled.is_streaming());
        assert!(AgentEventKind::TurnCancelled.ends_turn());
        assert!(!AgentEventKind::TurnStarted.ends_turn());
    }

    #[test]
    fn partials_round_trip_and_are_journaled() {
        let item = ItemId::new();
        let message = AgentEventKind::MessagePartial {
            message_id: item,
            phase: None,
            text: "so far".into(),
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "message_partial");
        assert!(json.get("phase").is_none());
        assert_eq!(
            serde_json::from_value::<AgentEventKind>(json).unwrap(),
            message
        );
        let reasoning = AgentEventKind::ReasoningPartial {
            reasoning_id: item,
            summary: "thinking".into(),
            content: String::new(),
        };
        assert_eq!(reasoning.name(), "reasoning_partial");
        let json = serde_json::to_value(&reasoning).unwrap();
        assert_eq!(
            serde_json::from_value::<AgentEventKind>(json).unwrap(),
            reasoning
        );
        assert!(!message.is_streaming() && !reasoning.is_streaming());
    }

    #[test]
    fn policies_parse_and_print_the_agents_names() {
        for policy in [
            ApprovalPolicy::Untrusted,
            ApprovalPolicy::OnRequest,
            ApprovalPolicy::Never,
        ] {
            assert_eq!(
                policy.to_string().parse::<ApprovalPolicy>().unwrap(),
                policy
            );
            assert_eq!(
                serde_json::to_value(policy).unwrap(),
                json!(policy.as_str())
            );
        }
        for mode in [
            SandboxMode::ReadOnly,
            SandboxMode::WorkspaceWrite,
            SandboxMode::DangerFullAccess,
        ] {
            assert_eq!(mode.to_string().parse::<SandboxMode>().unwrap(), mode);
            assert_eq!(serde_json::to_value(mode).unwrap(), json!(mode.as_str()));
        }
        assert!("always".parse::<ApprovalPolicy>().is_err());
    }
}
