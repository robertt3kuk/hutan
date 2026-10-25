//! `hutan hooks list|test|log` (protocol 0.49, `docs/integrations.md` § User hooks) against a
//! real daemon whose `daemon.toml` holds `[[hooks]]`, and the same commands typed inside an
//! agent's session, where they are refused.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common;

use std::path::Path;
use std::time::Duration;

use common::{HUTAN, TestDaemon, WAIT, command, finish};
use serde_json::Value;
use tokio::time::{sleep, timeout};

const HOOKS: &str = r#"
[[hooks]]
name = "quiet"
on = "session.exited"
run = ["/bin/sh", "-c", "cat > /dev/null; echo \"socket=$HUTAN_SOCKET\"; echo \"event=$HUTAN_EVENT\""]
when_focused = false
harness = "claude"

[[hooks]]
on = "turn.finished"
run = ["/bin/sh", "-c", "echo broke token=abcdefghijklmnopqrstuvwxyz0123456789 >&2; exit 4"]
timeout = 5

[[hooks]]
on = "session.exited"
shell = true
run = ["echo exited $HUTAN_EVENT_EXIT_CODE"]
"#;

async fn with_hooks() -> TestDaemon {
    TestDaemon::start_toml(HOOKS, |_| {}).await
}

async fn wait_runs(t: &TestDaemon, index: usize, runs: u64) -> Value {
    timeout(WAIT, async {
        loop {
            let list = t.hutan(&["hooks", "list", "--json"]).await.ok().json();
            if list["hooks"][index]["runs"].as_u64().unwrap() >= runs {
                return list;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the hook never ran")
}

#[tokio::test(flavor = "multi_thread")]
async fn list_says_what_there_is_and_that_nothing_is_configured() {
    let t = TestDaemon::start().await;
    let json = t.hutan(&["hooks", "list", "--json"]).await.ok().json();
    assert_eq!(json["hooks"].as_array().unwrap().len(), 0);
    assert_eq!(json["max_concurrent"], 4);
    let text = t.hutan(&["hooks", "list"]).await.ok().stdout;
    assert!(text.starts_with("no hooks"), "{text}");
    let out = t.hutan(&["hooks", "test", "1"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("no hooks"), "{out:?}");
    t.stop().await;

    let t = with_hooks().await;
    let json = t.hutan(&["hooks", "list", "--json"]).await.ok().json();
    let hooks = json["hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), 3);
    assert_eq!(hooks[0]["name"], "quiet");
    assert_eq!(hooks[0]["on"], "session.exited");
    assert_eq!(hooks[0]["harness"], "claude");
    assert_eq!(hooks[0]["when_focused"], false);
    assert_eq!(hooks[1]["timeout_secs"], 5);
    assert_eq!(hooks[2]["shell"], true);
    assert_eq!(hooks[2]["timeout_secs"], 30, "the default timeout");
    assert_eq!(hooks[2]["runs"], 0);
    assert!(hooks[2].get("last_run").is_none());
    let text = t.hutan(&["hooks", "list"]).await.ok().stdout;
    assert!(text.contains("session.exited (quiet)"), "{text}");
    assert!(text.contains("harness=claude unfocused only"), "{text}");
    assert!(text.contains("never"), "{text}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_runs_a_hook_by_number_or_name_and_exits_non_zero_when_it_fails() {
    let t = with_hooks().await;
    // By name: ignores the harness filter and `when_focused`, and sees the daemon's socket.
    let out = t.hutan(&["hooks", "test", "quiet"]).await.ok();
    assert!(
        out.stdout.contains("hook 1 (session.exited)"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("ok in"), "{}", out.stdout);
    assert!(
        out.stdout
            .contains(&format!("socket={}", t.paths.socket.display())),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("event=session.exited"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("\"test\": true"), "{}", out.stdout);

    let json = t.hutan(&["hooks", "test", "1", "--json"]).await.ok().json();
    assert_eq!(json["run"]["test"], true);
    assert_eq!(json["run"]["outcome"], "ok");
    assert_eq!(json["payload"]["event"], "session.exited");
    assert_eq!(json["payload"]["test"], true);

    // A hook that fails: printed, then exit 1; its stderr is masked.
    let out = t.hutan(&["hooks", "test", "2"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.contains("exit 4"), "{}", out.stdout);
    assert!(out.stdout.contains("broke"), "{}", out.stdout);
    assert!(!out.stdout.contains("abcdefghijkl"), "{}", out.stdout);

    let shell = t.hutan(&["hooks", "test", "3"]).await.ok();
    assert!(shell.stdout.contains("exited 1"), "{}", shell.stdout);

    for args in [
        &["hooks", "test", "0"][..],
        &["hooks", "test", "4"],
        &["hooks", "test", "nope"],
    ] {
        let out = t.hutan(args).await;
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn log_lists_the_runs_newest_first_with_their_output() {
    let t = with_hooks().await;
    assert!(
        t.hutan(&["hooks", "log"])
            .await
            .ok()
            .stdout
            .starts_with("no runs yet")
    );
    t.hutan(&["hooks", "test", "3"]).await.ok();
    t.hutan(&["hooks", "test", "2"]).await;
    let json = t.hutan(&["hooks", "log", "--json"]).await.ok().json();
    let runs = json["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["hook"], 1);
    assert_eq!(runs[0]["outcome"], "failed");
    assert_eq!(runs[0]["exit_code"], 4);
    assert_eq!(runs[1]["stdout"], "exited 1\n");
    assert!(runs[0]["id"].as_u64() > runs[1]["id"].as_u64());
    let only = t.hutan(&["hooks", "log", "3", "--json"]).await.ok().json();
    assert_eq!(only["runs"].as_array().unwrap().len(), 1);
    let limited = t
        .hutan(&["hooks", "log", "-n", "1", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(limited["runs"].as_array().unwrap().len(), 1);
    let text = t.hutan(&["hooks", "log"]).await.ok().stdout;
    assert!(
        text.contains("hook 2 turn.finished (test): exit 4"),
        "{text}"
    );
    assert!(text.contains("exited 1"), "{text}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_event_runs_the_hook_and_list_shows_the_run() {
    let t = with_hooks().await;
    t.new_session(&[], &["sh", "-c", "exit 7"]).await;
    let list = wait_runs(&t, 2, 1).await;
    let run = &list["hooks"][2]["last_run"];
    assert_eq!(run["outcome"], "ok");
    assert_eq!(run["stdout"], "exited 7\n");
    assert!(run.get("test").is_none());
    assert_eq!(list["hooks"][2]["failures"], 0);
    let text = t.hutan(&["hooks", "list"]).await.ok().stdout;
    assert!(text.contains("ok in"), "{text}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cli_typed_inside_an_agents_session_cannot_list_run_or_read_the_log() {
    let t = with_hooks().await;
    let session = t.new_session(&[], &["sh", "-c", "sleep 60"]).await;
    let mut report = command(
        Path::new(HUTAN),
        Some(&t.paths.socket),
        &[
            "agent", "report", "--source", "test", "--agent", "fake", "--state", "working",
            "--seq", "1",
        ],
    );
    report.env("HUTAN_SESSION_ID", &session);
    finish(report).await.ok();
    timeout(WAIT, async {
        loop {
            if t.session(&session).await.get("agent").is_some() {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the report never showed up");

    for args in [
        &["hooks", "list"][..],
        &["hooks", "test", "1"][..],
        &["hooks", "log"][..],
    ] {
        let mut agent = command(Path::new(HUTAN), Some(&t.paths.socket), args);
        agent.env("HUTAN_SESSION_ID", &session);
        let out = finish(agent).await;
        assert_eq!(out.code, 1, "{args:?}: {out:?}");
        assert!(
            out.stderr.contains("only available to the user"),
            "{args:?}: {out:?}"
        );
    }
    // Nothing ran: the user's own view has no run.
    let list = t.hutan(&["hooks", "list", "--json"]).await.ok().json();
    assert!(list["hooks"][1].get("last_run").is_none());
    t.stop().await;
}
