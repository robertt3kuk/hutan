//! `hutan agent report | release | hook` against a real daemon: what the hooks of Codex,
//! Claude Code, and the OMP extension run inside a Hutan session.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common;

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::{HUTAN, TestDaemon, WAIT, command, finish};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::time::{sleep, timeout};

/// A `hutan` invocation as an agent hook would make it: `HUTAN_SESSION_ID` set (or not).
fn agent_command(socket: &Path, session: Option<&str>, args: &[&str]) -> tokio::process::Command {
    let mut command = command(Path::new(HUTAN), Some(socket), args);
    match session {
        Some(id) => command.env("HUTAN_SESSION_ID", id),
        None => command.env_remove("HUTAN_SESSION_ID"),
    };
    command
}

async fn agent_of(t: &TestDaemon, id: &str) -> Value {
    t.session(id)
        .await
        .get("agent")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Polls `hutan ls` until the session's `agent.state` is `state`.
async fn wait_agent_state(t: &TestDaemon, id: &str, state: &str) -> Value {
    timeout(WAIT, async {
        loop {
            let agent = agent_of(t, id).await;
            if agent["state"] == state {
                return agent;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("session {id} never reported {state}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn report_and_release_show_up_in_ls_json() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    assert_eq!(
        agent_of(&t, &id).await,
        Value::Null,
        "no agent before a report"
    );

    let out = finish(agent_command(
        &t.paths.socket,
        Some(&id),
        &[
            "agent",
            "report",
            "--source",
            "test-hooks",
            "--agent",
            "omp",
            "--state",
            "working",
            "--seq",
            "10",
            "--message",
            "thinking",
            "--agent-session-id",
            "abc-123",
            "--",
            "omp",
            "--resume=abc-123",
        ],
    ))
    .await;
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
    let agent = wait_agent_state(&t, &id, "working").await;
    assert_eq!(agent["source"], "test-hooks");
    assert_eq!(agent["agent"], "omp");
    assert_eq!(agent["message"], "thinking");
    assert_eq!(agent["agent_session_id"], "abc-123");
    assert!(agent["updated_at_ms"].as_u64().unwrap() > 0);

    // The default sequence is the clock: a later report without `--seq` wins.
    finish(agent_command(
        &t.paths.socket,
        Some(&id),
        &[
            "agent",
            "report",
            "--source",
            "test-hooks",
            "--agent",
            "omp",
            "--state",
            "blocked",
        ],
    ))
    .await
    .ok();
    let agent = wait_agent_state(&t, &id, "blocked").await;
    assert_eq!(agent["agent_session_id"], "abc-123");

    // An explicit seq that is not newer is ignored, still exit 0.
    let stale = finish(agent_command(
        &t.paths.socket,
        Some(&id),
        &[
            "agent",
            "report",
            "--source",
            "test-hooks",
            "--agent",
            "omp",
            "--state",
            "idle",
            "--seq",
            "5",
        ],
    ))
    .await;
    assert_eq!(stale.code, 0);
    sleep(Duration::from_millis(200)).await;
    assert_eq!(agent_of(&t, &id).await["state"], "blocked");

    let out = finish(agent_command(
        &t.paths.socket,
        Some(&id),
        &["agent", "release", "--source", "test-hooks"],
    ))
    .await;
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
    timeout(WAIT, async {
        while agent_of(&t, &id).await != Value::Null {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the release never arrived");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_reports_itself_through_the_environment_the_daemon_gave_it() {
    let t = TestDaemon::start().await;
    // No flags for the socket or the session: HUTAN_BIN, HUTAN_SESSION_ID, and HUTAN_SOCKET
    // are all the process has.
    let script = r#"[ "$HUTAN_ENV" = 1 ] || exit 3
"$HUTAN_BIN" agent report --source self --agent fake --state working --message "from inside"
exec sleep 600"#;
    let id = t.new_session(&[], &["sh", "-c", script]).await;
    let agent = wait_agent_state(&t, &id, "working").await;
    assert_eq!(agent["message"], "from inside");
    assert_eq!(agent["source"], "self");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn outside_hutan_or_without_a_daemon_the_agent_commands_do_nothing_quietly() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let report = [
        "agent", "report", "--source", "x", "--agent", "omp", "--state", "idle",
    ];

    // Not inside a Hutan session.
    let out = finish(agent_command(&t.paths.socket, None, &report)).await;
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
    let out = finish(agent_command(
        &t.paths.socket,
        None,
        &["agent", "release", "--source", "x"],
    ))
    .await;
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );

    // A session the daemon does not know: a daemon error, still silent and 0.
    let out = finish(agent_command(
        &t.paths.socket,
        Some("01K7F3B5N2Q9W8E4R6T1Y0V3KM"),
        &report,
    ))
    .await;
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
    // Not even an id.
    let out = finish(agent_command(&t.paths.socket, Some("not-an-id"), &report)).await;
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
    sleep(Duration::from_millis(200)).await;
    assert_eq!(agent_of(&t, &id).await, Value::Null);

    // No daemon at all.
    let socket = t.paths.socket.clone();
    t.stop().await;
    let started = Instant::now();
    let out = finish(agent_command(&socket, Some(&id), &report)).await;
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "an unreachable daemon must not hold the agent up: {:?}",
        started.elapsed()
    );
}

/// Runs `hutan agent hook <agent>` with `input` on stdin.
async fn hook(
    socket: &Path,
    session: &str,
    agent: &str,
    input: &Value,
    extra_env: &[(&str, &str)],
) -> common::Out {
    let mut command = agent_command(socket, Some(session), &["agent", "hook", agent]);
    command.stdin(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.to_string().as_bytes()).await.unwrap();
    drop(stdin);
    let output = timeout(WAIT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    common::Out {
        code: output.status.code().unwrap(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_hook_events_drive_the_session_state() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let go = |event: Value| {
        let socket = t.paths.socket.clone();
        let id = id.clone();
        async move {
            let out = hook(&socket, &id, "claude", &event, &[]).await;
            assert_eq!(
                (out.code, out.stdout.as_str(), out.stderr.as_str()),
                (0, "", "")
            );
        }
    };

    go(json!({"hook_event_name": "SessionStart", "session_id": "conv-1", "source": "startup"}))
        .await;
    let agent = wait_agent_state(&t, &id, "idle").await;
    assert_eq!(agent["source"], "claude-hooks");
    assert_eq!(agent["agent"], "claude");
    assert_eq!(agent["agent_session_id"], "conv-1");

    go(json!({"hook_event_name": "UserPromptSubmit", "session_id": "conv-1", "prompt": "hi"}))
        .await;
    wait_agent_state(&t, &id, "working").await;
    // A subagent is a node of its own: its permission prompt blocks the session (the summary
    // names it) without touching the main agent's state, and its tool hooks clear it again.
    let sub = |event: &str| {
        json!({"hook_event_name": event, "session_id": "conv-1", "agent_id": "agent-abc123",
               "agent_type": "Explore", "tool_name": "Bash"})
    };
    go(sub("SubagentStart")).await;
    go(sub("PermissionRequest")).await;
    let agent = wait_agent_state(&t, &id, "blocked").await;
    assert_eq!(agent["blocked_sub"], "Explore");
    assert_eq!(
        agent.get("message"),
        None,
        "the message is the main agent's"
    );
    go(sub("PostToolUse")).await;
    let agent = wait_agent_state(&t, &id, "working").await;
    assert_eq!(agent.get("blocked_sub"), None);
    go(sub("SubagentStop")).await;
    go(json!({"hook_event_name": "PermissionRequest", "session_id": "conv-1", "tool_name": "Bash"})).await;
    let agent = wait_agent_state(&t, &id, "blocked").await;
    assert_eq!(agent["message"], "permission: Bash");
    go(json!({"hook_event_name": "Stop", "session_id": "conv-1"})).await;
    let agent = wait_agent_state(&t, &id, "idle").await;
    assert_eq!(agent.get("message"), None);

    // Events that mean nothing, and input that is not JSON, are quietly dropped.
    go(json!({"hook_event_name": "Notification", "session_id": "conv-1"})).await;
    let out = {
        let mut command = agent_command(&t.paths.socket, Some(&id), &["agent", "hook", "claude"]);
        command.stdin(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"{ not json").await.unwrap();
        drop(stdin);
        child.wait().await.unwrap()
    };
    assert_eq!(out.code(), Some(0));

    go(json!({"hook_event_name": "SessionEnd", "session_id": "conv-1", "reason": "other"})).await;
    timeout(WAIT, async {
        while agent_of(&t, &id).await != Value::Null {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("SessionEnd never released the report");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_hook_events_report_and_ignore_a_nested_codex() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let start = json!({"hook_event_name": "SessionStart", "session_id": "thread-9"});
    // Another Codex (a different thread) started inside this one: its hooks are not ours.
    let out = hook(
        &t.paths.socket,
        &id,
        "codex",
        &start,
        &[("CODEX_THREAD_ID", "outer-thread")],
    )
    .await;
    assert_eq!(out.code, 0);
    sleep(Duration::from_millis(300)).await;
    assert_eq!(agent_of(&t, &id).await, Value::Null);

    let out = hook(
        &t.paths.socket,
        &id,
        "codex",
        &start,
        &[("CODEX_THREAD_ID", "thread-9")],
    )
    .await;
    assert_eq!(out.code, 0);
    let agent = wait_agent_state(&t, &id, "idle").await;
    assert_eq!(
        (agent["source"].as_str(), agent["agent"].as_str()),
        (Some("codex-hooks"), Some("codex"))
    );
    assert_eq!(agent["agent_session_id"], "thread-9");
    t.stop().await;
}

/// The agents board (protocol 0.63): `hutan agents --board` reads `agents.board`, the reducer
/// of the Agents page over the daemon's state, and says where each row's facts come from.
#[tokio::test(flavor = "multi_thread")]
async fn the_board_lists_agents_and_says_where_their_facts_come_from() {
    let t = TestDaemon::start().await;
    let asking = t
        .new_session(
            &[],
            &[
                "sh",
                "-c",
                r#"[ "$HUTAN_ENV" = 1 ] || exit 3
"$HUTAN_BIN" agent report --source self --agent fake --state blocked --message "Run the migration?"
exec sleep 600"#,
            ],
        )
        .await;
    wait_agent_state(&t, &asking, "blocked").await;
    // A plain shell is no agent; an agent whose session ended is finished.
    t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let finished = t
        .new_session(
            &[],
            &[
                "sh",
                "-c",
                r#""$HUTAN_BIN" agent report --source done --agent fake --state idle; exit 0"#,
            ],
        )
        .await;

    // Waits on state: the finished session is counted once its process has ended.
    let board = timeout(WAIT, async {
        loop {
            let board = t.hutan(&["--json", "agents", "--board"]).await.ok().json();
            if board["counts"]["finished"] == 1 {
                return board;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the ended agent never counted as finished");

    let agents = board["agents"].as_array().unwrap();
    assert_eq!(
        agents.len(),
        1,
        "the shell is no agent and the finished one is hidden: {board}"
    );
    let row = &agents[0];
    assert_eq!(row["session_id"], asking);
    assert_eq!(
        (row["state"].as_str(), row["group"].as_str()),
        (Some("blocked"), Some("requests"))
    );
    assert_eq!(row["source"], "hook");
    assert_eq!(row["kind"], "terminal");
    assert_eq!(row["waiting"][0]["kind"], "terminal_prompt");
    assert!(
        row["waiting"][0]["title"]
            .as_str()
            .unwrap()
            .contains("Run the migration?")
    );
    assert_eq!(board["counts"]["agents"], 2);
    assert_eq!(board["counts"]["requests"], 1);
    assert_eq!(board["hidden_finished"], 1);
    assert_eq!(board["coverage"]["terminal_reported"], 2);
    assert_eq!(board["coverage"]["chats"], 0);

    // The text says the same, in the words of the Agents page.
    let text = t.hutan(&["agents", "--board"]).await.ok().stdout;
    for needle in [
        "Needs you",
        "Run the migration?",
        "terminal (state only)",
        "1 finished not listed",
    ] {
        assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
    }

    // `--finished` lists the finished one too, as completed.
    let all = t
        .hutan(&["--json", "agents", "--board", "--finished"])
        .await
        .ok()
        .json();
    let done = all["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["session_id"] == finished)
        .expect("listed with --finished");
    assert_eq!(
        (done["state"].as_str(), done["group"].as_str()),
        (Some("completed"), Some("finished"))
    );
    assert_eq!(all["hidden_finished"], serde_json::Value::Null);

    // An unknown workspace is an error, not an empty board.
    let missing = finish(command(
        Path::new(HUTAN),
        Some(&t.paths.socket),
        &["agents", "--board", "--ws", "no-such-workspace"],
    ))
    .await;
    assert_ne!(missing.code, 0, "{}", missing.stderr);
    t.stop().await;
}
