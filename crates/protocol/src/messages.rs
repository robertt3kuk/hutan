//! Typed requests, results, events, and DTOs for protocol v0.1 (`docs/protocol.md`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use hutan_core::agent::{DecidedBy, PermissionOutcome};
use hutan_core::{
    AgentEvent, AgentNode, ApprovalId, CheckpointAvailability, CheckpointId, CheckpointInfo, Epoch,
    NodeOutcome, QuestionId, QuestionOutcome, Seq, SessionId, SessionKind, SessionState, Task,
    TaskId, TerminalId, WorkspaceId,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ProtocolVersion;
use crate::account::{AccountLogin, AccountLogout};
use crate::assistant::{AssistantConfigure, AssistantListRequest, AssistantOpen, AssistantReset};
use crate::blocks::{Block, TerminalBlockText, TerminalBlocks};
use crate::chat::{
    ApprovalInfo, ApprovalListRequest, ApprovalRespond, ChatCreate, ChatHistoryRequest, ChatInfo,
    ChatInterrupt, ChatModelsRequest, ChatQueue, ChatQueueAdd, ChatQueueEdit, ChatQueueItemRef,
    ChatRetry, ChatSend, ChatSendNow, QuestionAnswer, QuestionInfo, QuestionListRequest,
    QueueChange,
};
use crate::checkpoint::{CheckpointDiffRequest, CheckpointListRequest, CheckpointRemove};
use crate::control::{ErrorBody, ErrorCode};
use crate::daemon_settings::DaemonSettingSet;
use crate::error::ProtocolError;
use crate::fs::{
    FsChange, FsFind, FsList, FsRead, FsSearch, FsSearchCancel, FsStat, FsUnwatch, FsWatch,
    FsWriteAbort, FsWriteBegin, FsWriteChunk, FsWriteCommit,
};
use crate::grants::{ControlGrantRevoke, ControlGrantsList};
use crate::history::{HistorySearch, HistorySetEnabled};
use crate::memory::{
    MemoryHistoryRequest, MemoryList, MemoryProposalResolve, MemoryProposalsList, MemoryPropose,
    MemoryScope, MemoryTransition, MemoryUpdate, MemoryWrite,
};
use crate::notify::{ClientFocus, NotificationIntent, NotifyAck, NotifyAct, NotifyMute};
use crate::project::{ProjectInfo, WorkspaceAssign, WorkspaceDelete};
use crate::provider::{
    ChatSetModel, ProviderAdd, ProviderRef, ProviderSetCredential, ProviderUpdate,
};
use crate::recovery::{Recovery, RecoveryAck};
use crate::render::Row;
use crate::resources::{
    AgentInterrupt, ProcessSignal, ResourceSample, ResourcesListRequest, ResourcesSubscribe,
};
use crate::task::{TaskCreate, TaskDelete, TaskListRequest, TaskRef, TaskReview, TaskUpdate};
use crate::tree::{AgentTreeRequest, NodeReport};
use crate::ui::{SessionSendText, UiCommandResult, UiPublish, UiRun, UiSetSetting, UiTarget};
use crate::worktree::{
    Placement, WorktreeCommit, WorktreeCreate, WorktreeDiffRequest, WorktreeFileDiffRequest,
    WorktreeFinish, WorktreeListRequest, WorktreeMerge, WorktreeRemove,
};

/// Parameterless request body; encoded as `{}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Empty {}

// ---------------------------------------------------------------------------------------------
// Requests

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
// `AgentReport` carries a whole node report; requests are built once, serialised, and dropped,
// so boxing it would only complicate every sender.
#[allow(clippy::large_enum_variant)]
pub enum Request {
    #[serde(rename = "hello")]
    Hello(Hello),
    #[serde(rename = "daemon.status")]
    DaemonStatus(Empty),
    #[serde(rename = "daemon.shutdown")]
    DaemonShutdown(DaemonShutdown),
    #[serde(rename = "events.subscribe")]
    EventsSubscribe(Empty),
    #[serde(rename = "workspace.create")]
    WorkspaceCreate(WorkspaceCreate),
    #[serde(rename = "workspace.list")]
    WorkspaceList(Empty),
    #[serde(rename = "workspace.rename")]
    WorkspaceRename(WorkspaceRename),
    #[serde(rename = "workspace.close")]
    WorkspaceClose(WorkspaceRef),
    /// Capability `workspace.projects` (protocol 0.6): the workspace for a folder.
    #[serde(rename = "workspace.open")]
    WorkspaceOpen(WorkspaceOpen),
    /// Capability `workspace.projects` (protocol 0.6).
    #[serde(rename = "workspace.add_root")]
    WorkspaceAddRoot(WorkspaceRoot),
    /// Capability `workspace.projects` (protocol 0.6).
    #[serde(rename = "workspace.remove_root")]
    WorkspaceRemoveRoot(WorkspaceRoot),
    /// Capability `workspace.projects` (protocol 0.6): open and remembered projects.
    #[serde(rename = "workspace.recent")]
    WorkspaceRecent(WorkspaceRecent),
    /// Capability `workspace.projects` (protocol 0.6): manual order.
    #[serde(rename = "workspace.move")]
    WorkspaceMove(WorkspaceMove),
    /// Capability `workspace.projects` (protocol 0.6).
    #[serde(rename = "workspace.pin")]
    WorkspacePin(WorkspacePin),
    /// Capability `projects.auto` (protocol 0.19): puts a project into a workspace or out of it;
    /// answered with the project's `WorkspaceInfo`.
    #[serde(rename = "workspace.assign")]
    WorkspaceAssign(WorkspaceAssign),
    /// Capability `projects.auto`: removes a workspace, its projects stay; answered with
    /// `Empty`.
    #[serde(rename = "workspace.delete")]
    WorkspaceDelete(WorkspaceDelete),
    #[serde(rename = "session.create")]
    SessionCreate(SessionCreate),
    #[serde(rename = "session.list")]
    SessionList(SessionListFilter),
    #[serde(rename = "session.snapshot")]
    SessionSnapshot(Empty),
    #[serde(rename = "session.stop")]
    SessionStop(SessionRef),
    #[serde(rename = "session.restart")]
    SessionRestart(SessionRef),
    #[serde(rename = "session.rename")]
    SessionRename(SessionRename),
    /// Capability `session.close` (protocol 0.30): ends a session and removes it from the
    /// lists (its journal, blocks and history-index records stay); a busy session needs
    /// `stop_running`, else `invalid_state` with the busy data; answered with `Empty`.
    #[serde(rename = "session.close")]
    SessionClose(SessionClose),
    /// Capability `session.close`: brings a closed session back the way restart recovery does;
    /// answered with `SessionResult`.
    #[serde(rename = "session.reopen")]
    SessionReopen(SessionRef),
    /// Capability `session.recovery` (protocol 0.26): dismiss what a daemon restart did to one
    /// session, or to all; answered with `RecoveryAcked`.
    #[serde(rename = "session.recovery_ack")]
    SessionRecoveryAck(RecoveryAck),
    #[serde(rename = "terminal.attach")]
    TerminalAttach(TerminalAttach),
    #[serde(rename = "terminal.detach")]
    TerminalDetach(TerminalRef),
    #[serde(rename = "terminal.resize")]
    TerminalResize(TerminalResize),
    #[serde(rename = "terminal.ack")]
    TerminalAck(TerminalAck),
    #[serde(rename = "terminal.history")]
    TerminalHistory(TerminalHistory),
    /// Capability `terminal.colors` (protocol 0.2).
    #[serde(rename = "terminal.set_colors")]
    TerminalSetColors(TerminalSetColors),
    /// Capability `agent.report` (protocol 0.4): a coding agent reports its state and how to
    /// resume it.
    #[serde(rename = "agent.report")]
    AgentReport(AgentReportRequest),
    /// Capability `agent.report` (protocol 0.4): the agent exited; forget its report.
    #[serde(rename = "agent.release")]
    AgentRelease(AgentReleaseRequest),
    /// Capability `agent.tree` (protocol 0.10): the observation tree of one session, or of all;
    /// answered with `AgentTree`.
    #[serde(rename = "agent.tree")]
    AgentTree(AgentTreeRequest),
    /// Capability `terminal.blocks` (protocol 0.5).
    #[serde(rename = "terminal.blocks")]
    TerminalBlocks(TerminalBlocks),
    /// Capability `terminal.blocks` (protocol 0.5).
    #[serde(rename = "terminal.block_text")]
    TerminalBlockText(TerminalBlockText),
    /// Capability `history.search` (protocol 0.8): search the session history index.
    #[serde(rename = "history.search")]
    HistorySearch(HistorySearch),
    /// Capability `history.search`: index state; answered with `HistoryStatusInfo`.
    #[serde(rename = "history.status")]
    HistoryStatus(Empty),
    /// Capability `history.search`: switch indexing on or off.
    #[serde(rename = "history.set_enabled")]
    HistorySetEnabled(HistorySetEnabled),
    /// Capability `history.search`: delete the index and start an empty one.
    #[serde(rename = "history.clear")]
    HistoryClear(Empty),
    /// Capability `history.search`: read every transcript again.
    #[serde(rename = "history.rebuild")]
    HistoryRebuild(Empty),
    /// Capability `history.sessions` (protocol 0.29): the session catalog, one row per agent
    /// session; answered with `HistorySessions`.
    #[serde(rename = "history.sessions")]
    HistorySessions(crate::history::HistorySessionsRequest),
    /// Capability `chat.codex` (protocol 0.9): open an agent conversation as a session.
    #[serde(rename = "chat.create")]
    ChatCreate(ChatCreate),
    /// Capability `chat.codex`: send the user's message; answered with `ChatSent`.
    #[serde(rename = "chat.send")]
    ChatSend(ChatSend),
    /// Capability `chat.retry` (protocol 0.34): send the last user message again without
    /// journaling it again; answered with `ChatSent`.
    #[serde(rename = "chat.retry")]
    ChatRetry(ChatRetry),
    /// Capability `chat.codex`: stop the turn in progress. Protocol 0.36 (capability
    /// `chat.queue`): also clears the queue unless `keep_queue`.
    #[serde(rename = "chat.interrupt")]
    ChatInterrupt(ChatInterrupt),
    /// Capability `chat.queue` (protocol 0.36): put a message at the end of the chat's queue;
    /// answered with `ChatQueued`.
    #[serde(rename = "chat.queue.add")]
    ChatQueueAdd(ChatQueueAdd),
    /// Capability `chat.queue`: change the text of a queued message; answered with `ChatQueued`.
    #[serde(rename = "chat.queue.edit")]
    ChatQueueEdit(ChatQueueEdit),
    /// Capability `chat.queue`: take a message out of the queue unsent.
    #[serde(rename = "chat.queue.remove")]
    ChatQueueRemove(ChatQueueItemRef),
    /// Capability `chat.queue`: take every message out of the queue unsent.
    #[serde(rename = "chat.queue.clear")]
    ChatQueueClear(SessionRef),
    /// Capability `chat.queue`: the chat's queue; answered with `ChatQueue`.
    #[serde(rename = "chat.queue.list")]
    ChatQueueList(SessionRef),
    /// Capability `chat.queue`: a message that does not wait for the turn (steers it, or
    /// interrupts it and goes next); answered with `ChatSentNow`.
    #[serde(rename = "chat.send_now")]
    ChatSendNow(ChatSendNow),
    /// Capability `chat.codex`: the journal of a session; answered with `ChatHistory`.
    #[serde(rename = "chat.history")]
    ChatHistory(ChatHistoryRequest),
    /// Capability `chat.codex`: the models an agent offers; answered with `ChatModels`.
    #[serde(rename = "chat.models")]
    ChatModels(ChatModelsRequest),
    /// Capability `chat.codex`: permission requests waiting for an answer.
    #[serde(rename = "approval.list")]
    ApprovalList(ApprovalListRequest),
    /// Capability `chat.codex`: answer a permission request with one of its options.
    #[serde(rename = "approval.respond")]
    ApprovalRespond(ApprovalRespond),
    /// Capability `chat.questions` (protocol 0.24): questions of agents waiting for an answer;
    /// answered with `QuestionList`.
    #[serde(rename = "question.list")]
    QuestionList(QuestionListRequest),
    /// Capability `chat.questions`: answer a question (values), decline it or cancel it.
    #[serde(rename = "question.answer")]
    QuestionAnswer(QuestionAnswer),
    /// Capability `chat.direct` (protocol 0.11): the configured model providers; answered with
    /// `ProviderList`.
    #[serde(rename = "provider.list")]
    ProviderList(Empty),
    /// Capability `chat.direct`: configure a provider; answered with `ProviderResult`.
    #[serde(rename = "provider.add")]
    ProviderAdd(ProviderAdd),
    /// Capability `chat.direct`: change a provider's name, base URL or default model.
    #[serde(rename = "provider.update")]
    ProviderUpdate(ProviderUpdate),
    /// Capability `chat.direct`: forget a provider and its stored credential; answered with
    /// `Empty`.
    #[serde(rename = "provider.remove")]
    ProviderRemove(ProviderRef),
    /// Capability `chat.direct`: store the API key in the Keychain. The secret is never echoed
    /// or logged.
    #[serde(rename = "provider.set_credential")]
    ProviderSetCredential(ProviderSetCredential),
    /// Capability `chat.direct`: delete the stored API key.
    #[serde(rename = "provider.clear_credential")]
    ProviderClearCredential(ProviderRef),
    /// Capability `chat.direct`: ask the provider which models it offers; answered with
    /// `ProviderModels`.
    #[serde(rename = "provider.models")]
    ProviderModels(ProviderRef),
    /// Capability `chat.direct`: look for Ollama and LM Studio on this machine (once, on
    /// request); answered with `ProviderDiscovery`.
    #[serde(rename = "provider.discover")]
    ProviderDiscover(Empty),
    /// Capability `chat.direct`: switch the model of a direct chat for its next turn; answered
    /// with `SessionResult`.
    #[serde(rename = "chat.set_model")]
    ChatSetModel(ChatSetModel),
    /// Capability `assistant` (protocol 0.12): the default assistant session of a scope,
    /// created on first use; answered with `SessionResult`.
    #[serde(rename = "assistant.open")]
    AssistantOpen(AssistantOpen),
    /// Capability `assistant`: the assistant sessions; answered with `AssistantList`.
    #[serde(rename = "assistant.list")]
    AssistantList(AssistantListRequest),
    /// Capability `assistant.backend` (protocol 0.20): the backend (OMP, Claude Code, Codex or
    /// a provider) and model assistants of a scope are created with from now on; answered with
    /// `AssistantConfig`.
    #[serde(rename = "assistant.configure")]
    AssistantConfigure(AssistantConfigure),
    /// Capability `daemon.settings` (protocol 0.28): remove the stored `assistant.configure`
    /// choice of a scope so it inherits again; answered with `Empty`. Never allowed to an
    /// agent-originated connection.
    #[serde(rename = "assistant.reset")]
    AssistantReset(AssistantReset),
    /// Capability `daemon.settings` (protocol 0.28): `daemon.toml` as a list of settings;
    /// answered with `DaemonSettings`.
    #[serde(rename = "daemon.settings.get")]
    DaemonSettingsGet(Empty),
    /// Capability `daemon.settings`: change or remove one setting of `daemon.toml`; answered
    /// with `DaemonSettings`. Never allowed to an agent-originated connection.
    #[serde(rename = "daemon.settings.set")]
    DaemonSettingsSet(DaemonSettingSet),
    /// Capability `control.grants` (protocol 0.31): the grants Mawas's `control` approvals
    /// stored ("allow for this chat", "always allow here"); answered with `ControlGrants`.
    /// Never allowed to an agent-originated connection.
    #[serde(rename = "control.grants.list")]
    ControlGrantsList(ControlGrantsList),
    /// Capability `control.grants`: remove one grant; answered with `Empty`. Never allowed to
    /// an agent-originated connection.
    #[serde(rename = "control.grants.revoke")]
    ControlGrantRevoke(ControlGrantRevoke),
    /// Capability `memory` (protocol 0.32): the curated entries of Mawas's memory; answered
    /// with `MemoryEntries`. Read-only.
    #[serde(rename = "memory.list")]
    MemoryList(MemoryList),
    /// Capability `memory`: the revisions of one entry; answered with `MemoryHistory`.
    #[serde(rename = "memory.history")]
    MemoryHistory(MemoryHistoryRequest),
    /// Capability `memory`: the user enters a new entry; answered with `MemoryReceipt`. Never
    /// allowed to an agent-originated connection.
    #[serde(rename = "memory.write")]
    MemoryWrite(MemoryWrite),
    /// Capability `memory`: the user replaces an entry's body at `expected_revision`; answered
    /// with `MemoryReceipt`. Never allowed to an agent-originated connection.
    #[serde(rename = "memory.update")]
    MemoryUpdate(MemoryUpdate),
    /// Capability `memory`: the user archives an entry; answered with `MemoryReceipt`. Never
    /// allowed to an agent-originated connection.
    #[serde(rename = "memory.archive")]
    MemoryArchive(MemoryTransition),
    /// Capability `memory`: the user restores an archived entry; answered with
    /// `MemoryReceipt`. Never allowed to an agent-originated connection.
    #[serde(rename = "memory.restore")]
    MemoryRestore(MemoryTransition),
    /// Capability `memory`: an agent proposes a change; answered with `MemoryProposed` and an
    /// approval for the user. Nothing is applied.
    #[serde(rename = "memory.propose")]
    MemoryPropose(MemoryPropose),
    /// Capability `memory`: the proposals (waiting, and optionally answered); answered with
    /// `MemoryProposals`. Never allowed to an agent-originated connection.
    #[serde(rename = "memory.proposals.list")]
    MemoryProposalsList(MemoryProposalsList),
    /// Capability `memory`: the user's answer to a proposal; answered with
    /// `MemoryProposalResolved`. Never allowed to an agent-originated connection.
    #[serde(rename = "memory.proposals.resolve")]
    MemoryProposalsResolve(MemoryProposalResolve),
    /// Capability `notify.intents` (protocol 0.13): the pending notification intents; answered
    /// with `NotifyList`.
    #[serde(rename = "notify.list")]
    NotifyList(Empty),
    /// Capability `notify.intents`: the intent was seen; answered with `Empty`.
    #[serde(rename = "notify.ack")]
    NotifyAck(NotifyAck),
    /// Capability `notify.intents`: the user chose an action of an intent; answered with
    /// `NotifyActed`.
    #[serde(rename = "notify.act")]
    NotifyAct(NotifyAct),
    /// Capability `notify.intents`: the session this connection's user is looking at;
    /// answered with `Empty`.
    #[serde(rename = "client.focus")]
    ClientFocus(ClientFocus),
    /// Capability `notify.mute` (protocol 0.34): silences a session's notifications, or lets
    /// them through again; answered with `SessionResult`.
    #[serde(rename = "notify.mute")]
    NotifyMute(NotifyMute),
    /// Capability `worktree` (protocol 0.14): a git worktree for a branch; answered with
    /// `WorktreeCreated`.
    #[serde(rename = "worktree.create")]
    WorktreeCreate(WorktreeCreate),
    /// Capability `worktree`: the worktrees of a workspace's repositories; answered with
    /// `WorktreeList`.
    #[serde(rename = "worktree.list")]
    WorktreeList(WorktreeListRequest),
    /// Capability `worktree`: remove a worktree; answered with `WorktreeRemoved`.
    #[serde(rename = "worktree.remove")]
    WorktreeRemove(WorktreeRemove),
    /// Capability `worktree.review` (protocol 0.22): the files a checkout changed against its
    /// base; answered with `WorktreeDiff`. Read-only.
    #[serde(rename = "worktree.diff")]
    WorktreeDiff(WorktreeDiffRequest),
    /// Capability `worktree.review`: the unified diff of one file; answered with
    /// `WorktreeFileDiff`. Read-only.
    #[serde(rename = "worktree.file_diff")]
    WorktreeFileDiff(WorktreeFileDiffRequest),
    /// Capability `worktree.review`: merge a worktree's branch into the main checkout's branch;
    /// answered with `WorktreeMerged`.
    #[serde(rename = "worktree.merge")]
    WorktreeMerge(WorktreeMerge),
    /// Capability `task.review` (protocol 0.25): stage everything in a worktree and commit it;
    /// answered with `WorktreeCommitted`.
    #[serde(rename = "worktree.commit")]
    WorktreeCommit(WorktreeCommit),
    /// Capability `finish` (protocol 0.35, ADR 0020): commit a checkout, merge a worktree into
    /// its target, push; user-only. Answered with `WorktreeFinished`.
    #[serde(rename = "worktree.finish")]
    WorktreeFinish(WorktreeFinish),
    /// Capability `task` (protocol 0.16): a new task; answered with `TaskResult`.
    #[serde(rename = "task.create")]
    TaskCreate(TaskCreate),
    /// Capability `task`: the tasks of a workspace, or of all; answered with `TaskList`.
    #[serde(rename = "task.list")]
    TaskList(TaskListRequest),
    /// Capability `task`: change a task or its state; answered with `TaskResult`.
    #[serde(rename = "task.update")]
    TaskUpdate(TaskUpdate),
    /// Capability `task`: delete a task; answered with `TaskDeleted`.
    #[serde(rename = "task.delete")]
    TaskDelete(TaskDelete),
    /// Capability `task`: run a ready task; answered with `TaskStarted`.
    #[serde(rename = "task.start")]
    TaskStart(TaskRef),
    /// Capability `task.review` (protocol 0.25): accept a done task or ask for changes;
    /// answered with `TaskReviewed`.
    #[serde(rename = "task.review")]
    TaskReview(TaskReview),
    /// Capability `accounts` (protocol 0.17): the logins of the harnesses; answered with
    /// `AccountList`.
    #[serde(rename = "account.list")]
    AccountList(Empty),
    /// Capability `accounts`: start a harness's own login in a terminal session; answered with
    /// `AccountSession`.
    #[serde(rename = "account.login")]
    AccountLogin(AccountLogin),
    /// Capability `accounts`: start a harness's own logout in a terminal session; answered with
    /// `AccountSession`.
    #[serde(rename = "account.logout")]
    AccountLogout(AccountLogout),
    /// Capability `resources` (protocol 0.18): one fresh sample of the processes of the live
    /// sessions and of `hutand`; answered with `ResourceSample`.
    #[serde(rename = "resources.list")]
    ResourcesList(ResourcesListRequest),
    /// Capability `resources`: sample the processes while this connection listens and send it
    /// `resources.updated` events; answered with `ResourcesSubscribed`.
    #[serde(rename = "resources.subscribe")]
    ResourcesSubscribe(ResourcesSubscribe),
    /// Capability `resources`: stop the `resources.updated` events of this connection; answered
    /// with `Empty`.
    #[serde(rename = "resources.unsubscribe")]
    ResourcesUnsubscribe(Empty),
    /// Capability `resources`: SIGKILL a session's whole process group (or a chat session's
    /// harness process and its descendants) now; answered with `SessionResult` once it ended.
    #[serde(rename = "session.kill")]
    SessionKill(SessionRef),
    /// Capability `resources`: signal one process of a session; answered with
    /// `ProcessSignaled`.
    #[serde(rename = "process.signal")]
    ProcessSignal(ProcessSignal),
    /// Capability `resources`: interrupt the main agent of a session or one subagent; answered
    /// with `AgentInterrupted`.
    #[serde(rename = "agent.interrupt")]
    AgentInterrupt(AgentInterrupt),
    /// Capability `control` (protocol 0.21): the desktop publishes its action catalog, settings
    /// catalog and layout (`docs/control.md`); answered with `Empty`.
    #[serde(rename = "ui.publish")]
    UiPublish(UiPublish),
    /// Capability `control`: the action catalog of a window; answered with `UiActionList`.
    #[serde(rename = "ui.actions")]
    UiActions(UiTarget),
    /// Capability `control`: the layout summary of a window; answered with `UiLayoutResult`.
    #[serde(rename = "ui.layout")]
    UiLayout(UiTarget),
    /// Capability `control`: run an action of a window's catalog; answered with `UiRan`.
    #[serde(rename = "ui.run")]
    UiRun(UiRun),
    /// Capability `control`: write a settings layer through a window; answered with `UiRan`.
    #[serde(rename = "ui.set_setting")]
    UiSetSetting(UiSetSetting),
    /// Capability `control`: the desktop's answer to a `ui.command` event; answered with `Empty`.
    #[serde(rename = "ui.command_result")]
    UiCommandResult(UiCommandResult),
    /// Capability `control`: type text into a terminal session; answered with `SessionTextSent`.
    #[serde(rename = "session.send_text")]
    SessionSendText(SessionSendText),
    /// Capability `fs` (protocol 0.33): a chunk of a file's text; answered with `FsFile`.
    #[serde(rename = "fs.read")]
    FsRead(FsRead),
    /// Capability `fs`: starts an atomic write; answered with `FsUpload`. A mutation: an agent
    /// asks first.
    #[serde(rename = "fs.write.begin")]
    FsWriteBegin(FsWriteBegin),
    /// Capability `fs`: the next piece of an upload; answered with `Empty`.
    #[serde(rename = "fs.write.chunk")]
    FsWriteChunk(FsWriteChunk),
    /// Capability `fs`: writes the file; answered with `FsWritten`.
    #[serde(rename = "fs.write.commit")]
    FsWriteCommit(FsWriteCommit),
    /// Capability `fs`: discards an upload; answered with `Empty`.
    #[serde(rename = "fs.write.abort")]
    FsWriteAbort(FsWriteAbort),
    /// Capability `fs`: what a path is; answered with `FsEntry`.
    #[serde(rename = "fs.stat")]
    FsStat(FsStat),
    /// Capability `fs`: the entries of a directory; answered with `FsListing`.
    #[serde(rename = "fs.list")]
    FsList(FsList),
    /// Capability `fs`: fuzzy file finder; answered with `FsFound`.
    #[serde(rename = "fs.find")]
    FsFind(FsFind),
    /// Capability `fs`: text search below a root, with `fs.search.progress` events; answered
    /// with `FsSearchResult` when it ends.
    #[serde(rename = "fs.search")]
    FsSearch(FsSearch),
    /// Capability `fs`: ends a running search; answered with `Empty`.
    #[serde(rename = "fs.search.cancel")]
    FsSearchCancel(FsSearchCancel),
    /// Capability `fs.watch`: `fs.changed` for paths; answered with `FsWatching`.
    #[serde(rename = "fs.watch")]
    FsWatch(FsWatch),
    /// Capability `fs.watch`: releases paths; answered with `FsWatching`.
    #[serde(rename = "fs.unwatch")]
    FsUnwatch(FsUnwatch),
    /// Capability `checkpoint` (protocol 0.37): checkpoints of agent chat turns, newest first;
    /// answered with `CheckpointList`. Read-only.
    #[serde(rename = "checkpoint.list")]
    CheckpointList(CheckpointListRequest),
    /// Capability `checkpoint`: what a checkpoint's turn changed (or what changed since its
    /// start); answered with `CheckpointDiff`. Read-only, bounded like `worktree.diff`.
    #[serde(rename = "checkpoint.diff")]
    CheckpointDiff(CheckpointDiffRequest),
    /// Capability `checkpoint`: delete a checkpoint or a session's checkpoints (refs and rows);
    /// answered with `CheckpointRemoved`. A mutation: an agent asks per `[control]`.
    #[serde(rename = "checkpoint.remove")]
    CheckpointRemove(CheckpointRemove),
}

impl Request {
    /// Splits into `(method, params)` for the control envelope.
    pub fn encode(&self) -> Result<(String, Value), ProtocolError> {
        split_tagged(serde_json::to_value(self)?, "method", "params")
    }

    /// Typed decoding of an envelope's `method` + `params`. Unknown methods and invalid params
    /// map to `bad_request` so the daemon can answer with the request id.
    pub fn decode(method: &str, params: Value) -> Result<Self, ErrorBody> {
        let mut obj = Map::new();
        obj.insert("method".into(), Value::String(method.to_owned()));
        obj.insert("params".into(), params);
        serde_json::from_value(Value::Object(obj))
            .map_err(|e| ErrorBody::new(ErrorCode::BadRequest, format!("`{method}`: {e}")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: ProtocolVersion,
    pub client: ClientInfo,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Last event the client applied, for replay after reconnect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumePoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub kind: ClientKind,
    pub version: String,
    /// Protocol 0.21 (capability `control`): the Hutan session this client runs in (from
    /// `HUTAN_SESSION_ID`), when it does. The daemon verifies that the session exists and is
    /// live; the connection is an agent's (`[control]` of `daemon.toml`) only while that session
    /// is a chat or an agent reported itself in it. A plain shell session is the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

/// The session a client runs in (`ClientInfo.origin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Desktop,
    Cli,
    Notifier,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumePoint {
    pub epoch: Epoch,
    pub seq: Seq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DaemonShutdown {
    /// Stop live sessions first; otherwise shutdown fails with `invalid_state` while any run.
    #[serde(default)]
    pub stop_sessions: bool,
    /// Also remove the stopped sessions from the restore set (only meaningful with
    /// `stop_sessions`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forget: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreate {
    pub name: String,
    /// Folders to add as member projects (an existing project of a folder is reused, a new one
    /// made when there is none); protocol 0.19 (`roots` before, which an older client sends).
    #[serde(default, alias = "roots")]
    pub paths: Vec<PathBuf>,
    /// Existing projects to move into the new workspace (protocol 0.19).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_ids: Vec<WorkspaceId>,
    /// Who asks (protocol 0.6); absent means `user`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<CreatedBy>,
}

/// Who created a workspace (protocol 0.6). `agent` is for programmatic callers acting for a
/// coding agent; the daemon records what the request says and does not verify it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreatedBy {
    #[default]
    User,
    Cli,
    Agent,
}

/// What a workspace is (protocol 0.6). Exactly one workspace is `home`. Protocol 0.19 adds
/// `workspace`: a container the user made, with projects in it (`WorkspaceInfo.parent_id`);
/// `project` is then the automatic kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Home,
    #[default]
    Project,
    Workspace,
}

/// `workspace.open` (protocol 0.6): finds the workspace one of whose roots contains `path`, or
/// creates one for the folder (its git repository root when inside a repository).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceOpen {
    /// An existing directory, absolute.
    pub path: PathBuf,
    /// Applies when a workspace is created; absent means `user`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<CreatedBy>,
}

/// Result of `workspace.open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceOpened {
    pub workspace: WorkspaceInfo,
    /// A workspace was created for the path (`false`: an existing one contains it).
    pub created: bool,
}

/// `workspace.add_root` / `workspace.remove_root` (protocol 0.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRoot {
    pub workspace_id: WorkspaceId,
    pub path: PathBuf,
}

/// `workspace.recent` (protocol 0.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkspaceRecent {
    /// At most [`RECENT_PROJECTS_MAX_LIMIT`] are answered; absent means
    /// [`RECENT_PROJECTS_DEFAULT_LIMIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

pub const RECENT_PROJECTS_DEFAULT_LIMIT: u32 = 20;
pub const RECENT_PROJECTS_MAX_LIMIT: u32 = 50;

/// One project of `workspace.recent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentProject {
    /// The project's primary root (the first of its workspace's roots).
    pub path: PathBuf,
    pub name: String,
    pub last_opened_ms: u64,
    /// The open workspace of the project; absent for a closed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// Result of `workspace.recent`: newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentProjects {
    pub projects: Vec<RecentProject>,
}

/// `workspace.move` (protocol 0.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMove {
    pub workspace_id: WorkspaceId,
    /// Place the workspace right before this one; absent moves it to the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<WorkspaceId>,
}

/// `workspace.pin` (protocol 0.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePin {
    pub workspace_id: WorkspaceId,
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRename {
    pub workspace_id: WorkspaceId,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub workspace_id: WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCreate {
    /// Defaults to the daemon's default workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(flatten)]
    pub spec: SessionSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionSpec {
    Terminal(TerminalSpec),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TerminalSpec {
    /// argv; empty runs the user's login shell.
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Extra environment for this process only; daemon-managed `HUTAN_*` vars win.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<TerminalSize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionListFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRename {
    pub session_id: SessionId,
    pub title: String,
}

/// `session.close` (protocol 0.30, capability `session.close`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionClose {
    pub session_id: SessionId,
    /// Stop the session even when something runs in it; without it a busy session answers
    /// `invalid_state` and `data.busy` says what runs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stop_running: bool,
}

/// What runs in a session that `session.close` refused to close (`ErrorBody.data.busy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBusy {
    /// The command line in the foreground of a terminal (secrets removed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The agent working or blocked in the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// `working` or `blocked` (an agent or a foreground command), `turn` (a chat's reply is
    /// being written), `approval` (a chat waits for a decision).
    pub state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub pixel_width: u16,
    #[serde(default)]
    pub pixel_height: u16,
}

/// The daemon sends a `TerminalRender::Snapshot` binary frame for this terminal before the
/// response, then frames paced by `terminal.ack`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAttach {
    pub terminal_id: TerminalId,
    /// Requests a resize as part of attaching (the PTY has one size shared by all clients).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<TerminalSize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalRef {
    pub terminal_id: TerminalId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalResize {
    pub terminal_id: TerminalId,
    pub size: TerminalSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAck {
    pub terminal_id: TerminalId,
    pub frame: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalHistory {
    pub terminal_id: TerminalId,
    /// 0 = the scrollback line directly above the viewport; larger = older.
    pub from_line: u32,
    pub count: u32,
}

/// The client's default terminal colors, used by the daemon to answer OSC 10/11/12 queries so
/// applications see the theme the user sees. Shared by all clients of the terminal, last writer
/// wins (like `terminal.resize`). Answered with `Empty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSetColors {
    pub terminal_id: TerminalId,
    pub colors: TerminalColors,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalColors {
    pub foreground: (u8, u8, u8),
    pub background: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
}

/// A coding agent running inside a session reports what it is doing and how to start it again
/// (`docs/integrations.md`). Answered with [`AgentApplied`].
///
/// `seq` must increase per `(session_id, source, node)` (`node` is the id in `node`, none for a
/// report without one); a request that does not is ignored (`applied: false`). `resume_argv`,
/// when present, replaces the session's resume reference: a plain command name on `PATH`
/// followed by arguments, at most 64 words and 8 KiB in total, with no apostrophe or control
/// character anywhere. The daemon runs it instead of the session's command after a daemon
/// restart. Absent leaves the stored reference alone.
///
/// Protocol 0.10 (capability `agent.tree`): `node` says which agent of the session's
/// observation tree the report is about. Without it the report is about the main agent, exactly
/// as in protocol 0.4. `state` is that node's state (absent: unchanged, so that a report can
/// carry only a model or an activity); `message`, `agent_session_id` and `resume_argv` belong
/// to the session's main agent and are only read when the report is about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentReportRequest {
    pub session_id: SessionId,
    /// Who reports (one integration, e.g. `claude-hooks`); sequencing is per source.
    pub source: String,
    /// The agent's name (`omp`, `claude`, `codex`).
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<AgentState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub seq: u64,
    /// The agent's own id for the conversation (what its resume command takes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_argv: Option<Vec<String>>,
    /// Protocol 0.10: the node this report is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeReport>,
    /// Protocol 0.18: the process id of the agent this report is about (the main agent, or
    /// `node`), when it runs as a process of the session. The resource manager only acts on it
    /// while it is in the session's process tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// The agent behind `source` is gone: clears its report and its resume reference.
///
/// Protocol 0.10: with `node`, only that node of the observation tree ends (`outcome`, default
/// `completed`), and the report and resume reference stay unless the node is the main agent.
/// Without it the main agent is released, and every subagent still running under it is
/// disconnected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReleaseRequest {
    pub session_id: SessionId,
    pub source: String,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<NodeOutcome>,
}

/// Result of `agent.report` and `agent.release`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentApplied {
    /// `false`: ignored because `seq` was not newer, the session has ended, or (release) the
    /// source held nothing.
    pub applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Waiting for the user.
    Idle,
    /// A turn is in progress.
    Working,
    /// Waiting for a decision (permission prompt, question).
    Blocked,
}

// ---------------------------------------------------------------------------------------------
// Results

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol: ProtocolVersion,
    pub daemon_version: String,
    pub epoch: Epoch,
    pub capabilities: Vec<String>,
    pub resume: ResumeOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeOutcome {
    /// Events after the client's `seq` follow immediately.
    Replayed,
    /// Client must re-fetch snapshots.
    SnapshotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatusInfo {
    pub daemon_version: String,
    pub protocol: ProtocolVersion,
    pub epoch: Epoch,
    pub pid: u32,
    pub uptime_ms: u64,
    pub sessions: u32,
    pub clients: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribed {
    /// Last event already emitted; the stream continues with `seq + 1`.
    pub seq: Seq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: WorkspaceId,
    pub name: String,
    pub roots: Vec<PathBuf>,
    /// Protocol 0.6 (capability `workspace.projects`); the fields below default for older
    /// daemons.
    #[serde(default)]
    pub kind: WorkspaceKind,
    #[serde(default)]
    pub pinned: bool,
    /// Manual order; the list is sorted by it.
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub created_by: CreatedBy,
    /// Latest session activity: a session created, input typed, or output produced (updated
    /// at most once per 10 s).
    #[serde(default)]
    pub last_active_ms: u64,
    #[serde(default)]
    pub live_sessions: u32,
    #[serde(default)]
    pub total_sessions: u32,
    /// Protocol 0.19 (capability `projects.auto`): the workspace a project is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<WorkspaceId>,
    /// Protocol 0.19: what identifies a project; absent for Home and workspaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceList {
    pub workspaces: Vec<WorkspaceInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub kind: SessionKind,
    pub title: String,
    pub state: SessionState,
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub terminal_id: Option<TerminalId>,
    pub created_at_ms: u64,
    /// What the coding agent in this session last reported (capability `agent.report`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentReport>,
    /// The session ended before a daemon restart and its final screen is gone (true only for
    /// ended sessions loaded from storage).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restored: bool,
    /// Present for agent chat sessions (kind `harness`; capability `chat.codex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatInfo>,
    /// Protocol 0.14 (capability `worktree`): the git checkout the session works in; absent
    /// outside a git repository or while its directory is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<Placement>,
    /// Protocol 0.19 (capability `projects.auto`): the folder the session is in now — the
    /// directory the shell reported (OSC 7), else the one it started in; absent while unknown.
    /// The session's project is the one it started in and does not follow this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// Protocol 0.26 (capability `session.recovery`): what the daemon start that restored this
    /// session did to it, until the user dismisses it or acts in the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Recovery>,
    /// Protocol 0.34 (capability `notify.mute`): the user muted the session's notifications
    /// (`notify.mute`): it raises no notification intent. It still counts as needing the user.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
}

/// The latest report of a coding agent running in a session: the main agent, with the state
/// of its subagents rolled in (protocol 0.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReport {
    pub source: String,
    pub agent: String,
    /// `blocked` while the main agent or any subagent waits for the user.
    pub state: AgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    pub updated_at_ms: u64,
    /// Protocol 0.10: the name of the subagent that blocks the session while the main agent
    /// itself does not (then `state` is `blocked` because of it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_sub: Option<String>,
}

/// Result of `session.create`, `.stop`, `.restart`, and `.rename`: the session after the change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResult {
    pub session: SessionInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionInfo>,
    pub seq: Seq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub workspaces: Vec<WorkspaceInfo>,
    pub sessions: Vec<SessionInfo>,
    /// Apply only events with a greater `seq` on top of this snapshot.
    pub seq: Seq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalAttached {
    /// Frame number of the snapshot already sent.
    pub frame: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalHistoryRows {
    pub from_line: u32,
    /// Newest first; may be shorter than requested at the top of history.
    pub rows: Vec<Row>,
}

/// Decodes a response `result` into the type expected for the request that was sent.
pub fn decode_result<T: DeserializeOwned>(value: Value) -> Result<T, ProtocolError> {
    Ok(serde_json::from_value(value)?)
}

// ---------------------------------------------------------------------------------------------
// Events

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
// `SessionUpdated` carries a whole `SessionInfo`; events are built once, serialised, and dropped,
// so boxing it would only complicate every consumer's pattern.
#[allow(clippy::large_enum_variant)]
pub enum Event {
    #[serde(rename = "workspace.updated")]
    WorkspaceUpdated { workspace: WorkspaceInfo },
    #[serde(rename = "workspace.closed")]
    WorkspaceClosed { workspace_id: WorkspaceId },
    #[serde(rename = "session.updated")]
    SessionUpdated { session: SessionInfo },
    #[serde(rename = "session.exited")]
    SessionExited {
        session_id: SessionId,
        code: Option<i32>,
        signal: Option<i32>,
    },
    #[serde(rename = "terminal.bell")]
    TerminalBell { terminal_id: TerminalId },
    #[serde(rename = "terminal.title")]
    TerminalTitle {
        terminal_id: TerminalId,
        title: Option<String>,
    },
    #[serde(rename = "terminal.notification")]
    TerminalNotification {
        terminal_id: TerminalId,
        title: Option<String>,
        body: String,
    },
    /// Capability `terminal.blocks` (protocol 0.5): a block started, changed state or was
    /// truncated; `block` is the whole block as it is now.
    #[serde(rename = "terminal.block")]
    TerminalBlock {
        terminal_id: TerminalId,
        block: Block,
    },
    /// Capability `chat.codex` (protocol 0.9): one event of a chat session, streamed as it
    /// happens. `event.seq` increases per session; deltas are streamed but not journaled.
    #[serde(rename = "chat.event")]
    ChatEvent {
        session_id: SessionId,
        event: Box<AgentEvent>,
    },
    /// Capability `chat.queue` (protocol 0.36): a chat's queue changed; it is carried whole.
    #[serde(rename = "chat.queue")]
    ChatQueueChanged {
        queue: ChatQueue,
        change: QueueChange,
    },
    /// Capability `checkpoint` (protocol 0.37): a turn's checkpoint was recorded (its end, or
    /// why there is none: `failed`, `off`). A turn that changed nothing, or whose folder cannot
    /// have checkpoints, raises none.
    #[serde(rename = "checkpoint.created")]
    CheckpointCreated { checkpoint: Box<CheckpointInfo> },
    /// Capability `checkpoint`: checkpoints were removed (`checkpoint.remove`, or retention).
    #[serde(rename = "checkpoint.removed")]
    CheckpointRemoved { ids: Vec<CheckpointId> },
    /// Capability `checkpoint`: whether a chat's turns get checkpoints changed; the same as its
    /// `ChatInfo.checkpoints`.
    #[serde(rename = "checkpoint.state")]
    CheckpointStateChanged {
        session_id: SessionId,
        state: CheckpointAvailability,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Capability `chat.codex`: an agent asks for permission.
    #[serde(rename = "approval.required")]
    ApprovalRequired { approval: Box<ApprovalInfo> },
    /// Capability `chat.codex`: a permission request is over (answered, or expired because
    /// its turn ended or the agent went away).
    #[serde(rename = "approval.resolved")]
    ApprovalResolved {
        approval_id: ApprovalId,
        session_id: SessionId,
        #[serde(flatten)]
        outcome: PermissionOutcome,
        /// Capability `chat.provenance` (protocol 0.15): the client that answered; absent
        /// when the request expired.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decided_by: Option<DecidedBy>,
    },
    /// Capability `chat.questions` (protocol 0.24): an agent asks the user a question.
    #[serde(rename = "question.required")]
    QuestionRequired { question: Box<QuestionInfo> },
    /// Capability `chat.questions`: a question is over (answered, declined, cancelled, or
    /// expired because its turn ended, the agent went away or the daemon restarted). A secret
    /// answer is never in the event.
    #[serde(rename = "question.resolved")]
    QuestionResolved {
        question_id: QuestionId,
        session_id: SessionId,
        #[serde(flatten)]
        outcome: QuestionOutcome,
        /// The client that answered; absent when the question expired.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decided_by: Option<DecidedBy>,
    },
    /// Capability `agent.tree` (protocol 0.10): nodes of a session's observation tree changed
    /// or were dropped. Coalesced: at most 4 per second per session. `nodes_changed` carries
    /// each node whole, as it is now; a client applies the event to the tree it fetched with
    /// `agent.tree`.
    #[serde(rename = "agent.tree_updated")]
    AgentTreeUpdated {
        session_id: SessionId,
        nodes_changed: Vec<AgentNode>,
        nodes_removed: Vec<String>,
    },
    /// Capability `notify.intents` (protocol 0.13): something deserves the user's attention.
    #[serde(rename = "notify.intent")]
    NotifyIntent { intent: NotificationIntent },
    /// Capability `notify.intents`: the user asked (a notification's "Open") to see a session;
    /// the desktop focuses it. Not a state change: a client that was disconnected when it
    /// went out finds it replayed with an old `at_ms` and ignores it.
    #[serde(rename = "ui.open")]
    UiOpen {
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<WorkspaceId>,
        at_ms: u64,
    },
    /// Capability `worktree` (protocol 0.14): worktrees were created or removed in a workspace's
    /// repositories; clients list them again.
    #[serde(rename = "worktree.changed")]
    WorktreeChanged { workspace_id: WorkspaceId },
    /// Capability `task` (protocol 0.16): a task was created or changed (its state, fields,
    /// sessions, or the state shown for it because a dependency changed); `task` is the whole
    /// task as it is now.
    #[serde(rename = "task.updated")]
    TaskUpdated { task: Task },
    /// Capability `task`: a task was deleted.
    #[serde(rename = "task.removed")]
    TaskRemoved { id: TaskId },
    /// Capability `accounts` (protocol 0.17): a login or logout session of `harness` ended, so
    /// its accounts may have changed; clients list them again.
    #[serde(rename = "account.changed")]
    AccountChanged { harness: String },
    /// Capability `resources` (protocol 0.18): the periodic sample for a connection that
    /// subscribed with `resources.subscribe`. Sent to that connection only, outside the replay
    /// ring: it carries the seq of the last retained event, never advances the sequence, and is
    /// not replayed after a reconnect.
    #[serde(rename = "resources.updated")]
    ResourcesUpdated {
        #[serde(flatten)]
        sample: Box<ResourceSample>,
    },
    /// Capability `control` (protocol 0.21): the daemon asks this desktop to run an action
    /// (`ui.run`) or write a setting (`ui.set_setting`, action `ui::SetSetting`) and expects
    /// `ui.command_result` with the same `request_id`. Sent to one connection only, with a seq of its
    /// own but outside the replay ring: it is never replayed.
    #[serde(rename = "ui.command")]
    UiCommand {
        request_id: u64,
        window_id: String,
        action: String,
        #[serde(default)]
        args: Value,
    },
    /// Capability `session.close` (protocol 0.30): the session was closed (`session.close`) and
    /// is gone from `session.list`; clients drop it. Clients without the capability see it vanish
    /// on their next `session.list`.
    #[serde(rename = "session.closed")]
    SessionClosed { session_id: SessionId },
    /// Capability `memory` (protocol 0.32): the memory of a scope changed (an entry was
    /// written, updated, archived or restored: `scope_revision` is the scope's new counter) or
    /// a proposal arrived or was answered (`scope_revision` is unchanged); clients list again.
    #[serde(rename = "memory.changed")]
    MemoryChanged {
        scope: MemoryScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<WorkspaceId>,
        scope_revision: u64,
    },
    /// Capability `fs.watch` (protocol 0.33): paths this connection watches (`fs.watch`)
    /// changed, gathered for at least 50 ms and at most 500 at a time; `overflow` says more
    /// happened than are listed (rescan). Sent to the watching connections only, outside the
    /// replay ring: it carries the seq of the last retained event and is never replayed.
    #[serde(rename = "fs.changed")]
    FsChanged {
        changes: Vec<FsChange>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        overflow: bool,
    },
    /// Capability `fs.watch`: something below a watched `root` changed (at most once a second
    /// per root): finder and tree caches of that root are stale. Delivered like `fs.changed`.
    #[serde(rename = "fs.generation")]
    FsGeneration { root: PathBuf, generation: u64 },
    /// Capability `fs`: how far the connection's `fs.search` named `search_id` has come. At most
    /// a few a second; delivered like `fs.changed`.
    #[serde(rename = "fs.search.progress")]
    FsSearchProgress {
        search_id: String,
        files_searched: u64,
        hits: u32,
    },
}

impl Event {
    /// Events one connection receives, outside the replay ring and the sequence: they carry the
    /// seq of the last retained event, so "newer than what I applied" cannot be asked of them
    /// and a lagging receiver just waits for the next one.
    pub fn is_connection_local(&self) -> bool {
        matches!(
            self,
            Self::ResourcesUpdated { .. }
                | Self::FsChanged { .. }
                | Self::FsGeneration { .. }
                | Self::FsSearchProgress { .. }
        )
    }

    pub fn encode(&self) -> Result<(String, Value), ProtocolError> {
        split_tagged(serde_json::to_value(self)?, "event", "data")
    }

    /// `Ok(None)` for events this build does not know; clients ignore them (compat rule).
    pub fn decode(event: &str, data: Value) -> Result<Option<Self>, ProtocolError> {
        if !Self::NAMES.contains(&event) {
            return Ok(None);
        }
        let mut obj = Map::new();
        obj.insert("event".into(), Value::String(event.to_owned()));
        obj.insert("data".into(), data);
        Ok(Some(serde_json::from_value(Value::Object(obj))?))
    }

    pub const NAMES: &'static [&'static str] = &[
        "workspace.updated",
        "workspace.closed",
        "session.updated",
        "session.exited",
        "terminal.bell",
        "terminal.title",
        "terminal.notification",
        "terminal.block",
        "chat.event",
        "approval.required",
        "approval.resolved",
        "question.required",
        "question.resolved",
        "agent.tree_updated",
        "notify.intent",
        "ui.open",
        "worktree.changed",
        "task.updated",
        "task.removed",
        "account.changed",
        "resources.updated",
        "ui.command",
        "session.closed",
        "memory.changed",
        "fs.changed",
        "fs.generation",
        "fs.search.progress",
        "chat.queue",
        "checkpoint.created",
        "checkpoint.removed",
        "checkpoint.state",
    ];
}

fn split_tagged(value: Value, tag: &str, content: &str) -> Result<(String, Value), ProtocolError> {
    let Value::Object(mut obj) = value else {
        return Err(ProtocolError::MalformedControl(
            "tagged enum did not encode as object",
        ));
    };
    let Some(Value::String(name)) = obj.remove(tag) else {
        return Err(ProtocolError::MalformedControl("tagged enum missing tag"));
    };
    let body = obj
        .remove(content)
        .unwrap_or_else(|| Value::Object(Map::new()));
    Ok((name, body))
}
