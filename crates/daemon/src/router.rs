//! Dispatches typed [`Request`]s to the services that own the state. Connection tasks decode;
//! the router decides.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use bytes::Bytes;
use hutan_core::{DeliveryActor, TerminalId};
use hutan_protocol::chat::ChatSent;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::grants::ControlGrants;
use hutan_protocol::messages::{
    ClientKind, DaemonStatusInfo, Empty, Request, SessionResult, Subscribed,
};
use hutan_protocol::notify::NotifyList;
use hutan_protocol::task::TaskResult;
use hutan_protocol::{Frame, PROTOCOL_VERSION, ProtocolError};
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::accounts::AccountService;
use crate::assistant::{AssistantService, Origin};
use crate::bus::EventBus;
use crate::conn::{ConnHandle, ConnId};
use crate::control::{Access, ControlService};
use crate::daemon_settings::SettingsService;
use crate::history::HistoryService;
use crate::memory::MemoryService;
use crate::notify::NotifyService;
use crate::providers::ProviderService;
use crate::registry::RegistryHandle;
use crate::resources::{Actor, ResourceService, audit};
use crate::tasks::TaskService;
use crate::terminals::TerminalHub;
use crate::uihub::UiHub;
use crate::worktrees::WorktreeService;
use hutan_protocol::ui::{SessionSendText, SessionTextSent};

/// The services a router hands requests to besides the registry and the terminal hub.
#[derive(Debug)]
pub struct Services {
    pub history: Arc<HistoryService>,
    pub assistant: Arc<AssistantService>,
    pub notify: Arc<NotifyService>,
    pub worktrees: Arc<WorktreeService>,
    pub tasks: Arc<TaskService>,
    pub accounts: Arc<AccountService>,
    pub resources: Arc<ResourceService>,
    pub control: Arc<ControlService>,
    pub memory: Arc<MemoryService>,
    pub ui: Arc<UiHub>,
    pub settings: Arc<SettingsService>,
}

#[derive(Debug)]
pub struct Router {
    registry: RegistryHandle,
    hub: Arc<TerminalHub>,
    history: Arc<HistoryService>,
    assistant: Arc<AssistantService>,
    notify: Arc<NotifyService>,
    worktrees: Arc<WorktreeService>,
    tasks: Arc<TaskService>,
    accounts: Arc<AccountService>,
    resources: Arc<ResourceService>,
    bus: Arc<EventBus>,
    started: Instant,
    shutdown: CancellationToken,
    clients: Arc<AtomicU32>,
    control: Arc<ControlService>,
    memory: Arc<MemoryService>,
    ui: Arc<UiHub>,
    settings: Arc<SettingsService>,
    /// The shutdown in progress asked to drop the stopped sessions from the restore set.
    forget: AtomicBool,
}

impl Router {
    pub fn new(
        registry: RegistryHandle,
        hub: Arc<TerminalHub>,
        services: Services,
        bus: Arc<EventBus>,
        shutdown: CancellationToken,
        clients: Arc<AtomicU32>,
    ) -> Self {
        Self {
            registry,
            hub,
            history: services.history,
            assistant: services.assistant,
            notify: services.notify,
            worktrees: services.worktrees,
            tasks: services.tasks,
            accounts: services.accounts,
            resources: services.resources,
            bus,
            started: Instant::now(),
            shutdown,
            clients,
            control: services.control,
            memory: services.memory,
            ui: services.ui,
            settings: services.settings,
            forget: AtomicBool::new(false),
        }
    }

    /// Runs one request from an already handshaken connection and returns the response result.
    pub async fn dispatch(&self, conn: &ConnHandle, request: Request) -> Result<Value, ErrorBody> {
        // A connection that runs inside an agent's session (a chat, or a terminal an agent
        // reported itself in) is an agent's: the policy of `[control]` decides before anything
        // runs. A plain shell session is the user.
        let Some(origin) = self.agent_origin(conn).await else {
            return self.run(conn, request).await;
        };
        let gated = self
            .control
            .gate(origin, conn.client_kind(), &request)
            .await?;
        let result = self.run(conn, request).await?;
        Ok(gated.annotate(result))
    }

    /// Runs one request, whoever asked.
    async fn run(&self, conn: &ConnHandle, request: Request) -> Result<Value, ErrorBody> {
        match request {
            Request::Hello(_) => Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "hello is only valid as the first request",
            )),
            Request::DaemonStatus(_) => {
                let counts = self.registry.session_counts().await?;
                ok(&DaemonStatusInfo {
                    daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
                    protocol: PROTOCOL_VERSION,
                    epoch: self.bus.epoch(),
                    pid: std::process::id(),
                    uptime_ms: u64::try_from(self.started.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                    sessions: counts.total,
                    clients: self.clients.load(Ordering::Relaxed),
                })
            }
            Request::DaemonShutdown(params) => {
                let counts = self.registry.session_counts().await?;
                if counts.live > 0 && !params.stop_sessions {
                    return Err(ErrorBody::new(
                        ErrorCode::InvalidState,
                        format!(
                            "{} live session(s); pass stop_sessions to shut down anyway",
                            counts.live
                        ),
                    ));
                }
                info!(
                    conn = %conn.id(),
                    stop_sessions = params.stop_sessions,
                    forget = params.forget,
                    "shutdown requested by client"
                );
                self.forget.store(params.forget, Ordering::Release);
                // The connection task queues the response before it next looks at the token,
                // and closing connections flush their queues first.
                self.shutdown.cancel();
                ok(&Empty {})
            }
            Request::EventsSubscribe(_) => ok(&Subscribed {
                seq: self.bus.subscribe(conn),
            }),
            Request::WorkspaceCreate(req) => ok(&self.registry.create_workspace(req).await?),
            Request::WorkspaceList(_) => ok(&self.registry.list_workspaces().await?),
            Request::WorkspaceRename(req) => ok(&self.registry.rename_workspace(req).await?),
            Request::WorkspaceClose(req) => {
                self.registry.close_workspace(req.workspace_id).await?;
                ok(&Empty {})
            }
            Request::WorkspaceOpen(req) => ok(&self.registry.open_workspace(req).await?),
            Request::WorkspaceAddRoot(req) => {
                ok(&self.registry.add_root(req.workspace_id, req.path).await?)
            }
            Request::WorkspaceRemoveRoot(req) => ok(&self
                .registry
                .remove_root(req.workspace_id, req.path)
                .await?),
            Request::WorkspaceRecent(req) => ok(&self.registry.recent_projects(req.limit).await?),
            Request::WorkspaceMove(req) => ok(&self.registry.move_workspace(req).await?),
            Request::WorkspacePin(req) => ok(&self.registry.pin_workspace(req).await?),
            Request::WorkspaceAssign(req) => ok(&self.registry.assign_workspace(req).await?),
            Request::WorkspaceDelete(req) => {
                self.registry.delete_workspace(req).await?;
                ok(&Empty {})
            }
            Request::SessionSnapshot(_) => ok(&self.registry.snapshot().await?),
            Request::SessionCreate(req) => ok(&SessionResult {
                session: self.registry.create_session(req).await?,
            }),
            Request::SessionList(filter) => ok(&self.registry.list_sessions(filter).await?),
            // Stop, restart and rename answer with the session as it is afterwards.
            Request::SessionStop(req) => {
                let stopped = self.registry.stop_session(req.session_id).await;
                audit(
                    Actor::of(conn),
                    "session.stop",
                    &req.session_id.to_string(),
                    &stopped,
                );
                ok(&SessionResult { session: stopped? })
            }
            Request::SessionClose(req) => {
                let closed = self.registry.close_session(req).await;
                audit(
                    Actor::of(conn),
                    "session.close",
                    &req.session_id.to_string(),
                    &closed,
                );
                closed?;
                ok(&Empty {})
            }
            Request::SessionReopen(req) => {
                let reopened = self.registry.reopen_session(req.session_id).await;
                audit(
                    Actor::of(conn),
                    "session.reopen",
                    &req.session_id.to_string(),
                    &reopened,
                );
                ok(&SessionResult { session: reopened? })
            }
            Request::SessionKill(req) => ok(&SessionResult {
                session: self
                    .resources
                    .kill_session(Actor::of(conn), req.session_id)
                    .await?,
            }),
            Request::ProcessSignal(req) => ok(&self.resources.signal(Actor::of(conn), req).await?),
            Request::AgentInterrupt(req) => {
                ok(&self.resources.interrupt(Actor::of(conn), req).await?)
            }
            Request::ResourcesList(req) => ok(&self.resources.list(req.session_id).await?),
            Request::ResourcesSubscribe(req) => {
                ok(&self.resources.subscribe(conn, req.interval_ms).await?)
            }
            Request::ResourcesUnsubscribe(_) => {
                self.resources.unsubscribe(conn.id());
                ok(&Empty {})
            }
            Request::SessionRestart(req) => ok(&SessionResult {
                session: self.registry.restart_session(req.session_id).await?,
            }),
            Request::SessionRename(req) => ok(&SessionResult {
                session: self.registry.rename_session(req).await?,
            }),
            Request::SessionRecoveryAck(req) => ok(&self.registry.ack_recovery(req).await?),
            Request::TerminalAttach(req) => ok(&self.hub.attach(conn, req)?),
            Request::TerminalDetach(req) => {
                self.hub.detach(conn.id(), req.terminal_id)?;
                ok(&Empty {})
            }
            Request::TerminalResize(req) => {
                self.hub.resize(req)?;
                ok(&Empty {})
            }
            Request::TerminalAck(req) => {
                self.hub.ack(conn.id(), req.terminal_id, req.frame)?;
                ok(&Empty {})
            }
            Request::TerminalHistory(req) => ok(&self.hub.history(req)?),
            Request::TerminalBlocks(req) => ok(&self.hub.blocks(req)?),
            Request::TerminalBlockText(req) => ok(&self.hub.block_text(req)?),
            Request::TerminalSetColors(req) => {
                self.hub.set_colors(req)?;
                ok(&Empty {})
            }
            Request::AgentReport(req) => ok(&self.registry.report_agent(req).await?),
            Request::AgentRelease(req) => ok(&self.registry.release_agent(req).await?),
            Request::AgentTree(req) => ok(&self.registry.agent_tree(req.session_id).await?),
            Request::HistorySearch(req) => ok(&self.history.search(req).await?),
            Request::HistoryStatus(_) => ok(&self.history.status().await?),
            // The catalog is built by slice OL-1; until then the capability is not advertised.
            Request::HistorySessions(req) => ok(&self.history.sessions(req).await?),
            Request::HistorySetEnabled(req) => ok(&self.history.set_enabled(req.enabled).await?),
            Request::HistoryClear(_) => {
                self.history.clear().await?;
                ok(&Empty {})
            }
            Request::HistoryRebuild(_) => {
                self.history.rebuild().await?;
                ok(&Empty {})
            }
            Request::ChatCreate(req) => ok(&SessionResult {
                session: self.registry.create_chat(req).await?,
            }),
            Request::ChatSend(req) => {
                self.live_chat(req.session_id).await?;
                // Resolved before anything is journaled or sent: a refused attachment fails
                // the whole message.
                let origin =
                    if !req.attachments.is_empty() && self.agent_origin(conn).await.is_some() {
                        Origin::Agent
                    } else {
                        Origin::User
                    };
                let attached = self
                    .assistant
                    .prepare(req.session_id, &req.attachments, origin)
                    .await?;
                let turn_id = self
                    .registry
                    .chats()
                    .send(req.session_id, req.text, attached)
                    .await?;
                // The user went on with the conversation: what a restart did to it is old news.
                if self.agent_origin(conn).await.is_none() {
                    self.registry.note_user_action(req.session_id);
                }
                ok(&ChatSent { turn_id })
            }
            Request::ChatInterrupt(req) => {
                self.live_chat(req.session_id).await?;
                self.registry.chats().interrupt(req.session_id).await?;
                ok(&Empty {})
            }
            Request::ChatHistory(req) => {
                self.registry.chat_target(req.session_id).await?;
                ok(&self.registry.chats().history(req).await?)
            }
            Request::ChatModels(req) => ok(&self.registry.chats().models(&req.agent).await?),
            Request::ApprovalList(req) => ok(&self.registry.chats().approval_list(req.session_id)),
            Request::ApprovalRespond(req) => {
                self.registry
                    .chats()
                    .respond(req.approval_id, req.option_id, conn.client_kind())
                    .await?;
                ok(&Empty {})
            }
            Request::QuestionList(req) => ok(&self.registry.chats().question_list(req.session_id)),
            Request::QuestionAnswer(req) => {
                self.registry
                    .chats()
                    .answer_question(req.question_id, req.response, conn.client_kind())
                    .await?;
                ok(&Empty {})
            }
            Request::ChatSetModel(req) => ok(&SessionResult {
                session: self.registry.set_chat_model(req).await?,
            }),
            Request::AssistantOpen(req) => ok(&SessionResult {
                session: self.assistant.open(req).await?,
            }),
            Request::AssistantList(req) => ok(&self.assistant.list(req).await?),
            Request::AssistantConfigure(req) => ok(&self.assistant.configure(req).await?),
            Request::AssistantReset(req) => {
                self.assistant.reset(req).await?;
                ok(&Empty {})
            }
            Request::DaemonSettingsGet(_) => ok(&self.settings.get().await?),
            Request::DaemonSettingsSet(req) => ok(&self.settings.set(req).await?),
            Request::ControlGrantsList(req) => ok(&ControlGrants {
                grants: self.control.grants_list(req.workspace_id).await,
            }),
            Request::ControlGrantRevoke(req) => {
                self.control.grant_revoke(req.id)?;
                ok(&Empty {})
            }
            Request::MemoryList(req) => ok(&self.memory.list(req).await?),
            Request::MemoryHistory(req) => ok(&self.memory.history(req).await?),
            Request::MemoryWrite(req) => ok(&self.memory.write(req, conn.client_kind()).await?),
            Request::MemoryUpdate(req) => ok(&self.memory.update(req, conn.client_kind()).await?),
            Request::MemoryArchive(req) => ok(&self
                .memory
                .transition(req, true, conn.client_kind())
                .await?),
            Request::MemoryRestore(req) => ok(&self
                .memory
                .transition(req, false, conn.client_kind())
                .await?),
            Request::MemoryPropose(req) => ok(&self.memory.propose(req, conn.origin()).await?),
            Request::MemoryProposalsList(req) => ok(&self.memory.proposals(req).await?),
            Request::MemoryProposalsResolve(req) => {
                ok(&self.memory.resolve(req, conn.client_kind()).await?)
            }
            Request::NotifyList(_) => ok(&NotifyList {
                intents: self.notify.list(),
            }),
            Request::NotifyAck(req) => {
                self.notify.ack(&req.id)?;
                ok(&Empty {})
            }
            Request::NotifyAct(req) => ok(&self.notify.act(conn.client_kind(), req).await?),
            Request::ClientFocus(req) => {
                self.notify.set_focus(conn.id(), req.session_id);
                ok(&Empty {})
            }
            Request::WorktreeCreate(req) => ok(&self.worktrees.create(req).await?),
            Request::WorktreeList(req) => ok(&self.worktrees.list(req).await?),
            Request::WorktreeRemove(req) => {
                let (result, removed) = self.worktrees.remove(req).await?;
                self.tasks
                    .worktree_removed(&removed, self.actor(conn).await);
                ok(&result)
            }
            Request::WorktreeDiff(req) => ok(&self.worktrees.diff(req).await?),
            Request::WorktreeFileDiff(req) => ok(&self.worktrees.file_diff(req).await?),
            Request::WorktreeMerge(req) => {
                let (merged, workspaces) = self.worktrees.merge(req).await?;
                self.tasks
                    .worktree_merged(&merged, &workspaces, self.actor(conn).await);
                ok(&merged)
            }
            Request::WorktreeCommit(req) => ok(&self.worktrees.commit(req).await?),
            Request::TaskCreate(req) => ok(&TaskResult {
                task: self.tasks.create(req).await?,
            }),
            Request::TaskList(req) => ok(&self.tasks.list(req)),
            Request::TaskUpdate(req) => ok(&TaskResult {
                task: self.tasks.update(req).await?,
            }),
            Request::TaskDelete(req) => ok(&self.tasks.delete(req)?),
            Request::TaskStart(req) => ok(&self.tasks.start(req.id).await?),
            Request::TaskReview(req) => {
                let by = self.actor(conn).await;
                ok(&self.tasks.review(req, by).await?)
            }
            Request::AccountList(_) => ok(&self.accounts.list().await),
            Request::AccountLogin(req) => ok(&self.accounts.login(req).await?),
            Request::AccountLogout(req) => ok(&self.accounts.logout(req).await?),
            Request::ProviderList(_) => ok(&self.providers().list().await?),
            Request::ProviderAdd(req) => ok(&self.providers().add(req).await?),
            Request::ProviderUpdate(req) => ok(&self.providers().update(req).await?),
            Request::ProviderRemove(req) => {
                self.providers().remove(&req.provider_id).await?;
                ok(&Empty {})
            }
            Request::ProviderSetCredential(req) => ok(&self
                .providers()
                .set_credential(&req.provider_id, req.secret)
                .await?),
            Request::ProviderClearCredential(req) => {
                ok(&self.providers().clear_credential(&req.provider_id).await?)
            }
            Request::ProviderModels(req) => ok(&self.providers().models(&req.provider_id).await?),
            Request::ProviderDiscover(_) => ok(&self.providers().discover().await?),
            Request::SessionSendText(req) => ok(&self
                .send_text(req, self.agent_origin(conn).await.is_none())
                .await?),
            Request::UiPublish(req) => {
                self.ui.publish(conn, req)?;
                ok(&Empty {})
            }
            Request::UiActions(req) => ok(&self.ui.actions(&req)?),
            Request::UiLayout(req) => ok(&self.ui.layout(&req)?),
            Request::UiRun(req) => ok(&self.ui.run(req).await?),
            Request::UiSetSetting(req) => ok(&self.ui.set_setting(req).await?),
            Request::UiCommandResult(req) => {
                self.ui.command_result(conn.id(), req)?;
                ok(&Empty {})
            }
        }
    }

    fn providers(&self) -> &Arc<ProviderService> {
        self.registry.chats().providers()
    }

    /// The session must be a chat session with a running agent.
    async fn live_chat(&self, id: hutan_core::SessionId) -> Result<(), ErrorBody> {
        if self.registry.chat_target(id).await? {
            Ok(())
        } else {
            Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("chat session {id} is not running"),
            ))
        }
    }

    /// `session.send_text`: types into the terminal of a live terminal session.
    async fn send_text(
        &self,
        req: SessionSendText,
        by_user: bool,
    ) -> Result<SessionTextSent, ErrorBody> {
        if req.text.len() > hutan_protocol::chat::SEND_MAX_BYTES {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!(
                    "text is {} bytes; at most {} are accepted",
                    req.text.len(),
                    hutan_protocol::chat::SEND_MAX_BYTES
                ),
            ));
        }
        let list = self
            .registry
            .list_sessions(hutan_protocol::messages::SessionListFilter::default())
            .await?;
        let session = list
            .sessions
            .iter()
            .find(|s| s.id == req.session_id)
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("session {} not found", req.session_id),
                )
            })?;
        if session.chat.is_some() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("session {} is a chat: use `chat.send`", session.id),
            ));
        }
        let Some(terminal) = session.terminal_id.filter(|_| session.state.is_live()) else {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("session {} is not running", session.id),
            ));
        };
        let mut bytes = req.text.into_bytes();
        if req.enter {
            bytes.push(b'\r');
        }
        let sent = bytes.len() as u64;
        if self.hub.input(terminal, Bytes::from(bytes)).await {
            self.registry.note_input(terminal, by_user);
        }
        Ok(SessionTextSent { bytes: sent })
    }

    /// Routes a kind=2 frame to its terminal. Only a malformed frame is an error (the caller
    /// closes the connection); unknown or ended terminals just drop the bytes. Waits while the
    /// terminal's input queue is full.
    pub async fn input(&self, conn: &ConnHandle, frame: &Frame) -> Result<(), ProtocolError> {
        let (terminal, bytes): (TerminalId, Bytes) = frame.split_terminal()?;
        // Input frames have no response to carry an approval's outcome: an agent-originated
        // connection types only where policy (or an earlier "allow for this session" of
        // `session.send_text`) lets it.
        let agent = self.agent_origin(conn).await;
        if let Some(origin) = agent
            && !self.control.input_allowed(origin).await
        {
            tracing::debug!(
                conn = %conn.id(),
                terminal = %terminal,
                "terminal input of an agent-originated connection dropped: not allowed by [control]"
            );
            return Ok(());
        }
        if self.hub.input(terminal, bytes).await {
            self.registry.note_input(terminal, agent.is_none());
        }
        Ok(())
    }

    /// Whether `session` exists and is live: what a client's claimed `origin` must satisfy.
    pub async fn origin_is_live(&self, session: hutan_core::SessionId) -> bool {
        self.control.origin_is_live(session).await
    }

    /// The session whose agent this connection is, when it is one: the verified `origin`
    /// while that session is an agent's (see [`crate::control::is_agent_session`]).
    async fn agent_origin(&self, conn: &ConnHandle) -> Option<hutan_core::SessionId> {
        let origin = conn.origin()?;
        self.control.is_agent_origin(origin).await.then_some(origin)
    }

    /// Who a delivery move is recorded for: an agent (ADR 0016), the desktop, or the CLI.
    async fn actor(&self, conn: &ConnHandle) -> DeliveryActor {
        if self.agent_origin(conn).await.is_some() {
            DeliveryActor::Agent
        } else if conn.client_kind() == Some(ClientKind::Desktop) {
            DeliveryActor::Desktop
        } else {
            DeliveryActor::Cli
        }
    }

    /// Drops everything the closed connection had attached.
    pub fn connection_closed(&self, conn: ConnId) {
        self.hub.connection_closed(conn);
        self.notify.connection_closed(conn);
        self.resources.unsubscribe(conn);
        self.ui.connection_closed(conn);
    }

    /// Stops the history indexer, applying what it has queued; the index stays on disk.
    pub async fn stop_history(&self) {
        self.history.shutdown().await;
    }

    /// Terminates every live session (SIGHUP, then SIGKILL after the grace) and waits for
    /// their processes to be gone. They stay in the restore set unless the shutdown request
    /// asked to forget them.
    pub async fn stop_sessions(&self) -> Result<(), ErrorBody> {
        self.registry
            .stop_all(self.forget.load(Ordering::Acquire))
            .await
    }
}

fn ok<T: Serialize>(value: &T) -> Result<Value, ErrorBody> {
    serde_json::to_value(value)
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("encoding result: {e}")))
}

/// Every wire method and how it is treated for an agent-originated connection (ADR 0016): the
/// one table. The match is exhaustive on purpose: a new request that is not classified here
/// does not build.
fn classification(request: &Request) -> (&'static str, Access) {
    use Access::{Mutate, Read, UserOnly};
    match request {
        Request::Hello(_) => ("hello", Read),
        Request::DaemonStatus(_) => ("daemon.status", Read),
        Request::DaemonShutdown(_) => ("daemon.shutdown", Mutate),
        Request::EventsSubscribe(_) => ("events.subscribe", Read),
        Request::WorkspaceCreate(_) => ("workspace.create", Mutate),
        Request::WorkspaceList(_) => ("workspace.list", Read),
        Request::WorkspaceRename(_) => ("workspace.rename", Mutate),
        Request::WorkspaceClose(_) => ("workspace.close", Mutate),
        Request::WorkspaceOpen(_) => ("workspace.open", Mutate),
        Request::WorkspaceAddRoot(_) => ("workspace.add_root", Mutate),
        Request::WorkspaceRemoveRoot(_) => ("workspace.remove_root", Mutate),
        Request::WorkspaceRecent(_) => ("workspace.recent", Read),
        Request::WorkspaceAssign(_) => ("workspace.assign", Mutate),
        Request::WorkspaceDelete(_) => ("workspace.delete", Mutate),
        Request::WorkspaceMove(_) => ("workspace.move", Mutate),
        Request::WorkspacePin(_) => ("workspace.pin", Mutate),
        Request::SessionCreate(_) => ("session.create", Mutate),
        Request::SessionList(_) => ("session.list", Read),
        Request::SessionSnapshot(_) => ("session.snapshot", Read),
        Request::SessionStop(_) => ("session.stop", Mutate),
        Request::SessionRestart(_) => ("session.restart", Mutate),
        Request::SessionRename(_) => ("session.rename", Mutate),
        Request::SessionClose(_) => ("session.close", Mutate),
        Request::SessionReopen(_) => ("session.reopen", Mutate),
        // Dismissing what a restart did is the user's: an agent must not hide it.
        Request::SessionRecoveryAck(_) => ("session.recovery_ack", UserOnly),
        Request::SessionSendText(_) => ("session.send_text", Mutate),
        Request::TerminalAttach(_) => ("terminal.attach", Read),
        Request::TerminalDetach(_) => ("terminal.detach", Read),
        Request::TerminalResize(_) => ("terminal.resize", Mutate),
        Request::TerminalAck(_) => ("terminal.ack", Read),
        Request::TerminalHistory(_) => ("terminal.history", Read),
        Request::TerminalSetColors(_) => ("terminal.set_colors", Mutate),
        // A coding agent describing its own state: what hooks do from inside a session.
        Request::AgentReport(_) => ("agent.report", Read),
        Request::AgentRelease(_) => ("agent.release", Read),
        Request::AgentTree(_) => ("agent.tree", Read),
        Request::TerminalBlocks(_) => ("terminal.blocks", Read),
        Request::TerminalBlockText(_) => ("terminal.block_text", Read),
        Request::HistorySearch(_) => ("history.search", Read),
        Request::HistoryStatus(_) => ("history.status", Read),
        Request::HistorySessions(_) => ("history.sessions", Read),
        Request::HistorySetEnabled(_) => ("history.set_enabled", Mutate),
        Request::HistoryClear(_) => ("history.clear", Mutate),
        Request::HistoryRebuild(_) => ("history.rebuild", Mutate),
        Request::ChatCreate(_) => ("chat.create", Mutate),
        Request::ChatSend(_) => ("chat.send", Mutate),
        Request::ChatInterrupt(_) => ("chat.interrupt", Mutate),
        Request::ChatHistory(_) => ("chat.history", Read),
        Request::ChatModels(_) => ("chat.models", Read),
        Request::ApprovalList(_) => ("approval.list", Read),
        // An agent never answers approvals, its own included.
        Request::ApprovalRespond(_) => ("approval.respond", UserOnly),
        Request::QuestionList(_) => ("question.list", Read),
        // An agent never answers questions, its own included.
        Request::QuestionAnswer(_) => ("question.answer", UserOnly),
        Request::ProviderList(_) => ("provider.list", Read),
        // Provider configuration decides where credentials are sent: the user's.
        Request::ProviderAdd(_) => ("provider.add", UserOnly),
        Request::ProviderUpdate(_) => ("provider.update", UserOnly),
        Request::ProviderRemove(_) => ("provider.remove", UserOnly),
        Request::ProviderSetCredential(_) => ("provider.set_credential", UserOnly),
        Request::ProviderClearCredential(_) => ("provider.clear_credential", UserOnly),
        Request::ProviderModels(_) => ("provider.models", Read),
        Request::ProviderDiscover(_) => ("provider.discover", Read),
        Request::ChatSetModel(_) => ("chat.set_model", Mutate),
        Request::AccountList(_) => ("account.list", Read),
        // Logging in or out changes which credentials a harness holds: the user's.
        Request::AccountLogin(_) => ("account.login", UserOnly),
        Request::AccountLogout(_) => ("account.logout", UserOnly),
        Request::AssistantOpen(_) => ("assistant.open", Mutate),
        Request::AssistantConfigure(_) => ("assistant.configure", Mutate),
        // Forgetting a scope's choice changes what answers the user's assistant: the user's.
        Request::AssistantReset(_) => ("assistant.reset", UserOnly),
        // Reading the settings is free; changing them is never an agent's, whatever
        // `[control] agents` says: an agent must not widen its own permissions (ADR 0016).
        Request::DaemonSettingsGet(_) => ("daemon.settings.get", Read),
        Request::DaemonSettingsSet(_) => ("daemon.settings.set", UserOnly),
        // The grants of Mawas's approvals are the user's: an agent must not read, widen or
        // take away what the user allowed it (ADR 0017).
        Request::ControlGrantsList(_) => ("control.grants.list", UserOnly),
        Request::ControlGrantRevoke(_) => ("control.grants.revoke", UserOnly),
        // Mawas's memory (protocol 0.32): reading is free; what is remembered is the user's
        // (ADR 0017): an agent only proposes, which changes nothing until the user answers.
        Request::MemoryList(_) => ("memory.list", Read),
        Request::MemoryHistory(_) => ("memory.history", Read),
        Request::MemoryWrite(_) => ("memory.write", UserOnly),
        Request::MemoryUpdate(_) => ("memory.update", UserOnly),
        Request::MemoryArchive(_) => ("memory.archive", UserOnly),
        Request::MemoryRestore(_) => ("memory.restore", UserOnly),
        Request::MemoryPropose(_) => ("memory.propose", Read),
        Request::MemoryProposalsList(_) => ("memory.proposals.list", UserOnly),
        Request::MemoryProposalsResolve(_) => ("memory.proposals.resolve", UserOnly),
        Request::AssistantList(_) => ("assistant.list", Read),
        Request::NotifyList(_) => ("notify.list", Read),
        Request::NotifyAck(_) => ("notify.ack", Mutate),
        // Acting on an intent can answer an approval.
        Request::NotifyAct(_) => ("notify.act", UserOnly),
        Request::ClientFocus(_) => ("client.focus", Read),
        Request::WorktreeCreate(_) => ("worktree.create", Mutate),
        Request::WorktreeList(_) => ("worktree.list", Read),
        Request::WorktreeRemove(_) => ("worktree.remove", Mutate),
        // Reading what a checkout changed is free; merging commits in the main checkout.
        Request::WorktreeDiff(_) => ("worktree.diff", Read),
        Request::WorktreeFileDiff(_) => ("worktree.file_diff", Read),
        Request::WorktreeMerge(_) => ("worktree.merge", Mutate),
        // Commits in a worktree an agent may work in, but it changes what a merge takes.
        Request::WorktreeCommit(_) => ("worktree.commit", Mutate),
        Request::TaskCreate(_) => ("task.create", Mutate),
        Request::TaskList(_) => ("task.list", Read),
        Request::TaskUpdate(_) => ("task.update", Mutate),
        Request::TaskDelete(_) => ("task.delete", Mutate),
        Request::TaskStart(_) => ("task.start", Mutate),
        // A verdict on finished work: an agent asks first (ADR 0016), like every mutation.
        Request::TaskReview(_) => ("task.review", Mutate),
        // The resource manager (protocol 0.18): looking is free, ending processes is not.
        Request::ResourcesList(_) => ("resources.list", Read),
        Request::ResourcesSubscribe(_) => ("resources.subscribe", Read),
        Request::ResourcesUnsubscribe(_) => ("resources.unsubscribe", Read),
        Request::SessionKill(_) => ("session.kill", Mutate),
        Request::ProcessSignal(_) => ("process.signal", Mutate),
        Request::AgentInterrupt(_) => ("agent.interrupt", Mutate),
        // The desktop's side of the control surface: an agent must not impersonate a window.
        Request::UiPublish(_) => ("ui.publish", UserOnly),
        Request::UiActions(_) => ("ui.actions", Read),
        Request::UiLayout(_) => ("ui.layout", Read),
        Request::UiRun(_) => ("ui.run", Mutate),
        Request::UiSetSetting(_) => ("ui.set_setting", Mutate),
        Request::UiCommandResult(_) => ("ui.command_result", UserOnly),
    }
}

/// How `request` is treated for an agent-originated connection.
pub fn classify(request: &Request) -> Access {
    classification(request).1
}

/// The wire name of `request`'s method.
pub fn method_name(request: &Request) -> &'static str {
    classification(request).0
}

#[cfg(test)]
mod classification_tests {
    use super::*;

    /// Every committed request fixture decodes to a request whose table name is its wire name,
    /// and the table's exhaustive match (a compile error for an unclassified variant) covers
    /// all of them.
    #[test]
    fn the_table_names_every_fixture_request_by_its_wire_method() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../protocol/tests/fixtures/v0");
        let mut seen = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !(name.starts_with("request_") && name.ends_with(".json")) {
                continue;
            }
            let value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let method = value["method"].as_str().unwrap().to_owned();
            let request = Request::decode(&method, value["params"].clone())
                .unwrap_or_else(|e| panic!("{name}: {}", e.message));
            assert_eq!(method_name(&request), method, "{name}");
            seen.insert(method);
        }
        assert!(
            seen.len() > 60,
            "too few request fixtures found: {}",
            seen.len()
        );
    }

    fn access_of(method: &str, params: serde_json::Value) -> Access {
        classify(&Request::decode(method, params).unwrap())
    }

    #[test]
    fn reads_mutations_and_user_only_requests_are_told_apart() {
        let id = "01K7F3B5N2Q9W8E4R6T1Y0V3KM";
        assert_eq!(
            access_of("session.list", serde_json::json!({})),
            Access::Read
        );
        assert_eq!(
            access_of("session.stop", serde_json::json!({"session_id": id})),
            Access::Mutate
        );
        assert_eq!(
            access_of(
                "approval.respond",
                serde_json::json!({"approval_id": id, "option_id": "allow_once"})
            ),
            Access::UserOnly,
            "an agent never answers approvals"
        );
        assert_eq!(
            access_of(
                "provider.set_credential",
                serde_json::json!({"provider_id": "p", "secret": "s"})
            ),
            Access::UserOnly
        );
        // Reading what a checkout changed is free; merging it into the main checkout asks.
        let path = serde_json::json!({"path": "/work/repo-wt/x"});
        assert_eq!(access_of("worktree.diff", path.clone()), Access::Read);
        assert_eq!(
            access_of(
                "worktree.file_diff",
                serde_json::json!({"path": "/work/repo-wt/x", "file": "a.txt"})
            ),
            Access::Read
        );
        assert_eq!(access_of("worktree.merge", path), Access::Mutate);
        for method in ["resources.list", "resources.unsubscribe"] {
            assert_eq!(
                access_of(method, serde_json::json!({})),
                Access::Read,
                "{method}"
            );
        }
        assert_eq!(
            access_of("resources.subscribe", serde_json::json!({})),
            Access::Read
        );
        for (method, params) in [
            ("session.kill", serde_json::json!({"session_id": id})),
            (
                "process.signal",
                serde_json::json!({"session_id": id, "pid": 7, "signal": "term"}),
            ),
            ("agent.interrupt", serde_json::json!({"session_id": id})),
        ] {
            assert_eq!(access_of(method, params), Access::Mutate, "{method}");
        }
        assert_eq!(
            access_of(
                "ui.run",
                serde_json::json!({"action": "workbench::SplitRight"})
            ),
            Access::Mutate
        );
    }
}
