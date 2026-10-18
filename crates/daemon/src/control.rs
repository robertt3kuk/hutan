//! Who is asking, and what an agent may change (protocol 0.21, ADR 0016, `docs/control.md`).
//!
//! A connection whose client says it runs in a Hutan session (`ClientInfo.origin`, verified at
//! `hello`: the session is live) is **agent-originated** while that session is an agent's: a
//! chat/harness session, or a terminal in which an agent reported itself (`SessionInfo.agent`)
//! and has not released itself. A plain shell session without an agent is the user typing
//! (`hutan stop` in a Hutan terminal is not gated). Every request is classified once
//! ([`Access`], in `router::classify`); for an agent-originated connection:
//!
//! - reads always run;
//! - mutations follow `[control] agents`: `ask` raises an approval (kind `control`, answered
//!   through the same machinery as an agent's tool approvals: `approval.list`,
//!   `approval.respond`, notification intents, the desktop's cards), `allow` runs them,
//!   `read_only` refuses them with `forbidden`;
//! - requests only the user may make (answering approvals, credentials and provider
//!   configuration, the desktop's own `ui.publish`) are refused whatever the policy says.
//!
//! Every mutation or refusal of an agent is written to the `approvals` audit table.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use hutan_core::agent::{DecidedBy, PermissionKind, PermissionOption, PermissionOptionKind};
use hutan_core::{ApprovalId, SessionId, SessionKind};
use hutan_protocol::chat::ApprovalInfo;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{ClientKind, Event, Request, SessionInfo, SessionListFilter};
use hutan_storage::{ApprovalOutcome, ApprovalRow, Storage};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::bus::EventBus;
use crate::chat::ApprovalSource;
use crate::config::{AgentPolicy, ControlSettings};
use crate::registry::{self, RegistryHandle};

/// The option that runs the request this once.
pub const ALLOW_ONCE: &str = "allow_once";
/// The option that also lets this session make the same kind of request without asking again.
pub const ALLOW_SESSION: &str = "allow_session";
/// The option that refuses.
pub const DENY: &str = "deny";

/// How a request is treated for an agent-originated connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Reads state, or only touches the caller's own connection: always allowed.
    Read,
    /// Changes something: follows `[control] agents`.
    Mutate,
    /// Only the user may do it: refused for agents under every policy.
    UserOnly,
}

/// What the gate decided for an allowed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gated {
    /// The request waited for the user, who chose this option.
    pub approved: Option<ApprovalNote>,
}

/// An approval a request waited for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalNote {
    pub option_id: String,
    pub waited_ms: u64,
}

impl Gated {
    /// Adds `control: {decision, waited_ms}` to an object result when the request waited for
    /// the user, so a caller (the MCP server) can say so.
    pub fn annotate(&self, mut result: Value) -> Value {
        if let (Some(note), Value::Object(map)) = (&self.approved, &mut result) {
            map.insert(
                "control".to_owned(),
                json!({"decision": note.option_id, "waited_ms": note.waited_ms}),
            );
        }
        result
    }
}

#[derive(Debug)]
struct PendingControl {
    info: ApprovalInfo,
    answer: Option<oneshot::Sender<(String, &'static str)>>,
}

#[derive(Debug, Default)]
struct State {
    pending: HashMap<ApprovalId, PendingControl>,
    /// `allow_session` decisions: (origin session, grant key of the request).
    grants: HashSet<(SessionId, String)>,
}

/// The policy gate and the `control` approvals.
#[derive(Debug)]
pub struct ControlService {
    settings: parking_lot::RwLock<ControlSettings>,
    bus: Arc<EventBus>,
    storage: Storage,
    registry: RegistryHandle,
    shutdown: CancellationToken,
    state: Mutex<State>,
}

fn lock(mutex: &Mutex<State>) -> MutexGuard<'_, State> {
    // The guarded data is never left half-updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn client_name(kind: Option<ClientKind>) -> &'static str {
    match kind {
        Some(ClientKind::Desktop) => "desktop",
        Some(ClientKind::Cli) => "cli",
        Some(ClientKind::Notifier) => "notifier",
        Some(ClientKind::Other) | None => "other",
    }
}

impl ControlService {
    pub fn new(
        settings: ControlSettings,
        bus: Arc<EventBus>,
        storage: Storage,
        registry: RegistryHandle,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        let service = Arc::new(Self {
            settings: parking_lot::RwLock::new(settings),
            bus,
            storage,
            registry: registry.clone(),
            shutdown,
            state: Mutex::new(State::default()),
        });
        let source: Weak<dyn ApprovalSource> =
            Arc::downgrade(&(service.clone() as Arc<dyn ApprovalSource>));
        registry.chats().set_external_approvals(source);
        service
    }

    pub fn settings(&self) -> ControlSettings {
        *self.settings.read()
    }

    /// Uses `settings` from now on (`daemon.settings.set` changed `[control]`): the policy is
    /// asked for each request and the timeout for each approval.
    pub fn apply_settings(&self, settings: ControlSettings) {
        *self.settings.write() = settings;
    }

    /// Whether `session` exists and is live: a claimed `origin` that is not gets ignored.
    pub async fn origin_is_live(&self, session: SessionId) -> bool {
        self.origin_session(session)
            .await
            .is_some_and(|s| s.state.is_live())
    }

    /// Whether a request from a client running in `session` is an agent's, now: the session is
    /// a chat/harness session, or a terminal in which an agent reported itself and has not
    /// released itself since. A plain shell is the user typing. Asked per request, not once at
    /// `hello`: an agent may report itself after a long-lived `hutan mcp` started.
    pub async fn is_agent_origin(&self, session: SessionId) -> bool {
        self.origin_session(session)
            .await
            .is_some_and(|s| is_agent_session(&s))
    }

    async fn origin_session(&self, session: SessionId) -> Option<SessionInfo> {
        match self
            .registry
            .list_sessions(SessionListFilter::default())
            .await
        {
            Ok(list) => list.sessions.into_iter().find(|s| s.id == session),
            Err(error) => {
                warn!(message = %error.message, "could not verify a client's origin");
                None
            }
        }
    }

    /// Decides whether an agent-originated `request` may run. Waits for the user under `ask`.
    pub async fn gate(
        &self,
        origin: SessionId,
        client: Option<ClientKind>,
        request: &Request,
    ) -> Result<Gated, ErrorBody> {
        let access = crate::router::classify(request);
        let free = Gated { approved: None };
        if access == Access::Read {
            return Ok(free);
        }
        let method = request_method(request);
        let describe = self.describe(request).await;
        if access == Access::UserOnly {
            self.audit_auto(origin, &describe, "deny_user_only");
            return Err(forbidden(
                format!("`{method}` is only available to the user, not to agents"),
                "user_only",
            ));
        }
        match self.settings().agents {
            AgentPolicy::ReadOnly => {
                self.audit_auto(origin, &describe, "deny_policy");
                Err(forbidden(
                    format!(
                        "agents are read-only here: `{method}` was refused \
                         (`[control] agents = \"read_only\"` in daemon.toml)"
                    ),
                    "read_only",
                ))
            }
            AgentPolicy::Allow => {
                self.audit_auto(origin, &describe, "allow_policy");
                Ok(free)
            }
            AgentPolicy::Ask => {
                let grant = grant_key(request);
                if lock(&self.state).grants.contains(&(origin, grant.clone())) {
                    self.audit_auto(origin, &describe, "allow_grant");
                    return Ok(free);
                }
                self.ask(origin, client, method, grant, describe).await
            }
        }
    }

    async fn ask(
        &self,
        origin: SessionId,
        _client: Option<ClientKind>,
        method: &'static str,
        grant: String,
        describe: Described,
    ) -> Result<Gated, ErrorBody> {
        let who = self.origin_name(origin).await;
        let info = ApprovalInfo {
            id: ApprovalId::new(),
            session_id: origin,
            turn_id: None,
            kind: PermissionKind::Control,
            title: format!("{who} asks Hutan to {}", describe.title),
            detail: Some(describe.detail.clone()),
            options: vec![
                option(ALLOW_ONCE, "Allow once", PermissionOptionKind::AllowOnce),
                option(
                    ALLOW_SESSION,
                    "Allow for this session",
                    PermissionOptionKind::AllowAlways,
                ),
                option(DENY, "Deny", PermissionOptionKind::RejectOnce),
            ],
            requested_at_ms: registry::now_ms(),
        };
        let id = info.id;
        let (answer, answered) = oneshot::channel();
        lock(&self.state).pending.insert(
            id,
            PendingControl {
                info: info.clone(),
                answer: Some(answer),
            },
        );
        self.write_row(&info, None, None, None);
        self.publish(&Event::ApprovalRequired {
            approval: Box::new(info.clone()),
        });
        info!(approval = %id, session = %origin, method, "an agent asks to control Hutan");

        let started = Instant::now();
        let timeout = Duration::from_secs(self.settings().approval_timeout_secs);
        let decision = tokio::select! {
            decision = answered => decision.ok(),
            () = tokio::time::sleep(timeout) => None,
            () = self.shutdown.cancelled() => None,
        };
        let waited_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        lock(&self.state).pending.remove(&id);
        let Some((option_id, decided_by)) = decision else {
            // Nobody answered in time (or the daemon is going away).
            self.write_row(&info, Some(ApprovalOutcome::Expired), None, None);
            self.publish(&Event::ApprovalResolved {
                approval_id: id,
                session_id: origin,
                outcome: hutan_core::agent::PermissionOutcome::Expired,
                decided_by: None,
            });
            return Err(forbidden(
                format!(
                    "nobody answered the approval within {}s: `{method}` was not run",
                    timeout.as_secs()
                ),
                "expired",
            ));
        };
        self.write_row(
            &info,
            Some(ApprovalOutcome::Selected),
            Some(option_id.clone()),
            Some(decided_by.to_owned()),
        );
        self.publish(&Event::ApprovalResolved {
            approval_id: id,
            session_id: origin,
            outcome: hutan_core::agent::PermissionOutcome::Selected {
                option_id: option_id.clone(),
            },
            decided_by: Some(DecidedBy {
                client_kind: decided_by.to_owned(),
                client_name: None,
            }),
        });
        match option_id.as_str() {
            ALLOW_ONCE => Ok(Gated {
                approved: Some(ApprovalNote {
                    option_id,
                    waited_ms,
                }),
            }),
            ALLOW_SESSION => {
                lock(&self.state).grants.insert((origin, grant));
                Ok(Gated {
                    approved: Some(ApprovalNote {
                        option_id,
                        waited_ms,
                    }),
                })
            }
            _ => Err(forbidden(
                format!("the user denied `{method}`: it was not run"),
                "deny",
            )),
        }
    }

    /// Whether an agent-originated connection may send terminal input frames (which carry no
    /// response, so they cannot wait for an approval): `allow` lets it, `read_only` does not,
    /// `ask` only after the user chose "allow for this session" for a `session.send_text` of
    /// that session. Frames are not audited one by one.
    pub fn input_allowed(&self, origin: SessionId) -> bool {
        match self.settings().agents {
            AgentPolicy::Allow => true,
            AgentPolicy::ReadOnly => false,
            AgentPolicy::Ask => lock(&self.state)
                .grants
                .contains(&(origin, "session.send_text".to_owned())),
        }
    }

    /// The control approvals still waiting, for `approval.list`.
    pub fn approval_list(&self, session: Option<SessionId>) -> Vec<ApprovalInfo> {
        lock(&self.state)
            .pending
            .values()
            .map(|p| p.info.clone())
            .filter(|info| session.is_none_or(|s| s == info.session_id))
            .collect()
    }

    /// Answers a control approval. `None` when `approval` is not one of ours.
    fn respond_inner(
        &self,
        approval: ApprovalId,
        option_id: String,
        client: Option<ClientKind>,
    ) -> Option<Result<(), ErrorBody>> {
        let mut state = lock(&self.state);
        let pending = state.pending.get_mut(&approval)?;
        if !pending.info.options.iter().any(|o| o.id == option_id) {
            let offered: Vec<&str> = pending.info.options.iter().map(|o| o.id.as_str()).collect();
            let mut error = ErrorBody::new(
                ErrorCode::OptionNotOffered,
                format!(
                    "option `{option_id}` was not offered; offered: {}",
                    offered.join(", ")
                ),
            );
            error.data = Some(json!({ "offered": offered }));
            return Some(Err(error));
        }
        let Some(answer) = pending.answer.take() else {
            return Some(Err(ErrorBody::new(
                ErrorCode::InvalidState,
                format!("approval {approval} was already answered"),
            )));
        };
        let _ = answer.send((option_id, client_name(client)));
        Some(Ok(()))
    }

    fn publish(&self, event: &Event) {
        if let Err(error) = self.bus.publish(event) {
            warn!(%error, "could not publish a control approval event");
        }
    }

    /// The name of the session an agent runs in, for the approval's title.
    async fn origin_name(&self, session: SessionId) -> String {
        let title = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await
            .ok()
            .and_then(|list| list.sessions.into_iter().find(|s| s.id == session))
            .map(|s| s.title);
        match title {
            Some(title) if !title.trim().is_empty() => format!("Agent in session “{title}”"),
            _ => format!("Agent in session {session}"),
        }
    }

    fn write_row(
        &self,
        info: &ApprovalInfo,
        outcome: Option<ApprovalOutcome>,
        option_id: Option<String>,
        decided_by: Option<String>,
    ) {
        let options_json = serde_json::to_string(&info.options).unwrap_or_else(|_| "[]".to_owned());
        let row = ApprovalRow {
            id: info.id,
            session_id: info.session_id,
            turn_id: None,
            kind: "control".to_owned(),
            title: info.title.clone(),
            options_json,
            requested_at_ms: info.requested_at_ms,
            resolved_at_ms: outcome.map(|_| registry::now_ms()),
            outcome,
            option_id,
            decided_by,
        };
        registry::log_write(self.storage.approvals().upsert(row), "a control approval");
    }

    /// A decision nobody was asked about (policy, a remembered grant): one finished row.
    fn audit_auto(&self, origin: SessionId, describe: &Described, decision: &str) {
        let now = registry::now_ms();
        let row = ApprovalRow {
            id: ApprovalId::new(),
            session_id: origin,
            turn_id: None,
            kind: "control".to_owned(),
            title: describe.title.clone(),
            options_json: "[]".to_owned(),
            requested_at_ms: now,
            resolved_at_ms: Some(now),
            outcome: Some(ApprovalOutcome::Selected),
            option_id: Some(decision.to_owned()),
            decided_by: Some("policy".to_owned()),
        };
        registry::log_write(self.storage.approvals().upsert(row), "a control decision");
    }

    /// Plain words for what `request` would do, with the sessions it names by title.
    async fn describe(&self, request: &Request) -> Described {
        let sessions = if names_session(request) {
            self.registry
                .list_sessions(SessionListFilter::default())
                .await
                .map(|list| list.sessions)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let session_name = |id: SessionId| -> String {
            match sessions.iter().find(|s| s.id == id) {
                Some(s) => format!("session “{}” ({id})", s.title),
                None => format!("session {id}"),
            }
        };
        describe_request(request, &session_name)
    }
}

impl ApprovalSource for ControlService {
    fn list(&self, session: Option<SessionId>) -> Vec<ApprovalInfo> {
        self.approval_list(session)
    }

    fn respond(
        &self,
        approval: ApprovalId,
        option_id: String,
        client: Option<ClientKind>,
    ) -> Option<Result<(), ErrorBody>> {
        self.respond_inner(approval, option_id, client)
    }
}

fn option(id: &str, label: &str, kind: PermissionOptionKind) -> PermissionOption {
    PermissionOption {
        id: id.to_owned(),
        label: label.to_owned(),
        kind,
        provider: None,
    }
}

fn forbidden(message: String, decision: &str) -> ErrorBody {
    let mut error = ErrorBody::new(ErrorCode::Forbidden, message);
    error.data = Some(json!({"control": {"decision": decision}}));
    error
}

/// A request in plain words: a title for the approval and the request itself as detail.
#[derive(Debug, Clone)]
pub struct Described {
    pub title: String,
    pub detail: String,
}

const DETAIL_MAX_CHARS: usize = 400;

fn names_session(request: &Request) -> bool {
    target_session(request).is_some()
}

/// The session a request acts on, when it names one.
fn target_session(request: &Request) -> Option<SessionId> {
    match request {
        Request::SessionStop(r) | Request::SessionRestart(r) | Request::ChatInterrupt(r) => {
            Some(r.session_id)
        }
        Request::SessionRename(r) => Some(r.session_id),
        Request::SessionKill(r) => Some(r.session_id),
        Request::SessionClose(r) => Some(r.session_id),
        Request::SessionReopen(r) => Some(r.session_id),
        Request::ProcessSignal(r) => Some(r.session_id),
        Request::AgentInterrupt(r) => Some(r.session_id),
        Request::SessionSendText(r) => Some(r.session_id),
        Request::ChatSend(r) => Some(r.session_id),
        Request::ChatSetModel(r) => Some(r.session_id),
        _ => None,
    }
}

/// The name of a request's method on the wire.
pub fn request_method(request: &Request) -> &'static str {
    crate::router::method_name(request)
}

/// What a grant of `allow_session` covers: the method, narrowed for the requests whose method
/// alone is too broad (`ui.run` per action, `ui.set_setting` per namespace).
pub fn grant_key(request: &Request) -> String {
    let method = request_method(request);
    match request {
        Request::UiRun(r) => format!("{method}:{}", r.action),
        Request::UiSetSetting(r) => format!("{method}:{}", r.namespace),
        _ => method.to_owned(),
    }
}

/// Whether a request from inside `session` is an agent's rather than the user's: a chat or
/// harness session is always an agent; a terminal only when an agent reported itself there
/// (`SessionInfo.agent` is cleared when the agent releases itself) and the session is live. An
/// agent without an integration in a terminal is not detected.
pub fn is_agent_session(session: &SessionInfo) -> bool {
    if !session.state.is_live() {
        return false;
    }
    match session.kind {
        SessionKind::Harness | SessionKind::Direct => true,
        SessionKind::Terminal => session.agent.is_some(),
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn describe_request(request: &Request, session: &dyn Fn(SessionId) -> String) -> Described {
    let detail = match serde_json::to_value(request) {
        Ok(value) => clip(
            &value
                .get("params")
                .map_or_else(String::new, Value::to_string),
            DETAIL_MAX_CHARS,
        ),
        Err(_) => String::new(),
    };
    let title = match request {
        Request::DaemonShutdown(r) => {
            if r.stop_sessions {
                "stop the Hutan daemon and every session".to_owned()
            } else {
                "stop the Hutan daemon".to_owned()
            }
        }
        Request::WorkspaceCreate(r) => format!("create the workspace “{}”", r.name),
        Request::WorkspaceRename(r) => format!("rename a workspace to “{}”", r.name),
        Request::WorkspaceAssign(r) => match r.workspace_id {
            Some(workspace) => format!("put project {} into workspace {workspace}", r.project_id),
            None => format!("take project {} out of its workspace", r.project_id),
        },
        Request::WorkspaceDelete(r) => format!("delete workspace {}", r.workspace_id),
        Request::WorkspaceClose(r) => format!("close workspace {}", r.workspace_id),
        Request::WorkspaceOpen(r) => format!("open the folder {} as a workspace", r.path.display()),
        Request::WorkspaceAddRoot(r) => {
            format!("add {} to workspace {}", r.path.display(), r.workspace_id)
        }
        Request::WorkspaceRemoveRoot(r) => {
            format!(
                "remove {} from workspace {}",
                r.path.display(),
                r.workspace_id
            )
        }
        Request::WorkspaceMove(r) => format!("move workspace {}", r.workspace_id),
        Request::WorkspacePin(r) => format!(
            "{} workspace {}",
            if r.pinned { "pin" } else { "unpin" },
            r.workspace_id
        ),
        Request::SessionCreate(_) => "start a new terminal session".to_owned(),
        Request::SessionStop(r) => format!("stop {}", session(r.session_id)),
        Request::SessionRestart(r) => format!("restart {}", session(r.session_id)),
        Request::SessionRename(r) => {
            format!("rename {} to “{}”", session(r.session_id), r.title)
        }
        Request::SessionClose(r) => format!(
            "close {}{}",
            session(r.session_id),
            if r.stop_running {
                ", stopping whatever runs in it"
            } else {
                ""
            }
        ),
        Request::SessionReopen(r) => format!("reopen the closed session {}", r.session_id),
        Request::SessionSendText(r) => format!(
            "type {} character(s) into {}{}",
            r.text.chars().count(),
            session(r.session_id),
            if r.enter { " and press enter" } else { "" }
        ),
        Request::TerminalResize(r) => format!("resize terminal {}", r.terminal_id),
        Request::TerminalSetColors(r) => format!("change the colors of terminal {}", r.terminal_id),
        Request::SessionKill(r) => {
            format!("kill {} and every process in it", session(r.session_id))
        }
        Request::ProcessSignal(r) => format!(
            "send the signal {:?} to process {} of {}",
            r.signal,
            r.pid,
            session(r.session_id)
        ),
        Request::AgentInterrupt(r) => format!(
            "interrupt {} of {}{}",
            r.node_id.as_deref().map_or_else(
                || "the main agent".to_owned(),
                |n| format!("the subagent {n}")
            ),
            session(r.session_id),
            if r.force { " (forcing it)" } else { "" }
        ),
        Request::HistorySetEnabled(r) => format!(
            "{} the session history index",
            if r.enabled { "switch on" } else { "switch off" }
        ),
        Request::HistoryClear(_) => "delete the session history index".to_owned(),
        Request::HistoryRebuild(_) => "rebuild the session history index".to_owned(),
        Request::ChatCreate(r) => format!("start a {} chat in {}", r.agent, r.cwd.display()),
        Request::ChatSend(r) => format!(
            "send a message ({} characters) to the chat {}",
            r.text.chars().count(),
            session(r.session_id)
        ),
        Request::ChatInterrupt(r) => format!("interrupt the turn of {}", session(r.session_id)),
        Request::ChatSetModel(r) => {
            format!("switch {} to the model {}", session(r.session_id), r.model)
        }
        Request::AssistantConfigure(_) => "change what runs an assistant".to_owned(),
        Request::AssistantOpen(_) => "open an assistant session".to_owned(),
        Request::NotifyAck(r) => format!("dismiss notification {}", r.id),
        Request::WorktreeCreate(r) => format!("create a git worktree for the branch {}", r.branch),
        Request::WorktreeRemove(r) => format!("remove the git worktree {}", r.path.display()),
        Request::WorktreeMerge(r) => format!(
            "{} the branch of the git worktree {} into {} in the main checkout",
            match r.strategy {
                hutan_protocol::worktree::MergeStrategy::Merge => "merge",
                hutan_protocol::worktree::MergeStrategy::Squash => "squash-merge",
                hutan_protocol::worktree::MergeStrategy::Rebase => "rebase and merge",
            },
            r.path.display(),
            r.into.as_deref().unwrap_or("the default branch")
        ),
        Request::TaskCreate(r) => format!("create the task “{}”", r.title),
        Request::TaskUpdate(r) => format!("change task {}", r.id),
        Request::TaskDelete(r) => format!("delete task {}", r.id),
        Request::TaskStart(r) => format!("start task {}", r.id),
        Request::TaskReview(r) => format!(
            "{} for task {}{}",
            match r.decision {
                hutan_core::ReviewDecision::Accept => "accept the work",
                hutan_core::ReviewDecision::RequestChanges => "request changes to the work",
            },
            r.id,
            r.note.as_deref().map_or_else(String::new, |note| format!(
                ": {}",
                note.chars().take(120).collect::<String>()
            ))
        ),
        Request::WorktreeCommit(r) => format!(
            "commit everything in the git worktree {} (“{}”)",
            r.path.display(),
            r.message.lines().next().unwrap_or_default()
        ),
        Request::UiRun(r) => format!("run the window action {}", r.action),
        Request::UiSetSetting(r) => format!(
            "change the setting {}.{} ({} layer)",
            r.namespace, r.key, r.layer
        ),
        other => format!("make the request `{}`", request_method(other)),
    };
    Described { title, detail }
}

#[cfg(test)]
mod tests {
    use hutan_core::{SessionState, WorkspaceId};
    use hutan_protocol::messages::{AgentReport, AgentState, SessionRename};
    use hutan_protocol::ui::{UiRun, UiSetSetting};

    use super::*;

    fn session(kind: SessionKind, state: SessionState, agent: bool) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
            kind,
            title: "t".to_owned(),
            state,
            command: Vec::new(),
            cwd: None,
            terminal_id: None,
            created_at_ms: 0,
            agent: agent.then(|| AgentReport {
                source: "hooks".to_owned(),
                agent: "omp".to_owned(),
                state: AgentState::Working,
                message: None,
                agent_session_id: None,
                updated_at_ms: 0,
                blocked_sub: None,
            }),
            restored: false,
            chat: None,
            placement: None,
            folder: None,
            recovery: None,
        }
    }

    #[test]
    fn a_plain_shell_is_the_user_and_everything_an_agent_runs_in_is_an_agent() {
        let running = SessionState::Running;
        assert!(!is_agent_session(&session(
            SessionKind::Terminal,
            running.clone(),
            false
        )));
        assert!(is_agent_session(&session(
            SessionKind::Terminal,
            running.clone(),
            true
        )));
        assert!(is_agent_session(&session(
            SessionKind::Harness,
            running.clone(),
            false
        )));
        assert!(is_agent_session(&session(
            SessionKind::Direct,
            running.clone(),
            false
        )));
        // A session that ended cannot be the origin of anything.
        assert!(!is_agent_session(&session(
            SessionKind::Harness,
            SessionState::Stopped,
            false
        )));
        assert!(!is_agent_session(&session(
            SessionKind::Terminal,
            SessionState::Stopped,
            true
        )));
    }

    #[test]
    fn grants_are_narrowed_for_the_broad_requests() {
        let id = SessionId::new();
        let rename = Request::SessionRename(SessionRename {
            session_id: id,
            title: "x".to_owned(),
        });
        assert_eq!(grant_key(&rename), "session.rename");
        let run = |action: &str| {
            Request::UiRun(UiRun {
                window_id: None,
                action: action.to_owned(),
                args: None,
            })
        };
        assert_ne!(
            grant_key(&run("workbench::SplitRight")),
            grant_key(&run("workbench::Quit"))
        );
        let set = |namespace: &str, key: &str| {
            Request::UiSetSetting(UiSetSetting {
                window_id: None,
                namespace: namespace.to_owned(),
                key: key.to_owned(),
                value: serde_json::json!(1),
                layer: "user".to_owned(),
            })
        };
        assert_eq!(
            grant_key(&set("terminal", "a")),
            grant_key(&set("terminal", "b"))
        );
        assert_ne!(
            grant_key(&set("terminal", "a")),
            grant_key(&set("chat", "a"))
        );
    }

    #[test]
    fn requests_are_described_in_plain_words_with_the_target_session() {
        let id = SessionId::new();
        let request = Request::SessionRename(SessionRename {
            session_id: id,
            title: "build".to_owned(),
        });
        let described = describe_request(&request, &|s| format!("session “api” ({s})"));
        assert!(
            described.title.contains("rename session “api”"),
            "{}",
            described.title
        );
        assert!(described.title.contains("“build”"));
        assert!(described.detail.contains("build"));
        let long = Request::UiRun(UiRun {
            window_id: None,
            action: "a".repeat(2000),
            args: None,
        });
        let described = describe_request(&long, &|s| s.to_string());
        assert!(described.detail.chars().count() <= DETAIL_MAX_CHARS + 1);
    }
}
