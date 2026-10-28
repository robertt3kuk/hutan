//! The seam between the daemon and a concrete agent (ADR 0005): spawn an agent, get a stream of
//! [`AgentEventDraft`]s, and call it through [`AgentAdapter`].

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use hutan_core::{
    AgentEventDraft, ApprovalId, ApprovalPolicy, ForkSupport, NodeUpdate, QuestionId,
    QuestionResponse, SandboxMode, TurnId,
};
use serde::{Deserialize, Serialize};

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
    /// ACP agents only: the `_meta` of `session/new` (and of `session/load` / `session/resume`),
    /// for what an agent takes through it (Claude Code's `claudeCode.options`, whose `sandbox`
    /// is Hutan's *Contained* preset). Codex ignores it.
    pub session_meta: Option<serde_json::Value>,
    /// Open the conversation as a fork of another one (ADR 0023) instead of starting a new one
    /// or continuing `resume_thread_id`: Codex `thread/fork`, ACP `session/fork`.
    pub fork: Option<ForkFrom>,
    /// Further folders the agent works in besides `cwd` (protocol 0.57, `ChatSpec.dirs`):
    /// absolute, canonical and existing. Each agent takes them its own way
    /// ([`AgentAdapter::dirs`]).
    pub dirs: Vec<PathBuf>,
    /// The conversation has had session folders at some time, so an agent that keeps its own
    /// list (OMP's session header) is asked for it on a resume and made equal to `dirs`.
    pub dirs_managed: bool,
}

/// How an agent takes a session folder it has not been given yet (`ChatDirApplied`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirsChange {
    /// With its next turn: Codex's per-turn sandbox policy, OMP's `/add-dir` between turns.
    NextTurn,
    /// Only when the session opens again: ACP agents that take `additionalDirectories` at
    /// `session/new|load|resume` (Claude Code).
    Restart,
    /// Not at all: the agent never advertised it, or its sandbox forbids writing there.
    Unsupported,
}

/// The session folders an agent holds, as the adapter knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDirs {
    /// What the agent has been given (and has confirmed where it confirms).
    pub given: Vec<PathBuf>,
    /// How a folder that is not in `given` reaches it.
    pub change: DirsChange,
}

/// The conversation a fork starts from, and where it is cut. What the agent forks is its own
/// copy of the parent's conversation, found by `thread_id` in the agent's own storage, so the
/// parent need not be running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkFrom {
    /// The parent's provider conversation (Codex thread, ACP session).
    pub thread_id: String,
    /// Where the conversation is cut; `None`: at its end (what the agent has now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<ForkPoint>,
    /// The fork works in another folder than its parent did (a new worktree, `StartSpec.cwd`):
    /// the folder the parent's conversation belongs to. Codex's `thread/fork` takes the new
    /// folder directly and ignores this; OMP refuses a `cwd` other than its parent's on
    /// `session/fork`, so it is forked here and then moved to `StartSpec.cwd` with `/move`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cwd: Option<PathBuf>,
}

/// The provider's names for the end of the turn a fork keeps, as Hutan journaled them. An
/// agent reads the one it knows; one that cannot cut at a turn refuses ([`AgentError::Unsupported`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkPoint {
    /// Codex: the turn's id (`thread/fork` `lastTurnId`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_turn_id: Option<String>,
    /// Claude Code's ACP adapter: the turn's last assistant message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<ForkMessage>,
}

/// An assistant message of the parent, named the way Claude Code's ACP adapter finds it
/// (`_meta.jetbrains.air.fork`, adapter 0.98): the ACP `messageId` of its chunks, and — for an
/// adapter that no longer knows the id — the SHA-256 of its text and how many equal messages
/// precede it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkMessage {
    pub id: String,
    /// `sha256:<hex>` of the message's text.
    pub fingerprint: String,
    /// The message's place among equal ones, counted from 1.
    pub occurrence: u32,
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
    /// The session mode the agent reports it runs in (ACP `modes` or a `mode` config option),
    /// `None` when it reports none (Codex). Read only: what the agent was started with.
    pub mode: Option<SessionMode>,
    /// What the agent says it enforces (Codex's `thread/start` answer: approval policy and
    /// sandbox); `None` when the agent reports none (ACP agents say a session mode instead).
    /// Read back, never what Hutan asked for.
    pub reported: Option<ReportedSafety>,
    /// What the agent can do for a fork, from what it advertised (ACP `sessionCapabilities`,
    /// the adapter's name and version); `None` when this start cannot tell (a Codex app server
    /// always cuts at a turn: `Some(Exact)`).
    pub fork: Option<ForkSupport>,
}

/// The approval policy and sandbox an agent reported when the conversation opened.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReportedSafety {
    /// The agent's name of its approval policy (`on-request`, `never`, `untrusted`).
    pub approval_policy: Option<String>,
    /// The agent's name of its sandbox (`read-only`, `workspace-write`, `danger-full-access`).
    pub sandbox: Option<String>,
    /// Whether the sandbox lets commands reach the network, when it said so.
    pub network: Option<bool>,
}

/// A session mode an agent reports (protocol 0.34, `ChatInfo.mode`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMode {
    /// The agent's id of the mode (`default`, `plan`, `bypassPermissions`).
    pub id: String,
    /// Its display name, when the agent gave one.
    pub name: Option<String>,
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
    /// The agent reports that its session mode changed (ACP `current_mode_update`, a `mode`
    /// config option update). Whoever changed it, the agent or its user: Hutan never does.
    Mode(SessionMode),
    /// The harness reported the limit windows of its account (Codex `account/rateLimits/updated`,
    /// Claude Code's `_claude/rateLimit` in a `usage_update`): not part of any conversation, so
    /// not an event of the journal (`docs/agents.md` § Usage).
    Limits(Box<crate::usage::RateLimits>),
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
    /// Context the agent is given **without** the journal showing it as the user's words: what
    /// Hutan adds on its own (a summary fork's digest, ADR 0023). ACP agents take each block as
    /// a resource of its own.
    pub unjournaled: Vec<ContextBlock>,
}

impl MessageExtras {
    pub fn is_empty(&self) -> bool {
        self.context.is_empty() && self.images.is_empty() && self.unjournaled.is_empty()
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

    /// The session folders the agent holds right now and how it takes new ones; `None` for
    /// agents that know no session folders (the daemon says `unsupported`).
    fn dirs(&self) -> Option<AgentDirs> {
        None
    }

    /// Makes `dirs` the wanted extra folders of the running conversation. What cannot be given
    /// now stays wanted and is reported by [`dirs`](Self::dirs): a change waits for the turn in
    /// progress, and for a restart where the agent only takes folders when it opens. Agents
    /// without session folders refuse.
    fn set_dirs(&self, dirs: Vec<PathBuf>) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            let _ = dirs;
            Err(AgentError::Unsupported(format!(
                "{} takes no extra folders",
                self.agent()
            )))
        })
    }

    /// Asks the agent to stop the turn in progress.
    fn interrupt(&self) -> BoxFuture<'_, Result<(), AgentError>>;

    /// Whether the agent takes a message while its turn runs ([`steer`](Self::steer)).
    fn can_steer(&self) -> bool {
        false
    }

    /// Gives the turn in progress more of the user's input, which the agent reads at its next
    /// step (Codex `turn/steer`): no new turn starts, and the turn's events say when the agent
    /// took it (its echo of the user message). Returns the running turn. Fails with
    /// [`AgentError::NoTurn`] when no turn runs and [`AgentError::Unsupported`] for agents that
    /// cannot take a message during a turn.
    fn steer(&self, text: String) -> BoxFuture<'_, Result<TurnId, AgentError>> {
        Box::pin(async move {
            let _ = text;
            Err(AgentError::Unsupported(format!(
                "{} takes no message while its turn runs",
                self.agent()
            )))
        })
    }

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
