//! `hutan notify ls|ack|act|watch|mute|unmute` against a real daemon: the real `hutan` binary
//! over the real socket, intents raised by real PTY sessions and by the fake Codex app server
//! replaying a transcript recorded from codex 0.162.1.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use common::{HUTAN, TestDaemon, WAIT, codex_fixture_bin, command};
use hutan_agents::CodexConfig;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdout};
use tokio::time::{sleep, timeout};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/codex-0.162")
        .join(format!("{name}.jsonl"))
}

fn replay(transcript: &Path, received: &Path) -> CodexConfig {
    CodexConfig {
        program: codex_fixture_bin(),
        args: vec![
            "replay".to_owned(),
            "--received".to_owned(),
            received.display().to_string(),
            transcript.display().to_string(),
            "app-server".to_owned(),
        ],
        env: Vec::new(),
    }
}

/// `hutan notify watch --json` with its output read line by line.
struct Watch {
    child: Child,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
}

impl Watch {
    fn spawn(t: &TestDaemon) -> Self {
        let mut cmd = command(
            Path::new(HUTAN),
            Some(&t.paths.socket),
            &["notify", "watch", "--json"],
        );
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        Self {
            child,
            lines: BufReader::new(stdout).lines(),
        }
    }

    async fn next(&mut self) -> Value {
        let line = timeout(WAIT, self.lines.next_line())
            .await
            .expect("watch printed nothing")
            .unwrap()
            .expect("watch ended");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
    }
}

async fn intents(t: &TestDaemon) -> Vec<Value> {
    let out = t.hutan(&["notify", "ls", "--json"]).await.ok();
    out.json()["intents"].as_array().unwrap().clone()
}

async fn wait_for_intents(t: &TestDaemon, at_least: usize) -> Vec<Value> {
    timeout(WAIT, async {
        loop {
            let found = intents(t).await;
            if found.len() >= at_least {
                return found;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the intents never showed up")
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_is_watched_listed_acted_on_and_its_turn_noted() {
    let work = tempfile::tempdir().unwrap();
    let config = replay(
        &fixture("approval-touch"),
        &work.path().join("received.jsonl"),
    );
    let t = TestDaemon::start_with(move |c| c.codex = Some(config)).await;
    let mut watch = Watch::spawn(&t);

    let cwd = work.path().display().to_string();
    let created = t
        .hutan(&[
            "chat",
            "new",
            "--json",
            "--cwd",
            &cwd,
            "--approval-policy",
            "untrusted",
            "--sandbox",
            "read-only",
        ])
        .await
        .ok();
    let session = created.json()["session"]["id"].as_str().unwrap().to_owned();
    t.hutan(&[
        "chat",
        "send",
        &session,
        "Run `touch hi.txt` in a shell, then reply with the word done.",
    ])
    .await
    .ok();

    // The watcher prints it as it is raised.
    let intent = watch.next().await;
    assert_eq!(intent["kind"], "approval_required");
    assert_eq!(intent["session_id"], session.as_str());
    let id = intent["id"].as_str().unwrap().to_owned();
    let actions: Vec<_> = intent["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions,
        ["accept", "acceptWithExecpolicyAmendment", "cancel"]
    );
    assert!(intent["body"].as_str().unwrap().contains("touch hi.txt"));

    // The table names it, its kind and its actions.
    // The human table names the intent, its kind and what can be done about it.
    let table = t.hutan(&["notify", "ls"]).await.ok().stdout;
    let row = table
        .lines()
        .find(|line| line.contains(&id))
        .expect("a row");
    assert!(row.contains("approval_required"), "{table}");
    for action in ["accept", "acceptWithExecpolicyAmendment", "cancel"] {
        assert!(row.contains(action), "{table}");
    }

    // An action the intent does not offer is refused and the intent stays.
    let refused = t.hutan(&["notify", "act", &id[..8], "decline"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(
        refused.stderr.contains("option_not_offered") && refused.stderr.contains("accept"),
        "{refused:?}"
    );
    let unknown = t.hutan(&["notify", "act", "0000", "accept"]).await;
    assert_eq!(unknown.code, 1);
    assert_eq!(intents(&t).await.len(), 1);

    // By unique prefix: the approval is answered.
    let acted = t
        .hutan(&["notify", "act", &id[..8], "accept", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(acted["route"], "approval");
    let approvals = t.hutan(&["approvals", "--json"]).await.ok();
    assert!(approvals.json()["approvals"].as_array().unwrap().is_empty());

    // The turn then ends in a session nobody has focused: the watcher prints that intent, and
    // it is all that is pending.
    let finished = watch.next().await;
    assert_eq!(finished["kind"], "chat_turn_finished");
    assert_eq!(finished["session_id"], session.as_str());
    let pending = intents(&t).await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["id"], finished["id"]);

    // Acknowledging by prefix empties the list.
    let finished_id = finished["id"].as_str().unwrap();
    let acked = t
        .hutan(&["notify", "ack", &finished_id[..8], "--json"])
        .await
        .ok()
        .json();
    assert_eq!(acked["id"], finished_id);
    assert_eq!(acked["acknowledged"], true);
    assert!(intents(&t).await.is_empty());
    watch.child.kill().await.unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_session_is_listed_replayed_to_a_late_watcher_and_opened() {
    let t = TestDaemon::start().await;
    let session = t.new_session(&[], &["sh", "-c", "exit 3"]).await;
    let pending = wait_for_intents(&t, 1).await;
    let intent = &pending[0];
    assert_eq!(intent["kind"], "session_exited_with_error");
    assert_eq!(intent["session_id"], session.as_str());
    assert_eq!(intent["body"], "The process exited with code 3.");
    let id = intent["id"].as_str().unwrap().to_owned();

    // A watcher that starts late still gets what is pending, first.
    let mut watch = Watch::spawn(&t);
    assert_eq!(watch.next().await["id"], id.as_str());

    // `open` goes out as ui.open; with no desktop connected nobody was reached (exit 0).
    let opened = t
        .hutan(&["notify", "act", &id, "open", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(opened["route"], "ui_open");
    assert_eq!(opened["desktop_clients"], 0);
    assert!(intents(&t).await.is_empty(), "acting acknowledges");

    let raw = {
        // Another failing session, acted on with --json.
        t.new_session(&[], &["sh", "-c", "exit 4"]).await;
        let pending = wait_for_intents(&t, 1).await;
        let id = pending[0]["id"].as_str().unwrap().to_owned();
        t.hutan(&["notify", "act", &id, "open", "--json"])
            .await
            .ok()
            .json()
    };
    assert_eq!(raw["route"], "ui_open");
    assert_eq!(raw["desktop_clients"], 0);

    // Acknowledging what is gone is an error.
    let gone = t.hutan(&["notify", "ack", &id]).await;
    assert_eq!(gone.code, 1, "{gone:?}");
    watch.child.kill().await.unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_errors_exit_two_and_nothing_is_pending_at_first() {
    let t = TestDaemon::start().await;
    let missing = t.hutan(&["notify", "act", "abcd"]).await;
    assert_eq!(missing.code, 2, "{missing:?}");
    let empty = t.hutan(&["notify", "ls", "--json"]).await.ok().json();
    assert!(empty["intents"].as_array().unwrap().is_empty());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mute_silences_a_session_until_unmute_and_ls_says_so() {
    let t = TestDaemon::start().await;
    let session = t.new_session(&[], &["sleep", "600"]).await;
    let muted_in_ls = |sessions: Vec<Value>| {
        sessions
            .iter()
            .find(|s| s["id"] == session.as_str())
            .map(|s| s["muted"].as_bool().unwrap_or(false))
    };
    assert_eq!(muted_in_ls(t.sessions().await), Some(false));

    let muted = t
        .hutan(&["notify", "mute", &session, "--json"])
        .await
        .ok()
        .json();
    assert_eq!(muted["id"], session.as_str());
    assert_eq!(muted["muted"], true);
    assert_eq!(muted_in_ls(t.sessions().await), Some(true));

    let said = t.hutan(&["notify", "unmute", &session]).await.ok();
    assert!(said.stdout.contains("are on"), "{said:?}");
    assert_eq!(muted_in_ls(t.sessions().await), Some(false));

    let unknown = t.hutan(&["notify", "mute", "no-such-session"]).await;
    assert_eq!(unknown.code, 1, "{unknown:?}");
    t.stop().await;
}
