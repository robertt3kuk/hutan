//! Tasks with dependencies (protocol 0.16) end to end: a real daemon on a temporary
//! `HUTAN_HOME`, real client connections, real PTY sessions (`sh -c`), `hutan-codex-fixture
//! replay` standing in for `codex app-server`, and git repositories the tests create in
//! temporary directories (never this repository, never the user's).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{TestDaemon, WAIT, create_in, sh, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::{Client, ClientError};
use hutan_core::{
    AfterPolicy, DeliveryActor, DeliveryState, NodeOutcome, ReviewDecision, SessionId, SessionKind,
    SessionState, Task, TaskId, TaskPlacement, TaskPlacementKind, TaskRun, TaskState, WorkspaceId,
};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{
    AgentApplied, AgentReleaseRequest, AgentReportRequest, AgentState, ClientKind, Event, Request,
    SessionInfo, WorkspaceOpen, WorkspaceOpened,
};
use hutan_protocol::notify::{IntentKind, NotificationIntent};
use hutan_protocol::task::{TaskCreate, TaskUpdate};
use hutan_protocol::worktree::{
    MergeStrategy, PlacementKind, WorktreeDiffRequest, WorktreeMerge, WorktreeRemove,
};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

// Fixtures ------------------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
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
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository `<tmp>/repo` on branch `main` with one commit.
struct Repo {
    _dir: TempDir,
    root: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&root, &["add", "a.txt"]);
        git(&root, &["commit", "-q", "-m", "first"]);
        Self { _dir: dir, root }
    }

    fn wt(&self, name: &str) -> PathBuf {
        self.root.parent().unwrap().join("repo-wt").join(name)
    }

    fn branches(&self) -> Vec<String> {
        git(&self.root, &["branch", "--format=%(refname:short)"])
            .lines()
            .map(str::to_owned)
            .collect()
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

struct Env {
    daemon: TestDaemon,
    client: Client,
    events: broadcast::Receiver<(hutan_core::Seq, Event)>,
    repo: Repo,
    workspace: WorkspaceId,
    /// Holds what the fake agent logs and what tasks write.
    scratch: TempDir,
}

impl Env {
    async fn start() -> Self {
        Self::start_with("", None).await
    }

    /// A daemon with `toml` as `daemon.toml`, whose codex replays the fixture `fixture`.
    async fn start_with(toml: &str, fixture: Option<&str>) -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let config = fixture.map(|name| replay(name, &scratch.path().join("received.jsonl")));
        Self::start_in(toml, scratch, config).await
    }

    /// A daemon whose codex replays the transcript file `transcript` writes into the scratch
    /// directory.
    async fn start_with_transcript(transcript: impl FnOnce(&Path) -> PathBuf) -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let file = transcript(scratch.path());
        let config = CodexConfig {
            program: fixture_bin(),
            args: vec![
                "replay".to_owned(),
                "--received".to_owned(),
                scratch.path().join("received.jsonl").display().to_string(),
                file.display().to_string(),
                "app-server".to_owned(),
            ],
            env: Vec::new(),
        };
        Self::start_in("", scratch, Some(config)).await
    }

    async fn start_in(toml: &str, scratch: TempDir, config: Option<CodexConfig>) -> Self {
        let daemon = TestDaemon::start_with(toml, move |c| {
            c.codex = config;
            // The checkpoints of chat turns judge what they leave for review; no user config.
            c.checkpoints.git_env = crate::checkpoint_turns::git_env();
        })
        .await;
        let repo = Repo::new();
        let (client, events) = daemon.connect_subscribed().await;
        let workspace = open(&client, &repo.root).await;
        Self {
            daemon,
            client,
            events,
            repo,
            workspace,
            scratch,
        }
    }

    fn create_request(&self, title: &str) -> TaskCreate {
        TaskCreate {
            workspace_id: self.workspace,
            title: title.to_owned(),
            prompt: None,
            depends_on: Vec::new(),
            placement: None,
            run: None,
            auto_start: false,
            after_policy: AfterPolicy::Done,
        }
    }

    /// A task that runs `script` with `sh -c`.
    fn script(&self, title: &str, script: &str, after: &[&Task]) -> TaskCreate {
        TaskCreate {
            run: Some(TaskRun::Terminal {
                argv: vec!["sh".into(), "-c".into(), script.into()],
            }),
            depends_on: after.iter().map(|t| t.id).collect(),
            ..self.create_request(title)
        }
    }

    async fn create(&self, req: TaskCreate) -> Task {
        self.client.task_create(req).await.unwrap()
    }

    async fn task(&self, id: TaskId) -> Task {
        self.client
            .task_list(None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.id == id)
            .expect("task listed")
    }

    /// Polls until the task satisfies `pick`.
    async fn wait(&self, id: TaskId, what: &str, pick: impl Fn(&Task) -> bool) -> Task {
        let found = timeout(WAIT, async {
            loop {
                let task = self.task(id).await;
                if pick(&task) {
                    return task;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await;
        match found {
            Ok(task) => task,
            Err(_) => panic!("task {id}: {what}; it is {:?}", self.task(id).await),
        }
    }

    async fn wait_state(&self, id: TaskId, state: TaskState) -> Task {
        self.wait(id, &format!("never became {state}"), |t| t.state == state)
            .await
    }

    async fn update(&self, id: TaskId, state: TaskState) -> Result<Task, ClientError> {
        self.client
            .task_update(TaskUpdate {
                state: Some(state),
                ..update_of(id)
            })
            .await
    }

    async fn session(&self, id: SessionId) -> SessionInfo {
        self.client
            .call::<hutan_protocol::messages::SessionList>(Request::SessionList(
                hutan_protocol::messages::SessionListFilter::default(),
            ))
            .await
            .unwrap()
            .sessions
            .into_iter()
            .find(|s| s.id == id)
            .expect("session listed")
    }

    async fn wait_session_state(&self, id: SessionId, state: SessionState) {
        timeout(WAIT, async {
            while self.session(id).await.state != state {
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("session {id} never reached {state:?}"));
    }

    /// The next `notify.intent` of `kind` for `session`.
    async fn intent(&mut self, kind: IntentKind, session: SessionId) -> NotificationIntent {
        wait_event(&mut self.events, "the task intent", |event| match event {
            Event::NotifyIntent { intent }
                if intent.kind == kind && intent.session_id == session =>
            {
                Some(intent.clone())
            }
            _ => None,
        })
        .await
    }

    async fn report(&self, session: SessionId, state: AgentState, seq: u64) {
        let applied: AgentApplied = self
            .client
            .call(Request::AgentReport(AgentReportRequest {
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
            }))
            .await
            .unwrap();
        assert!(applied.applied, "report ignored");
    }

    /// The main agent ends (`agent.release`).
    async fn release(&self, session: SessionId, seq: u64, outcome: Option<NodeOutcome>) {
        let applied: AgentApplied = self
            .client
            .call(Request::AgentRelease(AgentReleaseRequest {
                session_id: session,
                source: "test-agent".into(),
                seq,
                node: None,
                outcome,
            }))
            .await
            .unwrap();
        assert!(applied.applied, "release ignored");
    }

    /// A new daemon on the same `HUTAN_HOME`; the connection is made again.
    async fn restart(self) -> Self {
        let Self {
            daemon,
            client,
            events,
            repo,
            workspace,
            scratch,
        } = self;
        drop((client, events));
        let daemon = daemon.restart().await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            client,
            events,
            repo,
            workspace,
            scratch,
        }
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

fn update_of(id: TaskId) -> TaskUpdate {
    TaskUpdate {
        id,
        title: None,
        prompt: None,
        depends_on: None,
        placement: None,
        run: None,
        auto_start: None,
        after_policy: None,
        state: None,
    }
}

async fn open(client: &Client, path: &Path) -> WorkspaceId {
    client
        .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
            path: path.to_owned(),
            created_by: None,
        }))
        .await
        .unwrap()
        .workspace
        .id
}

fn message<T: std::fmt::Debug>(result: Result<T, ClientError>) -> (ErrorCode, String) {
    match result {
        Err(ClientError::Remote(error)) => (error.code, error.message),
        other => panic!("expected a remote error, got {other:?}"),
    }
}

fn worktree(branch: Option<&str>) -> Option<TaskPlacement> {
    Some(TaskPlacement {
        kind: TaskPlacementKind::Worktree,
        branch: branch.map(str::to_owned),
        base: None,
    })
}

// create / list / update / delete -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tasks_are_created_listed_in_dependency_order_and_announced() {
    let mut t = Env::start().await;
    let a = t.create(t.create_request("Write the parser")).await;
    assert_eq!(a.state, TaskState::Ready, "no dependency: ready at once");
    assert_eq!(a.workspace_id, t.workspace);
    assert_eq!(a.placement, TaskPlacement::default());
    assert!(a.sessions.is_empty() && a.started_at_ms.is_none());
    // The event carries the whole task as it is.
    let told = wait_event(&mut t.events, "task.updated", |event| match event {
        Event::TaskUpdated { task } if task.id == a.id => Some(task.clone()),
        _ => None,
    })
    .await;
    assert_eq!(told, a);

    let b = t
        .create(TaskCreate {
            depends_on: vec![a.id],
            prompt: Some("  Then test it.  ".into()),
            ..t.create_request("  Test the parser ")
        })
        .await;
    assert_eq!(b.title, "Test the parser", "titles are trimmed");
    assert_eq!(b.prompt.as_deref(), Some("  Then test it.  "));
    assert_eq!(b.state, TaskState::Todo, "its dependency is not done");
    let c = t
        .create(TaskCreate {
            depends_on: vec![b.id, a.id],
            ..t.create_request("Ship it")
        })
        .await;
    assert!(a.created_at_ms < b.created_at_ms && b.created_at_ms < c.created_at_ms);

    let listed = t.client.task_list(None, None).await.unwrap();
    assert_eq!(
        listed.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
        ["Write the parser", "Test the parser", "Ship it"]
    );
    let ready = t
        .client
        .task_list(Some(t.workspace), Some(TaskState::Ready))
        .await
        .unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, a.id);
    let elsewhere = t
        .client
        .task_list(Some(WorkspaceId::new()), None)
        .await
        .unwrap();
    assert!(elsewhere.is_empty());

    // Updating fields.
    let renamed = t
        .client
        .task_update(TaskUpdate {
            title: Some("Test it well".into()),
            prompt: Some(String::new()),
            auto_start: Some(true),
            run: Some(TaskRun::Terminal {
                argv: vec!["make".into(), "test".into()],
            }),
            placement: worktree(Some("task/test-it")),
            ..update_of(b.id)
        })
        .await
        .unwrap();
    assert_eq!(renamed.title, "Test it well");
    assert_eq!(renamed.prompt, None, "an empty prompt removes it");
    assert!(renamed.auto_start);
    assert_eq!(renamed.placement.branch.as_deref(), Some("task/test-it"));
    assert_eq!(renamed.depends_on, [a.id]);
    // New dependencies replace the old; duplicates collapse.
    let relinked = t
        .client
        .task_update(TaskUpdate {
            depends_on: Some(vec![]),
            ..update_of(c.id)
        })
        .await
        .unwrap();
    assert!(relinked.depends_on.is_empty());
    assert_eq!(relinked.state, TaskState::Ready);
    let relinked = t
        .client
        .task_update(TaskUpdate {
            depends_on: Some(vec![a.id, b.id, a.id]),
            ..update_of(c.id)
        })
        .await
        .unwrap();
    assert_eq!(relinked.depends_on, [a.id, b.id]);
    assert_eq!(relinked.state, TaskState::Todo);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_tasks_and_bad_changes_are_refused() {
    let t = Env::start().await;
    let a = t.create(t.create_request("a")).await;
    let b = t
        .create(TaskCreate {
            depends_on: vec![a.id],
            ..t.create_request("b")
        })
        .await;
    let c = t
        .create(TaskCreate {
            depends_on: vec![b.id],
            ..t.create_request("c")
        })
        .await;

    // A cycle is refused and the message names it.
    let (code, text) = message(
        t.client
            .task_update(TaskUpdate {
                depends_on: Some(vec![c.id]),
                ..update_of(a.id)
            })
            .await,
    );
    assert_eq!(code, ErrorCode::BadRequest);
    assert_eq!(text, "dependency cycle: `a` -> `c` -> `b` -> `a`");
    let (code, text) = message(
        t.client
            .task_update(TaskUpdate {
                depends_on: Some(vec![a.id]),
                ..update_of(a.id)
            })
            .await,
    );
    assert_eq!(
        (code, text.contains("itself")),
        (ErrorCode::BadRequest, true)
    );
    assert!(t.task(a.id).await.depends_on.is_empty(), "nothing changed");

    // Unknown and foreign dependencies, unknown workspace, unknown task.
    let ghost = TaskId::new();
    let new = |dep: TaskId| TaskCreate {
        depends_on: vec![dep],
        ..t.create_request("x")
    };
    assert_eq!(
        message(t.client.task_create(new(ghost)).await).0,
        ErrorCode::NotFound
    );
    let other = open(&t.client, &Repo::new().root).await;
    let (code, text) = message(
        t.client
            .task_create(TaskCreate {
                workspace_id: other,
                depends_on: vec![a.id],
                ..t.create_request("x")
            })
            .await,
    );
    assert_eq!(code, ErrorCode::BadRequest);
    assert!(text.contains("another workspace"), "{text}");
    assert_eq!(
        message(
            t.client
                .task_create(TaskCreate {
                    workspace_id: WorkspaceId::new(),
                    ..t.create_request("x")
                })
                .await
        )
        .0,
        ErrorCode::NotFound
    );
    assert_eq!(
        message(t.client.task_update(update_of(ghost)).await).0,
        ErrorCode::NotFound
    );
    assert_eq!(
        message(t.client.task_start(ghost).await).0,
        ErrorCode::NotFound
    );
    assert_eq!(
        message(t.client.task_delete(ghost, false).await).0,
        ErrorCode::NotFound
    );

    // Invalid fields.
    for bad in [
        TaskCreate {
            title: "   ".into(),
            ..t.create_request("")
        },
        TaskCreate {
            title: "two\nlines".into(),
            ..t.create_request("")
        },
        TaskCreate {
            run: Some(TaskRun::Chat {
                agent: "codex".into(),
                model: None,
                account: None,
                safety: None,
            }),
            ..t.create_request("chat without a prompt")
        },
        TaskCreate {
            run: Some(TaskRun::Chat {
                agent: "gemini".into(),
                model: None,
                account: None,
                safety: None,
            }),
            prompt: Some("hi".into()),
            ..t.create_request("unknown agent")
        },
        TaskCreate {
            run: Some(TaskRun::Terminal { argv: vec![] }),
            ..t.create_request("nothing to run")
        },
        TaskCreate {
            placement: Some(TaskPlacement {
                kind: TaskPlacementKind::Main,
                branch: Some("x".into()),
                base: None,
            }),
            ..t.create_request("main with a branch")
        },
        TaskCreate {
            placement: worktree(Some("--force")),
            ..t.create_request("option as a branch")
        },
    ] {
        let title = bad.title.clone();
        assert_eq!(
            message(t.client.task_create(bad).await).0,
            ErrorCode::BadRequest,
            "{title}"
        );
    }
    for state in [
        TaskState::Ready,
        TaskState::Running,
        TaskState::Blocked,
        TaskState::Failed,
    ] {
        assert_eq!(
            message(t.update(a.id, state).await).0,
            ErrorCode::BadRequest,
            "{state}"
        );
    }
    assert_eq!(t.client.task_list(None, None).await.unwrap().len(), 3);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_task_others_depend_on_needs_cascade() {
    let mut t = Env::start().await;
    let a = t.create(t.create_request("a")).await;
    let b = t
        .create(TaskCreate {
            depends_on: vec![a.id],
            ..t.create_request("b")
        })
        .await;
    let c = t
        .create(TaskCreate {
            depends_on: vec![b.id],
            ..t.create_request("c")
        })
        .await;
    let free = t.create(t.create_request("free")).await;

    let (code, text) = message(t.client.task_delete(a.id, false).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert!(text.contains("`b`") && text.contains("`c`"), "{text}");
    assert_eq!(t.client.task_list(None, None).await.unwrap().len(), 4);

    // A leaf goes alone.
    assert_eq!(t.client.task_delete(c.id, false).await.unwrap(), [c.id]);
    wait_event(&mut t.events, "task.removed", |event| match event {
        Event::TaskRemoved { id } if *id == c.id => Some(()),
        _ => None,
    })
    .await;
    // With cascade the dependants go first, in the order they depend on each other.
    let c2 = t
        .create(TaskCreate {
            depends_on: vec![b.id],
            ..t.create_request("c2")
        })
        .await;
    let removed = t.client.task_delete(a.id, true).await.unwrap();
    assert_eq!(removed.len(), 3);
    assert_eq!(removed[2], a.id);
    assert_eq!(
        removed[..2].iter().copied().collect::<BTreeSet<_>>(),
        [b.id, c2.id].into_iter().collect::<BTreeSet<_>>()
    );
    assert_eq!(
        removed[0], c2.id,
        "a task is removed before the one it depends on"
    );
    let left = t.client.task_list(None, None).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, free.id);

    // What went stays gone after a restart.
    let t = t.restart().await;
    let after = t.client.task_list(None, None).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, free.id);
    t.stop().await;
}

// start: main checkout, finish detection --------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_task_runs_in_the_main_checkout_and_is_done_when_it_exits_zero() {
    let mut t = Env::start().await;
    let out = t.scratch.path().join("task-id.txt");
    let a = t
        .create(t.script(
            "Build",
            &format!("printenv HUTAN_TASK_ID > {}; sleep 1", out.display()),
            &[],
        ))
        .await;
    let b = t.create(t.script("Test", "exit 0", &[&a])).await;
    assert_eq!(b.state, TaskState::Todo);

    // Not ready: refused, with what it waits for.
    let (code, text) = message(t.client.task_start(b.id).await);
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(text.contains("`Build` (ready)"), "{text}");

    let started = t.client.task_start(a.id).await.unwrap();
    assert_eq!(started.task.state, TaskState::Running);
    assert!(started.task.started_at_ms.is_some());
    assert_eq!(started.task.sessions, [started.session.id]);
    assert!(started.warnings.is_empty());
    let session = started.session;
    assert_eq!(session.kind, SessionKind::Terminal);
    assert_eq!(session.workspace_id, t.workspace);
    assert_eq!(session.title, "Build");
    assert_eq!(session.cwd.as_deref(), Some(t.repo.root.as_path()));
    assert_eq!(
        session.command,
        [
            "sh",
            "-c",
            &format!("printenv HUTAN_TASK_ID > {}; sleep 1", out.display())
        ]
    );
    let placement = session.placement.expect("a session in a repository");
    assert_eq!(placement.kind, PlacementKind::Main);
    assert_eq!(placement.root, t.repo.root);
    // The task is running now; starting again is refused.
    assert_eq!(
        message(t.client.task_start(a.id).await).0,
        ErrorCode::InvalidState
    );

    let done = t.wait_state(a.id, TaskState::Done).await;
    assert_eq!(done.outcome.as_deref(), Some("exited 0"));
    assert!(done.ended_at_ms.unwrap() >= done.started_at_ms.unwrap());
    assert_eq!(fs::read_to_string(&out).unwrap().trim(), a.id.to_string());

    // The dependant is ready now, and the finish raised an intent for the session that ran it.
    let b = t.wait_state(b.id, TaskState::Ready).await;
    assert!(b.sessions.is_empty());
    let intent = t.intent(IntentKind::TaskDone, session.id).await;
    assert_eq!(intent.title, "Build: task done");
    assert!(
        intent.body.starts_with("Finished after "),
        "{}",
        intent.body
    );
    assert!(intent.body.ends_with("(exited 0)."), "{}", intent.body);
    assert_eq!(intent.workspace_id, Some(t.workspace));

    // The events told every step: running, then done, then the dependant's readiness.
    let started_b = t.client.task_start(b.id).await.unwrap();
    assert_eq!(started_b.task.state, TaskState::Running);
    t.wait_state(b.id, TaskState::Done).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_task_fails_and_does_not_release_its_dependants() {
    let mut t = Env::start().await;
    let a = t.create(t.script("Flaky", "echo boom; exit 3", &[])).await;
    let b = t.create(t.script("After", "exit 0", &[&a])).await;
    let started = t.client.task_start(a.id).await.unwrap();
    let failed = t.wait_state(a.id, TaskState::Failed).await;
    assert_eq!(failed.outcome.as_deref(), Some("exited 3"));
    assert!(failed.ended_at_ms.is_some());
    let intent = t.intent(IntentKind::TaskFailed, started.session.id).await;
    assert_eq!(intent.title, "Flaky: task failed");
    assert!(intent.body.ends_with("(exited 3)."), "{}", intent.body);

    assert_eq!(t.task(b.id).await.state, TaskState::Todo);
    let (code, text) = message(t.client.task_start(b.id).await);
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(text.contains("`Flaky` (failed)"), "{text}");
    // A failed task is not started again by accident.
    let (code, text) = message(t.client.task_start(a.id).await);
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(text.contains("exited 3"), "{text}");

    // Run again: reset, start, and now it succeeds.
    let reset = t.update(a.id, TaskState::Todo).await.unwrap();
    assert_eq!(reset.state, TaskState::Ready);
    assert!(
        reset.outcome.is_none() && reset.ended_at_ms.is_none() && reset.started_at_ms.is_none()
    );
    assert_eq!(
        reset.sessions,
        [started.session.id],
        "the earlier run stays listed"
    );
    t.client
        .task_update(TaskUpdate {
            run: Some(TaskRun::Terminal {
                argv: vec!["true".into()],
            }),
            ..update_of(a.id)
        })
        .await
        .unwrap();
    let second = t.client.task_start(a.id).await.unwrap();
    assert_eq!(second.task.sessions.len(), 2);
    t.wait_state(a.id, TaskState::Done).await;
    t.wait_state(b.id, TaskState::Ready).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_without_a_run_opens_a_shell_that_the_user_finishes() {
    let t = Env::start().await;
    let a = t.create(t.create_request("By hand")).await;
    let started = t.client.task_start(a.id).await.unwrap();
    assert!(started.session.command.is_empty(), "a login shell");
    assert_eq!(started.session.cwd.as_deref(), Some(t.repo.root.as_path()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(t.task(a.id).await.state, TaskState::Running);
    // `task done`: the user says it is done; the session is left alone.
    let done = t.update(a.id, TaskState::Done).await.unwrap();
    assert_eq!(done.state, TaskState::Done);
    assert_eq!(done.outcome.as_deref(), Some("marked done"));
    assert!(t.session(started.session.id).await.state.is_live());
    // A finished task takes no more events from its session.
    common::stop(&t.client, started.session.id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(t.task(a.id).await.state, TaskState::Done);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_that_goes_idle_after_working_finishes_the_task_and_a_failed_one_fails_it() {
    let mut t = Env::start().await;
    let a = t.create(t.script("Agent run", "exec sleep 600", &[])).await;
    let b = t
        .create(t.script("Doomed run", "exec sleep 600", &[]))
        .await;
    let c = t.create(t.script("Idle run", "exec sleep 600", &[])).await;
    let sa = t.client.task_start(a.id).await.unwrap().session.id;
    let sb = t.client.task_start(b.id).await.unwrap().session.id;
    let sc = t.client.task_start(c.id).await.unwrap().session.id;

    // Idle without having worked: nothing finished.
    t.report(sc, AgentState::Idle, 1).await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!(t.task(c.id).await.state, TaskState::Running);

    // Blocked is shown while the agent waits, and the task works on when it does.
    t.report(sa, AgentState::Working, 1).await;
    t.report(sa, AgentState::Blocked, 2).await;
    let blocked = t.wait_state(a.id, TaskState::Blocked).await;
    assert!(blocked.ended_at_ms.is_none());
    let seen = wait_event(&mut t.events, "task.updated blocked", |event| match event {
        Event::TaskUpdated { task } if task.id == a.id && task.state == TaskState::Blocked => {
            Some(())
        }
        _ => None,
    })
    .await;
    let () = seen;
    t.report(sa, AgentState::Working, 3).await;
    t.wait_state(a.id, TaskState::Running).await;
    assert!(t.task(a.id).await.outcome.is_none());

    // Working, then idle: the agent finished.
    t.report(sa, AgentState::Idle, 4).await;
    let done = t.wait_state(a.id, TaskState::Done).await;
    assert_eq!(done.outcome.as_deref(), Some("agent finished"));
    assert!(
        t.session(sa).await.state.is_live(),
        "the session is left running"
    );
    t.intent(IntentKind::TaskDone, sa).await;

    // A main agent that ends with a failure.
    t.report(sb, AgentState::Working, 1).await;
    t.release(sb, 2, Some(NodeOutcome::Failed)).await;
    let failed = t.wait_state(b.id, TaskState::Failed).await;
    assert_eq!(failed.outcome.as_deref(), Some("agent failed"));
    t.intent(IntentKind::TaskFailed, sb).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_stops_the_session_and_a_stopped_session_cancels_the_task() {
    let t = Env::start().await;
    let a = t.create(t.script("Long", "exec sleep 600", &[])).await;
    let b = t.create(t.script("After long", "exit 0", &[&a])).await;
    let c = t
        .create(t.script("Stopped by hand", "exec sleep 600", &[]))
        .await;
    let sa = t.client.task_start(a.id).await.unwrap().session.id;
    let sc = t.client.task_start(c.id).await.unwrap().session.id;

    // A running task cannot be edited or deleted.
    let (code, _) = message(
        t.client
            .task_update(TaskUpdate {
                title: Some("x".into()),
                ..update_of(a.id)
            })
            .await,
    );
    assert_eq!(code, ErrorCode::InvalidState);
    // Its dependant is named first; with cascade the running task still holds it back.
    assert_eq!(
        message(t.client.task_delete(a.id, false).await).0,
        ErrorCode::Conflict
    );
    let (code, text) = message(t.client.task_delete(a.id, true).await);
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(text.contains("`Long`") && text.contains("cancel"), "{text}");
    assert_eq!(t.client.task_list(None, None).await.unwrap().len(), 3);
    assert_eq!(
        message(t.update(a.id, TaskState::Todo).await).0,
        ErrorCode::InvalidState,
        "a running task is cancelled, not reset"
    );

    let cancelled = t.update(a.id, TaskState::Cancelled).await.unwrap();
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert_eq!(cancelled.outcome.as_deref(), Some("cancelled"));
    t.wait_session_state(sa, SessionState::Stopped).await;
    // Its dependant waits for a done task, not a cancelled one.
    assert_eq!(t.task(b.id).await.state, TaskState::Todo);

    // The user stops a session by hand: the task is cancelled too.
    common::stop(&t.client, sc).await.unwrap();
    let c = t.wait_state(c.id, TaskState::Cancelled).await;
    assert_eq!(c.outcome.as_deref(), Some("session stopped"));

    // A cancelled task is reset to run again; it cannot be declared done, but a todo one can.
    let again = t.update(a.id, TaskState::Todo).await.unwrap();
    assert_eq!(again.state, TaskState::Ready);
    assert_eq!(
        message(t.update(c.id, TaskState::Done).await).0,
        ErrorCode::InvalidState
    );
    let by_hand = t.update(b.id, TaskState::Done).await.unwrap();
    assert_eq!(by_hand.outcome.as_deref(), Some("marked done"));
    assert_eq!(by_hand.started_at_ms, None, "it never ran");
    t.stop().await;
}

// start: worktrees ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_task_gets_its_own_checkout_and_reuses_it_when_it_runs_again() {
    let t = Env::start().await;
    let a = t
        .create(TaskCreate {
            placement: worktree(None),
            ..t.script("Fix the login bug", "exit 0", &[])
        })
        .await;
    let b = t
        .create(TaskCreate {
            placement: Some(TaskPlacement {
                kind: TaskPlacementKind::Worktree,
                branch: Some("feature/export".into()),
                base: Some("main".into()),
            }),
            ..t.script("Export", "exit 0", &[])
        })
        .await;

    let started = t.client.task_start(a.id).await.unwrap();
    let dir = t.repo.wt("task-fix-the-login-bug");
    assert_eq!(started.session.cwd.as_deref(), Some(dir.as_path()));
    assert!(dir.join("a.txt").is_file(), "a checkout of the repository");
    let placement = started.session.placement.expect("placement");
    assert_eq!(placement.kind, PlacementKind::Worktree);
    assert_eq!(placement.branch.as_deref(), Some("task/fix-the-login-bug"));
    assert!(
        t.repo
            .branches()
            .contains(&"task/fix-the-login-bug".to_owned())
    );
    let listed = t.client.worktree_list(Some(t.workspace)).await.unwrap();
    let made = listed.iter().find(|w| w.path == dir).expect("listed");
    assert!(made.created_by_hutan && !made.main);
    t.wait_state(a.id, TaskState::Done).await;

    // The same task again: the worktree of its branch is used, nothing is created.
    t.update(a.id, TaskState::Todo).await.unwrap();
    let again = t.client.task_start(a.id).await.unwrap();
    assert_eq!(again.session.cwd.as_deref(), Some(dir.as_path()));
    assert_eq!(
        t.client
            .worktree_list(Some(t.workspace))
            .await
            .unwrap()
            .len(),
        listed.len()
    );
    t.wait_state(a.id, TaskState::Done).await;

    // An explicit branch, started from `main`.
    let started = t.client.task_start(b.id).await.unwrap();
    assert_eq!(
        started.session.cwd.as_deref(),
        Some(t.repo.wt("feature-export").as_path())
    );
    assert!(t.repo.branches().contains(&"feature/export".to_owned()));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_task_on_the_branch_of_the_main_checkout_does_not_start() {
    let t = Env::start().await;
    let a = t
        .create(TaskCreate {
            placement: worktree(Some("main")),
            ..t.script("On main", "exit 0", &[])
        })
        .await;
    let (code, text) = message(t.client.task_start(a.id).await);
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(text.contains("main checkout"), "{text}");
    // The task is as it was: nothing ran, and it can be fixed and started.
    let after = t.task(a.id).await;
    assert_eq!(after.state, TaskState::Ready);
    assert!(after.sessions.is_empty() && after.started_at_ms.is_none());
    let fixed = t
        .client
        .task_update(TaskUpdate {
            placement: worktree(Some("elsewhere")),
            ..update_of(a.id)
        })
        .await
        .unwrap();
    assert_eq!(fixed.placement.branch.as_deref(), Some("elsewhere"));
    t.client.task_start(a.id).await.unwrap();
    t.wait_state(a.id, TaskState::Done).await;

    // A branch name git refuses is reported by the start, not by the creation.
    let bad = t
        .create(TaskCreate {
            placement: worktree(Some("a..b")),
            ..t.script("Bad branch", "exit 0", &[])
        })
        .await;
    assert_eq!(
        message(t.client.task_start(bad.id).await).0,
        ErrorCode::BadRequest
    );
    assert_eq!(t.task(bad.id).await.state, TaskState::Ready);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_main_task_warns_when_another_live_agent_works_in_the_checkout() {
    let t = Env::start().await;
    let mut other = sh("exec sleep 600");
    other.cwd = Some(t.repo.root.clone());
    let other = create_in(&t.client, Some(t.workspace), other).await;
    t.report(other.id, AgentState::Working, 1).await;
    // Placement is derived by the daemon; wait for it to know the agent.
    timeout(WAIT, async {
        while t.session(other.id).await.agent.is_none() {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();

    let a = t
        .create(t.script("Second agent", "exec sleep 600", &[]))
        .await;
    let started = t.client.task_start(a.id).await.unwrap();
    assert_eq!(started.warnings.len(), 1, "{:?}", started.warnings);
    assert!(
        started.warnings[0].contains(&other.id.to_string()),
        "{:?}",
        started.warnings
    );
    assert!(
        started.warnings[0].contains(t.repo.root.to_str().unwrap()),
        "{:?}",
        started.warnings
    );

    // In a worktree of its own there is nothing to warn about.
    let b = t
        .create(TaskCreate {
            placement: worktree(None),
            ..t.script("Third agent", "exec sleep 600", &[])
        })
        .await;
    assert!(t.client.task_start(b.id).await.unwrap().warnings.is_empty());
    t.stop().await;
}

// start: Codex chats --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_task_sends_its_prompt_and_is_done_when_the_turn_completes() {
    let mut t = Env::start_with("", Some("plain-turn")).await;
    let a = t
        .create(TaskCreate {
            prompt: Some("Reply with the single word pong and nothing else.".into()),
            run: Some(TaskRun::Chat {
                agent: "codex".into(),
                model: None,
                account: None,
                safety: None,
            }),
            ..t.create_request("Say pong")
        })
        .await;
    let b = t.create(t.script("After pong", "exit 0", &[&a])).await;
    let started = t.client.task_start(a.id).await.unwrap();
    assert_eq!(started.session.kind, SessionKind::Harness);
    assert_eq!(started.session.title, "Say pong");
    assert_eq!(started.session.cwd.as_deref(), Some(t.repo.root.as_path()));
    assert_eq!(started.task.sessions, [started.session.id]);

    let done = t.wait_state(a.id, TaskState::Done).await;
    assert_eq!(done.outcome.as_deref(), Some("turn completed"));
    let received = fs::read_to_string(t.scratch.path().join("received.jsonl")).unwrap();
    assert!(
        received.contains("Reply with the single word pong"),
        "the prompt reached the agent as the first message"
    );
    let intent = t.intent(IntentKind::TaskDone, started.session.id).await;
    assert_eq!(intent.title, "Say pong: task done");
    t.wait_state(b.id, TaskState::Ready).await;
    // The chat itself is left alive for the user to continue.
    assert!(t.session(started.session.id).await.state.is_live());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_task_whose_turn_fails_is_failed() {
    let mut t = Env::start_with("", Some("turn-failed")).await;
    let a = t
        .create(TaskCreate {
            prompt: Some("Do something that fails.".into()),
            run: Some(TaskRun::Chat {
                agent: "codex".into(),
                model: None,
                account: None,
                safety: None,
            }),
            ..t.create_request("Doomed chat")
        })
        .await;
    let started = t.client.task_start(a.id).await.unwrap();
    let failed = t.wait_state(a.id, TaskState::Failed).await;
    assert!(
        failed
            .outcome
            .as_deref()
            .is_some_and(|o| o.starts_with("turn failed: ")),
        "{:?}",
        failed.outcome
    );
    let intent = t.intent(IntentKind::TaskFailed, started.session.id).await;
    assert!(intent.body.contains("turn failed"), "{}", intent.body);
    t.stop().await;
}

// auto start ----------------------------------------------------------------------------------

/// Follows `task.updated` events until every task of `ids` is done; the order in which they
/// started running, with the most that ever ran at once.
async fn follow_until_done(
    events: &mut broadcast::Receiver<(hutan_core::Seq, Event)>,
    ids: &[TaskId],
) -> (Vec<TaskId>, usize) {
    let mut states: std::collections::HashMap<TaskId, TaskState> = std::collections::HashMap::new();
    let mut order = Vec::new();
    let mut most = 0;
    timeout(Duration::from_secs(60), async {
        loop {
            let (_, event) = events.recv().await.expect("event stream closed");
            let Event::TaskUpdated { task } = event else {
                continue;
            };
            if ids.contains(&task.id) {
                if task.state == TaskState::Running && !order.contains(&task.id) {
                    order.push(task.id);
                }
                states.insert(task.id, task.state);
            }
            let running = states
                .values()
                .filter(|s| matches!(s, TaskState::Running | TaskState::Blocked))
                .count();
            most = most.max(running);
            if ids
                .iter()
                .all(|id| states.get(id) == Some(&TaskState::Done))
            {
                return;
            }
        }
    })
    .await
    .expect("the tasks never all finished");
    (order, most)
}

#[tokio::test(flavor = "multi_thread")]
async fn ready_tasks_with_auto_start_run_three_at_a_time_and_the_rest_in_order() {
    let mut t = Env::start().await;
    let mut ids = Vec::new();
    for n in 1..=5 {
        let task = t
            .create(TaskCreate {
                auto_start: true,
                ..t.script(&format!("auto {n}"), "sleep 1", &[])
            })
            .await;
        ids.push(task.id);
    }
    // A task that does not ask for it never starts by itself.
    let manual = t.create(t.script("manual", "exit 0", &[])).await;

    let (order, most) = follow_until_done(&mut t.events, &ids).await;
    assert_eq!(order, ids, "first come, first served");
    assert_eq!(most, 3, "[tasks] max_parallel defaults to 3");
    assert_eq!(t.task(manual.id).await.state, TaskState::Ready);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dependants_start_by_themselves_when_their_dependency_is_done_within_the_limit() {
    let mut t = Env::start_with("[tasks]\nmax_parallel = 1\n", None).await;
    let root = t.create(t.script("Build", "sleep 1", &[])).await;
    let mut ids = Vec::new();
    for name in ["Test", "Lint", "Docs"] {
        let task = t
            .create(TaskCreate {
                auto_start: true,
                ..t.script(name, "sleep 1", &[&root])
            })
            .await;
        ids.push(task.id);
    }
    // Nothing starts before the dependency is done.
    sleep(Duration::from_millis(400)).await;
    for id in &ids {
        assert_eq!(t.task(*id).await.state, TaskState::Todo);
    }
    t.client.task_start(root.id).await.unwrap();

    let mut all = vec![root.id];
    all.extend(&ids);
    let (order, most) = follow_until_done(&mut t.events, &all).await;
    assert_eq!(order, all);
    assert_eq!(most, 1, "one at a time with max_parallel = 1");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_auto_task_that_cannot_start_is_failed_and_frees_its_slot() {
    let mut t = Env::start_with("[tasks]\nmax_parallel = 1\n", None).await;
    let broken = t
        .create(TaskCreate {
            auto_start: true,
            placement: worktree(Some("main")),
            ..t.script("Broken placement", "exit 0", &[])
        })
        .await;
    let next = t
        .create(TaskCreate {
            auto_start: true,
            ..t.script("Fine", "exit 0", &[])
        })
        .await;
    let failed = t.wait_state(broken.id, TaskState::Failed).await;
    let outcome = failed.outcome.unwrap();
    assert!(outcome.starts_with("start failed: "), "{outcome}");
    assert!(outcome.contains("main checkout"), "{outcome}");
    assert!(failed.sessions.is_empty());
    t.wait_state(next.id, TaskState::Done).await;
    let _ = &mut t.events;
    t.stop().await;
}

// restart -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn running_tasks_keep_running_when_their_session_is_restored_and_fail_when_it_is_not() {
    let t = Env::start().await;
    let a = t.create(t.script("Survivor", "exec sleep 600", &[])).await;
    let b = t.create(t.script("After survivor", "exit 0", &[&a])).await;
    // A task nobody started keeps every field.
    let keep = t
        .create(TaskCreate {
            prompt: Some("remember me".into()),
            placement: Some(TaskPlacement {
                kind: TaskPlacementKind::Worktree,
                branch: Some("task/keep".into()),
                base: Some("main".into()),
            }),
            run: Some(TaskRun::Chat {
                agent: "codex".into(),
                model: None,
                account: None,
                safety: None,
            }),
            depends_on: vec![a.id, b.id],
            ..t.create_request("Kept")
        })
        .await;
    let started = t.client.task_start(a.id).await.unwrap();

    // Respawn is the default: the same session is alive again and the task still runs.
    let t = t.restart().await;
    let a2 = t.task(a.id).await;
    assert_eq!(a2.state, TaskState::Running);
    assert_eq!(a2.sessions, [started.session.id]);
    assert_eq!(a2.started_at_ms, started.task.started_at_ms);
    assert!(t.session(started.session.id).await.state.is_live());
    let b2 = t.task(b.id).await;
    assert_eq!(b2.state, TaskState::Todo);
    assert_eq!(b2.depends_on, [a.id]);
    assert_eq!(t.task(keep.id).await, keep, "every field is stored");
    // And it is followed again: stopping the restored session ends the task.
    common::stop(&t.client, started.session.id).await.unwrap();
    t.wait_state(a.id, TaskState::Cancelled).await;
    t.update(a.id, TaskState::Todo).await.unwrap();
    t.client.task_start(a.id).await.unwrap();
    t.wait_state(a.id, TaskState::Running).await;

    // Sessions listed as stopped after a restart are not running: the task is failed.
    fs::write(
        &t.daemon.paths.config_file,
        "[daemon]\nrestore_sessions = \"stopped\"\n",
    )
    .unwrap();
    let running = t.task(a.id).await;
    let session = *running.sessions.last().unwrap();
    let mut t = t.restart().await;
    let failed = t.task(a.id).await;
    assert_eq!(failed.state, TaskState::Failed);
    assert_eq!(failed.outcome.as_deref(), Some("interrupted"));
    assert!(failed.ended_at_ms.is_some());
    assert_eq!(failed.sessions, running.sessions);
    assert_eq!(t.task(b.id).await.state, TaskState::Todo);
    // The session that carried it says what it cost.
    let recovery = t.session(session).await.recovery.expect("a recovery");
    assert!(
        matches!(
            recovery.kind,
            hutan_protocol::recovery::RecoveryKind::NotRestarted { .. }
        ),
        "{:?}",
        recovery.kind
    );
    assert_eq!(
        recovery.lost,
        [hutan_protocol::recovery::Lost::Task {
            task_id: a.id,
            title: "Survivor".into()
        }]
    );
    // The restart is told like any other failure.
    let pending = t.client.notify_list().await.unwrap();
    let intent = pending
        .iter()
        .find(|i| i.kind == IntentKind::TaskFailed && i.session_id == session)
        .unwrap_or_else(|| panic!("no task_failed intent in {pending:?}"));
    assert!(intent.body.contains("interrupted"), "{}", intent.body);
    let _ = &mut t.events;
    t.stop().await;
}

// workspaces ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_workspace_takes_its_tasks_with_it() {
    let mut t = Env::start().await;
    let a = t.create(t.create_request("Doomed")).await;
    let b = t
        .create(TaskCreate {
            depends_on: vec![a.id],
            ..t.create_request("Doomed too")
        })
        .await;
    t.client
        .call::<hutan_protocol::messages::Empty>(Request::WorkspaceClose(
            hutan_protocol::messages::WorkspaceRef {
                workspace_id: t.workspace,
            },
        ))
        .await
        .unwrap();
    let mut gone = BTreeSet::new();
    while gone.len() < 2 {
        let id = wait_event(&mut t.events, "task.removed", |event| match event {
            Event::TaskRemoved { id } => Some(*id),
            _ => None,
        })
        .await;
        gone.insert(id);
    }
    assert_eq!(gone, [a.id, b.id].into_iter().collect());
    assert!(t.client.task_list(None, None).await.unwrap().is_empty());
    let t = t.restart().await;
    assert!(t.client.task_list(None, None).await.unwrap().is_empty());
    t.stop().await;
}

// delivery (protocol 0.25) ---------------------------------------------------------------------

/// Writes `name` in the checkout `dir` and commits it.
fn commit_file(dir: &Path, name: &str, text: &str) {
    fs::write(dir.join(name), text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", &format!("add {name}")]);
}

impl Env {
    /// A worktree task that runs `exit 0` on the branch of its title (`task/<slug>`), started
    /// and done.
    async fn finished_worktree_task(&self, title: &str, branch: Option<&str>) -> Task {
        let task = self
            .create(TaskCreate {
                placement: worktree(branch),
                ..self.script(title, "exit 0", &[])
            })
            .await;
        self.client.task_start(task.id).await.unwrap();
        self.wait_state(task.id, TaskState::Done).await
    }

    async fn review(
        &self,
        id: TaskId,
        decision: ReviewDecision,
        note: Option<&str>,
    ) -> Result<hutan_protocol::task::TaskReviewed, ClientError> {
        self.client
            .task_review(id, decision, note.map(str::to_owned))
            .await
    }
}

fn deliveries(task: &Task) -> Vec<DeliveryState> {
    task.delivery_history.iter().map(|r| r.state).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_task_awaits_review_until_a_person_accepts_it_or_asks_for_changes() {
    let mut t = Env::start().await;
    let a = t
        .create(TaskCreate {
            placement: worktree(None),
            ..t.script("Fix login", "exit 0", &[])
        })
        .await;
    assert_eq!(a.delivery, DeliveryState::None);
    t.client.task_start(a.id).await.unwrap();
    let done = t.wait_state(a.id, TaskState::Done).await;
    assert_eq!(done.delivery, DeliveryState::AwaitingReview);
    assert_eq!(deliveries(&done), [DeliveryState::AwaitingReview]);
    assert_eq!(done.delivery_history[0].by, DeliveryActor::Daemon);
    assert!(done.delivery_history[0].at_ms >= done.started_at_ms.unwrap());
    // The move is announced like every other change of the task.
    wait_event(
        &mut t.events,
        "task.updated awaiting_review",
        |event| match event {
            Event::TaskUpdated { task }
                if task.id == a.id && task.delivery == DeliveryState::AwaitingReview =>
            {
                Some(())
            }
            _ => None,
        },
    )
    .await;

    // Only a done task can be reviewed.
    let running = t
        .create(t.script("Still running", "exec sleep 600", &[]))
        .await;
    t.client.task_start(running.id).await.unwrap();
    let (code, text) = message(t.review(running.id, ReviewDecision::Accept, None).await);
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(text.contains("only a done task"), "{text}");
    assert_eq!(
        message(t.review(TaskId::new(), ReviewDecision::Accept, None).await).0,
        ErrorCode::NotFound
    );
    let long = "x".repeat(16 * 1024 + 1);
    assert_eq!(
        message(
            t.review(a.id, ReviewDecision::RequestChanges, Some(&long))
                .await
        )
        .0,
        ErrorCode::BadRequest
    );

    // The CLI accepts; accepting again records nothing new.
    let accepted = t.review(a.id, ReviewDecision::Accept, None).await.unwrap();
    assert!(accepted.session.is_none());
    assert_eq!(accepted.task.delivery, DeliveryState::Accepted);
    assert_eq!(
        accepted.task.delivery_history.last().unwrap().by,
        DeliveryActor::Cli
    );
    let again = t.review(a.id, ReviewDecision::Accept, None).await.unwrap();
    assert_eq!(again.task.delivery_history.len(), 2);

    // The desktop asks for changes: the terminal agent cannot be written to, so the note stays
    // on the task, which stays done.
    let desktop = t.daemon.connect_as(ClientKind::Desktop).await;
    let asked = desktop
        .task_review(
            a.id,
            ReviewDecision::RequestChanges,
            Some("  Rename the helper.  ".into()),
        )
        .await
        .unwrap();
    assert!(asked.session.is_none());
    assert_eq!(asked.task.state, TaskState::Done);
    assert_eq!(asked.task.delivery, DeliveryState::ChangesRequested);
    assert_eq!(asked.task.change_request(), Some("Rename the helper."));
    assert_eq!(
        asked.task.delivery_history.last().unwrap().by,
        DeliveryActor::Desktop
    );
    let accepted = t.review(a.id, ReviewDecision::Accept, None).await.unwrap();
    assert_eq!(
        deliveries(&accepted.task),
        [
            DeliveryState::AwaitingReview,
            DeliveryState::Accepted,
            DeliveryState::ChangesRequested,
            DeliveryState::Accepted
        ]
    );
    assert_eq!(accepted.task.change_request(), None);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_work_that_a_person_or_an_agent_did_awaits_review() {
    let t = Env::start().await;
    // A command that exits 0 in the main checkout has nothing to review.
    let plain = t.create(t.script("Run tests", "exit 0", &[])).await;
    t.client.task_start(plain.id).await.unwrap();
    let plain = t.wait_state(plain.id, TaskState::Done).await;
    assert_eq!(plain.delivery, DeliveryState::None);
    assert!(plain.delivery_history.is_empty());

    // An agent that finishes in the main checkout does.
    let agent = t.create(t.script("Agent run", "exec sleep 600", &[])).await;
    let session = t.client.task_start(agent.id).await.unwrap().session.id;
    t.report(session, AgentState::Working, 1).await;
    t.report(session, AgentState::Idle, 2).await;
    let agent = t.wait_state(agent.id, TaskState::Done).await;
    assert_eq!(agent.delivery, DeliveryState::AwaitingReview);

    // "Marked done" is the user's word, not a run that left work: nothing awaits review, but
    // the verdict can still be given (a dependant may wait for it).
    let shell = t.create(t.create_request("Shell")).await;
    t.client.task_start(shell.id).await.unwrap();
    let shell = t.update(shell.id, TaskState::Done).await.unwrap();
    assert_eq!(shell.delivery, DeliveryState::None);
    let accepted = t
        .review(shell.id, ReviewDecision::Accept, None)
        .await
        .unwrap();
    assert_eq!(accepted.task.delivery, DeliveryState::Accepted);

    // Running it again forgets the old verdict (the history stays).
    t.update(shell.id, TaskState::Todo).await.unwrap();
    let reset = t.task(shell.id).await;
    assert_eq!(reset.delivery, DeliveryState::None);
    assert_eq!(deliveries(&reset), [DeliveryState::Accepted]);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dependency_policy_waits_for_acceptance_or_the_merge() {
    let mut t = Env::start().await;
    let a = t.finished_worktree_task("Alpha", None).await;
    assert_eq!(a.delivery, DeliveryState::AwaitingReview);
    let dependant = |title: &str, policy| TaskCreate {
        after_policy: policy,
        ..t.script(title, "exit 0", &[&a])
    };
    let by_done = t.create(dependant("After done", AfterPolicy::Done)).await;
    let by_accept = t
        .create(dependant("After accept", AfterPolicy::Accepted))
        .await;
    let by_merge = t
        .create(dependant("After merge", AfterPolicy::Merged))
        .await;
    assert_eq!(by_accept.after_policy, AfterPolicy::Accepted);
    // Today's behaviour is the default: done is enough.
    assert_eq!(by_done.state, TaskState::Ready);
    assert_eq!(by_accept.state, TaskState::Todo);
    assert_eq!(by_merge.state, TaskState::Todo);
    let (code, text) = message(t.client.task_start(by_accept.id).await);
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(
        text.contains("awaiting_review") && text.contains("accepted"),
        "{text}"
    );

    // Accepting releases the first, not the second.
    t.review(a.id, ReviewDecision::Accept, None).await.unwrap();
    t.wait_state(by_accept.id, TaskState::Ready).await;
    assert_eq!(t.task(by_merge.id).await.state, TaskState::Todo);
    wait_event(&mut t.events, "task.updated ready", |event| match event {
        Event::TaskUpdated { task }
            if task.id == by_accept.id && task.state == TaskState::Ready =>
        {
            Some(())
        }
        _ => None,
    })
    .await;

    // Asking for changes takes the acceptance back.
    t.review(a.id, ReviewDecision::RequestChanges, None)
        .await
        .unwrap();
    t.wait_state(by_accept.id, TaskState::Todo).await;

    // A policy can be changed while the task waits.
    let relaxed = t
        .client
        .task_update(TaskUpdate {
            after_policy: Some(AfterPolicy::Done),
            ..update_of(by_merge.id)
        })
        .await
        .unwrap();
    assert_eq!(relaxed.after_policy, AfterPolicy::Done);
    t.wait_state(by_merge.id, TaskState::Ready).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_merge_marks_the_task_merged_with_its_commit_and_a_removal_discards_it() {
    let t = Env::start().await;
    let a = t.finished_worktree_task("Alpha", None).await;
    let b = t.finished_worktree_task("Beta", Some("feature/beta")).await;
    let gated = t
        .create(TaskCreate {
            after_policy: AfterPolicy::Merged,
            ..t.script("After merge", "exit 0", &[&a])
        })
        .await;
    t.review(a.id, ReviewDecision::Accept, None).await.unwrap();
    assert_eq!(t.task(gated.id).await.state, TaskState::Todo);

    // One commit on the branch, and work that was never committed.
    let dir_a = t.repo.wt("task-alpha");
    commit_file(&dir_a, "alpha.txt", "alpha\n");
    fs::write(dir_a.join("scratch.txt"), "wip\n").unwrap();
    let diff = t
        .client
        .worktree_diff(WorktreeDiffRequest {
            path: dir_a.clone(),
            base: None,
            include_uncommitted: true,
        })
        .await
        .unwrap();
    assert_eq!(diff.commits.len(), 1, "{diff:?}");
    assert_eq!(diff.commits[0].subject, "add alpha.txt");
    let flags: Vec<(&str, bool, bool)> = diff
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.committed, f.uncommitted))
        .collect();
    assert_eq!(
        flags,
        [("alpha.txt", true, false), ("scratch.txt", false, true)]
    );

    let merged = t
        .client
        .worktree_merge(WorktreeMerge {
            path: dir_a.clone(),
            into: None,
            strategy: MergeStrategy::Squash,
            message: Some("Alpha".into()),
        })
        .await
        .unwrap();
    assert!(merged.source_dirty, "the scratch file stayed behind");
    let after = t.task(a.id).await;
    assert_eq!(after.delivery, DeliveryState::Merged);
    let record = after.delivery_history.last().unwrap();
    assert_eq!(record.state, DeliveryState::Merged);
    assert_eq!(record.commit.as_deref(), Some(merged.commit.as_str()));
    assert_eq!(record.strategy.as_deref(), Some("squash"));
    assert_eq!(record.by, DeliveryActor::Cli);
    // Its dependants that waited for the merge go on, and nothing else was touched.
    t.wait_state(gated.id, TaskState::Ready).await;
    assert_eq!(t.task(b.id).await.delivery, DeliveryState::AwaitingReview);
    // Merged work cannot be reviewed any more.
    assert_eq!(
        message(t.review(a.id, ReviewDecision::Accept, None).await).0,
        ErrorCode::InvalidState
    );

    // Removing the worktree of work nobody merged discards it.
    t.client
        .worktree_remove(WorktreeRemove {
            path: t.repo.wt("feature-beta"),
            force: false,
            delete_branch: true,
        })
        .await
        .unwrap();
    let discarded = t.task(b.id).await;
    assert_eq!(discarded.delivery, DeliveryState::Discarded);
    assert_eq!(
        deliveries(&discarded),
        [DeliveryState::AwaitingReview, DeliveryState::Discarded]
    );
    // And removing the worktree of merged work leaves it merged.
    t.client
        .worktree_remove(WorktreeRemove {
            path: dir_a,
            force: true,
            delete_branch: true,
        })
        .await
        .unwrap();
    assert_eq!(t.task(a.id).await.delivery, DeliveryState::Merged);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_requested_on_a_chat_task_reach_the_chat_and_the_task_runs_again() {
    // Two turns, each held until the test made the agent's edit.
    let t = Env::start_with_transcript(|dir| {
        crate::checkpoint_turns::held_turns(dir, &[dir.join("gate-0"), dir.join("gate-1")])
    })
    .await;
    let release = |n: usize| fs::write(t.scratch.path().join(format!("gate-{n}")), "").unwrap();
    let a = t
        .create(TaskCreate {
            prompt: Some("Reply with the single word pong and nothing else.".into()),
            run: Some(TaskRun::Chat {
                agent: "codex".into(),
                model: None,
                account: None,
                safety: None,
            }),
            ..t.create_request("Say pong")
        })
        .await;
    let started = t.client.task_start(a.id).await.unwrap();
    fs::write(t.repo.root.join("pong.txt"), "pong\n").unwrap();
    release(0);
    // The main checkout, but the agent's turn changed a file: it awaits review.
    let done = t
        .wait(a.id, "never awaited review", |task| {
            task.state == TaskState::Done && task.delivery == DeliveryState::AwaitingReview
        })
        .await;
    assert_eq!(deliveries(&done), [DeliveryState::AwaitingReview]);

    let asked = t
        .review(
            a.id,
            ReviewDecision::RequestChanges,
            Some("Also say it in French."),
        )
        .await
        .unwrap();
    assert_eq!(asked.session, Some(started.session.id));
    // The replayed second turn may be over by the time the answer is built.
    assert!(
        asked
            .task
            .delivery_history
            .iter()
            .any(|r| r.state == DeliveryState::ChangesRequested),
        "{:?}",
        asked.task
    );
    let received = fs::read_to_string(t.scratch.path().join("received.jsonl")).unwrap();
    assert!(
        received.contains("Also say it in French."),
        "the note reached the agent as a user message"
    );

    // The second turn changes the file again and ends: awaiting review again, and the history
    // says what happened.
    fs::write(t.repo.root.join("pong.txt"), "pong\nping\n").unwrap();
    release(1);
    let again = t
        .wait(a.id, "never awaited review again", |task| {
            task.state == TaskState::Done
                && task.delivery == DeliveryState::AwaitingReview
                && task.delivery_history.len() == 3
        })
        .await;
    assert_eq!(
        deliveries(&again),
        [
            DeliveryState::AwaitingReview,
            DeliveryState::ChangesRequested,
            DeliveryState::AwaitingReview
        ]
    );
    assert_eq!(
        again.delivery_history[1].note.as_deref(),
        Some("Also say it in French.")
    );

    // Without a note there is nothing to send: the decision is only recorded.
    let quiet = t
        .review(a.id, ReviewDecision::RequestChanges, None)
        .await
        .unwrap();
    assert!(quiet.session.is_none());
    assert_eq!(quiet.task.state, TaskState::Done);

    // A chat that was stopped cannot take the note: it stays on the task.
    common::stop(&t.client, started.session.id).await.unwrap();
    let kept = t
        .review(
            a.id,
            ReviewDecision::RequestChanges,
            Some("One more thing."),
        )
        .await
        .unwrap();
    assert!(kept.session.is_none());
    assert_eq!(kept.task.state, TaskState::Done);
    assert_eq!(kept.task.change_request(), Some("One more thing."));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delivery_and_policy_survive_a_restart() {
    let t = Env::start().await;
    let a = t.finished_worktree_task("Alpha", None).await;
    let b = t
        .create(TaskCreate {
            after_policy: AfterPolicy::Accepted,
            ..t.script("After accept", "exit 0", &[&a])
        })
        .await;
    t.review(a.id, ReviewDecision::RequestChanges, Some("Not yet."))
        .await
        .unwrap();
    let before = t.task(a.id).await;
    assert_eq!(before.delivery, DeliveryState::ChangesRequested);

    let t = t.restart().await;
    assert_eq!(t.task(a.id).await, before, "every field, the history too");
    let b2 = t.task(b.id).await;
    assert_eq!(b2.after_policy, AfterPolicy::Accepted);
    assert_eq!(b2.state, TaskState::Todo, "still waiting for acceptance");
    t.review(a.id, ReviewDecision::Accept, None).await.unwrap();
    t.wait_state(b.id, TaskState::Ready).await;
    t.stop().await;
}

#[test]
fn the_codex_fixture_names_used_here_exist() {
    for name in ["plain-turn", "turn-failed"] {
        assert!(fixtures().join(format!("{name}.jsonl")).is_file(), "{name}");
    }
}
