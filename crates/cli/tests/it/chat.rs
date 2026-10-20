//! `hutan chat …`, `hutan approvals`, `hutan approve`, `hutan questions`, `hutan answer` against a real daemon whose codex is
//! the fake app server replaying transcripts recorded from codex 0.162.1.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use common::{HUTAN, Out, TestDaemon, WAIT, codex_fixture_bin, command, finish};
use hutan_agents::CodexConfig;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
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

struct Chat {
    t: TestDaemon,
    work: tempfile::TempDir,
}

impl Chat {
    async fn with_fixture(name: &str) -> Self {
        let work = tempfile::tempdir().unwrap();
        let config = replay(&fixture(name), &work.path().join("received.jsonl"));
        let t = TestDaemon::start_with(move |c| c.codex = Some(config)).await;
        Self { t, work }
    }

    fn cwd(&self) -> String {
        self.work.path().display().to_string()
    }

    /// `hutan chat new --json` with extra flags; returns the session id.
    async fn new_chat(&self, extra: &[&str]) -> String {
        let cwd = self.cwd();
        let mut args = vec!["chat", "new", "--json", "--cwd", &cwd];
        args.extend_from_slice(extra);
        let out = self.t.hutan(&args).await.ok();
        out.json()["session"]["id"].as_str().unwrap().to_owned()
    }

    async fn pending_approvals(&self) -> Vec<Value> {
        let out = self.t.hutan(&["approvals", "--json"]).await.ok();
        out.json()["approvals"].as_array().unwrap().clone()
    }

    async fn wait_for_approval(&self) -> Value {
        timeout(WAIT, async {
            loop {
                if let Some(first) = self.pending_approvals().await.into_iter().next() {
                    return first;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("an approval never showed up")
    }

    /// Spawns `hutan <args>` with piped output.
    fn spawn(&self, args: &[&str]) -> tokio::process::Child {
        let mut cmd = command(Path::new(HUTAN), Some(&self.t.paths.socket), args);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.spawn().unwrap()
    }
}

async fn finished(child: tokio::process::Child) -> Out {
    let output = timeout(WAIT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    Out {
        code: output.status.code().expect("killed by a signal"),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn new_send_wait_and_log_show_the_conversation() {
    let c = Chat::with_fixture("plain-turn").await;
    let created = c.t.hutan(&["chat", "new", "--cwd", &c.cwd()]).await.ok();
    let words: Vec<_> = created.stdout.split_whitespace().collect();
    assert_eq!(&words[0], &"session", "{created:?}");
    assert_eq!(&words[2..4], &["codex", "gpt-6.1-sol"], "{created:?}");
    assert!(
        created
            .stdout
            .contains("approvals: on-request, sandbox: workspace-write"),
        "{created:?}"
    );
    let id = words[1].to_owned();

    let sent =
        c.t.hutan(&[
            "chat",
            "send",
            &id,
            "Reply with the single word pong and nothing else.",
            "--wait",
        ])
        .await
        .ok();
    assert_eq!(sent.stdout, "pong\n", "streamed reply only: {sent:?}");

    // The journal replays it, the user's message included.
    let log = c.t.hutan(&["chat", "log", &id]).await.ok();
    assert_eq!(
        log.stdout, "> Reply with the single word pong and nothing else.\npong\n",
        "{log:?}"
    );
    let json = c.t.hutan(&["--json", "chat", "log", &id]).await.ok().json();
    let kinds: Vec<&str> = json["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"turn_completed") && kinds.contains(&"usage"),
        "{kinds:?}"
    );
    assert!(
        json["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["delivery"] == "replay")
    );

    // It is an ordinary session in the list, kind harness, back to idle.
    let session = c.t.session(&id).await;
    assert_eq!(session["kind"], "harness");
    assert_eq!(session["state"]["state"], "idle");
    assert_eq!(session["chat"]["model"], "gpt-6.1-sol");
    let table = c.t.hutan(&["ls"]).await.ok().stdout;
    assert!(table.contains(&id) && table.contains("idle"), "{table}");
    c.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn send_without_wait_returns_the_turn_and_wait_streams_json_lines() {
    let c = Chat::with_fixture("plain-turn").await;
    let id = c.new_chat(&[]).await;
    let sent =
        c.t.hutan(&["--json", "chat", "send", &id, "ping", "--wait"])
            .await
            .ok();
    let events: Vec<Value> = sent
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let types: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert!(
        types.contains(&"message_delta") && types.last() == Some(&"turn_completed"),
        "{types:?}"
    );
    assert!(
        events
            .iter()
            .all(|e| e["session_id"] == id.as_str() && e["delivery"] == "live")
    );
    c.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_approval_is_listed_answered_and_the_reply_streams() {
    let c = Chat::with_fixture("approval-touch").await;
    let id = c
        .new_chat(&["--approval-policy", "untrusted", "--sandbox", "read-only"])
        .await;

    // The waiting sender prints the request and how to answer it, then the reply.
    let waiting = c.spawn(&[
        "chat",
        "send",
        &id,
        "Run `touch hi.txt` in a shell, then reply with the word done.",
        "--wait",
    ]);
    let approval = c.wait_for_approval().await;
    let approval_id = approval["id"].as_str().unwrap().to_owned();
    assert_eq!(approval["session_id"], id.as_str());
    assert_eq!(approval["kind"], "command");
    assert!(approval["title"].as_str().unwrap().contains("touch hi.txt"));

    let table = c.t.hutan(&["approvals"]).await.ok().stdout;
    let mut lines = table.lines();
    let header: Vec<_> = lines.next().unwrap().split_whitespace().collect();
    assert_eq!(header, ["ID", "SESSION", "REQUEST", "OPTIONS"]);
    let row = lines.next().unwrap();
    assert!(
        row.contains(&approval_id)
            && row.contains("touch hi.txt")
            && row.contains("accept acceptWithExecpolicyAmendment cancel"),
        "{table}"
    );

    // An option the request did not offer is refused; the request stays.
    let refused = c.t.hutan(&["approve", &approval_id[..8], "decline"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(
        refused.stderr.contains("option_not_offered") && refused.stderr.contains("accept"),
        "{refused:?}"
    );
    assert_eq!(c.pending_approvals().await.len(), 1);
    let unknown = c.t.hutan(&["approve", "0000", "accept"]).await;
    assert_eq!(unknown.code, 1);
    assert!(
        unknown.stderr.contains("no pending approval"),
        "{unknown:?}"
    );

    // By unique prefix.
    let approved =
        c.t.hutan(&["approve", &approval_id[..8], "accept"])
            .await
            .ok();
    assert_eq!(approved.stdout, format!("approved {approval_id}: accept\n"));

    let out = finished(waiting).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout.contains(&format!("? approval {approval_id}: ")),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains(&format!(
            "hutan approve {approval_id} <accept|acceptWithExecpolicyAmendment|cancel>"
        )),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains(&format!("approval {approval_id}: accept")),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("• shell: /bin/zsh -lc 'touch hi.txt' → completed"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.trim_end().ends_with("done"), "{}", out.stdout);
    assert!(c.pending_approvals().await.is_empty());
    let none = c.t.hutan(&["approvals"]).await.ok();
    assert_eq!(none.stdout, "no pending approvals\n");

    // The log shows the same conversation, approval included.
    let log = c.t.hutan(&["chat", "log", &id]).await.ok().stdout;
    assert!(
        log.contains("• shell:") && log.contains(&format!("approval {approval_id}: accept")),
        "{log}"
    );
    // ... and who answered it: this CLI.
    assert!(
        log.contains(&format!("approval {approval_id}: accept (decided by cli)")),
        "{log}"
    );
    c.t.stop().await;
}

impl Chat {
    async fn pending_questions(&self) -> Vec<Value> {
        let out = self.t.hutan(&["questions", "--json"]).await.ok();
        out.json()["questions"].as_array().unwrap().clone()
    }

    async fn wait_for_question(&self) -> Value {
        timeout(WAIT, async {
            loop {
                if let Some(first) = self.pending_questions().await.into_iter().next() {
                    return first;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("a question never showed up")
    }
}

const SECRET: &str = "s3cr3t-token";

#[tokio::test(flavor = "multi_thread")]
async fn a_question_is_listed_answered_by_field_with_the_secret_from_stdin() {
    use tokio::io::AsyncWriteExt;

    let c = Chat::with_fixture("question-user-input").await;
    let id = c.new_chat(&[]).await;
    let waiting = c.spawn(&["chat", "send", &id, "Ask me something.", "--wait"]);
    let question = c.wait_for_question().await;
    let question_id = question["id"].as_str().unwrap().to_owned();
    assert_eq!(question["session_id"], id.as_str());

    let table = c.t.hutan(&["questions"]).await.ok().stdout;
    let mut lines = table.lines();
    let header: Vec<_> = lines.next().unwrap().split_whitespace().collect();
    assert_eq!(header, ["ID", "SESSION", "QUESTION", "FIELDS"]);
    let row = lines.next().unwrap();
    assert!(
        row.contains(&question_id)
            && row.contains("Codex has 3 questions")
            && row.contains("scope=Only the CLI|CLI and daemon")
            && row.contains("token=(secret)"),
        "{table}"
    );

    // What does not fit is refused before it reaches the daemon or the agent.
    let unknown =
        c.t.hutan(&["answer", &question_id[..8], "nonsense=1"])
            .await;
    assert_eq!(unknown.code, 2, "{unknown:?}");
    assert!(
        unknown.stderr.contains("no field `nonsense`"),
        "{unknown:?}"
    );
    let missing =
        c.t.hutan(&["answer", &question_id[..8], "scope=Only the CLI"])
            .await;
    assert_eq!(missing.code, 1, "{missing:?}");
    assert!(missing.stderr.contains("required"), "{missing:?}");
    let conflicting =
        c.t.hutan(&["answer", &question_id[..8], "--decline", "scope=x"])
            .await;
    assert_eq!(conflicting.code, 2, "{conflicting:?}");
    let no_such = c.t.hutan(&["answer", "0000", "--decline"]).await;
    assert_eq!(no_such.code, 1);
    assert!(
        no_such.stderr.contains("no pending question"),
        "{no_such:?}"
    );
    assert_eq!(c.pending_questions().await.len(), 1, "still pending");

    // The secret is read from stdin, so it is on no command line.
    let mut cmd = command(
        Path::new(HUTAN),
        Some(&c.t.paths.socket),
        &[
            "answer",
            &question_id[..8],
            "scope=Only the CLI",
            "note=none",
            "token=-",
        ],
    );
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(format!("{SECRET}\n").as_bytes())
        .await
        .unwrap();
    drop(stdin);
    let answered = finished(child).await;
    assert_eq!(answered.code, 0, "{answered:?}");
    assert_eq!(
        answered.stdout,
        format!("{question_id} answered: note, scope, token (secret)\n")
    );
    assert!(!answered.stdout.contains(SECRET) && !answered.stderr.contains(SECRET));

    let out = finished(waiting).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout
            .contains(&format!("? question {question_id}: Codex has 3 questions")),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains(&format!(
            "hutan answer {question_id} <field=value ...|--decline|--cancel>"
        )),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains(&format!(
            "question {question_id}: answered (secret: token) (decided by cli)"
        )),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains(SECRET));
    assert!(c.pending_questions().await.is_empty());
    assert_eq!(
        c.t.hutan(&["questions"]).await.ok().stdout,
        "no pending questions\n"
    );

    // The log tells the same, and the journal's JSON holds no secret.
    let log = c.t.hutan(&["chat", "log", &id]).await.ok().stdout;
    assert!(
        log.contains(&format!(
            "question {question_id}: answered (secret: token) (decided by cli)"
        )),
        "{log}"
    );
    let json = c.t.hutan(&["--json", "chat", "log", &id]).await.ok().stdout;
    assert!(!json.contains(SECRET));
    c.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_question_can_be_declined_and_typed_values_are_parsed_by_field() {
    let c = Chat::with_fixture("question-elicitation").await;
    let id = c.new_chat(&[]).await;
    let waiting = c.spawn(&["chat", "send", &id, "Deploy.", "--wait"]);
    let question = c.wait_for_question().await;
    let question_id = question["id"].as_str().unwrap().to_owned();
    // Numbers and booleans are typed by their fields: a wrong one is a usage error.
    let bad_number =
        c.t.hutan(&[
            "answer",
            &question_id[..8],
            "environment=staging",
            "replicas=many",
        ])
        .await;
    assert_eq!(bad_number.code, 2, "{bad_number:?}");
    let bad_bool =
        c.t.hutan(&[
            "answer",
            &question_id[..8],
            "environment=staging",
            "notify=maybe",
        ])
        .await;
    assert_eq!(bad_bool.code, 2, "{bad_bool:?}");
    let declined =
        c.t.hutan(&["--json", "answer", &question_id[..8], "--decline"])
            .await
            .ok();
    assert_eq!(
        declined.json(),
        serde_json::json!({"question_id": question_id, "action": "decline"})
    );
    let out = finished(waiting).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout.contains(&format!(
            "question {question_id}: declined (decided by cli)"
        )),
        "{}",
        out.stdout
    );
    c.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_waiting_send_fails_when_the_turn_is_cancelled() {
    let c = Chat::with_fixture("approval-interrupt").await;
    let id = c.new_chat(&[]).await;
    let waiting = c.spawn(&["chat", "send", &id, "Run `touch hi.txt`", "--wait"]);
    c.wait_for_approval().await;
    let interrupted = c.t.hutan(&["chat", "interrupt", &id]).await.ok();
    assert!(
        interrupted.stdout.contains("interrupted"),
        "{interrupted:?}"
    );
    let out = finished(waiting).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stdout.contains("expired") && out.stdout.contains("turn cancelled"),
        "{}",
        out.stdout
    );
    assert!(c.pending_approvals().await.is_empty());
    c.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_turn_exits_one_with_the_reason() {
    let c = Chat::with_fixture("turn-failed").await;
    let id = c.new_chat(&[]).await;
    let out = c.t.hutan(&["chat", "send", &id, "hi", "--wait"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stdout.contains(
            "✗ turn failed: The 'hutan-no-such-model' model is not supported when using Codex"
        ),
        "{out:?}"
    );
    c.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_follows_the_session_live_and_ends_with_it() {
    let c = Chat::with_fixture("plain-turn").await;
    let id = c.new_chat(&[]).await;
    let mut attach = c.spawn(&["chat", "attach", &id]);
    let mut lines = BufReader::new(attach.stdout.take().unwrap()).lines();
    // Give the attach a moment to subscribe before the turn starts.
    sleep(Duration::from_millis(300)).await;
    c.t.hutan(&["chat", "send", &id, "ping", "--wait"])
        .await
        .ok();
    let mut seen = Vec::new();
    timeout(WAIT, async {
        while let Some(line) = lines.next_line().await.unwrap() {
            seen.push(line);
            if seen.iter().any(|l| l == "pong") {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("attach never printed the reply: {seen:?}"));
    assert!(
        seen.iter().any(|l| l.starts_with("> Reply with")),
        "{seen:?}"
    );

    // Stopping the session ends the attach.
    c.t.hutan(&["stop", &id]).await.ok();
    let status = timeout(WAIT, attach.wait()).await.unwrap().unwrap();
    assert_eq!(status.code(), Some(0));
    let rest: Vec<String> = {
        let mut rest = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            rest.push(line);
        }
        rest
    };
    assert!(
        rest.iter()
            .any(|l| l.contains(&id) && l.contains("stopped")),
        "{rest:?}"
    );
    c.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_to_an_ended_session_prints_the_history_and_returns() {
    let c = Chat::with_fixture("plain-turn").await;
    let id = c.new_chat(&[]).await;
    c.t.hutan(&["chat", "send", &id, "ping", "--wait"])
        .await
        .ok();
    c.t.hutan(&["stop", &id]).await.ok();
    let out = c.t.hutan(&["chat", "attach", &id]).await.ok();
    assert!(
        out.stdout
            .starts_with("> Reply with the single word pong and nothing else.\npong\n"),
        "{out:?}"
    );
    assert!(out.stdout.trim_end().ends_with("stopped"), "{out:?}");
    c.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn models_are_listed_from_the_agents_catalog() {
    let c = Chat::with_fixture("plain-turn").await;
    let table = c.t.hutan(&["chat", "models"]).await.ok().stdout;
    assert!(
        table.contains("gpt-6.1-sol (default)\tGPT-6.1-Sol"),
        "{table}"
    );
    assert!(table.contains("gpt-5.6-luna\t"), "{table}");
    let json = c.t.hutan(&["--json", "chat", "models"]).await.ok().json();
    assert_eq!(json["agent"], "codex");
    c.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mistakes_are_explained() {
    let c = Chat::with_fixture("plain-turn").await;
    let terminal = c.t.new_session(&[], &["cat"]).await;
    let chat = c.new_chat(&[]).await;

    // Chat commands on a terminal session, and the reverse.
    let out = c.t.hutan(&["chat", "send", &terminal, "hi"]).await;
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("terminal session"), "{out:?}");
    let out = c.t.hutan(&["send", &chat, "hi"]).await;
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("chat session") && out.stderr.contains("hutan chat"),
        "{out:?}"
    );
    let out = c.t.hutan(&["history", &chat]).await;
    assert!(out.stderr.contains("hutan chat log"), "{out:?}");

    // A directory that does not exist; an unknown model; a policy that is not one.
    let out =
        c.t.hutan(&["chat", "new", "--cwd", "/nonexistent/hutan-dir"])
            .await;
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("bad_request") && out.stderr.contains("not an existing directory"),
        "{out:?}"
    );
    let out =
        c.t.hutan(&["chat", "new", "--cwd", &c.cwd(), "--model", "gpt-nope"])
            .await;
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("provider_error") && out.stderr.contains("gpt-nope"),
        "{out:?}"
    );
    let out =
        c.t.hutan(&["chat", "new", "--approval-policy", "always"])
            .await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("unknown approval policy"), "{out:?}");

    // A second message while the first turn runs.
    let out = c.t.hutan(&["chat", "interrupt", &chat]).await;
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("invalid_state") && out.stderr.contains("no turn"),
        "{out:?}"
    );
    c.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_commands_without_a_daemon_exit_three() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("nothing.sock");
    let out = finish(command(Path::new(HUTAN), Some(&socket), &["approvals"])).await;
    assert_eq!(out.code, 3, "{out:?}");
}
