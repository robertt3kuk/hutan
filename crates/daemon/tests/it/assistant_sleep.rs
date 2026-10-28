//! Idle harness assistants give their agent process back (`docs/assistant.md` § Idle
//! assistants): a real daemon on a temporary `HUTAN_HOME`, real client connections and
//! `hutan-acp-fixture replay` standing in for `omp acp`. The idle time is shortened through
//! `DaemonConfig::assistant_idle_stop`; nothing here starts a real harness or calls a model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use common::{TestDaemon, WAIT, remote_code, stop, wait_event};
use hutan_client::Client;
use hutan_core::agent::{MessageRole, NoticeLevel, PermissionOutcome};
use hutan_core::{AgentEvent, AgentEventKind, Seq, SessionId, SessionState};
use hutan_daemon::GatewayConfig;
use hutan_gateway::{CredentialStore, HttpConfig, MemoryStore};
use hutan_protocol::ErrorCode;
use hutan_protocol::assistant::{AssistantOpen, AssistantScope};
use hutan_protocol::chat::{
    ApprovalInfo, ApprovalList, ApprovalListRequest, ApprovalRespond, ChatCreate, ChatHistory,
    ChatHistoryRequest,
};
use hutan_protocol::messages::{
    Empty, Event, Request, SessionInfo, SessionList, SessionListFilter, SessionResult,
};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

const SESSION: &str = "sess_fixture_1";
/// The model the fixtures start on, so that the configured assistant needs no model switch.
const FIXTURE_MODEL: &str = "gitlab-duo/duo-chat-opus-4-6";
/// How long the assistants of these tests may sit idle.
const IDLE: Duration = Duration::from_millis(700);

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/acp-omp-18.11")
}

/// The fake ACP agent, next to the test binaries (`just test` builds it first).
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

/// A directory of transcripts, used one per spawn of the agent, in this order.
fn script(dir: &Path, names: &[&str]) -> PathBuf {
    let scripted = dir.join("script");
    std::fs::create_dir_all(&scripted).unwrap();
    for (index, name) in names.iter().enumerate() {
        std::fs::copy(
            fixtures().join(format!("{name}.jsonl")),
            scripted.join(format!("{:02}-{name}.jsonl", index + 1)),
        )
        .unwrap();
    }
    scripted
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

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn wait_gone(pid: u32) {
    timeout(WAIT, async {
        while alive(pid) {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("process {pid} is still running"));
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

struct S {
    daemon: TestDaemon,
    /// The agents' working directory; holds the scripted transcripts and the received log.
    work: tempfile::TempDir,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
}

fn tweak(config: &mut hutan_daemon::DaemonConfig) {
    config.gateway = gateway();
    config.assistant_idle_stop = Some(IDLE);
}

impl S {
    /// A daemon whose `omp` (the default assistant's backend) replays the transcripts `names`,
    /// one per start of the agent.
    async fn start(names: &[&str]) -> Self {
        let work = tempfile::tempdir().unwrap();
        let scripted = script(work.path(), names);
        let toml = format!(
            "[terminal]\nenv_inherit = \"daemon\"\n\n\
             [harness.omp]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {scripted:?}, \"acp\"]\n\n\
             [assistant]\nbackend = \"omp\"\nmodel = \"{FIXTURE_MODEL}\"\n",
            bin = acp_bin(),
            received = work.path().join("received.jsonl"),
        );
        let daemon = TestDaemon::start_with(&toml, tweak).await;
        Self::around(daemon, work).await
    }

    async fn around(daemon: TestDaemon, work: tempfile::TempDir) -> Self {
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    /// The same daemon after a restart: new epoch, same database, same fake agent.
    async fn restarted(self) -> Self {
        let Self { daemon, work, .. } = self;
        let daemon = daemon.restart_with(tweak).await;
        Self::around(daemon, work).await
    }

    fn received_of(&self, method: &str) -> Vec<Value> {
        std::fs::read_to_string(self.work.path().join("received.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["msg"].clone())
            .filter(|msg| msg["method"] == method)
            .collect()
    }

    /// How many times the agent was started.
    fn starts(&self) -> usize {
        self.received_of("initialize").len()
    }

    async fn open(&self) -> SessionInfo {
        self.client
            .call::<SessionResult>(Request::AssistantOpen(AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: false,
            }))
            .await
            .unwrap()
            .session
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

    /// The processes of the session's agent, as the resource manager sees them.
    async fn pids(&self, id: SessionId) -> Vec<u32> {
        self.client
            .resources_list(Some(id))
            .await
            .unwrap()
            .sessions
            .iter()
            .filter(|s| s.session_id == id)
            .flat_map(|s| s.processes.iter().map(|p| p.pid))
            .collect()
    }

    /// The pid of the agent of a session that runs one.
    async fn agent_pid(&self, id: SessionId) -> u32 {
        let pids = self.pids(id).await;
        assert_eq!(pids.len(), 1, "one agent process: {pids:?}");
        pids[0]
    }

    async fn turn_end(&mut self) -> AgentEvent {
        wait_event(&mut self.events, "the turn to end", |event| match event {
            Event::ChatEvent { event, .. } if event.kind.ends_turn() => Some((**event).clone()),
            _ => None,
        })
        .await
    }

    /// One turn, to its end.
    async fn turn(&mut self, id: SessionId, text: &str) {
        self.client.chat_send(id, text).await.unwrap();
        let end = self.turn_end().await;
        assert!(
            matches!(end.kind, AgentEventKind::TurnCompleted { .. }),
            "{end:?}"
        );
    }

    /// Until the idle sleeper has stopped the agent that ran as `pid`.
    async fn asleep(&self, id: SessionId, pid: u32) {
        wait_gone(pid).await;
        self.session_is_idle_and_empty(id).await;
    }

    async fn session_is_idle_and_empty(&self, id: SessionId) {
        let session = self.session(id).await;
        assert_eq!(session.state, SessionState::Idle, "{session:?}");
        assert!(session.recovery.is_none(), "no recovery chip: {session:?}");
        // The resource manager sees no process, nothing failed or stopped.
        timeout(WAIT, async {
            while !self.pids(id).await.is_empty() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the session has no process");
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

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_assistants_agent_exits_while_the_session_stays_idle_with_its_history() {
    let mut t = S::start(&["plain-turn"]).await;
    let assistant = t.open().await;
    assert!(assistant.chat.as_ref().unwrap().assistant);
    t.turn(assistant.id, "Say hello.").await;
    let pid = t.agent_pid(assistant.id).await;
    let before = t.session(assistant.id).await;
    assert_eq!(before.state, SessionState::Idle);

    t.asleep(assistant.id, pid).await;

    let sleeping = t.session(assistant.id).await;
    assert_eq!(sleeping.id, assistant.id, "the same session");
    assert_eq!(
        sleeping.state,
        SessionState::Idle,
        "not stopped, not failed"
    );
    let chat = sleeping.chat.clone().unwrap();
    assert_eq!(chat.model, FIXTURE_MODEL, "the same model");
    assert_eq!(chat.agent_session_id.as_deref(), Some(SESSION));
    assert!(!chat.turn_active);
    assert!(sleeping.recovery.is_none());
    let history = t.history(assistant.id).await;
    assert_eq!(answers(&history), ["Hello, world."], "the history is there");
    assert_eq!(t.starts(), 1, "nothing started the agent again");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_send_wakes_the_agent_resumes_the_conversation_and_the_turn_streams() {
    let mut t = S::start(&["plain-turn", "load"]).await;
    let assistant = t.open().await;
    t.turn(assistant.id, "Say hello.").await;
    let before = t.history(assistant.id).await;
    let pid = t.agent_pid(assistant.id).await;
    t.asleep(assistant.id, pid).await;
    assert!(
        t.received_of("session/load").is_empty(),
        "still the first agent"
    );

    let sent = t.client.chat_send(assistant.id, "again").await.unwrap();
    let started = wait_event(&mut t.events, "the turn to start", |event| match event {
        Event::ChatEvent { event, .. } if matches!(event.kind, AgentEventKind::TurnStarted) => {
            Some((**event).clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(started.turn_id, Some(sent.turn_id));
    let end = t.turn_end().await;
    assert!(matches!(end.kind, AgentEventKind::TurnCompleted { .. }));

    let loads = t.received_of("session/load");
    assert_eq!(loads.len(), 1, "the agent continued the stored session");
    assert_eq!(loads[0]["params"]["sessionId"], SESSION);
    assert_eq!(t.received_of("session/new").len(), 1, "no new conversation");
    assert_eq!(t.starts(), 2);
    let woken = t.session(assistant.id).await;
    assert_eq!(woken.state, SessionState::Idle);
    assert!(woken.recovery.is_none(), "a wake is not a recovery");
    assert_eq!(
        woken.chat.unwrap().agent_session_id.as_deref(),
        Some(SESSION)
    );
    assert_ne!(t.agent_pid(assistant.id).await, pid, "a new process");

    let after = t.history(assistant.id).await;
    assert_eq!(answers(&after), ["Hello, world.", "Welcome back."]);
    assert_eq!(
        &after.events[..before.events.len()],
        &before.events[..],
        "what was there is unchanged"
    );
    assert!(
        after.events.windows(2).all(|w| w[0].seq < w[1].seq),
        "seq never goes back"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_approval_keeps_the_assistant_awake_and_answering_starts_the_clock() {
    let mut t = S::start(&["permission-allow"]).await;
    let assistant = t.open().await;
    t.client
        .chat_send(assistant.id, "edit lib.rs")
        .await
        .unwrap();
    let approval = wait_event(&mut t.events, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some((**approval).clone()),
        _ => None,
    })
    .await;
    let pid = t.agent_pid(assistant.id).await;

    // Several times the idle time: the agent stays, the approval stays.
    sleep(IDLE * 3).await;
    assert!(alive(pid), "an agent waiting for the user is not stopped");
    assert_eq!(t.approvals().await, std::slice::from_ref(&approval));
    assert_eq!(
        t.session(assistant.id).await.state,
        SessionState::WaitingApproval
    );

    let answered_at = std::time::Instant::now();
    t.client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval.id,
            option_id: "allow_once".into(),
        }))
        .await
        .unwrap();
    wait_event(&mut t.events, "approval.resolved", |event| match event {
        Event::ApprovalResolved { outcome, .. } => {
            assert_eq!(
                *outcome,
                PermissionOutcome::Selected {
                    option_id: "allow_once".into()
                }
            );
            Some(())
        }
        _ => None,
    })
    .await;
    t.turn_end().await;
    assert!(alive(pid), "the clock starts at the answer, not before it");

    // Idle now: it goes to sleep, no sooner than the idle time after the answer.
    t.asleep(assistant.id, pid).await;
    assert!(answered_at.elapsed() >= IDLE, "{:?}", answered_at.elapsed());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn assistant_open_returns_a_sleeping_assistant_without_starting_its_agent() {
    let mut t = S::start(&["plain-turn", "load"]).await;
    let assistant = t.open().await;
    t.turn(assistant.id, "Say hello.").await;
    let pid = t.agent_pid(assistant.id).await;
    t.asleep(assistant.id, pid).await;
    assert_eq!(t.starts(), 1);

    let opened = t.open().await;
    assert_eq!(opened.id, assistant.id, "the default is still that session");
    assert_eq!(opened.state, SessionState::Idle);
    assert_eq!(answers(&t.history(opened.id).await), ["Hello, world."]);
    // Neither the open nor reading the history started a process.
    sleep(IDLE / 2).await;
    assert_eq!(t.starts(), 1);
    assert!(t.pids(assistant.id).await.is_empty());
    // It starts on the first send.
    t.turn(assistant.id, "again").await;
    assert_eq!(t.starts(), 2);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_restart_restores_a_sleeping_assistant_asleep_and_the_first_send_starts_it() {
    let mut t = S::start(&["plain-turn", "load"]).await;
    let assistant = t.open().await;
    t.turn(assistant.id, "Say hello.").await;
    let pid = t.agent_pid(assistant.id).await;
    t.asleep(assistant.id, pid).await;

    let mut t = t.restarted().await;
    let restored = t.session(assistant.id).await;
    assert_eq!(restored.id, assistant.id);
    assert_eq!(restored.state, SessionState::Idle);
    assert!(!restored.restored);
    sleep(IDLE).await;
    assert_eq!(t.starts(), 1, "the restore started no agent");
    assert!(t.pids(assistant.id).await.is_empty());
    assert_eq!(answers(&t.history(assistant.id).await), ["Hello, world."]);
    // Opening it still does not start it.
    assert_eq!(t.open().await.id, assistant.id);
    assert_eq!(t.starts(), 1);

    t.turn(assistant.id, "again").await;
    assert_eq!(t.starts(), 2);
    let loads = t.received_of("session/load");
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0]["params"]["sessionId"], SESSION);
    assert_eq!(
        answers(&t.history(assistant.id).await),
        ["Hello, world.", "Welcome back."]
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assistant_that_was_awake_at_shutdown_comes_back_asleep_too() {
    let mut t = S::start(&["plain-turn", "load"]).await;
    let assistant = t.open().await;
    t.turn(assistant.id, "Say hello.").await;
    assert_eq!(t.starts(), 1);

    let mut t = t.restarted().await;
    assert_eq!(t.session(assistant.id).await.state, SessionState::Idle);
    sleep(IDLE).await;
    assert_eq!(
        t.starts(),
        1,
        "no agent was started for the restored assistant"
    );
    assert!(t.pids(assistant.id).await.is_empty());
    t.turn(assistant.id, "again").await;
    assert_eq!(t.starts(), 2);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chats_that_are_not_assistants_are_left_running() {
    let t = S::start(&["plain-turn"]).await;
    let chat = t
        .client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: None,
            cwd: t.work.path().to_owned(),
            agent: "omp".to_owned(),
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
        .unwrap()
        .session;
    assert!(!chat.chat.as_ref().unwrap().is_assistant());
    let pid = t.agent_pid(chat.id).await;
    sleep(IDLE * 4).await;
    assert!(alive(pid), "an agent chat keeps its process");
    assert_eq!(t.pids(chat.id).await, [pid]);
    assert_eq!(t.session(chat.id).await.state, SessionState::Idle);
    assert_eq!(t.starts(), 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wake_the_agent_cannot_resume_starts_a_new_conversation_and_says_so() {
    let mut t = S::start(&["plain-turn", "load-fails", "plain-turn"]).await;
    let assistant = t.open().await;
    t.turn(assistant.id, "Say hello.").await;
    let pid = t.agent_pid(assistant.id).await;
    t.asleep(assistant.id, pid).await;

    // `session/load` is refused: the second agent is dropped, the third starts afresh.
    t.turn(assistant.id, "again").await;
    assert_eq!(t.received_of("session/load").len(), 1);
    assert_eq!(t.received_of("session/new").len(), 2);
    assert_eq!(t.starts(), 3);
    let history = t.history(assistant.id).await;
    assert_eq!(answers(&history), ["Hello, world.", "Hello, world."]);
    let notice = history
        .events
        .iter()
        .find_map(|e| match &e.kind {
            AgentEventKind::Notice { level, message } => Some((*level, message.as_str())),
            _ => None,
        })
        .expect("the journal says so");
    assert_eq!(
        notice,
        (
            NoticeLevel::Info,
            "The agent started a new conversation; the transcript is kept."
        )
    );
    let woken = t.session(assistant.id).await;
    assert_eq!(woken.state, SessionState::Idle);
    assert!(woken.recovery.is_none());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wake_that_fails_fails_the_send_and_the_session_stays_usable_for_a_retry() {
    // The second agent speaks another ACP version: it cannot be started at all.
    let mut t = S::start(&["plain-turn", "old-protocol", "load"]).await;
    let assistant = t.open().await;
    t.turn(assistant.id, "Say hello.").await;
    let pid = t.agent_pid(assistant.id).await;
    t.asleep(assistant.id, pid).await;

    let failed = t.client.chat_send(assistant.id, "again").await;
    assert_eq!(remote_code(failed), ErrorCode::ProviderError);
    let session = t.session(assistant.id).await;
    assert_eq!(session.state, SessionState::Idle, "not failed: {session:?}");
    assert!(t.pids(assistant.id).await.is_empty());
    assert_eq!(answers(&t.history(assistant.id).await), ["Hello, world."]);

    // The retry wakes it for real.
    t.turn(assistant.id, "once more").await;
    assert_eq!(t.starts(), 3);
    assert_eq!(
        answers(&t.history(assistant.id).await),
        ["Hello, world.", "Welcome back."]
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sleeping_assistant_can_be_stopped_and_a_stopped_one_is_not_woken_by_open() {
    let mut t = S::start(&["plain-turn", "load"]).await;
    let assistant = t.open().await;
    t.turn(assistant.id, "Say hello.").await;
    let pid = t.agent_pid(assistant.id).await;
    t.asleep(assistant.id, pid).await;

    let stopped = stop(&t.client, assistant.id).await.unwrap();
    assert_eq!(stopped.state, SessionState::Stopped);
    assert_eq!(t.starts(), 1, "nothing was started to stop it");
    assert_eq!(
        remote_code(t.client.chat_send(assistant.id, "hi").await),
        ErrorCode::InvalidState
    );
    // A stopped default keeps today's behaviour: opening it restarts the agent.
    let reopened = t.open().await;
    assert_eq!(reopened.id, assistant.id);
    timeout(WAIT, async {
        while t.received_of("session/load").is_empty() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the agent was started again");
    assert_eq!(t.starts(), 2);
    assert_eq!(
        t.received_of("session/load")[0]["params"]["sessionId"],
        SESSION
    );
    t.stop().await;
}

/// Whether any process still has `needle` in its command line.
fn any_process_with(needle: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", needle])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_an_assistant_starts_no_agent_until_the_first_message() {
    let mut t = S::start(&["plain-turn"]).await;
    // The agent would log into this file: its path is in the process's command line too.
    let marker = t.work.path().join("received.jsonl");
    let marker = marker.to_string_lossy().into_owned();

    let assistant = t.open().await;
    assert!(assistant.chat.as_ref().unwrap().assistant);
    assert_eq!(assistant.state, SessionState::Idle, "{assistant:?}");
    assert_eq!(
        assistant.chat.as_ref().unwrap().model,
        FIXTURE_MODEL,
        "the configured model is on the record"
    );
    // A second look changes nothing, and nothing runs: no agent, nothing for `hutan ps`.
    let again = t.open().await;
    assert_eq!(again.id, assistant.id);
    sleep(Duration::from_millis(300)).await;
    assert_eq!(t.starts(), 0, "the agent was never initialized");
    assert!(!any_process_with(&marker), "no agent process");
    t.session_is_idle_and_empty(assistant.id).await;
    assert!(t.history(assistant.id).await.events.is_empty());
    assert_eq!(
        t.client.resources_list(None).await.unwrap().sessions.len(),
        0,
        "the resource manager has no row for it"
    );

    // The first message starts it and the turn streams as ever.
    t.turn(assistant.id, "Say hello.").await;
    assert_eq!(t.starts(), 1);
    assert!(any_process_with(&marker), "the agent runs now");
    let history = t.history(assistant.id).await;
    assert_eq!(answers(&history), ["Hello, world."]);
    t.agent_pid(assistant.id).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assistant_that_was_never_started_comes_back_from_a_restart_still_without_an_agent() {
    let t = S::start(&["plain-turn"]).await;
    let assistant = t.open().await;
    assert_eq!(assistant.state, SessionState::Idle);
    let mut t = t.restarted().await;
    let after = t.session(assistant.id).await;
    assert_eq!(after.state, SessionState::Idle);
    sleep(Duration::from_millis(300)).await;
    assert_eq!(t.starts(), 0);
    assert!(t.pids(assistant.id).await.is_empty());
    // And it still starts with its first message.
    t.turn(assistant.id, "Say hello.").await;
    assert_eq!(t.starts(), 1);
    t.stop().await;
}
