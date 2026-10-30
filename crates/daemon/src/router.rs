//! Dispatches typed [`Request`]s to the services that own the state. Connection tasks decode;
//! the router decides.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use bytes::Bytes;
use hutan_core::{DeliveryActor, TerminalId};
use hutan_protocol::chat::{ChatQueued, ChatSent, MessageOrigin};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::grants::ControlGrants;
use hutan_protocol::messages::{
    ClientKind, DaemonStatusInfo, Empty, Event, Request, SessionCreate, SessionResult, SessionSpec,
    StopBy, Subscribed,
};
use hutan_protocol::notify::NotifyList;
use hutan_protocol::task::TaskResult;
use hutan_protocol::{Frame, PROTOCOL_VERSION, ProtocolError};
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::accounts::AccountService;
use crate::assistant::{AssistantService, Origin};
use crate::bus::EventBus;
use crate::checkpoints::turns::RestoreContext;
use crate::conn::{ConnHandle, ConnId};
use crate::control::{Access, ControlService};
use crate::daemon_settings::SettingsService;
use crate::files::FileService;
use crate::history::HistoryService;
use crate::hooks::HooksService;
use crate::insight::InsightHub;
use crate::lsp::LspHub;
use crate::memory::MemoryService;
use crate::notify::NotifyService;
use crate::providers::ProviderService;
use crate::registry::RegistryHandle;
use crate::resources::{Actor, ResourceService, audit};
use crate::tasks::TaskService;
use crate::terminals::TerminalHub;
use crate::uihub::UiHub;
use crate::update::UpdateService;
use crate::usage::UsageService;
use crate::worktrees::WorktreeService;
use hutan_protocol::ui::{SessionSendText, SessionTextSent};

mod activity;
mod board;
mod fork;
mod insight;

/// The services a router hands requests to besides the registry and the terminal hub.
#[derive(Debug)]
pub struct Services {
    pub history: Arc<HistoryService>,
    pub assistant: Arc<AssistantService>,
    pub notify: Arc<NotifyService>,
    pub worktrees: Arc<WorktreeService>,
    pub tasks: Arc<TaskService>,
    pub accounts: Arc<AccountService>,
    pub usage: Arc<UsageService>,
    pub resources: Arc<ResourceService>,
    pub control: Arc<ControlService>,
    pub memory: Arc<MemoryService>,
    pub files: Arc<FileService>,
    pub lsp: Arc<LspHub>,
    pub ui: Arc<UiHub>,
    pub hooks: Arc<HooksService>,
    pub insight: Arc<InsightHub>,
    pub updates: Arc<UpdateService>,
    pub settings: Arc<SettingsService>,
    pub storage: hutan_storage::Storage,
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
    usage: Arc<UsageService>,
    resources: Arc<ResourceService>,
    bus: Arc<EventBus>,
    started: Instant,
    shutdown: CancellationToken,
    clients: Arc<AtomicU32>,
    control: Arc<ControlService>,
    memory: Arc<MemoryService>,
    files: Arc<FileService>,
    lsp: Arc<LspHub>,
    ui: Arc<UiHub>,
    hooks: Arc<HooksService>,
    insight: Arc<InsightHub>,
    updates: Arc<UpdateService>,
    settings: Arc<SettingsService>,
    storage: hutan_storage::Storage,
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
            usage: services.usage,
            resources: services.resources,
            bus,
            started: Instant::now(),
            shutdown,
            clients,
            control: services.control,
            memory: services.memory,
            files: services.files,
            lsp: services.lsp,
            ui: services.ui,
            hooks: services.hooks,
            insight: services.insight,
            updates: services.updates,
            settings: services.settings,
            storage: services.storage,
            forget: AtomicBool::new(false),
        }
    }

    /// Runs one request from an already handshaken connection and returns the response result.
    pub async fn dispatch(&self, conn: &ConnHandle, request: Request) -> Result<Value, ErrorBody> {
        // A connection that runs inside an agent's session (a chat, or a terminal an agent
        // reported itself in) is an agent's: the policy of `[control]` decides before anything
        // runs. A plain shell session is the user.
        let Some(origin) = self.agent_origin(conn).await else {
            return self.run(conn, request, None).await;
        };
        // The chunks and the commit of an upload go on with what the begin was allowed to
        // start: one approval covers the write, not one per chunk.
        if self.files.continues_upload(conn.id(), &request) {
            return self.run(conn, request, None).await;
        }
        let gated = self
            .control
            .gate(origin, conn.client_kind(), &request)
            .await?;
        let result = self.run(conn, request, gated.sender.as_ref()).await?;
        Ok(gated.annotate(result))
    }

    /// Runs one request, whoever asked.
    async fn run(
        &self,
        conn: &ConnHandle,
        request: Request,
        sender: Option<&MessageOrigin>,
    ) -> Result<Value, ErrorBody> {
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
                    restart = params.restart,
                    "shutdown requested by client"
                );
                self.forget.store(params.forget, Ordering::Release);
                // Protocol 0.42: every client hears who stopped it before the connections
                // close (they flush their queues first), so none starts it again by itself.
                let by = match conn.client_kind() {
                    Some(ClientKind::Desktop) => StopBy::Desktop,
                    Some(ClientKind::Cli) => StopBy::Cli,
                    _ => StopBy::Other,
                };
                if let Err(error) = self.bus.publish(&Event::DaemonStopping {
                    by,
                    restart: params.restart,
                }) {
                    warn!(%error, "could not announce the shutdown");
                }
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
            Request::TerminalSearch(req) => ok(&self.hub.search(conn.id(), req).await?),
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
            Request::ChatCreate(req) => {
                // An agent placed in a worktree that is still setting up waits for it
                // (`[worktrees] wait_for_setup`).
                self.worktrees.wait_for_setup(&req.cwd).await;
                ok(&SessionResult {
                    session: self.registry.create_chat(req).await?,
                })
            }
            Request::ChatAddDir(req) => {
                let (session, paste) = self.registry.add_dir(req.session_id, req.path).await?;
                self.type_for_the_user(paste).await;
                ok(&SessionResult { session })
            }
            Request::ChatRemoveDir(req) => {
                let (session, paste) = self.registry.remove_dir(req.session_id, req.path).await?;
                self.type_for_the_user(paste).await;
                ok(&SessionResult { session })
            }
            Request::ChatSend(req) => {
                self.live_chat(req.session_id).await?;
                let by = self.actor(conn).await;
                // Resolved before anything is journaled or sent: a refused attachment fails
                // the whole message.
                let origin = if !req.attachments.is_empty() && by == DeliveryActor::Agent {
                    Origin::Agent
                } else {
                    Origin::User
                };
                let attached = self
                    .assistant
                    .prepare(req.session_id, &req.attachments, origin)
                    .await?;
                // The turn this starts re-opens the work of a done task in the sender's name.
                self.tasks.note_sender(req.session_id, by);
                let chats = self.registry.chats();
                // A message Mawas sends (with the user's approval) is not the user's going on
                // with the conversation: it says who sent it and releases nothing.
                let turn_id = match sender {
                    Some(sender) => {
                        chats
                            .send_from(req.session_id, req.text, attached, sender)
                            .await?
                    }
                    None => chats.send(req.session_id, req.text, attached).await?,
                };
                // The user went on with the conversation: what a restart did to it is old news.
                if by != DeliveryActor::Agent {
                    self.registry.note_user_action(req.session_id);
                }
                ok(&ChatSent { turn_id })
            }
            Request::ChatRetry(req) => {
                self.live_chat(req.session_id).await?;
                let by = self.actor(conn).await;
                self.tasks.note_sender(req.session_id, by);
                let turn_id = self.registry.chats().retry(req.session_id).await?;
                if by != DeliveryActor::Agent {
                    self.registry.note_user_action(req.session_id);
                }
                ok(&ChatSent { turn_id })
            }
            Request::ChatFork(req) => ok(&self.fork_chat(conn, req).await?),
            Request::AgentInsight(req) => ok(&self.agent_insight(req).await?),
            Request::AgentsBoard(req) => ok(&self.agents_board(req).await?),
            Request::AgentsActivity(req) => ok(&self.agents_activity(req).await?),
            Request::AgentMeasureContext(req) => ok(&self.measure_context(req).await?),
            Request::ChatInterrupt(req) => {
                self.live_chat(req.session_id).await?;
                self.registry
                    .chats()
                    .interrupt(req.session_id, req.keep_queue)
                    .await?;
                ok(&Empty {})
            }
            Request::ChatQueueAdd(req) => {
                self.live_chat(req.session_id).await?;
                let by = self.actor(conn).await;
                self.tasks.note_sender(req.session_id, by);
                let item = self
                    .registry
                    .chats()
                    .queue_add(req.session_id, req.text, sender.cloned())
                    .await?;
                ok(&ChatQueued { item })
            }
            Request::ChatQueueEdit(req) => {
                self.registry.chat_target(req.session_id).await?;
                let item = self
                    .registry
                    .chats()
                    .queue_edit(req.session_id, req.item_id, req.text)
                    .await?;
                ok(&ChatQueued { item })
            }
            Request::ChatQueueRemove(req) => {
                self.registry.chat_target(req.session_id).await?;
                self.registry
                    .chats()
                    .queue_remove(req.session_id, req.item_id)
                    .await?;
                ok(&Empty {})
            }
            Request::ChatQueueClear(req) => {
                self.registry.chat_target(req.session_id).await?;
                self.registry.chats().queue_clear(req.session_id).await?;
                ok(&Empty {})
            }
            Request::ChatQueueList(req) => {
                self.registry.chat_target(req.session_id).await?;
                ok(&self.registry.chats().queue_list(req.session_id).await?)
            }
            Request::ChatSendNow(req) => {
                self.live_chat(req.session_id).await?;
                let by = self.actor(conn).await;
                self.tasks.note_sender(req.session_id, by);
                let sent = self
                    .registry
                    .chats()
                    .send_now(req.session_id, req.text, req.item_id, sender.cloned())
                    .await?;
                if by != DeliveryActor::Agent {
                    self.registry.note_user_action(req.session_id);
                }
                ok(&sent)
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
            Request::AskSuggest(req) => {
                // Only a Mawas chat suggests, whoever else got this far (the user's own
                // connection, a plain shell): the gate refuses other agents before this.
                let Some(from) = conn.origin() else {
                    return Err(not_mawas());
                };
                if !self.control.is_mawas_origin(from).await {
                    return Err(not_mawas());
                }
                // She does not steer the user's answers to a request of a Mawas session (her
                // own, or another Mawas chat's).
                let asking = self
                    .registry
                    .chats()
                    .check_suggestion(&req.target, &req.answer)?;
                if self.control.is_mawas_origin(asking).await {
                    let mut error = ErrorBody::new(
                        ErrorCode::Forbidden,
                        "Mawas does not suggest answers to the requests of a Mawas session",
                    );
                    error.data = Some(serde_json::json!({"control": {"decision": "mawas_target"}}));
                    return Err(error);
                }
                ok(&self.registry.chats().suggest(from, req)?)
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
            Request::AssistantFocus(req) => ok(&SessionResult {
                session: self.assistant.focus(req).await?,
            }),
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
            Request::MemoryImport(req) => ok(&self.memory.import(req, conn.client_kind()).await?),
            Request::MemoryFilesStatus(req) => ok(&self.memory.files_status(req).await?),
            Request::HooksList(_) => ok(&self.hooks.list()),
            Request::HooksLog(req) => ok(&self.hooks.log(req)),
            Request::HooksTest(req) => ok(&self.hooks.test(req).await?),
            Request::UpdateStatus(_) => ok(&self.updates.status()),
            Request::UpdateCheck(_) => ok(&self.updates.check().await),
            Request::UpdateInstall(req) => ok(&self.updates.install(req).await?),
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
                self.insight.focus(conn, req.session_id);
                ok(&Empty {})
            }
            Request::NotifyMute(req) => {
                let session = self.registry.set_muted(req).await?;
                if session.muted {
                    self.notify.session_muted(session.id);
                }
                ok(&SessionResult { session })
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
            Request::GitLog(req) => ok(&self.worktrees.git_log(req).await?),
            Request::GitBlame(req) => ok(&self.worktrees.git_blame(req).await?),
            Request::GitShow(req) => ok(&self.worktrees.git_show(req).await?),
            Request::GitFile(req) => ok(&self.worktrees.git_file(req).await?),
            Request::WorktreeMerge(req) => {
                let (merged, workspaces) = self.worktrees.merge(req).await?;
                self.tasks
                    .worktree_merged(&merged, &workspaces, self.actor(conn).await);
                ok(&merged)
            }
            Request::WorktreeCommit(req) => ok(&self.worktrees.commit(req).await?),
            Request::ProjectGitInit(req) => {
                let target = req.project_id.to_string();
                let initialised = self.worktrees.git_init(req).await;
                audit(Actor::of(conn), "project.git_init", &target, &initialised);
                ok(&initialised?)
            }
            Request::WorktreeFinish(req) => {
                let finished = self.worktrees.finish(req).await?;
                let by = self.actor(conn).await;
                if let Some((merged, workspaces)) = &finished.merged {
                    self.tasks.worktree_merged(merged, workspaces, by);
                }
                if let Some(commit) = finished.landed() {
                    self.tasks
                        .checkout_finished(&finished.checkout, commit, by)
                        .await;
                }
                self.hooks.finish_done(&finished.result, &finished.checkout);
                ok(&finished.result)
            }
            Request::WorktreeHunks(req) => ok(&self.worktrees.hunks(req).await?),
            Request::WorktreeStage(req) => {
                let target = req.checkout.display().to_string();
                let staged = self.worktrees.change_index(req, true).await;
                audit(Actor::of(conn), "worktree.stage", &target, &staged);
                ok(&staged?)
            }
            Request::WorktreeUnstage(req) => {
                let target = req.checkout.display().to_string();
                let unstaged = self.worktrees.change_index(req, false).await;
                audit(Actor::of(conn), "worktree.unstage", &target, &unstaged);
                ok(&unstaged?)
            }
            Request::WorktreeDiscard(req) => {
                let target = req.checkout.display().to_string();
                let ctx = self.restore_context().await;
                let discarded = self.worktrees.discard(req, ctx).await;
                audit(Actor::of(conn), "worktree.discard", &target, &discarded);
                ok(&discarded?)
            }
            Request::WorktreeSetup(req) => ok(&self.worktrees.setup(req).await?),
            Request::WorktreeSetupTrust(req) => {
                let by = self.actor(conn).await;
                ok(&self.worktrees.setup_trust(req, by.as_str()).await?)
            }
            Request::WorktreeCleanup(req) => ok(&self.worktrees.cleanup(req).await?),
            Request::WorktreePrune(req) => {
                let (pruned, removed) = self.worktrees.prune(req).await?;
                let by = self.actor(conn).await;
                for removed in &removed {
                    self.tasks.worktree_removed(removed, by);
                }
                ok(&pruned)
            }
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
                ok(&self.tasks.review(req, by, sender).await?)
            }
            Request::AccountList(_) => ok(&self.accounts.list().await),
            Request::UsageSummary(req) => ok(&self.usage.summary(req.refresh).await?),
            Request::AccountLogin(req) => ok(&self.accounts.login(req).await?),
            Request::AccountLogout(req) => ok(&self.accounts.logout(req).await?),
            Request::AccountAdd(req) => ok(&self.accounts.add(req).await?),
            Request::AccountRemove(req) => ok(&self.accounts.remove(req).await?),
            Request::AccountDefault(req) => {
                self.accounts.set_default(req).await?;
                ok(&Empty {})
            }
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
            Request::FsRead(req) => {
                let trusted = self.agent_origin(conn).await.is_none();
                ok(&self.files.read(conn.id(), trusted, req).await?)
            }
            Request::FsStat(req) => {
                let trusted = self.agent_origin(conn).await.is_none();
                ok(&self.files.stat(trusted, req).await?)
            }
            Request::FsList(req) => {
                let trusted = self.agent_origin(conn).await.is_none();
                ok(&self.files.list(trusted, req).await?)
            }
            Request::FsFind(req) => ok(&self.files.find(req).await?),
            Request::FsSearch(req) => ok(&self.files.search(conn, req).await?),
            Request::FsSearchCancel(req) => ok(&self.files.search_cancel(conn.id(), &req)),
            Request::FsWriteBegin(req) => {
                let agent = self.agent_origin(conn).await;
                ok(&self.files.write_begin(conn, agent, req).await?)
            }
            Request::FsWriteChunk(req) => ok(&self.files.write_chunk(conn.id(), req).await?),
            Request::FsWriteCommit(req) => {
                let upload = req.upload_id.clone();
                let by_user = self.agent_origin(conn).await.is_none();
                let written = self.files.write_commit(conn, req).await;
                let what = written
                    .as_ref()
                    .map_or(upload, |w| w.path.display().to_string());
                audit(Actor::of(conn), "fs.write", &what, &written);
                if by_user && let Ok(written) = &written {
                    // A save during a turn is a conflict of that turn's restore (`during`).
                    self.registry.chats().checkpoints().saved(&written.path);
                }
                ok(&written?)
            }
            Request::FsWriteAbort(req) => ok(&self.files.write_abort(conn.id(), &req)),
            Request::FsCreate(req) => {
                let agent = self.agent_origin(conn).await;
                let what = req.path.display().to_string();
                let made = self.files.create(conn.id(), agent, req).await;
                audit(Actor::of(conn), "fs.create", &what, &made);
                ok(&made?)
            }
            Request::FsRename(req) => {
                let agent = self.agent_origin(conn).await;
                let what = format!("{} to {}", req.from.display(), req.to.display());
                let renamed = self.files.rename(conn.id(), agent, req).await;
                audit(Actor::of(conn), "fs.rename", &what, &renamed);
                ok(&renamed?)
            }
            Request::FsMove(req) => {
                let agent = self.agent_origin(conn).await;
                let what = format!("{} items", req.items.len());
                let moved = self.files.move_items(conn.id(), agent, req).await;
                audit(Actor::of(conn), "fs.move", &what, &moved);
                ok(&moved?)
            }
            Request::FsCopy(req) => {
                let agent = self.agent_origin(conn).await;
                let what = format!("{} items", req.items.len());
                let copied = self.files.copy(conn.id(), agent, req).await;
                audit(Actor::of(conn), "fs.copy", &what, &copied);
                ok(&copied?)
            }
            Request::FsTrash(req) => {
                let agent = self.agent_origin(conn).await;
                let what = format!("{} paths", req.paths.len());
                let trashed = self.files.trash(conn.id(), agent, req).await;
                audit(Actor::of(conn), "fs.trash", &what, &trashed);
                ok(&trashed?)
            }
            Request::FsRestore(req) => {
                let agent = self.agent_origin(conn).await;
                let what = req.ticket.clone();
                let restored = self.files.restore(conn.id(), agent, req).await;
                audit(Actor::of(conn), "fs.restore", &what, &restored);
                ok(&restored?)
            }
            Request::FsWatch(req) => {
                let agent = self.agent_origin(conn).await;
                ok(&self.files.watch(conn, agent, req).await?)
            }
            Request::FsUnwatch(req) => ok(&self.files.unwatch(conn.id(), req).await?),
            Request::CheckpointList(req) => {
                ok(&self.registry.chats().checkpoints().list(req).await?)
            }
            Request::CheckpointDiff(req) => {
                ok(&self.registry.chats().checkpoints().diff(req).await?)
            }
            Request::CheckpointRemove(req) => {
                let target = match (req.checkpoint_id, req.session_id) {
                    (Some(id), _) => id.to_string(),
                    (None, Some(session)) => session.to_string(),
                    (None, None) => String::new(),
                };
                let removed = self.registry.chats().checkpoints().remove(req).await;
                audit(Actor::of(conn), "checkpoint.remove", &target, &removed);
                ok(&removed?)
            }
            Request::CheckpointPreview(req) => {
                let ctx = self.restore_context().await;
                ok(&self
                    .registry
                    .chats()
                    .checkpoints()
                    .preview(req, &ctx)
                    .await?)
            }
            Request::CheckpointRestore(req) => {
                let target = req.checkpoint_id.to_string();
                let ctx = self.restore_context().await;
                let restored = self.registry.chats().checkpoints().restore(req, ctx).await;
                audit(Actor::of(conn), "checkpoint.restore", &target, &restored);
                ok(&restored?)
            }
            Request::DocOpen(req) => {
                let agent = self.agent_origin(conn).await.is_some();
                ok(&self.lsp.doc_open(conn, agent, req).await?)
            }
            Request::DocChange(req) => {
                self.lsp.doc_change(conn.id(), req)?;
                ok(&Empty {})
            }
            Request::DocSave(req) => {
                self.lsp.doc_save(conn.id(), &req.doc_id)?;
                ok(&Empty {})
            }
            Request::DocClose(req) => {
                self.lsp.doc_close(conn.id(), &req.doc_id)?;
                ok(&Empty {})
            }
            Request::LspStatus(req) => ok(&self.lsp.status(req).await?),
            Request::LspDiagnostics(req) => ok(&self.lsp.diagnostics(req).await?),
            Request::LspHover(req) => ok(&self.lsp.hover(req).await?),
            Request::LspDefinition(req) => ok(&self.lsp.definition(req).await?),
            Request::LspCompletion(req) => ok(&self.lsp.completion(conn.id(), req).await?),
            Request::LspCompletionResolve(req) => {
                ok(&self.lsp.completion_resolve(conn.id(), req).await?)
            }
            Request::LspSignatureHelp(req) => ok(&self.lsp.signature_help(conn.id(), req).await?),
            Request::LspRestart(req) => {
                let restarted = self.lsp.restart(&req.server_id).await;
                audit(Actor::of(conn), "lsp.restart", &req.server_id, &restarted);
                ok(&restarted?)
            }
            Request::LspStop(req) => {
                let stopped = self.lsp.stop(&req.server_id).await;
                audit(Actor::of(conn), "lsp.stop", &req.server_id, &stopped);
                ok(&stopped?)
            }
            Request::LspTrust(req) => {
                let folder = req.folder.display().to_string();
                let decided = self.lsp.set_trust(client_label(conn), req).await;
                audit(Actor::of(conn), "lsp.trust", &folder, &decided);
                ok(&decided?)
            }
        }
    }

    fn providers(&self) -> &Arc<ProviderService> {
        self.registry.chats().providers()
    }

    /// What a preview or a restore of checkpoints needs from the rest of the daemon: the
    /// sessions (who works in the checkout, whose turn runs) and the file service's watchers,
    /// told of each file a restore changes.
    async fn restore_context(&self) -> RestoreContext {
        let sessions = self
            .registry
            .list_sessions(hutan_protocol::messages::SessionListFilter::default())
            .await
            .map(|list| list.sessions)
            .unwrap_or_default();
        let files = Arc::clone(&self.files);
        RestoreContext {
            sessions,
            announce: Arc::new(move |checkpoint, path| files.restoring(path, checkpoint)),
        }
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

    /// Types `paste` at the prompt of a terminal agent, without Enter: the user confirms
    /// (`chat.add_dir` on a terminal session, protocol 0.57). Typed by the user's request, so
    /// the input counts as the user's.
    async fn type_for_the_user(&self, paste: Option<crate::registry::Paste>) {
        if let Some(paste) = paste
            && self
                .hub
                .input(paste.terminal, Bytes::from(paste.text.into_bytes()))
                .await
        {
            self.registry.note_input(paste.terminal, true);
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
        self.insight.connection_closed(conn);
        self.resources.unsubscribe(conn);
        self.ui.connection_closed(conn);
        self.files.connection_closed(conn);
        self.lsp.connection_closed(conn);
    }

    /// Stops the language servers: the daemon is ending.
    pub async fn stop_lsp(&self) {
        self.lsp.shutdown().await;
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

/// The client kind as a decision records it (`lsp.trust`): `desktop`, `cli`, ….
fn client_label(conn: &ConnHandle) -> &'static str {
    match conn.client_kind() {
        Some(ClientKind::Desktop) => "desktop",
        Some(ClientKind::Cli) => "cli",
        Some(ClientKind::Notifier) => "notifier",
        _ => "client",
    }
}

/// What `ask.suggest` answers a connection that is not a Mawas chat.
fn not_mawas() -> ErrorBody {
    let mut error = ErrorBody::new(
        ErrorCode::Forbidden,
        "`ask.suggest` is for Mawas, the user's assistant: nobody else suggests answers",
    );
    error.data = Some(serde_json::json!({"control": {"decision": "not_mawas"}}));
    error
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
        // Putting a folder under version control (protocol 0.43) is the user's decision alone
        // (it writes `.git` and `.gitignore` in the user's folder): an agent never does it,
        // whatever `[control]` says.
        Request::ProjectGitInit(_) => ("project.git_init", UserOnly),
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
        Request::TerminalSearch(_) => ("terminal.search", Read),
        Request::HistorySearch(_) => ("history.search", Read),
        Request::HistoryStatus(_) => ("history.status", Read),
        Request::HistorySessions(_) => ("history.sessions", Read),
        Request::HistorySetEnabled(_) => ("history.set_enabled", Mutate),
        Request::HistoryClear(_) => ("history.clear", Mutate),
        Request::HistoryRebuild(_) => ("history.rebuild", Mutate),
        Request::ChatCreate(_) => ("chat.create", Mutate),
        Request::ChatSend(_) => ("chat.send", Mutate),
        // Adding a folder widens what an agent may touch: only the user does it (ADR 0023).
        Request::ChatAddDir(_) => ("chat.add_dir", UserOnly),
        Request::ChatRemoveDir(_) => ("chat.remove_dir", UserOnly),
        // Sends the same message again: what `chat.send` would do.
        Request::ChatRetry(_) => ("chat.retry", Mutate),
        // A new chat that starts with another's conversation: what `chat.create` would do.
        Request::ChatFork(_) => ("chat.fork", Mutate),
        // What the agent's context holds: read-only. `/context` is a turn sent to the agent, so
        // asking for it is what `chat.send` would be.
        Request::AgentInsight(_) => ("agent.insight", Read),
        // The board of every agent (protocol 0.63): the reducer of the Agents page, read-only.
        Request::AgentsBoard(_) => ("agents.board", Read),
        // What happened in a window (protocol 0.65): read-only, from what the daemon records.
        Request::AgentsActivity(_) => ("agents.activity", Read),
        Request::AgentMeasureContext(_) => ("agent.measure_context", Mutate),
        Request::ChatInterrupt(_) => ("chat.interrupt", Mutate),
        // The queue (protocol 0.36): what `chat.send` would do, later or now.
        Request::ChatQueueAdd(_) => ("chat.queue.add", Mutate),
        Request::ChatQueueEdit(_) => ("chat.queue.edit", Mutate),
        Request::ChatQueueRemove(_) => ("chat.queue.remove", Mutate),
        Request::ChatQueueClear(_) => ("chat.queue.clear", Mutate),
        Request::ChatQueueList(_) => ("chat.queue.list", Read),
        Request::ChatSendNow(_) => ("chat.send_now", Mutate),
        Request::ChatHistory(_) => ("chat.history", Read),
        Request::ChatModels(_) => ("chat.models", Read),
        Request::ApprovalList(_) => ("approval.list", Read),
        // An agent never answers approvals, its own included.
        Request::ApprovalRespond(_) => ("approval.respond", UserOnly),
        Request::QuestionList(_) => ("question.list", Read),
        // An agent never answers questions, its own included.
        Request::QuestionAnswer(_) => ("question.answer", UserOnly),
        // Advisory only: it attaches words to a card, answers nothing, and only Mawas asks.
        Request::AskSuggest(_) => ("ask.suggest", Mutate),
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
        // Reading what the harnesses report and what turns used; nothing it says is a secret.
        Request::UsageSummary(_) => ("usage.summary", Read),
        // Logging in or out changes which credentials a harness holds: the user's.
        Request::AccountLogin(_) => ("account.login", UserOnly),
        Request::AccountLogout(_) => ("account.logout", UserOnly),
        // Which login a harness runs as, and deleting one, is the user's: an agent must not
        // spend (or remove) another subscription.
        Request::AccountAdd(_) => ("account.add", UserOnly),
        Request::AccountRemove(_) => ("account.remove", UserOnly),
        Request::AccountDefault(_) => ("account.default", UserOnly),
        Request::AssistantOpen(_) => ("assistant.open", Mutate),
        Request::AssistantConfigure(_) => ("assistant.configure", Mutate),
        // Forgetting a scope's choice changes what answers the user's assistant: the user's.
        Request::AssistantReset(_) => ("assistant.reset", UserOnly),
        // A Focus tells Mawas which workspace to look at, and adds that workspace's notes and
        // instructions to what she is given: the user's choice, never hers (ADR 0025).
        Request::AssistantFocus(_) => ("assistant.focus", UserOnly),
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
        Request::MemoryImport(_) => ("memory.import", UserOnly),
        Request::MemoryFilesStatus(_) => ("memory.files.status", UserOnly),
        Request::AssistantList(_) => ("assistant.list", Read),
        // A hook's command line can carry a token, its log what the command printed, and a test
        // runs the user's command: all the user's (protocol 0.48). There is no request that
        // adds or changes a hook: they live in the user's `daemon.toml`.
        Request::HooksList(_) => ("hooks.list", UserOnly),
        Request::HooksLog(_) => ("hooks.log", UserOnly),
        // What is known about updates, without contacting anything.
        Request::UpdateStatus(_) => ("update.status", Read),
        // A request to GitHub on the user's behalf: what a mutation is, for an agent.
        Request::UpdateCheck(_) => ("update.check", Mutate),
        // Replacing Hutan runs Homebrew in a terminal: an agent can never update or downgrade
        // it (ADR 0024).
        Request::UpdateInstall(_) => ("update.install", UserOnly),
        Request::HooksTest(_) => ("hooks.test", UserOnly),
        Request::NotifyList(_) => ("notify.list", Read),
        Request::NotifyAck(_) => ("notify.ack", Mutate),
        // Acting on an intent can answer an approval.
        Request::NotifyAct(_) => ("notify.act", UserOnly),
        Request::ClientFocus(_) => ("client.focus", Read),
        // What the user is told about is the user's: an agent must not silence its own
        // approvals (protocol 0.34).
        Request::NotifyMute(_) => ("notify.mute", UserOnly),
        Request::WorktreeCreate(_) => ("worktree.create", Mutate),
        Request::WorktreeList(_) => ("worktree.list", Read),
        Request::WorktreeRemove(_) => ("worktree.remove", Mutate),
        // Reading what a checkout changed is free; merging commits in the main checkout.
        Request::WorktreeDiff(_) => ("worktree.diff", Read),
        Request::WorktreeFileDiff(_) => ("worktree.file_diff", Read),
        // History is read-only: what the repository already records.
        Request::GitLog(_) => ("git.log", Read),
        Request::GitBlame(_) => ("git.blame", Read),
        Request::GitShow(_) => ("git.show", Read),
        Request::GitFile(_) => ("git.file", Read),
        Request::WorktreeMerge(_) => ("worktree.merge", Mutate),
        // Commits in a worktree an agent may work in, but it changes what a merge takes; taking
        // only what the user staged is the user's (ADR 0020 § Amendment 2026-10-26).
        Request::WorktreeCommit(r) if r.staged_only => ("worktree.commit", UserOnly),
        Request::WorktreeCommit(_) => ("worktree.commit", Mutate),
        // Staging, unstaging and discarding are the user's, like Finish: an agent's own tools
        // are git in its shell and its file edits (ADR 0020).
        Request::WorktreeHunks(_) => ("worktree.hunks", Read),
        Request::WorktreeStage(_) => ("worktree.stage", UserOnly),
        Request::WorktreeUnstage(_) => ("worktree.unstage", UserOnly),
        Request::WorktreeDiscard(_) => ("worktree.discard", UserOnly),
        // The user's Finish commits, merges and pushes to a remote: never an agent's, whatever
        // `[control]` says (ADR 0020). Agents keep their own tools.
        Request::WorktreeFinish(_) => ("worktree.finish", UserOnly),
        // Running the commands a repository asks for again: a change an agent asks about first.
        Request::WorktreeSetup(_) => ("worktree.setup", Mutate),
        // Confirming that those commands may run is the user's alone, whatever `[control]` says:
        // an agent that could confirm its own setup would run anything the repository holds.
        Request::WorktreeSetupTrust(_) => ("worktree.setup_trust", UserOnly),
        Request::WorktreeCleanup(_) => ("worktree.cleanup", Read),
        // Removing worktrees in bulk, teardown commands included, is the user's.
        Request::WorktreePrune(_) => ("worktree.prune", UserOnly),
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
        // The file service (protocol 0.33): reading, listing, finding, searching and watching
        // are free. Writing replaces a file the user may have open: an agent asks, once per
        // write: the begin asks, and the chunks and the commit that follow it are covered by
        // its approval (`FileService::continues_upload`).
        Request::FsRead(_) => ("fs.read", Read),
        Request::FsStat(_) => ("fs.stat", Read),
        Request::FsList(_) => ("fs.list", Read),
        Request::FsFind(_) => ("fs.find", Read),
        Request::FsSearch(_) => ("fs.search", Read),
        Request::FsSearchCancel(_) => ("fs.search.cancel", Read),
        Request::FsWatch(_) => ("fs.watch", Read),
        Request::FsUnwatch(_) => ("fs.unwatch", Read),
        // Checkpoints (protocol 0.37): reading them is free; removing one is a change an agent
        // asks for under `[control]`. A preview is a read (protocol 0.38); restoring files is
        // the user's alone, whatever `[control]` says: an agent must not unwind the user's work
        // (ADR 0019 Decision 10).
        Request::CheckpointList(_) => ("checkpoint.list", Read),
        Request::CheckpointDiff(_) => ("checkpoint.diff", Read),
        Request::CheckpointRemove(_) => ("checkpoint.remove", Mutate),
        Request::CheckpointPreview(_) => ("checkpoint.preview", Read),
        Request::CheckpointRestore(_) => ("checkpoint.restore", UserOnly),
        // File operations (protocol 0.54): all six change the disk; an agent asks.
        Request::FsCreate(_) => ("fs.create", Mutate),
        Request::FsRename(_) => ("fs.rename", Mutate),
        Request::FsMove(_) => ("fs.move", Mutate),
        Request::FsCopy(_) => ("fs.copy", Mutate),
        Request::FsTrash(_) => ("fs.trash", Mutate),
        Request::FsRestore(_) => ("fs.restore", Mutate),
        Request::FsWriteBegin(_) => ("fs.write.begin", Mutate),
        Request::FsWriteChunk(_) => ("fs.write.chunk", Mutate),
        Request::FsWriteCommit(_) => ("fs.write.commit", Mutate),
        // Throwing away an upload of its own connection changes nothing on disk.
        Request::FsWriteAbort(_) => ("fs.write.abort", Read),
        // Language servers (protocol 0.39): a document mirror holds what its sender sends and
        // changes nothing on disk, so opening, editing and closing one is no mutation; an
        // agent may only open documents inside the workspace folders. Looking is free.
        // Starting or stopping a server uses the user's memory: an agent asks. Trusting a
        // folder lets a server run the project's build scripts: the user's alone, whatever
        // `[control]` says (ADR 0012 amendment 5).
        Request::DocOpen(_) => ("doc.open", Read),
        Request::DocChange(_) => ("doc.change", Read),
        Request::DocSave(_) => ("doc.save", Read),
        Request::DocClose(_) => ("doc.close", Read),
        Request::LspStatus(_) => ("lsp.status", Read),
        Request::LspDiagnostics(_) => ("lsp.diagnostics", Read),
        Request::LspHover(_) => ("lsp.hover", Read),
        Request::LspDefinition(_) => ("lsp.definition", Read),
        Request::LspCompletion(_) => ("lsp.completion", Read),
        Request::LspCompletionResolve(_) => ("lsp.completion_resolve", Read),
        Request::LspSignatureHelp(_) => ("lsp.signature_help", Read),
        Request::LspRestart(_) => ("lsp.restart", Mutate),
        Request::LspStop(_) => ("lsp.stop", Mutate),
        Request::LspTrust(_) => ("lsp.trust", UserOnly),
    }
}

/// How `request` is treated for an agent-originated connection.
pub fn classify(request: &Request) -> Access {
    match request {
        // Creating a session with folders of its own is the same widening as adding them
        // (ADR 0023 decision 4); without folders the request keeps its class.
        Request::ChatCreate(req) if !req.extra_dirs.is_empty() => Access::UserOnly,
        // Putting the files of a folder back to a turn is `checkpoint.restore`: user-only.
        Request::ChatFork(req) if req.restore_files => Access::UserOnly,
        Request::SessionCreate(SessionCreate {
            spec: SessionSpec::Terminal(spec),
            ..
        }) if !spec.extra_dirs.is_empty() => Access::UserOnly,
        _ => classification(request).1,
    }
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
        // History is read-only too.
        for (method, params) in [
            (
                "git.log",
                serde_json::json!({"path": "/work/repo", "limit": 5}),
            ),
            ("git.blame", serde_json::json!({"path": "/work/repo/a.rs"})),
            (
                "git.show",
                serde_json::json!({"path": "/work/repo", "rev": "HEAD"}),
            ),
            (
                "git.file",
                serde_json::json!({"path": "/work/repo/a.rs", "rev": "HEAD"}),
            ),
        ] {
            assert_eq!(access_of(method, params), Access::Read, "{method}");
        }
        assert_eq!(access_of("worktree.merge", path), Access::Mutate);
        assert_eq!(
            access_of(
                "worktree.finish",
                serde_json::json!({"checkout": "/work/repo", "message": "x", "push": true})
            ),
            Access::UserOnly,
            "an agent never finishes: it would commit and push"
        );
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
        assert_eq!(
            access_of(
                "notify.mute",
                serde_json::json!({"session_id": id, "muted": true})
            ),
            Access::UserOnly,
            "an agent never silences what the user is told"
        );
    }

    /// The file service (protocol 0.33): looking is free, writing asks.
    #[test]
    fn files_are_free_to_read_and_ask_to_write() {
        let path = serde_json::json!({"path": "/repo/a.rs"});
        for (method, params) in [
            ("fs.read", path.clone()),
            ("fs.stat", path.clone()),
            ("fs.list", serde_json::json!({"dir": "/repo"})),
            (
                "fs.find",
                serde_json::json!({"root": "/repo", "query": "a"}),
            ),
            (
                "fs.search",
                serde_json::json!({"search_id": "s", "root": "/repo", "pattern": "a"}),
            ),
            ("fs.search.cancel", serde_json::json!({"search_id": "s"})),
            ("fs.watch", serde_json::json!({"paths": []})),
            ("fs.unwatch", serde_json::json!({"paths": []})),
            ("fs.write.abort", serde_json::json!({"upload_id": "u"})),
        ] {
            assert_eq!(access_of(method, params), Access::Read, "{method}");
        }
        for (method, params) in [
            ("fs.write.begin", path),
            (
                "fs.write.chunk",
                serde_json::json!({"upload_id": "u", "seq": 0, "text": "x"}),
            ),
            (
                "fs.write.commit",
                serde_json::json!({"upload_id": "u", "total_bytes": 1}),
            ),
        ] {
            assert_eq!(access_of(method, params), Access::Mutate, "{method}");
        }
    }
}
