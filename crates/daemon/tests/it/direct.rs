//! Direct model chat end to end: a real daemon on a temporary `HUTAN_HOME`, real client
//! connections, the in-memory credential store, and local mock HTTP servers standing in for the
//! providers (`hutan_gateway::testing`). No real provider is ever contacted.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use common::{TestDaemon, WAIT, remote_code, wait_event};
use hutan_client::Client;
use hutan_core::agent::MessageRole;
use hutan_core::{AgentEvent, AgentEventKind, Seq, SessionId, SessionState};
use hutan_daemon::GatewayConfig;
use hutan_gateway::testing::{MockServer, Reply};
use hutan_gateway::{CredentialStore, HttpConfig, MemoryStore, ProbeTarget, ProviderKind};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{
    ChatCreate, ChatHistory, ChatHistoryRequest, ChatInterrupt, ChatRetry, ChatSend, ChatSent,
};
use hutan_protocol::messages::{
    Empty, Event, Request, SessionInfo, SessionList, SessionListFilter, SessionResult,
};
use hutan_protocol::provider::{
    AGENT_DIRECT, ChatSetModel, ProviderAdd, ProviderDiscovery, ProviderInfo, ProviderList,
    ProviderModels, ProviderRef, ProviderResult, ProviderSetCredential, ProviderUpdate,
};
use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

const OPENAI_PATH: &str = "/v1/chat/completions";

// Log capture ---------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;
    fn make_writer(&'a self) -> LogCapture {
        self.clone()
    }
}

/// Every log line of every daemon of this test binary, at TRACE level, kept in memory so a test
/// can scan them for a secret it knows to be unique.
static LOGS: LazyLock<LogCapture> = LazyLock::new(|| {
    let capture = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    // The daemon harness tries to install its own subscriber later and finds this one.
    let _ = tracing::subscriber::set_global_default(subscriber);
    capture
});

fn logged() -> String {
    String::from_utf8_lossy(&LOGS.0.lock()).into_owned()
}

// Harness -------------------------------------------------------------------------------------

fn gateway_config(store: &Arc<MemoryStore>, targets: Vec<ProbeTarget>) -> GatewayConfig {
    GatewayConfig {
        credentials: Some(store.clone() as Arc<dyn CredentialStore>),
        http: HttpConfig {
            no_proxy: true,
            connect_retries: 1,
            retry_backoff: Duration::from_millis(5),
            ..HttpConfig::default()
        },
        discovery: Some(targets),
    }
}

struct Direct {
    daemon: TestDaemon,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
    /// Everything seen by the client, as JSON text, for secret scans.
    seen: Arc<Mutex<String>>,
    capture: broadcast::Receiver<(Seq, Event)>,
    store: Arc<MemoryStore>,
    work: tempfile::TempDir,
    targets: Vec<ProbeTarget>,
}

impl Direct {
    async fn start() -> Self {
        Self::start_with("", Vec::new()).await
    }

    async fn start_with(toml: &str, targets: Vec<ProbeTarget>) -> Self {
        LazyLock::force(&LOGS);
        let store = Arc::new(MemoryStore::new());
        let config = gateway_config(&store, targets.clone());
        let daemon = TestDaemon::start_with(toml, move |c| c.gateway = config).await;
        Self::attach(
            daemon,
            store,
            tempfile::tempdir().unwrap(),
            targets,
            Default::default(),
        )
        .await
    }

    async fn attach(
        daemon: TestDaemon,
        store: Arc<MemoryStore>,
        work: tempfile::TempDir,
        targets: Vec<ProbeTarget>,
        seen: Arc<Mutex<String>>,
    ) -> Self {
        let (client, events) = daemon.connect_subscribed().await;
        let capture = client.events();
        Self {
            daemon,
            client,
            events,
            seen,
            capture,
            store,
            work,
            targets,
        }
    }

    /// A daemon restart on the same home, credential store and providers.
    async fn restart(self) -> Self {
        let Self {
            daemon,
            store,
            work,
            targets,
            seen,
            ..
        } = self;
        let config = gateway_config(&store, targets.clone());
        let daemon = daemon.restart_with(move |c| c.gateway = config).await;
        Self::attach(daemon, store, work, targets, seen).await
    }

    fn note<T: Serialize>(&self, value: &T) {
        let mut seen = self.seen.lock();
        seen.push_str(&serde_json::to_string(value).unwrap());
        seen.push('\n');
    }

    async fn call<T: serde::de::DeserializeOwned + Serialize>(&self, request: Request) -> T {
        let result: T = self.client.call(request).await.unwrap();
        self.note(&result);
        result
    }

    async fn try_call<T: serde::de::DeserializeOwned + Serialize>(
        &self,
        request: Request,
    ) -> Result<T, hutan_client::ClientError> {
        let result = self.client.call::<T>(request).await;
        if let Ok(value) = &result {
            self.note(value);
        }
        result
    }

    async fn add_provider(
        &self,
        kind: &str,
        name: &str,
        base_url: Option<String>,
        default_model: Option<&str>,
    ) -> ProviderInfo {
        self.call::<ProviderResult>(Request::ProviderAdd(ProviderAdd {
            kind: kind.to_owned(),
            name: name.to_owned(),
            base_url,
            default_model: default_model.map(str::to_owned),
        }))
        .await
        .provider
    }

    async fn set_key(&self, provider: &str, secret: &str) -> ProviderInfo {
        self.call::<ProviderResult>(Request::ProviderSetCredential(ProviderSetCredential {
            provider_id: provider.to_owned(),
            secret: secret.to_owned(),
        }))
        .await
        .provider
    }

    fn create_request(&self, provider: &str, model: Option<&str>, system: Option<&str>) -> Request {
        Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: self.work.path().to_owned(),
            agent: AGENT_DIRECT.to_owned(),
            model: model.map(str::to_owned),
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: Some(provider.to_owned()),
            system_prompt: system.map(str::to_owned),
        })
    }

    async fn create(
        &self,
        provider: &str,
        model: Option<&str>,
        system: Option<&str>,
    ) -> SessionInfo {
        self.call::<SessionResult>(self.create_request(provider, model, system))
            .await
            .session
    }

    async fn send(&self, id: SessionId, text: &str) -> ChatSent {
        self.call(Request::ChatSend(ChatSend {
            session_id: id,
            text: text.to_owned(),
            attachments: vec![],
        }))
        .await
    }

    async fn history(&self, id: SessionId) -> ChatHistory {
        self.call(Request::ChatHistory(ChatHistoryRequest {
            session_id: id,
            before_seq: None,
            limit: 1000,
        }))
        .await
    }

    async fn session(&self, id: SessionId) -> SessionInfo {
        self.call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .sessions
            .into_iter()
            .find(|s| s.id == id)
            .expect("session listed")
    }

    async fn wait_state(&self, id: SessionId, state: SessionState) -> SessionInfo {
        timeout(WAIT, async {
            loop {
                let session = self.session(id).await;
                if session.state == state {
                    return session;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("session never reached {state:?}"))
    }

    /// Chat events of the session up to and including the end of the turn.
    async fn turn_events(&mut self, id: SessionId) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        loop {
            let event = wait_event(&mut self.events, "the turn to end", |e| match e {
                Event::ChatEvent { session_id, event } if *session_id == id => {
                    Some((**event).clone())
                }
                _ => None,
            })
            .await;
            let end = event.kind.ends_turn();
            out.push(event);
            if end {
                return out;
            }
        }
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

fn names(events: &[AgentEvent]) -> Vec<&'static str> {
    events.iter().map(|e| e.kind.name()).collect()
}

fn message_texts(events: &[AgentEvent], role: MessageRole) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::MessageEnd { role: r, text, .. } if *r == role => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn delta_text(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::MessageDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// An OpenAI-compatible stream of `chunks` that ends with `usage` (prompt, completion tokens).
fn openai_reply(chunks: &[&str], usage: (u64, u64)) -> Reply {
    let mut body: Vec<String> = chunks
        .iter()
        .map(|c| {
            format!(
                "data: {}\n\n",
                serde_json::json!({"choices":[{"index":0,"delta":{"content": c}}]})
            )
        })
        .collect();
    body.push(
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n"
            .to_owned(),
    );
    body.push(format!(
        "data: {}\n\n",
        serde_json::json!({"choices":[],"usage":{"prompt_tokens":usage.0,"completion_tokens":usage.1,"total_tokens":usage.0+usage.1}})
    ));
    body.push("data: [DONE]\n\n".to_owned());
    Reply::sse(body)
}

/// The messages of the nth request to `path` as `(role, content)`.
fn sent_messages(mock: &MockServer, path: &str, n: usize) -> Vec<(String, String)> {
    let body = mock.requests_to(path)[n].json();
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["role"].as_str().unwrap().to_owned(),
                m["content"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(r, c)| ((*r).to_owned(), (*c).to_owned()))
        .collect()
}

fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            files.extend(files_under(&path));
        } else if kind.is_file() {
            files.push(path);
        }
    }
    files
}

// Providers -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_advertises_direct_chat() {
    let t = Direct::start().await;
    assert!(
        t.client
            .welcome()
            .capabilities
            .iter()
            .any(|c| c == "chat.direct")
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn providers_are_added_updated_listed_and_survive_a_restart() {
    let t = Direct::start().await;
    let compat = t
        .add_provider(
            "openai-compatible",
            "My Router!",
            Some("http://127.0.0.1:1234/v1/".into()),
            Some("m-1"),
        )
        .await;
    assert_eq!(compat.id, "my-router");
    assert_eq!(compat.kind, "openai-compatible");
    assert_eq!(
        compat.base_url, "http://127.0.0.1:1234/v1",
        "no trailing slash"
    );
    assert_eq!(compat.default_model.as_deref(), Some("m-1"));
    assert!(!compat.has_credential);

    // Kind defaults: name and base URL.
    let ollama = t.add_provider("ollama", "ollama", None, None).await;
    assert_eq!(ollama.id, "ollama");
    assert_eq!(ollama.base_url, "http://127.0.0.1:11434");
    let openai = t.add_provider("openai", "OpenAI", None, None).await;
    assert_eq!(openai.base_url, "https://api.openai.com/v1");
    let anthropic = t.add_provider("anthropic", "Anthropic", None, None).await;
    assert_eq!(anthropic.base_url, "https://api.anthropic.com");
    // A second provider with a taken name gets another id.
    let again = t.add_provider("ollama", "Ollama", None, None).await;
    assert_eq!(again.id, "ollama-2");

    let updated = t
        .call::<ProviderResult>(Request::ProviderUpdate(ProviderUpdate {
            provider_id: "my-router".into(),
            name: Some("Router".into()),
            base_url: Some("http://127.0.0.1:4321/v1".into()),
            default_model: Some(String::new()),
        }))
        .await
        .provider;
    assert_eq!(updated.id, "my-router", "the id never changes");
    assert_eq!(updated.name, "Router");
    assert_eq!(updated.base_url, "http://127.0.0.1:4321/v1");
    assert_eq!(
        updated.default_model, None,
        "an empty default model clears it"
    );

    let keyed = t.set_key("openai", "sk-persist-0001").await;
    assert!(keyed.has_credential);
    let listed = t
        .call::<ProviderList>(Request::ProviderList(Empty {}))
        .await;
    let ids: Vec<&str> = listed.providers.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        ["my-router", "ollama", "openai", "anthropic", "ollama-2"]
    );
    let flags: Vec<bool> = listed.providers.iter().map(|p| p.has_credential).collect();
    assert_eq!(flags, [false, false, true, false, false]);

    // A restart: same providers, same order, the credential still there.
    let t = t.restart().await;
    let after = t
        .call::<ProviderList>(Request::ProviderList(Empty {}))
        .await;
    assert_eq!(after.providers.len(), 5);
    assert_eq!(after.providers[0].name, "Router");
    assert_eq!(after.providers[0].base_url, "http://127.0.0.1:4321/v1");
    assert_eq!(after.providers[0].default_model, None);
    assert_eq!(
        after
            .providers
            .iter()
            .map(|p| p.created_at_ms)
            .collect::<Vec<_>>(),
        listed
            .providers
            .iter()
            .map(|p| p.created_at_ms)
            .collect::<Vec<_>>()
    );
    assert!(after.providers[2].has_credential);
    assert!(!after.providers[1].has_credential);

    // Removing a provider removes its credential too.
    t.call::<Empty>(Request::ProviderRemove(ProviderRef {
        provider_id: "openai".into(),
    }))
    .await;
    assert!(t.store.get("openai").unwrap().is_none());
    let t = t.restart().await;
    let left = t
        .call::<ProviderList>(Request::ProviderList(Empty {}))
        .await;
    assert_eq!(left.providers.len(), 4);
    assert!(left.providers.iter().all(|p| p.id != "openai"));

    // Clearing a credential keeps the provider.
    t.set_key("anthropic", "sk-ant-persist-0002").await;
    let cleared = t
        .call::<ProviderResult>(Request::ProviderClearCredential(ProviderRef {
            provider_id: "anthropic".into(),
        }))
        .await;
    assert!(!cleared.provider.has_credential);
    assert!(t.store.get("anthropic").unwrap().is_none());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_requests_are_validated() {
    let t = Direct::start().await;
    let add = |kind: &str, name: &str, base_url: Option<&str>| {
        Request::ProviderAdd(ProviderAdd {
            kind: kind.to_owned(),
            name: name.to_owned(),
            base_url: base_url.map(str::to_owned),
            default_model: None,
        })
    };
    let bad = [
        add("gpt", "x", None),
        add("openai-compatible", "x", None),
        add("openai", "   ", None),
        add("openai", "x", Some("not a url")),
        add("openai", "x", Some("ftp://host/v1")),
        add("openai", "x", Some("https://user:hunter2@host/v1")),
        add("openai", "x", Some("https://host/v1?key=abc")),
    ];
    for request in bad {
        let result = t.try_call::<ProviderResult>(request.clone()).await;
        assert_eq!(remote_code(result), ErrorCode::BadRequest, "{request:?}");
    }
    let missing = |request| async { remote_code(t.try_call::<ProviderResult>(request).await) };
    assert_eq!(
        missing(Request::ProviderSetCredential(ProviderSetCredential {
            provider_id: "nope".into(),
            secret: "x".into()
        }))
        .await,
        ErrorCode::NotFound
    );
    t.add_provider("ollama", "ollama", None, None).await;
    for secret in ["", "   ", "two\nlines", "tab\tinside"] {
        assert_eq!(
            missing(Request::ProviderSetCredential(ProviderSetCredential {
                provider_id: "ollama".into(),
                secret: secret.into()
            }))
            .await,
            ErrorCode::BadRequest,
            "{secret:?}"
        );
    }
    assert!(t.store.get("ollama").unwrap().is_none());
    // Surrounding whitespace (a pasted newline) is trimmed.
    t.set_key("ollama", "  trimmed-key\n").await;
    assert_eq!(
        t.store.get("ollama").unwrap().unwrap().expose(),
        "trimmed-key"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn models_come_from_the_provider_and_mark_the_default() {
    let mock = MockServer::start().await;
    mock.route(
        "GET",
        "/v1/models",
        Reply::json(200, r#"{"data":[{"id":"m-2"},{"id":"m-1"}]}"#),
    );
    let t = Direct::start().await;
    let p = t
        .add_provider(
            "openai-compatible",
            "mock",
            Some(format!("{}/v1", mock.url())),
            Some("m-2"),
        )
        .await;
    let models: ProviderModels = t
        .call(Request::ProviderModels(ProviderRef {
            provider_id: p.id.clone(),
        }))
        .await;
    let listed: Vec<(&str, bool)> = models
        .models
        .iter()
        .map(|m| (m.id.as_str(), m.is_default))
        .collect();
    assert_eq!(listed, [("m-1", false), ("m-2", true)]);

    // A provider that needs a key and has none says so, without a request.
    let openai = t.add_provider("openai", "openai", None, None).await;
    let error = t
        .try_call::<ProviderModels>(Request::ProviderModels(ProviderRef {
            provider_id: openai.id.clone(),
        }))
        .await;
    let hutan_client::ClientError::Remote(error) = error.unwrap_err() else {
        panic!("a remote error");
    };
    assert_eq!(error.code, ErrorCode::ProviderError);
    assert!(
        error.message.contains("hutan provider key openai"),
        "{}",
        error.message
    );

    // An unreachable provider is a provider error, not a hang.
    let dead = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let down = t.add_provider("ollama", "down", Some(dead), None).await;
    let result = t
        .try_call::<ProviderModels>(Request::ProviderModels(ProviderRef {
            provider_id: down.id,
        }))
        .await;
    assert_eq!(remote_code(result), ErrorCode::ProviderError);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_probes_only_on_request_and_reports_what_answered() {
    let ollama = MockServer::start().await;
    ollama.route(
        "GET",
        "/api/tags",
        Reply::json(
            200,
            r#"{"models":[{"name":"qwen3:8b"},{"name":"llama3:latest"}]}"#,
        ),
    );
    let lm_studio = MockServer::start().await;
    lm_studio.route(
        "GET",
        "/v1/models",
        Reply::json(200, r#"{"data":[{"id":"gemma"}]}"#),
    );
    let silent = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let targets = vec![
        ProbeTarget {
            kind: ProviderKind::Ollama,
            name: "Ollama".into(),
            base_url: ollama.url(),
        },
        ProbeTarget {
            kind: ProviderKind::OpenAiCompatible,
            name: "LM Studio".into(),
            base_url: format!("{}/v1", lm_studio.url()),
        },
        ProbeTarget {
            kind: ProviderKind::Ollama,
            name: "Nobody home".into(),
            base_url: silent,
        },
    ];
    let t = Direct::start_with("", targets).await;
    sleep(Duration::from_millis(200)).await;
    assert!(
        ollama.requests().is_empty() && lm_studio.requests().is_empty(),
        "nothing is probed in the background"
    );
    t.add_provider("ollama", "My Ollama", Some(ollama.url()), None)
        .await;

    let found: ProviderDiscovery = t.call(Request::ProviderDiscover(Empty {})).await;
    assert_eq!(found.found.len(), 2, "{found:?}");
    let by_name = |name: &str| found.found.iter().find(|d| d.name == name).unwrap();
    assert_eq!(by_name("Ollama").kind, "ollama");
    assert_eq!(by_name("Ollama").models, ["llama3:latest", "qwen3:8b"]);
    assert_eq!(by_name("Ollama").provider_id.as_deref(), Some("my-ollama"));
    assert_eq!(by_name("LM Studio").kind, "openai-compatible");
    assert_eq!(by_name("LM Studio").provider_id, None);
    t.stop().await;
}

// Direct chat ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_direct_chat_streams_journals_continues_with_its_history_and_survives_a_restart() {
    let mock = MockServer::start().await;
    mock.route("POST", OPENAI_PATH, openai_reply(&["Hel", "lo"], (9, 2)));
    mock.route(
        "POST",
        OPENAI_PATH,
        openai_reply(&["Second ", "answer"], (20, 3)),
    );
    mock.route("POST", OPENAI_PATH, openai_reply(&["Third"], (30, 1)));
    let mut t = Direct::start().await;
    let provider = t
        .add_provider(
            "openai-compatible",
            "Local",
            Some(format!("{}/v1", mock.url())),
            Some("m-1"),
        )
        .await;

    let created = t.create(&provider.id, None, Some("Be brief")).await;
    assert_eq!(created.kind, hutan_core::SessionKind::Harness);
    assert_eq!(created.state, SessionState::Idle);
    assert_eq!(created.terminal_id, None);
    let chat = created.chat.clone().unwrap();
    assert_eq!(chat.agent, AGENT_DIRECT);
    assert_eq!(chat.model, "m-1", "the provider's default model");
    assert_eq!(chat.provider_id.as_deref(), Some("local"));

    let sent = t.send(created.id, "hi").await;
    let events = t.turn_events(created.id).await;
    assert_eq!(
        names(&events)
            .into_iter()
            .filter(|n| *n != "message_delta")
            .collect::<Vec<_>>(),
        [
            "turn_started",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "usage",
            "turn_completed"
        ]
    );
    assert!(events.iter().all(|e| e.turn_id == Some(sent.turn_id)));
    assert_eq!(delta_text(&events), "Hello");
    assert!(
        events.windows(2).all(|w| w[0].seq < w[1].seq),
        "seq increases"
    );
    let started = &events[0];
    assert_eq!(
        started.payload.as_ref().unwrap(),
        &serde_json::json!({"provider_id": "local", "model": "m-1"})
    );
    let AgentEventKind::Usage { total, last, .. } = &events
        .iter()
        .find(|e| e.kind.name() == "usage")
        .unwrap()
        .kind
    else {
        unreachable!()
    };
    assert_eq!(last.unwrap().input_tokens, Some(9));
    assert_eq!(last.unwrap().output_tokens, Some(2));
    assert_eq!(
        last.unwrap().cached_input_tokens,
        None,
        "never reported, never zero"
    );
    assert_eq!(*total, *last);
    let idle = t.wait_state(created.id, SessionState::Idle).await;
    assert!(!idle.chat.unwrap().turn_active);
    assert_eq!(
        sent_messages(&mock, OPENAI_PATH, 0),
        pairs(&[("system", "Be brief"), ("user", "hi")])
    );
    assert_eq!(mock.requests_to(OPENAI_PATH)[0].json()["model"], "m-1");

    // The second turn carries the first exchange.
    t.send(created.id, "again").await;
    let second = t.turn_events(created.id).await;
    assert_eq!(delta_text(&second), "Second answer");
    assert_eq!(
        sent_messages(&mock, OPENAI_PATH, 1),
        pairs(&[
            ("system", "Be brief"),
            ("user", "hi"),
            ("assistant", "Hello"),
            ("user", "again")
        ])
    );
    let AgentEventKind::Usage { total, .. } = &second
        .iter()
        .find(|e| e.kind.name() == "usage")
        .unwrap()
        .kind
    else {
        unreachable!()
    };
    assert_eq!(
        total.unwrap().input_tokens,
        Some(29),
        "the conversation's running total"
    );
    t.wait_state(created.id, SessionState::Idle).await;

    // The journal: no deltas, everything replayed, the whole conversation in order.
    let before = t.history(created.id).await;
    assert!(
        before
            .events
            .iter()
            .all(|e| e.kind.name() != "message_delta")
    );
    assert!(
        before
            .events
            .iter()
            .all(|e| e.delivery == hutan_core::Delivery::Replay)
    );
    assert_eq!(
        message_texts(&before.events, MessageRole::User),
        ["hi", "again"]
    );
    assert_eq!(
        message_texts(&before.events, MessageRole::Assistant),
        ["Hello", "Second answer"]
    );

    // A daemon restart: the session comes back idle with the same history, and the next turn
    // still has the whole conversation.
    let mut t = t.restart().await;
    let restored = t.wait_state(created.id, SessionState::Idle).await;
    assert_eq!(restored.chat.as_ref().unwrap().agent, AGENT_DIRECT);
    assert_eq!(
        restored.chat.as_ref().unwrap().provider_id.as_deref(),
        Some("local")
    );
    let after = t.history(created.id).await;
    assert_eq!(
        after.events, before.events,
        "history is unchanged by the restart"
    );
    t.send(created.id, "and after the restart").await;
    let third = t.turn_events(created.id).await;
    assert_eq!(delta_text(&third), "Third");
    assert_eq!(
        sent_messages(&mock, OPENAI_PATH, 2),
        pairs(&[
            ("system", "Be brief"),
            ("user", "hi"),
            ("assistant", "Hello"),
            ("user", "again"),
            ("assistant", "Second answer"),
            ("user", "and after the restart")
        ])
    );
    assert!(
        third[0].seq > before.last_seq,
        "seq continues across the restart"
    );
    let AgentEventKind::Usage { total, .. } = &third
        .iter()
        .find(|e| e.kind.name() == "usage")
        .unwrap()
        .kind
    else {
        unreachable!()
    };
    assert_eq!(
        total.unwrap().input_tokens,
        Some(59),
        "the total continues from the journal"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_model_can_be_switched_between_turns_and_the_history_stays() {
    let openai = MockServer::start().await;
    openai.route("POST", OPENAI_PATH, openai_reply(&["from A"], (5, 2)));
    let ollama = MockServer::start().await;
    ollama.route(
        "POST",
        "/api/chat",
        Reply::ndjson([
            "{\"message\":{\"role\":\"assistant\",\"content\":\"from \"},\"done\":false}\n",
            "{\"message\":{\"content\":\"B\"},\"done\":false}\n{\"done\":true,\"done_reason\":\"stop\",\"prompt_eval_count\":11,\"eval_count\":2}\n",
        ]),
    );
    let mut t = Direct::start().await;
    let a = t
        .add_provider(
            "openai-compatible",
            "A",
            Some(format!("{}/v1", openai.url())),
            Some("model-a"),
        )
        .await;
    let b = t
        .add_provider("ollama", "B", Some(ollama.url()), None)
        .await;
    let created = t.create(&a.id, None, None).await;

    t.send(created.id, "first").await;
    t.turn_events(created.id).await;
    t.wait_state(created.id, SessionState::Idle).await;

    // Same provider, other model; then another provider.
    let result: SessionResult = t
        .call(Request::ChatSetModel(ChatSetModel {
            session_id: created.id,
            provider_id: None,
            model: "model-a2".into(),
        }))
        .await;
    let chat = result.session.chat.unwrap();
    assert_eq!(
        (chat.model.as_str(), chat.provider_id.as_deref()),
        ("model-a2", Some("a"))
    );
    // Setting what is already set changes nothing (and journals nothing).
    t.call::<SessionResult>(Request::ChatSetModel(ChatSetModel {
        session_id: created.id,
        provider_id: Some("a".into()),
        model: "model-a2".into(),
    }))
    .await;
    let switched: SessionResult = t
        .call(Request::ChatSetModel(ChatSetModel {
            session_id: created.id,
            provider_id: Some(b.id.clone()),
            model: "llama3".into(),
        }))
        .await;
    assert_eq!(
        switched
            .session
            .chat
            .as_ref()
            .unwrap()
            .provider_id
            .as_deref(),
        Some("b")
    );
    assert_eq!(switched.session.chat.as_ref().unwrap().model, "llama3");

    t.send(created.id, "second").await;
    let events = t.turn_events(created.id).await;
    assert_eq!(delta_text(&events), "from B");
    // (The two notices of the switches are still in the stream ahead of the turn.)
    let started = events
        .iter()
        .find(|e| matches!(e.kind, AgentEventKind::TurnStarted))
        .unwrap();
    assert_eq!(
        started.payload.as_ref().unwrap(),
        &serde_json::json!({"provider_id": "b", "model": "llama3"}),
        "the turn records the provider and model it used"
    );
    // The history came along to the new provider, over its own API.
    assert_eq!(openai.requests_to(OPENAI_PATH).len(), 1);
    let body = ollama.requests_to("/api/chat")[0].json();
    assert_eq!(body["model"], "llama3");
    let roles: Vec<(&str, &str)> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["role"].as_str().unwrap(), m["content"].as_str().unwrap()))
        .collect();
    assert_eq!(
        roles,
        [
            ("user", "first"),
            ("assistant", "from A"),
            ("user", "second")
        ]
    );

    // The journal records both switches (and not the no-op), in order, between the turns.
    let history = t.history(created.id).await;
    let notices: Vec<String> = history
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::Notice { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        notices,
        [
            "model switched from a / model-a to a / model-a2",
            "model switched from a / model-a2 to b / llama3"
        ]
    );
    let order = names(&history.events);
    let first_end = order.iter().position(|n| *n == "turn_completed").unwrap();
    let first_notice = order.iter().position(|n| *n == "notice").unwrap();
    let second_start = order.iter().rposition(|n| *n == "turn_started").unwrap();
    assert!(
        first_end < first_notice && first_notice < second_start,
        "{order:?}"
    );

    // The switch is part of the session: it survives a restart.
    let t = t.restart().await;
    let restored = t.wait_state(created.id, SessionState::Idle).await;
    let chat = restored.chat.unwrap();
    assert_eq!(
        (chat.model.as_str(), chat.provider_id.as_deref()),
        ("llama3", Some("b"))
    );

    // Bad switches.
    let nobody = SessionId::new();
    for (request, code) in [
        (
            ChatSetModel {
                session_id: created.id,
                provider_id: Some("nope".into()),
                model: "x".into(),
            },
            ErrorCode::BadRequest,
        ),
        (
            ChatSetModel {
                session_id: created.id,
                provider_id: None,
                model: "  ".into(),
            },
            ErrorCode::BadRequest,
        ),
        (
            ChatSetModel {
                session_id: nobody,
                provider_id: None,
                model: "x".into(),
            },
            ErrorCode::NotFound,
        ),
    ] {
        let result = t
            .try_call::<SessionResult>(Request::ChatSetModel(request.clone()))
            .await;
        assert_eq!(remote_code(result), code, "{request:?}");
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupting_cancels_the_http_stream_and_keeps_what_was_said() {
    let mock = MockServer::start().await;
    mock.route(
        "POST",
        OPENAI_PATH,
        Reply::sse(["data: {\"choices\":[{\"delta\":{\"content\":\"partial \"}}]}\n\n"])
            .then_hold(),
    );
    mock.route("POST", OPENAI_PATH, openai_reply(&["recovered"], (1, 1)));
    let mut t = Direct::start().await;
    let p = t
        .add_provider(
            "openai-compatible",
            "mock",
            Some(format!("{}/v1", mock.url())),
            Some("m"),
        )
        .await;
    let created = t.create(&p.id, None, None).await;
    t.send(created.id, "talk").await;
    wait_event(&mut t.events, "the first delta", |e| match e {
        Event::ChatEvent { event, .. }
            if matches!(event.kind, AgentEventKind::MessageDelta { .. }) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    t.wait_state(created.id, SessionState::Running).await;

    t.call::<Empty>(Request::ChatInterrupt(ChatInterrupt {
        session_id: created.id,
        keep_queue: false,
    }))
    .await;
    let events = t.turn_events(created.id).await;
    assert!(matches!(
        events.last().unwrap().kind,
        AgentEventKind::TurnCancelled
    ));
    assert_eq!(message_texts(&events, MessageRole::Assistant), ["partial "]);
    timeout(WAIT, mock.wait_for_disconnects(1))
        .await
        .expect("the provider saw the connection close");
    t.wait_state(created.id, SessionState::Idle).await;

    // Nothing to interrupt now.
    let again = t
        .try_call::<Empty>(Request::ChatInterrupt(ChatInterrupt {
            session_id: created.id,
            keep_queue: false,
        }))
        .await;
    assert_eq!(remote_code(again), ErrorCode::InvalidState);

    // The partial reply is part of the conversation; the chat goes on.
    t.send(created.id, "continue").await;
    t.turn_events(created.id).await;
    assert_eq!(
        sent_messages(&mock, OPENAI_PATH, 1),
        pairs(&[
            ("user", "talk"),
            ("assistant", "partial "),
            ("user", "continue")
        ])
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_session_mid_turn_ends_the_turn_and_the_session() {
    let mock = MockServer::start().await;
    mock.route(
        "POST",
        OPENAI_PATH,
        Reply::sse(["data: {\"choices\":[{\"delta\":{\"content\":\"half\"}}]}\n\n"]).then_hold(),
    );
    let mut t = Direct::start().await;
    let p = t
        .add_provider(
            "openai-compatible",
            "mock",
            Some(format!("{}/v1", mock.url())),
            Some("m"),
        )
        .await;
    let created = t.create(&p.id, None, None).await;
    t.send(created.id, "go").await;
    wait_event(&mut t.events, "the first delta", |e| match e {
        Event::ChatEvent { event, .. }
            if matches!(event.kind, AgentEventKind::MessageDelta { .. }) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    let stopped = common::stop(&t.client, created.id).await.unwrap();
    assert_eq!(stopped.state, SessionState::Stopped);
    let history = t.history(created.id).await;
    assert!(
        matches!(
            history.events.last().unwrap().kind,
            AgentEventKind::TurnCancelled
        ),
        "{:?}",
        names(&history.events)
    );
    assert_eq!(
        message_texts(&history.events, MessageRole::Assistant),
        ["half"]
    );
    timeout(WAIT, mock.wait_for_disconnects(1)).await.unwrap();
    // A stopped chat cannot be written to.
    let result = t
        .try_call::<ChatSent>(Request::ChatSend(ChatSend {
            session_id: created.id,
            text: "hello?".into(),
            attachments: vec![],
        }))
        .await;
    assert_eq!(remote_code(result), ErrorCode::InvalidState);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_failures_end_the_turn_with_a_clear_message_and_leave_the_chat_usable() {
    let mock = MockServer::start().await;
    mock.route(
        "POST",
        OPENAI_PATH,
        Reply::json(
            401,
            r#"{"error":{"message":"Incorrect API key provided: sk-LEAKCHECK-wrong-key-0000"}}"#,
        ),
    );
    mock.route(
        "POST",
        OPENAI_PATH,
        Reply::json(429, r#"{"error":{"message":"slow down"}}"#).header("retry-after", "3"),
    );
    mock.route("POST", OPENAI_PATH, openai_reply(&["fine"], (1, 1)));
    let mut t = Direct::start().await;
    let p = t
        .add_provider(
            "openai",
            "OpenAI",
            Some(format!("{}/v1", mock.url())),
            Some("gpt-x"),
        )
        .await;
    let created = t.create(&p.id, None, None).await;

    // No key: refused before a turn exists, saying what to do.
    let result = t
        .try_call::<ChatSent>(Request::ChatSend(ChatSend {
            session_id: created.id,
            text: "hello".into(),
            attachments: vec![],
        }))
        .await;
    let hutan_client::ClientError::Remote(error) = result.unwrap_err() else {
        panic!("a remote error");
    };
    assert_eq!(error.code, ErrorCode::ProviderError);
    assert!(
        error.message.contains("hutan provider key openai"),
        "{}",
        error.message
    );
    assert!(mock.requests().is_empty(), "no request without a key");
    assert_eq!(
        t.history(created.id).await.events.len(),
        0,
        "and nothing journaled"
    );
    assert_eq!(t.session(created.id).await.state, SessionState::Idle);

    // A rejected key.
    t.set_key(&p.id, "sk-LEAKCHECK-rejected-key-1111").await;
    t.send(created.id, "hello").await;
    let events = t.turn_events(created.id).await;
    let AgentEventKind::TurnFailed { message, details } = &events.last().unwrap().kind else {
        panic!("a failed turn: {:?}", names(&events));
    };
    assert!(message.contains("refused the credentials"), "{message}");
    assert!(
        !message.contains("LEAKCHECK"),
        "the provider's echo of the key is scrubbed: {message}"
    );
    assert!(details.as_deref().unwrap().contains("gpt-x"));
    assert_eq!(message_texts(&events, MessageRole::User), ["hello"]);
    t.wait_state(created.id, SessionState::Idle).await;

    // Rate limited: the retry hint is in the message.
    t.send(created.id, "again").await;
    let events = t.turn_events(created.id).await;
    let AgentEventKind::TurnFailed { message, .. } = &events.last().unwrap().kind else {
        panic!("a failed turn");
    };
    assert!(
        message.contains("rate limited") && message.contains("retry after 3s"),
        "{message}"
    );

    // And then it works, with the earlier user messages still part of the conversation.
    t.wait_state(created.id, SessionState::Idle).await;
    t.send(created.id, "third").await;
    let events = t.turn_events(created.id).await;
    assert!(matches!(
        events.last().unwrap().kind,
        AgentEventKind::TurnCompleted { .. }
    ));
    assert_eq!(
        sent_messages(&mock, OPENAI_PATH, 2)
            .iter()
            .filter(|(r, _)| r == "user")
            .map(|(_, c)| c.as_str())
            .collect::<Vec<_>>(),
        ["hello", "again", "third"]
    );
    t.stop().await;
}

/// `chat.retry` (protocol 0.34): a rate-limited turn retried twice, then answered; the journal
/// holds the user message once, and every request carried it once.
#[tokio::test(flavor = "multi_thread")]
async fn a_retry_sends_the_last_message_again_without_journaling_it_twice() {
    let mock = MockServer::start().await;
    for _ in 0..2 {
        mock.route(
            "POST",
            OPENAI_PATH,
            Reply::json(429, r#"{"error":{"message":"slow down"}}"#).header("retry-after", "1"),
        );
    }
    mock.route("POST", OPENAI_PATH, openai_reply(&["done"], (1, 1)));
    let mut t = Direct::start().await;
    let p = t
        .add_provider(
            "openai",
            "OpenAI",
            Some(format!("{}/v1", mock.url())),
            Some("gpt-x"),
        )
        .await;
    t.set_key(&p.id, "sk-test").await;
    let created = t.create(&p.id, None, None).await;

    // Nothing to retry before the first message.
    let early = t
        .try_call::<ChatSent>(Request::ChatRetry(ChatRetry {
            session_id: created.id,
        }))
        .await;
    assert_eq!(remote_code(early), ErrorCode::InvalidState);

    t.send(created.id, "hello").await;
    let failed = t.turn_events(created.id).await;
    assert!(matches!(
        failed.last().unwrap().kind,
        AgentEventKind::TurnFailed { .. }
    ));
    for attempt in 0..2 {
        t.wait_state(created.id, SessionState::Idle).await;
        t.call::<ChatSent>(Request::ChatRetry(ChatRetry {
            session_id: created.id,
        }))
        .await;
        let events = t.turn_events(created.id).await;
        assert!(
            message_texts(&events, MessageRole::User).is_empty(),
            "attempt {attempt}: the retried message is not journaled again"
        );
        if attempt == 1 {
            assert_eq!(message_texts(&events, MessageRole::Assistant), ["done"]);
        }
    }
    let history = t.history(created.id).await;
    assert_eq!(
        message_texts(&history.events, MessageRole::User),
        ["hello"],
        "one user message after two retries"
    );
    // Every request sent the message once, as the last user message.
    for n in 0..3 {
        let users: Vec<String> = sent_messages(&mock, OPENAI_PATH, n)
            .into_iter()
            .filter(|(role, _)| role == "user")
            .map(|(_, text)| text)
            .collect();
        assert_eq!(users, ["hello"], "request {n}");
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn context_beyond_the_limit_drops_the_oldest_messages_and_says_so() {
    let mock = MockServer::start().await;
    for reply in ["aaaaaaaaaa", "bbbbbbbbbb", "cccccccccc"] {
        mock.route("POST", OPENAI_PATH, openai_reply(&[reply], (1, 1)));
    }
    // 40 characters of conversation fit; the system prompt is free.
    let mut t = Direct::start_with("[gateway]\nmax_context_chars = 40\n", Vec::new()).await;
    let p = t
        .add_provider(
            "openai-compatible",
            "mock",
            Some(format!("{}/v1", mock.url())),
            Some("m"),
        )
        .await;
    let created = t
        .create(&p.id, None, Some("system prompt is not counted"))
        .await;

    t.send(created.id, "q1-1111").await;
    let first = t.turn_events(created.id).await;
    assert!(
        !names(&first).contains(&"notice"),
        "nothing was dropped yet"
    );
    t.wait_state(created.id, SessionState::Idle).await;
    t.send(created.id, "q2-2222").await;
    let second = t.turn_events(created.id).await;
    assert!(
        !names(&second).contains(&"notice"),
        "7 + 10 + 7 + new 7 fits in 40"
    );
    t.wait_state(created.id, SessionState::Idle).await;

    t.send(created.id, "q3-33333333").await;
    let third = t.turn_events(created.id).await;
    // New message 11, budget left 29: "bbbbbbbbbb" (10), "q2-2222" (7) and "aaaaaaaaaa" (10)
    // fit, "q1-1111" does not. A conversation does not start with the assistant's answer, so
    // "aaaaaaaaaa" goes too: two messages are left out.
    let notice = third
        .iter()
        .find_map(|e| match &e.kind {
            AgentEventKind::Notice { message, .. } => Some(message.clone()),
            _ => None,
        })
        .expect("the turn notes the truncation");
    assert!(
        notice.contains("2 oldest message") && notice.contains("40"),
        "{notice}"
    );
    let sent = sent_messages(&mock, OPENAI_PATH, 2);
    assert_eq!(sent[0].0, "system");
    assert_eq!(
        sent[1..]
            .iter()
            .map(|(_, c)| c.as_str())
            .collect::<Vec<_>>(),
        ["q2-2222", "bbbbbbbbbb", "q3-33333333"]
    );
    // The journal still has everything; only the request was shortened.
    let history = t.history(created.id).await;
    assert_eq!(
        message_texts(&history.events, MessageRole::User),
        ["q1-1111", "q2-2222", "q3-33333333"]
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_a_direct_chat_validates_the_request() {
    let t = Direct::start().await;
    let p = t.add_provider("ollama", "ollama", None, None).await;
    let create = |provider: Option<&str>, model: Option<&str>| {
        Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: t.work.path().to_owned(),
            agent: AGENT_DIRECT.to_owned(),
            model: model.map(str::to_owned),
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: provider.map(str::to_owned),
            system_prompt: None,
        })
    };
    for (request, what) in [
        (create(None, Some("m")), "no provider"),
        (create(Some("nope"), Some("m")), "unknown provider"),
        (create(Some(&p.id), None), "no model and no default"),
        (create(Some(&p.id), Some("  ")), "blank model"),
    ] {
        let result = t.try_call::<SessionResult>(request).await;
        assert_eq!(remote_code(result), ErrorCode::BadRequest, "{what}");
    }
    let too_long = t
        .try_call::<SessionResult>(t.create_request(&p.id, Some("m"), Some(&"x".repeat(40_000))))
        .await;
    assert_eq!(remote_code(too_long), ErrorCode::BadRequest);
    let list = t
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await;
    assert!(
        list.sessions.is_empty(),
        "a refused chat leaves no session behind"
    );
    // `chat.models` is for agents; a direct chat's models are the provider's.
    let result = t
        .try_call::<hutan_protocol::chat::ChatModels>(Request::ChatModels(
            hutan_protocol::chat::ChatModelsRequest {
                agent: AGENT_DIRECT.into(),
            },
        ))
        .await;
    assert_eq!(remote_code(result), ErrorCode::BadRequest);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_whose_provider_was_removed_stays_listed_and_fails_clearly() {
    let mock = MockServer::start().await;
    mock.route("POST", OPENAI_PATH, openai_reply(&["hi"], (1, 1)));
    let t = Direct::start().await;
    let p = t
        .add_provider(
            "openai-compatible",
            "mock",
            Some(format!("{}/v1", mock.url())),
            Some("m"),
        )
        .await;
    let created = t.create(&p.id, None, None).await;
    t.call::<Empty>(Request::ProviderRemove(ProviderRef {
        provider_id: p.id.clone(),
    }))
    .await;
    let t = t.restart().await;
    t.wait_state(created.id, SessionState::Idle).await;
    let result = t
        .try_call::<ChatSent>(Request::ChatSend(ChatSend {
            session_id: created.id,
            text: "hello".into(),
            attachments: vec![],
        }))
        .await;
    let hutan_client::ClientError::Remote(error) = result.unwrap_err() else {
        panic!("a remote error");
    };
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.message.contains(&p.id), "{}", error.message);
    // Pointing the chat at another provider repairs it.
    let q = t
        .add_provider(
            "openai-compatible",
            "other",
            Some(format!("{}/v1", mock.url())),
            Some("m"),
        )
        .await;
    t.call::<SessionResult>(Request::ChatSetModel(ChatSetModel {
        session_id: created.id,
        provider_id: Some(q.id),
        model: "m".into(),
    }))
    .await;
    t.call::<ChatSent>(Request::ChatSend(ChatSend {
        session_id: created.id,
        text: "hello again".into(),
        attachments: vec![],
    }))
    .await;
    t.stop().await;
}

// Secrets -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_is_used_but_appears_nowhere_it_must_not() {
    const SECRET: &str = "LEAKCHECK-5c1f0e9a77b2-super-secret-api-key";
    let mock = MockServer::start().await;
    mock.route(
        "GET",
        "/v1/models",
        Reply::json(200, r#"{"data":[{"id":"m"}]}"#),
    );
    mock.route("POST", OPENAI_PATH, openai_reply(&["answer"], (2, 1)));
    mock.route(
        "POST",
        OPENAI_PATH,
        Reply::json(
            401,
            &format!(r#"{{"error":{{"message":"bad key {SECRET}"}}}}"#),
        ),
    );
    let mut t = Direct::start().await;
    let p = t
        .add_provider(
            "openai",
            "OpenAI",
            Some(format!("{}/v1", mock.url())),
            Some("m"),
        )
        .await;
    let keyed = t.set_key(&p.id, SECRET).await;
    assert!(keyed.has_credential);
    let after = t
        .call::<ProviderList>(Request::ProviderList(Empty {}))
        .await;
    assert!(after.providers[0].has_credential);

    t.call::<ProviderModels>(Request::ProviderModels(ProviderRef {
        provider_id: p.id.clone(),
    }))
    .await;
    let created = t.create(&p.id, None, Some("Be brief")).await;
    t.send(created.id, "hello").await;
    t.turn_events(created.id).await;
    t.wait_state(created.id, SessionState::Idle).await;
    // The provider echoes the key back in an error: it must not reach the journal either.
    t.send(created.id, "again").await;
    let failed = t.turn_events(created.id).await;
    assert!(matches!(
        failed.last().unwrap().kind,
        AgentEventKind::TurnFailed { .. }
    ));
    t.wait_state(created.id, SessionState::Idle).await;
    t.history(created.id).await;

    // The key was really used: the provider received it as a bearer token.
    let sent = &mock.requests_to(OPENAI_PATH)[0];
    assert_eq!(
        sent.header("authorization"),
        Some(&*format!("Bearer {SECRET}"))
    );
    assert_eq!(t.store.get(&p.id).unwrap().unwrap().expose(), SECRET);

    // Every response and event the client saw.
    let seen = t.seen.lock().clone();
    assert!(!seen.is_empty());
    assert!(!seen.contains("LEAKCHECK"), "a response carries the secret");
    let mut all_events = String::new();
    loop {
        match t.capture.try_recv() {
            Ok((_, event)) => all_events.push_str(&format!("{:?}\n", event.encode().unwrap())),
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => break,
        }
    }
    assert!(!all_events.is_empty(), "the capture saw events");
    assert!(
        !all_events.contains("LEAKCHECK"),
        "an event carries the secret"
    );

    // The database, its WAL, and every other file of the daemon's home.
    let home = t.daemon.paths.base.clone();
    let t = t.restart().await;
    t.daemon
        .paths
        .database
        .exists()
        .then_some(())
        .expect("the database exists");
    let mut scanned = 0;
    for file in files_under(&home) {
        let bytes = std::fs::read(&file).unwrap();
        scanned += 1;
        assert!(
            !bytes.windows(b"LEAKCHECK".len()).any(|w| w == b"LEAKCHECK"),
            "{} contains the secret",
            file.display()
        );
    }
    assert!(scanned >= 1);
    // What the database does hold of the provider: config and the pointer into the Keychain.
    let db = rusqlite::Connection::open(&t.daemon.paths.database).unwrap();
    let (kind, keychain_ref): (String, String) = db
        .query_row("SELECT kind, keychain_ref FROM providers", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(kind, "openai");
    assert_eq!(keychain_ref, "dev.hutan.provider/openai");
    let columns: Vec<String> = db
        .prepare("PRAGMA table_info(providers)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        columns,
        [
            "id",
            "kind",
            "name",
            "base_url",
            "default_model",
            "keychain_ref",
            "created_at_ms"
        ]
    );
    t.stop().await;

    // And the daemon's own logs, at every level.
    let log = logged();
    assert!(
        log.contains("credential stored"),
        "the daemon logged the event, not the secret"
    );
    assert!(!log.contains("LEAKCHECK"), "a log line carries the secret");
}
