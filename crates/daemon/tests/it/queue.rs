//! The chat queue (protocol 0.36, capability `chat.queue`, slice UX-QUEUE-STEER) end to end: a
//! real daemon on a temporary `HUTAN_HOME`, real client connections, and the recorded fixture
//! agents (`hutan-acp-fixture replay` for OMP, which takes no message during a turn;
//! `hutan-codex-fixture replay` for Codex, which steers with `turn/steer`). A `hold` entry in a
//! transcript keeps a turn running until the test releases it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::{Path, PathBuf};

use common::{TestDaemon, WAIT, remote_code, wait_event};
use hutan_client::Client;
use hutan_core::agent::{MessageRole, NoticeLevel};
use hutan_core::{AgentEvent, AgentEventKind, Seq, SessionId, SessionState};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{
    ChatCreate, ChatHistory, ChatHistoryRequest, ChatQueue, ChatSentNow, QUEUE_MAX_BYTES,
    QUEUE_MAX_ITEMS, QueueChange, QueueHold,
};
use hutan_protocol::messages::{
    Event, Request, SessionInfo, SessionList, SessionListFilter, SessionResult,
};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

fn agent_fixtures(dir: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures")
        .join(dir)
}

/// The fake agents, next to the test binaries; `just test` builds the workspace binaries first.
fn fixture_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().and_then(Path::parent).unwrap().join(name);
    assert!(
        bin.is_file(),
        "{name} not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

/// The entries of a committed transcript.
fn entries(dir: &str, name: &str) -> Vec<Value> {
    std::fs::read_to_string(agent_fixtures(dir).join(format!("{name}.jsonl")))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn write_transcript(path: &Path, entries: &[Value]) {
    let lines: Vec<String> = entries.iter().map(Value::to_string).collect();
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn hold(file: &Path) -> Value {
    json!({"dir": "hold", "file": file})
}

/// What OMP says and asks for one more turn: a prompt, a reply, its end.
fn acp_turn(reply: &str) -> [Value; 3] {
    [
        json!({"dir": "out", "msg": {"jsonrpc": "2.0", "method": "session/prompt", "id": 1}}),
        json!({"dir": "in", "msg": {"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": "sess_fixture_1",
            "update": {"sessionUpdate": "agent_message_chunk", "messageId": format!("msg-{reply}"),
                "content": {"type": "text", "text": reply}}}}}),
        json!({"dir": "in", "msg": {"jsonrpc": "2.0", "id": 0, "result": {"stopReason": "end_turn"}}}),
    ]
}

struct Q {
    daemon: TestDaemon,
    /// The agent's working directory; also holds the transcripts and the received log.
    work: tempfile::TempDir,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
}

impl Q {
    /// A daemon whose `omp` replays the transcript `make` writes (a file, or a directory of
    /// transcripts used one per spawn).
    async fn omp(make: impl FnOnce(&Path) -> PathBuf) -> Self {
        let work = tempfile::tempdir().unwrap();
        let transcript = make(work.path());
        let toml = format!(
            "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\n",
            bin = fixture_bin("hutan-acp-fixture"),
            received = work.path().join("received.jsonl"),
        );
        let daemon = TestDaemon::start_with(&toml, |_| {}).await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    /// A daemon whose `codex` replays the transcript `make` writes.
    async fn codex(make: impl FnOnce(&Path) -> PathBuf) -> Self {
        let work = tempfile::tempdir().unwrap();
        let transcript = make(work.path());
        let config = hutan_agents::CodexConfig {
            program: fixture_bin("hutan-codex-fixture"),
            args: vec![
                "replay".to_owned(),
                "--received".to_owned(),
                work.path().join("received.jsonl").display().to_string(),
                transcript.display().to_string(),
                "app-server".to_owned(),
            ],
            env: Vec::new(),
        };
        let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.work.path().join(name)
    }

    /// Lets a `hold` on `name` go on.
    fn release(&self, name: &str) {
        std::fs::write(self.path(name), "").unwrap();
    }

    async fn create(&self, agent: &str) -> SessionInfo {
        self.client
            .call::<SessionResult>(Request::ChatCreate(ChatCreate {
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
            }))
            .await
            .unwrap()
            .session
    }

    /// What the fake agent received, as `(method or "response", message)`.
    fn received(&self) -> Vec<(String, Value)> {
        std::fs::read_to_string(self.path("received.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let wrapper: Value = serde_json::from_str(line).unwrap();
                let msg = wrapper["msg"].clone();
                (msg["method"].as_str().unwrap_or("response").to_owned(), msg)
            })
            .collect()
    }

    /// The texts of the prompts OMP received, in order.
    fn prompts(&self) -> Vec<String> {
        self.received()
            .into_iter()
            .filter(|(method, _)| method == "session/prompt")
            .map(|(_, msg)| {
                msg["params"]["prompt"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
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

    async fn wait_state(&self, id: SessionId, state: SessionState) {
        timeout(WAIT, async {
            loop {
                if self.session(id).await.state == state {
                    return;
                }
                sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("session never reached {state:?}"));
    }

    async fn chat_event(&mut self, what: &str, pick: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        wait_event(&mut self.events, what, |event| match event {
            Event::ChatEvent { event, .. } if pick(event) => Some((**event).clone()),
            _ => None,
        })
        .await
    }

    /// The next `chat.queue` event whose change `pick` accepts.
    async fn queue_event(
        &mut self,
        what: &str,
        pick: impl Fn(&QueueChange) -> bool,
    ) -> (ChatQueue, QueueChange) {
        wait_event(&mut self.events, what, |event| match event {
            Event::ChatQueueChanged { queue, change } if pick(change) => {
                Some((queue.clone(), *change))
            }
            _ => None,
        })
        .await
    }

    /// Waits for `count` turns of the session to have ended, counting from now.
    async fn turn_ends(&mut self, count: usize) -> Vec<AgentEvent> {
        let mut ends = Vec::new();
        while ends.len() < count {
            ends.push(
                self.chat_event("a turn to end", |e| e.kind.ends_turn())
                    .await,
            );
        }
        ends
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

fn texts(queue: &ChatQueue) -> Vec<&str> {
    queue.items.iter().map(|item| item.text.as_str()).collect()
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

fn notices(history: &ChatHistory) -> Vec<(NoticeLevel, &str)> {
    history
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::Notice { level, message } => Some((*level, message.as_str())),
            _ => None,
        })
        .collect()
}

fn turns_started(history: &ChatHistory) -> usize {
    history
        .events
        .iter()
        .filter(|e| matches!(e.kind, AgentEventKind::TurnStarted))
        .count()
}

/// OMP's `two-turns` with its first turn held after the first text until `release`, and one
/// more turn: three prompts in all.
fn held_three_turns(dir: &Path) -> PathBuf {
    let mut script = entries("acp-omp-18.11", "two-turns");
    // initialize, its answer, session/new, its answer, the prompt, "one" | the end of turn 1.
    script.insert(6, hold(&dir.join("release")));
    script.extend(acp_turn("three"));
    let path = dir.join("three-turns.jsonl");
    write_transcript(&path, &script);
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_sent_during_a_turn_wait_in_order_and_edits_and_removals_change_what_is_sent() {
    let mut t = Q::omp(held_three_turns).await;
    let created = t.create("omp").await;
    let id = created.id;
    // An idle chat with nothing queued.
    let empty = t.client.chat_queue(id).await.unwrap();
    assert!(empty.items.is_empty() && empty.held.is_none());
    assert!(!empty.steer, "OMP takes no message during a turn");

    t.client.chat_send(id, "first").await.unwrap();
    t.chat_event("the first text", |e| {
        matches!(e.kind, AgentEventKind::MessageDelta { .. })
    })
    .await;
    // The send key during a turn: queued, not refused.
    let a = t.client.chat_queue_add(id, "A").await.unwrap();
    let (queue, change) = t.queue_event("A queued", |_| true).await;
    assert_eq!(change, QueueChange::Added { item_id: a.id });
    assert_eq!(texts(&queue), ["A"]);
    let b = t.client.chat_queue_add(id, "B").await.unwrap();
    let c = t.client.chat_queue_add(id, "C").await.unwrap();
    assert!(a.id < b.id && b.id < c.id);
    // Edit one, remove another.
    let edited = t
        .client
        .chat_queue_edit(id, b.id, "B, edited")
        .await
        .unwrap();
    assert_eq!((edited.id, edited.text.as_str()), (b.id, "B, edited"));
    t.client.chat_queue_remove(id, a.id).await.unwrap();
    assert_eq!(
        remote_code(t.client.chat_queue_remove(id, a.id).await),
        ErrorCode::NotFound,
        "a removed message is gone"
    );
    let queue = t.client.chat_queue(id).await.unwrap();
    assert_eq!(texts(&queue), ["B, edited", "C"]);
    // Another client (a desktop that reconnects) sees the same queue.
    let other = t.daemon.connect().await;
    assert_eq!(other.chat_queue(id).await.unwrap(), queue);
    drop(other);

    // The turn ends: the queue goes, one message per turn, in order.
    t.release("release");
    let ends = t.turn_ends(3).await;
    assert!(
        ends.iter()
            .all(|e| matches!(e.kind, AgentEventKind::TurnCompleted { .. })),
        "{ends:?}"
    );
    t.wait_state(id, SessionState::Idle).await;
    assert_eq!(t.prompts(), ["first", "B, edited", "C"]);
    assert!(t.client.chat_queue(id).await.unwrap().items.is_empty());
    let history = t.history(id).await;
    assert_eq!(answers(&history), ["one", "two", "three"]);
    // A message that left the queue cannot be edited any more.
    assert_eq!(
        remote_code(t.client.chat_queue_edit(id, c.id, "late").await),
        ErrorCode::NotFound
    );
    t.stop().await;
}

/// OMP's `cancel`: one turn that runs until it is cancelled; `then` turns follow.
fn cancellable(dir: &Path, then: &[&str]) -> PathBuf {
    let mut script = entries("acp-omp-18.11", "cancel");
    for reply in then {
        script.extend(acp_turn(reply));
    }
    let path = dir.join("cancellable.jsonl");
    write_transcript(&path, &script);
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn the_queue_is_bounded_and_checked_and_stop_clears_it() {
    let mut t = Q::omp(|dir| cancellable(dir, &[])).await;
    let id = t.create("omp").await.id;
    t.client.chat_send(id, "long task").await.unwrap();
    t.chat_event("text", |e| {
        matches!(e.kind, AgentEventKind::MessageDelta { .. })
    })
    .await;

    assert_eq!(
        remote_code(t.client.chat_queue_add(id, "  ").await),
        ErrorCode::BadRequest
    );
    for n in 0..QUEUE_MAX_ITEMS {
        t.client
            .chat_queue_add(id, format!("message {n}"))
            .await
            .unwrap();
    }
    assert_eq!(
        remote_code(t.client.chat_queue_add(id, "one too many").await),
        ErrorCode::InvalidState,
        "at most {QUEUE_MAX_ITEMS} wait"
    );
    let queue = t.client.chat_queue(id).await.unwrap();
    assert_eq!(queue.items.len(), QUEUE_MAX_ITEMS);
    // The text of the queue is bounded too: no edit makes it larger than the limit.
    // One message is at most SEND_MAX_BYTES; five of a fifth of the queue's limit do not fit.
    let big = "x".repeat(QUEUE_MAX_BYTES / 5 + 1);
    let first = queue.items[0].id;
    let mut refused = false;
    for item in &queue.items {
        if let Err(error) = t.client.chat_queue_edit(id, item.id, big.clone()).await {
            assert_eq!(remote_code::<()>(Err(error)), ErrorCode::InvalidState);
            refused = true;
            break;
        }
    }
    assert!(
        refused,
        "five messages of a fifth of the limit each do not fit"
    );
    assert_eq!(
        remote_code(t.client.chat_queue_edit(id, 9_999, "nobody").await),
        ErrorCode::NotFound
    );
    // `chat.send_now` takes text or a queued message, not both and not neither.
    for (text, item) in [(Some("x".to_owned()), Some(first)), (None, None)] {
        assert_eq!(
            remote_code(t.client.chat_send_now(id, text, item).await),
            ErrorCode::BadRequest
        );
    }

    // Stop stops: the queue is cleared before the turn ends, and nothing starts after it.
    t.client.chat_interrupt(id).await.unwrap();
    let (queue, change) = t
        .queue_event("the queue cleared by Stop", |c| {
            matches!(c, QueueChange::Cleared { .. })
        })
        .await;
    assert_eq!(
        change,
        QueueChange::Cleared {
            count: u32::try_from(QUEUE_MAX_ITEMS).unwrap(),
            stopped: true
        }
    );
    assert!(queue.items.is_empty());
    let end = t
        .chat_event("the turn to end", |e| e.kind.ends_turn())
        .await;
    assert!(matches!(end.kind, AgentEventKind::TurnCancelled));
    t.wait_state(id, SessionState::Idle).await;
    assert!(t.client.chat_queue(id).await.unwrap().items.is_empty());
    assert_eq!(turns_started(&t.history(id).await), 1);
    assert_eq!(t.prompts(), ["long task"]);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_with_keep_queue_keeps_the_messages_waiting_until_the_user_sends() {
    let mut t = Q::omp(|dir| cancellable(dir, &["queued answer"])).await;
    let id = t.create("omp").await.id;
    t.client.chat_send(id, "long task").await.unwrap();
    t.chat_event("text", |e| {
        matches!(e.kind, AgentEventKind::MessageDelta { .. })
    })
    .await;
    let kept = t.client.chat_queue_add(id, "after the stop").await.unwrap();
    t.queue_event("queued", |c| matches!(c, QueueChange::Added { .. }))
        .await;

    t.client.chat_stop(id, true).await.unwrap();
    let (queue, change) = t
        .queue_event("the queue holds", |c| *c == QueueChange::Held)
        .await;
    assert_eq!(change, QueueChange::Held);
    assert_eq!(queue.held, Some(QueueHold::Stopped));
    t.chat_event("the turn to end", |e| e.kind.ends_turn())
        .await;
    t.wait_state(id, SessionState::Idle).await;
    let queue = t.client.chat_queue(id).await.unwrap();
    assert_eq!(texts(&queue), ["after the stop"], "kept, not sent");
    assert_eq!(queue.held, Some(QueueHold::Stopped));
    assert_eq!(t.prompts(), ["long task"]);

    // The user goes on: Send now of the kept message starts it and the queue no longer holds.
    let sent = t
        .client
        .chat_send_now(id, None, Some(kept.id))
        .await
        .unwrap();
    assert!(matches!(sent, ChatSentNow::Started { .. }), "{sent:?}");
    t.chat_event("the queued turn to end", |e| e.kind.ends_turn())
        .await;
    assert_eq!(t.prompts(), ["long task", "after the stop"]);
    let queue = t.client.chat_queue(id).await.unwrap();
    assert!(queue.items.is_empty() && queue.held.is_none(), "{queue:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn send_now_on_an_agent_without_steering_interrupts_the_turn_says_so_and_sends_next() {
    let mut t = Q::omp(|dir| cancellable(dir, &["Done.", "Later done."])).await;
    let id = t.create("omp").await.id;
    t.client.chat_send(id, "long task").await.unwrap();
    t.chat_event("text", |e| {
        matches!(e.kind, AgentEventKind::MessageDelta { .. })
    })
    .await;
    t.client.chat_queue_add(id, "later").await.unwrap();

    let sent = t
        .client
        .chat_send_now(id, Some("Stop and fix the build".to_owned()), None)
        .await
        .unwrap();
    assert_eq!(sent, ChatSentNow::Interrupting);
    // The transcript says the turn is interrupted for it, before the turn ends.
    let notice = t
        .chat_event("the notice", |e| {
            matches!(&e.kind, AgentEventKind::Notice { message, .. } if message.contains("interrupts the turn"))
        })
        .await;
    let AgentEventKind::Notice { level, message } = &notice.kind else {
        unreachable!()
    };
    assert_eq!(*level, NoticeLevel::Warning);
    assert!(message.contains("interrupts the turn"), "{message}");
    let ends = t.turn_ends(3).await;
    assert!(matches!(ends[0].kind, AgentEventKind::TurnCancelled));
    assert!(matches!(ends[1].kind, AgentEventKind::TurnCompleted { .. }));
    t.wait_state(id, SessionState::Idle).await;
    // The message went first, then the rest of the queue.
    assert_eq!(
        t.prompts(),
        ["long task", "Stop and fix the build", "later"]
    );
    assert!(t.client.chat_queue(id).await.unwrap().items.is_empty());
    let history = t.history(id).await;
    assert!(
        notices(&history)
            .iter()
            .any(|(_, text)| text.contains("interrupts the turn")),
        "the notice is journaled: {:?}",
        notices(&history)
    );
    t.stop().await;
}

/// Codex's `plain-turn` where the user steers the running turn: `turn/steer` after the
/// agent's echo of the first message, then Codex's echo of the steered input.
fn steered_turn(dir: &Path, steer: &str) -> PathBuf {
    let mut script = entries("codex-0.162", "plain-turn");
    let echo = script
        .iter()
        .position(|e| {
            e["msg"]["method"] == "item/completed"
                && e["msg"]["params"]["item"]["type"] == "userMessage"
        })
        .unwrap();
    let thread = script[echo]["msg"]["params"]["threadId"].clone();
    let turn = script[echo]["msg"]["params"]["turnId"].clone();
    let item = json!({"type": "userMessage", "id": "01a00000-0000-7000-8000-0000000000ff",
        "clientId": null, "content": [{"type": "text", "text": steer, "text_elements": []}]});
    let steered = [
        json!({"dir": "out", "msg": {"method": "turn/steer", "id": 0}}),
        json!({"dir": "in", "msg": {"id": 0, "result": {"turnId": turn}}}),
        json!({"dir": "in", "msg": {"method": "item/started", "params": {
            "item": item, "threadId": thread, "turnId": turn}}}),
        json!({"dir": "in", "msg": {"method": "item/completed", "params": {
            "item": item, "threadId": thread, "turnId": turn}}}),
    ];
    for (offset, entry) in steered.into_iter().enumerate() {
        script.insert(echo + 1 + offset, entry);
    }
    let path = dir.join("steered.jsonl");
    write_transcript(&path, &script);
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn send_now_steers_a_codex_turn_at_its_next_step_without_a_new_turn() {
    let mut t = Q::codex(|dir| steered_turn(dir, "Focus on the tests")).await;
    let id = t.create("codex").await.id;
    let sent = t
        .client
        .chat_send(id, "Reply with the single word pong and nothing else.")
        .await
        .unwrap();
    // The agent's echo of the first message: the turn runs and Codex waits for the steer.
    t.chat_event("the first user message", |e| {
        matches!(
            e.kind,
            AgentEventKind::MessageEnd {
                role: MessageRole::User,
                ..
            }
        )
    })
    .await;
    let queued = t
        .client
        .chat_queue_add(id, "Focus on the tests")
        .await
        .unwrap();
    assert!(t.client.chat_queue(id).await.unwrap().steer, "Codex steers");
    let now = t
        .client
        .chat_send_now(id, None, Some(queued.id))
        .await
        .unwrap();
    assert_eq!(
        now,
        ChatSentNow::Steered {
            turn_id: sent.turn_id
        }
    );
    let end = t
        .chat_event("the turn to end", |e| e.kind.ends_turn())
        .await;
    assert!(matches!(end.kind, AgentEventKind::TurnCompleted { .. }));
    assert_eq!(end.turn_id, Some(sent.turn_id), "no new turn");

    let steers: Vec<Value> = t
        .received()
        .into_iter()
        .filter(|(method, _)| method == "turn/steer")
        .map(|(_, msg)| msg)
        .collect();
    assert_eq!(steers.len(), 1, "{:?}", t.received());
    assert_eq!(
        steers[0]["params"]["input"][0]["text"],
        "Focus on the tests"
    );
    assert_eq!(
        steers[0]["params"]["expectedTurnId"],
        "01a00000-0000-7000-8000-000000000002"
    );
    assert_eq!(
        t.received()
            .iter()
            .filter(|(method, _)| method == "turn/start")
            .count(),
        1
    );
    let history = t.history(id).await;
    let users: Vec<&str> = history
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
        .collect();
    assert_eq!(
        users,
        [
            "Reply with the single word pong and nothing else.",
            "Focus on the tests"
        ]
    );
    assert!(t.client.chat_queue(id).await.unwrap().items.is_empty());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_queue_survives_a_daemon_restart_and_waits_until_the_user_sends() {
    let mut t = Q::omp(|dir| {
        let script = dir.join("script");
        std::fs::create_dir_all(&script).unwrap();
        // The first agent runs a turn the daemon's stop cuts off.
        let mut first = entries("acp-omp-18.11", "two-turns");
        first.truncate(6);
        first.push(hold(&dir.join("never")));
        write_transcript(&script.join("01-cut.jsonl"), &first);
        // The second continues the conversation and runs two more turns.
        let mut second = entries("acp-omp-18.11", "load");
        second.extend(acp_turn("again"));
        write_transcript(&script.join("02-load.jsonl"), &second);
        script
    })
    .await;
    let id = t.create("omp").await.id;
    t.client.chat_send(id, "first").await.unwrap();
    t.chat_event("text", |e| {
        matches!(e.kind, AgentEventKind::MessageDelta { .. })
    })
    .await;
    let a = t.client.chat_queue_add(id, "A").await.unwrap();
    let b = t.client.chat_queue_add(id, "B").await.unwrap();

    let Q { daemon, work, .. } = t;
    let daemon = daemon.restart().await;
    let (client, events) = daemon.connect_subscribed().await;
    let t = Q {
        daemon,
        work,
        client,
        events,
    };
    t.wait_state(id, SessionState::Idle).await;
    // Kept, in order, and waiting: the agent went away mid-turn.
    let queue = t.client.chat_queue(id).await.unwrap();
    assert_eq!(texts(&queue), ["A", "B"]);
    assert_eq!(queue.held, Some(QueueHold::AgentGone));
    assert_eq!(
        queue.items.iter().map(|i| i.id).collect::<Vec<_>>(),
        [a.id, b.id]
    );
    // Nothing was sent by itself.
    assert_eq!(t.prompts(), ["first"]);

    // Send now of B: it goes first; then the queue goes on with A.
    let mut t = t;
    let sent = t.client.chat_send_now(id, None, Some(b.id)).await.unwrap();
    assert!(matches!(sent, ChatSentNow::Started { .. }), "{sent:?}");
    t.turn_ends(2).await;
    t.wait_state(id, SessionState::Idle).await;
    assert_eq!(t.prompts(), ["first", "B", "A"]);
    let queue = t.client.chat_queue(id).await.unwrap();
    assert!(queue.items.is_empty() && queue.held.is_none(), "{queue:?}");
    t.stop().await;
}
