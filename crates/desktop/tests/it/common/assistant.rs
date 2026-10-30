//! The assistant in GPUI tests (M16-B): an in-process daemon with the in-memory credential store,
//! a provider `mock` that points at a loopback OpenAI-compatible server, and `daemon.toml`
//! naming it as the assistants' provider. Nothing touches the login keychain, a real provider
//! or a real agent transcript.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::workbench::Workbench;
use hutan_desktop::workbench::chat::{ChatModel, ChatView};
use hutan_desktop::workbench::panels::AssistantPanel;
use hutan_gateway::testing::{MockServer, Reply};
use hutan_gateway::{CredentialStore, HttpConfig, MemoryStore};
use hutan_protocol::messages::Request;
use hutan_protocol::provider::{ProviderAdd, ProviderResult, ProviderSetCredential};
use hutan_settings::{SettingsPaths, SettingsStore};

use super::history::HistoryEnv;
use super::{TestDaemon, WAIT, draw, open_with, wait_until};

/// Where the mock provider answers chat completions.
pub const PATH: &str = "/v1/chat/completions";

/// What an [`AssistantEnv`] daemon is told in `daemon.toml`.
#[derive(Default)]
pub struct Options<'a> {
    /// Name the `mock` provider under `[assistant]` (the assistants' provider and model).
    pub configured: bool,
    /// More lines of the `[assistant]` table (`context_budget_chars = 500`).
    pub assistant: &'a str,
    /// More tables (`[assistant.context]` …), written after `[assistant]`.
    pub tables: &'a str,
    /// Index the synthetic transcripts of this environment.
    pub history: Option<&'a HistoryEnv>,
    /// The `codex app-server` of the daemon (a replay of a recorded transcript): a Codex chat
    /// next to the assistants.
    pub codex: Option<hutan_agents::CodexConfig>,
    /// A change of the daemon's configuration (its timings, a fake poster).
    pub tweak: Option<fn(&mut hutan_daemon::DaemonConfig)>,
}

pub struct AssistantEnv {
    pub daemon: TestDaemon,
    pub mock: MockServer,
    /// A scratch directory, canonical: project folders live in it.
    pub base: PathBuf,
    _tmp: tempfile::TempDir,
}

impl AssistantEnv {
    pub fn start(options: Options<'_>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let mut toml = String::new();
        if options.configured || !options.assistant.is_empty() {
            toml.push_str("[assistant]\n");
            if options.configured {
                toml.push_str("provider = \"mock\"\nmodel = \"m-1\"\n");
            }
            toml.push_str(options.assistant);
            toml.push('\n');
        }
        toml.push_str(options.tables);
        toml.push('\n');
        if let Some(history) = options.history {
            toml.push_str(&history.daemon_toml(true));
        }
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
        let daemon = TestDaemon::start_configured(
            Some(&toml),
            options.codex,
            hutan_daemon::GatewayConfig {
                credentials: Some(store),
                http: HttpConfig {
                    no_proxy: true,
                    ..HttpConfig::default()
                },
                discovery: Some(Vec::new()),
            },
            None,
            |config| {
                if let Some(tweak) = options.tweak {
                    tweak(config);
                }
            },
        );
        let mock = daemon.http_mock();
        let _: ProviderResult = daemon.call(Request::ProviderAdd(ProviderAdd {
            kind: "openai-compatible".into(),
            name: "mock".into(),
            base_url: Some(format!("{}/v1", mock.url())),
            default_model: Some("m-1".into()),
        }));
        let _: ProviderResult =
            daemon.call(Request::ProviderSetCredential(ProviderSetCredential {
                provider_id: "mock".into(),
                secret: "mock-key".into(),
            }));
        Self {
            daemon,
            mock,
            base,
            _tmp: tmp,
        }
    }

    /// A project folder `name` below the scratch directory.
    pub fn project(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        fs::create_dir_all(root.join("src")).unwrap();
        root
    }
}

/// An SSE answer of one chunk.
pub fn reply(text: &str) -> Reply {
    Reply::sse([
        format!(
            "data: {}\n\n",
            serde_json::json!({"choices":[{"index":0,"delta":{"role":"assistant","content": text}}]})
        ),
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".into(),
        "data: [DONE]\n\n".into(),
    ])
}

/// Opens a workbench on `daemon` and waits for the first synchronisation. Home opens the Global
/// chat (`assistant.home_scope = "global"`, the default).
pub fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_settings(
        cx,
        daemon,
        super::with_assistant(SettingsStore::new(SettingsPaths::in_dir(daemon.home()))),
    )
}

/// Like [`open`], with `assistant.home_scope = "follow_workspace"`: Home follows the active
/// workspace and the header has the scope dropdown, as before ADR 0025.
pub fn open_following<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let settings = super::with_assistant(SettingsStore::new(SettingsPaths::in_dir(daemon.home())));
    settings
        .write_user(
            "assistant.home_scope",
            toml::Value::String("follow_workspace".to_owned()),
        )
        .expect("follow the workspace");
    open_settings(cx, daemon, settings)
}

/// Like [`open`] on a settings store the test keeps (clones share state).
pub fn open_settings<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    settings: SettingsStore,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, settings, daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

/// Like [`wait_until`], but the GPUI clock moves too (debounce timers are fake time).
pub fn pump_until(
    cx: &mut VisualTestContext,
    what: &str,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    wait_until(cx, what, |cx| {
        cx.executor().advance_clock(Duration::from_millis(100));
        condition(cx)
    });
}

/// The Assistant panel, once it has been drawn.
pub fn panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<AssistantPanel> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<AssistantPanel>("assistant"))
        .expect("the Assistant panel is drawn")
}

/// Opens the Assistant, then waits until the shown scope has its assistant.
pub fn open_assistant(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
) -> Entity<AssistantPanel> {
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    let panel = panel(view, cx);
    wait_until(cx, "the assistant to open", |cx| {
        panel.read_with(cx, |p, cx| p.session(cx).is_some())
    });
    panel
}

/// The session, model and view the panel shows.
pub fn shown(
    panel: &Entity<AssistantPanel>,
    cx: &mut VisualTestContext,
) -> (SessionId, Entity<ChatModel>, Entity<ChatView>) {
    panel.read_with(cx, |p, cx| {
        let session = p.session(cx).expect("an assistant is shown");
        (
            session,
            p.chat_model(session).expect("its model"),
            p.chat_view(session).expect("its view"),
        )
    })
}

/// The `(user, assistant)` texts of a transcript, in order.
pub fn texts(model: &Entity<ChatModel>, cx: &VisualTestContext) -> Vec<(bool, String)> {
    use hutan_chat_view::Body;
    model.read_with(cx, |m, _| {
        m.transcript()
            .entries()
            .iter()
            .filter_map(|e| match &e.body {
                Body::User(msg) => Some((true, msg.text.clone())),
                Body::Assistant(msg) => Some((false, msg.text.clone())),
                _ => None,
            })
            .collect()
    })
}

/// Waits until the transcript has a finished assistant message with `text`.
pub fn wait_answer(model: &Entity<ChatModel>, cx: &mut VisualTestContext, text: &str) {
    use hutan_chat_view::Body;
    wait_until(cx, &format!("the answer `{text}`"), |cx| {
        model.read_with(cx, |m, _| {
            m.transcript()
                .entries()
                .iter()
                .any(|e| matches!(&e.body, Body::Assistant(a) if a.text == text && a.done))
                && !m.transcript().turn_active()
        })
    });
}

/// The user message the provider received last (what the daemon assembled).
pub fn last_prompt(mock: &MockServer) -> String {
    let requests = mock.requests_to(PATH);
    let last = requests.last().expect("the provider got a request").json();
    last["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .expect("a user message")
        .to_owned()
}

/// Polls `check` on this thread (the daemon runs on its own runtime) until it holds.
pub fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}
