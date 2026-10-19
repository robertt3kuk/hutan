//! The seam between the daemon and a concrete agent (ADR 0005): spawn an agent, get a stream of
//! [`AgentEventDraft`]s, and call it through [`AgentAdapter`].

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use hutan_core::{
    AgentEventDraft, ApprovalId, ApprovalPolicy, NodeUpdate, QuestionId, QuestionResponse,
    SandboxMode, TurnId,
};

use crate::error::AgentError;

/// What opens (or reopens) one conversation with an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartSpec {
    pub cwd: PathBuf,
    /// A model from the agent's catalog; `None` takes the catalog's default.
    pub model: Option<String>,
    pub approval_policy: ApprovalPolicy,
    pub sandbox: SandboxMode,
    /// Continue this provider conversation instead of starting a new one.
    pub resume_thread_id: Option<String>,
    /// Text the agent is given as instructions of the thread, next to its own (Codex
    /// `developerInstructions` on `thread/start` and `thread/resume`, MW-2: the memory the user
    /// approved). ACP agents take their instructions on the command line and ignore it.
    pub developer_instructions: Option<String>,
}

/// The conversation that is open after [`StartSpec`] was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub model: String,
    pub thread_id: String,
    pub resumed: bool,
    /// Whether the agent takes images (protocol 0.23): `Some` when its handshake says so (ACP
    /// `promptCapabilities.image`), `None` when it cannot tell before a message is sent.
    pub images: Option<bool>,
}

/// How an agent process ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    /// The last lines the process wrote to stderr.
    pub stderr_tail: String,
}

/// What an adapter's event stream carries.
#[derive(Debug, Clone, PartialEq)]
pub enum AdapterMessage {
    Event(Box<AgentEventDraft>),
    /// Something about an agent of the session's observation tree: the main agent's thread, or a
    /// subagent's (`docs/integrations.md`).
    Node(Box<NodeUpdate>),
    /// The process ended; nothing follows.
    Exited(ExitInfo),
}

/// Read-only context attached to a message (`docs/assistant.md`): a label for the user and
/// the resolved, redacted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBlock {
    pub label: String,
    pub text: String,
}

/// An image attached to a message (protocol 0.23, `docs/agents.md` § Rich input): the bytes of
/// a PNG, JPEG, GIF or WebP file the daemon read, which is not redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBlock {
    /// What the user's message says about it: `[image: shot.png · image/png · 120 KiB]`.
    pub header: String,
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    pub media_type: String,
    pub data: Arc<Vec<u8>>,
    /// A file that holds exactly these bytes, for agents that read images from disk (Codex):
    /// the file it was attached from, or a temporary copy the daemon wrote.
    pub path: Option<PathBuf>,
}

/// What a message carries besides its text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageExtras {
    /// Text attachments, one by one (ACP embedded context).
    pub context: Vec<ContextBlock>,
    pub images: Vec<ImageBlock>,
}

impl MessageExtras {
    pub fn is_empty(&self) -> bool {
        self.context.is_empty() && self.images.is_empty()
    }
}

/// A turn the agent accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRef {
    pub turn_id: TurnId,
    pub provider_turn_id: String,
}

/// One running agent conversation. Methods never block the caller's event loop on the agent's
/// event stream, but they wait for the agent's reply: call them from a spawned task.
pub trait AgentAdapter: Send + Sync + std::fmt::Debug {
    /// The agent's short name (`codex`, `omp`, `claude`).
    fn agent(&self) -> &str;

    /// Sends the user's message; the turn's events follow on the stream.
    fn send_message(&self, text: String) -> BoxFuture<'_, Result<TurnRef, AgentError>>;

    /// [`send_message`](Self::send_message) with attachments. Agents that cannot take context
    /// refuse a message that has some ([`AgentError::Unsupported`]); one that cannot take
    /// images refuses images the same way.
    fn send_message_with(
        &self,
        text: String,
        extras: MessageExtras,
    ) -> BoxFuture<'_, Result<TurnRef, AgentError>> {
        Box::pin(async move {
            if extras.is_empty() {
                self.send_message(text).await
            } else {
                Err(AgentError::Unsupported(format!(
                    "{} does not take attached context",
                    self.agent()
                )))
            }
        })
    }

    /// Switches the model of the running conversation; the next turn uses it. Agents without
    /// model switching refuse.
    fn set_model(&self, model: String) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            let _ = model;
            Err(AgentError::Unsupported(format!(
                "{} does not offer model switching",
                self.agent()
            )))
        })
    }

    /// Asks the agent to stop the turn in progress.
    fn interrupt(&self) -> BoxFuture<'_, Result<(), AgentError>>;

    /// Answers a pending permission request with one of the options it offered.
    fn respond(
        &self,
        approval: ApprovalId,
        option_id: String,
    ) -> BoxFuture<'_, Result<(), AgentError>>;

    /// Answers a pending question (Codex `requestUserInput`, an MCP or ACP elicitation) in the
    /// shape the agent asked for: values, a decline, or a cancel. Agents that never ask refuse.
    /// [`AgentError::UnknownQuestion`] when the question is not pending,
    /// [`AgentError::BadAnswer`] when the values cannot be put into the agent's shape.
    fn answer_question(
        &self,
        question: QuestionId,
        response: QuestionResponse,
    ) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            let _ = (question, response);
            Err(AgentError::Unsupported(format!(
                "{} does not ask questions",
                self.agent()
            )))
        })
    }

    /// Ends the process (politely first); the stream ends with [`AdapterMessage::Exited`].
    fn shutdown(&self) -> BoxFuture<'_, ()>;

    /// The agent's own process id while it runs (the process whose descendants are the
    /// session's processes); `None` for agents that are not a process of their own, and after
    /// the process was reaped.
    fn pid(&self) -> Option<u32> {
        None
    }

    /// Asks the agent to stop the turn in progress of one of its threads — a subagent's; the
    /// thread is named by the node id the adapter reported for it. Fails with
    /// [`AgentError::NoTurn`] when that thread has no turn in progress and with
    /// [`AgentError::Unsupported`] when the agent cannot stop a single subagent.
    fn interrupt_thread(&self, _thread_id: &str) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async {
            Err(AgentError::Unsupported(
                "this agent cannot stop a single subagent".to_owned(),
            ))
        })
    }
}
