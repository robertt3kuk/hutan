//! Assistant chat end to end (protocol 0.12, `docs/assistant.md`): a real daemon on a temporary
//! `HUTAN_HOME`, real client connections, the in-memory credential store, a mock
//! OpenAI-compatible server standing in for the provider, real terminals, files and the history
//! index (its sources pointed at temporary directories). No real provider, key or transcript is
//! ever touched.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{TestDaemon, WAIT, create_in, remote_code, sh, terminal_of, wait_event};
use hutan_client::{Client, ClientError};
use hutan_core::agent::MessageRole;
use hutan_core::{AgentEvent, AgentEventKind, Seq, SessionId, SessionState, WorkspaceId};
use hutan_daemon::{DaemonConfig, GatewayConfig, Paths};
use hutan_gateway::testing::{MockServer, Reply};
use hutan_gateway::{CredentialStore, HttpConfig, MemoryStore};
use hutan_protocol::ErrorCode;
use hutan_protocol::assistant::{
    ATTACHMENTS_MAX, AssistantList, AssistantListRequest, AssistantOpen, AssistantScope,
    Attachment, AttachmentKind, LineRange, attachments_meta,
};
use hutan_protocol::blocks::BlockState;
use hutan_protocol::chat::{ChatHistory, ChatHistoryRequest, ChatSend, ChatSent};
use hutan_protocol::history::{HistoryHit, HistoryHits, HistoryRole, HistorySearch, HistorySource};
use hutan_protocol::messages::{
    Event, Request, SessionInfo, SessionList, SessionListFilter, SessionRef, SessionResult,
    WorkspaceCreate, WorkspaceInfo, WorkspaceKind, WorkspaceList,
};
use hutan_protocol::provider::{ProviderAdd, ProviderResult, ProviderSetCredential};
use tokio::sync::broadcast;
use tokio::time::sleep;

const PATH: &str = "/v1/chat/completions";
const SECRET: &str = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD";

fn reply(text: &str) -> Reply {
    Reply::sse([
        format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"delta":{"content": text}}]})
        ),
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".to_owned(),
        "data: [DONE]\n\n".to_owned(),
    ])
}

struct Env {
    _tmp: tempfile::TempDir,
    /// The temporary directory, canonical.
    base: PathBuf,
    /// A project folder inside it.
    work: PathBuf,
    daemon: TestDaemon,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
    store: Arc<MemoryStore>,
    mock: MockServer,
}

fn gateway(store: &Arc<MemoryStore>) -> GatewayConfig {
    GatewayConfig {
        credentials: Some(store.clone() as Arc<dyn CredentialStore>),
        http: HttpConfig {
            no_proxy: true,
            connect_retries: 1,
            retry_backoff: Duration::from_millis(5),
            ..HttpConfig::default()
        },
        discovery: Some(Vec::new()),
    }
}

impl Env {
    /// A daemon whose `daemon.toml` is `toml`, plus `[history]` pointing at temporary source
    /// directories when `history` is set. The provider `mock` exists and is the assistants'.
    async fn start(toml: &str, history: bool, configure_provider: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("home");
        let work = base.join("work");
        for dir in ["claude", "codex", "omp"] {
            fs::create_dir_all(base.join("sources").join(dir)).unwrap();
        }
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(work.join("src")).unwrap();
        let mut config = String::new();
        if configure_provider {
            config.push_str("[assistant]\nprovider = \"mock\"\nmodel = \"m-1\"\n");
        }
        config.push_str(toml);
        if history {
            config.push_str(&format!(
                "\n[history]\nenabled = true\n[history.roots]\nclaude = \"{}\"\ncodex = \"{}\"\nomp = \"{}\"\n",
                base.join("sources/claude").display(),
                base.join("sources/codex").display(),
                base.join("sources/omp").display(),
            ));
        }
        fs::write(home.join("daemon.toml"), config).unwrap();
        let paths = Paths::in_home(&home);
        let store = Arc::new(MemoryStore::new());
        let mut daemon_config = DaemonConfig::new(paths.clone());
        daemon_config.gateway = gateway(&store);
        let running = hutan_daemon::start(daemon_config).await.unwrap();
        let daemon = TestDaemon {
            _dir: tempfile::tempdir().unwrap(),
            paths,
            daemon: running,
        };
        let mock = MockServer::start().await;
        let (client, events) = daemon.connect_subscribed().await;
        let env = Self {
            _tmp: tmp,
            base,
            work,
            daemon,
            client,
            events,
            store,
            mock,
        };
        env.call::<ProviderResult>(Request::ProviderAdd(ProviderAdd {
            kind: "openai-compatible".into(),
            name: "mock".into(),
            base_url: Some(format!("{}/v1", env.mock.url())),
            default_model: Some("m-1".into()),
        }))
        .await;
        env.call::<ProviderResult>(Request::ProviderSetCredential(ProviderSetCredential {
            provider_id: "mock".into(),
            secret: "mock-key".into(),
        }))
        .await;
        env
    }

    async fn call<T: serde::de::DeserializeOwned>(&self, request: Request) -> T {
        self.client.call(request).await.unwrap()
    }

    async fn try_call<T: serde::de::DeserializeOwned>(
        &self,
        request: Request,
    ) -> Result<T, ClientError> {
        self.client.call(request).await
    }

    async fn restart(self) -> Self {
        let Self {
            _tmp,
            base,
            work,
            daemon,
            store,
            mock,
            ..
        } = self;
        let for_config = store.clone();
        let daemon = daemon
            .restart_with(move |c| c.gateway = gateway(&for_config))
            .await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            _tmp,
            base,
            work,
            daemon,
            client,
            events,
            store,
            mock,
        }
    }

    async fn workspaces(&self) -> Vec<WorkspaceInfo> {
        self.call::<WorkspaceList>(Request::WorkspaceList(hutan_protocol::messages::Empty {}))
            .await
            .workspaces
    }

    async fn home(&self) -> WorkspaceInfo {
        self.workspaces()
            .await
            .into_iter()
            .find(|w| w.kind == WorkspaceKind::Home)
            .unwrap()
    }

    /// A project workspace named `name` rooted at `root`.
    async fn project(&self, name: &str, root: &PathBuf) -> WorkspaceInfo {
        fs::create_dir_all(root).unwrap();
        self.call(Request::WorkspaceCreate(WorkspaceCreate {
            name: name.to_owned(),
            paths: vec![root.clone()],
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
    }

    async fn open(
        &self,
        scope: AssistantScope,
        workspace: Option<WorkspaceId>,
        new: bool,
    ) -> SessionInfo {
        self.try_open(scope, workspace, new).await.unwrap()
    }

    async fn try_open(
        &self,
        scope: AssistantScope,
        workspace: Option<WorkspaceId>,
        new: bool,
    ) -> Result<SessionInfo, ClientError> {
        self.try_call::<SessionResult>(Request::AssistantOpen(AssistantOpen {
            scope,
            workspace_id: workspace,
            new,
        }))
        .await
        .map(|r| r.session)
    }

    async fn send(
        &self,
        session: SessionId,
        text: &str,
        attachments: Vec<Attachment>,
    ) -> Result<ChatSent, ClientError> {
        self.try_call(Request::ChatSend(ChatSend {
            session_id: session,
            text: text.to_owned(),
            attachments,
        }))
        .await
    }

    /// The events of the session up to the end of the turn.
    async fn turn(&mut self, session: SessionId) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        loop {
            let event = wait_event(&mut self.events, "the turn to end", |e| match e {
                Event::ChatEvent { session_id, event } if *session_id == session => {
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

    async fn ask(
        &mut self,
        session: SessionId,
        text: &str,
        attachments: Vec<Attachment>,
    ) -> Vec<AgentEvent> {
        self.send(session, text, attachments).await.unwrap();
        self.turn(session).await
    }

    async fn journal(&self, session: SessionId) -> Vec<AgentEvent> {
        self.call::<ChatHistory>(Request::ChatHistory(ChatHistoryRequest {
            session_id: session,
            before_seq: None,
            limit: 1000,
        }))
        .await
        .events
    }

    async fn session(&self, id: SessionId) -> SessionInfo {
        self.call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .sessions
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
    }

    async fn search(&self, query: &str) -> Vec<HistoryHit> {
        self.call::<HistoryHits>(Request::HistorySearch(HistorySearch {
            query: query.to_owned(),
            limit: 20,
            sources: Vec::new(),
            roles: Vec::new(),
            project: None,
            since_ms: None,
            until_ms: None,
        }))
        .await
        .hits
    }

    async fn wait_hits(&self, query: &str) -> Vec<HistoryHit> {
        let deadline = Instant::now() + WAIT;
        loop {
            let hits = self.search(query).await;
            if !hits.is_empty() {
                return hits;
            }
            assert!(Instant::now() < deadline, "no hits for `{query}` in time");
            sleep(Duration::from_millis(50)).await;
        }
    }

    /// The messages of the nth request to the mock as `(role, content)`.
    fn sent(&self, n: usize) -> Vec<(String, String)> {
        self.mock.requests_to(PATH)[n].json()["messages"]
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

    /// A terminal session in `workspace` whose shell printed one finished block: the command
    /// `cargo test -p daemon` with two lines of output, one of them holding a secret.
    async fn block_terminal(&self, workspace: WorkspaceId) -> (SessionInfo, u64) {
        let script = format!(
            "printf '\\033]133;A\\007$ \\033]133;B\\007cargo test -p daemon\\r\\n\\033]133;C\\007flibbertigibbet test output\\r\\nsecond line key={SECRET}\\r\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007'; exec cat"
        );
        let session = create_in(&self.client, Some(workspace), sh(&script)).await;
        let terminal = terminal_of(&session);
        let deadline = Instant::now() + WAIT;
        loop {
            let blocks = self
                .client
                .terminal_blocks(terminal, None, 10)
                .await
                .unwrap();
            if let Some(done) = blocks
                .blocks
                .iter()
                .find(|b| b.state == BlockState::Finished)
            {
                return (session, done.id);
            }
            assert!(Instant::now() < deadline, "the block never finished");
            sleep(Duration::from_millis(30)).await;
        }
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

fn user_text(events: &[AgentEvent]) -> String {
    events
        .iter()
        .find_map(|e| match &e.kind {
            AgentEventKind::MessageEnd {
                role: MessageRole::User,
                text,
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .expect("a user message")
}

fn user_event(events: &[AgentEvent]) -> &AgentEvent {
    events
        .iter()
        .find(|e| {
            matches!(
                e.kind,
                AgentEventKind::MessageEnd {
                    role: MessageRole::User,
                    ..
                }
            )
        })
        .expect("a user message")
}

fn file(path: &str) -> Attachment {
    Attachment::File {
        path: path.to_owned(),
        picked: false,
        range: None,
    }
}

async fn error_of<T: std::fmt::Debug>(result: Result<T, ClientError>) -> (ErrorCode, String) {
    match result {
        Err(ClientError::Remote(body)) => (body.code, body.message),
        other => panic!("expected a remote error, got {other:?}"),
    }
}

// assistant.open / assistant.list ---------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_advertises_the_assistant() {
    let env = Env::start("", false, true).await;
    assert!(
        env.client
            .welcome()
            .capabilities
            .iter()
            .any(|c| c == "assistant")
    );
    assert!(env.client.supports_assistant());
    // The assistant arrived with protocol 0.12; later minors keep it.
    assert!(env.client.welcome().protocol.minor >= 12);
    env.stop().await;
}

/// `daemon.toml` tables that make every harness unavailable (no real OMP, Claude Code or Codex
/// of this machine is ever asked).
const NO_HARNESSES: &str = "\n[terminal]\nenv_inherit = \"daemon\"\n[harness.omp]\ncli = \"/nonexistent/hutan-test/omp\"\n[harness.claude]\ncli = \"/nonexistent/hutan-test/claude\"\n[codex]\nbin = \"/nonexistent/hutan-test/codex\"\n";

#[tokio::test(flavor = "multi_thread")]
async fn with_nothing_configured_and_nothing_ready_opening_says_what_to_set() {
    let env = Env::start(NO_HARNESSES, false, false).await;
    env.call::<hutan_protocol::messages::Empty>(Request::ProviderRemove(
        hutan_protocol::provider::ProviderRef {
            provider_id: "mock".into(),
        },
    ))
    .await;
    let (code, message) = error_of(env.try_open(AssistantScope::Global, None, false).await).await;
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(
        message.contains("[assistant]") && message.contains("provider"),
        "{message}"
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_nothing_configured_a_ready_provider_is_the_assistants_backend() {
    // `[assistant]` names nothing, but the provider `mock` has a key: it answers, with its
    // default model, and the provider is not called to open the assistant.
    let env = Env::start(NO_HARNESSES, false, false).await;
    let session = env.open(AssistantScope::Global, None, false).await;
    let chat = session.chat.clone().unwrap();
    assert_eq!(chat.agent, "assistant");
    assert_eq!(chat.provider_id.as_deref(), Some("mock"));
    assert_eq!(chat.model, "m-1");
    assert!(env.mock.requests().is_empty());
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn each_scope_has_one_default_assistant_that_survives_stops_and_restarts() {
    let env = Env::start("", false, true).await;
    let home = env.home().await;
    let project = env.project("proj", &env.work.clone()).await;

    let global = env.open(AssistantScope::Global, None, false).await;
    let chat = global.chat.clone().unwrap();
    assert_eq!(chat.agent, "assistant");
    assert_eq!(chat.provider_id.as_deref(), Some("mock"));
    assert_eq!(chat.model, "m-1");
    assert_eq!(
        global.workspace_id, home.id,
        "the global assistant lives in Home"
    );
    assert_eq!(global.title, "Mawas");
    assert_eq!(global.state, SessionState::Idle);
    assert_eq!(
        env.open(AssistantScope::Global, None, false).await.id,
        global.id,
        "the default is returned again"
    );

    let local = env
        .open(AssistantScope::Workspace, Some(project.id), false)
        .await;
    assert_ne!(local.id, global.id);
    assert_eq!(local.workspace_id, project.id);
    assert_eq!(local.title, "Mawas · proj");
    assert_eq!(
        env.open(AssistantScope::Workspace, Some(project.id), false)
            .await
            .id,
        local.id
    );
    // More can be created; the default stays the first.
    let more = env
        .open(AssistantScope::Workspace, Some(project.id), true)
        .await;
    assert_ne!(more.id, local.id);
    assert_eq!(more.title, "Mawas · proj 2");
    assert_eq!(
        env.open(AssistantScope::Workspace, Some(project.id), false)
            .await
            .id,
        local.id
    );

    // Bad requests.
    let (code, _) = error_of(env.try_open(AssistantScope::Workspace, None, false).await).await;
    assert_eq!(code, ErrorCode::BadRequest);
    let (code, message) = error_of(
        env.try_open(AssistantScope::Workspace, Some(home.id), false)
            .await,
    )
    .await;
    assert_eq!(code, ErrorCode::BadRequest);
    assert!(message.contains("global"), "{message}");
    let (code, _) = error_of(
        env.try_open(AssistantScope::Workspace, Some(WorkspaceId::new()), false)
            .await,
    )
    .await;
    assert_eq!(code, ErrorCode::NotFound);

    // The list: oldest first, scope and default per entry, filters.
    let all = env
        .call::<AssistantList>(Request::AssistantList(AssistantListRequest::default()))
        .await
        .assistants;
    assert_eq!(
        all.iter()
            .map(|e| (e.session.id, e.scope, e.default))
            .collect::<Vec<_>>(),
        [
            (global.id, AssistantScope::Global, true),
            (local.id, AssistantScope::Workspace, true),
            (more.id, AssistantScope::Workspace, false),
        ]
    );
    let only_global = env
        .call::<AssistantList>(Request::AssistantList(AssistantListRequest {
            scope: Some(AssistantScope::Global),
            workspace_id: None,
        }))
        .await;
    assert_eq!(only_global.assistants.len(), 1);
    let of_project = env
        .call::<AssistantList>(Request::AssistantList(AssistantListRequest {
            scope: None,
            workspace_id: Some(project.id),
        }))
        .await;
    assert_eq!(of_project.assistants.len(), 2);

    // A stopped default is started again by `open`.
    env.call::<SessionResult>(Request::SessionStop(SessionRef {
        session_id: local.id,
    }))
    .await;
    assert!(!env.session(local.id).await.state.is_live());
    let again = env
        .open(AssistantScope::Workspace, Some(project.id), false)
        .await;
    assert_eq!(again.id, local.id);
    assert!(env.session(local.id).await.state.is_live());

    // And a daemon restart brings the same defaults back.
    let env = env.restart().await;
    assert_eq!(
        env.open(AssistantScope::Global, None, false).await.id,
        global.id
    );
    assert_eq!(
        env.open(AssistantScope::Workspace, Some(project.id), false)
            .await
            .id,
        local.id
    );
    env.stop().await;
}

// Attachments -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn attachments_are_resolved_journaled_and_sent_and_nothing_else_is() {
    let mut env = Env::start("", true, true).await;
    let work = env.work.clone();
    let project = env.project("proj", &work).await;
    fs::write(
        work.join("src/lib.rs"),
        format!("pub fn alpha() {{}}\nconst KEY: &str = \"{SECRET}\";\npub fn gamma() {{}}\n"),
    )
    .unwrap();
    fs::write(work.join("private.txt"), "UNATTACHED-FILE").unwrap();

    let (terminal, block) = env.block_terminal(project.id).await;
    let terminal_id = terminal_of(&terminal);
    let other = create_in(
        &env.client,
        Some(project.id),
        sh("echo UNATTACHED-TERMINAL; exec cat"),
    )
    .await;
    assert_ne!(other.id, terminal.id);
    env.wait_hits("flibbertigibbet").await;

    // An earlier conversation in another assistant of the workspace, for `agent_transcript`.
    let assistant = env
        .open(AssistantScope::Workspace, Some(project.id), false)
        .await;
    let earlier = env
        .open(AssistantScope::Workspace, Some(project.id), true)
        .await;
    env.mock
        .route("POST", PATH, reply("earlier answer about pangolins"));
    env.ask(earlier.id, "earlier question about pangolins", vec![])
        .await;
    env.mock
        .set_route("POST", PATH, reply("The answer is zorblax"));

    let attachments = vec![
        Attachment::ShellBlock {
            terminal_id,
            block_id: block,
        },
        Attachment::ShellRecent {
            terminal_id,
            lines: 20,
        },
        Attachment::AgentTranscript {
            session_id: earlier.id.to_string(),
            last_turns: 3,
        },
        Attachment::File {
            path: "src/lib.rs".into(),
            range: Some(LineRange { start: 1, end: 2 }),
            picked: false,
        },
        Attachment::Search {
            query: "flibbertigibbet".into(),
            limit: 3,
        },
    ];
    let events = env
        .ask(assistant.id, "why does this fail?", attachments)
        .await;
    let text = user_text(&events);

    // The message: the question, then every part under its header.
    assert!(
        text.starts_with("why does this fail?\n\n--- attached context"),
        "{text}"
    );
    assert!(text.ends_with("--- end of attached context ---"), "{text}");
    for header in [
        "[shell: cargo test -p daemon · exit 0 · 2 lines]\nflibbertigibbet test output\nsecond line key=",
        &format!("[shell: terminal {terminal_id} · last "),
        "[agent: Mawas · proj 2 · hutan chat · 1 turns]\nuser: earlier question about pangolins\n\nassistant: earlier answer about pangolins",
        "[file: src/lib.rs · lines 1-2 · 2 lines]\npub fn alpha() {}\nconst KEY: &str =",
        "[search: \"flibbertigibbet\" · 1 hits]\n1. [hutan ",
    ] {
        assert!(text.contains(header), "missing {header:?} in\n{text}");
    }
    // Redacted, and only what was attached.
    for absent in [SECRET, "sk-abc", "UNATTACHED", "gamma"] {
        assert!(!text.contains(absent), "{absent} leaked into\n{text}");
    }
    assert!(text.contains("[REDACTED]"), "{text}");

    // What the event says about it, without the text.
    let meta = attachments_meta(user_event(&events));
    assert_eq!(
        meta.iter().map(|m| m.kind).collect::<Vec<_>>(),
        [
            AttachmentKind::ShellBlock,
            AttachmentKind::ShellRecent,
            AttachmentKind::AgentTranscript,
            AttachmentKind::File,
            AttachmentKind::Search,
        ]
    );
    assert_eq!(meta[0].label, "cargo test -p daemon");
    assert_eq!(meta[3].label, "src/lib.rs:1-2");
    assert!(meta.iter().all(|m| m.chars > 0 && !m.truncated), "{meta:?}");

    // The provider received exactly that, behind the assistant's system prompt.
    let sent = env.sent(1);
    assert_eq!(sent[0].0, "system");
    assert!(sent[0].1.contains("Hutan's assistant"), "{}", sent[0].1);
    assert_eq!(sent.last().unwrap(), &("user".to_owned(), text.clone()));
    let wire = env.mock.requests_to(PATH)[1].text();
    assert!(
        !wire.contains("UNATTACHED") && !wire.contains(SECRET),
        "{wire}"
    );

    // It is in the journal, with its meta, and the conversation continues with it.
    let journal = env.journal(assistant.id).await;
    assert_eq!(user_text(&journal), text);
    assert_eq!(attachments_meta(user_event(&journal)), meta);
    env.mock.set_route("POST", PATH, reply("ok"));
    env.ask(assistant.id, "and then?", vec![]).await;
    let second = env.sent(2);
    assert!(
        second
            .iter()
            .any(|(role, content)| role == "user" && content == &text)
    );
    assert_eq!(
        second.last().unwrap(),
        &("user".to_owned(), "and then?".to_owned())
    );

    // The chat journals are in the index, openable: the assistant's answer is a hit of the
    // assistant's session.
    let hits = env.wait_hits("zorblax").await;
    let hit = hits
        .iter()
        .find(|h| h.session_id == Some(assistant.id))
        .unwrap_or_else(|| panic!("no hit of the assistant in {hits:?}"));
    assert_eq!(hit.source, HistorySource::Hutan);
    assert_eq!(hit.role, HistoryRole::Assistant);
    assert_eq!(hit.session_key, assistant.id.to_string());
    assert_eq!(hit.title.as_deref(), Some("Mawas · proj"));
    assert_eq!(hit.resume_argv, None);
    let question = env.wait_hits("pangolins").await;
    assert!(question.iter().any(|h| h.session_id == Some(earlier.id)));
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transcript_of_another_tool_comes_from_the_index() {
    let mut env = Env::start("", true, true).await;
    let id = "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60";
    let path = env
        .base
        .join(format!("sources/claude/-work-app/{id}.jsonl"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        [
            r#"{"type":"user","timestamp":"2026-10-15T09:00:00.000Z","cwd":"/work/app","sessionId":"5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60","message":{"role":"user","content":[{"type":"text","text":"why does the quokka scheduler stall under load"}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-10-15T09:00:05.000Z","cwd":"/work/app","message":{"role":"assistant","content":[{"type":"text","text":"Checking the scheduler queue."}]}}"#,
            r#"{"type":"ai-title","aiTitle":"Scheduler stall investigation","sessionId":"5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60"}"#,
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    env.wait_hits("quokka").await;
    let assistant = env.open(AssistantScope::Global, None, false).await;
    env.mock.route("POST", PATH, reply("seen"));
    let events = env
        .ask(
            assistant.id,
            "summarise",
            vec![Attachment::AgentTranscript {
                session_id: id.into(),
                last_turns: 5,
            }],
        )
        .await;
    let text = user_text(&events);
    assert!(
        text.contains("[agent: Scheduler stall investigation · claude · 1 turns]\nuser: why does the quokka scheduler stall under load\n\nassistant: Checking the scheduler queue."),
        "{text}"
    );

    // An id nobody indexed is not found, and says where ids come from.
    let missing = env
        .send(
            assistant.id,
            "x",
            vec![Attachment::AgentTranscript {
                session_id: "no-such-session".into(),
                last_turns: 1,
            }],
        )
        .await;
    let (code, message) = error_of(missing).await;
    assert_eq!(code, ErrorCode::NotFound);
    assert!(message.contains("hutan search"), "{message}");
    env.stop().await;
}

// Permissions, bounds and confinement -----------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn refused_attachments_name_their_setting_and_nothing_is_read_or_sent() {
    let env = Env::start(
        "\n[assistant.context]\nshell = false\n[assistant.workspaces.locked]\nfiles = false\n",
        false,
        true,
    )
    .await;
    let work = env.work.clone();
    let project = env.project("proj", &work).await;
    let locked_root = env.base.join("locked");
    let locked = env.project("locked", &locked_root).await;
    fs::write(work.join("notes.txt"), "fine").unwrap();
    fs::write(locked_root.join("notes.txt"), "locked").unwrap();
    let terminal = create_in(&env.client, Some(project.id), sh("echo hello; exec cat")).await;
    let terminal_id = terminal_of(&terminal);

    let proj = env
        .open(AssistantScope::Workspace, Some(project.id), false)
        .await;
    let lock = env
        .open(AssistantScope::Workspace, Some(locked.id), false)
        .await;
    let global = env.open(AssistantScope::Global, None, false).await;

    let refused = |result: Result<ChatSent, ClientError>| async move {
        let (code, message) = error_of(result).await;
        (code, message)
    };
    // shell = false for workspaces, with the setting named; the same terminal is fine for the
    // global assistant (shell is on there).
    let (code, message) = refused(
        env.send(
            proj.id,
            "q",
            vec![Attachment::ShellRecent {
                terminal_id,
                lines: 5,
            }],
        )
        .await,
    )
    .await;
    assert_eq!(code, ErrorCode::Forbidden);
    assert!(message.contains("assistant.context.shell"), "{message}");
    // A per-workspace override names its own table.
    let (code, message) = refused(env.send(lock.id, "q", vec![file("notes.txt")]).await).await;
    assert_eq!(code, ErrorCode::Forbidden);
    assert!(
        message.contains("assistant.workspaces.locked.files"),
        "{message}"
    );
    // The global assistant has no files by default.
    let (code, message) = refused(env.send(global.id, "q", vec![file("notes.txt")]).await).await;
    assert_eq!(code, ErrorCode::Forbidden);
    assert!(
        message.contains("assistant.global_context.files"),
        "{message}"
    );
    // One refused attachment fails the message, even behind allowed ones.
    let (code, _) = refused(
        env.send(
            proj.id,
            "q",
            vec![
                file("notes.txt"),
                Attachment::ShellRecent {
                    terminal_id,
                    lines: 5,
                },
            ],
        )
        .await,
    )
    .await;
    assert_eq!(code, ErrorCode::Forbidden);
    // Nothing was journaled or sent.
    for id in [proj.id, lock.id, global.id] {
        assert!(env.journal(id).await.is_empty(), "{id}");
    }
    assert_eq!(env.mock.requests_to(PATH).len(), 0);

    // Search needs the index.
    let (code, _) = refused(
        env.send(
            proj.id,
            "q",
            vec![Attachment::Search {
                query: "x".into(),
                limit: 1,
            }],
        )
        .await,
    )
    .await;
    assert_eq!(code, ErrorCode::InvalidState);

    // Own bounds.
    let bad = |attachment| {
        let env = &env;
        async move {
            error_of(env.send(proj.id, "q", vec![attachment]).await)
                .await
                .0
        }
    };
    assert_eq!(
        bad(Attachment::ShellRecent {
            terminal_id,
            lines: 501
        })
        .await,
        ErrorCode::BadRequest
    );
    assert_eq!(
        bad(Attachment::AgentTranscript {
            session_id: "s".into(),
            last_turns: 21
        })
        .await,
        ErrorCode::BadRequest
    );
    assert_eq!(
        bad(Attachment::Search {
            query: "x".into(),
            limit: 11
        })
        .await,
        ErrorCode::BadRequest
    );
    assert_eq!(
        bad(Attachment::File {
            path: "notes.txt".into(),
            range: Some(LineRange { start: 5, end: 2 }),
            picked: false
        })
        .await,
        ErrorCode::BadRequest
    );
    let many = vec![file("notes.txt"); ATTACHMENTS_MAX + 1];
    assert_eq!(
        error_of(env.send(proj.id, "q", many).await).await.0,
        ErrorCode::BadRequest
    );

    // Files stay under the workspace roots: absolute elsewhere, `..`, a symlink out, a
    // sibling that shares a prefix.
    let outside = env.base.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "top secret").unwrap();
    symlink(outside.join("secret.txt"), work.join("link.txt")).unwrap();
    symlink(&outside, work.join("linkdir")).unwrap();
    let evil = env.base.join("work-evil");
    fs::create_dir_all(&evil).unwrap();
    fs::write(evil.join("x.txt"), "x").unwrap();
    for path in [
        outside.join("secret.txt").display().to_string(),
        "../outside/secret.txt".to_owned(),
        format!("{}/../outside/secret.txt", work.display()),
        "link.txt".to_owned(),
        "linkdir/secret.txt".to_owned(),
        evil.join("x.txt").display().to_string(),
        "/etc/hosts".to_owned(),
    ] {
        assert_eq!(bad(file(&path)).await, ErrorCode::Forbidden, "{path}");
    }
    assert_eq!(bad(file("missing.txt")).await, ErrorCode::NotFound);
    // Terminals and blocks that do not exist; the Home folder is not this workspace's.
    assert_eq!(
        error_of(
            env.send(
                global.id,
                "q",
                vec![Attachment::ShellRecent {
                    terminal_id: hutan_core::TerminalId(9999),
                    lines: 5
                }]
            )
            .await
        )
        .await
        .0,
        ErrorCode::NotFound
    );
    assert_eq!(
        error_of(
            env.send(
                global.id,
                "q",
                vec![Attachment::ShellBlock {
                    terminal_id,
                    block_id: 99
                }]
            )
            .await
        )
        .await
        .0,
        ErrorCode::NotFound
    );
    assert_eq!(
        env.mock.requests_to(PATH).len(),
        0,
        "no refused message reached the provider"
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_global_assistant_reads_files_only_under_home_when_allowed_and_the_budget_is_split() {
    let mut env = Env::start(
        "context_budget_chars = 400\n[assistant.global_context]\nfiles = true\n",
        false,
        true,
    )
    .await;
    // `context_budget_chars` belongs under [assistant]; the toml above puts it there because the
    // provider keys come first.
    let home = env.home().await;
    let folder = home.roots[0].clone();
    fs::write(folder.join("one.txt"), "1".repeat(1000)).unwrap();
    fs::write(
        folder.join("two.txt"),
        format!("{}\n{}", "2".repeat(100), "tail"),
    )
    .unwrap();
    let outside = env.base.join("elsewhere.txt");
    fs::write(&outside, "elsewhere").unwrap();
    let global = env.open(AssistantScope::Global, None, false).await;

    let (code, _) = error_of(
        env.send(global.id, "q", vec![file(outside.to_str().unwrap())])
            .await,
    )
    .await;
    assert_eq!(code, ErrorCode::Forbidden, "only the Home folder");

    env.mock.route("POST", PATH, reply("done"));
    let events = env
        .ask(global.id, "compare", vec![file("one.txt"), file("two.txt")])
        .await;
    let text = user_text(&events);
    let meta = attachments_meta(user_event(&events));
    // 400 characters over two attachments: 200 each. The first is cut, the second is not.
    assert_eq!(meta[0].chars, 200);
    assert!(meta[0].truncated);
    assert!(
        text.contains("[file: one.txt · 1 lines · cut to the first 200 characters]"),
        "{text}"
    );
    assert_eq!(meta[1].chars, 105);
    assert!(!meta[1].truncated);
    assert!(text.contains("[file: two.txt · 2 lines]"), "{text}");
    assert!(text.contains(&"1".repeat(200)) && !text.contains(&"1".repeat(201)));
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_block_over_its_share_keeps_the_end_and_says_so() {
    let mut env = Env::start("context_budget_chars = 60\n", false, true).await;
    let work = env.work.clone();
    let project = env.project("proj", &work).await;
    let script = "printf '\\033]133;A\\007$ \\033]133;B\\007seq 1 40\\r\\n\\033]133;C\\007'; seq 1 40 | while read n; do printf 'line-%s\\r\\n' $n; done; printf '\\033]133;D;2\\007\\033]133;A\\007$ \\033]133;B\\007'; exec cat";
    let session = create_in(&env.client, Some(project.id), sh(script)).await;
    let terminal = terminal_of(&session);
    let deadline = Instant::now() + WAIT;
    let block = loop {
        let blocks = env
            .client
            .terminal_blocks(terminal, None, 10)
            .await
            .unwrap();
        if let Some(done) = blocks
            .blocks
            .iter()
            .find(|b| b.state == BlockState::Finished)
        {
            break done.id;
        }
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(30)).await;
    };
    let assistant = env
        .open(AssistantScope::Workspace, Some(project.id), false)
        .await;
    env.mock.route("POST", PATH, reply("ok"));
    let events = env
        .ask(
            assistant.id,
            "why exit 2?",
            vec![Attachment::ShellBlock {
                terminal_id: terminal,
                block_id: block,
            }],
        )
        .await;
    let text = user_text(&events);
    assert!(
        text.contains("exit 2 · 40 lines · cut to the last 60 of "),
        "{text}"
    );
    assert!(
        text.contains("line-40") && !text.contains("line-1\n"),
        "{text}"
    );
    let meta = attachments_meta(user_event(&events));
    assert!(meta[0].truncated && meta[0].chars == 60, "{meta:?}");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_or_unknown_session_takes_no_chat_message() {
    let env = Env::start("", false, true).await;
    let work = env.work.clone();
    let project = env.project("proj", &work).await;
    let terminal = create_in(&env.client, Some(project.id), sh("exec cat")).await;
    let result = env.send(terminal.id, "x", vec![file("a")]).await;
    assert!(matches!(
        remote_code(result),
        ErrorCode::BadRequest | ErrorCode::InvalidState | ErrorCode::NotFound
    ));
    let (code, _) = error_of(env.send(SessionId::new(), "x", vec![file("a")]).await).await;
    assert!(matches!(code, ErrorCode::NotFound | ErrorCode::BadRequest));
    env.stop().await;
}

// Images (protocol 0.23) ------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn images_reach_a_provider_that_says_the_model_reads_them_and_are_refused_otherwise() {
    let mut env = Env::start("", false, true).await;
    let png = b"\x89PNG\r\n\x1a\nxyz";
    let data = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(png)
    };
    let image = || Attachment::Image {
        path: None,
        data_base64: Some(data.clone()),
        media_type: "image/png".into(),
    };
    let models = |modalities: &[&str]| {
        Reply::json(
            200,
            &serde_json::json!({"data": [
                {"id": "m-1", "architecture": {"input_modalities": modalities}}
            ]})
            .to_string(),
        )
    };

    // The model list says `m-1` reads images: the message carries an `image_url` data URL.
    env.mock
        .route("GET", "/v1/models", models(&["text", "image"]));
    env.mock.route("POST", PATH, reply("a tiny image"));
    let assistant = env.open(AssistantScope::Global, None, false).await;
    let events = env.ask(assistant.id, "what is this?", vec![image()]).await;
    let content = env.mock.requests_to(PATH)[0].json()["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"]
        .clone();
    assert_eq!(content[0]["type"], "text");
    assert!(
        content[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("what is this?")
    );
    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(
        content[1]["image_url"]["url"],
        format!("data:image/png;base64,{data}")
    );
    // The journal has the header and the meta, not the bytes.
    let text = user_text(&events);
    assert!(text.contains("[image: pasted image · image/png"), "{text}");
    assert!(!text.contains(&data));
    let meta = attachments_meta(user_event(&events));
    assert_eq!(meta[0].kind, AttachmentKind::Image);
    assert_eq!(meta[0].size, Some(png.len() as u64));
    assert_eq!(meta[0].path, None);

    // A model the list says is text only: `unsupported`, and nothing reaches the provider.
    env.mock.set_route("GET", "/v1/models", models(&["text"]));
    let text_only = env.open(AssistantScope::Global, None, true).await;
    let before = env.mock.requests_to(PATH).len();
    let refused = env.send(text_only.id, "and this?", vec![image()]).await;
    assert_eq!(remote_code(refused), ErrorCode::Unsupported);
    assert_eq!(env.mock.requests_to(PATH).len(), before);
    env.stop().await;
}
