//! The ACP client (ADR 0015, `docs/agents.md` § ACP harnesses): an agent that speaks the Agent
//! Client Protocol v1 over stdio (`omp acp`, Claude Code's ACP adapter) as an [`AgentAdapter`].
//!
//! [`AcpAdapter::start`] spawns the process and runs the handshake: `initialize` (Hutan offers
//! no file system and no terminal: the agent uses its own tools, and Hutan's approvals gate
//! them), the protocol version gate, `session/new` — or, to continue a conversation,
//! `session/load` (else `session/resume`) with the stored session id — and `authenticate` with
//! the agent's own method when it says it needs one. Then a pump task turns the agent's
//! messages into [`AdapterMessage`]s ([`mapper`], [`nodes`]); `session/prompt` is a turn, whose
//! response ends it in order behind the turn's updates ([`Transport::request_ordered`]).
//!
//! Accounts stay with the harness: Hutan never logs in for an agent. An agent that needs a
//! login Hutan cannot do for it fails to start with [`AgentError::AuthRequired`].

mod mapper;
mod nodes;
mod udiff;
mod wire;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use hutan_core::{ApprovalId, ModelInfo, QuestionId, QuestionResponse};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::adapter::{
    AdapterMessage, AgentAdapter, ExitInfo, MessageExtras, StartSpec, Started, TurnRef,
};
use crate::codex::{AnswerError, Mapped, Reply};
use crate::error::AgentError;
use crate::jsonrpc::{Incoming, Launch, REQUEST_TIMEOUT, Transport};

pub use mapper::{Finished, Mapper, TurnOutcome};
pub use nodes::{NodeFeed, TREE_SOURCE};
pub use wire::PROTOCOL_VERSION;

/// Events buffered between the adapter and the daemon before the adapter waits.
const EVENT_QUEUE: usize = 512;
/// How long the adapter waits for the exit status after the output closed.
const EXIT_WAIT: Duration = Duration::from_secs(3);
/// How long `session/close` may take before the process is ended anyway.
const CLOSE_WAIT: Duration = Duration::from_secs(2);
/// JSON-RPC "method not found".
const METHOD_NOT_FOUND: i64 = -32601;

/// Where an ACP agent's executable is and how to start it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpConfig {
    /// `omp`, `claude`: what events and the agent tree call the agent.
    pub agent: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl AcpConfig {
    fn launch(&self, cwd: Option<PathBuf>) -> Launch {
        Launch {
            program: self.program.clone(),
            args: self.args.clone(),
            cwd,
            env: self.env.clone(),
        }
    }
}

/// What a successful [`AcpAdapter::start`] returns.
#[derive(Debug)]
pub struct AcpSession {
    pub adapter: Arc<AcpAdapter>,
    pub started: Started,
    pub events: mpsc::Receiver<AdapterMessage>,
    /// The models the session offers; empty when the agent offers no choice.
    pub models: Vec<ModelInfo>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded data is never left half-updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a session's model is switched.
#[derive(Debug, Clone)]
enum ModelSwitch {
    /// `session/set_config_option` with this option id.
    Config(String),
    /// The earlier `session/set_model`.
    Legacy,
}

#[derive(Debug, Clone)]
struct ModelChoice {
    switch: ModelSwitch,
    current: String,
    models: Vec<ModelInfo>,
}

/// How a session's mode is switched.
#[derive(Debug, Clone)]
struct ModeChoice {
    config_id: Option<String>,
    available: Vec<String>,
    current: String,
}

#[derive(Debug)]
struct Shared {
    transport: Arc<Transport>,
    agent: String,
    session_id: String,
    events: mpsc::Sender<AdapterMessage>,
    mapper: Mutex<Mapper>,
    nodes: Mutex<NodeFeed>,
    model: Mutex<Option<ModelChoice>>,
    turn_active: AtomicBool,
    embedded_context: bool,
    /// The agent said `promptCapabilities.image`: image content blocks are accepted.
    images: bool,
    can_close: bool,
}

/// One ACP agent process with one open session.
#[derive(Debug)]
pub struct AcpAdapter {
    shared: Arc<Shared>,
}

/// A request made while no pump runs yet: answered in order, with everything the agent says
/// meanwhile dropped (the replay of `session/load` is Hutan's own journal already) or refused.
async fn open_call(
    transport: &Transport,
    incoming: &mut mpsc::Receiver<Incoming>,
    method: &str,
    params: Value,
) -> Result<Value, AgentError> {
    transport.request_ordered(method, params, 0).await?;
    let wait = async {
        while let Some(message) = incoming.recv().await {
            match message {
                Incoming::Completed { result, .. } => return result,
                Incoming::Request { id, method, .. } => {
                    if let Err(error) = transport
                        .respond_error(
                            &id,
                            METHOD_NOT_FOUND,
                            &format!("`{method}` is not available yet"),
                        )
                        .await
                    {
                        debug!(%error, "could not refuse a request during the handshake");
                    }
                }
                Incoming::Notification { .. } => {}
                Incoming::Closed => break,
            }
        }
        Err(AgentError::Exited {
            detail: transport.stderr_tail(),
        })
    };
    timeout(REQUEST_TIMEOUT, wait).await.unwrap_or_else(|_| {
        Err(AgentError::Timeout {
            method: method.to_owned(),
        })
    })
}

fn parse<T: serde::de::DeserializeOwned>(method: &str, value: Value) -> Result<T, AgentError> {
    serde_json::from_value(value).map_err(|e| {
        AgentError::Protocol(format!("`{method}` answered with an unexpected shape: {e}"))
    })
}

fn is_auth_required(error: &AgentError) -> bool {
    matches!(error, AgentError::Rpc { code, .. } if *code == wire::AUTH_REQUIRED)
}

/// The select options of a config option, flat or in groups: `(value, name, description)`.
fn select_values(options: &[Value]) -> Vec<(String, String, Option<String>)> {
    let mut found = Vec::new();
    for option in options {
        if let Some(group) = option.get("options").and_then(Value::as_array) {
            found.extend(select_values(group));
        } else if let Some(value) = option.get("value").and_then(Value::as_str) {
            found.push((
                value.to_owned(),
                option
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(value)
                    .to_owned(),
                option
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            ));
        }
    }
    found
}

fn model_info(
    value: String,
    name: String,
    description: Option<String>,
    current: &str,
) -> ModelInfo {
    ModelInfo {
        is_default: value == current,
        id: value,
        display_name: name,
        description: description.unwrap_or_default(),
        hidden: false,
        default_reasoning_effort: None,
        reasoning_efforts: Vec::new(),
    }
}

/// The model choice a session offers: its `model` select option, else the earlier `models`
/// state.
fn model_choice(result: &wire::SessionResult) -> Option<ModelChoice> {
    if let Some(option) = result
        .config_options
        .iter()
        .find(|o| o.category.as_deref() == Some("model") || o.id == "model")
    {
        let current = option.current_value.as_str().unwrap_or_default().to_owned();
        let models: Vec<ModelInfo> = select_values(&option.options)
            .into_iter()
            .map(|(value, name, description)| model_info(value, name, description, &current))
            .collect();
        if !models.is_empty() {
            return Some(ModelChoice {
                switch: ModelSwitch::Config(option.id.clone()),
                current,
                models,
            });
        }
    }
    let state = result.models.as_ref()?;
    let models: Vec<ModelInfo> = state
        .available_models
        .iter()
        .map(|m| {
            model_info(
                m.model_id.clone(),
                if m.name.is_empty() {
                    m.model_id.clone()
                } else {
                    m.name.clone()
                },
                m.description.clone(),
                &state.current_model_id,
            )
        })
        .collect();
    (!models.is_empty()).then(|| ModelChoice {
        switch: ModelSwitch::Legacy,
        current: state.current_model_id.clone(),
        models,
    })
}

/// The mode choice a session offers: its `modes` state, else a `mode` select option.
fn mode_choice(result: &wire::SessionResult) -> Option<ModeChoice> {
    if let Some(state) = &result.modes
        && !state.available_modes.is_empty()
    {
        return Some(ModeChoice {
            config_id: None,
            available: state.available_modes.iter().map(|m| m.id.clone()).collect(),
            current: state.current_mode_id.clone(),
        });
    }
    let option = result
        .config_options
        .iter()
        .find(|o| o.category.as_deref() == Some("mode") || o.id == "mode")?;
    Some(ModeChoice {
        config_id: Some(option.id.clone()),
        available: select_values(&option.options)
            .into_iter()
            .map(|(value, ..)| value)
            .collect(),
        current: option.current_value.as_str().unwrap_or_default().to_owned(),
    })
}

fn unknown_model(model: &str, models: &[ModelInfo]) -> AgentError {
    AgentError::UnknownModel {
        model: model.to_owned(),
        available: models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// What the handshake learned.
struct Opened {
    session_id: String,
    resumed: bool,
    result: wire::SessionResult,
    capabilities: wire::AgentCapabilities,
    close: bool,
}

/// `initialize`, the version gate, and the session (new, loaded or resumed), with the agent's
/// own `authenticate` when the session call says it is needed.
async fn open(
    transport: &Transport,
    incoming: &mut mpsc::Receiver<Incoming>,
    spec: &StartSpec,
    mcp_servers: &[Value],
) -> Result<Opened, AgentError> {
    let init = wire::InitializeParams {
        protocol_version: wire::PROTOCOL_VERSION,
        client_capabilities: wire::ClientCapabilities {
            fs: wire::FsCapabilities {
                read_text_file: false,
                write_text_file: false,
            },
            terminal: false,
            elicitation: wire::ElicitationCapabilities {
                form: serde_json::Map::new(),
            },
        },
        client_info: wire::ClientInfo {
            name: "hutan",
            title: "Hutan",
            version: env!("CARGO_PKG_VERSION"),
        },
    };
    let params = serde_json::to_value(&init)
        .map_err(|e| AgentError::Protocol(format!("encoding `initialize`: {e}")))?;
    let init: wire::InitializeResult = parse(
        "initialize",
        open_call(transport, incoming, "initialize", params).await?,
    )?;
    if init.protocol_version != wire::PROTOCOL_VERSION {
        return Err(AgentError::UnsupportedAcp {
            found: init.protocol_version,
            supported: wire::PROTOCOL_VERSION,
        });
    }
    let capabilities = init.agent_capabilities;
    let servers: Vec<Value> = mcp_servers
        .iter()
        .filter(|server| match server.get("type").and_then(Value::as_str) {
            Some("http") => capabilities.mcp_capabilities.http,
            Some("sse") => capabilities.mcp_capabilities.sse,
            _ => true,
        })
        .cloned()
        .collect();
    if servers.len() != mcp_servers.len() {
        warn!("some MCP servers were not passed: the agent does not take that transport");
    }
    let cwd = spec.cwd.to_string_lossy().into_owned();

    let (method, params, session_id) = match &spec.resume_thread_id {
        Some(id) => {
            let method = if capabilities.load_session {
                "session/load"
            } else if capabilities.session_capabilities.resume.is_some() {
                "session/resume"
            } else {
                return Err(AgentError::Unsupported(format!(
                    "{} cannot continue a conversation (it offers neither session/load nor session/resume)",
                    init.agent_info
                        .as_ref()
                        .map_or("the agent", |a| a.name.as_str())
                )));
            };
            let params = serde_json::to_value(wire::LoadSessionParams {
                session_id: id,
                cwd: &cwd,
                mcp_servers: &servers,
            });
            (method, params, Some(id.clone()))
        }
        None => (
            "session/new",
            serde_json::to_value(wire::NewSessionParams {
                cwd: &cwd,
                mcp_servers: &servers,
            }),
            None,
        ),
    };
    let params = params.map_err(|e| AgentError::Protocol(format!("encoding `{method}`: {e}")))?;

    let mut answer = open_call(transport, incoming, method, params.clone()).await;
    if let Err(error) = &answer
        && is_auth_required(error)
    {
        authenticate(transport, incoming, &init.auth_methods, error).await?;
        answer = open_call(transport, incoming, method, params).await;
    }
    let result: wire::SessionResult = parse(method, answer?)?;
    let resumed = session_id.is_some();
    let Some(session_id) = session_id.or_else(|| result.session_id.clone()) else {
        return Err(AgentError::Protocol(
            "`session/new` answered without a sessionId".to_owned(),
        ));
    };
    Ok(Opened {
        session_id,
        resumed,
        result,
        close: capabilities.session_capabilities.close.is_some(),
        capabilities,
    })
}

/// The agent said it needs authentication: use its own plain method (OMP's `agent`: the
/// credentials the harness already holds). A method that needs a terminal or an environment
/// variable is the harness's own login flow, which `account.login` runs for the user.
async fn authenticate(
    transport: &Transport,
    incoming: &mut mpsc::Receiver<Incoming>,
    methods: &[wire::AuthMethod],
    cause: &AgentError,
) -> Result<(), AgentError> {
    let Some(method) = methods.iter().find(|m| m.kind.is_none()) else {
        return Err(AgentError::AuthRequired {
            detail: format!("; log in with the agent's own flow (`hutan account login`): {cause}"),
        });
    };
    let params = serde_json::to_value(wire::AuthenticateParams {
        method_id: &method.id,
    })
    .map_err(|e| AgentError::Protocol(format!("encoding `authenticate`: {e}")))?;
    open_call(transport, incoming, "authenticate", params)
        .await
        .map(|_| ())
        .map_err(|error| match error {
            AgentError::Rpc { message, .. } => AgentError::AuthRequired {
                detail: format!(
                    " ({message}); log in with the agent's own flow (`hutan account login`)"
                ),
            },
            other => other,
        })
}

impl AcpAdapter {
    /// Starts the process and opens (or continues) the conversation `spec` describes. `mode`
    /// is a session mode the agent offers; `mcp_servers` are ACP `McpServer` objects.
    pub async fn start(
        config: &AcpConfig,
        spec: &StartSpec,
        mode: Option<&str>,
        mcp_servers: &[Value],
    ) -> Result<AcpSession, AgentError> {
        let (transport, mut incoming, exit) =
            Transport::spawn_v2(&config.launch(Some(spec.cwd.clone())))?;
        match Self::begin(&transport, &mut incoming, spec, mode, mcp_servers).await {
            Ok(begun) => Ok(Self::run(config, transport, incoming, exit, begun)),
            Err(error) => {
                transport.shutdown().await;
                Err(error)
            }
        }
    }

    async fn begin(
        transport: &Transport,
        incoming: &mut mpsc::Receiver<Incoming>,
        spec: &StartSpec,
        mode: Option<&str>,
        mcp_servers: &[Value],
    ) -> Result<Begun, AgentError> {
        let opened = open(transport, incoming, spec, mcp_servers).await?;
        let mut model = model_choice(&opened.result);
        let mut mode_state = mode_choice(&opened.result);
        let session = opened.session_id.clone();

        if let Some(requested) = &spec.model {
            let Some(choice) = model.as_mut() else {
                return Err(AgentError::Unsupported(
                    "the agent does not offer a choice of models".to_owned(),
                ));
            };
            if !choice.models.iter().any(|m| &m.id == requested) {
                return Err(unknown_model(requested, &choice.models));
            }
            if &choice.current != requested {
                set_model_call(transport, incoming, &session, choice, requested).await?;
            }
        }
        if let Some(wanted) = mode {
            let Some(choice) = mode_state.as_mut() else {
                return Err(AgentError::Unsupported(
                    "the agent does not offer session modes".to_owned(),
                ));
            };
            if !choice.available.iter().any(|m| m == wanted) {
                return Err(AgentError::Unsupported(format!(
                    "the agent does not offer mode `{wanted}`; offered: {}",
                    choice.available.join(", ")
                )));
            }
            if choice.current != wanted {
                set_mode_call(transport, incoming, &session, choice, wanted).await?;
            }
        }
        Ok(Begun {
            cwd: spec.cwd.to_string_lossy().into_owned(),
            opened,
            model,
            mode: mode_state,
        })
    }

    fn run(
        config: &AcpConfig,
        transport: Arc<Transport>,
        incoming: mpsc::Receiver<Incoming>,
        exit: oneshot::Receiver<ExitInfo>,
        begun: Begun,
    ) -> AcpSession {
        let Begun {
            cwd,
            opened,
            model,
            mode,
        } = begun;
        let (events_tx, events) = mpsc::channel(EVENT_QUEUE);
        let model_name = model
            .as_ref()
            .map_or_else(String::new, |m| m.current.clone());
        let mut mapper = Mapper::new(&config.agent, opened.session_id.clone());
        mapper.set_cwd(cwd);
        mapper.set_baseline(
            model.as_ref().map(|m| m.current.clone()),
            mode.as_ref().map(|m| m.current.clone()),
        );
        // The channel is empty, so these cannot wait.
        let locator = mapper.resume_locator(&model_name);
        let _ = events_tx.try_send(AdapterMessage::Event(Box::new(locator)));
        let feed = NodeFeed::new(&config.agent, opened.session_id.clone());
        let mut root = feed.started(&model_name);
        // The main agent is the harness process (`hutan ps` marks it).
        root.pid = transport.pid();
        let _ = events_tx.try_send(AdapterMessage::Node(Box::new(root)));
        let models = model.as_ref().map_or_else(Vec::new, |m| m.models.clone());
        let started = Started {
            model: if model_name.is_empty() {
                "default".to_owned()
            } else {
                model_name
            },
            thread_id: opened.session_id.clone(),
            resumed: opened.resumed,
            images: Some(opened.capabilities.prompt_capabilities.image),
        };
        let shared = Arc::new(Shared {
            transport,
            agent: config.agent.clone(),
            session_id: opened.session_id,
            events: events_tx,
            mapper: Mutex::new(mapper),
            nodes: Mutex::new(feed),
            model: Mutex::new(model),
            turn_active: AtomicBool::new(false),
            embedded_context: opened.capabilities.prompt_capabilities.embedded_context,
            images: opened.capabilities.prompt_capabilities.image,
            can_close: opened.close,
        });
        tokio::spawn(pump(shared.clone(), incoming, exit));
        AcpSession {
            adapter: Arc::new(Self { shared }),
            started,
            events,
            models,
        }
    }

    /// The models the session offers right now.
    pub fn models(&self) -> Vec<ModelInfo> {
        lock(&self.shared.model)
            .as_ref()
            .map_or_else(Vec::new, |m| m.models.clone())
    }
}

struct Begun {
    cwd: String,
    opened: Opened,
    model: Option<ModelChoice>,
    mode: Option<ModeChoice>,
}

async fn set_model_call(
    transport: &Transport,
    incoming: &mut mpsc::Receiver<Incoming>,
    session: &str,
    choice: &mut ModelChoice,
    model: &str,
) -> Result<(), AgentError> {
    let (method, params) = switch_request(&choice.switch, session, model)?;
    open_call(transport, incoming, method, params).await?;
    choice.current = model.to_owned();
    Ok(())
}

async fn set_mode_call(
    transport: &Transport,
    incoming: &mut mpsc::Receiver<Incoming>,
    session: &str,
    choice: &mut ModeChoice,
    mode: &str,
) -> Result<(), AgentError> {
    let (method, params) = mode_request(choice, session, mode)?;
    open_call(transport, incoming, method, params).await?;
    choice.current = mode.to_owned();
    Ok(())
}

fn encode<T: serde::Serialize>(method: &str, params: &T) -> Result<Value, AgentError> {
    serde_json::to_value(params)
        .map_err(|e| AgentError::Protocol(format!("encoding `{method}`: {e}")))
}

fn switch_request(
    switch: &ModelSwitch,
    session: &str,
    model: &str,
) -> Result<(&'static str, Value), AgentError> {
    match switch {
        ModelSwitch::Config(config_id) => {
            let params = encode(
                "session/set_config_option",
                &wire::SetConfigOptionParams {
                    session_id: session,
                    config_id,
                    value: model,
                },
            )?;
            Ok(("session/set_config_option", params))
        }
        ModelSwitch::Legacy => Ok((
            "session/set_model",
            json!({"sessionId": session, "modelId": model}),
        )),
    }
}

fn mode_request(
    choice: &ModeChoice,
    session: &str,
    mode: &str,
) -> Result<(&'static str, Value), AgentError> {
    match &choice.config_id {
        Some(config_id) => {
            let params = encode(
                "session/set_config_option",
                &wire::SetConfigOptionParams {
                    session_id: session,
                    config_id,
                    value: mode,
                },
            )?;
            Ok(("session/set_config_option", params))
        }
        None => {
            let params = encode(
                "session/set_mode",
                &wire::SetModeParams {
                    session_id: session,
                    mode_id: mode,
                },
            )?;
            Ok(("session/set_mode", params))
        }
    }
}

/// Sends drafts; false when the consumer is gone.
async fn emit(shared: &Shared, drafts: Vec<hutan_core::AgentEventDraft>) -> bool {
    for draft in drafts {
        if shared
            .events
            .send(AdapterMessage::Event(Box::new(draft)))
            .await
            .is_err()
        {
            return false;
        }
    }
    true
}

async fn emit_nodes(shared: &Shared, updates: Vec<hutan_core::NodeUpdate>) -> bool {
    for update in updates {
        if shared
            .events
            .send(AdapterMessage::Node(Box::new(update)))
            .await
            .is_err()
        {
            return false;
        }
    }
    true
}

async fn send_reply(shared: &Shared, reply: Reply) {
    let Reply { id, outcome } = reply;
    let sent = match outcome {
        Ok(result) => shared.transport.respond(&id, result).await,
        Err((code, message)) => shared.transport.respond_error(&id, code, &message).await,
    };
    if let Err(error) = sent {
        debug!(%error, "could not answer a request of the agent");
    }
}

/// Turns what the process says into [`AdapterMessage`]s until it is gone.
async fn pump(
    shared: Arc<Shared>,
    mut incoming: mpsc::Receiver<Incoming>,
    exit: oneshot::Receiver<ExitInfo>,
) {
    while let Some(message) = incoming.recv().await {
        let alive = match message {
            Incoming::Notification { method, params } => {
                let updates = if method == "session/update" {
                    lock(&shared.nodes).update(&params)
                } else {
                    Vec::new()
                };
                let drafts = lock(&shared.mapper).notification(&method, params);
                emit_nodes(&shared, updates).await && emit(&shared, drafts).await
            }
            Incoming::Request { id, method, params } => {
                let Mapped { drafts, reply } = lock(&shared.mapper).request(id, &method, params);
                let blocked = drafts.iter().any(|d| {
                    matches!(
                        d.kind,
                        hutan_core::AgentEventKind::PermissionRequested { .. }
                            | hutan_core::AgentEventKind::QuestionRequested { .. }
                    )
                });
                let node = blocked.then(|| lock(&shared.nodes).blocked());
                let alive = emit(&shared, drafts).await
                    && emit_nodes(&shared, node.into_iter().collect()).await;
                if let Some(reply) = reply {
                    send_reply(&shared, reply).await;
                }
                alive
            }
            // The process went away under the prompt: the turn is the daemon's to close (it
            // knows whether the user stopped the session, and has the stderr).
            Incoming::Completed {
                result: Err(AgentError::Exited { .. }),
                ..
            } => true,
            Incoming::Completed { result, .. } => {
                let finished = lock(&shared.mapper).completed(result);
                let cancelled = finished.outcome == Some(TurnOutcome::Cancelled);
                let ended = finished.outcome.is_some();
                for reply in finished.replies {
                    send_reply(&shared, reply).await;
                }
                let alive = emit(&shared, finished.drafts).await;
                if ended {
                    shared.turn_active.store(false, Ordering::Release);
                }
                let updates = if ended {
                    lock(&shared.nodes).turn_ended(cancelled)
                } else {
                    Vec::new()
                };
                alive && emit_nodes(&shared, updates).await
            }
            Incoming::Closed => break,
        };
        if !alive {
            shared.transport.shutdown().await;
            return;
        }
    }
    shared.turn_active.store(false, Ordering::Release);
    let expired = lock(&shared.mapper).expire_all();
    if !emit(&shared, expired).await {
        return;
    }
    let info = match timeout(EXIT_WAIT, exit).await {
        Ok(Ok(info)) => info,
        Ok(Err(_)) | Err(_) => {
            warn!("the agent's exit status never arrived");
            ExitInfo {
                stderr_tail: shared.transport.stderr_tail(),
                ..ExitInfo::default()
            }
        }
    };
    let _ = shared.events.send(AdapterMessage::Exited(info)).await;
}

impl AgentAdapter for AcpAdapter {
    fn agent(&self) -> &str {
        &self.shared.agent
    }

    fn send_message(&self, text: String) -> BoxFuture<'_, Result<TurnRef, AgentError>> {
        self.send_message_with(text, MessageExtras::default())
    }

    fn send_message_with(
        &self,
        text: String,
        extras: MessageExtras,
    ) -> BoxFuture<'_, Result<TurnRef, AgentError>> {
        Box::pin(async move {
            let shared = &self.shared;
            let MessageExtras { context, images } = extras;
            if !images.is_empty() && !shared.images {
                return Err(AgentError::Unsupported(format!(
                    "{} does not accept images (its handshake offers no `promptCapabilities.image`)",
                    shared.agent
                )));
            }
            if shared.turn_active.swap(true, Ordering::AcqRel) {
                return Err(AgentError::TurnInProgress);
            }
            // What the journal shows as the user's message is what the agent is given; an
            // image is journaled by its header only.
            let journaled = {
                let mut full = text.clone();
                for block in &context {
                    full.push_str("\n\n");
                    full.push_str(&block.text);
                }
                for image in &images {
                    full.push_str("\n\n");
                    full.push_str(&image.header);
                }
                full
            };
            let mut prompt = Vec::new();
            if shared.embedded_context {
                if !text.trim().is_empty() {
                    prompt.push(json!({"type": "text", "text": text}));
                }
                for (index, block) in context.iter().enumerate() {
                    prompt.push(json!({
                        "type": "resource",
                        "resource": {
                            "uri": format!("hutan://context/{}/{}", index + 1, slug(&block.label)),
                            "mimeType": "text/plain",
                            "text": block.text,
                        }
                    }));
                }
            } else {
                let mut quoted = text.clone();
                for block in &context {
                    quoted.push_str("\n\n");
                    quoted.push_str(&block.text);
                }
                prompt.push(json!({"type": "text", "text": quoted}));
            }
            for image in &images {
                use base64::Engine as _;
                prompt.push(json!({
                    "type": "image",
                    "mimeType": image.media_type,
                    "data": base64::engine::general_purpose::STANDARD.encode(image.data.as_slice()),
                }));
            }
            let (turn_id, drafts, working) = {
                let (turn_id, drafts) = lock(&shared.mapper).begin_turn(&journaled);
                (turn_id, drafts, lock(&shared.nodes).turn_started())
            };
            let sent = async {
                if !emit(shared, drafts).await {
                    return Err(AgentError::Exited {
                        detail: String::new(),
                    });
                }
                emit_nodes(shared, vec![working]).await;
                shared
                    .transport
                    .request_ordered(
                        "session/prompt",
                        json!({"sessionId": shared.session_id, "prompt": prompt}),
                        0,
                    )
                    .await
            }
            .await;
            if let Err(error) = sent {
                shared.turn_active.store(false, Ordering::Release);
                let failed = lock(&shared.mapper).fail_turn(error.to_string());
                emit(shared, failed).await;
                let idle = lock(&shared.nodes).turn_ended(true);
                emit_nodes(shared, idle).await;
                return Err(error);
            }
            let provider_turn_id = turn_id.to_string();
            Ok(TurnRef {
                turn_id,
                provider_turn_id,
            })
        })
    }

    fn pid(&self) -> Option<u32> {
        self.shared.transport.pid()
    }

    fn interrupt(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            let shared = &self.shared;
            if !shared.turn_active.load(Ordering::Acquire) {
                return Err(AgentError::NoTurn);
            }
            // The cancellation first, then the answer (`cancelled`) to every request the agent
            // still waits on (ACP § Cancellation: the client must answer them).
            shared
                .transport
                .notify(
                    "session/cancel",
                    Some(json!({"sessionId": shared.session_id})),
                )
                .await?;
            let replies = lock(&shared.mapper).cancel_pending();
            for reply in replies {
                send_reply(shared, reply).await;
            }
            Ok(())
        })
    }

    fn respond(
        &self,
        approval: ApprovalId,
        option_id: String,
    ) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            let shared = &self.shared;
            let answered = lock(&shared.mapper).answer(approval, &option_id);
            let (rpc_id, result) = answered.map_err(|error| match error {
                AnswerError::Unknown => AgentError::UnknownApproval,
                AnswerError::NotOffered(option) => AgentError::OptionNotOffered { option },
                AnswerError::Invalid(problem) => AgentError::BadAnswer(problem),
            })?;
            if let Err(error) = shared.transport.respond(&rpc_id, result).await {
                lock(&shared.mapper).unanswer(approval);
                return Err(error);
            }
            // ACP has no "resolved" notification: the answer reaching the agent is the end of
            // the request.
            let resolved = lock(&shared.mapper).answered(approval);
            let (more_waiting, unblocked) = {
                let waiting = lock(&shared.mapper).has_pending();
                (waiting, lock(&shared.nodes).unblocked())
            };
            emit(shared, resolved.into_iter().collect()).await;
            if !more_waiting {
                emit_nodes(shared, vec![unblocked]).await;
            }
            Ok(())
        })
    }

    fn answer_question(
        &self,
        question: QuestionId,
        response: QuestionResponse,
    ) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            let shared = &self.shared;
            let answered = lock(&shared.mapper).answer_question(question, &response);
            let (rpc_id, result) = answered.map_err(|error| match error {
                AnswerError::Unknown | AnswerError::NotOffered(_) => AgentError::UnknownQuestion,
                AnswerError::Invalid(problem) => AgentError::BadAnswer(problem),
            })?;
            if let Err(error) = shared.transport.respond(&rpc_id, result).await {
                lock(&shared.mapper).unanswer_question(question);
                return Err(error);
            }
            let resolved = lock(&shared.mapper).question_answered(question);
            let (more_waiting, unblocked) = {
                let waiting = lock(&shared.mapper).has_pending();
                (waiting, lock(&shared.nodes).unblocked())
            };
            emit(shared, resolved.into_iter().collect()).await;
            if !more_waiting {
                emit_nodes(shared, vec![unblocked]).await;
            }
            Ok(())
        })
    }

    fn set_model(&self, model: String) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            let shared = &self.shared;
            let choice = lock(&shared.model).clone().ok_or_else(|| {
                AgentError::Unsupported(format!("{} does not offer model switching", shared.agent))
            })?;
            if !choice.models.iter().any(|m| m.id == model) {
                return Err(unknown_model(&model, &choice.models));
            }
            let (method, params) = switch_request(&choice.switch, &shared.session_id, &model)?;
            shared.transport.request(method, params).await?;
            if let Some(current) = lock(&shared.model).as_mut() {
                current.current.clone_from(&model);
            }
            let notice = lock(&shared.mapper).model_switched(&model);
            emit(shared, notice.into_iter().collect()).await;
            Ok(())
        })
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let shared = &self.shared;
            if shared.can_close {
                // Best effort: the agent frees the session; the process ends either way.
                let closing = shared
                    .transport
                    .request("session/close", json!({"sessionId": shared.session_id}));
                let _ = timeout(CLOSE_WAIT, closing).await;
            }
            shared.transport.shutdown().await;
        })
    }
}

/// `label` as a path segment of a resource URI.
fn slug(label: &str) -> String {
    let slug: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(40)
        .collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "context".to_owned()
    } else {
        slug.to_owned()
    }
}

/// The models an agent offers, from a throwaway session in `cwd` (started, asked, closed).
pub async fn list_models(config: &AcpConfig, cwd: PathBuf) -> Result<Vec<ModelInfo>, AgentError> {
    let spec = StartSpec {
        cwd,
        model: None,
        approval_policy: hutan_core::ApprovalPolicy::default(),
        sandbox: hutan_core::SandboxMode::default(),
        resume_thread_id: None,
        developer_instructions: None,
    };
    let session = AcpAdapter::start(config, &spec, None, &[]).await?;
    let models = session.models.clone();
    session.adapter.shutdown().await;
    Ok(models)
}
