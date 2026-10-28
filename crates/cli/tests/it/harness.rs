//! `hutan chat new --agent omp`, `hutan account …` and `hutan task add --agent` against a real
//! daemon whose `omp` is the fake ACP agent (`hutan-acp-fixture replay`) and whose harness CLIs
//! are shell scripts.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use common::{HUTAN, Out, TestDaemon, WAIT, acp_fixture_bin, codex_fixture_bin, command};
use serde_json::Value;
use tokio::time::{sleep, timeout};

const OPUS: &str = "gitlab-duo/duo-chat-opus-4-6";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"))
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

struct Env {
    t: TestDaemon,
    work: tempfile::TempDir,
}

impl Env {
    /// A daemon whose `omp` replays `transcript`; the fake CLIs report omp logged in to
    /// `anthropic` and Claude Code logged out, and Codex answers `account/read` from a script.
    async fn start(transcript: &str) -> Self {
        let work = tempfile::tempdir().unwrap();
        let w = work.path();
        let omp = executable(
            w,
            "omp-cli",
            r#"case "$1" in
  models) echo '{"models":[{"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","selector":"anthropic/claude-opus-4-5","name":"Claude Opus 4.5"}]}' ;;
  usage) echo '{"reports":[{"provider":"anthropic","metadata":{"planType":"max","email":"me@example.com"},"limits":[]}]}' ;;
  *) echo "fake omp $*" ;;
esac"#,
        );
        let claude = executable(
            w,
            "claude-cli",
            r#"if [ "$1 $2" = "auth status" ]; then echo '{"loggedIn":false,"authMethod":"none"}'; else echo "fake claude $*"; fi"#,
        );
        let codex_script = w.join("codex-account.jsonl");
        std::fs::write(
            &codex_script,
            r#"{"dir":"out","msg":{"method":"initialize","id":1,"params":{}}}
{"dir":"in","msg":{"id":1,"result":{"userAgent":"hutan/0.162.1 (Mac OS; arm64)"}}}
{"dir":"out","msg":{"method":"initialized"}}
{"dir":"out","msg":{"method":"account/read","id":2,"params":{}}}
{"dir":"in","msg":{"id":2,"result":{"account":{"type":"chatgpt","email":"me@example.com","planType":"plus"},"requiresOpenaiAuth":false}}}
"#,
        )
        .unwrap();
        let toml = format!(
            "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\ncli = {omp:?}\n\n[harness.claude]\ncli = {claude:?}\n\n[codex]\nbin = {codex:?}\nargs = [\"replay\", {codex_script:?}, \"app-server\"]\n",
            bin = acp_fixture_bin(),
            received = w.join("received.jsonl"),
            transcript = fixture(transcript),
            codex = codex_fixture_bin(),
        );
        let t = TestDaemon::start_toml(&toml, |_| {}).await;
        Self { t, work }
    }

    fn cwd(&self) -> String {
        self.work.path().display().to_string()
    }

    fn received(&self) -> Vec<Value> {
        std::fs::read_to_string(self.work.path().join("received.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["msg"].clone())
            .collect()
    }

    async fn new_chat(&self, extra: &[&str]) -> String {
        let cwd = self.cwd();
        let mut args = vec!["chat", "new", "--agent", "omp", "--json", "--cwd", &cwd];
        args.extend_from_slice(extra);
        self.t.hutan(&args).await.ok().json()["session"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

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
async fn an_omp_chat_is_created_sent_to_and_logged_like_any_chat() {
    let e = Env::start("plain-turn").await;
    let created =
        e.t.hutan(&["chat", "new", "--agent", "omp", "--cwd", &e.cwd()])
            .await
            .ok();
    let words: Vec<_> = created.stdout.split_whitespace().collect();
    assert_eq!(&words[0], &"session", "{created:?}");
    assert_eq!(&words[2..4], &["omp", OPUS], "{created:?}");
    assert_eq!(
        words.len(),
        4,
        "no approval policy or sandbox is shown for a harness: {created:?}"
    );
    let id = words[1].to_owned();

    let sent =
        e.t.hutan(&["chat", "send", &id, "Say hello.", "--wait"])
            .await
            .ok();
    assert_eq!(sent.stdout, "Hello, world.\n", "{sent:?}");
    let log = e.t.hutan(&["chat", "log", &id]).await.ok();
    assert_eq!(log.stdout, "> Say hello.\nHello, world.\n", "{log:?}");

    let session = e.t.session(&id).await;
    assert_eq!(session["kind"], "harness");
    assert_eq!(session["chat"]["agent"], "omp");
    assert_eq!(session["chat"]["agent_session_id"], "sess_fixture_1");
    let prompt = e
        .received()
        .into_iter()
        .find(|m| m["method"] == "session/prompt")
        .unwrap();
    assert_eq!(prompt["params"]["prompt"][0]["text"], "Say hello.");
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_harness_approval_is_listed_and_answered_with_the_agents_option() {
    let e = Env::start("permission-allow").await;
    let id = e.new_chat(&[]).await;
    let waiting = e.spawn(&["chat", "send", &id, "edit lib.rs", "--wait"]);
    let approval = timeout(WAIT, async {
        loop {
            let out = e.t.hutan(&["approvals", "--json"]).await.ok();
            if let Some(first) = out.json()["approvals"].as_array().unwrap().first() {
                return first.clone();
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("an approval never showed up");
    let approval_id = approval["id"].as_str().unwrap().to_owned();
    assert_eq!(approval["kind"], "file_change");
    let table = e.t.hutan(&["approvals"]).await.ok().stdout;
    assert!(
        table.contains("allow_once allow_always reject_once reject_always"),
        "{table}"
    );

    let refused = e.t.hutan(&["approve", &approval_id[..8], "accept"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("option_not_offered"), "{refused:?}");
    e.t.hutan(&["approve", &approval_id[..8], "allow_once"])
        .await
        .ok();
    let out = finished(waiting).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert!(out.stdout.contains("Edited."), "{}", out.stdout);
    let received = e.received();
    let reply = received.iter().find(|m| m.get("method").is_none()).unwrap();
    assert_eq!(reply["result"]["outcome"]["optionId"], "allow_once");
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_ends_a_harness_turn() {
    let e = Env::start("cancel").await;
    let id = e.new_chat(&[]).await;
    let waiting = e.spawn(&["chat", "send", &id, "long task", "--wait"]);
    timeout(WAIT, async {
        loop {
            if e.received().iter().any(|m| m["method"] == "session/prompt") {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    e.t.hutan(&["chat", "interrupt", &id]).await.ok();
    let out = finished(waiting).await;
    assert_eq!(
        out.code, 1,
        "a cancelled turn is a failure of --wait: {out:?}"
    );
    assert!(e.received().iter().any(|m| m["method"] == "session/cancel"));
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn harness_options_are_validated() {
    let e = Env::start("plain-turn").await;
    let cwd = e.cwd();
    // The harness's own settings decide what asks.
    let out =
        e.t.hutan(&[
            "chat",
            "new",
            "--agent",
            "omp",
            "--cwd",
            &cwd,
            "--approval-policy",
            "never",
        ])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("belong to the harness"), "{out:?}");
    // clap: unknown agents, a mode of a direct chat.
    let out =
        e.t.hutan(&["chat", "new", "--agent", "gemini", "--cwd", &cwd])
            .await;
    assert_eq!(out.code, 2, "{out:?}");
    let out =
        e.t.hutan(&[
            "chat",
            "new",
            "--agent",
            "omp",
            "--direct",
            "--provider",
            "x",
        ])
        .await;
    assert_eq!(out.code, 2, "{out:?}");
    let out =
        e.t.hutan(&[
            "chat",
            "new",
            "--agent",
            "omp",
            "--cwd",
            &cwd,
            "--model",
            "nobody/nothing",
        ])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("nobody/nothing"), "{out:?}");
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_models_and_model_use_the_agent_flag() {
    let e = Env::start("model-switch").await;
    let models = e.t.hutan(&["chat", "models", "--agent", "omp"]).await.ok();
    // The agent's own session says what it offers: 17 models, one is the default.
    assert_eq!(models.stdout.lines().count(), 17, "{models:?}");
    assert!(
        models.stdout.contains(&format!("{OPUS} (default)\t"))
            && models.stdout.contains("claude-haiku-4-5"),
        "{models:?}"
    );
    let json =
        e.t.hutan(&["--json", "chat", "models", "--agent", "omp"])
            .await
            .ok()
            .json();
    assert_eq!(json["agent"], "omp");

    let id = e
        .new_chat(&["--model", "gitlab-duo/claude-haiku-4-5-20251001"])
        .await;
    let switched = e.t.hutan(&["chat", "model", &id, OPUS]).await.ok();
    assert_eq!(switched.stdout, format!("session {id} omp {OPUS}\n"));
    let refused = e.t.hutan(&["chat", "model", &id, "nobody/nothing"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn account_ls_shows_what_each_harness_reports() {
    let e = Env::start("plain-turn").await;
    let table = e.t.hutan(&["account", "ls"]).await.ok().stdout;
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(
        lines[0].split_whitespace().collect::<Vec<_>>(),
        ["HARNESS", "PROVIDER", "ACCOUNT", "PLAN", "STATUS"]
    );
    assert!(
        lines[1].starts_with("omp")
            && lines[1].contains("anthropic")
            && lines[1].contains("max")
            && lines[1].ends_with("logged in"),
        "{table}"
    );
    assert!(
        lines[2].starts_with("claude") && lines[2].ends_with("logged out"),
        "{table}"
    );
    assert!(
        lines[3].starts_with("codex") && lines[3].contains("ChatGPT") && lines[3].contains("plus"),
        "{table}"
    );
    assert!(!table.contains("example.com"), "{table}");

    let json = e.t.hutan(&["--json", "account", "ls"]).await.ok();
    assert!(!json.stdout.contains("example.com"), "{json:?}");
    let accounts = json.json()["accounts"].as_array().unwrap().clone();
    assert_eq!(accounts.len(), 3);
    assert_eq!(accounts[0]["status"], "logged_in");
    assert_eq!(accounts[1]["status"], "logged_out");
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn accounts_are_added_listed_defaulted_used_by_a_chat_and_removed() {
    let e = Env::start("plain-turn").await;
    // Adding creates the account's empty home; `--no-login` leaves the login for later.
    let added =
        e.t.hutan(&[
            "--json",
            "account",
            "add",
            "omp",
            "second",
            "--label",
            "Second",
            "--no-login",
        ])
        .await
        .ok()
        .json();
    assert_eq!(added["name"], "second");
    let home = std::path::PathBuf::from(added["home"].as_str().unwrap());
    assert!(home.is_dir());
    // A name the daemon refuses is a failed command, and nothing is created.
    let refused =
        e.t.hutan(&["account", "add", "omp", "Bad Name", "--no-login"])
            .await;
    assert_eq!(refused.code, 1, "{refused:?}");

    let table = e.t.hutan(&["account", "list"]).await.ok().stdout;
    assert!(
        table.contains("HARNESS  ACCOUNT  LABEL   STATUS"),
        "{table}"
    );
    assert!(table.contains("omp      second   Second"), "{table}");
    assert!(!table.contains("default"), "no default yet:\n{table}");

    // Set, read back, and clear a default; the harness's own login can be named too.
    let out =
        e.t.hutan(&["account", "default", "omp", "second"])
            .await
            .ok()
            .stdout;
    assert_eq!(out, "new omp sessions take second\n");
    let table = e.t.hutan(&["account", "list"]).await.ok().stdout;
    assert!(
        table
            .lines()
            .any(|l| l.starts_with("omp") && l.ends_with("default")),
        "{table}"
    );
    let json = e.t.hutan(&["--json", "account", "list"]).await.ok().json();
    assert_eq!(json["defaults"][0]["account"], "second");
    assert_eq!(json["managed"][0]["name"], "second");
    let missing = e.t.hutan(&["account", "default", "omp", "ghost"]).await;
    assert_eq!(missing.code, 1, "{missing:?}");

    // A chat takes the default; `--account system` is the harness's own login.
    let id = e.new_chat(&[]).await;
    let sessions = e.t.hutan(&["--json", "ls"]).await.ok().json();
    let chat = sessions["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id.as_str())
        .unwrap()["chat"]
        .clone();
    assert_eq!(chat["account"]["name"], "second");
    let own = e.new_chat(&["--account", "system"]).await;
    let sessions = e.t.hutan(&["--json", "ls"]).await.ok().json();
    let chat = sessions["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == own.as_str())
        .unwrap()["chat"]
        .clone();
    assert!(chat.get("account").is_none(), "{chat}");

    // Removing needs a yes: without a terminal and without --yes nothing happens.
    let refused = e.t.hutan(&["account", "remove", "omp", "second"]).await;
    assert_eq!(refused.code, 2, "{refused:?}");
    assert!(home.is_dir());
    // A live chat on the account keeps it.
    let busy =
        e.t.hutan(&["account", "remove", "omp", "second", "--yes"])
            .await;
    assert_eq!(busy.code, 1, "{busy:?}");
    e.t.hutan(&["stop", &id]).await.ok();
    e.t.wait_state(&id, "stopped").await;
    let out =
        e.t.hutan(&["account", "remove", "omp", "second", "--yes"])
            .await
            .ok()
            .stdout;
    assert!(out.starts_with("removed omp account second"), "{out}");
    assert!(!home.exists());
    let table = e.t.hutan(&["account", "list"]).await.ok().stdout;
    assert!(!table.contains("second"), "{table}");
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn account_login_and_logout_start_the_harnesss_own_flow_in_a_session() {
    let e = Env::start("plain-turn").await;
    let started =
        e.t.hutan(&["--json", "account", "login", "omp", "anthropic"])
            .await
            .ok()
            .json();
    let session = &started["session"];
    assert_eq!(session["title"], "Log in to omp (anthropic)");
    let command: Vec<&str> = session["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert!(command[0].ends_with("omp-cli"), "{command:?}");
    assert_eq!(&command[1..], ["login", "anthropic"]);
    let id = session["id"].as_str().unwrap();
    // The text form says where to complete it.
    let text = e.t.hutan(&["account", "login", "claude"]).await.ok().stdout;
    assert!(
        text.contains("complete the login in it (`hutan attach "),
        "{text}"
    );
    e.t.wait_state(id, "exited").await;

    let out =
        e.t.hutan(&["account", "logout", "omp", "anthropic"])
            .await
            .ok()
            .stdout;
    assert!(out.contains("type `/logout anthropic`"), "{out}");
    let listed = e.t.hutan(&["ls"]).await.ok().stdout;
    assert!(
        listed.contains("Log in to Claude Code") && listed.contains("Log out of omp"),
        "{listed}"
    );

    // clap and the daemon refuse what a harness cannot take.
    assert_eq!(e.t.hutan(&["account", "login", "gemini"]).await.code, 2);
    let out = e.t.hutan(&["account", "login", "claude", "openai"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("takes no provider"), "{out:?}");
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_runs_as_an_omp_chat() {
    let e = Env::start("plain-turn").await;
    let repo = e.work.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let ws =
        e.t.hutan(&["project", "open", repo.to_str().unwrap(), "--json"])
            .await
            .ok()
            .json()["workspace"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
    // clap: an agent needs a prompt and excludes a command.
    assert_eq!(
        e.t.hutan(&["task", "add", "X", "--ws", &ws, "--agent", "omp"])
            .await
            .code,
        2
    );
    assert_eq!(
        e.t.hutan(&[
            "task", "add", "X", "--ws", &ws, "--agent", "omp", "--prompt", "hi", "--", "sh"
        ])
        .await
        .code,
        2
    );
    assert_eq!(
        e.t.hutan(&[
            "task", "add", "X", "--ws", &ws, "--agent", "gemini", "--prompt", "hi"
        ])
        .await
        .code,
        2
    );

    let id =
        e.t.hutan(&[
            "task",
            "add",
            "Say hello",
            "--ws",
            &ws,
            "--agent",
            "omp",
            "--prompt",
            "Say hello.",
        ])
        .await
        .ok()
        .stdout
        .trim()
        .to_owned();
    let tasks =
        e.t.hutan(&["--json", "task", "ls", "--ws", &ws])
            .await
            .ok()
            .json();
    assert_eq!(
        tasks["tasks"][0]["run"],
        serde_json::json!({"kind": "chat", "agent": "omp"})
    );
    e.t.hutan(&["task", "start", &id]).await.ok();
    timeout(WAIT, async {
        loop {
            let tasks =
                e.t.hutan(&["--json", "task", "ls", "--ws", &ws])
                    .await
                    .ok()
                    .json();
            if tasks["tasks"][0]["state"] == "done" {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the task finishes with its turn");
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_shows_what_a_turn_used_and_what_the_harnesses_report() {
    let e = Env::start("plain-turn").await;
    // Nothing has run, and the fake OMP reports no limit windows.
    let none = e.t.hutan(&["usage"]).await.ok().stdout;
    assert!(none.starts_with("no usage yet"), "{none}");
    let created =
        e.t.hutan(&["chat", "new", "--agent", "omp", "--cwd", &e.cwd()])
            .await
            .ok();
    let id = created.stdout.split_whitespace().nth(1).unwrap().to_owned();
    e.t.hutan(&["chat", "send", &id, "Say hello.", "--wait"])
        .await
        .ok();
    // The turn is recorded as it ends: wait for the summary to show it.
    let summary = timeout(WAIT, async {
        loop {
            let json = e.t.hutan(&["--json", "usage", "--cached"]).await.ok();
            let value: Value = serde_json::from_str(json.stdout.trim()).unwrap();
            if value["harnesses"][0]["week"]["turns"] == 1 {
                return value;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the turn's usage");
    let omp = &summary["harnesses"][0];
    assert_eq!(omp["harness"], "omp");
    assert_eq!(omp["today"]["total_tokens"], 1234);
    assert_eq!(omp["today"]["cost_is_estimate"], true);
    let text = e.t.hutan(&["usage", "--cached"]).await.ok().stdout;
    assert!(
        text.contains("OMP") && text.contains("1234 tokens"),
        "{text}"
    );
    assert!(
        text.contains("(estimate)") && text.contains("no limit reported"),
        "{text}"
    );
    assert!(text.contains("gitlab-duo"), "{text}");
}

// Safety presets (SANDBOX-1) --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn chat_new_takes_a_safety_preset_and_prints_what_the_harness_reported() {
    let e = Env::start("plain-turn").await;
    let cwd = e.cwd();

    // OMP passes `--tools read,grep,glob` and says it cannot read that back.
    let out =
        e.t.hutan(&[
            "chat",
            "new",
            "--agent",
            "omp",
            "--cwd",
            &cwd,
            "--safety",
            "read_only",
        ])
        .await
        .ok();
    assert!(
        out.stdout.contains("safety read_only: enforced by harness")
            && !out.stdout.contains("NOT APPLIED"),
        "{}",
        out.stdout
    );
    let json =
        e.t.hutan(&[
            "chat", "new", "--agent", "omp", "--cwd", &cwd, "--safety", "full", "--json",
        ])
        .await
        .ok()
        .json();
    let safety = &json["session"]["chat"]["safety"];
    assert_eq!(safety["preset"], "full");
    assert_eq!(safety["enforced_by"], "none");

    // OMP's Contained is Hutan's Seatbelt: the session says who enforces it and the network.
    let contained =
        e.t.hutan(&[
            "chat",
            "new",
            "--agent",
            "omp",
            "--cwd",
            &cwd,
            "--safety",
            "contained",
            "--json",
        ])
        .await
        .ok()
        .json();
    let safety = &contained["session"]["chat"]["safety"];
    assert_eq!(safety["preset"], "contained");
    assert_eq!(safety["enforced_by"], "hutan");
    assert_eq!(safety["network"], true);
    assert_eq!(contained["session"]["containment"]["applied"], true);
    let text =
        e.t.hutan(&[
            "chat",
            "new",
            "--agent",
            "omp",
            "--cwd",
            &cwd,
            "--safety",
            "contained",
        ])
        .await
        .ok()
        .stdout;
    assert!(
        text.contains("safety contained: enforced by hutan, network on"),
        "{text}"
    );

    // clap: not a preset, and not together with the settings it replaces.
    for args in [
        ["--safety", "yolo"].as_slice(),
        ["--safety", "ask", "--sandbox", "read-only"].as_slice(),
        ["--safety", "ask", "--approval-policy", "never"].as_slice(),
        ["--safety", "ask", "--mode", "plan"].as_slice(),
    ] {
        let mut all = vec!["chat", "new", "--agent", "omp", "--cwd", &cwd];
        all.extend_from_slice(args);
        assert_eq!(e.t.hutan(&all).await.code, 2, "{args:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn task_add_takes_a_safety_preset_for_a_chat_and_the_daemon_refuses_what_the_harness_cannot()
{
    let e = Env::start("plain-turn").await;
    let repo = e.work.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let ws =
        e.t.hutan(&["project", "open", repo.to_str().unwrap(), "--json"])
            .await
            .ok()
            .json()["workspace"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
    let add = |safety: &'static str, agent: &'static str| {
        let ws = ws.clone();
        let e = &e;
        async move {
            e.t.hutan(&[
                "task", "add", "Review", "--ws", &ws, "--agent", agent, "--prompt", "Look.",
                "--safety", safety,
            ])
            .await
        }
    };
    // OMP's Contained is Hutan's Seatbelt, which this daemon offers: the task is made, and it
    // keeps the preset the chat will start with.
    let contained = add("contained", "omp").await.ok().stdout.trim().to_owned();
    assert!(!contained.is_empty());

    let id = add("read_only", "omp").await.ok().stdout.trim().to_owned();
    let tasks =
        e.t.hutan(&["--json", "task", "ls", "--ws", &ws])
            .await
            .ok()
            .json();
    let task = tasks["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| {
            t["id"]
                .as_str()
                .is_some_and(|i| i.starts_with(&id) || id.starts_with(i))
        })
        .unwrap()
        .clone();
    assert_eq!(task["run"]["kind"], "chat");
    assert_eq!(task["run"]["safety"], "read_only");

    // A preset is for a chat: not with a command, and not without a prompt.
    assert_eq!(
        e.t.hutan(&[
            "task", "add", "X", "--ws", &ws, "--safety", "ask", "--", "sh"
        ])
        .await
        .code,
        2
    );
}

// Agent insight (protocol 0.59): the recorded OMP 18.12.3 exchange, then `/context`.
#[tokio::test(flavor = "multi_thread")]
async fn chat_insight_prints_an_omp_chats_numbers_and_measures_its_context_on_request() {
    let e = Env::start("insight-omp").await;
    let created =
        e.t.hutan(&["chat", "new", "--agent", "omp", "--cwd", &e.cwd()])
            .await
            .ok();
    let id = created.stdout.split_whitespace().nth(1).unwrap().to_owned();
    for text in [
        "Read hello.txt and tell me what it says.",
        "Say it again briefly.",
    ] {
        e.t.hutan(&["chat", "send", &id, text, "--wait"]).await.ok();
    }

    let json =
        e.t.hutan(&["--json", "chat", "insight", &id])
            .await
            .ok()
            .json();
    assert_eq!(json["harness"], "omp");
    assert_eq!(json["context"]["used"], 11_200);
    assert_eq!(json["context"]["window"], 200_000);
    assert_eq!(json["totals"]["turns"], 2);
    assert_eq!(json["totals"]["input"], 9_400);
    assert_eq!(json["totals"]["cache_read"], 24_000);
    assert_eq!(json["totals"]["cost_kind"], "estimate");
    assert_eq!(json["can_measure"], true);
    assert!(
        json["context"].get("breakdown").is_none(),
        "not measured yet: {json}"
    );

    let text = e.t.hutan(&["chat", "insight", &id]).await.ok().stdout;
    assert!(
        text.contains("Context    11.2k tokens of 200k (6%)"),
        "{text}"
    );
    assert!(text.contains("by category: not measured"), "{text}");
    assert!(
        text.contains("Cost       ≈ $0.0364 (the agent's own estimate)"),
        "{text}"
    );
    assert!(text.contains("Tools      1 call: read ×1"), "{text}");

    let measured =
        e.t.hutan(&["chat", "insight", &id, "--measure"])
            .await
            .ok()
            .stdout;
    assert!(measured.contains("by category:"), "{measured}");
    assert!(measured.contains("System prompt"), "{measured}");
    assert!(measured.contains("2.6k"), "{measured}");
    assert!(measured.contains("(reserved for compaction)"), "{measured}");
    assert!(
        measured.contains("Tokens     2 turns"),
        "/context is not a turn: {measured}"
    );
    e.t.stop().await;
}
