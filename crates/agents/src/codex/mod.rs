//! The Codex adapter: `codex app-server` over stdio JSON-RPC (ADR 0005, `docs/agents.md`).
//!
//! [`CodexAppServerAdapter::start`] spawns the process, initializes it, checks its version
//! against [`SUPPORTED_VERSIONS`], opens or resumes a thread, and returns the adapter together
//! with the stream of [`AdapterMessage`]s. Notifications become [`AgentEventDraft`]s
//! ([`mapper`]); approval requests of the server become pending requests that
//! [`AgentAdapter::respond`] answers.

mod mapper;
mod nodes;
mod wire;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use hutan_core::{ApprovalId, ForkSupport, ModelInfo, QuestionId, QuestionResponse, TurnId};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::adapter::{
    AdapterMessage, AgentAdapter, ExitInfo, MessageExtras, StartSpec, Started, TurnRef,
};
use crate::error::AgentError;
use crate::jsonrpc::{Incoming, Launch, Transport};

pub use mapper::{AnswerError, Mapped, Mapper, QuestionReply, Reply};
pub use nodes::{NodeFeed, TREE_SOURCE};

/// Codex versions this adapter is written against: `>=0.160, <0.170`.
pub const SUPPORTED_VERSIONS: &str = ">=0.160, <0.170";
const MIN_VERSION: (u32, u32) = (0, 160);
const MAX_VERSION_EXCLUSIVE: (u32, u32) = (0, 170);

/// Events buffered between the adapter and the daemon before the adapter waits.
const EVENT_QUEUE: usize = 512;
/// How long the adapter waits for the exit status after the output closed.
const EXIT_WAIT: Duration = Duration::from_secs(3);
/// Catalog pages followed at most.
const MAX_MODEL_PAGES: usize = 10;

/// Where the Codex executable is and how to start its app server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConfig {
    pub program: PathBuf,
    /// Arguments of the program; the default is `app-server`. Overrides go here too
    /// (`app-server -c model_reasoning_effort="low"`).
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            program: PathBuf::from("codex"),
            args: vec!["app-server".to_owned()],
            env: Vec::new(),
        }
    }
}

impl CodexConfig {
    fn launch(&self, cwd: Option<PathBuf>) -> Launch {
        Launch {
            program: self.program.clone(),
            args: self.args.clone(),
            cwd,
            env: self.env.clone(),
        }
    }
}

/// The version in a server's `userAgent` (`<originator>/<version> (<platform>) ...`), checked
/// against [`SUPPORTED_VERSIONS`]. Returns the version text.
pub fn check_version(user_agent: &str) -> Result<String, AgentError> {
    let version = user_agent
        .split_once('/')
        .map(|(_, rest)| rest.split([' ', '(']).next().unwrap_or(""))
        .unwrap_or("");
    let mut numbers = version
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .map(|digits| digits.parse::<u32>().ok());
    let (Some(Some(major)), Some(Some(minor))) = (numbers.next(), numbers.next()) else {
        return Err(AgentError::UnknownVersion(user_agent.to_owned()));
    };
    if (major, minor) < MIN_VERSION || (major, minor) >= MAX_VERSION_EXCLUSIVE {
        return Err(AgentError::UnsupportedVersion {
            found: version.to_owned(),
            supported: SUPPORTED_VERSIONS.to_owned(),
        });
    }
    Ok(version.to_owned())
}

/// Subagent threads whose running turn is remembered at most; past this the memory starts over.
const MAX_TRACKED_THREADS: usize = 256;

#[derive(Debug, Default)]
struct TurnTracker {
    current: Option<String>,
    last_done: Option<String>,
    /// Running turn per subagent thread.
    threads: HashMap<String, String>,
}

#[derive(Debug)]
struct Shared {
    transport: Arc<Transport>,
    thread_id: String,
    mapper: Mutex<Mapper>,
    nodes: Mutex<NodeFeed>,
    turn: Mutex<TurnTracker>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The guarded data is never left half-updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One Codex app-server process with one open thread.
#[derive(Debug)]
pub struct CodexAppServerAdapter {
    shared: Arc<Shared>,
}

/// What a successful [`CodexAppServerAdapter::start`] returns.
#[derive(Debug)]
pub struct CodexSession {
    pub adapter: Arc<CodexAppServerAdapter>,
    pub started: Started,
    pub events: mpsc::Receiver<AdapterMessage>,
}

async fn call<P: Serialize, R: DeserializeOwned>(
    transport: &Transport,
    method: &str,
    params: &P,
) -> Result<R, AgentError> {
    let params = serde_json::to_value(params)
        .map_err(|e| AgentError::Protocol(format!("encoding `{method}`: {e}")))?;
    let result = transport.request(method, params).await?;
    serde_json::from_value(result).map_err(|e| {
        AgentError::Protocol(format!("`{method}` answered with an unexpected shape: {e}"))
    })
}

/// `initialize`, version check, `initialized`.
async fn handshake(transport: &Transport) -> Result<String, AgentError> {
    let init = wire::InitializeParams {
        client_info: wire::ClientInfo {
            name: "hutan",
            title: "Hutan",
            version: env!("CARGO_PKG_VERSION"),
        },
        capabilities: wire::InitializeCapabilities {
            experimental_api: false,
        },
    };
    let result: wire::InitializeResult = call(transport, "initialize", &init).await?;
    let version = check_version(&result.user_agent)?;
    transport.notify("initialized", None).await?;
    Ok(version)
}

/// The agent's model catalog, newest page first, hidden models included.
async fn model_catalog(transport: &Transport) -> Result<Vec<ModelInfo>, AgentError> {
    let mut models = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_MODEL_PAGES {
        let page: wire::ModelListResult = call(
            transport,
            "model/list",
            &wire::ModelListParams {
                cursor: cursor.as_deref(),
            },
        )
        .await?;
        models.extend(page.data.into_iter().map(|m| {
            ModelInfo {
                id: m.model.unwrap_or(m.id),
                display_name: m.display_name,
                description: m.description,
                is_default: m.is_default,
                hidden: m.hidden,
                default_reasoning_effort: m.default_reasoning_effort,
                reasoning_efforts: m
                    .supported_reasoning_efforts
                    .into_iter()
                    .map(|e| e.reasoning_effort)
                    .collect(),
            }
        }));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    Ok(models)
}

/// The catalog's choice: the requested model when the catalog offers it, else the default.
fn choose_model(models: &[ModelInfo], requested: Option<&str>) -> Result<String, AgentError> {
    let available = || {
        models
            .iter()
            .filter(|m| !m.hidden)
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match requested {
        Some(model) => models
            .iter()
            .find(|m| m.id == model)
            .map(|m| m.id.clone())
            .ok_or_else(|| AgentError::UnknownModel {
                model: model.to_owned(),
                available: available(),
            }),
        None => models
            .iter()
            .find(|m| m.is_default && !m.hidden)
            .or_else(|| models.iter().find(|m| !m.hidden))
            .map(|m| m.id.clone())
            .ok_or_else(|| AgentError::UnknownModel {
                model: "(catalog default)".to_owned(),
                available: "none".to_owned(),
            }),
    }
}

/// The models a fresh Codex process offers (starts one, asks, ends it).
pub async fn list_models(config: &CodexConfig) -> Result<Vec<ModelInfo>, AgentError> {
    let (transport, _incoming, _exit) = Transport::spawn(&config.launch(None))?;
    let outcome = async {
        handshake(&transport).await?;
        model_catalog(&transport).await
    }
    .await;
    transport.shutdown().await;
    outcome
}

/// The login Codex reports: names and plan only (`account/read`; no e-mail address, no token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAccount {
    /// `chatgpt`, `apiKey`, `amazonBedrock`; `None` when Codex has no login.
    pub kind: Option<String>,
    /// The ChatGPT plan (`plus`, `pro`, ...) when Codex reports one.
    pub plan: Option<String>,
}

/// Who Codex is logged in as, by its own account: starts an app server, asks `account/read`,
/// ends it.
pub async fn read_account(config: &CodexConfig) -> Result<CodexAccount, AgentError> {
    let (transport, _incoming, _exit) = Transport::spawn(&config.launch(None))?;
    let outcome = async {
        handshake(&transport).await?;
        let result: Value =
            call(&transport, "account/read", &json!({"refreshToken": false})).await?;
        let account = result.get("account").filter(|a| !a.is_null());
        Ok(CodexAccount {
            kind: account
                .and_then(|a| a.get("type"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            plan: account
                .and_then(|a| a.get("planType"))
                .and_then(Value::as_str)
                .filter(|plan| *plan != "unknown")
                .map(str::to_owned),
        })
    }
    .await;
    transport.shutdown().await;
    outcome
}

/// The account's limit windows, by Codex's own `account/rateLimits/read`: starts an app server,
/// asks, ends it. `Ok(None)` when Codex reports none (an API-key login has no plan windows).
pub async fn read_rate_limits(
    config: &CodexConfig,
) -> Result<Option<crate::usage::RateLimits>, AgentError> {
    let (transport, _incoming, _exit) = Transport::spawn(&config.launch(None))?;
    let outcome = async {
        handshake(&transport).await?;
        let result: Value = call(&transport, "account/rateLimits/read", &json!({})).await?;
        Ok(crate::usage::codex_rate_limits(&result))
    }
    .await;
    transport.shutdown().await;
    outcome
}

impl CodexAppServerAdapter {
    /// Starts the process and opens (or resumes) the conversation `spec` describes.
    pub async fn start(config: &CodexConfig, spec: &StartSpec) -> Result<CodexSession, AgentError> {
        let (transport, incoming, exit) = Transport::spawn(&config.launch(Some(spec.cwd.clone())))?;
        match Self::open(&transport, spec).await {
            Ok((started, mapper)) => Ok(Self::run(transport, incoming, exit, started, mapper)),
            Err(error) => {
                transport.shutdown().await;
                Err(error)
            }
        }
    }

    async fn open(
        transport: &Transport,
        spec: &StartSpec,
    ) -> Result<(Started, Mapper), AgentError> {
        handshake(transport).await?;
        let cwd = spec.cwd.to_string_lossy();
        let policy = spec.approval_policy.as_str();
        let sandbox = spec.sandbox.as_str();
        let (thread_id, model, resumed, reported) = match (&spec.fork, &spec.resume_thread_id) {
            (Some(from), _) => {
                // A fork is the agent's own copy of the parent's thread, cut at the end of a
                // turn it names (`lastTurnId`, inclusive; it cannot be running).
                let last_turn_id = match &from.point {
                    None => None,
                    Some(point) => Some(point.provider_turn_id.as_deref().ok_or_else(|| {
                        AgentError::Unsupported(
                            "Codex cuts a fork at a turn by the turn's own id, which this turn \
                             has no record of"
                                .to_owned(),
                        )
                    })?),
                };
                let result: wire::ThreadResult = call(
                    transport,
                    "thread/fork",
                    &wire::ThreadForkParams {
                        thread_id: &from.thread_id,
                        last_turn_id,
                        model: spec.model.as_deref(),
                        cwd: &cwd,
                        approval_policy: policy,
                        sandbox,
                        approvals_reviewer: "user",
                        exclude_turns: true,
                        developer_instructions: spec.developer_instructions.as_deref(),
                    },
                )
                .await?;
                let reported = result.reported_safety();
                let model = result
                    .model
                    .or_else(|| spec.model.clone())
                    .unwrap_or_default();
                (result.thread.id, model, false, reported)
            }
            (None, Some(thread)) => {
                let result: wire::ThreadResult = call(
                    transport,
                    "thread/resume",
                    &wire::ThreadResumeParams {
                        thread_id: thread,
                        model: spec.model.as_deref(),
                        cwd: &cwd,
                        approval_policy: policy,
                        sandbox,
                        approvals_reviewer: "user",
                        exclude_turns: true,
                        developer_instructions: spec.developer_instructions.as_deref(),
                    },
                )
                .await?;
                let reported = result.reported_safety();
                let model = result
                    .model
                    .or_else(|| spec.model.clone())
                    .unwrap_or_default();
                // The server answers for the thread it resumed; it is the one we asked for.
                (thread.clone(), model, true, reported)
            }
            (None, None) => {
                let catalog = model_catalog(transport).await?;
                let model = choose_model(&catalog, spec.model.as_deref())?;
                let result: wire::ThreadResult = call(
                    transport,
                    "thread/start",
                    &wire::ThreadStartParams {
                        model: &model,
                        cwd: &cwd,
                        approval_policy: policy,
                        sandbox,
                        approvals_reviewer: "user",
                        developer_instructions: spec.developer_instructions.as_deref(),
                    },
                )
                .await?;
                let reported = result.reported_safety();
                (result.thread.id, model, false, reported)
            }
        };
        let mapper = Mapper::new(thread_id.clone());
        Ok((
            Started {
                model,
                thread_id,
                resumed,
                // The app-server takes `localImage` input; whether the model reads it is the
                // model's business.
                images: None,
                // Codex says its approval policy and sandbox instead (`ChatInfo`).
                mode: None,
                reported,
                // The app server cuts at any turn (`thread/fork` `lastTurnId`).
                fork: Some(ForkSupport::Exact),
            },
            mapper,
        ))
    }

    fn run(
        transport: Arc<Transport>,
        incoming: mpsc::Receiver<Incoming>,
        exit: oneshot::Receiver<ExitInfo>,
        started: Started,
        mut mapper: Mapper,
    ) -> CodexSession {
        let (events_tx, events) = mpsc::channel(EVENT_QUEUE);
        // The channel is empty, so this cannot wait.
        let locator = mapper.resume_locator(&started.model);
        let _ = events_tx.try_send(AdapterMessage::Event(Box::new(locator)));
        let mut root = NodeFeed::new(started.thread_id.clone()).started(&started.model);
        // The main agent is the app-server process; its subagent threads live inside it and
        // have no process of their own.
        root.pid = transport.pid();
        let _ = events_tx.try_send(AdapterMessage::Node(Box::new(root)));
        let shared = Arc::new(Shared {
            transport,
            thread_id: started.thread_id.clone(),
            mapper: Mutex::new(mapper),
            nodes: Mutex::new(NodeFeed::new(started.thread_id.clone())),
            turn: Mutex::new(TurnTracker::default()),
        });
        tokio::spawn(pump(shared.clone(), incoming, exit, events_tx));
        CodexSession {
            adapter: Arc::new(Self { shared }),
            started,
            events,
        }
    }
}

/// Turns what the process says into [`AdapterMessage`]s until it is gone.
async fn pump(
    shared: Arc<Shared>,
    mut incoming: mpsc::Receiver<Incoming>,
    exit: oneshot::Receiver<ExitInfo>,
    events: mpsc::Sender<AdapterMessage>,
) {
    let send = |drafts: Vec<hutan_core::AgentEventDraft>| {
        let events = events.clone();
        async move {
            for draft in drafts {
                if events
                    .send(AdapterMessage::Event(Box::new(draft)))
                    .await
                    .is_err()
                {
                    return false;
                }
            }
            true
        }
    };
    while let Some(message) = incoming.recv().await {
        match message {
            Incoming::Notification { method, params } => {
                // The account's limit windows are not part of the conversation: they go to
                // the daemon's usage records, and the mapper drops the notification.
                if method == "account/rateLimits/updated"
                    && let Some(limits) = crate::usage::codex_rate_limits(&params)
                    && events
                        .send(AdapterMessage::Limits(Box::new(limits)))
                        .await
                        .is_err()
                {
                    shared.transport.shutdown().await;
                    return;
                }
                track_turn(&shared, &method, &params);
                let updates = lock(&shared.nodes).notification(&method, &params);
                for update in updates {
                    if events
                        .send(AdapterMessage::Node(Box::new(update)))
                        .await
                        .is_err()
                    {
                        shared.transport.shutdown().await;
                        return;
                    }
                }
                let drafts = lock(&shared.mapper).notification(&method, params);
                if !send(drafts).await {
                    shared.transport.shutdown().await;
                    return;
                }
            }
            Incoming::Request { id, method, params } => {
                let Mapped { drafts, reply } = lock(&shared.mapper).request(id, &method, params);
                if !send(drafts).await {
                    shared.transport.shutdown().await;
                    return;
                }
                if let Some(Reply { id, outcome }) = reply {
                    let sent = match outcome {
                        Ok(result) => shared.transport.respond(&id, result).await,
                        Err((code, message)) => {
                            shared.transport.respond_error(&id, code, &message).await
                        }
                    };
                    if let Err(error) = sent {
                        debug!(%error, "could not answer a request of the agent");
                    }
                }
            }
            // Codex's adapter makes no ordered requests.
            Incoming::Completed { .. } => {}
            Incoming::Closed => break,
        }
    }
    let expired = lock(&shared.mapper).expire_all();
    if !send(expired).await {
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
    let _ = events.send(AdapterMessage::Exited(info)).await;
}

/// Keeps the provider id of the turn in progress (what `turn/interrupt` needs): of the
/// conversation's own thread, and of each subagent thread.
fn track_turn(shared: &Shared, method: &str, params: &Value) {
    let id = params
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let thread = params.pointer("/threadId").and_then(Value::as_str);
    let mut turn = lock(&shared.turn);
    if let Some(thread) = thread.filter(|thread| *thread != shared.thread_id) {
        match (method, id) {
            ("turn/started", Some(id)) => {
                if turn.threads.len() >= MAX_TRACKED_THREADS {
                    turn.threads.clear();
                }
                turn.threads.insert(thread.to_owned(), id);
            }
            ("turn/completed", Some(id)) if turn.threads.get(thread) == Some(&id) => {
                turn.threads.remove(thread);
            }
            _ => {}
        }
        return;
    }
    match (method, id) {
        ("turn/started", Some(id)) if turn.last_done.as_deref() != Some(id.as_str()) => {
            turn.current = Some(id);
        }
        ("turn/completed", Some(id)) => {
            if turn.current.as_deref() == Some(id.as_str()) {
                turn.current = None;
            }
            turn.last_done = Some(id);
        }
        _ => {}
    }
}

impl AgentAdapter for CodexAppServerAdapter {
    fn agent(&self) -> &str {
        "codex"
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
            // Text attachments reach Codex as quoted text the daemon appended to `text`; only
            // images are input items of their own, and they are read from disk.
            if !extras.context.is_empty() {
                return Err(AgentError::Unsupported(
                    "codex takes text attachments quoted in the message, not as context blocks"
                        .to_owned(),
                ));
            }
            let mut paths = Vec::with_capacity(extras.images.len());
            for image in &extras.images {
                let path = image.path.as_ref().and_then(|path| path.to_str());
                paths.push(path.ok_or_else(|| {
                    AgentError::Unsupported(
                        "codex reads images from a file with a UTF-8 path".to_owned(),
                    )
                })?);
            }
            let mut input = vec![wire::UserInput::Text {
                text: &text,
                text_elements: Vec::new(),
            }];
            input.extend(
                paths
                    .into_iter()
                    .map(|path| wire::UserInput::LocalImage { path }),
            );
            let started: wire::TurnStartResult = call(
                &shared.transport,
                "turn/start",
                &wire::TurnStartParams {
                    thread_id: &shared.thread_id,
                    input,
                },
            )
            .await?;
            let provider_turn_id = started.turn.id;
            {
                let mut turn = lock(&shared.turn);
                if turn.last_done.as_deref() != Some(provider_turn_id.as_str()) {
                    turn.current = Some(provider_turn_id.clone());
                }
            }
            let turn_id: TurnId = lock(&shared.mapper).turn_id(&provider_turn_id);
            Ok(TurnRef {
                turn_id,
                provider_turn_id,
            })
        })
    }

    fn interrupt(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move {
            let shared = &self.shared;
            let turn = lock(&shared.turn)
                .current
                .clone()
                .ok_or(AgentError::NoTurn)?;
            let _: Value = call(
                &shared.transport,
                "turn/interrupt",
                &wire::TurnInterruptParams {
                    thread_id: &shared.thread_id,
                    turn_id: &turn,
                },
            )
            .await?;
            Ok(())
        })
    }

    fn can_steer(&self) -> bool {
        true
    }

    fn steer(&self, text: String) -> BoxFuture<'_, Result<TurnId, AgentError>> {
        Box::pin(async move {
            let shared = &self.shared;
            let turn = lock(&shared.turn)
                .current
                .clone()
                .ok_or(AgentError::NoTurn)?;
            let steered: wire::TurnSteerResult = call(
                &shared.transport,
                "turn/steer",
                &wire::TurnSteerParams {
                    thread_id: &shared.thread_id,
                    input: vec![wire::UserInput::Text {
                        text: &text,
                        text_elements: Vec::new(),
                    }],
                    expected_turn_id: &turn,
                },
            )
            .await?;
            Ok(lock(&shared.mapper).turn_id(&steered.turn_id))
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
            let QuestionReply {
                rpc_id,
                result,
                interrupt,
            } = answered.map_err(|error| match error {
                AnswerError::Unknown | AnswerError::NotOffered(_) => AgentError::UnknownQuestion,
                AnswerError::Invalid(problem) => AgentError::BadAnswer(problem),
            })?;
            if let Err(error) = shared.transport.respond(&rpc_id, result).await {
                lock(&shared.mapper).unanswer_question(question);
                return Err(error);
            }
            if interrupt {
                // Cancelling a question stops the turn, as cancelling an approval does; a turn
                // that is already over has nothing to stop.
                match self.interrupt().await {
                    Ok(()) | Err(AgentError::NoTurn) => {}
                    Err(error) => debug!(%error, "could not stop the turn of a cancelled question"),
                }
            }
            Ok(())
        })
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.shared.transport.shutdown().await })
    }

    fn pid(&self) -> Option<u32> {
        self.shared.transport.pid()
    }

    fn interrupt_thread(&self, thread_id: &str) -> BoxFuture<'_, Result<(), AgentError>> {
        let thread = thread_id.to_owned();
        Box::pin(async move {
            let shared = &self.shared;
            if thread == shared.thread_id {
                return self.interrupt().await;
            }
            let turn = lock(&shared.turn)
                .threads
                .get(&thread)
                .cloned()
                .ok_or(AgentError::NoTurn)?;
            let _: Value = call(
                &shared.transport,
                "turn/interrupt",
                &wire::TurnInterruptParams {
                    thread_id: &thread,
                    turn_id: &turn,
                },
            )
            .await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_inside_the_range_pass() {
        assert_eq!(
            check_version("hutan/0.162.1 (Mac OS 26.6; arm64) x").unwrap(),
            "0.162.1"
        );
        assert!(check_version("hutan/0.160.0").is_ok());
        assert!(check_version("hutan/0.169.9-alpha.2 (Linux)").is_ok());
    }

    #[test]
    fn versions_outside_the_range_name_both_versions() {
        for found in ["0.144.2", "0.159.9", "0.170.0", "1.0.0"] {
            let error = check_version(&format!("hutan/{found} (Mac OS)")).unwrap_err();
            let text = error.to_string();
            assert!(
                text.contains(found) && text.contains(">=0.160, <0.170"),
                "{text}"
            );
        }
    }

    #[test]
    fn unreadable_versions_are_refused() {
        for agent in ["", "codex", "hutan/abc", "hutan/0 (x)"] {
            assert!(
                matches!(check_version(agent), Err(AgentError::UnknownVersion(_))),
                "{agent}"
            );
        }
    }

    fn model(id: &str, is_default: bool, hidden: bool) -> ModelInfo {
        ModelInfo {
            id: id.to_owned(),
            display_name: id.to_owned(),
            description: String::new(),
            is_default,
            hidden,
            default_reasoning_effort: None,
            reasoning_efforts: Vec::new(),
        }
    }

    #[test]
    fn the_catalog_picks_the_model_not_a_config_default() {
        let catalog = [
            model("a", false, false),
            model("b", true, false),
            model("c", false, true),
        ];
        assert_eq!(choose_model(&catalog, None).unwrap(), "b");
        assert_eq!(choose_model(&catalog, Some("a")).unwrap(), "a");
        let error = choose_model(&catalog, Some("gpt-nope"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("gpt-nope") && error.contains("a, b"),
            "{error}"
        );
        assert!(
            !error.contains(", c"),
            "hidden models are not advertised: {error}"
        );
    }
}
