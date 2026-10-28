//! Chats through ACP harnesses (`omp`, `claude`) and accounts, end to end: a real daemon on a
//! temporary `HUTAN_HOME`, real client connections, `hutan-acp-fixture replay` standing in for
//! `omp acp`, and small shell scripts standing in for the harnesses' own CLIs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, remote_code, stop, wait_event};
use hutan_client::Client;
use hutan_core::agent::{MessageRole, PermissionOutcome};
use hutan_core::{
    AgentEvent, AgentEventKind, ApprovalPolicy, ForkMode, ForkSupport, Seq, SessionKind,
    SessionState, TaskRun, TaskState, TurnId,
};
use hutan_protocol::ErrorCode;
use hutan_protocol::account::AccountStatus;
use hutan_protocol::assistant::Attachment;
use hutan_protocol::chat::{
    ApprovalInfo, ApprovalList, ApprovalListRequest, ApprovalRespond, ChatCreate, ChatFork,
    ChatForked, ChatHistory, ChatHistoryRequest, ChatSent, ForkWhere,
};
use hutan_protocol::messages::{
    Empty, Event, Request, SessionInfo, SessionList, SessionListFilter, SessionRef, SessionResult,
    WorkspaceCreate, WorkspaceInfo,
};
use hutan_protocol::task::TaskCreate;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

const SESSION: &str = "sess_fixture_1";
const OPUS: &str = "gitlab-duo/duo-chat-opus-4-6";
const HAIKU: &str = "gitlab-duo/claude-haiku-4-5-20251001";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/acp-omp-18.11")
}

/// The fake ACP agent, next to the test binaries; `just test` builds the workspace binaries
/// first.
fn acp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(
        bin.is_file(),
        "hutan-acp-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

fn codex_bin() -> PathBuf {
    acp_bin().with_file_name("hutan-codex-fixture")
}

/// `[harness.<id>]` pointing at the fake agent replaying `transcript` (a file, or a directory
/// of transcripts used one per spawn), and daemon-mode environments (no login shell is run).
fn harness_toml(id: &str, transcript: &Path, received: &Path) -> String {
    format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.{id}]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\n",
        bin = acp_bin(),
    )
}

fn fixture(name: &str) -> PathBuf {
    fixtures().join(format!("{name}.jsonl"))
}

/// A directory of transcripts for a daemon that will spawn the agent several times.
fn script(dir: &Path, names: &[&str]) -> PathBuf {
    let scripted = dir.join("script");
    std::fs::create_dir_all(&scripted).unwrap();
    for (index, name) in names.iter().enumerate() {
        std::fs::copy(
            fixture(name),
            scripted.join(format!("{:02}-{name}.jsonl", index + 1)),
        )
        .unwrap();
    }
    scripted
}

/// A fake harness CLI: a shell script that lives at a stable path named by its content, under
/// the system temp directory, and is written once. macOS vets every new executable file before
/// its first run, which takes seconds when many tests start at once; scripts of fixed
/// content/inode are vetted once, not once per test.
fn executable(_dir: &Path, name: &str, body: &str) -> PathBuf {
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
    let staging = dir.join(format!(".{}-{}", std::process::id(), name));
    std::fs::write(&staging, &text).unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&staging, &path).unwrap();
    path
}

fn received_in(path: &Path) -> Vec<(String, Value)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let wrapper: Value = serde_json::from_str(line).unwrap();
            let msg = wrapper["msg"].clone();
            (msg["method"].as_str().unwrap_or("response").to_owned(), msg)
        })
        .collect()
}

struct H {
    daemon: TestDaemon,
    /// The agent's working directory; also holds the scripted transcripts and the received log.
    work: tempfile::TempDir,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
}

impl H {
    /// A daemon whose `omp` replays `transcript`.
    async fn omp(transcript: &Path) -> Self {
        Self::with(
            |work| harness_toml("omp", transcript, &work.join("received.jsonl")),
            |_| {},
        )
        .await
    }

    async fn with(
        toml: impl FnOnce(&Path) -> String,
        tweak: impl FnOnce(&mut hutan_daemon::DaemonConfig),
    ) -> Self {
        let work = tempfile::tempdir().unwrap();
        let toml = toml(work.path());
        let daemon = TestDaemon::start_with(&toml, tweak).await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    fn received(&self) -> Vec<(String, Value)> {
        received_in(&self.work.path().join("received.jsonl"))
    }

    fn received_of(&self, method: &str) -> Vec<Value> {
        self.received()
            .into_iter()
            .filter(|(name, _)| name == method)
            .map(|(_, msg)| msg)
            .collect()
    }

    fn request(&self, agent: &str) -> ChatCreate {
        ChatCreate {
            contain: None,
            workspace_id: None,
            cwd: self.work.path().to_owned(),
            agent: agent.to_owned(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        }
    }

    async fn try_create(&self, req: ChatCreate) -> Result<SessionInfo, hutan_client::ClientError> {
        self.client
            .call::<SessionResult>(Request::ChatCreate(req))
            .await
            .map(|r| r.session)
    }

    async fn create(&self, agent: &str) -> SessionInfo {
        self.try_create(self.request(agent)).await.unwrap()
    }

    async fn send(&self, id: hutan_core::SessionId, text: &str) -> ChatSent {
        self.client.chat_send(id, text).await.unwrap()
    }

    async fn history(&self, id: hutan_core::SessionId) -> ChatHistory {
        self.client
            .call(Request::ChatHistory(ChatHistoryRequest {
                session_id: id,
                before_seq: None,
                limit: 500,
            }))
            .await
            .unwrap()
    }

    async fn session(&self, id: hutan_core::SessionId) -> SessionInfo {
        self.client
            .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap()
            .sessions
            .into_iter()
            .find(|s| s.id == id)
            .expect("session listed")
    }

    async fn wait_state(&self, id: hutan_core::SessionId, state: SessionState) -> SessionInfo {
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

    async fn approvals(&self) -> Vec<ApprovalInfo> {
        self.client
            .call::<ApprovalList>(Request::ApprovalList(ApprovalListRequest {
                session_id: None,
            }))
            .await
            .unwrap()
            .approvals
    }

    async fn respond(
        &self,
        approval: &ApprovalInfo,
        option: &str,
    ) -> Result<Empty, hutan_client::ClientError> {
        self.client
            .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
                approval_id: approval.id,
                option_id: option.to_owned(),
            }))
            .await
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

async fn chat_event(
    events: &mut broadcast::Receiver<(Seq, Event)>,
    what: &str,
    mut pick: impl FnMut(&AgentEvent) -> bool,
) -> AgentEvent {
    wait_event(events, what, |event| match event {
        Event::ChatEvent { event, .. } if pick(event) => Some((**event).clone()),
        _ => None,
    })
    .await
}

async fn required(events: &mut broadcast::Receiver<(Seq, Event)>) -> ApprovalInfo {
    wait_event(events, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some((**approval).clone()),
        _ => None,
    })
    .await
}

fn answers(history: &ChatHistory) -> Vec<&str> {
    history
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::MessageEnd {
                role: MessageRole::Assistant,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

// Chats -----------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_omp_chat_streams_journals_and_returns_the_session_to_idle() {
    let mut t = H::omp(&fixture("plain-turn")).await;
    assert!(t.client.supports_harness_chat() && t.client.supports_accounts());
    let created = t.create("omp").await;
    assert_eq!(created.kind, SessionKind::Harness);
    assert_eq!(created.state, SessionState::Idle);
    assert_eq!(created.terminal_id, None);
    assert_eq!(
        created.title,
        format!(
            "omp · {}",
            t.work.path().file_name().unwrap().to_string_lossy()
        )
    );
    let chat = created.chat.clone().unwrap();
    assert_eq!(chat.agent, "omp");
    assert_eq!(chat.model, OPUS, "the model the agent starts with");
    assert_eq!(
        chat.agent_session_id.as_deref(),
        Some(SESSION),
        "what `omp --resume` continues"
    );
    assert!(!chat.turn_active);

    let sent = t.send(created.id, "Say hello.").await;
    let started = chat_event(&mut t.events, "turn_started", |e| {
        matches!(e.kind, AgentEventKind::TurnStarted)
    })
    .await;
    assert_eq!(started.turn_id, Some(sent.turn_id));
    let mut text = String::new();
    loop {
        let event = chat_event(&mut t.events, "the turn to end", |_| true).await;
        if let AgentEventKind::MessageDelta { text: delta } = &event.kind {
            text.push_str(delta);
        }
        if event.kind.ends_turn() {
            assert!(matches!(event.kind, AgentEventKind::TurnCompleted { .. }));
            break;
        }
    }
    assert_eq!(text, "Hello, world.");
    let idle = t.wait_state(created.id, SessionState::Idle).await;
    assert!(!idle.chat.unwrap().turn_active);

    let history = t.history(created.id).await;
    let kinds: Vec<_> = history.events.iter().map(|e| e.kind.name()).collect();
    for expected in [
        "resume_locator",
        "turn_started",
        "reasoning_end",
        "message_end",
        "usage",
        "turn_completed",
    ] {
        assert!(kinds.contains(&expected), "{expected} in {kinds:?}");
    }
    assert!(!kinds.contains(&"message_delta"), "{kinds:?}");
    assert_eq!(answers(&history), ["Hello, world."]);
    // The agent tree has the harness as the main agent.
    let trees = t.client.agent_tree(Some(created.id)).await.unwrap();
    let root = &trees[0].nodes[&trees[0].root];
    assert_eq!((root.agent.as_str(), root.source.as_str()), ("omp", "acp"));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_are_validated_for_harness_chats() {
    let t = H::omp(&fixture("plain-turn")).await;
    // The harness decides what asks and what is sandboxed: Hutan does not pretend to.
    let mut req = t.request("omp");
    req.approval_policy = Some(ApprovalPolicy::Never);
    assert_eq!(remote_code(t.try_create(req).await), ErrorCode::BadRequest);
    let mut req = t.request("codex");
    req.mode = Some("plan".into());
    assert_eq!(remote_code(t.try_create(req).await), ErrorCode::BadRequest);
    let mut req = t.request("omp");
    req.model = Some("nobody/nothing".into());
    let error = t.try_create(req).await.unwrap_err().to_string();
    assert!(
        error.contains("nobody/nothing") && error.contains(HAIKU),
        "{error}"
    );
    assert_eq!(
        remote_code(t.try_create(t.request("gemini")).await),
        ErrorCode::BadRequest
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_harness_says_what_to_install() {
    let t = H::with(
        |work| {
            format!(
                "[terminal]\nenv_inherit = \"daemon\"\n[harness.claude]\nbin = {:?}\n",
                work.join("no-such-claude-code-acp")
            )
        },
        |_| {},
    )
    .await;
    let error = t
        .try_create(t.request("claude"))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no-such-claude-code-acp"), "{error}");
    assert!(
        error.contains("@agentclientprotocol/claude-agent-acp"),
        "{error}"
    );
    assert!(error.contains("[harness.claude]"), "{error}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn approvals_of_a_harness_chat_are_answered_with_the_options_the_agent_offered() {
    let mut t = H::omp(&fixture("permission-allow")).await;
    let created = t.create("omp").await;
    t.send(created.id, "edit lib.rs").await;
    let approval = required(&mut t.events).await;
    assert_eq!(approval.session_id, created.id);
    assert_eq!(approval.title, "edit: src/lib.rs");
    let offered: Vec<_> = approval.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(
        offered,
        ["allow_once", "allow_always", "reject_once", "reject_always"]
    );
    let waiting = t
        .wait_state(created.id, SessionState::WaitingApproval)
        .await;
    assert_eq!(waiting.chat.unwrap().pending_approvals, 1);
    assert_eq!(t.approvals().await, std::slice::from_ref(&approval));

    // Only an offered option reaches the agent.
    for bad in ["allow", "", "ALLOW_ONCE", "cancelled"] {
        let result = t.respond(&approval, bad).await;
        assert_eq!(remote_code(result), ErrorCode::OptionNotOffered, "{bad:?}");
    }
    assert!(t.received_of("response").is_empty());

    t.respond(&approval, "allow_once").await.unwrap();
    let (outcome, decided_by) =
        wait_event(&mut t.events, "approval.resolved", |event| match event {
            Event::ApprovalResolved {
                outcome,
                decided_by,
                ..
            } => Some((outcome.clone(), decided_by.clone())),
            _ => None,
        })
        .await;
    assert_eq!(
        outcome,
        PermissionOutcome::Selected {
            option_id: "allow_once".into()
        }
    );
    assert_eq!(decided_by.unwrap().client_kind, "cli");
    chat_event(&mut t.events, "the turn to end", |e| e.kind.ends_turn()).await;
    t.wait_state(created.id, SessionState::Idle).await;
    assert!(t.approvals().await.is_empty());
    assert_eq!(
        t.received_of("response")[0]["result"]["outcome"]["optionId"],
        "allow_once"
    );
    // The journal replays who decided, and the diff the agent made.
    let history = t.history(created.id).await;
    assert!(history.events.iter().any(|e| matches!(
        &e.kind,
        AgentEventKind::PermissionResolved { decided_by: Some(by), .. } if by.client_kind == "cli"
    )));
    assert!(
        history
            .events
            .iter()
            .any(|e| matches!(&e.kind, AgentEventKind::FileDiff { .. }))
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denied_request_is_recorded_and_the_chat_goes_on() {
    let mut t = H::omp(&fixture("permission-deny")).await;
    let created = t.create("omp").await;
    t.send(created.id, "edit lib.rs").await;
    let approval = required(&mut t.events).await;
    t.respond(&approval, "reject_once").await.unwrap();
    chat_event(&mut t.events, "the turn to end", |e| e.kind.ends_turn()).await;
    t.wait_state(created.id, SessionState::Idle).await;
    let history = t.history(created.id).await;
    assert_eq!(
        answers(&history),
        ["I will edit a file.", "Understood, I did not edit."]
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupting_expires_the_waiting_approval_and_cancels_the_turn() {
    let mut t = H::omp(&fixture("permission-cancel")).await;
    let created = t.create("omp").await;
    t.send(created.id, "edit lib.rs").await;
    let approval = required(&mut t.events).await;
    t.client.chat_interrupt(created.id).await.unwrap();
    let (id, outcome) = wait_event(&mut t.events, "approval.resolved", |event| match event {
        Event::ApprovalResolved {
            approval_id,
            outcome,
            ..
        } => Some((*approval_id, outcome.clone())),
        _ => None,
    })
    .await;
    assert_eq!((id, outcome), (approval.id, PermissionOutcome::Expired));
    let end = chat_event(&mut t.events, "the turn to end", |e| e.kind.ends_turn()).await;
    assert!(matches!(end.kind, AgentEventKind::TurnCancelled));
    t.wait_state(created.id, SessionState::Idle).await;
    assert!(t.approvals().await.is_empty());
    // Nothing is left to interrupt.
    assert_eq!(
        remote_code(t.client.chat_interrupt(created.id).await),
        ErrorCode::InvalidState
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_approval_expires_when_the_agent_ends_the_turn_without_it() {
    let mut t = H::omp(&fixture("permission-expires")).await;
    let created = t.create("omp").await;
    t.send(created.id, "edit lib.rs").await;
    let approval = required(&mut t.events).await;
    let outcome = wait_event(&mut t.events, "approval.resolved", |event| match event {
        Event::ApprovalResolved {
            approval_id,
            outcome,
            ..
        } if *approval_id == approval.id => Some(outcome.clone()),
        _ => None,
    })
    .await;
    assert_eq!(outcome, PermissionOutcome::Expired);
    t.wait_state(created.id, SessionState::Idle).await;
    let late = t.respond(&approval, "allow_once").await;
    assert_eq!(remote_code(late), ErrorCode::NotFound);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_message_during_a_turn_is_refused_and_an_interrupt_ends_it() {
    let mut t = H::omp(&fixture("cancel")).await;
    let created = t.create("omp").await;
    t.send(created.id, "long task").await;
    let again = t.client.chat_send(created.id, "again").await;
    assert_eq!(remote_code(again), ErrorCode::InvalidState);
    chat_event(&mut t.events, "text", |e| {
        matches!(e.kind, AgentEventKind::MessageDelta { .. })
    })
    .await;
    t.client.chat_interrupt(created.id).await.unwrap();
    let end = chat_event(&mut t.events, "the turn to end", |e| e.kind.ends_turn()).await;
    assert!(matches!(end.kind, AgentEventKind::TurnCancelled));
    t.wait_state(created.id, SessionState::Idle).await;
    let history = t.history(created.id).await;
    assert_eq!(
        answers(&history),
        ["Working on it"],
        "the text so far is kept"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_dies_mid_turn_fails_the_session_and_ends_the_turn() {
    let mut t = H::omp(&fixture("exit-mid-turn")).await;
    let created = t.create("omp").await;
    t.send(created.id, "go").await;
    let end = chat_event(&mut t.events, "the turn to end", |e| e.kind.ends_turn()).await;
    assert!(
        matches!(end.kind, AgentEventKind::TurnFailed { .. }),
        "{:?}",
        end.kind
    );
    let failed = timeout(WAIT, async {
        loop {
            let session = t.session(created.id).await;
            if matches!(session.state, SessionState::Failed { .. }) {
                return session;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(failed.state, SessionState::Failed { .. }));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mode_is_applied_at_creation_and_an_unknown_one_is_refused() {
    let t = H::omp(&fixture("mode-plan")).await;
    let mut req = t.request("omp");
    req.mode = Some("plan".into());
    t.try_create(req).await.unwrap();
    assert_eq!(
        t.received_of("session/set_mode")[0]["params"]["modeId"],
        "plan"
    );
    t.stop().await;

    let t = H::omp(&fixture("plain-turn")).await;
    let mut req = t.request("omp");
    req.mode = Some("yolo".into());
    let error = t.try_create(req).await.unwrap_err();
    assert!(error.to_string().contains("default, plan"), "{error}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn authentication_failures_point_at_the_harnesses_own_login() {
    let t = H::omp(&fixture("auth-terminal-only")).await;
    let error = t
        .try_create(t.request("omp"))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("not logged in"), "{error}");
    assert!(error.contains("hutan account login omp"), "{error}");
    t.stop().await;
}

// Models ----------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn models_are_switched_on_a_running_harness_chat() {
    let t = H::omp(&fixture("model-switch")).await;
    let mut req = t.request("omp");
    req.model = Some(HAIKU.into());
    let created = t.try_create(req).await.unwrap();
    assert_eq!(created.chat.unwrap().model, HAIKU);

    let switched = t
        .client
        .chat_set_model(created.id, None, OPUS)
        .await
        .unwrap();
    assert_eq!(switched.chat.unwrap().model, OPUS);
    let set = t.received_of("session/set_config_option");
    assert_eq!(set.len(), 2);
    assert_eq!(set[1]["params"]["value"], OPUS);
    // The session record follows, and survives in the list.
    assert_eq!(t.session(created.id).await.chat.unwrap().model, OPUS);
    // The journal says so.
    let history = t.history(created.id).await;
    assert!(history.events.iter().any(|e| matches!(
        &e.kind,
        AgentEventKind::Notice { message, .. } if message == &format!("model: {OPUS}")
    )));
    // A model the agent does not offer changes nothing.
    let refused = t
        .client
        .chat_set_model(created.id, None, "nobody/nothing")
        .await;
    assert_eq!(remote_code(refused), ErrorCode::BadRequest);
    assert_eq!(t.session(created.id).await.chat.unwrap().model, OPUS);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn models_are_the_options_of_an_agent_session_and_omp_falls_back_to_its_own_list() {
    let omp_models = r#"{"models":[
        {"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","selector":"anthropic/claude-opus-4-5","name":"Claude Opus 4.5","thinking":["low","high"]},
        {"provider":"openai-codex","kind":"chat","id":"gpt-5","selector":"openai-codex/gpt-5","name":"GPT-5"}
    ]}"#;
    let transcript = fixture("plain-turn");
    let t = H::with(
        |work| {
            let script = executable(
                work,
                "omp",
                &format!("case \"$1\" in models) cat <<'EOF'\n{omp_models}\nEOF\n;; esac"),
            );
            format!(
                "[terminal]\nenv_inherit = \"daemon\"\n[harness.omp]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\ncli = {script:?}\n[harness.claude]\nbin = {bin:?}\nargs = [\"replay\", {transcript:?}, \"acp\"]\n",
                bin = acp_bin(),
                received = work.join("received.jsonl"),
            )
        },
        |_| {},
    )
    .await;
    // The agent's own session says what it offers (OMP too).
    let omp = t.client.chat_models("omp").await.unwrap();
    assert_eq!(
        omp.models.len(),
        17,
        "the models of the agent's own session"
    );
    assert!(omp.models.iter().any(|m| m.id == HAIKU));
    assert_eq!(
        omp.models
            .iter()
            .filter(|m| m.is_default)
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        [OPUS]
    );
    // Cached for five minutes: no second session.
    t.client.chat_models("omp").await.unwrap();
    assert_eq!(t.received_of("session/new").len(), 1);
    let claude = t.client.chat_models("claude").await.unwrap();
    assert_eq!(claude.models.len(), 17);
    assert_eq!(
        remote_code(t.client.chat_models("gemini").await),
        ErrorCode::BadRequest
    );
    t.stop().await;

    // An OMP whose ACP session cannot be had still lists its models from `omp models --json`.
    let t = H::with(
        |work| {
            let script = executable(
                work,
                "omp",
                &format!("case \"$1\" in models) cat <<'EOF'\n{omp_models}\nEOF\n;; esac"),
            );
            format!(
                "[terminal]\nenv_inherit = \"daemon\"\n[harness.omp]\nbin = {bin:?}\nargs = [\"replay\", {transcript:?}, \"acp\"]\ncli = {script:?}\n",
                bin = acp_bin(),
                transcript = fixture("auth-terminal-only"),
            )
        },
        |_| {},
    )
    .await;
    let omp = t.client.chat_models("omp").await.unwrap();
    let ids: Vec<_> = omp.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["anthropic/claude-opus-4-5", "openai-codex/gpt-5"]);
    assert_eq!(omp.models[0].reasoning_efforts, ["low", "high"]);
    // Claude has no such fallback: the reason is the answer.
    let error = t
        .client
        .chat_models("claude")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("claude-code-acp"), "{error}");
    t.stop().await;
}

// Restore ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_restart_continues_the_session_with_session_load_and_keeps_the_history() {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["plain-turn", "load"]);
    let toml = harness_toml("omp", &scripted, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with(&toml, |_| {}).await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let t = H {
        daemon,
        work,
        client,
        events: events.resubscribe(),
    };
    let created = t.create("omp").await;
    t.send(created.id, "Say hello.").await;
    chat_event(&mut events, "the turn to end", |e| e.kind.ends_turn()).await;
    let before = t.history(created.id).await;
    assert_eq!(answers(&before), ["Hello, world."]);

    // The daemon restarts: the agent comes back through `session/load` of the stored session.
    let H { daemon, work, .. } = t;
    let daemon = daemon.restart().await;
    let (client, mut events) = daemon.connect_subscribed().await;
    let t = H {
        daemon,
        work,
        client,
        events: events.resubscribe(),
    };
    let restored = t.wait_state(created.id, SessionState::Idle).await;
    assert_eq!(restored.id, created.id, "the same session");
    assert!(!restored.restored, "the journal has the whole conversation");
    let recovery = restored.recovery.clone().expect("the restart is reported");
    assert_eq!(
        recovery.kind,
        hutan_protocol::recovery::RecoveryKind::ChatResumed {
            thread_id: Some(SESSION.to_owned()),
            last_turn: hutan_protocol::recovery::LastTurn::Completed,
        }
    );
    let chat = restored.chat.unwrap();
    assert_eq!(chat.agent_session_id.as_deref(), Some(SESSION));
    let loads = t.received_of("session/load");
    assert_eq!(loads.len(), 1, "{:?}", t.received());
    assert_eq!(loads[0]["params"]["sessionId"], SESSION);
    assert_eq!(
        t.received_of("session/new").len(),
        1,
        "only the first run made a session"
    );

    // The replay of the agent's own history stays out: the journal is unchanged.
    let after = t.history(created.id).await;
    assert_eq!(&after.events[..before.events.len()], &before.events[..]);
    assert!(
        !after
            .events
            .iter()
            .skip(before.events.len())
            .any(|e| matches!(
                &e.kind,
                AgentEventKind::MessageEnd { text, .. } if text == "earlier answer"
            ))
    );
    t.send(created.id, "again").await;
    chat_event(&mut events, "the resumed turn to end", |e| {
        e.kind.ends_turn()
    })
    .await;
    let all = t.history(created.id).await;
    assert_eq!(answers(&all), ["Hello, world.", "Welcome back."]);
    assert!(
        all.events.windows(2).all(|w| w[0].seq < w[1].seq),
        "seq never goes back"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_harness_chat_ends_its_agent_and_restarting_continues_it() {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &["plain-turn", "resume-only"]);
    let toml = harness_toml("omp", &scripted, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with(&toml, |_| {}).await;
    let (client, events) = daemon.connect_subscribed().await;
    let t = H {
        daemon,
        work,
        client,
        events,
    };
    let created = t.create("omp").await;
    let stopped = stop(&t.client, created.id).await.unwrap();
    assert_eq!(stopped.state, SessionState::Stopped);
    assert_eq!(
        remote_code(t.client.chat_send(created.id, "hi").await),
        ErrorCode::InvalidState
    );
    t.client
        .call::<SessionResult>(Request::SessionRestart(SessionRef {
            session_id: created.id,
        }))
        .await
        .unwrap();
    t.wait_state(created.id, SessionState::Idle).await;
    // The second agent offers no `session/load`: it is continued with `session/resume`.
    assert_eq!(
        t.received_of("session/resume")[0]["params"]["sessionId"],
        SESSION
    );
    t.stop().await;
}

// Attachments -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn attached_context_reaches_a_harness_as_embedded_resources() {
    let mut t = H::omp(&fixture("plain-turn")).await;
    let root = t.work.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.txt"), "remember the milk\n").unwrap();
    let project: WorkspaceInfo = t
        .client
        .call::<WorkspaceInfo>(Request::WorkspaceCreate(WorkspaceCreate {
            name: "proj".into(),
            paths: vec![root.clone()],
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap();
    let mut req = t.request("omp");
    req.cwd = std::fs::canonicalize(&root).unwrap();
    req.workspace_id = Some(project.id);
    let created = t.try_create(req).await.unwrap();
    t.client
        .chat_send_with(
            created.id,
            "what do my notes say?",
            vec![Attachment::File {
                path: "notes.txt".into(),
                range: None,
                picked: false,
            }],
        )
        .await
        .unwrap();
    chat_event(&mut t.events, "the turn to end", |e| e.kind.ends_turn()).await;
    let prompt = &t.received_of("session/prompt")[0]["params"]["prompt"];
    assert_eq!(prompt[0]["text"], "what do my notes say?");
    assert_eq!(prompt[1]["type"], "resource");
    assert!(
        prompt[1]["resource"]["text"]
            .as_str()
            .unwrap()
            .contains("remember the milk")
    );
    // The journal's user message is what the agent was given.
    let history = t.history(created.id).await;
    assert!(history.events.iter().any(|e| matches!(
        &e.kind,
        AgentEventKind::MessageEnd { role: MessageRole::User, text, .. }
            if text.contains("what do my notes say?") && text.contains("remember the milk")
    )));
    // A Codex chat still takes none.
    t.stop().await;
}

// Rich input (protocol 0.23) --------------------------------------------------------------------

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nxyz";

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn image_path(path: &Path) -> Attachment {
    Attachment::Image {
        path: Some(path.display().to_string()),
        data_base64: None,
        media_type: "image/png".into(),
    }
}

/// `UX-MODE-CHIP` (protocol 0.34): `ChatInfo.mode` is the mode the harness reports at the start
/// (OMP's `mode` config option) and follows its updates; Hutan never asks for a mode itself.
#[tokio::test(flavor = "multi_thread")]
async fn the_mode_a_harness_reports_is_the_chats_mode_and_follows_its_updates() {
    let mut t = H::omp(&fixture("unknown-update")).await;
    assert!(t.client.supports_chat_mode());
    let created = t.create("omp").await;
    let mode = created.chat.as_ref().unwrap().mode.clone().unwrap();
    assert_eq!((mode.id.as_str(), mode.label()), ("default", "Default"));
    assert!(!mode.approves_everything);
    t.send(created.id, "go").await;
    let changed = wait_event(&mut t.events, "the mode update", |event| match event {
        Event::SessionUpdated { session } if session.id == created.id => session
            .chat
            .as_ref()
            .and_then(|chat| chat.mode.clone())
            .filter(|mode| mode.id == "plan"),
        _ => None,
    })
    .await;
    assert_eq!(changed.label(), "Plan");
    assert!(
        t.received_of("session/set_mode").is_empty()
            && t.received_of("session/set_config_option").is_empty(),
        "Hutan never changes an ordinary chat's mode"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn images_and_pasted_text_reach_an_omp_chat_and_are_journaled_without_bytes() {
    let mut t = H::omp(&fixture("plain-turn")).await;
    let created = t.create("omp").await;
    assert_eq!(created.chat.as_ref().unwrap().images, Some(true));
    let shot = t.work.path().join("shot.png");
    std::fs::write(&shot, PNG).unwrap();
    t.client
        .chat_send_with(
            created.id,
            "what is this?",
            vec![
                image_path(&shot),
                Attachment::Image {
                    path: None,
                    data_base64: Some(b64(PNG)),
                    media_type: "image/png".into(),
                },
                Attachment::PastedText {
                    text: "line one\nline two".into(),
                },
            ],
        )
        .await
        .unwrap();
    chat_event(&mut t.events, "the turn to end", |e| e.kind.ends_turn()).await;
    let prompt = &t.received_of("session/prompt")[0]["params"]["prompt"];
    assert_eq!(prompt[0]["text"], "what is this?");
    assert_eq!(prompt[1]["type"], "resource");
    assert!(
        prompt[1]["resource"]["text"]
            .as_str()
            .unwrap()
            .contains("line two")
    );
    for image in [&prompt[2], &prompt[3]] {
        assert_eq!(image["type"], "image");
        assert_eq!(image["mimeType"], "image/png");
        assert_eq!(image["data"], b64(PNG));
    }
    // The journal names the images and keeps path and hash, never the bytes.
    let history = t.history(created.id).await;
    let user = history
        .events
        .iter()
        .find(|e| {
            matches!(
                &e.kind,
                AgentEventKind::MessageEnd {
                    role: MessageRole::User,
                    ..
                }
            )
        })
        .unwrap();
    let AgentEventKind::MessageEnd { text, .. } = &user.kind else {
        unreachable!()
    };
    assert!(text.contains("[image: shot.png · image/png"), "{text}");
    assert!(!text.contains(&b64(PNG)));
    let meta = hutan_protocol::assistant::attachments_meta(user);
    assert_eq!(meta.len(), 3);
    assert_eq!(meta[0].media_type.as_deref(), Some("image/png"));
    assert_eq!(meta[0].size, Some(PNG.len() as u64));
    assert_eq!(
        meta[0].path.as_deref(),
        Some(std::fs::canonicalize(&shot).unwrap().to_str().unwrap())
    );
    assert_eq!(meta[0].sha256.as_ref().map(String::len), Some(64));
    assert_eq!(meta[1].path, None);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_omp_without_image_support_refuses_images_as_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let text = std::fs::read_to_string(fixture("plain-turn")).unwrap();
    assert!(text.contains("\"image\":true"));
    let without = dir.path().join("no-images.jsonl");
    std::fs::write(&without, text.replace("\"image\":true", "\"image\":false")).unwrap();
    let t = H::omp(&without).await;
    let created = t.create("omp").await;
    assert_eq!(created.chat.as_ref().unwrap().images, Some(false));
    let error = t
        .client
        .chat_send_with(
            created.id,
            "look",
            vec![Attachment::Image {
                path: None,
                data_base64: Some(b64(PNG)),
                media_type: "image/png".into(),
            }],
        )
        .await;
    assert_eq!(remote_code(error), ErrorCode::Unsupported);
    assert!(t.received_of("session/prompt").is_empty());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_are_bounded_and_checked() {
    let t = H::omp(&fixture("plain-turn")).await;
    let created = t.create("omp").await;
    let send = |attachment: Attachment| {
        let client = t.client.clone();
        async move {
            remote_code(
                client
                    .chat_send_with(created.id, "x", vec![attachment])
                    .await,
            )
        }
    };
    // A folder, a file that is no image, an image over 5 MiB.
    assert_eq!(send(image_path(t.work.path())).await, ErrorCode::BadRequest);
    let text = t.work.path().join("notes.png");
    std::fs::write(&text, "not an image").unwrap();
    assert_eq!(send(image_path(&text)).await, ErrorCode::BadRequest);
    let big = t.work.path().join("big.png");
    let mut bytes = PNG.to_vec();
    bytes.resize(5 * 1024 * 1024 + 1, 0);
    std::fs::write(&big, &bytes).unwrap();
    assert_eq!(send(image_path(&big)).await, ErrorCode::BadRequest);
    // Inline data over the frame-sized limit, bad base64, and too much pasted text.
    let mut inline = PNG.to_vec();
    inline.resize(hutan_protocol::assistant::IMAGE_INLINE_MAX_BYTES + 1, 0);
    assert_eq!(
        send(Attachment::Image {
            path: None,
            data_base64: Some(b64(&inline)),
            media_type: "image/png".into()
        })
        .await,
        ErrorCode::BadRequest
    );
    assert_eq!(
        send(Attachment::Image {
            path: None,
            data_base64: Some("!!not base64!!".into()),
            media_type: "image/png".into()
        })
        .await,
        ErrorCode::BadRequest
    );
    assert_eq!(
        send(Attachment::PastedText {
            text: "x".repeat(hutan_protocol::assistant::PASTED_TEXT_MAX_BYTES + 1)
        })
        .await,
        ErrorCode::BadRequest
    );
    assert!(t.received_of("session/prompt").is_empty());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_may_not_attach_images_or_read_picked_paths_outside_the_roots() {
    let transcript = fixture("plain-turn");
    let mut t = H::with(
        |work| {
            format!(
                "[control]\nagents = \"allow\"\n{}",
                harness_toml("omp", &transcript, &work.join("received.jsonl"))
            )
        },
        |_| {},
    )
    .await;
    let created = t.create("omp").await;
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "outside the roots").unwrap();
    let shot = outside.path().join("shot.png");
    std::fs::write(&shot, PNG).unwrap();

    // A client running inside the chat's own session is an agent's.
    let mut options = t.daemon.options();
    options.client.origin = Some(hutan_protocol::messages::Origin {
        session_id: created.id,
    });
    let agent = Client::connect(options).await.unwrap();
    for attachment in [
        image_path(&shot),
        Attachment::PastedText { text: "hi".into() },
        Attachment::File {
            path: secret.display().to_string(),
            range: None,
            picked: true,
        },
    ] {
        let refused = agent
            .chat_send_with(created.id, "look", vec![attachment])
            .await;
        assert_eq!(remote_code(refused), ErrorCode::Forbidden);
    }
    assert!(t.received_of("session/prompt").is_empty());

    // The user's own client may: the same picked path is read.
    t.client
        .chat_send_with(
            created.id,
            "read it",
            vec![Attachment::File {
                path: secret.display().to_string(),
                range: None,
                picked: true,
            }],
        )
        .await
        .unwrap();
    chat_event(&mut t.events, "the turn to end", |e| e.kind.ends_turn()).await;
    let prompt = &t.received_of("session/prompt")[0]["params"]["prompt"];
    assert!(prompt.to_string().contains("outside the roots"));
    t.stop().await;
}

// Tasks -----------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tasks_run_as_omp_and_claude_chats() {
    let t = H::omp(&fixture("plain-turn")).await;
    let root = t.work.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    let project = t
        .client
        .call::<WorkspaceInfo>(Request::WorkspaceCreate(WorkspaceCreate {
            name: "tasks".into(),
            paths: vec![root],
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap();
    let create = |title: &str, agent: &str| TaskCreate {
        workspace_id: project.id,
        title: title.to_owned(),
        prompt: Some("Say hello.".into()),
        depends_on: Vec::new(),
        placement: None,
        run: Some(TaskRun::Chat {
            contain: None,
            agent: agent.to_owned(),
            model: None,
            account: None,
            safety: None,
        }),
        auto_start: false,
        after_policy: Default::default(),
    };
    // `claude` is accepted as an agent too; a made-up one is not.
    t.client
        .task_create(create("claude task", "claude"))
        .await
        .unwrap();
    assert!(
        t.client
            .task_create(create("gemini task", "gemini"))
            .await
            .is_err()
    );

    let task = t.client.task_create(create("hello", "omp")).await.unwrap();
    let started = t.client.task_start(task.id).await.unwrap();
    let session = t.session(started.session.id).await;
    assert_eq!(session.chat.as_ref().map(|c| c.agent.as_str()), Some("omp"));
    let done = timeout(WAIT, async {
        loop {
            let task = t
                .client
                .task_list(None, None)
                .await
                .unwrap()
                .into_iter()
                .find(|x| x.id == task.id)
                .unwrap();
            if task.state == TaskState::Done {
                return task;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the task finishes with its turn");
    assert_eq!(done.sessions, [started.session.id]);
    let history = t.history(started.session.id).await;
    assert_eq!(answers(&history), ["Hello, world."]);
    assert_eq!(
        t.received_of("session/prompt")[0]["params"]["prompt"][0]["text"],
        "Say hello."
    );
    t.stop().await;
}

/// A chat task starts its chat on the model it names (`task.chat_model`); one the harness does
/// not offer fails the start, naming the choices.
#[tokio::test(flavor = "multi_thread")]
async fn a_chat_task_starts_on_the_model_it_names() {
    let t = H::omp(&fixture("model-switch")).await;
    assert!(t.client.supports_task_chat_model());
    let root = t.work.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    let project = t
        .client
        .call::<WorkspaceInfo>(Request::WorkspaceCreate(WorkspaceCreate {
            name: "tasks".into(),
            paths: vec![root],
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap();
    let create = |title: &str, model: &str| TaskCreate {
        workspace_id: project.id,
        title: title.to_owned(),
        prompt: Some("Say hello.".into()),
        depends_on: Vec::new(),
        placement: None,
        run: Some(TaskRun::Chat {
            contain: None,
            agent: "omp".into(),
            model: Some(model.to_owned()),
            account: None,
            safety: None,
        }),
        auto_start: false,
        after_policy: Default::default(),
    };
    // An empty model is refused at creation.
    assert!(t.client.task_create(create("empty", " ")).await.is_err());

    // One the harness does not offer fails the start and names the choices.
    let bad = t
        .client
        .task_create(create("nobody", "nobody/nothing"))
        .await
        .unwrap();
    let error = t.client.task_start(bad.id).await.unwrap_err().to_string();
    assert!(
        error.contains("nobody/nothing") && error.contains(HAIKU),
        "{error}"
    );

    let task = t.client.task_create(create("haiku", HAIKU)).await.unwrap();
    assert_eq!(
        task.run,
        Some(TaskRun::Chat {
            contain: None,
            agent: "omp".into(),
            model: Some(HAIKU.into()),
            account: None,
            safety: None,
        })
    );
    let started = t.client.task_start(task.id).await.unwrap();
    let session = t.session(started.session.id).await;
    assert_eq!(session.chat.as_ref().map(|c| c.model.as_str()), Some(HAIKU));
    assert!(
        !t.received_of("session/set_config_option").is_empty(),
        "the harness was asked for the model"
    );
    t.stop().await;
}

// Accounts --------------------------------------------------------------------------------------

/// The fake harness CLIs of the accounts tests, and a Codex app server that knows `account/read`.
struct Accounts {
    t: H,
}

impl Accounts {
    async fn start(codex_account: &str) -> Self {
        let t = H::with(
            |work| {
                let omp = executable(
                    work,
                    "omp",
                    r#"case "$1" in
  models) cat <<'EOF'
{"models":[
 {"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","selector":"anthropic/claude-opus-4-5","name":"Claude Opus 4.5"},
 {"provider":"anthropic","kind":"chat","id":"claude-haiku-4-5","selector":"anthropic/claude-haiku-4-5","name":"Claude Haiku 4.5"},
 {"provider":"openai-codex","kind":"chat","id":"gpt-5","selector":"openai-codex/gpt-5","name":"GPT-5"}
]}
EOF
  ;;
  usage) cat <<'EOF'
{"reports":[{"provider":"openai-codex","metadata":{"planType":"plus","email":"me@example.com","accountId":"acct_1"},"limits":[]}],"accountsWithoutUsage":[]}
EOF
  ;;
  login) echo "fake omp login $2" ;;
  *) echo "fake omp $*" ;;
esac"#,
                );
                let claude = executable(
                    work,
                    "claude",
                    r#"if [ "$1 $2" = "auth status" ]; then
  echo '{"loggedIn":false,"authMethod":"none","configDirectory":"/Users/someone/.claude"}'
else
  echo "fake claude $*"
fi"#,
                );
                let transcript = work.join("codex-account.jsonl");
                std::fs::write(&transcript, codex_account).unwrap();
                format!(
                    "[terminal]\nenv_inherit = \"daemon\"\n[harness.omp]\ncli = {omp:?}\n[harness.claude]\ncli = {claude:?}\n[codex]\nbin = {codex:?}\nargs = [\"replay\", {transcript:?}, \"app-server\"]\n",
                    codex = codex_bin(),
                )
            },
            |_| {},
        )
        .await;
        Self { t }
    }
}

const CODEX_CHATGPT: &str = r#"{"dir":"out","msg":{"method":"initialize","id":1,"params":{}}}
{"dir":"in","msg":{"id":1,"result":{"userAgent":"hutan/0.162.1 (Mac OS; arm64)"}}}
{"dir":"out","msg":{"method":"initialized"}}
{"dir":"out","msg":{"method":"account/read","id":2,"params":{}}}
{"dir":"in","msg":{"id":2,"result":{"account":{"type":"chatgpt","email":"me@example.com","planType":"plus"},"requiresOpenaiAuth":false}}}
"#;

const CODEX_LOGGED_OUT: &str = r#"{"dir":"out","msg":{"method":"initialize","id":1,"params":{}}}
{"dir":"in","msg":{"id":1,"result":{"userAgent":"hutan/0.162.1 (Mac OS; arm64)"}}}
{"dir":"out","msg":{"method":"initialized"}}
{"dir":"out","msg":{"method":"account/read","id":2,"params":{}}}
{"dir":"in","msg":{"id":2,"result":{"account":null,"requiresOpenaiAuth":true}}}
"#;

#[tokio::test(flavor = "multi_thread")]
async fn accounts_list_what_each_harness_reports_without_tokens_or_addresses() {
    let a = Accounts::start(CODEX_CHATGPT).await;
    let accounts = a.t.client.account_list().await.unwrap();
    let summary: Vec<_> = accounts
        .iter()
        .map(|x| {
            (
                x.harness.as_str(),
                x.provider.as_str(),
                x.label.as_str(),
                x.plan.as_deref(),
                x.status,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                "omp",
                "anthropic",
                "anthropic",
                None,
                AccountStatus::LoggedIn
            ),
            (
                "omp",
                "openai-codex",
                "openai-codex",
                Some("plus"),
                AccountStatus::LoggedIn
            ),
            (
                "claude",
                "anthropic",
                "Claude Code",
                None,
                AccountStatus::LoggedOut
            ),
            (
                "codex",
                "openai",
                "ChatGPT",
                Some("plus"),
                AccountStatus::LoggedIn
            ),
        ]
    );
    // What the harnesses print about people never reaches a client.
    let json = serde_json::to_string(&accounts).unwrap();
    for secret in ["example.com", "acct_1", "/Users/someone"] {
        assert!(!json.contains(secret), "{json}");
    }
    a.t.stop().await;

    let a = Accounts::start(CODEX_LOGGED_OUT).await;
    let codex =
        a.t.client
            .account_list()
            .await
            .unwrap()
            .into_iter()
            .find(|x| x.harness == "codex")
            .unwrap();
    assert_eq!(codex.status, AccountStatus::LoggedOut);
    a.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_harness_that_cannot_be_asked_is_listed_as_unknown_with_what_to_do() {
    let t = H::with(
        |work| {
            format!(
                "[terminal]\nenv_inherit = \"daemon\"\n[harness.omp]\ncli = {:?}\n[harness.claude]\ncli = {:?}\n[codex]\nbin = {:?}\n",
                work.join("no-omp"),
                work.join("no-claude"),
                work.join("no-codex"),
            )
        },
        |_| {},
    )
    .await;
    let accounts = t.client.account_list().await.unwrap();
    assert_eq!(accounts.len(), 3);
    for account in &accounts {
        assert_eq!(account.status, AccountStatus::Unknown, "{account:?}");
        let detail = account.detail.as_deref().unwrap();
        assert!(
            detail.contains("was not found") && detail.contains("daemon.toml"),
            "{detail}"
        );
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_runs_the_harnesss_own_flow_in_a_terminal_session_and_announces_the_change() {
    let mut a = Accounts::start(CODEX_CHATGPT).await;
    let started =
        a.t.client
            .account_login("omp", Some("anthropic".into()))
            .await
            .unwrap();
    assert_eq!(started.instructions, None);
    let session = started.session;
    assert_eq!(session.kind, SessionKind::Terminal);
    assert_eq!(session.title, "Log in to omp (anthropic)");
    assert_eq!(session.command.len(), 3, "{:?}", session.command);
    assert!(
        session.command[0].ends_with("-omp"),
        "{:?}",
        session.command
    );
    assert_eq!(&session.command[1..], ["login", "anthropic"]);
    // The user completes it in the session; when it ends the accounts are listed again.
    wait_event(&mut a.t.events, "account.changed", |event| match event {
        Event::AccountChanged { harness } if harness == "omp" => Some(()),
        _ => None,
    })
    .await;

    // Claude Code and Codex have login commands of their own.
    let claude =
        a.t.client
            .account_login("claude", None)
            .await
            .unwrap()
            .session;
    assert_eq!(&claude.command[1..], ["auth", "login"]);
    let codex =
        a.t.client
            .account_login("codex", None)
            .await
            .unwrap()
            .session;
    assert_eq!(&codex.command[1..], ["login"]);
    let logout =
        a.t.client
            .account_logout("claude", None)
            .await
            .unwrap()
            .session;
    assert_eq!(&logout.command[1..], ["auth", "logout"]);
    a.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn omp_has_no_logout_command_so_the_session_says_what_to_type() {
    let a = Accounts::start(CODEX_CHATGPT).await;
    let out =
        a.t.client
            .account_logout("omp", Some("anthropic".into()))
            .await
            .unwrap();
    assert!(
        out.instructions
            .as_deref()
            .unwrap()
            .contains("/logout anthropic")
    );
    assert_eq!(
        out.session.command.len(),
        1,
        "omp itself: {:?}",
        out.session.command
    );
    a.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn logins_are_validated() {
    let a = Accounts::start(CODEX_CHATGPT).await;
    let c = &a.t.client;
    for (harness, provider) in [
        ("gemini", None),
        ("omp", Some("--help")),
        ("omp", Some("a b")),
        ("claude", Some("openai")),
        ("codex", Some("anthropic")),
    ] {
        let result = c.account_login(harness, provider.map(str::to_owned)).await;
        assert_eq!(
            remote_code(result),
            ErrorCode::BadRequest,
            "{harness} {provider:?}"
        );
    }
    // Nothing was started.
    let sessions = c
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap();
    assert!(sessions.sessions.is_empty(), "{:?}", sessions.sessions);
    a.t.stop().await;
}

// Hutan's own MCP server (protocol 0.21, docs/control.md) -----------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_harness_chat_gets_hutans_mcp_server_with_its_own_session_id() {
    let t = H::omp(&fixture("plain-turn")).await;
    let created = t.create("omp").await;
    let new = t.received_of("session/new");
    let servers = new[0]["params"]["mcpServers"].as_array().unwrap();
    let hutan = servers
        .iter()
        .find(|s| s["name"] == "hutan")
        .unwrap_or_else(|| panic!("no hutan server in {servers:?}"));
    assert_eq!(hutan["args"], serde_json::json!(["mcp"]));
    let env = hutan["env"].as_array().unwrap();
    let value = |name: &str| {
        env.iter()
            .find(|e| e["name"] == name)
            .and_then(|e| e["value"].as_str())
            .map(str::to_owned)
    };
    assert_eq!(
        value("HUTAN_SESSION_ID"),
        Some(created.id.to_string()),
        "requests of the server are attributed to this session"
    );
    assert_eq!(
        value("HUTAN_SOCKET"),
        Some(t.daemon.paths.socket.to_string_lossy().into_owned())
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn control_mcp_false_keeps_hutans_server_out_of_harness_chats() {
    let t = H::with(
        |work| {
            format!(
                "{}\n[control]\nmcp = false\n",
                harness_toml("omp", &fixture("plain-turn"), &work.join("received.jsonl"))
            )
        },
        |_| {},
    )
    .await;
    t.create("omp").await;
    let new = t.received_of("session/new");
    assert_eq!(new[0]["params"]["mcpServers"], serde_json::json!([]));
    t.stop().await;
}

// Usage and limits ------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_omp_turn_is_recorded_with_its_tokens_its_cost_and_its_provider() {
    // `plain-turn`: `usage_update` says the session cost 0.0042 so far, the prompt's answer says
    // what the turn used (1 200 in, 900 of them cached, 34 out, 1 234 in all).
    let mut t = H::omp(&fixture("plain-turn")).await;
    let created = t.create("omp").await;
    t.send(created.id, "Say hello.").await;
    let summary = wait_event(
        &mut t.events,
        "usage.changed with the turn",
        |event| match event {
            Event::UsageChanged { summary }
                if summary
                    .harnesses
                    .iter()
                    .any(|h| h.harness == "omp" && h.week.turns == 1) =>
            {
                Some((**summary).clone())
            }
            _ => None,
        },
    )
    .await;
    let omp = &summary.harnesses[0];
    assert_eq!(omp.harness, "omp");
    assert!(omp.limits.is_empty(), "OMP was not asked for limits");
    for totals in [&omp.today, &omp.week] {
        assert_eq!(
            (
                totals.input_tokens,
                totals.cached_input_tokens,
                totals.output_tokens,
                totals.total_tokens
            ),
            (1_200, 900, 34, 1_234)
        );
        assert_eq!(totals.cost_usd, Some(0.0042));
        assert!(totals.cost_is_estimate, "a harness's cost is an estimate");
    }
    // OMP's model ids are `provider/model`: the provider is charged.
    assert_eq!(summary.providers[0].provider, "gitlab-duo");
    t.stop().await;
}

/// OMP's own `usage --json`, as a fake CLI prints it: the committed report with every reset in
/// the far future (a window that has reset is not shown).
fn omp_usage_report() -> String {
    fn later(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map.iter_mut() {
                    if key == "resetsAt" {
                        *inner = Value::from(4_102_444_800_000u64);
                    } else {
                        later(inner);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(later),
            _ => {}
        }
    }
    let mut report: Value =
        serde_json::from_str(include_str!("../fixtures/omp-usage.json")).unwrap();
    later(&mut report);
    report.to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn omp_is_asked_for_its_limits_only_when_a_refresh_is_asked() {
    let mut t = H::with(
        |work| {
            let omp = executable(
                work,
                "omp-usage",
                &format!("case \"$1\" in\n  usage) cat <<'EOF'\n{}\nEOF\n  ;;\n  *) echo \"fake omp $*\" ;;\nesac", omp_usage_report()),
            );
            format!("[terminal]\nenv_inherit = \"daemon\"\n[harness.omp]\ncli = {omp:?}\n")
        },
        |c| c.usage_probes = true,
    )
    .await;
    let before = t.client.usage_summary(false).await.unwrap();
    assert!(
        before.harnesses.is_empty(),
        "nothing is known until a harness says so"
    );

    let summary = t.client.usage_summary(true).await.unwrap();
    let omp = &summary.harnesses[0];
    assert_eq!(omp.harness, "omp");
    let windows: Vec<_> = omp
        .limits
        .iter()
        .map(|l| {
            (
                l.provider.as_str(),
                l.id.as_str(),
                l.label.as_str(),
                l.used_percent.round() as u64,
                l.resets_at_ms,
            )
        })
        .collect();
    assert_eq!(
        windows,
        [
            ("anthropic", "5h", "5-hour", 7, Some(4_102_444_800_000)),
            ("anthropic", "7d", "weekly", 23, Some(4_102_444_800_000)),
            (
                "anthropic",
                "7d:fable",
                "weekly (fable)",
                0,
                Some(4_102_444_800_000)
            ),
        ],
        "the tightest of the two accounts' 5-hour windows"
    );
    // Everyone is told, and what was learned stays without another refresh.
    wait_event(
        &mut t.events,
        "usage.changed with the windows",
        |event| match event {
            Event::UsageChanged { summary }
                if summary.harnesses.iter().any(|h| h.limits.len() == 3) =>
            {
                Some(())
            }
            _ => None,
        },
    )
    .await;
    let again = t.client.usage_summary(false).await.unwrap();
    assert_eq!(again.harnesses[0].limits, omp.limits);
    // The answer carries no e-mail address, account id or organisation.
    let text = serde_json::to_string(&again).unwrap();
    for private in [
        "example.com",
        "acct-redacted",
        "org-redacted",
        "Example Org",
    ] {
        assert!(!text.contains(private), "{private} in {text}");
    }
    t.stop().await;
}

// Fork (ADR 0023). The transcripts are in `crates/agents/tests/fixtures/acp-omp-18.11`, built
// from the ACP `session/fork` schema, OMP's recorded handshake and what `claude-agent-acp`
// 0.98.0 answers (read in its source; no login here).

fn fork_request(session: hutan_core::SessionId, turn: Option<TurnId>, mode: ForkMode) -> Request {
    Request::ChatFork(ChatFork {
        session_id: session,
        turn_id: turn,
        mode,
        title: None,
        where_: ForkWhere::ThisFolder,
    })
}

/// A harness that replays `names`, one transcript per spawn.
async fn forking(agent: &str, names: &'static [&'static str]) -> H {
    let agent = agent.to_owned();
    H::with(
        move |work| {
            let scripted = script(work, names);
            harness_toml(&agent, &scripted, &work.join("received.jsonl"))
        },
        |_| {},
    )
    .await
}

/// Two answered turns, `first` and `second`.
async fn two_answered_turns(t: &mut H, agent: &str) -> (SessionInfo, Vec<TurnId>) {
    let created = t.create(agent).await;
    for text in ["first", "second"] {
        t.send(created.id, text).await;
        chat_event(&mut t.events, "the turn to end", |e| e.kind.ends_turn()).await;
        t.wait_state(created.id, SessionState::Idle).await;
    }
    let turns = t
        .history(created.id)
        .await
        .events
        .iter()
        .filter(|e| matches!(e.kind, AgentEventKind::TurnStarted))
        .filter_map(|e| e.turn_id)
        .collect();
    (created, turns)
}

async fn try_fork(
    t: &H,
    session: hutan_core::SessionId,
    turn: Option<TurnId>,
    mode: ForkMode,
) -> Result<ChatForked, hutan_client::ClientError> {
    t.client.call(fork_request(session, turn, mode)).await
}

fn users(history: &ChatHistory) -> Vec<&str> {
    history
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::MessageEnd {
                role: MessageRole::User,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_omp_chat_forks_at_its_latest_turn_and_says_why_an_earlier_one_cannot() {
    let mut t = forking("omp", &["two-turns", "fork-head", "fork-head"]).await;
    let (parent, turns) = two_answered_turns(&mut t, "omp").await;
    let support = t.session(parent.id).await.chat.unwrap().fork.unwrap();
    assert_eq!(support.support, ForkSupport::Head, "what OMP advertised");
    assert!(support.note.unwrap().contains("newest turn"));
    let sessions = |t: &H| {
        let client = t.client.clone();
        async move {
            client
                .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
                .await
                .unwrap()
                .sessions
                .len()
        }
    };
    assert_eq!(sessions(&t).await, 1);

    // An earlier turn: refused, and the answer names what would work. Nothing was started.
    let error = try_fork(&t, parent.id, Some(turns[0]), ForkMode::Exact)
        .await
        .unwrap_err();
    let hutan_client::ClientError::Remote(error) = error else {
        panic!("{error:?}")
    };
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert!(error.message.contains("latest turn"), "{}", error.message);
    let data = error.data.unwrap();
    assert_eq!(data["support"], "head");
    assert_eq!(data["alternatives"], serde_json::json!(["head", "summary"]));
    assert!(t.received_of("session/fork").is_empty());
    assert_eq!(sessions(&t).await, 1);

    // The latest turn, by `head`.
    let forked = try_fork(&t, parent.id, None, ForkMode::Head).await.unwrap();
    let child = forked.session;
    assert_eq!(forked.fork.mode, ForkMode::Head);
    assert_eq!((forked.fork.ordinal, forked.fork.turn_id), (2, turns[1]));
    assert_eq!(forked.fork.support, ForkSupport::Head);
    assert_eq!(child.state, SessionState::Idle);
    assert_eq!(child.forked_from.as_ref().unwrap().session_id, parent.id);
    assert_eq!(
        child.chat.as_ref().unwrap().agent_session_id.as_deref(),
        Some("sess_fixture_2"),
        "the harness's own copy of the conversation"
    );
    let fork = &t.received_of("session/fork")[0]["params"];
    assert_eq!(fork["sessionId"], SESSION);
    assert_eq!(
        fork["cwd"],
        parent.cwd.as_ref().unwrap().display().to_string()
    );
    assert!(fork.get("_meta").is_none(), "{fork}");
    let copied = t.history(child.id).await;
    assert_eq!(users(&copied), ["first", "second"]);
    assert_eq!(answers(&copied), ["one", "two"]);
    // What `copied_events` says is what precedes the notice: the parent's resume locator was not
    // copied, and the one the fork's own agent wrote comes after it.
    let notice_at = copied
        .events
        .iter()
        .position(|e| matches!(&e.kind, AgentEventKind::Notice { .. }))
        .unwrap();
    assert_eq!(notice_at as u64, forked.fork.copied_events);
    let parent_events = t.history(parent.id).await.events;
    assert!(
        parent_events
            .iter()
            .any(|e| matches!(e.kind, AgentEventKind::ResumeLocator { .. })),
        "the parent has a locator the copy left out"
    );
    assert!(
        copied.events[..notice_at]
            .iter()
            .all(|e| !matches!(e.kind, AgentEventKind::ResumeLocator { .. }))
    );

    // The fork takes turns; the parent does not hear of them.
    t.send(child.id, "hi").await;
    chat_event(&mut t.events, "the fork's turn to end", |e| {
        e.session_id == child.id && e.kind.ends_turn()
    })
    .await;
    assert_eq!(
        answers(&t.history(child.id).await),
        ["one", "two", "Hello, world."]
    );
    assert_eq!(answers(&t.history(parent.id).await), ["one", "two"]);
    assert_eq!(
        t.received_of("session/prompt").last().unwrap()["params"]["sessionId"],
        "sess_fixture_2"
    );

    // `exact` at the newest turn is the same cut: OMP can make it.
    let exact = try_fork(&t, parent.id, Some(turns[1]), ForkMode::Exact)
        .await
        .unwrap();
    assert_eq!(exact.fork.mode, ForkMode::Exact);
    assert_eq!(sessions(&t).await, 3);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_chat_cuts_at_a_message_through_the_adapters_fork_point() {
    use sha2::{Digest as _, Sha256};
    let mut t = forking("claude", &["claude-two-turns", "claude-fork-exact"]).await;
    let (parent, turns) = two_answered_turns(&mut t, "claude").await;
    assert_eq!(
        t.session(parent.id)
            .await
            .chat
            .unwrap()
            .fork
            .unwrap()
            .support,
        ForkSupport::Exact,
        "an adapter new enough to read a fork point"
    );
    // The journal kept the agent's own id of each assistant message.
    let history = t.history(parent.id).await;
    let ids: Vec<_> = history
        .events
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                AgentEventKind::MessageEnd {
                    role: MessageRole::Assistant,
                    ..
                }
            )
        })
        .map(|e| e.provider.item_id.clone().unwrap())
        .collect();
    assert_eq!(ids, ["msg-1", "msg-2"]);

    let forked = try_fork(&t, parent.id, Some(turns[0]), ForkMode::Exact)
        .await
        .unwrap();
    assert_eq!(forked.fork.ordinal, 1);
    assert_eq!(forked.fork.support, ForkSupport::Exact);
    let meta = &t.received_of("session/fork")[0]["params"]["_meta"];
    assert_eq!(
        meta["jetbrains"]["air"]["fork"],
        serde_json::json!({
            "version": 1,
            "messageId": "msg-1",
            "messageFingerprint": format!("sha256:{:x}", Sha256::digest(b"one")),
            "messageOccurrence": 1,
        })
    );
    let copied = t.history(forked.session.id).await;
    assert_eq!(users(&copied), ["first"]);
    assert_eq!(answers(&copied), ["one"]);
    assert_eq!(
        forked.session.chat.unwrap().agent_session_id.as_deref(),
        Some("sess_fixture_2")
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_summary_fork_starts_a_new_conversation_and_hands_it_a_digest_with_its_first_message() {
    let mut t = forking("omp", &["two-turns", "plain-turn"]).await;
    let (parent, turns) = two_answered_turns(&mut t, "omp").await;
    let forked = try_fork(&t, parent.id, Some(turns[0]), ForkMode::Summary)
        .await
        .unwrap();
    let child = forked.session;
    assert_eq!(forked.fork.mode, ForkMode::Summary);
    assert_eq!(child.forked_from.as_ref().unwrap().mode, ForkMode::Summary);
    assert!(
        t.received_of("session/fork").is_empty(),
        "a fresh conversation, not a fork"
    );
    assert_eq!(t.received_of("session/new").len(), 2);
    // The history shown is Hutan's copy of turn 1, and says what the agent knows of it.
    let copied = t.history(child.id).await;
    assert_eq!(users(&copied), ["first"]);
    assert!(copied.events.iter().any(|e| matches!(
        &e.kind,
        AgentEventKind::Notice { message, .. } if message.contains("summary of turns 1–1")
    )));

    t.send(child.id, "continue").await;
    chat_event(&mut t.events, "the fork's turn to end", |e| {
        e.session_id == child.id && e.kind.ends_turn()
    })
    .await;
    let prompt = t.received_of("session/prompt").pop().unwrap()["params"]["prompt"].to_string();
    assert!(prompt.contains("continue"), "{prompt}");
    assert!(prompt.contains("Turn 1"), "the digest of turn 1: {prompt}");
    assert!(prompt.contains("not by a model"), "{prompt}");
    assert!(
        !prompt.contains("Turn 2") && !prompt.contains("second"),
        "only the turns kept: {prompt}"
    );
    let journal = t.history(child.id).await;
    assert_eq!(
        users(&journal),
        ["first", "continue"],
        "the user's words, not the digest"
    );
    assert!(journal.events.iter().any(|e| matches!(
        &e.kind,
        AgentEventKind::Notice { message, .. } if message.contains("gave the agent the summary")
    )));
    t.wait_state(child.id, SessionState::Idle).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fork_the_agent_refuses_leaves_no_session_behind() {
    let mut t = forking("omp", &["two-turns", "fork-error"]).await;
    let (parent, _) = two_answered_turns(&mut t, "omp").await;
    let error = try_fork(&t, parent.id, None, ForkMode::Head)
        .await
        .unwrap_err();
    let hutan_client::ClientError::Remote(error) = error else {
        panic!("{error:?}")
    };
    assert_eq!(error.code, ErrorCode::ProviderError);
    assert!(
        error.message.contains("fork was cancelled"),
        "{}",
        error.message
    );
    let listed = t
        .client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap();
    assert_eq!(listed.sessions.len(), 1, "no half-made fork");
    assert_eq!(answers(&t.history(parent.id).await), ["one", "two"]);
    t.stop().await;
}
