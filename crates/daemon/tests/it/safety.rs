//! Safety presets (protocol 0.51, capability `chat.safety`, ADR 0021) end to end: a real daemon
//! on a temporary `HUTAN_HOME`, real client connections, the fixture harnesses standing in for
//! `omp acp`, Claude Code's ACP adapter and `codex app-server`. Each test asserts the exact
//! arguments, session modes, `_meta` and thread parameters a preset makes, and the effective
//! state the chat reports back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, remote_code};
use hutan_client::Client;
use hutan_core::{ApprovalPolicy, SafetyEnforcer, SafetyPreset, SandboxMode, Seq, TaskRun};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ChatCreate, ChatSafety};
use hutan_protocol::messages::{
    Event, Request, SessionInfo, SessionList, SessionListFilter, SessionResult, WorkspaceCreate,
    WorkspaceInfo,
};
use hutan_protocol::task::TaskCreate;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::timeout;

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
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"))
}

fn codex_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/codex-0.162")
        .join(format!("{name}.jsonl"))
}

/// A wrapper standing in for `omp` / the Claude Code adapter: it appends the arguments it was
/// started with to `$ARGV_LOG`, one per line and a `--spawn--` line after them, and becomes the
/// fake agent (`$FIXTURE`). One file of fixed content in the system temp directory, so the
/// system vets it once, not once per test.
fn argv_wrapper() -> PathBuf {
    static WRAPPER: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| {
        let text = "#!/bin/sh\n{ printf '%s\\n' \"$@\"; echo --spawn--; } >> \"$ARGV_LOG\"\nexec \"$FIXTURE\" \"$@\"\n";
        let dir = std::env::temp_dir().join("hutan-test-fake-clis");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("safety-argv-wrapper");
        if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
            return path;
        }
        let staging = dir.join(format!(".{}-safety-argv-wrapper", std::process::id()));
        std::fs::write(&staging, text).unwrap();
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&staging, &path).unwrap();
        path
    });
    WRAPPER.clone()
}

/// Which transcript each fake harness replays; `None` leaves the harness out of `daemon.toml`.
#[derive(Default)]
struct Setup {
    omp: Option<&'static str>,
    claude: Option<&'static str>,
    codex: Option<&'static str>,
}

struct S {
    daemon: TestDaemon,
    work: tempfile::TempDir,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
}

impl S {
    async fn start(setup: Setup) -> Self {
        let work = tempfile::tempdir().unwrap();
        let wrapper = argv_wrapper();
        let acp = bin("hutan-acp-fixture");
        let harness = |id: &str, transcript: &str| {
            format!(
                "[harness.{id}]\nbin = {wrapper:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\nenv = {{ ARGV_LOG = {log:?}, FIXTURE = {acp:?} }}\n\n",
                received = work.path().join(format!("{id}-received.jsonl")),
                transcript = acp_fixture(transcript),
                log = work.path().join(format!("{id}-argv.log")),
            )
        };
        let mut toml = "[terminal]\nenv_inherit = \"daemon\"\n\n".to_owned();
        if let Some(name) = setup.omp {
            toml.push_str(&harness("omp", name));
        }
        if let Some(name) = setup.claude {
            toml.push_str(&harness("claude", name));
        }
        if let Some(name) = setup.codex {
            toml.push_str(&format!(
                "[codex]\nbin = {codex:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"app-server\"]\n\n",
                codex = bin("hutan-codex-fixture"),
                received = work.path().join("codex-received.jsonl"),
                transcript = codex_fixture(name),
            ));
        }
        let daemon = TestDaemon::start_with(&toml, |_| {}).await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    fn request(&self, agent: &str, safety: Option<SafetyPreset>) -> ChatCreate {
        ChatCreate {
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
            safety,
        }
    }

    async fn try_create(&self, req: ChatCreate) -> Result<SessionInfo, hutan_client::ClientError> {
        self.client
            .call::<SessionResult>(Request::ChatCreate(req))
            .await
            .map(|r| r.session)
    }

    async fn create(&self, agent: &str, safety: Option<SafetyPreset>) -> SessionInfo {
        self.try_create(self.request(agent, safety)).await.unwrap()
    }

    /// What the harness was started with, one entry per spawn.
    fn spawns(&self, id: &str) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(self.work.path().join(format!("{id}-argv.log")))
            .unwrap_or_default();
        let mut spawns = Vec::new();
        let mut current = Vec::new();
        for line in text.lines() {
            if line == "--spawn--" {
                spawns.push(std::mem::take(&mut current));
            } else {
                current.push(line.to_owned());
            }
        }
        spawns
    }

    /// The arguments after the fixture's own (`replay --received FILE TRANSCRIPT acp`).
    fn extras(&self, id: &str) -> Vec<Vec<String>> {
        self.spawns(id)
            .into_iter()
            .map(|argv| argv.into_iter().skip(5).collect())
            .collect()
    }

    fn received_of(&self, file: &str, method: &str) -> Vec<Value> {
        std::fs::read_to_string(self.work.path().join(file))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["msg"].clone())
            .filter(|msg| msg["method"] == method)
            .collect()
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

fn safety_of(session: &SessionInfo) -> ChatSafety {
    session.chat.clone().unwrap().safety.expect("chat safety")
}

// OMP -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn omp_gets_the_approval_mode_or_tool_list_of_its_preset_and_says_it_cannot_confirm_it() {
    let t = S::start(Setup {
        omp: Some("plain-turn"),
        ..Setup::default()
    })
    .await;
    assert!(t.client.supports_chat_safety());

    let none = t.create("omp", None).await;
    assert!(none.chat.unwrap().safety.is_none(), "no preset, no claim");
    let ask = t.create("omp", Some(SafetyPreset::Ask)).await;
    let read_only = t.create("omp", Some(SafetyPreset::ReadOnly)).await;
    let full = t.create("omp", Some(SafetyPreset::Full)).await;

    let extras = t.extras("omp");
    assert_eq!(extras.len(), 4, "{extras:?}");
    assert!(extras[0].is_empty(), "without a preset OMP starts bare");
    assert_eq!(extras[1], ["--approval-mode", "always-ask"]);
    assert_eq!(extras[2], ["--tools", "read,grep,glob"]);
    assert_eq!(extras[3], ["--approval-mode", "yolo"]);

    let ask = safety_of(&ask);
    assert_eq!(
        (ask.preset, ask.enforced_by),
        (SafetyPreset::Ask, SafetyEnforcer::Harness)
    );
    assert!(ask.note.unwrap().contains("does not report it back"));
    let read_only = safety_of(&read_only);
    assert_eq!(read_only.enforced_by, SafetyEnforcer::Harness);
    assert!(read_only.note.unwrap().contains("not an OS boundary"));
    let full = safety_of(&full);
    assert_eq!(
        (full.preset, full.enforced_by),
        (SafetyPreset::Full, SafetyEnforcer::None),
        "full access has no boundary"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn omp_refuses_contained_with_the_reason_and_starts_nothing() {
    let t = S::start(Setup {
        omp: Some("plain-turn"),
        ..Setup::default()
    })
    .await;
    let error = t
        .try_create(t.request("omp", Some(SafetyPreset::Contained)))
        .await
        .unwrap_err();
    let text = error.to_string();
    assert_eq!(remote_code(Err::<(), _>(error)), ErrorCode::BadRequest);
    assert!(
        text.contains("Contained") && text.contains("no OS sandbox") && text.contains("SANDBOX-2"),
        "{text}"
    );
    assert!(t.spawns("omp").is_empty(), "nothing was started");
    t.stop().await;
}

// Claude Code -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn claude_gets_a_session_mode_and_for_contained_the_bash_sandbox_through_session_meta() {
    let t = S::start(Setup {
        claude: Some("claude-modes-set"),
        ..Setup::default()
    })
    .await;
    let contained = t.create("claude", Some(SafetyPreset::Contained)).await;
    let read_only = t.create("claude", Some(SafetyPreset::ReadOnly)).await;
    let full = t.create("claude", Some(SafetyPreset::Full)).await;

    let modes: Vec<_> = t
        .received_of("claude-received.jsonl", "session/set_mode")
        .into_iter()
        .map(|m| m["params"]["modeId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(modes, ["acceptEdits", "plan", "bypassPermissions"]);

    let news = t.received_of("claude-received.jsonl", "session/new");
    assert_eq!(news.len(), 3);
    assert_eq!(
        news[0]["params"]["_meta"],
        json!({"claudeCode": {"options": {"sandbox": {
            "enabled": true,
            "autoAllowBashIfSandboxed": true,
            "allowUnsandboxedCommands": false,
        }}}}),
        "Contained turns Claude Code's Bash sandbox on and forbids the unsandboxed retry"
    );
    assert!(news[1]["params"].get("_meta").is_none());
    assert!(news[2]["params"].get("_meta").is_none());
    assert!(
        t.extras("claude").iter().all(Vec::is_empty),
        "Claude Code takes no safety flags in ACP mode"
    );

    let contained_safety = safety_of(&contained);
    assert_eq!(
        (contained_safety.preset, contained_safety.enforced_by),
        (SafetyPreset::Contained, SafetyEnforcer::Harness)
    );
    assert!(
        contained_safety
            .note
            .unwrap()
            .contains("does not report it back"),
        "the sandbox is requested, not confirmed"
    );
    assert_eq!(
        contained.chat.unwrap().mode.unwrap().id,
        "acceptEdits",
        "the mode is what the harness reports"
    );
    let plan = safety_of(&read_only);
    assert_eq!(
        (plan.enforced_by, plan.note),
        (SafetyEnforcer::Harness, None)
    );
    let bypass = full.chat.clone().unwrap();
    assert!(bypass.mode.unwrap().approves_everything);
    assert_eq!(safety_of(&full).enforced_by, SafetyEnforcer::None);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_ask_is_its_default_mode_and_sends_nothing_else() {
    let t = S::start(Setup {
        claude: Some("claude-modes-default"),
        ..Setup::default()
    })
    .await;
    let ask = t.create("claude", Some(SafetyPreset::Ask)).await;
    assert!(
        t.received_of("claude-received.jsonl", "session/set_mode")
            .is_empty(),
        "already in default"
    );
    assert!(
        t.received_of("claude-received.jsonl", "session/new")[0]["params"]
            .get("_meta")
            .is_none()
    );
    let safety = safety_of(&ask);
    assert_eq!(
        (safety.enforced_by, safety.note),
        (SafetyEnforcer::Harness, None)
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_that_does_not_offer_the_mode_fails_the_chat_naming_what_it_offers() {
    // OMP's transcript offers `default` and `plan` only.
    let t = S::start(Setup {
        claude: Some("plain-turn"),
        ..Setup::default()
    })
    .await;
    let error = t
        .try_create(t.request("claude", Some(SafetyPreset::Contained)))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("acceptEdits") && error.contains("default, plan"),
        "{error}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mode_change_inside_claude_makes_the_chat_say_the_preset_is_no_longer_applied() {
    let mut t = S::start(Setup {
        claude: Some("claude-modes-set-then-default"),
        ..Setup::default()
    })
    .await;
    let session = t.create("claude", Some(SafetyPreset::Contained)).await;
    let id = session.id;
    let unapplied = common::wait_event(
        &mut t.events,
        "session.updated with the preset not applied",
        |event| match event {
            Event::SessionUpdated { session } if session.id == id => session
                .chat
                .as_ref()
                .and_then(|chat| chat.safety.clone())
                .filter(|safety| safety.enforced_by == SafetyEnforcer::None),
            _ => None,
        },
    )
    .await;
    assert_eq!(unapplied.preset, SafetyPreset::Contained);
    let note = unapplied.note.unwrap();
    assert!(
        note.starts_with("Not applied") && note.contains("mode default"),
        "{note}"
    );
    t.stop().await;
}

// Codex -----------------------------------------------------------------------------------------

async fn codex_chat(transcript: &'static str, preset: SafetyPreset) -> (S, SessionInfo, Value) {
    let t = S::start(Setup {
        codex: Some(transcript),
        ..Setup::default()
    })
    .await;
    let session = t.create("codex", Some(preset)).await;
    let start = t
        .received_of("codex-received.jsonl", "thread/start")
        .remove(0);
    (t, session, start["params"].clone())
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_gets_the_sandbox_and_approval_policy_of_each_preset_and_reports_what_it_enforces() {
    let cases = [
        (
            "safety-ask",
            SafetyPreset::Ask,
            (ApprovalPolicy::OnRequest, SandboxMode::WorkspaceWrite),
            ("on-request", "workspace-write"),
            SafetyEnforcer::Harness,
            Some(false),
        ),
        (
            "safety-contained",
            SafetyPreset::Contained,
            (ApprovalPolicy::Never, SandboxMode::WorkspaceWrite),
            ("never", "workspace-write"),
            SafetyEnforcer::Harness,
            Some(false),
        ),
        (
            "safety-read-only",
            SafetyPreset::ReadOnly,
            (ApprovalPolicy::Never, SandboxMode::ReadOnly),
            ("never", "read-only"),
            SafetyEnforcer::Harness,
            Some(false),
        ),
        (
            "safety-full",
            SafetyPreset::Full,
            (ApprovalPolicy::Never, SandboxMode::DangerFullAccess),
            ("never", "danger-full-access"),
            SafetyEnforcer::None,
            Some(true),
        ),
    ];
    for (transcript, preset, (policy, sandbox), (wire_policy, wire_sandbox), enforcer, network) in
        cases
    {
        let (t, session, params) = codex_chat(transcript, preset).await;
        assert_eq!(params["approvalPolicy"], wire_policy, "{preset}");
        assert_eq!(params["sandbox"], wire_sandbox, "{preset}");
        let chat = session.chat.clone().unwrap();
        assert_eq!(chat.approval_policy, policy, "{preset}");
        assert_eq!(chat.sandbox, sandbox, "{preset}");
        let safety = safety_of(&session);
        assert_eq!(safety.preset, preset);
        assert_eq!(safety.enforced_by, enforcer, "{preset}");
        assert_eq!(safety.network, network, "{preset}");
        assert_eq!(safety.note, None, "{preset}");
        t.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_that_enforces_something_else_than_asked_is_reported_as_not_applied() {
    // The server answers `untrusted` and `readOnly` to a Contained request.
    let (t, session, _) = codex_chat("safety-mismatch", SafetyPreset::Contained).await;
    let safety = safety_of(&session);
    assert_eq!(safety.enforced_by, SafetyEnforcer::None);
    let note = safety.note.unwrap();
    assert!(
        note.starts_with("Not applied")
            && note.contains("sandbox read-only")
            && note.contains("approvals untrusted"),
        "{note}"
    );
    t.stop().await;
}

// Requests --------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_preset_cannot_be_combined_with_the_settings_it_replaces_and_direct_chats_have_none() {
    let t = S::start(Setup {
        omp: Some("plain-turn"),
        codex: Some("safety-ask"),
        ..Setup::default()
    })
    .await;
    let bad = |req: ChatCreate| async {
        let error = t.try_create(req).await.unwrap_err();
        let text = error.to_string();
        (remote_code(Err::<(), _>(error)), text)
    };
    let mut req = t.request("codex", Some(SafetyPreset::Ask));
    req.sandbox = Some(SandboxMode::ReadOnly);
    let (code, text) = bad(req).await;
    assert_eq!(code, ErrorCode::BadRequest);
    assert!(text.contains("either `safety` or"), "{text}");

    let mut req = t.request("omp", Some(SafetyPreset::Ask));
    req.mode = Some("plan".into());
    assert_eq!(bad(req).await.0, ErrorCode::BadRequest);

    let (code, text) = bad(t.request("direct", Some(SafetyPreset::Ask))).await;
    assert_eq!(code, ErrorCode::BadRequest);
    assert!(text.contains("nothing to contain"), "{text}");
    assert!(t.spawns("omp").is_empty());
    t.stop().await;
}

async fn project(t: &S) -> WorkspaceInfo {
    let root = t.work.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    t.client
        .call::<WorkspaceInfo>(Request::WorkspaceCreate(WorkspaceCreate {
            name: "tasks".into(),
            paths: vec![root],
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_task_carries_its_preset_into_the_chat_and_is_refused_when_the_harness_cannot() {
    let t = S::start(Setup {
        omp: Some("plain-turn"),
        codex: Some("safety-read-only"),
        ..Setup::default()
    })
    .await;
    let project = project(&t).await;
    let create = |agent: &str, safety| TaskCreate {
        workspace_id: project.id,
        title: format!("{agent} task"),
        prompt: Some("Say hello.".into()),
        depends_on: Vec::new(),
        placement: None,
        run: Some(TaskRun::Chat {
            agent: agent.to_owned(),
            model: None,
            account: None,
            safety,
        }),
        auto_start: false,
        after_policy: Default::default(),
    };
    // Refused when the task is made: the reason is the harness's.
    let error = t
        .client
        .task_create(create("omp", Some(SafetyPreset::Contained)))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no OS sandbox"), "{error}");

    let task = t
        .client
        .task_create(create("codex", Some(SafetyPreset::ReadOnly)))
        .await
        .unwrap();
    assert_eq!(
        task.run,
        Some(TaskRun::Chat {
            agent: "codex".into(),
            model: None,
            account: None,
            safety: Some(SafetyPreset::ReadOnly),
        })
    );
    let started = t.client.task_start(task.id).await.unwrap();
    let id = started.session.id;
    let safety = timeout(WAIT, async {
        loop {
            let sessions = t
                .client
                .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
                .await
                .unwrap()
                .sessions;
            let session = sessions.into_iter().find(|s| s.id == id).unwrap();
            if let Some(safety) = session.chat.and_then(|c| c.safety) {
                return safety;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the task's chat reports its safety");
    assert_eq!(safety.preset, SafetyPreset::ReadOnly);
    assert_eq!(safety.enforced_by, SafetyEnforcer::Harness);
    let params = t
        .received_of("codex-received.jsonl", "thread/start")
        .remove(0)["params"]
        .clone();
    assert_eq!(params["sandbox"], "read-only");
    assert_eq!(params["approvalPolicy"], "never");
    t.stop().await;
}
