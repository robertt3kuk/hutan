//! User hooks (protocol 0.48, capability `hooks`, `docs/integrations.md` § User hooks) end to end:
//! a real daemon on a temporary `HUTAN_HOME` whose `daemon.toml` holds `[[hooks]]`, real sessions,
//! chats on the committed Codex transcripts, and shell scripts as the hooks' commands. A hook
//! writes what it was given into a temporary directory; the tests read it back once the hook's
//! run is in `hooks.list`, never after a sleep.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{TestDaemon, WAIT, create, create_in, remote_code, sh};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::{ApprovalId, ApprovalPolicy, NodeOutcome, SandboxMode, SessionId, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ApprovalRespond, ChatCreate};
use hutan_protocol::fs::FsWriteBegin;
use hutan_protocol::hooks::{HookOutcome, HooksList, HooksLogRequest};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Empty, Origin, Request, SessionResult,
    WorkspaceInfo, WorkspaceOpen, WorkspaceOpened,
};
use hutan_protocol::task::{TaskCreate, TaskUpdate};
use hutan_protocol::ui::{
    AppearanceSummary, DockSummary, FocusSummary, LayoutSummary, PaneItem, PaneSummary, UiPublish,
};
use hutan_protocol::worktree::{MergeStrategy, WorktreeFinish};
use serde_json::{Value, json};
use tokio::time::sleep;

// Helpers -------------------------------------------------------------------------------------

/// A TOML basic string for `text`.
fn q(text: impl AsRef<str>) -> String {
    format!("{:?}", text.as_ref())
}

fn qp(path: &Path) -> String {
    q(path.to_str().unwrap())
}

/// Writes the payload to `<out>/<tag>.<pid>.json` and the `HUTAN_EVENT*` variables to
/// `<out>/<tag>.<pid>.env`.
const CAPTURE: &str =
    r#"cat > "$1/$2.$$.json"; printenv | grep '^HUTAN_EVENT' | sort > "$1/$2.$$.env""#;

/// A `[[hooks]]` entry that captures what it is given under `tag`; `extra` is more keys.
fn capture(on: &str, out: &Path, tag: &str, extra: &str) -> String {
    format!(
        "[[hooks]]\non = {on}\nrun = [\"/bin/sh\", \"-c\", {script}, \"sh\", {out}, {tag}]\n{extra}\n",
        on = q(on),
        script = q(CAPTURE),
        out = qp(out),
        tag = q(tag),
    )
}

/// A `[[hooks]]` entry that runs `script` with `sh -c`; `$1` is `out`.
fn script_hook(on: &str, out: &Path, script: &str, extra: &str) -> String {
    format!(
        "[[hooks]]\non = {on}\nrun = [\"/bin/sh\", \"-c\", {script}, \"sh\", {out}]\n{extra}\n",
        on = q(on),
        script = q(script),
        out = qp(out),
    )
}

/// The payloads captured under `tag`.
fn payloads(out: &Path, tag: &str) -> Vec<Value> {
    captured(out, tag, "json")
        .into_iter()
        .map(|text| serde_json::from_str(&text).unwrap())
        .collect()
}

fn captured(out: &Path, tag: &str, extension: &str) -> Vec<String> {
    let mut files: Vec<PathBuf> = fs::read_dir(out)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_str().unwrap();
            name.starts_with(&format!("{tag}.")) && name.ends_with(&format!(".{extension}"))
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect()
}

async fn list(client: &Client) -> HooksList {
    client.hooks_list().await.unwrap()
}

/// Waits until hook `index` has finished `runs` runs.
async fn wait_runs(client: &Client, index: usize, runs: u64) -> HooksList {
    wait_for(
        client,
        &format!("hook {index} to have run {runs} times"),
        |l| l.hooks[index].runs >= runs,
    )
    .await
}

async fn wait_for(
    client: &Client,
    what: &str,
    mut done: impl FnMut(&HooksList) -> bool,
) -> HooksList {
    let deadline = Instant::now() + WAIT;
    loop {
        let now = list(client).await;
        if done(&now) {
            return now;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {now:#?}"
        );
        sleep(Duration::from_millis(15)).await;
    }
}

/// Opens `path` as a project.
async fn open_project(client: &Client, path: &Path) -> WorkspaceInfo {
    client
        .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
            path: path.to_owned(),
            created_by: None,
        }))
        .await
        .unwrap()
        .workspace
}

/// A session of `project` that exits with `code`, titled `title`.
async fn exit_session(
    client: &Client,
    project: Option<WorkspaceId>,
    code: i32,
    title: &str,
) -> SessionId {
    let mut spec = sh(&format!("exit {code}"));
    spec.title = Some(title.to_owned());
    create_in(client, project, spec).await.id
}

fn files_in(dir: &Path, prefix: &str) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .starts_with(prefix)
        })
        .count()
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid.trim()])
        .output()
        .unwrap()
        .status
        .success()
}

/// The keys every payload has, removed after checking they are there.
fn take(payload: &mut Value, key: &str) -> Value {
    payload
        .as_object_mut()
        .unwrap()
        .remove(key)
        .unwrap_or_else(|| panic!("no `{key}` in {payload}"))
}

struct Env {
    daemon: TestDaemon,
    out: tempfile::TempDir,
    client: Client,
}

impl Env {
    /// A daemon whose `daemon.toml` is `toml(out)` and a user connection.
    async fn start(toml: impl FnOnce(&Path) -> String) -> Self {
        let out = tempfile::tempdir().unwrap();
        let text = toml(out.path());
        let daemon = TestDaemon::start_with(&text, |_| {}).await;
        let client = daemon.connect().await;
        Self {
            daemon,
            out,
            client,
        }
    }

    fn out(&self) -> &Path {
        self.out.path()
    }

    async fn stop(self) {
        drop(self.client);
        self.daemon.stop().await;
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/codex-0.162")
}

/// The fake app server, next to the test binaries; `just test` builds the workspace binaries
/// first.
fn fixture_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-codex-fixture");
    assert!(
        bin.is_file(),
        "hutan-codex-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

fn replay(name: &str, received: &Path) -> CodexConfig {
    CodexConfig {
        program: fixture_bin(),
        args: vec![
            "replay".to_owned(),
            "--received".to_owned(),
            received.display().to_string(),
            fixtures()
                .join(format!("{name}.jsonl"))
                .display()
                .to_string(),
            "app-server".to_owned(),
        ],
        env: Vec::new(),
    }
}

/// A daemon with hooks and a Codex that replays the committed transcript `fixture`.
async fn chat_env(fixture: &str, toml: impl FnOnce(&Path) -> String) -> (Env, tempfile::TempDir) {
    let out = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let text = toml(out.path());
    let config = replay(fixture, &work.path().join("received.jsonl"));
    let daemon = TestDaemon::start_with(&text, move |c| c.codex = Some(config)).await;
    let client = daemon.connect().await;
    (
        Env {
            daemon,
            out,
            client,
        },
        work,
    )
}

async fn start_chat(
    client: &Client,
    cwd: &Path,
    policy: Option<ApprovalPolicy>,
    sandbox: Option<SandboxMode>,
) -> SessionId {
    client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            contain: None,
            workspace_id: None,
            cwd: cwd.to_owned(),
            agent: "codex".to_owned(),
            model: None,
            approval_policy: policy,
            sandbox,
            title: Some("fix the bug".to_owned()),
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        }))
        .await
        .unwrap()
        .session
        .id
}

async fn report(client: &Client, request: AgentReportRequest) {
    let applied: AgentApplied = client.call(Request::AgentReport(request)).await.unwrap();
    assert!(applied.applied, "report ignored");
}

fn main_report(session: SessionId, state: AgentState, seq: u64) -> AgentReportRequest {
    AgentReportRequest {
        pid: None,
        session_id: session,
        source: "test-agent".into(),
        agent: "fake".into(),
        state: Some(state),
        message: None,
        seq,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }
}

// The events ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_session_that_exits_runs_its_hook_with_the_documented_payload_and_variables() {
    let env = Env::start(|out| capture("session.exited", out, "exit", "")).await;
    let project_dir = tempfile::tempdir().unwrap();
    let project = open_project(&env.client, project_dir.path()).await;
    let session = exit_session(&env.client, Some(project.id), 3, "build").await;

    let hooks = wait_runs(&env.client, 0, 1).await;
    let hook = &hooks.hooks[0];
    assert_eq!(hook.failures, 0);
    let last = hook.last_run.as_ref().unwrap();
    assert_eq!(last.outcome, HookOutcome::Ok);
    assert_eq!(last.exit_code, Some(0));
    assert_eq!(last.session_id, Some(session));
    assert!(!last.test);

    let mut payload = payloads(env.out(), "exit").remove(0);
    assert!(take(&mut payload, "time_ms").as_u64().unwrap() > 0);
    assert_eq!(take(&mut payload, "session_id"), json!(session.to_string()));
    assert_eq!(
        take(&mut payload, "project_id"),
        json!(project.id.to_string())
    );
    assert_eq!(
        payload,
        json!({
            "schema": 1,
            "event": "session.exited",
            "session_title": "build",
            "project": project.name,
            "state": "exited",
            "exit_code": 3,
        }),
        "the documented payload, nothing else"
    );

    // The same facts as variables: HUTAN_EVENT is the name, the rest HUTAN_EVENT_<FIELD>.
    let vars = captured(env.out(), "exit", "env").remove(0);
    let vars: Vec<&str> = vars.lines().collect();
    for expected in [
        "HUTAN_EVENT=session.exited",
        "HUTAN_EVENT_EXIT_CODE=3",
        "HUTAN_EVENT_SCHEMA=1",
        "HUTAN_EVENT_SESSION_TITLE=build",
        "HUTAN_EVENT_STATE=exited",
    ] {
        assert!(vars.contains(&expected), "{expected} in {vars:?}");
    }
    assert!(vars.contains(&format!("HUTAN_EVENT_SESSION_ID={session}").as_str()));
    assert!(vars.contains(&format!("HUTAN_EVENT_PROJECT={}", project.name).as_str()));

    // The log has the run, newest first.
    let log = env
        .client
        .hooks_log(HooksLogRequest::default())
        .await
        .unwrap();
    assert_eq!(log.runs.len(), 1);
    assert_eq!(log.runs[0], *last);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn titles_are_redacted_one_line_and_bounded_and_nothing_else_leaks() {
    let env = Env::start(|out| capture("session.exited", out, "t", "")).await;
    let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
    let title = format!("deploy token={secret}\n{}", "word ".repeat(100));
    exit_session(&env.client, None, 0, &title).await;
    wait_runs(&env.client, 0, 1).await;
    let payload = payloads(env.out(), "t").remove(0);
    let shown = payload["session_title"].as_str().unwrap();
    assert!(shown.chars().count() <= 120, "{shown}");
    assert!(!shown.contains('\n'));
    assert!(!shown.contains(&secret[..12]), "{shown}");
    let env_file = captured(env.out(), "t", "env").remove(0);
    assert!(!env_file.contains(&secret[..12]), "{env_file}");
    // No path, command or environment of the session is in it.
    let keys: Vec<&String> = payload.as_object().unwrap().keys().collect();
    for forbidden in ["cwd", "command", "env", "path", "transcript"] {
        assert!(keys.iter().all(|k| !k.contains(forbidden)), "{keys:?}");
    }
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_agents_turns_and_attention_run_their_hooks() {
    let env = Env::start(|out| {
        [
            capture("turn.finished", out, "done", ""),
            capture("turn.failed", out, "failed", ""),
            capture("attention.changed", out, "attn", ""),
        ]
        .concat()
    })
    .await;
    let finished = create(&env.client, sh("exec sleep 600")).await;
    let failed = create(&env.client, sh("exec sleep 600")).await;

    // A turn that ends normally.
    report(
        &env.client,
        main_report(finished.id, AgentState::Working, 1),
    )
    .await;
    report(&env.client, main_report(finished.id, AgentState::Idle, 2)).await;
    wait_runs(&env.client, 0, 1).await;
    let mut payload = payloads(env.out(), "done").remove(0);
    assert_eq!(payload["event"], "turn.finished");
    assert_eq!(payload["state"], "completed");
    assert_eq!(payload["session_id"], json!(finished.id.to_string()));
    // The agent program the session reported.
    assert_eq!(payload["harness"], "fake");
    assert!(payload["duration_ms"].is_u64());
    take(&mut payload, "time_ms");
    assert!(
        payload.get("changed_files").is_none(),
        "a terminal agent does not say"
    );

    // A turn that fails: the agent says it ended with an error.
    report(&env.client, main_report(failed.id, AgentState::Working, 1)).await;
    let released: AgentApplied = env
        .client
        .call(Request::AgentRelease(
            hutan_protocol::messages::AgentReleaseRequest {
                session_id: failed.id,
                source: "test-agent".into(),
                seq: 2,
                node: None,
                outcome: Some(NodeOutcome::Failed),
            },
        ))
        .await
        .unwrap();
    assert!(released.applied);
    wait_runs(&env.client, 1, 1).await;
    let payload = payloads(env.out(), "failed").remove(0);
    assert_eq!(payload["event"], "turn.failed");
    assert_eq!(payload["state"], "failed");
    assert_eq!(payload["session_id"], json!(failed.id.to_string()));
    assert_eq!(
        list(&env.client).await.hooks[0].runs,
        1,
        "a failure is no finish"
    );

    // An agent that blocks needs the user; one that goes on does not.
    report(
        &env.client,
        main_report(finished.id, AgentState::Blocked, 3),
    )
    .await;
    report(
        &env.client,
        main_report(finished.id, AgentState::Working, 4),
    )
    .await;
    wait_runs(&env.client, 2, 2).await;
    let attention: Vec<Value> = payloads(env.out(), "attn");
    let states: Vec<(&str, bool)> = attention
        .iter()
        .map(|p| {
            (
                p["state"].as_str().unwrap(),
                p["needs_attention"].as_bool().unwrap(),
            )
        })
        .collect();
    assert!(states.contains(&("attention", true)), "{states:?}");
    assert!(states.contains(&("clear", false)), "{states:?}");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_runs_its_hook_and_raises_attention_until_it_is_answered() {
    let (env, work) = chat_env("approval-touch", |out| {
        [
            capture("approval.requested", out, "ask", ""),
            capture("attention.changed", out, "attn", ""),
            capture("turn.finished", out, "done", ""),
        ]
        .concat()
    })
    .await;
    let session = start_chat(
        &env.client,
        work.path(),
        Some(ApprovalPolicy::Untrusted),
        Some(SandboxMode::ReadOnly),
    )
    .await;
    env.client
        .chat_send(
            session,
            "Run `touch hi.txt` in a shell, then reply with the word done.",
        )
        .await
        .unwrap();

    wait_runs(&env.client, 0, 1).await;
    let ask = payloads(env.out(), "ask").remove(0);
    assert_eq!(ask["event"], "approval.requested");
    assert_eq!(ask["state"], "pending");
    assert_eq!(ask["session_id"], json!(session.to_string()));
    assert_eq!(ask["session_title"], "fix the bug");
    assert_eq!(ask["harness"], "codex");
    assert!(
        ask["detail"].as_str().unwrap().contains("touch hi.txt"),
        "{ask}"
    );
    assert!(ask["detail"].as_str().unwrap().chars().count() <= 240);
    // Only the title: the approval's detail (the command line) is not handed over.
    assert!(ask.get("command").is_none() && ask.get("options").is_none());
    let approval: ApprovalId = ask["approval_id"].as_str().unwrap().parse().unwrap();
    wait_runs(&env.client, 1, 1).await;
    assert_eq!(payloads(env.out(), "attn")[0]["needs_attention"], true);

    // Answering clears the attention and lets the turn end.
    env.client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval,
            option_id: "accept".to_owned(),
        }))
        .await
        .unwrap();
    wait_runs(&env.client, 2, 1).await;
    wait_runs(&env.client, 1, 2).await;
    let done = payloads(env.out(), "done").remove(0);
    assert_eq!(done["event"], "turn.finished");
    assert_eq!(done["harness"], "codex");
    assert_eq!(done["session_title"], "fix the bug");
    let attention = payloads(env.out(), "attn");
    assert!(attention.iter().any(|p| p["needs_attention"] == false));
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_turn_says_how_many_files_it_changed_and_a_failed_turn_has_its_own_event() {
    let (env, work) = chat_env("file-change", |out| {
        [
            capture("turn.finished", out, "done", ""),
            capture("approval.requested", out, "ask", ""),
        ]
        .concat()
    })
    .await;
    let session = start_chat(
        &env.client,
        work.path(),
        Some(ApprovalPolicy::Untrusted),
        Some(SandboxMode::ReadOnly),
    )
    .await;
    env.client
        .chat_send(session, "change a file")
        .await
        .unwrap();
    wait_runs(&env.client, 1, 1).await;
    let approval: ApprovalId = payloads(env.out(), "ask")[0]["approval_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    env.client
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval,
            option_id: "accept".to_owned(),
        }))
        .await
        .unwrap();
    wait_runs(&env.client, 0, 1).await;
    let done = payloads(env.out(), "done").remove(0);
    assert_eq!(done["changed_files"], 1, "{done}");
    env.stop().await;

    let (env, work) = chat_env("turn-failed", |out| {
        [
            capture("turn.finished", out, "done", ""),
            capture("turn.failed", out, "failed", ""),
        ]
        .concat()
    })
    .await;
    let session = start_chat(&env.client, work.path(), None, None).await;
    env.client.chat_send(session, "fail").await.unwrap();
    wait_runs(&env.client, 1, 1).await;
    let failed = payloads(env.out(), "failed").remove(0);
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["session_id"], json!(session.to_string()));
    // The failure's message is the model's or the network's words: not part of the payload.
    assert!(
        failed.get("detail").is_none() && failed.get("message").is_none(),
        "{failed}"
    );
    assert_eq!(list(&env.client).await.hooks[0].runs, 0);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_that_is_done_runs_its_hook_with_its_title_and_never_its_prompt() {
    let env = Env::start(|out| capture("task.done", out, "task", "")).await;
    let dir = tempfile::tempdir().unwrap();
    let project = open_project(&env.client, dir.path()).await;
    let task = env
        .client
        .task_create(TaskCreate {
            workspace_id: project.id,
            title: "Ship the release".to_owned(),
            prompt: Some("a prompt with the secret plan".to_owned()),
            depends_on: Vec::new(),
            placement: None,
            run: None,
            auto_start: false,
            after_policy: Default::default(),
        })
        .await
        .unwrap();
    env.client
        .task_update(TaskUpdate {
            id: task.id,
            title: None,
            prompt: None,
            depends_on: None,
            placement: None,
            run: None,
            auto_start: None,
            after_policy: None,
            state: Some(hutan_core::TaskState::Done),
        })
        .await
        .unwrap();
    wait_runs(&env.client, 0, 1).await;
    let payload = payloads(env.out(), "task").remove(0);
    assert_eq!(payload["event"], "task.done");
    assert_eq!(payload["state"], "done");
    assert_eq!(payload["task_id"], json!(task.id.to_string()));
    assert_eq!(payload["task_title"], "Ship the release");
    assert_eq!(payload["project"], json!(project.name));
    assert!(!payload.to_string().contains("secret plan"));
    env.stop().await;
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finish_runs_its_hook_with_the_steps_and_the_project() {
    let env = Env::start(|out| {
        format!(
            "[finish]\npush = false\n{}",
            capture("finish.done", out, "finish", "")
        )
    })
    .await;
    let repo = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(repo.path()).unwrap().join("shop");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(&root, &["config", key, value]);
    }
    fs::write(root.join("a.txt"), "one\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    let project = open_project(&env.client, &root).await;
    fs::write(root.join("b.txt"), "two\n").unwrap();
    fs::write(root.join("c.txt"), "three\n").unwrap();
    let finished = env
        .client
        .worktree_finish(WorktreeFinish {
            checkout: root.clone(),
            message: "Add b and c".to_owned(),
            push: Some(false),
            into: None,
            strategy: MergeStrategy::Merge,
        })
        .await
        .unwrap();
    assert!(!finished.failed());
    wait_runs(&env.client, 0, 1).await;
    let payload = payloads(env.out(), "finish").remove(0);
    assert_eq!(payload["event"], "finish.done");
    assert_eq!(payload["state"], "ok");
    assert_eq!(
        payload["detail"], "shop",
        "the checkout's name, not its path"
    );
    assert_eq!(payload["branch"], "main");
    assert_eq!(payload["commit_step"], "ok");
    assert_eq!(payload["changed_files"], 2);
    assert_eq!(payload["project"], json!(project.name));
    assert!(
        payload.get("merge_step").is_none(),
        "a main checkout has no merge"
    );
    assert!(
        !payload.to_string().contains(root.to_str().unwrap()),
        "no path in {payload}"
    );
    env.stop().await;
}

// Filters, focus, repositories -----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn filters_pick_the_hooks_of_a_project_and_a_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap().join("shop");
    fs::create_dir(&path).unwrap();
    let env = Env::start(|out| {
        [
            capture("session.exited", out, "any", ""),
            capture("session.exited", out, "name", "project = \"SHOP\""),
            capture("session.exited", out, "other", "project = \"nope\""),
            capture("session.exited", out, "harness", "harness = \"claude\""),
            capture("session.exited", out, "workspace", "workspace = \"nope\""),
            capture("turn.finished", out, "event", ""),
        ]
        .concat()
    })
    .await;
    let project = open_project(&env.client, &path).await;
    assert_eq!(project.name, "shop");
    exit_session(&env.client, Some(project.id), 0, "one").await;
    // Every hook is offered the event in one pass, so once two have run, the others were
    // decided on: a hook that did not match was never queued.
    wait_runs(&env.client, 0, 1).await;
    wait_runs(&env.client, 1, 1).await;
    let hooks = list(&env.client).await;
    for index in 2..6 {
        assert_eq!(hooks.hooks[index].runs, 0, "hook {index} must not match");
        assert_eq!(hooks.hooks[index].dropped, 0);
    }
    assert_eq!(payloads(env.out(), "name")[0]["project"], "shop");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_harness_filter_matches_the_agent_the_session_reported() {
    let env = Env::start(|out| {
        [
            capture("turn.finished", out, "fake", "harness = \"FAKE\""),
            capture("turn.finished", out, "claude", "harness = \"claude\""),
            capture("turn.finished", out, "any", ""),
        ]
        .concat()
    })
    .await;
    let session = create(&env.client, sh("exec sleep 600")).await;
    report(&env.client, main_report(session.id, AgentState::Working, 1)).await;
    report(&env.client, main_report(session.id, AgentState::Idle, 2)).await;
    wait_runs(&env.client, 0, 1).await;
    wait_runs(&env.client, 2, 1).await;
    assert_eq!(list(&env.client).await.hooks[1].runs, 0);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_is_matched_by_its_id_too() {
    let dir = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    let out = tempfile::tempdir().unwrap();
    // The id is only known once the project exists: the daemon starts, the project is opened,
    // and a second daemon on the same home then reads the hook that names it.
    let first = TestDaemon::start().await;
    let client = first.connect().await;
    let project = open_project(&client, &path).await;
    drop(client);
    let toml = capture(
        "session.exited",
        out.path(),
        "id",
        &format!("project = {}", q(project.id.to_string())),
    );
    fs::write(&first.paths.config_file, toml).unwrap();
    let second = first.restart().await;
    let client = second.connect().await;
    exit_session(&client, Some(project.id), 0, "x").await;
    wait_runs(&client, 0, 1).await;
    assert_eq!(
        payloads(out.path(), "id")[0]["project_id"],
        json!(project.id.to_string())
    );
    drop(client);
    second.stop().await;
}

fn layout(window: &str, active: bool) -> UiPublish {
    UiPublish {
        actions: Vec::new(),
        settings: Vec::new(),
        layout: LayoutSummary {
            window_id: window.to_owned(),
            workspace_id: None,
            preset: "default".into(),
            docks: vec![DockSummary {
                position: "left".into(),
                visible: true,
                panels: vec!["sessions".into()],
                active: Some("sessions".into()),
            }],
            panes: PaneSummary::Pane {
                pane_id: "pane-1".into(),
                items: vec![PaneItem {
                    kind: "placeholder".into(),
                    session_id: None,
                    panel: None,
                    path: None,
                    title: "Welcome".into(),
                }],
                active: Some(0),
            },
            focused: FocusSummary {
                kind: "pane".into(),
                target: "pane-1".into(),
            },
            zoomed: None,
            appearance: AppearanceSummary {
                chrome: "system".into(),
                density: "compact".into(),
            },
            window_active: active,
        },
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hook_with_when_focused_false_waits_out_a_focused_window() {
    let env = Env::start(|out| {
        [
            capture("session.exited", out, "always", ""),
            capture("session.exited", out, "away", "when_focused = false"),
        ]
        .concat()
    })
    .await;
    let desktop = env.daemon.connect().await;
    desktop
        .call::<Empty>(Request::UiPublish(layout("w1", true)))
        .await
        .unwrap();
    exit_session(&env.client, None, 0, "while focused").await;
    wait_runs(&env.client, 0, 1).await;
    let hooks = list(&env.client).await;
    assert_eq!(hooks.hooks[1].runs, 0);
    assert_eq!(hooks.hooks[1].skipped, 1);
    assert_eq!(hooks.hooks[0].skipped, 0);

    // The window loses the focus: the hook runs again.
    desktop
        .call::<Empty>(Request::UiPublish(layout("w1", false)))
        .await
        .unwrap();
    exit_session(&env.client, None, 0, "while away").await;
    wait_runs(&env.client, 1, 1).await;
    assert_eq!(list(&env.client).await.hooks[1].skipped, 1);
    assert_eq!(
        payloads(env.out(), "away")[0]["session_title"],
        "while away"
    );
    drop(desktop);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hooks_in_a_repository_are_never_read() {
    let project = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(project.path()).unwrap();
    let evil = tempfile::tempdir().unwrap();
    let hook = capture("session.exited", evil.path(), "evil", "");
    fs::create_dir(root.join(".hutan")).unwrap();
    for name in [
        ".hutan/daemon.toml",
        ".hutan/hooks.toml",
        ".hutan/hutan.toml",
        ".hutan/settings.toml",
        "daemon.toml",
        "hutan.toml",
    ] {
        fs::write(root.join(name), &hook).unwrap();
    }
    let env = Env::start(|out| capture("session.exited", out, "mine", "")).await;
    let workspace = open_project(&env.client, &root).await;
    let mut spec = sh("exit 0");
    spec.cwd = Some(root.clone());
    create_in(&env.client, Some(workspace.id), spec).await;
    let hooks = wait_runs(&env.client, 0, 1).await;
    assert_eq!(hooks.hooks.len(), 1, "only the user's own file counts");
    assert_eq!(
        files_in(evil.path(), "evil"),
        0,
        "a repository's hook never ran"
    );
    env.stop().await;
}

// Limits ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_hook_past_its_timeout_is_killed_with_everything_it_started() {
    let env = Env::start(|out| {
        script_hook(
            "session.exited",
            out,
            r#"sleep 600 & echo $! > "$1/grandchild"; echo $$ > "$1/child"; wait"#,
            "timeout = 1",
        )
    })
    .await;
    exit_session(&env.client, None, 0, "x").await;
    let hooks = wait_runs(&env.client, 0, 1).await;
    let run = hooks.hooks[0].last_run.clone().unwrap();
    assert_eq!(run.outcome, HookOutcome::TimedOut);
    assert_eq!(run.exit_code, None);
    assert_eq!(run.error.as_deref(), Some("killed after 1 s"));
    assert!(run.duration_ms >= 1000, "{}", run.duration_ms);
    assert_eq!(hooks.hooks[0].failures, 1);
    for name in ["child", "grandchild"] {
        let pid = fs::read_to_string(env.out().join(name)).unwrap();
        let deadline = Instant::now() + WAIT;
        while alive(&pid) {
            assert!(Instant::now() < deadline, "{name} {pid} is still running");
            sleep(Duration::from_millis(10)).await;
        }
    }
    env.stop().await;
}

/// Starts, then waits for `<out>/go`.
const BLOCKING: &str = r#"touch "$1/started.$$"; while [ ! -e "$1/go" ]; do sleep 0.05; done"#;

#[tokio::test(flavor = "multi_thread")]
async fn at_most_four_hooks_run_at_once() {
    let env = Env::start(|out| {
        (0..6)
            .map(|_| script_hook("session.exited", out, BLOCKING, ""))
            .collect()
    })
    .await;
    exit_session(&env.client, None, 0, "x").await;
    let busy = wait_for(&env.client, "four hooks running", |l| l.running == 4).await;
    assert_eq!(busy.max_concurrent, 4);
    // Four started; the two others wait for a free place. Every wait below is on state.
    let deadline = Instant::now() + WAIT;
    while files_in(env.out(), "started.") < 4 {
        assert!(Instant::now() < deadline, "four hooks never started");
        sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(list(&env.client).await.running, 4);
    assert_eq!(
        files_in(env.out(), "started."),
        4,
        "the fifth waits for a place"
    );

    fs::write(env.out().join("go"), "").unwrap();
    let done = wait_for(&env.client, "all six to finish", |l| {
        l.hooks.iter().all(|h| h.runs == 1)
    })
    .await;
    assert_eq!(done.running, 0);
    assert_eq!(files_in(env.out(), "started."), 6);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_queue_drops_events_and_counts_them() {
    let env = Env::start(|out| script_hook("session.exited", out, BLOCKING, "")).await;
    exit_session(&env.client, None, 0, "first").await;
    // The first event is taken by the worker and runs; the queue is empty again.
    let deadline = Instant::now() + WAIT;
    while files_in(env.out(), "started.") < 1 {
        assert!(Instant::now() < deadline, "the hook never started");
        sleep(Duration::from_millis(10)).await;
    }
    let capacity = list(&env.client).await.queue_capacity;
    assert_eq!(capacity, 8);
    for n in 0..capacity + 3 {
        exit_session(&env.client, None, 0, &format!("more {n}")).await;
    }
    let hooks = wait_for(&env.client, "three dropped events", |l| {
        l.hooks[0].dropped >= 3
    })
    .await;
    assert_eq!(hooks.hooks[0].dropped, 3, "eight wait, the others are lost");
    assert_eq!(hooks.hooks[0].runs, 0);

    fs::write(env.out().join("go"), "").unwrap();
    let hooks = wait_runs(&env.client, 0, 9).await;
    assert_eq!(hooks.hooks[0].dropped, 3);
    assert_eq!(
        hooks.hooks[0].runs, 9,
        "the running one and the eight that waited"
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn output_is_captured_redacted_and_bounded_in_the_log() {
    let env = Env::start(|out| {
        [
            script_hook(
                "session.exited",
                out,
                r#"cat >/dev/null; echo "token=abcdefghijklmnopqrstuvwxyz0123456789"; head -c 300000 /dev/zero | tr '\0' a; echo oops >&2; exit 2"#,
                "",
            ),
            script_hook("session.exited", out, "exit 0", ""),
        ]
        .concat()
    })
    .await;
    exit_session(&env.client, None, 0, "x").await;
    wait_runs(&env.client, 0, 1).await;
    wait_runs(&env.client, 1, 1).await;
    let log = env
        .client
        .hooks_log(HooksLogRequest {
            hook: Some(0),
            limit: None,
        })
        .await
        .unwrap();
    let run = &log.runs[0];
    assert_eq!(run.outcome, HookOutcome::Failed);
    assert_eq!(run.exit_code, Some(2));
    assert!(run.output_truncated);
    assert!(run.stdout.len() <= 4096, "{}", run.stdout.len());
    assert!(
        !run.stdout.contains("abcdefghijkl"),
        "redacted: {}",
        &run.stdout[..80]
    );
    assert_eq!(run.stderr, "oops\n");
    // Only that hook's runs, newest first, bounded by `limit`.
    let all = env
        .client
        .hooks_log(HooksLogRequest::default())
        .await
        .unwrap();
    assert_eq!(all.runs.len(), 2);
    assert!(all.runs[0].id > all.runs[1].id);
    assert_eq!(all.capacity, 200);
    let one = env
        .client
        .hooks_log(HooksLogRequest {
            hook: None,
            limit: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(one.runs.len(), 1);
    env.stop().await;
}

// hooks.test -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_test_runs_the_hook_with_a_sample_payload_and_reports_how_it_went() {
    let env = Env::start(|out| {
        [
            script_hook(
                "turn.finished",
                out,
                r#"cat > "$1/test.json"; echo hello; echo oops >&2; exit 4"#,
                "harness = \"claude\"\nproject = \"shop\"",
            ),
            "[[hooks]]\non = \"session.exited\"\nrun = [\"/nonexistent/hutan-hook\"]\n".to_owned(),
            "[[hooks]]\non = \"task.done\"\nshell = true\nrun = [\"echo $HUTAN_EVENT $HUTAN_EVENT_TEST $HUTAN_EVENT_TASK_TITLE\"]\n".to_owned(),
        ]
        .concat()
    })
    .await;
    let tested = env.client.hooks_test(0).await.unwrap();
    assert_eq!(tested.run.outcome, HookOutcome::Failed);
    assert_eq!(tested.run.exit_code, Some(4));
    assert_eq!(tested.run.stdout, "hello\n");
    assert_eq!(tested.run.stderr, "oops\n");
    assert!(tested.run.test);
    assert_eq!(
        tested.payload.event,
        hutan_protocol::hooks::HookEvent::TurnFinished
    );
    assert!(tested.payload.test);
    assert_eq!(tested.payload.harness.as_deref(), Some("claude"));
    assert_eq!(tested.payload.project.as_deref(), Some("shop"));
    // The command got exactly that payload on its standard input.
    let seen: Value =
        serde_json::from_str(&fs::read_to_string(env.out().join("test.json")).unwrap()).unwrap();
    assert_eq!(seen, serde_json::to_value(&tested.payload).unwrap());
    assert_eq!(seen["test"], true);

    let missing = env.client.hooks_test(1).await.unwrap();
    assert_eq!(missing.run.outcome, HookOutcome::SpawnFailed);
    assert!(
        missing
            .run
            .error
            .unwrap()
            .contains("/nonexistent/hutan-hook")
    );

    let shell = env.client.hooks_test(2).await.unwrap();
    assert_eq!(shell.run.outcome, HookOutcome::Ok);
    assert_eq!(shell.run.stdout, "task.done true Sample task\n");

    let hooks = list(&env.client).await;
    assert_eq!(
        hooks
            .hooks
            .iter()
            .map(|h| (h.runs, h.failures))
            .collect::<Vec<_>>(),
        [(1, 1), (1, 1), (1, 0)]
    );
    assert!(hooks.hooks[0].last_run.as_ref().unwrap().test);
    assert_eq!(
        remote_code(env.client.hooks_test(3).await),
        ErrorCode::NotFound
    );
    let log = env
        .client
        .hooks_log(HooksLogRequest::default())
        .await
        .unwrap();
    assert_eq!(log.runs.len(), 3, "tests are in the log");
    env.stop().await;
}

// Who may touch hooks --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_cannot_read_run_or_change_hooks() {
    let env = Env::start(|out| {
        format!(
            "[control]\nagents = \"allow\"\n{}",
            capture("session.exited", out, "mine", "")
        )
    })
    .await;
    let before = fs::read_to_string(&env.daemon.paths.config_file).unwrap();
    // An agent reported itself in this terminal session: its connection is an agent's.
    let session = create(&env.client, sh("exec sleep 600")).await;
    report(&env.client, main_report(session.id, AgentState::Working, 1)).await;
    let mut options = env.daemon.options();
    options.client.origin = Some(Origin {
        session_id: session.id,
    });
    let agent = Client::connect(options).await.unwrap();

    let user_only = |error: hutan_client::ClientError| match error {
        hutan_client::ClientError::Remote(body) => {
            assert_eq!(body.code, ErrorCode::Forbidden, "{body:?}");
            assert_eq!(
                body.data.as_ref().unwrap()["control"]["decision"],
                "user_only"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    };
    // A command line can carry a token: listing, the log and running a test are the user's.
    user_only(agent.hooks_list().await.unwrap_err());
    user_only(
        agent
            .hooks_log(HooksLogRequest::default())
            .await
            .unwrap_err(),
    );
    user_only(agent.hooks_test(0).await.unwrap_err());
    // There is no request that adds or changes a hook, and the one that writes `daemon.toml` is
    // the user's too, whatever the key.
    for key in ["hooks", "hooks.0.run", "control.agents"] {
        user_only(
            agent
                .daemon_setting_set(
                    key,
                    Some(json!([{"on": "turn.finished", "run": ["touch", "x"]}])),
                )
                .await
                .unwrap_err(),
        );
    }
    // Not through the file service either, though `agents = "allow"`: the file is outside the
    // folders the daemon tracks, and `picked` counts for nothing from an agent.
    let denied = agent
        .call::<Value>(Request::FsWriteBegin(FsWriteBegin {
            path: env.daemon.paths.config_file.clone(),
            expect: None,
            create: false,
            size_hint: None,
            picked: true,
        }))
        .await;
    assert_eq!(remote_code(denied), ErrorCode::Forbidden);
    assert_eq!(
        fs::read_to_string(&env.daemon.paths.config_file).unwrap(),
        before,
        "daemon.toml is unchanged"
    );

    // The user still can, from a plain connection.
    assert_eq!(list(&env.client).await.hooks.len(), 1);
    assert_eq!(
        env.client.hooks_test(0).await.unwrap().run.outcome,
        HookOutcome::Ok
    );
    drop(agent);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hooks_a_daemon_runs_are_the_ones_it_started_with() {
    // A `[[hooks]]` entry that is not valid stops the start with a message that names it: a
    // typo does not silently run nothing.
    let dir = tempfile::tempdir().unwrap();
    let paths = hutan_daemon::Paths::in_home(dir.path());
    fs::write(
        &paths.config_file,
        "[[hooks]]\non = \"turn.finished\"\nrun = [\"true\"]\ntimeout = 301\n",
    )
    .unwrap();
    let Err(error) = hutan_daemon::start(hutan_daemon::DaemonConfig::new(paths)).await else {
        panic!("an invalid hook stops the start");
    };
    let error = error.to_string();
    assert!(error.contains("timeout"), "{error}");
}

// The ntfy recipe -------------------------------------------------------------------------------

/// What a server on the loopback interface was sent: the request line, the headers, the body.
struct Received {
    request_line: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Received {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A one-request HTTP server standing in for ntfy: the address it listens on, and what it got.
fn local_ntfy() -> (String, std::sync::mpsc::Receiver<Received>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut headers = Vec::new();
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end().to_owned();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
                headers.push((name.to_owned(), value.trim().to_owned()));
            }
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .unwrap();
        let _ = tx.send(Received {
            request_line: request_line.trim_end().to_owned(),
            headers,
            body: String::from_utf8(body).unwrap(),
        });
    });
    (address, rx)
}

async fn received(rx: std::sync::mpsc::Receiver<Received>) -> Received {
    tokio::task::spawn_blocking(move || rx.recv_timeout(WAIT))
        .await
        .unwrap()
        .expect("the local ntfy server was never called")
}

/// The recipe of `docs/integrations.md` § User hooks, with the server's address for
/// `https://ntfy.sh` (and curl told to ignore the proxy of the environment).
fn ntfy_hook(on: &str, address: &str, home: &Path) -> String {
    let command = r#"curl -s --max-time 10 --noproxy '*' \
  -H "Title: Hutan: approval needed" -H "Priority: high" -H "Tags: lock" \
  --data-raw "$HUTAN_EVENT_SESSION_TITLE: $HUTAN_EVENT_DETAIL" \
  "http://ADDRESS/$(cat TOPIC)""#
        .replace("ADDRESS", address)
        .replace("TOPIC", home.join("ntfy-topic").to_str().unwrap());
    format!(
        "[[hooks]]\nname = \"Phone: an agent is waiting\"\non = {on}\nshell = true\nrun = [{command}]\nwhen_focused = false\n",
        on = q(on),
        command = q(command),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ntfy_recipe_posts_the_kind_and_the_title_to_a_local_server() {
    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join("ntfy-topic"), "0123456789abcdef\n").unwrap();
    let (address, rx) = local_ntfy();
    let toml = ntfy_hook("approval.requested", &address, home.path());
    let (env, work) = chat_env("approval-touch", move |_| toml).await;
    let session = start_chat(
        &env.client,
        work.path(),
        Some(ApprovalPolicy::Untrusted),
        Some(SandboxMode::ReadOnly),
    )
    .await;
    env.client
        .chat_send(
            session,
            "Run `touch hi.txt` in a shell, then reply with the word done.",
        )
        .await
        .unwrap();
    let got = received(rx).await;
    assert_eq!(got.request_line, "POST /0123456789abcdef HTTP/1.1");
    assert_eq!(got.header("Title"), Some("Hutan: approval needed"));
    assert_eq!(got.header("Priority"), Some("high"));
    assert_eq!(got.header("Tags"), Some("lock"));
    // The session's title and the approval's title: nothing of the conversation.
    assert!(got.body.starts_with("fix the bug: "), "{}", got.body);
    assert!(got.body.contains("touch hi.txt"), "{}", got.body);
    assert!(got.body.chars().count() <= 120 + 2 + 240, "{}", got.body);
    let hooks = wait_runs(&env.client, 0, 1).await;
    assert_eq!(
        hooks.hooks[0].last_run.as_ref().unwrap().outcome,
        HookOutcome::Ok
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_title_that_starts_with_an_at_sign_is_sent_as_text_not_as_a_file() {
    // `curl -d @file` would send the file; the recipe uses `--data-raw`.
    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join("ntfy-topic"), "topic\n").unwrap();
    let secret = home.path().join("private.txt");
    fs::write(&secret, "the contents of a private file").unwrap();
    let (address, rx) = local_ntfy();
    let toml = ntfy_hook("session.exited", &address, home.path());
    let env = Env::start(move |_| toml).await;
    exit_session(&env.client, None, 0, &format!("@{}", secret.display())).await;
    let got = received(rx).await;
    assert!(got.body.starts_with('@'), "{}", got.body);
    assert!(
        !got.body.contains("contents of a private file"),
        "{}",
        got.body
    );
    env.stop().await;
}
