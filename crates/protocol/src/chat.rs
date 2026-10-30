//! Agent chat sessions and approvals (protocol 0.9, capability `chat.codex`; direct chats since
//! 0.11, capability `chat.direct`): DTOs of
//! `chat.*`, `approval.*` and their events. The event itself is the core
//! [`AgentEvent`](hutan_core::AgentEvent).

use std::path::PathBuf;

use hutan_core::agent::{PermissionKind, PermissionOption};
use hutan_core::{
    AgentEvent, ApprovalId, ApprovalPolicy, CheckpointId, ForkMode, ForkSupport, ModelInfo,
    QuestionId, QuestionResponse, QuestionSpec, SandboxMode, SessionId, SkippedFile, TurnId,
    WorkspaceId,
};
use hutan_core::{SafetyEnforcer, SafetyPreset};
use serde::{Deserialize, Serialize};

use crate::assistant::Attachment;
use crate::checkpoint::CheckpointRestored;
use crate::messages::SessionInfo;
use crate::worktree::WorktreeSetup;

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
    /// Protocol 0.50 (capability `accounts.multi`), `codex`, `omp` and `claude` only: the
    /// managed account the agent runs as (`account.list`), or [`crate::account::SYSTEM_ACCOUNT`]
    /// for the harness's own login. Absent takes the project's default, else the harness's
    /// default, else the harness's own login. `not_found` for an account that does not exist,
    /// `bad_request` for `direct` chats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Protocol 0.51 (capability `chat.safety`, ADR 0021), `codex`, `omp` and `claude` only: a
    /// safety preset, which the daemon turns into what the harness is started with (Codex's
    /// approval policy and sandbox, Claude Code's session mode and Bash sandbox, OMP's approval
    /// mode or tool list: `docs/agents.md` § Safety presets). Absent: the harness's own
    /// settings, as before. `bad_request`, with the reason, when the harness cannot honour the
    /// preset (OMP has no OS sandbox, so no *Contained*), for a `direct` chat, and together
    /// with `approval_policy`, `sandbox` or `mode`, which it replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<SafetyPreset>,
    /// Protocol 0.52 (capability `sandbox`, ADR 0021 decision 3), `omp` only: run the harness
    /// inside Hutan's Seatbelt boundary. With `safety: contained` OMP gets this boundary and no
    /// prompts; *Read-only* and *Ask* may carry it too. `bad_request`, with the reason, for a
    /// harness that runs its own OS sandbox (Codex, Claude Code: Seatbelt does not nest) and for
    /// a `direct` chat; `unsupported` when the boundary cannot be applied (never started
    /// uncontained).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contain: Option<hutan_core::Contain>,
    /// Protocol 0.57 (capability `session.dirs`, ADR 0023 decision 4), `codex`, `omp` and
    /// `claude` only: further folders the agent works in besides `cwd`, in this order. Each is
    /// an absolute, existing directory the daemon makes canonical; at most
    /// [`SESSION_DIRS_MAX`], without duplicates and none equal to `cwd` (`bad_request` with the
    /// reason otherwise; also for a `direct` chat or an assistant, which have no tools to give
    /// folders to). Folders are **user-only**: an agent-originated connection that sends any
    /// gets `forbidden`. They are kept across restarts and restores and listed in
    /// [`ChatInfo::dirs`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_dirs: Vec<PathBuf>,
}

/// Most extra folders one session holds (`chat.create`'s `extra_dirs`, `chat.add_dir`, a
/// terminal's `extra_dirs`).
pub const SESSION_DIRS_MAX: usize = 8;

/// `chat.add_dir` (protocol 0.57, capability `session.dirs`): adds a folder to a session.
/// Answered with `SessionResult`: the session as it is now (`ChatInfo.dirs` for a chat,
/// `SessionInfo.extra_dirs` for a terminal). **User-only** (`forbidden` for an agent-originated
/// connection). `path` must be an absolute, existing directory outside Hutan's data folder and
/// the `[sandbox] read_deny` set, not the session's own folder and not already in the list;
/// `bad_request` otherwise, `invalid_state` when the session already holds
/// [`SESSION_DIRS_MAX`] extra folders, for a session that has ended and for a direct chat or an
/// assistant. A running Codex or OMP chat takes it at once or with its next turn
/// ([`ChatDirApplied`]); Claude Code and a contained OMP need a restart; a terminal agent
/// (OMP, Claude Code) is *pasted* `/add-dir <path>` at its prompt, without the Enter, for the
/// user to confirm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAddDir {
    pub session_id: SessionId,
    pub path: PathBuf,
}

/// `chat.remove_dir` (protocol 0.57, capability `session.dirs`): takes an extra folder out of a
/// session; answered like [`ChatAddDir`]. User-only. `not_found` for a folder that is not in
/// the list (the session's own folder cannot be removed: `bad_request`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRemoveDir {
    pub session_id: SessionId,
    pub path: PathBuf,
}

/// Which folder of a session this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatDirRole {
    /// The session's working directory: always first, cannot be removed.
    Primary,
    /// A folder added with `extra_dirs` or `chat.add_dir`.
    Extra,
}

/// How far the agent has taken a folder, **as the daemon knows it** (never pretended).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatDirApplied {
    /// The running agent has it.
    Now,
    /// The agent is given it with its next turn (Codex's per-turn sandbox policy, an OMP
    /// `/add-dir` that waits for the running turn).
    NextTurn,
    /// Only a restart gives it to the agent: ACP agents take their folders when the session
    /// opens (Claude Code), and a Contain boundary is fixed when the process starts. Also what
    /// a session without a running agent says: the folder is given at the next start.
    Restart,
    /// A terminal agent was *pasted* `/add-dir <path>` at its prompt; it applies when the user
    /// confirms.
    Paste,
    /// This agent cannot take it (a Codex chat in a read-only sandbox may not write there).
    Unsupported,
}

/// One folder of a session (`ChatInfo.dirs`, `SessionInfo.extra_dirs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDir {
    /// Absolute and canonical.
    pub path: PathBuf,
    pub role: ChatDirRole,
    pub applied: ChatDirApplied,
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

/// `chat.fork` (protocol 0.56, capability `chat.fork`, ADR 0023): a new chat that starts with
/// the conversation of `session_id` up to a turn. The parent is never changed; the journal
/// through that turn is **copied** into the new session, so either can be deleted on its own.
/// Answered with [`ChatForked`].
///
/// - `mode` says how the cut is made and is never substituted: `exact` (the default) cuts after
///   `turn_id` where the chat's harness can (direct chats, Codex, Claude Code whose adapter takes
///   a fork point; OMP only when `turn_id` is the newest turn); `head` forks the conversation as
///   it is now (`turn_id` must be absent or the newest turn); `summary` starts a fresh
///   conversation that is given a bounded digest of the turns up to `turn_id`, which the
///   journal says. A harness that cannot make the cut asked for refuses with `unsupported`,
///   whose `data` names `support` ([`ChatForkSupport`]) and the `alternatives` that would work.
/// - `turn_id` defaults to the newest turn. It must be a turn of the chat that has ended:
///   `invalid_state` for an unknown turn and for one that is still running; `invalid_state` too
///   while the chat has a turn in progress and when it has no turn to fork at yet.
/// - The fork inherits the parent's agent, model, safety preset, account and folder. It is
///   created like `chat.create`: the session is answered `idle` once its agent is ready; it
///   follows `[control]` for an agent-originated caller. `bad_request` for a session that is
///   not a chat, `unsupported` for an assistant, `not_found` for an unknown session.
///
/// Where the fork works (protocol 0.58, capability `chat.fork.worktree`, ADR 0023 decision 3):
///
/// - `where: this_folder` (the default) leaves every file alone. With `restore_files` the
///   checkout of the parent is first brought to the files as they were at the end of the turn:
///   the same restore as `checkpoint.restore` over the turns after it (previewed by the daemon,
///   Undo is the restore's own checkpoint, answered as [`ForkDone::restored`]); a conflict, a
///   turn running in the checkout, or no checkpoints refuses the whole fork before anything
///   changes, and a fork that then fails to start is undone.
/// - `where: new_worktree` creates a git worktree on a new branch `branch` (default
///   `fork/<title>-t<N>`) from the commit the turn ended on, lays the files of the turn's
///   checkpoint on it as uncommitted changes (modified, deleted and untracked files; the paths
///   the checkpoint skipped are listed), runs the worktree's setup like `worktree.create`, and
///   the fork works there: Codex forks with the new `cwd`, OMP forks and is moved with
///   `/move`, a Claude Code chat can only be forked with a `summary` there (its transcript
///   belongs to the old folder), a direct chat just starts there. `unsupported` when the turn
///   has no checkpoint (`data.reason`), `conflict` when the branch exists. `restore_files`
///   is `bad_request` with it (the worktree already has the turn's files).
/// - `archive_parent` closes the parent session once the fork is up (`session.close`: its
///   journal stays and `session.reopen` brings it back): the conversation rewind of ADR 0023
///   is "fork, then archive the parent". A parent that cannot be closed is left open and
///   [`ForkDone::notes`] says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatFork {
    pub session_id: SessionId,
    /// The turn the new conversation ends with (inclusive); default: the newest turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default)]
    pub mode: ForkMode,
    /// The new session's name; default: the parent's, marked as a fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Where the fork works: `this_folder`, the parent's folder, or `new_worktree`.
    #[serde(default, rename = "where")]
    pub where_: ForkWhere,
    /// `new_worktree`: the branch the worktree gets; default `fork/<title slug>-t<N>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// `this_folder`: first put the files of the parent's folder back to how the turn left
    /// them (previewed, with Undo).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restore_files: bool,
    /// Close the parent session when the fork is up.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archive_parent: bool,
}

/// Where a fork works (`chat.fork`'s `where`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkWhere {
    /// The parent's folder: no file is touched (unless `restore_files`).
    #[default]
    ThisFolder,
    /// A new git worktree whose files are the checkpoint of the turn (protocol 0.58).
    NewWorktree,
}

/// Result of `chat.fork`: the new session, and what was done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatForked {
    pub session: SessionInfo,
    pub fork: ForkDone,
}

/// What a `chat.fork` did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkDone {
    /// How the cut was made.
    pub mode: ForkMode,
    /// What the parent's harness can do.
    pub support: ForkSupport,
    /// The parent's turn the conversation ends with, and its place among the parent's turns
    /// (counted from 1).
    pub turn_id: TurnId,
    pub ordinal: u32,
    /// Events of the parent's journal copied into the new session.
    pub copied_events: u64,
    /// `where: new_worktree` (protocol 0.58): the worktree the fork works in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<ForkWorktree>,
    /// `restore_files` (protocol 0.58): the restore of the parent's folder; its `checkpoint` is
    /// what `checkpoint.restore` of Undo takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored: Option<CheckpointRestored>,
    /// `archive_parent` (protocol 0.58): the parent session was closed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub parent_archived: bool,
    /// Things the user should know that did not stop the fork (a parent that could not be
    /// closed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The worktree a `chat.fork` with `where: new_worktree` made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkWorktree {
    /// Canonical directory; the fork's folder (inside it, when the parent worked in a
    /// subfolder of its checkout).
    pub path: PathBuf,
    pub branch: String,
    /// The commit the branch starts from: the one the turn ended on.
    pub base: String,
    /// Where the files come from.
    pub files: ForkFiles,
    /// The checkpoint whose tree was laid on the worktree (`files: turn | earlier`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<CheckpointId>,
    /// Paths the checkpoint left out (too large, likely secrets, filtered, nested repositories),
    /// which the worktree has as `base` has them (or not at all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SkippedFile>,
    /// The worktree's setup, as `worktree.create` reports it; absent when none is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<WorktreeSetup>,
}

/// Which files a fork's new worktree holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkFiles {
    /// The files as the turn left them (its checkpoint's end).
    Turn,
    /// The turn changed no file: those of the latest earlier turn that did.
    Earlier,
    /// No turn up to this one changed a file: `base` as it is.
    Base,
}

/// What a chat's harness can do for a fork (`ChatInfo.fork`, protocol 0.56).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatForkSupport {
    pub support: ForkSupport,
    /// Why the support is not `exact`, in a sentence (OMP: only at its newest turn; an adapter
    /// that takes no fork point; an assistant). Absent for `exact`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Result of `chat.send`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSent {
    pub turn_id: TurnId,
}

/// `chat.interrupt`: stops the turn in progress.
///
/// Protocol 0.36 (capability `chat.queue`): Stop also clears the chat's queue, so nothing queued
/// starts after it, unless `keep_queue`: the queued messages stay and the queue holds
/// ([`QueueHold::Stopped`]) until the user sends again. Older peers send `{session_id}` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatInterrupt {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_queue: bool,
}

/// Most messages one chat's queue holds (protocol 0.36, capability `chat.queue`).
pub const QUEUE_MAX_ITEMS: usize = 20;
/// Most bytes of text the queued messages of one chat hold together: a whole queue always fits
/// in one `chat.queue` event.
pub const QUEUE_MAX_BYTES: usize = 1024 * 1024;

/// One message waiting in a chat's queue (protocol 0.36, capability `chat.queue`). Queued
/// messages are text only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedMessage {
    /// Unique within the session's queue; never reused while the daemon runs.
    pub id: u64,
    pub text: String,
    pub queued_at_ms: u64,
    /// Protocol 0.62: who wrote it when it was not the user — a message Mawas queued with the
    /// user's approval. The queue strip says *from Mawas*; the user can send it now or remove
    /// it like any queued message. Editing it removes the label: the words are then the user's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<MessageOrigin>,
}

/// The key of a user message's `payload` that holds its [`MessageOrigin`] (protocol 0.62).
pub const MESSAGE_ORIGIN_KEY: &str = "origin";

/// Who a user message of a chat is from when the user did not type it (protocol 0.62,
/// capability `control.grants.target`, ADR 0025 R4): carried in the `payload` of the user's
/// `message_end` event under [`MESSAGE_ORIGIN_KEY`] and on the [`QueuedMessage`] it waited as.
/// The agent is told too (a context block of Hutan's with the message), and the journal has a
/// notice, so the three agree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageOrigin {
    pub kind: MessageOriginKind,
    /// The Mawas chat that sent it.
    pub session_id: SessionId,
    /// How the user approved sending it: `allow_once`, `allow_chat` (the approval that stored
    /// the grant) or `allow_grant` (a stored grant covered it).
    pub approval: String,
}

/// What wrote a message the user did not type. A kind this build does not know makes the
/// whole [`MessageOrigin`] unreadable, so [`message_origin`] returns `None` for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageOriginKind {
    /// Mawas, the assistant, with the user's approval.
    Mawas,
}

/// The origin a chat event's `payload` names; `None` for a message the user typed and for any
/// other event.
pub fn message_origin(event: &AgentEvent) -> Option<MessageOrigin> {
    event
        .payload
        .as_ref()
        .and_then(|payload| payload.get(MESSAGE_ORIGIN_KEY))
        .and_then(|origin| serde_json::from_value(origin.clone()).ok())
}

/// Why a queue waits instead of sending its next message once no turn runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueHold {
    /// The user stopped the turn and kept the queue (`chat.interrupt` with `keep_queue`).
    Stopped,
    /// The last turn failed: the next message would most likely fail the same way.
    TurnFailed,
    /// A queued message could not be sent; it is back at the front.
    SendFailed,
    /// The agent went away (the session stopped, or the daemon restarted) with messages queued.
    AgentGone,
}

/// A chat's queue (protocol 0.36, capability `chat.queue`): the answer of `chat.queue.list`, and
/// what every `chat.queue` event carries, whole.
///
/// The daemon owns it (bounded: [`QUEUE_MAX_ITEMS`], [`QUEUE_MAX_BYTES`]) and keeps it with the
/// journal, so it survives client reconnects and daemon restarts. When a turn ends and the queue
/// is not held, its first message is sent as the next turn. A turn that fails, a send that
/// fails, a Stop with `keep_queue` and an agent that goes away hold the queue; the next
/// `chat.send` or `chat.send_now` releases it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatQueue {
    pub session_id: SessionId,
    /// In the order they are sent.
    pub items: Vec<QueuedMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<QueueHold>,
    /// The agent takes a message while its turn runs (Codex `turn/steer`): `chat.send_now`
    /// steers the turn at its next tool boundary. `false`: `chat.send_now` interrupts the turn
    /// and sends the message when it has ended.
    #[serde(default)]
    pub steer: bool,
}

/// `chat.queue.add`: puts a message at the end of the chat's queue; answered with
/// [`ChatQueued`]. When no turn runs and the queue is not held, it is sent at once.
/// `bad_request` for an empty or too long text ([`SEND_MAX_BYTES`]), `invalid_state` when the
/// queue is full or the session is not running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatQueueAdd {
    pub session_id: SessionId,
    pub text: String,
}

/// `chat.queue.edit`: replaces the text of a queued message; answered with [`ChatQueued`].
/// `not_found` when it is no longer queued (sent, removed or cleared meanwhile).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatQueueEdit {
    pub session_id: SessionId,
    pub item_id: u64,
    pub text: String,
}

/// `chat.queue.remove`: takes a message out of the queue unsent. `not_found` when it is no
/// longer queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatQueueItemRef {
    pub session_id: SessionId,
    pub item_id: u64,
}

/// Result of `chat.queue.add` and `chat.queue.edit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatQueued {
    pub item: QueuedMessage,
}

/// `chat.send_now` (protocol 0.36, capability `chat.queue`): a message that should not wait for
/// the turn: `text`, or the queued message `item_id` (exactly one of them). Answered with
/// [`ChatSentNow`]: with no turn running it starts one (and releases a held queue); while a turn
/// runs, an agent that steers ([`ChatQueue::steer`]) gets it at its next tool boundary, any
/// other has its turn interrupted (the journal says so) and gets it right after, ahead of the
/// rest of the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSendNow {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<u64>,
}

/// Result of `chat.send_now`: how the message went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "delivery", rename_all = "snake_case")]
pub enum ChatSentNow {
    /// No turn was running: it started one.
    Started { turn_id: TurnId },
    /// The running turn takes it at its next tool boundary.
    Steered { turn_id: TurnId },
    /// The agent cannot take a message during a turn: the turn is being interrupted, and the
    /// message is first in the queue, sent when the turn has ended.
    Interrupting,
}

/// What changed in a chat's queue (the `chat.queue` event).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum QueueChange {
    Added {
        item_id: u64,
    },
    Edited {
        item_id: u64,
    },
    Removed {
        item_id: u64,
    },
    /// `chat.queue.clear`, or a Stop (`stopped`) took every message out unsent.
    Cleared {
        count: u32,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stopped: bool,
    },
    /// The message left the queue as the turn `turn_id`.
    Delivered {
        item_id: u64,
        turn_id: TurnId,
    },
    /// The queue holds (see [`ChatQueue::held`]).
    Held,
    /// The queue no longer holds.
    Released,
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
    /// Protocol 0.62 (capability `control.grants.target`): the agent a `control` approval of
    /// Mawas acts on, and the exact text it would send. Absent for every other approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ControlTarget>,
}

/// The agent a `control` approval of Mawas acts on (protocol 0.62): what the card shows besides
/// its title — *who* is addressed and, for a message, *all* of what is said (at most
/// [`MAWAS_MESSAGE_MAX_BYTES`](crate::grants::MAWAS_MESSAGE_MAX_BYTES); a longer one is
/// refused before anything is asked).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlTarget {
    /// The session that would receive the message or the interrupt.
    pub session_id: SessionId,
    /// Its title when the approval was raised (cut at 200 characters).
    pub title: String,
    /// The whole message (`chat.send`, `chat.send_now`, `chat.queue.add`) or review note
    /// (`task.review`); absent for an interrupt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
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
    /// Protocol 0.34 (capability `chat.mode`): the session mode the harness reports it runs in
    /// (ACP `modes.currentModeId` or a `mode` config option, and their updates). Absent when the
    /// harness reports none (Codex, whose approval policy and sandbox say it, and direct
    /// chats). Hutan only reads it: it never changes an ordinary chat's mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ChatMode>,
    /// Protocol 0.37 (capability `checkpoint`, ADR 0019): whether the chat's turns get
    /// checkpoints of its checkout — `on`, `off` (turned off in the settings, or switched off by
    /// failures or low disk) or `unsupported` (not in git, the home directory, …), with the
    /// reason. Absent for chats that never have checkpoints (assistants, direct chats) and until
    /// the agent first started; it keeps the last state once the agent stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoints: Option<hutan_core::CheckpointStatus>,
    /// Protocol 0.50 (capability `accounts.multi`): the managed account the agent runs as,
    /// fixed when the chat was created (a restart or restore keeps it). Absent: the harness's
    /// own login, and chats that never had an account (direct chats).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<crate::account::ChatAccount>,
    /// Protocol 0.51 (capability `chat.safety`): the safety preset the chat was created with
    /// and what the harness confirmed of it. Absent for chats created without one, direct and
    /// assistant chats, and until the agent first started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<ChatSafety>,
    /// Protocol 0.56 (capability `chat.fork`): what the chat's agent can do for `chat.fork`,
    /// from what it advertised when it started. Absent until the agent first started (it keeps
    /// the last answer once the agent stopped) and for chats that cannot be forked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<ChatForkSupport>,
    /// Protocol 0.57 (capability `session.dirs`): the folders the agent works in — the
    /// session's `cwd` first (`primary`), then the extra ones in the order they were added —
    /// each with how far the agent has taken it. Absent (empty) for direct chats and
    /// assistants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dirs: Vec<ChatDir>,
    /// Protocol 0.61 (capability `assistant.focus`, ADR 0025): the workspace a Global Mawas chat
    /// is focused on (`assistant.focus`). Absent for a chat with no focus (*All workspaces*)
    /// and for every chat that is not a Global assistant: a workspace chat's workspace is its
    /// scope, not a focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<WorkspaceId>,
}

/// What a chat's safety preset amounts to, **as the harness reported it** (protocol 0.51,
/// capability `chat.safety`; ADR 0021 decision 6): never only what Hutan asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSafety {
    /// The preset the chat was created with.
    pub preset: SafetyPreset,
    /// Who enforces the boundary. `none`: no boundary exists (*Full access*), or the harness
    /// did not confirm the preset (`note` says so: *not applied*).
    pub enforced_by: SafetyEnforcer,
    /// Whether the harness's sandbox lets commands reach the network, when it said so (Codex's
    /// `networkAccess`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    /// What was not confirmed, or what the harness cannot say, in a sentence. Absent when the
    /// harness confirmed everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ChatInfo {
    /// Whether this is an assistant session: the provider-backed one (`agent: "assistant"`) or
    /// a harness assistant (protocol 0.20).
    pub fn is_assistant(&self) -> bool {
        self.assistant || self.agent == crate::assistant::AGENT_ASSISTANT
    }
}

/// A harness's session mode (protocol 0.34, capability `chat.mode`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMode {
    /// The harness's id of the mode (`default`, `plan`, `acceptEdits`, `bypassPermissions`).
    pub id: String,
    /// The harness's name for it, when it gave one (`Plan Mode`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The mode lets the agent act without asking (Claude Code `bypassPermissions`, a `yolo`
    /// or full-access mode): clients show it as a warning.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approves_everything: bool,
}

impl ChatMode {
    /// The mode `id` (named `name`), with [`approves_everything`](Self::approves_everything)
    /// decided from its id.
    pub fn new(id: impl Into<String>, name: Option<String>) -> Self {
        let id = id.into();
        let approves_everything = Self::id_approves_everything(&id);
        Self {
            id,
            name: name.filter(|n| !n.trim().is_empty()),
            approves_everything,
        }
    }

    /// Whether a mode id means "act without asking": `bypassPermissions`, `yolo`,
    /// `full-access` / `full_access` (any case).
    pub fn id_approves_everything(id: &str) -> bool {
        let id = id.to_ascii_lowercase();
        id.contains("bypass") || id.contains("yolo") || id.replace('_', "-").contains("full-access")
    }

    /// What a person reads: the name, else the id.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}
