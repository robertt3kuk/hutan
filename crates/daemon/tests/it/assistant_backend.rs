//! The assistant's backend per scope (protocol 0.20, `docs/assistant.md` § Backends): a real
//! daemon on a temporary `HUTAN_HOME`, real client connections, `hutan-acp-fixture replay`
//! standing in for `omp acp` and Claude Code's adapter, `hutan-codex-fixture replay` for
//! `codex app-server`, and a provider that is never called. Nothing here starts a real harness,
//! reads a login or calls a model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{TestDaemon, remote_code, wait_event};
use hutan_client::Client;
use hutan_core::agent::MessageRole;
use hutan_core::{
    AgentEvent, AgentEventKind, ApprovalPolicy, SandboxMode, Seq, SessionId, SessionState,
};
use hutan_daemon::GatewayConfig;
use hutan_gateway::{CredentialStore, HttpConfig, MemoryStore};
use hutan_protocol::ErrorCode;
use hutan_protocol::assistant::{
    AssistantBackend, AssistantConfig, AssistantConfigure, AssistantList, AssistantListRequest,
    AssistantOpen, AssistantScope, Attachment,
};
use hutan_protocol::chat::{ChatHistory, ChatHistoryRequest};
use hutan_protocol::messages::{
    Event, Request, SessionInfo, SessionList, SessionListFilter, SessionResult, WorkspaceCreate,
    WorkspaceInfo, WorkspaceList,
};
use hutan_protocol::provider::{ProviderAdd, ProviderResult};
use serde_json::Value;
use tokio::sync::broadcast;

fn acp_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/acp-omp-18.11")
}

fn codex_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/codex-0.162")
}

/// A fixture binary, next to the test binaries (`just test` builds them first).
fn fixture_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().and_then(Path::parent).unwrap().join(name);
    assert!(
        bin.is_file(),
        "{name} not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

/// A fake harness CLI (`omp models --json`, `claude auth status`…): a shell script of fixed
/// content at a path named by its content, written once for the same reason as the wrapper.
fn fake_cli(name: &str, body: &str) -> PathBuf {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let text = format!("#!/bin/sh\n{body}\n");
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let dir = std::env::temp_dir().join("hutan-test-fake-clis");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{:016x}-{name}", hasher.finish()));
    if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
        return path;
    }
    let staging = dir.join(format!(".{}-{name}", std::process::id()));
    std::fs::write(&staging, &text).unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&staging, &path).unwrap();
    path
}

/// What `omp models --json` prints for a user logged in to Anthropic and OpenAI.
const OMP_MODELS_JSON: &str = r#"{"models":[
 {"provider":"anthropic","kind":"chat","id":"claude-opus-4-1","selector":"anthropic/claude-opus-4-1","name":"Claude Opus 4.1"},
 {"provider":"openai","kind":"chat","id":"gpt-5","selector":"openai/gpt-5","name":"GPT-5"}
]}"#;

/// The model the ACP fixtures start on (`fixtures/acp-omp-18.11`, option `model`).
const FIXTURE_MODEL: &str = "gitlab-duo/duo-chat-opus-4-6";
/// The newest Sonnet of that provider in the same fixtures.
const FIXTURE_SONNET: &str = "gitlab-duo/duo-chat-sonnet-4-6";

/// `[assistant]` lines naming a harness and the model the fixtures start on, so that the
/// harness's session is the one the transcripts script (no model switch).
const OMP_ASSISTANT: &str = "backend = \"omp\"\nmodel = \"gitlab-duo/duo-chat-opus-4-6\"";
const CLAUDE_ASSISTANT: &str = "backend = \"claude\"\nmodel = \"gitlab-duo/duo-chat-opus-4-6\"";
/// The same, in Advisor mode (read-only launch).
const OMP_ADVISOR: &str =
    "backend = \"omp\"\nmodel = \"gitlab-duo/duo-chat-opus-4-6\"\nmode = \"advisor\"";
const CLAUDE_ADVISOR: &str =
    "backend = \"claude\"\nmodel = \"gitlab-duo/duo-chat-opus-4-6\"\nmode = \"advisor\"";

/// A wrapper standing in for `omp` / the Claude Code adapter: it records the arguments it was
/// started with in `$ARGV_LOG` and becomes the fake agent (`$FIXTURE`). One file of fixed
/// content in the system temp directory, so the system vets it once, not once per test.
fn argv_logging_wrapper() -> PathBuf {
    static WRAPPER: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| {
        let text = "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$ARGV_LOG\"\nexec \"$FIXTURE\" \"$@\"\n";
        let dir = std::env::temp_dir().join("hutan-test-fake-clis");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("assistant-backend-argv-wrapper");
        if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
            return path;
        }
        let staging = dir.join(format!(
            ".{}-assistant-backend-argv-wrapper",
            std::process::id()
        ));
        std::fs::write(&staging, text).unwrap();
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&staging, &path).unwrap();
        path
    });
    WRAPPER.clone()
}

fn gateway() -> GatewayConfig {
    GatewayConfig {
        credentials: Some(Arc::new(MemoryStore::new()) as Arc<dyn CredentialStore>),
        http: HttpConfig {
            no_proxy: true,
            connect_retries: 1,
            retry_backoff: Duration::from_millis(5),
            ..HttpConfig::default()
        },
        discovery: Some(Vec::new()),
    }
}

struct B {
    daemon: TestDaemon,
    /// The project folder (canonical), holding `notes.txt`.
    project: PathBuf,
    /// Where the fake agents log what they received and how they were started.
    logs: tempfile::TempDir,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
}

/// Which fake agents the daemon is set up with.
struct Setup {
    omp: &'static str,
    claude: &'static str,
    codex: &'static str,
    /// Lines for `[assistant]`.
    assistant: &'static str,
    /// `[harness.omp] bin` names a program that does not exist.
    missing_omp: bool,
    /// `[codex] bin` names a program that does not exist.
    missing_codex: bool,
    /// The body of a fake `omp` CLI (`[harness.omp] cli`), which answers `omp models --json`
    /// for the readiness check of the automatic backend.
    omp_cli: Option<String>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            omp: "plain-turn",
            claude: "mode-plan",
            codex: "plain-turn",
            assistant: "",
            missing_omp: false,
            missing_codex: false,
            omp_cli: None,
        }
    }
}

impl B {
    async fn start(setup: Setup) -> Self {
        let logs = tempfile::tempdir().unwrap();
        let toml = Self::toml(logs.path(), &setup);
        let daemon = TestDaemon::start_with(&toml, |config| config.gateway = gateway()).await;
        Self::around(daemon, logs).await
    }

    async fn around(daemon: TestDaemon, logs: tempfile::TempDir) -> Self {
        let (client, events) = daemon.connect_subscribed().await;
        let project = logs.path().canonicalize().unwrap().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("notes.txt"), "remember the milk\n").unwrap();
        Self {
            daemon,
            project,
            logs,
            client,
            events,
        }
    }

    /// A `daemon.toml` whose harnesses are the fake agents: `omp` and `claude` through the
    /// argument-logging wrapper, Codex through its fixture directly.
    fn toml(logs: &Path, setup: &Setup) -> String {
        let wrapper = argv_logging_wrapper();
        let acp = fixture_bin("hutan-acp-fixture");
        let codex = fixture_bin("hutan-codex-fixture");
        let harness = |id: &str, cli: Option<PathBuf>| {
            format!(
                "[harness.{id}]\nbin = {wrapper:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\nenv = {{ ARGV_LOG = {log:?}, FIXTURE = {acp:?} }}\n{cli}\n",
                received = logs.join(format!("{id}-received.jsonl")),
                transcript = acp_fixtures().join(format!(
                    "{}.jsonl",
                    if id == "omp" { setup.omp } else { setup.claude }
                )),
                log = logs.join(format!("{id}-argv.log")),
                cli = cli.map_or_else(String::new, |cli| format!("cli = {cli:?}\n")),
            )
        };
        let omp = if setup.missing_omp {
            format!("[harness.omp]\nbin = {:?}\n\n", logs.join("no-such-omp"))
        } else {
            harness(
                "omp",
                setup.omp_cli.as_deref().map(|body| fake_cli("omp", body)),
            )
        };
        // The CLI that reports Claude Code's login is never the real one on this machine.
        let claude = harness("claude", Some(logs.join("no-such-claude")));
        let codex_table = if setup.missing_codex {
            format!("[codex]\nbin = {:?}\n\n", logs.join("no-such-codex"))
        } else {
            format!(
                "[codex]\nbin = {codex:?}\nargs = [\"replay\", \"--received\", {codex_received:?}, {codex_transcript:?}, \"app-server\"]\n\n",
                codex_received = logs.join("codex-received.jsonl"),
                codex_transcript = codex_fixtures().join(format!("{}.jsonl", setup.codex)),
            )
        };
        format!(
            "[terminal]\nenv_inherit = \"daemon\"\n\n{omp}{claude}{codex_table}[assistant]\n{}\n",
            setup.assistant,
        )
    }

    /// What a fake agent received, as `(method or "response", message)`.
    fn received(&self, id: &str) -> Vec<(String, Value)> {
        std::fs::read_to_string(self.logs.path().join(format!("{id}-received.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let wrapper: Value = serde_json::from_str(line).unwrap();
                let msg = wrapper["msg"].clone();
                (msg["method"].as_str().unwrap_or("response").to_owned(), msg)
            })
            .collect()
    }

    fn received_of(&self, id: &str, method: &str) -> Vec<Value> {
        self.received(id)
            .into_iter()
            .filter(|(name, _)| name == method)
            .map(|(_, msg)| msg)
            .collect()
    }

    /// The arguments the wrapper started `id` with, one per line.
    fn argv(&self, id: &str) -> Vec<String> {
        std::fs::read_to_string(self.logs.path().join(format!("{id}-argv.log")))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    async fn workspace(&self, name: &str) -> WorkspaceInfo {
        self.client
            .call::<WorkspaceInfo>(Request::WorkspaceCreate(WorkspaceCreate {
                name: name.into(),
                paths: vec![self.project.clone()],
                project_ids: Vec::new(),
                created_by: None,
            }))
            .await
            .unwrap()
    }

    /// Stores a backend for a scope. A harness backend given no model is stored with the one
    /// the fixtures start on: these tests are about which backend runs, and a different model
    /// would need a transcript that switches to it. The automatic pick has its own tests
    /// (`configure_exact`).
    async fn configure(
        &self,
        scope: AssistantScope,
        workspace: Option<&WorkspaceInfo>,
        backend: AssistantBackend,
        model: Option<&str>,
        provider: Option<&str>,
    ) -> Result<AssistantConfig, hutan_client::ClientError> {
        let harness = backend != AssistantBackend::Provider && provider.is_none();
        let model = model.or(harness.then_some(FIXTURE_MODEL));
        self.configure_exact(scope, workspace, backend, model, provider)
            .await
    }

    /// Stores exactly what it is given.
    async fn configure_exact(
        &self,
        scope: AssistantScope,
        workspace: Option<&WorkspaceInfo>,
        backend: AssistantBackend,
        model: Option<&str>,
        provider: Option<&str>,
    ) -> Result<AssistantConfig, hutan_client::ClientError> {
        self.client
            .call(Request::AssistantConfigure(AssistantConfigure {
                scope,
                workspace_id: workspace.map(|w| w.id),
                backend,
                model: model.map(str::to_owned),
                provider: provider.map(str::to_owned),
            }))
            .await
    }

    async fn open(
        &self,
        scope: AssistantScope,
        workspace: Option<&WorkspaceInfo>,
        new: bool,
    ) -> Result<SessionInfo, hutan_client::ClientError> {
        self.client
            .call::<SessionResult>(Request::AssistantOpen(AssistantOpen {
                scope,
                workspace_id: workspace.map(|w| w.id),
                new,
            }))
            .await
            .map(|r| r.session)
    }

    /// The first message of an assistant: opening one starts nothing, this starts its agent.
    /// The turn itself is not the test's business (the fixture may not script it); the agent
    /// having started is.
    async fn start_agent(&self, id: SessionId) -> SessionInfo {
        let _ = self.client.chat_send(id, "hello").await;
        tokio::time::timeout(common::WAIT, async {
            loop {
                let session = self.session(id).await;
                if session
                    .chat
                    .as_ref()
                    .is_some_and(|c| c.agent_session_id.is_some())
                {
                    return session;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the first message starts the agent")
    }

    async fn list(&self) -> AssistantList {
        self.client
            .call(Request::AssistantList(AssistantListRequest::default()))
            .await
            .unwrap()
    }

    async fn history(&self, id: SessionId) -> ChatHistory {
        self.client
            .call(Request::ChatHistory(ChatHistoryRequest {
                session_id: id,
                before_seq: None,
                limit: 500,
            }))
            .await
            .unwrap()
    }

    async fn session(&self, id: SessionId) -> SessionInfo {
        self.client
            .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap()
            .sessions
            .into_iter()
            .find(|s| s.id == id)
            .expect("session listed")
    }

    async fn wait_turn_end(&mut self) -> AgentEvent {
        wait_event(&mut self.events, "the turn to end", |event| match event {
            Event::ChatEvent { event, .. } if event.kind.ends_turn() => Some((**event).clone()),
            _ => None,
        })
        .await
    }

    async fn add_provider(&self, name: &str) {
        self.client
            .call::<ProviderResult>(Request::ProviderAdd(ProviderAdd {
                kind: "openai-compatible".into(),
                name: name.into(),
                base_url: Some("http://127.0.0.1:9/v1".into()),
                default_model: Some("m-1".into()),
            }))
            .await
            .unwrap();
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

/// A daemon with nothing configured and no harness that is ready.
fn nothing_ready() -> Setup {
    Setup {
        missing_omp: true,
        missing_codex: true,
        ..Setup::default()
    }
}

/// The fake `omp` CLI of a user who is logged in: `omp models --json`.
fn omp_models_script() -> String {
    format!("case \"$1\" in models) cat <<'EOF'\n{OMP_MODELS_JSON}\nEOF\n;; esac")
}

#[tokio::test(flavor = "multi_thread")]
async fn when_nothing_is_ready_open_names_what_was_checked_and_what_to_configure() {
    let t = B::start(nothing_ready()).await;
    let error = t
        .open(AssistantScope::Global, None, false)
        .await
        .unwrap_err();
    assert_eq!(remote_code(Err::<(), _>(error)), ErrorCode::InvalidState);
    let message = t
        .open(AssistantScope::Global, None, false)
        .await
        .unwrap_err()
        .to_string();
    // The ways to configure it, as before…
    assert!(
        message.contains("assistant.configure") && message.contains("[assistant]"),
        "{message}"
    );
    // …and one reason per candidate the automatic backend looked at.
    for candidate in [
        "omp: ",
        "claude: ",
        "codex: ",
        "provider: none is configured",
    ] {
        assert!(message.contains(candidate), "{candidate} in {message}");
    }
    assert!(message.contains("was not found"), "{message}");
    assert!(t.list().await.assistants.is_empty());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_without_a_key_is_not_ready_but_one_on_this_machine_is() {
    let t = B::start(nothing_ready()).await;
    // `openai` needs a key and has none.
    t.client
        .call::<ProviderResult>(Request::ProviderAdd(ProviderAdd {
            kind: "openai".into(),
            name: "OpenAI".into(),
            base_url: None,
            default_model: Some("gpt-5".into()),
        }))
        .await
        .unwrap();
    let message = t
        .open(AssistantScope::Global, None, false)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("provider: `openai` has no API key"),
        "{message}"
    );

    // A server on this machine needs none; the first ready provider is used, with its default
    // model (no catalog is read and no model is called).
    t.add_provider("mock").await;
    let session = t.open(AssistantScope::Global, None, false).await.unwrap();
    let chat = session.chat.clone().unwrap();
    assert!(chat.is_assistant());
    assert_eq!(chat.provider_id.as_deref(), Some("mock"));
    assert_eq!(chat.model, "m-1");
    let entry = t.list().await.assistants.remove(0);
    assert_eq!(entry.backend, AssistantBackend::Provider);
    assert!(entry.default);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_nothing_configured_a_ready_omp_runs_the_assistant_one_tier_below_its_default() {
    let t = B::start(Setup {
        omp: "model-switch",
        omp_cli: Some(omp_models_script()),
        missing_codex: true,
        ..Setup::default()
    })
    .await;
    // The catalog of the omp session defaults to Opus 4.6.
    let catalog = t.client.chat_models("omp").await.unwrap();
    assert_eq!(
        catalog
            .models
            .iter()
            .filter(|m| m.is_default)
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        [FIXTURE_MODEL]
    );
    let catalog_sessions = t.received_of("omp", "session/new").len();
    let session = t.open(AssistantScope::Global, None, false).await.unwrap();
    let chat = session.chat.clone().unwrap();
    assert_eq!(chat.agent, "omp");
    assert!(chat.is_assistant());
    // Opening started nothing and picked nothing; the first message does both.
    assert_eq!(chat.model, "");
    assert_eq!(session.state, SessionState::Idle);
    assert_eq!(
        t.received_of("omp", "session/new").len(),
        catalog_sessions,
        "no session of the assistant's own yet"
    );
    let session = t.start_agent(session.id).await;
    let chat = session.chat.clone().unwrap();
    assert_eq!(chat.model, FIXTURE_SONNET, "what the session runs");
    // The harness was asked for that model.
    let set = t.received_of("omp", "session/set_config_option");
    assert_eq!(set.last().unwrap()["params"]["value"], FIXTURE_SONNET);
    // The listing says what runs it, and the default is that session.
    let entry = t.list().await.assistants.remove(0);
    assert_eq!(entry.backend, AssistantBackend::Omp);
    assert!(entry.default);
    assert_eq!(entry.session.chat.unwrap().model, FIXTURE_SONNET);
    // A second open finds the same session and starts nothing.
    let sessions_before = t.received_of("omp", "session/new").len();
    assert!(sessions_before >= 1, "the agent started");
    assert_eq!(
        t.open(AssistantScope::Global, None, false)
            .await
            .unwrap()
            .id,
        session.id
    );
    assert_eq!(t.received_of("omp", "session/new").len(), sessions_before);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_model_is_used_as_it_is_and_a_backend_without_one_gets_the_pick() {
    let haiku = "gitlab-duo/claude-haiku-4-5-20251001";
    let t = B::start(Setup {
        omp: "model-switch",
        ..Setup::default()
    })
    .await;
    // Stored with a model: used as it is, no catalog is read for it.
    t.configure(
        AssistantScope::Global,
        None,
        AssistantBackend::Omp,
        Some(haiku),
        None,
    )
    .await
    .unwrap();
    let session = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert_eq!(session.chat.clone().unwrap().model, haiku);
    assert!(
        t.received_of("omp", "session/new").is_empty(),
        "opening starts nothing"
    );
    let session = t.start_agent(session.id).await;
    assert_eq!(session.chat.clone().unwrap().model, haiku);
    assert_eq!(
        t.received_of("omp", "session/new").len(),
        1,
        "only the assistant's own session: no throwaway session for a catalog"
    );
    t.stop().await;

    // Stored without a model (what the setup stores): the pick fills the gap.
    let t = B::start(Setup {
        omp: "model-switch",
        ..Setup::default()
    })
    .await;
    t.configure_exact(
        AssistantScope::Global,
        None,
        AssistantBackend::Omp,
        None,
        None,
    )
    .await
    .unwrap();
    let session = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert_eq!(
        session.chat.clone().unwrap().model,
        "",
        "picked when the agent starts"
    );
    let session = t.start_agent(session.id).await;
    assert_eq!(session.chat.unwrap().model, FIXTURE_SONNET);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_toml_backend_omp_creates_a_read_only_omp_assistant_in_advisor_mode() {
    let mut t = B::start(Setup {
        assistant: OMP_ADVISOR,
        ..Setup::default()
    })
    .await;
    let session = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert_eq!(session.state, SessionState::Idle);
    let chat = session.chat.clone().unwrap();
    assert_eq!(chat.agent, "omp");
    assert!(chat.assistant && chat.is_assistant());
    assert_eq!(session.title, "Mawas");
    assert!(t.argv("omp").is_empty(), "opening starts nothing");
    // The first message starts the agent and is an ordinary harness turn: it streams.
    t.client.chat_send(session.id, "Say hello.").await.unwrap();
    t.wait_turn_end().await;
    // OMP started with its tools limited to reading and the assistant prompt appended.
    let argv = t.argv("omp");
    let tools = argv.iter().position(|a| a == "--tools").expect("--tools");
    assert_eq!(argv[tools + 1], "read,grep,glob");
    let prompt = argv
        .iter()
        .position(|a| a == "--append-system-prompt")
        .expect("--append-system-prompt");
    assert!(argv[prompt + 1].contains("cannot run commands"), "{argv:?}");
    // The default is that session, found again by a second open.
    let again = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert_eq!(again.id, session.id);
    let entry = t.list().await.assistants.remove(0);
    assert!(entry.default);
    assert_eq!(entry.backend, AssistantBackend::Omp);
    let history = t.history(session.id).await;
    assert!(history.events.iter().any(|e| matches!(
        &e.kind,
        AgentEventKind::MessageEnd {
            role: MessageRole::Assistant,
            ..
        }
    )));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ask_before_acting_omp_assistant_has_every_tool_and_routes_its_approvals_to_hutan() {
    let t = B::start(Setup {
        assistant: OMP_ASSISTANT,
        ..Setup::default()
    })
    .await;
    let session = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert!(session.chat.clone().unwrap().assistant);
    t.start_agent(session.id).await;
    // OMP started with no tool restriction, asking the client (Hutan's approval cards) for
    // everything above reading, and the prompt that says it can act.
    let argv = t.argv("omp");
    assert!(!argv.iter().any(|a| a == "--tools"), "{argv:?}");
    let mode = argv
        .iter()
        .position(|a| a == "--approval-mode")
        .expect("--approval-mode");
    assert_eq!(argv[mode + 1], "always-ask");
    let prompt = argv
        .iter()
        .position(|a| a == "--append-system-prompt")
        .expect("--append-system-prompt");
    assert!(argv[prompt + 1].contains("can act"), "{argv:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_advisor_claude_assistant_runs_in_plan_mode() {
    let t = B::start(Setup {
        assistant: CLAUDE_ADVISOR,
        ..Setup::default()
    })
    .await;
    let session = t.open(AssistantScope::Global, None, false).await.unwrap();
    let chat = session.chat.clone().unwrap();
    assert_eq!(chat.agent, "claude");
    assert!(chat.assistant);
    t.start_agent(session.id).await;
    let modes = t.received_of("claude", "session/set_mode");
    assert_eq!(modes.len(), 1, "{:?}", t.received("claude"));
    assert_eq!(modes[0]["params"]["modeId"], "plan");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ask_before_acting_claude_assistant_runs_in_its_default_permission_mode() {
    let t = B::start(Setup {
        assistant: CLAUDE_ASSISTANT,
        ..Setup::default()
    })
    .await;
    let session = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert!(session.chat.clone().unwrap().assistant);
    t.start_agent(session.id).await;
    // The fixture's session starts in `default` already, and nothing asks for `plan`.
    let modes = t.received_of("claude", "session/set_mode");
    assert!(
        modes.iter().all(|m| m["params"]["modeId"] != "plan"),
        "{modes:?}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_advisor_codex_assistant_has_a_read_only_sandbox_and_never_asks() {
    let t = B::start(Setup {
        assistant: "backend = \"codex\"\nmode = \"advisor\"",
        ..Setup::default()
    })
    .await;
    let session = t.open(AssistantScope::Global, None, false).await.unwrap();
    let chat = session.chat.clone().unwrap();
    assert_eq!(chat.agent, "codex");
    assert!(chat.assistant);
    assert_eq!(chat.sandbox, SandboxMode::ReadOnly);
    assert_eq!(chat.approval_policy, ApprovalPolicy::Never);
    t.start_agent(session.id).await;
    let start = t.received_of("codex", "thread/start");
    assert_eq!(start[0]["params"]["sandbox"], "read-only");
    assert_eq!(start[0]["params"]["approvalPolicy"], "never");
    assert_eq!(
        t.list().await.assistants[0].backend,
        AssistantBackend::Codex
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ask_before_acting_codex_assistant_writes_in_the_workspace_and_asks_on_request() {
    let t = B::start(Setup {
        assistant: "backend = \"codex\"",
        ..Setup::default()
    })
    .await;
    let session = t.open(AssistantScope::Global, None, false).await.unwrap();
    let chat = session.chat.clone().unwrap();
    assert_eq!(chat.agent, "codex");
    assert!(chat.assistant);
    assert_eq!(chat.sandbox, SandboxMode::WorkspaceWrite);
    assert_eq!(chat.approval_policy, ApprovalPolicy::OnRequest);
    t.start_agent(session.id).await;
    let start = t.received_of("codex", "thread/start");
    assert_eq!(start[0]["params"]["sandbox"], "workspace-write");
    assert_eq!(start[0]["params"]["approvalPolicy"], "on-request");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_assistant_is_the_direct_chat_it_always_was() {
    let t = B::start(Setup {
        assistant: "provider = \"mock\"\nmodel = \"m-1\"",
        ..Setup::default()
    })
    .await;
    t.add_provider("mock").await;
    // `provider` alone (no `backend`) is the provider backend, as before protocol 0.20.
    let session = t.open(AssistantScope::Global, None, false).await.unwrap();
    let chat = session.chat.clone().unwrap();
    assert_eq!(chat.agent, "assistant");
    assert!(!chat.assistant && chat.is_assistant());
    assert_eq!(chat.provider_id.as_deref(), Some("mock"));
    assert_eq!(
        t.list().await.assistants[0].backend,
        AssistantBackend::Provider
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn configure_validates_and_echoes_what_it_stored() {
    let t = B::start(Setup::default()).await;
    let project = t.workspace("proj").await;
    let echoed = t
        .configure(
            AssistantScope::Workspace,
            Some(&project),
            AssistantBackend::Omp,
            Some("anthropic/claude-sonnet-4-5"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(echoed.backend, AssistantBackend::Omp);
    assert_eq!(echoed.workspace_id, Some(project.id));
    assert_eq!(echoed.model.as_deref(), Some("anthropic/claude-sonnet-4-5"));

    let code =
        |result: Result<AssistantConfig, hutan_client::ClientError>| remote_code(result.map(drop));
    // A provider backend needs a provider that exists; a harness backend takes none.
    assert_eq!(
        code(
            t.configure(
                AssistantScope::Global,
                None,
                AssistantBackend::Provider,
                None,
                None
            )
            .await
        ),
        ErrorCode::BadRequest
    );
    assert_eq!(
        code(
            t.configure(
                AssistantScope::Global,
                None,
                AssistantBackend::Provider,
                None,
                Some("nope")
            )
            .await
        ),
        ErrorCode::BadRequest
    );
    assert_eq!(
        code(
            t.configure(
                AssistantScope::Global,
                None,
                AssistantBackend::Omp,
                None,
                Some("mock")
            )
            .await
        ),
        ErrorCode::BadRequest
    );
    // The workspace scope needs a workspace, and not the Home one.
    assert_eq!(
        code(
            t.configure(
                AssistantScope::Workspace,
                None,
                AssistantBackend::Omp,
                None,
                None
            )
            .await
        ),
        ErrorCode::BadRequest
    );
    let home = t
        .client
        .call::<WorkspaceList>(Request::WorkspaceList(hutan_protocol::messages::Empty {}))
        .await
        .unwrap()
        .workspaces
        .into_iter()
        .find(|w| w.kind == hutan_protocol::messages::WorkspaceKind::Home)
        .unwrap();
    assert_eq!(
        code(
            t.configure(
                AssistantScope::Workspace,
                Some(&home),
                AssistantBackend::Omp,
                None,
                None
            )
            .await
        ),
        ErrorCode::BadRequest
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn configuration_survives_a_daemon_restart_and_a_workspace_wins_over_the_global_choice() {
    let t = B::start(Setup::default()).await;
    let project = t.workspace("proj").await;
    t.configure(
        AssistantScope::Global,
        None,
        AssistantBackend::Omp,
        None,
        None,
    )
    .await
    .unwrap();
    t.configure(
        AssistantScope::Workspace,
        Some(&project),
        AssistantBackend::Claude,
        None,
        None,
    )
    .await
    .unwrap();
    // Nothing was opened yet: the daemon restarts with only the stored choices.
    let B {
        daemon,
        logs,
        client,
        ..
    } = t;
    drop(client);
    let daemon = daemon
        .restart_with(|config| config.gateway = gateway())
        .await;
    let t = B::around(daemon, logs).await;
    let project = t
        .client
        .call::<WorkspaceList>(Request::WorkspaceList(hutan_protocol::messages::Empty {}))
        .await
        .unwrap()
        .workspaces
        .into_iter()
        .find(|w| w.name == "proj")
        .expect("the workspace is still there");

    let global = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert_eq!(global.chat.as_ref().unwrap().agent, "omp");
    let scoped = t
        .open(AssistantScope::Workspace, Some(&project), false)
        .await
        .unwrap();
    let chat = scoped.chat.as_ref().unwrap();
    assert_eq!((chat.agent.as_str(), chat.assistant), ("claude", true));
    assert_eq!(scoped.workspace_id, project.id);
    assert_ne!(scoped.workspace_id, global.workspace_id);
    let listed = t.list().await;
    assert_eq!(listed.assistants.len(), 2);
    assert!(listed.assistants.iter().all(|e| e.default));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_without_its_own_choice_follows_the_global_one() {
    let t = B::start(Setup {
        assistant: OMP_ASSISTANT,
        ..Setup::default()
    })
    .await;
    let project = t.workspace("proj").await;
    // `daemon.toml` says omp; the global choice says claude, and wins for every workspace
    // that has none of its own.
    t.configure(
        AssistantScope::Global,
        None,
        AssistantBackend::Claude,
        None,
        None,
    )
    .await
    .unwrap();
    let session = t
        .open(AssistantScope::Workspace, Some(&project), false)
        .await
        .unwrap();
    assert_eq!(session.chat.as_ref().unwrap().agent, "claude");
    assert_eq!(session.title, "Mawas · proj");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_the_backend_keeps_each_conversation_and_opens_the_one_for_the_backend() {
    let t = B::start(Setup {
        assistant: OMP_ASSISTANT,
        ..Setup::default()
    })
    .await;
    t.add_provider("mock").await;
    let omp = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert_eq!(omp.chat.as_ref().unwrap().agent, "omp");

    t.configure(
        AssistantScope::Global,
        None,
        AssistantBackend::Provider,
        Some("m-1"),
        Some("mock"),
    )
    .await
    .unwrap();
    let direct = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert_ne!(
        direct.id, omp.id,
        "a second assistant for the second backend"
    );
    assert_eq!(direct.chat.as_ref().unwrap().agent, "assistant");
    let listed = t.list().await.assistants;
    assert_eq!(listed.len(), 2);
    assert_eq!(
        listed
            .iter()
            .map(|e| (e.session.id, e.default))
            .collect::<Vec<_>>(),
        [(omp.id, false), (direct.id, true)],
        "the default is the oldest session of the configured backend"
    );
    assert_eq!(direct.title, "Mawas 2");

    // Back to OMP: the first conversation is the default again, not a third assistant.
    t.configure(
        AssistantScope::Global,
        None,
        AssistantBackend::Omp,
        None,
        None,
    )
    .await
    .unwrap();
    let back = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert_eq!(back.id, omp.id);
    // `new` makes one more on the configured backend.
    let more = t.open(AssistantScope::Global, None, true).await.unwrap();
    assert_ne!(more.id, omp.id);
    assert_eq!(more.chat.as_ref().unwrap().agent, "omp");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_reach_an_omp_assistant_as_embedded_context() {
    let mut t = B::start(Setup {
        assistant: OMP_ASSISTANT,
        ..Setup::default()
    })
    .await;
    let project = t.workspace("proj").await;
    let session = t
        .open(AssistantScope::Workspace, Some(&project), false)
        .await
        .unwrap();
    assert_eq!(session.chat.as_ref().unwrap().agent, "omp");
    t.client
        .chat_send_with(
            session.id,
            "what do my notes say?",
            vec![Attachment::File {
                path: "notes.txt".into(),
                range: None,
                picked: false,
            }],
        )
        .await
        .unwrap();
    t.wait_turn_end().await;
    let prompt = &t.received_of("omp", "session/prompt")[0]["params"]["prompt"];
    assert_eq!(prompt[0]["text"], "what do my notes say?");
    assert_eq!(prompt[1]["type"], "resource");
    let resource = prompt[1]["resource"]["text"].as_str().unwrap();
    assert!(resource.contains("remember the milk"), "{resource}");
    assert!(
        resource.contains("[file: notes.txt"),
        "the header says what it is: {resource}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settings_refusal_still_gates_what_a_harness_assistant_may_read() {
    let t = B::start(Setup {
        assistant: "backend = \"omp\"\nmodel = \"gitlab-duo/duo-chat-opus-4-6\"\n[assistant.context]\nfiles = false",
        ..Setup::default()
    })
    .await;
    let project = t.workspace("proj").await;
    let session = t
        .open(AssistantScope::Workspace, Some(&project), false)
        .await
        .unwrap();
    let error = t
        .client
        .chat_send_with(
            session.id,
            "read it",
            vec![Attachment::File {
                path: "notes.txt".into(),
                range: None,
                picked: false,
            }],
        )
        .await
        .unwrap_err();
    assert_eq!(remote_code(Err::<(), _>(error)), ErrorCode::Forbidden);
    assert!(t.received_of("omp", "session/prompt").is_empty());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_reach_a_codex_assistant_as_quoted_text() {
    let mut t = B::start(Setup {
        assistant: "backend = \"codex\"",
        ..Setup::default()
    })
    .await;
    let project = t.workspace("proj").await;
    let session = t
        .open(AssistantScope::Workspace, Some(&project), false)
        .await
        .unwrap();
    assert_eq!(session.chat.as_ref().unwrap().agent, "codex");
    t.client
        .chat_send_with(
            session.id,
            "what do my notes say?",
            vec![Attachment::File {
                path: "notes.txt".into(),
                range: None,
                picked: false,
            }],
        )
        .await
        .unwrap();
    t.wait_turn_end().await;
    let turn = &t.received_of("codex", "turn/start")[0]["params"]["input"];
    let text = turn[0]["text"].as_str().unwrap();
    assert!(text.starts_with("what do my notes say?"), "{text}");
    assert!(text.contains("--- attached context"), "{text}");
    assert!(text.contains("remember the milk"), "{text}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plain_codex_chat_takes_files_and_images() {
    // No assistant backend at all: this is an ordinary Codex chat.
    let t = B::start(Setup::default()).await;
    let project = t.workspace("proj").await;
    let plain = t
        .client
        .call::<SessionResult>(Request::ChatCreate(hutan_protocol::chat::ChatCreate {
            workspace_id: Some(project.id),
            cwd: t.project.clone(),
            agent: "codex".into(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        }))
        .await
        .unwrap();
    assert!(!plain.session.chat.as_ref().unwrap().assistant);
    let mut t = t;
    t.client
        .chat_send_with(
            plain.session.id,
            "what is this?",
            vec![
                Attachment::File {
                    path: "notes.txt".into(),
                    range: None,
                    picked: false,
                },
                Attachment::Image {
                    path: None,
                    data_base64: Some("iVBORw0KGgp4eXo=".into()),
                    media_type: "image/png".into(),
                },
            ],
        )
        .await
        .unwrap();
    t.wait_turn_end().await;
    let input = &t.received_of("codex", "turn/start")[0]["params"]["input"];
    let text = input[0]["text"].as_str().unwrap();
    assert!(text.contains("remember the milk"), "{text}");
    assert!(text.contains("[image: pasted image · image/png"), "{text}");
    // The clipboard image is a temporary file for the agent, gone when the turn ended.
    assert_eq!(input[1]["type"], "localImage");
    let temporary = std::path::PathBuf::from(input[1]["path"].as_str().unwrap());
    assert_eq!(temporary.parent().unwrap().file_name().unwrap(), "tmp");
    assert!(!temporary.exists(), "removed when the turn ended");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_in_a_workspace_follows_the_workspace_choice_unless_it_has_its_own() {
    let t = B::start(Setup::default()).await;
    // `workspace.create` makes a workspace whose member project is the folder's project.
    let shop = t.workspace("shop").await;
    let project = t
        .client
        .call::<WorkspaceList>(Request::WorkspaceList(hutan_protocol::messages::Empty {}))
        .await
        .unwrap()
        .workspaces
        .into_iter()
        .find(|w| w.parent_id == Some(shop.id))
        .expect("the workspace's member project");
    t.configure(
        AssistantScope::Workspace,
        Some(&shop),
        AssistantBackend::Claude,
        None,
        None,
    )
    .await
    .unwrap();
    let inherited = t
        .open(AssistantScope::Workspace, Some(&project), false)
        .await
        .unwrap();
    assert_eq!(inherited.chat.as_ref().unwrap().agent, "claude");
    assert_eq!(
        inherited.workspace_id, project.id,
        "the assistant lives in the project"
    );
    // Its own choice wins.
    t.configure(
        AssistantScope::Workspace,
        Some(&project),
        AssistantBackend::Omp,
        None,
        None,
    )
    .await
    .unwrap();
    let own = t
        .open(AssistantScope::Workspace, Some(&project), false)
        .await
        .unwrap();
    assert_eq!(own.chat.as_ref().unwrap().agent, "omp");
    assert_ne!(own.id, inherited.id);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_harness_that_cannot_start_fails_its_first_message_and_another_backend_still_works() {
    let t = B::start(Setup {
        assistant: OMP_ASSISTANT,
        missing_omp: true,
        ..Setup::default()
    })
    .await;
    // Opening starts nothing, so it cannot fail on a harness that is not there…
    let omp = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert_eq!(omp.state, SessionState::Idle);
    assert_eq!(omp.chat.as_ref().unwrap().agent, "omp");
    // …the first message does, with the harness's error, and the session stays as it was.
    let error = t.client.chat_send(omp.id, "hello").await.unwrap_err();
    assert_eq!(remote_code(Err::<(), _>(error)), ErrorCode::ProviderError);
    assert_eq!(t.session(omp.id).await.state, SessionState::Idle);
    // Another backend still works next to it; the one that could not start is not its default.
    t.configure(
        AssistantScope::Global,
        None,
        AssistantBackend::Claude,
        None,
        None,
    )
    .await
    .unwrap();
    let ok = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert_eq!(ok.chat.as_ref().unwrap().agent, "claude");
    let listed = t.list().await.assistants;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].session.id, omp.id);
    assert!(!listed[0].default && listed[1].default);
    // Choosing OMP again makes that session the default again, and sending tries again.
    t.configure(
        AssistantScope::Global,
        None,
        AssistantBackend::Omp,
        None,
        None,
    )
    .await
    .unwrap();
    let again = t.open(AssistantScope::Global, None, false).await.unwrap();
    assert_eq!(again.id, omp.id);
    let error = t.client.chat_send(omp.id, "again").await.unwrap_err();
    assert_eq!(remote_code(Err::<(), _>(error)), ErrorCode::ProviderError);
    assert_eq!(t.session(omp.id).await.state, SessionState::Idle);
    t.stop().await;
}
