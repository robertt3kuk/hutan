//! Session folders (protocol 0.57, capability `session.dirs`, ADR 0023) end to end: a real
//! daemon on a temporary `HUTAN_HOME`, real client connections, the fixture harnesses standing
//! in for `omp acp`, Claude Code's ACP adapter and `codex app-server`. Each test asserts what
//! the harness received, what the session reports back (`ChatInfo.dirs`, `applied`), who may ask
//! (user-only), and what the file service and Contain do with a folder.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, create, remote_code, stop, wait_event};
use hutan_client::Client;
use hutan_core::{AgentEventKind, SessionId, SessionState};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ChatCreate, ChatDir, ChatDirApplied, ChatDirRole, ChatHistoryRequest};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Event, Origin, Request, SessionCreate,
    SessionInfo, SessionList, SessionListFilter, SessionRef, SessionResult, SessionSpec,
};
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};

fn agents_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures")
}

fn bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().and_then(Path::parent).unwrap().join(name);
    assert!(
        bin.is_file(),
        "{name} not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

fn acp_fixture(name: &str) -> PathBuf {
    agents_fixtures()
        .join("acp-omp-18.11")
        .join(format!("{name}.jsonl"))
}

fn jsonl(lines: &[Value]) -> String {
    lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn fixture_lines(name: &str) -> Vec<Value> {
    std::fs::read_to_string(acp_fixture(name))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// What OMP's local command prints, as the fake agent says it: the text, then the end of the
/// prompt.
fn local(text: &str) -> Vec<Value> {
    vec![
        json!({"dir": "out", "msg": {"jsonrpc": "2.0", "method": "session/prompt", "id": 1}}),
        json!({"dir": "in", "msg": {"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": "sess_fixture_1",
            "update": {"sessionUpdate": "agent_message_chunk", "messageId": "local",
                "content": {"type": "text", "text": text}}}}}),
        json!({"dir": "in", "msg": {"jsonrpc": "2.0", "id": 0, "result": {"stopReason": "end_turn"}}}),
    ]
}

/// A directory of transcripts, one per spawn of the agent.
fn script(dir: &Path, spawns: &[Vec<Value>]) -> PathBuf {
    let scripted = dir.join("script");
    std::fs::create_dir_all(&scripted).unwrap();
    for (index, lines) in spawns.iter().enumerate() {
        std::fs::write(
            scripted.join(format!("{:02}.jsonl", index + 1)),
            jsonl(lines),
        )
        .unwrap();
    }
    scripted
}

/// `[harness.<id>]` replaying `transcript`, and daemon-mode environments.
fn harness_toml(id: &str, transcript: &Path, received: &Path) -> String {
    format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.{id}]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\n",
        bin = bin("hutan-acp-fixture"),
    )
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

struct S {
    daemon: TestDaemon,
    work: tempfile::TempDir,
    client: Client,
    events: tokio::sync::broadcast::Receiver<(hutan_core::Seq, Event)>,
}

impl S {
    async fn start_with(work: tempfile::TempDir, toml: impl FnOnce(&Path) -> String) -> Self {
        let toml = toml(work.path());
        let daemon = TestDaemon::start_with(&toml, |_| {}).await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    /// A harness daemon whose `id` replays the transcripts `spawns`, one per start.
    async fn harness(id: &'static str, spawns: &[Vec<Value>]) -> Self {
        let work = tempfile::tempdir().unwrap();
        let scripted = script(work.path(), spawns);
        Self::start_with(work, |dir| {
            harness_toml(id, &scripted, &dir.join("received.jsonl"))
        })
        .await
    }

    /// A folder inside the test's temporary directory, canonical.
    fn folder(&self, name: &str) -> PathBuf {
        let dir = self.work.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn cwd(&self) -> PathBuf {
        let dir = self.work.path().join("project");
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn request(&self, agent: &str, extra_dirs: &[&Path]) -> ChatCreate {
        ChatCreate {
            extra_dirs: extra_dirs.iter().map(|dir| dir.to_path_buf()).collect(),
            contain: None,
            workspace_id: None,
            cwd: self.cwd(),
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

    async fn create(&self, agent: &str, extra_dirs: &[&Path]) -> SessionInfo {
        self.client
            .chat_create(self.request(agent, extra_dirs))
            .await
            .unwrap()
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

    /// The text of every `session/prompt` the fake agent received.
    fn prompts(&self) -> Vec<String> {
        self.received_of("session/prompt")
            .iter()
            .map(|msg| {
                msg["params"]["prompt"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
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

    async fn restart(&self, id: SessionId) -> SessionInfo {
        self.client
            .call::<SessionResult>(Request::SessionRestart(SessionRef { session_id: id }))
            .await
            .unwrap();
        self.wait_state(id, SessionState::Idle).await
    }

    async fn stop_daemon(self) {
        self.daemon.stop().await;
    }
}

fn dirs_of(session: &SessionInfo) -> Vec<(PathBuf, ChatDirRole, ChatDirApplied)> {
    session
        .chat
        .as_ref()
        .map(|chat| &chat.dirs)
        .unwrap_or(&session.extra_dirs)
        .iter()
        .map(|dir| (dir.path.clone(), dir.role, dir.applied))
        .collect()
}

fn dir(
    path: &Path,
    role: ChatDirRole,
    applied: ChatDirApplied,
) -> (PathBuf, ChatDirRole, ChatDirApplied) {
    (path.to_path_buf(), role, applied)
}

use ChatDirApplied::{NextTurn, Now, Restart};
use ChatDirRole::{Extra, Primary};

// Claude Code ------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn claude_gets_additional_directories_at_every_start_and_later_folders_wait_for_a_restart() {
    let s = S::harness(
        "claude",
        &[
            fixture_lines("dirs-claude-new"),
            fixture_lines("dirs-claude-load"),
        ],
    )
    .await;
    let (a, b) = (s.folder("a"), s.folder("b"));
    let created = s.create("claude", &[&a]).await;
    assert_eq!(
        s.received_of("session/new")[0]["params"]["additionalDirectories"],
        json!([a])
    );
    let cwd = s.cwd();
    assert_eq!(
        dirs_of(&created),
        [dir(&cwd, Primary, Now), dir(&a, Extra, Now)]
    );

    // ACP defines no mid-session change: the folder is kept and says what it needs.
    let added = s.client.chat_add_dir(created.id, &b).await.unwrap();
    assert_eq!(
        dirs_of(&added),
        [
            dir(&cwd, Primary, Now),
            dir(&a, Extra, Now),
            dir(&b, Extra, Restart)
        ]
    );
    assert!(
        s.received_of("session/prompt").is_empty(),
        "nothing is typed to Claude"
    );

    // Stopped, nothing runs: every extra folder waits for the next start.
    let stopped = stop(&s.client, created.id).await.unwrap();
    assert_eq!(stopped.state, SessionState::Stopped);
    assert_eq!(
        dirs_of(&s.session(created.id).await),
        [
            dir(&cwd, Primary, Now),
            dir(&a, Extra, Restart),
            dir(&b, Extra, Restart)
        ]
    );
    // The restart resends the whole list on `session/load` (ACP: omitted means none).
    let restarted = s.restart(created.id).await;
    assert_eq!(
        s.received_of("session/load")[0]["params"]["additionalDirectories"],
        json!([a, b])
    );
    assert_eq!(
        dirs_of(&restarted),
        [
            dir(&cwd, Primary, Now),
            dir(&a, Extra, Now),
            dir(&b, Extra, Now)
        ]
    );
    s.stop_daemon().await;
}

// OMP ----------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn omp_is_given_folders_with_local_commands_and_takes_a_change_between_turns() {
    let head = fixture_lines("dirs-omp-new")[..4].to_vec();
    let work = tempfile::tempdir().unwrap();
    let (a, b) = {
        let a = work.path().join("a");
        let b = work.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        (a.canonicalize().unwrap(), b.canonicalize().unwrap())
    };
    let mut spawn = head;
    spawn.extend(local(&format!("Added {}.", a.display())));
    spawn.extend(local(&format!("Added {}.", b.display())));
    spawn.extend(local(&format!("Removed {}.", a.display())));
    let scripted = script(work.path(), &[spawn]);
    let s = S::start_with(work, |dir| {
        harness_toml("omp", &scripted, &dir.join("received.jsonl"))
    })
    .await;
    let cwd = s.cwd();
    let created = s.create("omp", &[&a]).await;
    assert_eq!(s.prompts(), [format!("/add-dir {}", a.display())]);
    assert_eq!(
        dirs_of(&created),
        [dir(&cwd, Primary, Now), dir(&a, Extra, Now)]
    );

    let added = s.client.chat_add_dir(created.id, &b).await.unwrap();
    assert_eq!(
        dirs_of(&added),
        [
            dir(&cwd, Primary, Now),
            dir(&a, Extra, Now),
            dir(&b, Extra, Now)
        ]
    );
    let removed = s.client.chat_remove_dir(created.id, &a).await.unwrap();
    assert_eq!(
        dirs_of(&removed),
        [dir(&cwd, Primary, Now), dir(&b, Extra, Now)]
    );
    assert_eq!(
        s.prompts(),
        [
            format!("/add-dir {}", a.display()),
            format!("/add-dir {}", b.display()),
            format!("/remove-dir {}", a.display()),
        ]
    );
    // The commands are not turns: the journal has no message of theirs.
    let history: hutan_protocol::chat::ChatHistory = s
        .client
        .call(Request::ChatHistory(ChatHistoryRequest {
            session_id: created.id,
            before_seq: None,
            limit: 100,
        }))
        .await
        .unwrap();
    assert!(
        !history.events.iter().any(|event| matches!(
            event.kind,
            AgentEventKind::MessageStart { .. } | AgentEventKind::MessageEnd { .. }
        )),
        "{:?}",
        history.events
    );
    s.stop_daemon().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_taken_out_while_omp_was_stopped_is_taken_out_of_its_session_at_the_next_start() {
    let head = fixture_lines("dirs-omp-new")[..4].to_vec();
    let work = tempfile::tempdir().unwrap();
    let a = work.path().join("a");
    std::fs::create_dir_all(&a).unwrap();
    let a = a.canonicalize().unwrap();
    let mut first = head;
    first.extend(local(&format!("Added {}.", a.display())));
    // The second start resumes the session: OMP lists the folder it kept; Hutan takes it out.
    let load = fixture_lines("load");
    let mut second = vec![
        load[0].clone(),
        load[1].clone(),
        load[2].clone(),
        load[5].clone(),
    ];
    second.extend(local(&format!(
        "Workspace directories:\n  {} (working directory)\n  {}",
        work.path().join("project").display(),
        a.display()
    )));
    second.extend(local(&format!("Removed {}.", a.display())));
    let scripted = script(work.path(), &[first, second]);
    let s = S::start_with(work, |dir| {
        harness_toml("omp", &scripted, &dir.join("received.jsonl"))
    })
    .await;
    let created = s.create("omp", &[&a]).await;
    stop(&s.client, created.id).await.unwrap();
    let removed = s.client.chat_remove_dir(created.id, &a).await.unwrap();
    assert_eq!(
        dirs_of(&removed).len(),
        1,
        "only the session's own folder is left"
    );
    s.restart(created.id).await;
    assert_eq!(
        s.prompts(),
        [
            format!("/add-dir {}", a.display()),
            "/dirs".to_owned(),
            format!("/remove-dir {}", a.display()),
        ]
    );
    s.stop_daemon().await;
}

// Codex ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn codex_gets_writable_roots_with_the_next_turn() {
    let work = tempfile::tempdir().unwrap();
    let toml = format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[codex]\nbin = {codex:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"app-server\"]\n",
        codex = bin("hutan-codex-fixture"),
        received = work.path().join("received.jsonl"),
        transcript = agents_fixtures().join("codex-0.162/plain-turn.jsonl"),
    );
    let mut s = S::start_with(work, |_| toml).await;
    let a = s.folder("a");
    let cwd = s.cwd();
    let created = s.create("codex", &[&a]).await;
    // Nothing is claimed before a turn carried it.
    assert_eq!(
        dirs_of(&created),
        [dir(&cwd, Primary, Now), dir(&a, Extra, NextTurn)]
    );
    s.client.chat_send(created.id, "hello").await.unwrap();
    wait_event(&mut s.events, "the turn to end", |event| match event {
        Event::ChatEvent { event, .. } if event.kind.ends_turn() => Some(()),
        _ => None,
    })
    .await;
    let turns = s.received_of("turn/start");
    assert_eq!(
        turns[0]["params"]["sandboxPolicy"],
        json!({"type": "workspaceWrite", "writableRoots": [a]})
    );
    let idle = s.wait_state(created.id, SessionState::Idle).await;
    assert_eq!(
        dirs_of(&idle),
        [dir(&cwd, Primary, Now), dir(&a, Extra, Now)]
    );
    s.stop_daemon().await;
}

// Who may ask ----------------------------------------------------------------------------------------

/// A connection that says it runs inside `session`, whose agent has reported itself.
async fn agent_in(s: &S, session: SessionId) -> Client {
    let reporter = s.daemon.connect().await;
    let applied: AgentApplied = reporter
        .call(Request::AgentReport(AgentReportRequest {
            session_id: session,
            source: "test-hooks".into(),
            agent: "fake".into(),
            state: Some(AgentState::Working),
            message: None,
            seq: 1,
            agent_session_id: None,
            resume_argv: None,
            pid: None,
            node: None,
        }))
        .await
        .unwrap();
    assert!(applied.applied);
    let mut options = s.daemon.options();
    options.client.origin = Some(Origin {
        session_id: session,
    });
    Client::connect(options).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_may_not_add_or_give_folders_under_any_policy() {
    let work = tempfile::tempdir().unwrap();
    let scripted = script(work.path(), &[fixture_lines("dirs-claude-new")]);
    // `allow`: the most permissive control policy; user-only requests are refused all the same.
    let s = S::start_with(work, |dir| {
        format!(
            "[control]\nagents = \"allow\"\n\n{}",
            harness_toml("claude", &scripted, &dir.join("received.jsonl"))
        )
    })
    .await;
    let a = s.folder("a");
    let shell = create(&s.client, common::sh("sleep 60")).await;
    let agent = agent_in(&s, shell.id).await;
    let chat = s.create("claude", &[]).await;

    for result in [
        agent.chat_add_dir(chat.id, &a).await.map(|_| ()),
        agent.chat_remove_dir(chat.id, &a).await.map(|_| ()),
        agent
            .chat_create(s.request("claude", &[&a]))
            .await
            .map(|_| ()),
    ] {
        let error = match result {
            Err(hutan_client::ClientError::Remote(error)) => error,
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert_eq!(error.code, ErrorCode::Forbidden, "{error:?}");
        assert!(
            error.message.contains("only available to the user"),
            "{error:?}"
        );
    }
    let mut terminal = common::sh("sleep 60");
    terminal.extra_dirs = vec![a.clone()];
    let refused = agent
        .call::<SessionResult>(Request::SessionCreate(SessionCreate {
            workspace_id: None,
            spec: SessionSpec::Terminal(terminal),
        }))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::Forbidden);
    // Nothing changed.
    assert_eq!(dirs_of(&s.session(chat.id).await).len(), 1);

    // The user may.
    let added = s.client.chat_add_dir(chat.id, &a).await.unwrap();
    assert_eq!(dirs_of(&added).len(), 2);
    s.stop_daemon().await;
}

// What a folder must be --------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_must_be_an_existing_absolute_directory_that_is_not_the_sessions_own_or_a_duplicate()
 {
    let s = S::harness("claude", &[fixture_lines("dirs-claude-new")]).await;
    let a = s.folder("a");
    let file = s.work.path().join("file.txt");
    std::fs::write(&file, "x").unwrap();
    let cwd = s.cwd();
    let data = s.daemon.paths.base.clone();

    let bad = |result: Result<SessionInfo, hutan_client::ClientError>, needle: &str| match result {
        Err(hutan_client::ClientError::Remote(error)) => {
            assert_eq!(error.code, ErrorCode::BadRequest, "{error:?}");
            assert!(error.message.contains(needle), "{needle}: {error:?}");
        }
        other => panic!("expected bad_request ({needle}), got {other:?}"),
    };
    // At creation.
    for (dirs, needle) in [
        (vec![PathBuf::from("relative")], "not an absolute path"),
        (
            vec![s.work.path().join("missing")],
            "not an existing directory",
        ),
        (vec![file.clone()], "not an existing directory"),
        (vec![cwd.clone()], "the session's own folder"),
        (
            vec![a.clone(), a.clone()],
            "already one of the session's folders",
        ),
        (vec![data.clone()], "Hutan's own data folder"),
        (
            (0..9).map(|n| s.folder(&format!("many{n}"))).collect(),
            "at most 8",
        ),
    ] {
        let refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
        bad(
            s.client.chat_create(s.request("claude", &refs)).await,
            needle,
        );
    }
    // A direct chat has no tools to give folders to.
    let mut direct = s.request("direct", &[&a]);
    direct.provider_id = Some("none".into());
    bad(s.client.chat_create(direct).await, "no tools");

    // And while the session lives.
    let chat = s.create("claude", &[&a]).await;
    bad(
        s.client.chat_add_dir(chat.id, &a).await,
        "already one of the session's folders",
    );
    bad(
        s.client.chat_add_dir(chat.id, &cwd).await,
        "the session's own folder",
    );
    bad(
        s.client.chat_add_dir(chat.id, &data).await,
        "Hutan's own data folder",
    );
    bad(s.client.chat_remove_dir(chat.id, &cwd).await, "own folder");
    assert_eq!(
        remote_code(s.client.chat_remove_dir(chat.id, &s.folder("b")).await),
        ErrorCode::NotFound
    );
    // Eight extra folders at most.
    for n in 0..7 {
        s.client
            .chat_add_dir(chat.id, s.folder(&format!("more{n}")))
            .await
            .unwrap();
    }
    assert_eq!(
        dirs_of(&s.session(chat.id).await).len(),
        9,
        "the own folder and eight extras"
    );
    assert_eq!(
        remote_code(
            s.client
                .chat_add_dir(chat.id, s.folder("one-too-many"))
                .await
        ),
        ErrorCode::InvalidState
    );
    s.stop_daemon().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_in_the_read_deny_set_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let secret = work.path().join("secrets");
    let inside = secret.join("keys");
    std::fs::create_dir_all(&inside).unwrap();
    let scripted = script(work.path(), &[fixture_lines("dirs-claude-new")]);
    let secret = secret.canonicalize().unwrap();
    let s = S::start_with(work, |dir| {
        format!(
            "[sandbox]\nread_deny = [{secret:?}]\n\n{}",
            harness_toml("claude", &scripted, &dir.join("received.jsonl"))
        )
    })
    .await;
    for path in [secret.clone(), secret.join("keys")] {
        let refused = s.client.chat_create(s.request("claude", &[&path])).await;
        match refused {
            Err(hutan_client::ClientError::Remote(error)) => {
                assert_eq!(error.code, ErrorCode::BadRequest);
                assert!(error.message.contains("read_deny"), "{error:?}");
            }
            other => panic!("expected bad_request, got {other:?}"),
        }
    }
    s.stop_daemon().await;
}

// The file service -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_file_service_reads_a_session_folder_while_the_session_has_it() {
    let s = S::harness("claude", &[fixture_lines("dirs-claude-new")]).await;
    let a = s.folder("a");
    std::fs::write(a.join("notes.txt"), "from the extra folder\n").unwrap();
    let elsewhere = s.folder("elsewhere");
    std::fs::write(elsewhere.join("notes.txt"), "not a root\n").unwrap();

    // Not a root before anything names it.
    let refused = s.client.fs_read_all(&a.join("notes.txt"), false).await;
    assert_eq!(remote_code(refused), ErrorCode::Forbidden);

    let chat = s.create("claude", &[]).await;
    s.client.chat_add_dir(chat.id, &a).await.unwrap();
    let text = s
        .client
        .fs_read_all(&a.join("notes.txt"), false)
        .await
        .unwrap();
    assert_eq!(text.text, "from the extra folder\n");
    // Only the folder that was added.
    assert_eq!(
        remote_code(
            s.client
                .fs_read_all(&elsewhere.join("notes.txt"), false)
                .await
        ),
        ErrorCode::Forbidden
    );

    // Refused again after the removal.
    s.client.chat_remove_dir(chat.id, &a).await.unwrap();
    assert_eq!(
        remote_code(s.client.fs_read_all(&a.join("notes.txt"), false).await),
        ErrorCode::Forbidden
    );

    // And when the session ends, the folders go with it.
    s.client.chat_add_dir(chat.id, &a).await.unwrap();
    assert!(
        s.client
            .fs_read_all(&a.join("notes.txt"), false)
            .await
            .is_ok()
    );
    stop(&s.client, chat.id).await.unwrap();
    assert_eq!(
        remote_code(s.client.fs_read_all(&a.join("notes.txt"), false).await),
        ErrorCode::Forbidden
    );
    s.stop_daemon().await;
}

// Terminals ----------------------------------------------------------------------------------------------

/// A fake agent CLI that logs its arguments and then waits.
fn fake_cli(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn a_known_agent_in_a_terminal_is_started_with_add_dir_for_each_folder() {
    let t = TestDaemon::start_with("[terminal]\nenv_inherit = \"daemon\"\n", |_| {}).await;
    let client = t.connect().await;
    let work = tempfile::tempdir().unwrap();
    let a = work.path().join("a with space");
    let b = work.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (a, b) = (a.canonicalize().unwrap(), b.canonicalize().unwrap());
    let log = work.path().join("argv.log");
    let claude = fake_cli(
        work.path(),
        "claude",
        &format!("printf '%s\\n' \"$@\" > {log:?}\nexec sleep 60"),
    );
    let mut spec = common::spec(&[claude.to_str().unwrap(), "--resume", "x"]);
    spec.cwd = Some(work.path().canonicalize().unwrap());
    spec.extra_dirs = vec![a.clone(), b.clone()];
    let session = create(&client, spec).await;
    timeout(WAIT, async {
        while !log.exists() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // The `=` form: Claude Code's option is variadic and would take what follows as a folder.
    let argv = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        argv,
        format!(
            "--add-dir={}\n--add-dir={}\n--resume\nx\n",
            a.display(),
            b.display()
        )
    );
    assert_eq!(
        dirs_of(&session)
            .into_iter()
            .map(|(path, role, _)| (path, role))
            .collect::<Vec<_>>(),
        [(a.clone(), Extra), (b.clone(), Extra)]
    );
    // The folders are listed with how the running process holds them.
    let listed = client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap()
        .sessions
        .into_iter()
        .find(|s| s.id == session.id)
        .unwrap();
    assert!(
        listed.extra_dirs.iter().all(|d: &ChatDir| d.applied == Now),
        "{listed:?}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_terminal_agent_is_typed_add_dir_without_enter_for_the_user_to_confirm() {
    let t = TestDaemon::start_with("[terminal]\nenv_inherit = \"daemon\"\n", |_| {}).await;
    let client = t.connect().await;
    let work = tempfile::tempdir().unwrap();
    let a = work.path().join("a");
    std::fs::create_dir_all(&a).unwrap();
    let a = a.canonicalize().unwrap();
    let typed = format!("/add-dir {}", a.display());
    let log = work.path().join("typed.log");
    // OMP's own status mark names the agent; then the terminal is raw and the program keeps
    // exactly the bytes typed at it.
    let script = format!(
        "printf '\\033]7501;state=idle:app=omp\\033\\\\'; stty raw -echo; dd bs=1 count={} of={log:?} 2>/dev/null; exec sleep 60",
        typed.len()
    );
    let mut spec = common::sh(&script);
    // A folder next to the session's own, not inside it.
    let project = work.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    spec.cwd = Some(project.canonicalize().unwrap());
    let session = create(&client, spec).await;
    timeout(WAIT, async {
        loop {
            let listed = client
                .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
                .await
                .unwrap()
                .sessions
                .into_iter()
                .find(|s| s.id == session.id)
                .unwrap();
            if listed
                .terminal_agent
                .as_ref()
                .is_some_and(|a| a.app == "omp")
            {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the agent is recognised");

    let added = client.chat_add_dir(session.id, &a).await.unwrap();
    assert_eq!(
        added
            .extra_dirs
            .iter()
            .map(|d| (d.path.clone(), d.applied))
            .collect::<Vec<_>>(),
        [(a.clone(), ChatDirApplied::Paste)]
    );
    timeout(WAIT, async {
        while std::fs::read_to_string(&log).unwrap_or_default().len() < typed.len() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the program received the typed text");
    // Exactly the command: no Enter, so the agent waits for the user.
    assert_eq!(std::fs::read_to_string(&log).unwrap(), typed);

    // A folder given to the user's own file service while the session lives.
    std::fs::write(a.join("f.txt"), "hi").unwrap();
    assert_eq!(
        client
            .fs_read_all(&a.join("f.txt"), false)
            .await
            .unwrap()
            .text,
        "hi"
    );
    let removed = client.chat_remove_dir(session.id, &a).await.unwrap();
    assert!(removed.extra_dirs.is_empty());
    assert_eq!(
        remote_code(client.fs_read_all(&a.join("f.txt"), false).await),
        ErrorCode::Forbidden
    );
    stop(&client, session.id).await.unwrap();
    t.stop().await;
}
