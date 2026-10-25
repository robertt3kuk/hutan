//! Worktree setup, teardown and retention (protocol 0.48, capability `worktree.setup`) end to end:
//! a real daemon on a temporary `HUTAN_HOME`, real connections and PTY sessions, and git
//! repositories the tests make in temporary directories.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use common::{TestDaemon, View, create_in, remote_code, sh, stop, terminal_of, wait_event};
use hutan_client::Client;
use hutan_core::{SessionState, TaskPlacement, TaskPlacementKind, TaskRun, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Event, Origin, Request, SessionInfo, SessionList,
    SessionListFilter, WorkspaceOpen, WorkspaceOpened,
};
use hutan_protocol::task::TaskCreate;
use hutan_protocol::worktree::{
    CleanupReason, WorktreeCleanup, WorktreeCleanupRequest, WorktreeCreate, WorktreeInfo,
    WorktreePrune, WorktreeRemove, WorktreeSetup, WorktreeSetupRequest, WorktreeSetupState,
    WorktreeSetupTrust,
};
use tempfile::TempDir;
use tokio::sync::broadcast;

// Fixtures ------------------------------------------------------------------------------------

fn git_with(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
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
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_with(dir, args, &[])
}

/// `<tmp>/repo` on `main` with a gitignore, a tracked `config.json` and one commit made in 2020.
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
        fs::write(root.join(".gitignore"), ".env*\nsecrets/\n").unwrap();
        fs::write(root.join("config.json"), "{}\n").unwrap();
        fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&root, &["add", "."]);
        git_with(
            &root,
            &["commit", "-q", "-m", "first"],
            &[
                ("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z"),
                ("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z"),
            ],
        );
        Self { _dir: dir, root }
    }

    fn wt(&self, name: &str) -> PathBuf {
        self.root.parent().unwrap().join("repo-wt").join(name)
    }

    /// Writes (and commits) `.hutan/worktree.toml`.
    fn config(&self, text: &str) {
        fs::create_dir_all(self.root.join(".hutan")).unwrap();
        fs::write(self.root.join(".hutan/worktree.toml"), text).unwrap();
        git(&self.root, &["add", ".hutan/worktree.toml"]);
        git(&self.root, &["commit", "-q", "-m", "worktree config"]);
    }

    fn branches(&self) -> Vec<String> {
        git(&self.root, &["branch", "--format=%(refname:short)"])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

struct Env {
    daemon: TestDaemon,
    client: Client,
    events: broadcast::Receiver<(hutan_core::Seq, Event)>,
    repo: Repo,
    workspace: WorkspaceId,
}

impl Env {
    async fn start() -> Self {
        Self::start_with("").await
    }

    /// `toml` is `daemon.toml`; `[worktrees.projects.repo]` names the project of the repository.
    async fn start_with(toml: &str) -> Self {
        let daemon = TestDaemon::start_with(toml, |_| {}).await;
        let repo = Repo::new();
        let (client, events) = daemon.connect_subscribed().await;
        let workspace = client
            .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
                path: repo.root.clone(),
                created_by: None,
            }))
            .await
            .unwrap()
            .workspace
            .id;
        Self {
            daemon,
            client,
            events,
            repo,
            workspace,
        }
    }

    fn request(&self, branch: &str) -> WorktreeCreate {
        WorktreeCreate {
            workspace_id: self.workspace,
            repo: None,
            branch: branch.to_owned(),
            base: None,
            path: None,
            no_setup: false,
        }
    }

    async fn create(&self, branch: &str) -> WorktreeInfo {
        self.client
            .worktree_create(self.request(branch))
            .await
            .unwrap()
    }

    async fn list(&self) -> Vec<WorktreeInfo> {
        self.client
            .worktree_list(Some(self.workspace))
            .await
            .unwrap()
    }

    async fn setup_of(&self, path: &Path) -> Option<WorktreeSetup> {
        self.list()
            .await
            .into_iter()
            .find(|w| w.path == path)
            .and_then(|w| w.setup)
    }

    /// Waits for the `worktree.changed` events until the worktree's setup satisfies `pick`.
    async fn wait_setup(
        &mut self,
        path: &Path,
        what: &str,
        pick: impl Fn(&WorktreeSetup) -> bool,
    ) -> WorktreeSetup {
        loop {
            if let Some(setup) = self.setup_of(path).await
                && pick(&setup)
            {
                return setup;
            }
            let workspace = self.workspace;
            wait_event(&mut self.events, what, |event| match event {
                Event::WorktreeChanged { workspace_id } if *workspace_id == workspace => Some(()),
                _ => None,
            })
            .await;
        }
    }

    async fn sessions(&self) -> Vec<SessionInfo> {
        self.client
            .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap()
            .sessions
    }

    async fn setup_session(&self, path: &Path) -> SessionInfo {
        self.sessions()
            .await
            .into_iter()
            .find(|s| s.title == "Setup" && s.cwd.as_deref() == Some(path))
            .expect("a Setup session in the worktree")
    }

    async fn agent(&self) -> Client {
        let session = create_in(&self.client, None, sh("sleep 60")).await;
        let reporter = self.daemon.connect().await;
        let applied: AgentApplied = reporter
            .call(Request::AgentReport(AgentReportRequest {
                session_id: session.id,
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
        let mut options = self.daemon.options();
        options.client.origin = Some(Origin {
            session_id: session.id,
        });
        Client::connect(options).await.unwrap()
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

const OWN_SETUP: &str = "[worktrees.projects.repo]\n";

// copy ----------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn only_gitignored_files_matching_a_glob_are_copied_and_the_rest_is_told() {
    let mut env = Env::start().await;
    env.repo
        .config("copy = [\".env*\", \"*.json\", \"*.txt\", \"secrets/*.key\"]\nmystery = 1\n");
    fs::write(env.repo.root.join(".env"), "A=1\n").unwrap();
    fs::write(env.repo.root.join(".env.local"), "B=2\n").unwrap();
    fs::create_dir(env.repo.root.join("secrets")).unwrap();
    fs::write(env.repo.root.join("secrets/api.key"), "k\n").unwrap();
    // Untracked and not ignored: not copied.
    fs::write(env.repo.root.join("notes.txt"), "n\n").unwrap();

    let created = env.create("feature/copy").await;
    let wt = env.repo.wt("feature-copy");
    assert_eq!(created.path, wt);
    // Copying is the whole setup: it is done, with no session.
    let setup = created.setup.expect("a setup state");
    assert_eq!(setup.state, WorktreeSetupState::Done);
    assert_eq!(setup.copied, 3);
    assert_eq!(setup.session_id, None);
    let message = setup.message.unwrap();
    assert!(message.contains("unknown key `mystery`"), "{message}");
    assert!(
        message.contains("notes.txt (not ignored by git"),
        "{message}"
    );
    assert!(
        message.contains("config.json (not ignored by git"),
        "{message}"
    );

    assert_eq!(fs::read_to_string(wt.join(".env")).unwrap(), "A=1\n");
    assert_eq!(fs::read_to_string(wt.join(".env.local")).unwrap(), "B=2\n");
    assert_eq!(
        fs::read_to_string(wt.join("secrets/api.key")).unwrap(),
        "k\n"
    );
    assert!(!wt.join("notes.txt").exists());
    // The tracked file is the checkout's own, untouched.
    assert_eq!(fs::read_to_string(wt.join("config.json")).unwrap(), "{}\n");
    assert!(env.sessions().await.iter().all(|s| s.title != "Setup"));
    // The state is listed, and the clients are told.
    env.wait_setup(&wt, "listed setup", |s| s.state == WorktreeSetupState::Done)
        .await;
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_setup_skips_everything_and_setup_runs_it_afterwards() {
    let mut env = Env::start_with(&format!("{OWN_SETUP}setup = [\"touch ran\"]\n")).await;
    fs::write(env.repo.root.join(".env"), "A=1\n").unwrap();
    env.repo.config("copy = [\".env\"]\n");

    let created = env
        .client
        .worktree_create(WorktreeCreate {
            no_setup: true,
            ..env.request("quiet")
        })
        .await
        .unwrap();
    assert_eq!(created.setup, None);
    assert!(!created.path.join(".env").exists());
    assert!(env.sessions().await.iter().all(|s| s.title != "Setup"));

    let result = env
        .client
        .worktree_setup(WorktreeSetupRequest {
            path: created.path.clone(),
        })
        .await
        .unwrap();
    assert!(result.worktree.setup.is_some());
    env.wait_setup(&created.path, "setup done", |s| {
        s.state == WorktreeSetupState::Done
    })
    .await;
    assert_eq!(
        fs::read_to_string(created.path.join(".env")).unwrap(),
        "A=1\n"
    );
    assert!(created.path.join("ran").exists());
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_is_refused_for_the_main_worktree_a_running_setup_and_an_empty_config() {
    let mut env = Env::start_with(&format!("{OWN_SETUP}setup = [\"cat\"]\n")).await;
    let main = env.repo.root.clone();
    let refused = env
        .client
        .worktree_setup(WorktreeSetupRequest { path: main })
        .await;
    assert_eq!(remote_code(refused), ErrorCode::InvalidState);
    let relative = env
        .client
        .worktree_setup(WorktreeSetupRequest {
            path: "relative".into(),
        })
        .await;
    assert_eq!(remote_code(relative), ErrorCode::BadRequest);

    let created = env.create("busy").await;
    assert_eq!(created.setup.unwrap().state, WorktreeSetupState::Running);
    let again = env
        .client
        .worktree_setup(WorktreeSetupRequest {
            path: created.path.clone(),
        })
        .await;
    assert_eq!(remote_code(again), ErrorCode::InvalidState);
    let session = env.setup_session(&created.path).await;
    stop(&env.client, session.id).await.unwrap();
    env.wait_setup(&created.path, "stopped", |s| {
        s.state == WorktreeSetupState::Failed
    })
    .await;
    env.stop().await;

    // Nothing configured at all.
    let env = Env::start().await;
    let created = env.create("plain").await;
    assert_eq!(created.setup, None);
    let nothing = env
        .client
        .worktree_setup(WorktreeSetupRequest { path: created.path })
        .await;
    assert_eq!(remote_code(nothing), ErrorCode::InvalidState);
    env.stop().await;
}

// running -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn setup_runs_in_a_visible_session_that_shows_its_output_and_can_be_stopped() {
    let mut env = Env::start_with(&format!(
        "{OWN_SETUP}setup = [\"echo hello-from-setup\", \"cat\"]\n"
    ))
    .await;
    let created = env.create("visible").await;
    let setup = created.setup.unwrap();
    assert_eq!(setup.state, WorktreeSetupState::Running);
    assert_eq!(setup.commands, ["echo hello-from-setup", "cat"]);
    let session_id = setup.session_id.expect("the Setup session");

    let session = env.setup_session(&created.path).await;
    assert_eq!(session.id, session_id);
    assert_eq!(session.cwd.as_deref(), Some(created.path.as_path()));
    assert!(session.state.is_live());
    // The worktree lists it as a session working in it.
    assert!(
        env.list()
            .await
            .iter()
            .find(|w| w.path == created.path)
            .unwrap()
            .live_sessions
            .contains(&session.id)
    );
    // The user sees the commands and their output.
    let mut view = View::attach(&env.client, terminal_of(&session), None).await;
    view.wait_for("the echoed command", |screen| {
        screen.contains("$ echo hello-from-setup")
    })
    .await;
    view.wait_for("its output", |screen| screen.contains("hello-from-setup"))
        .await;

    // Stopping it is the user's call: the setup failed, and says it was stopped.
    stop(&env.client, session.id).await.unwrap();
    let failed = env
        .wait_setup(&created.path, "setup stopped", |s| {
            s.state == WorktreeSetupState::Failed
        })
        .await;
    assert!(failed.message.unwrap().contains("stopped"));
    assert_eq!(failed.session_id, Some(session_id));
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_setup_is_done_and_a_failing_command_fails_it_and_stops_the_rest() {
    let mut env = Env::start_with(&format!(
        "{OWN_SETUP}setup = [\"echo one > one.txt\", \"echo two > two.txt\"]\n"
    ))
    .await;
    let ok = env.create("works").await;
    let done = env
        .wait_setup(&ok.path, "done", |s| s.state == WorktreeSetupState::Done)
        .await;
    assert_eq!(done.copied, 0);
    assert_eq!(
        fs::read_to_string(ok.path.join("two.txt")).unwrap(),
        "two\n"
    );
    let session = env.setup_session(&ok.path).await;
    assert_eq!(session.id, done.session_id.unwrap());
    env.stop().await;

    let mut env = Env::start_with(&format!(
        "{OWN_SETUP}setup = [\"exit 7\", \"touch ran-after\"]\n"
    ))
    .await;
    let bad = env.create("breaks").await;
    let failed = env
        .wait_setup(&bad.path, "failed", |s| {
            s.state == WorktreeSetupState::Failed
        })
        .await;
    assert!(failed.message.unwrap().contains("code 7"));
    assert!(!bad.path.join("ran-after").exists());
    // The session stays listed with its code: *Show output* has something to show.
    let session = env.setup_session(&bad.path).await;
    assert_eq!(session.id, failed.session_id.unwrap());
    assert!(matches!(
        session.state,
        SessionState::Exited { code: Some(7), .. }
    ));
    // Run again once the command is fixed in the same worktree: a new Setup session.
    env.stop().await;
}

// trust ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn commands_of_the_repository_wait_for_the_user_and_are_remembered_by_their_hash() {
    // Agents may mutate without asking, so what stops them is the trust rule itself.
    let mut env = Env::start_with("[control]\nagents = \"allow\"\n").await;
    env.repo
        .config("setup = [\"touch trusted-ran\"]\nteardown = [\"true\"]\n");

    // Nothing runs before the user said yes; the sheet has the exact commands and the hash.
    let first = env.create("first").await;
    let setup = first.setup.clone().unwrap();
    assert_eq!(setup.state, WorktreeSetupState::NeedsTrust);
    assert_eq!(setup.commands, ["touch trusted-ran"]);
    assert_eq!(setup.teardown, ["true"]);
    let hash = setup.hash.clone().expect("a hash to confirm");
    assert_eq!(hash.len(), 64);
    assert_eq!(setup.session_id, None);
    assert!(!first.path.join("trusted-ran").exists());
    assert!(env.sessions().await.iter().all(|s| s.title != "Setup"));

    // An agent cannot confirm, nor can it get its own worktree's commands to run.
    let agent = env.agent().await;
    let forbidden = agent
        .worktree_setup_trust(WorktreeSetupTrust {
            path: first.path.clone(),
            hash: hash.clone(),
        })
        .await;
    assert_eq!(remote_code(forbidden), ErrorCode::Forbidden);
    let by_agent = agent
        .worktree_create(env.request("by-agent"))
        .await
        .unwrap();
    assert_eq!(
        by_agent.setup.as_ref().map(|s| s.state),
        Some(WorktreeSetupState::NeedsTrust),
        "the agent's worktree waits for the user too"
    );
    assert!(!by_agent.path.join("trusted-ran").exists());
    assert!(env.sessions().await.iter().all(|s| s.title != "Setup"));

    // A hash the user was not shown is refused, and the commands still did not run.
    let stale = env
        .client
        .worktree_setup_trust(WorktreeSetupTrust {
            path: first.path.clone(),
            hash: "0".repeat(64),
        })
        .await;
    assert_eq!(remote_code(stale), ErrorCode::Conflict);
    assert!(!first.path.join("trusted-ran").exists());

    // The user confirms: the setup runs.
    let result = env
        .client
        .worktree_setup_trust(WorktreeSetupTrust {
            path: first.path.clone(),
            hash: hash.clone(),
        })
        .await
        .unwrap();
    assert_ne!(
        result.worktree.setup.unwrap().state,
        WorktreeSetupState::NeedsTrust
    );
    env.wait_setup(&first.path, "done", |s| s.state == WorktreeSetupState::Done)
        .await;
    assert!(first.path.join("trusted-ran").exists());

    // The same commands in the next worktree run without asking.
    let second = env.create("second").await;
    assert_ne!(second.setup.unwrap().state, WorktreeSetupState::NeedsTrust);
    env.wait_setup(&second.path, "done", |s| {
        s.state == WorktreeSetupState::Done
    })
    .await;
    assert!(second.path.join("trusted-ran").exists());

    // Changing the commands asks again, with another hash.
    env.repo
        .config("setup = [\"touch trusted-ran\", \"touch added\"]\n");
    let third = env.create("third").await;
    let again = third.setup.unwrap();
    assert_eq!(again.state, WorktreeSetupState::NeedsTrust);
    assert_ne!(again.hash.unwrap(), hash);
    assert!(!third.path.join("added").exists());
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_confirmation_survives_a_daemon_restart() {
    let env = Env::start().await;
    env.repo.config("setup = [\"touch ran\"]\n");
    let first = env.create("first").await;
    let hash = first.setup.unwrap().hash.unwrap();
    env.client
        .worktree_setup_trust(WorktreeSetupTrust {
            path: first.path.clone(),
            hash,
        })
        .await
        .unwrap();
    let Env {
        daemon,
        repo,
        workspace,
        client,
        ..
    } = env;
    drop(client);
    let daemon = daemon.restart().await;
    let client = daemon.connect().await;
    let second = client
        .worktree_create(WorktreeCreate {
            workspace_id: workspace,
            repo: None,
            branch: "second".into(),
            base: None,
            path: None,
            no_setup: false,
        })
        .await
        .unwrap();
    assert_ne!(
        second.setup.unwrap().state,
        WorktreeSetupState::NeedsTrust,
        "the user confirmed these commands before the restart"
    );
    drop(repo);
    daemon.stop().await;
}

// teardown ------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn teardown_runs_before_removal_and_a_failing_one_keeps_the_worktree() {
    let env = Env::start_with(&format!(
        "{OWN_SETUP}teardown = [\"echo bye > \\\"$HUTAN_MAIN_CHECKOUT/teardown.log\\\"\"]\n"
    ))
    .await;
    let created = env.create("temporary").await;
    env.client
        .worktree_remove(WorktreeRemove {
            path: created.path.clone(),
            force: false,
            delete_branch: false,
        })
        .await
        .unwrap();
    assert!(!created.path.exists());
    assert_eq!(
        fs::read_to_string(env.repo.root.join("teardown.log")).unwrap(),
        "bye\n"
    );
    env.stop().await;

    let env = Env::start_with(&format!(
        "{OWN_SETUP}teardown = [\"echo it-broke >&2; exit 4\"]\n"
    ))
    .await;
    let created = env.create("stuck").await;
    let refused = env
        .client
        .worktree_remove(WorktreeRemove {
            path: created.path.clone(),
            force: false,
            delete_branch: false,
        })
        .await;
    let (code, message) = match refused {
        Err(hutan_client::ClientError::Remote(error)) => (error.code, error.message),
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(
        message.contains("it-broke") && message.contains("exited with 4"),
        "{message}"
    );
    assert!(created.path.exists(), "the worktree stays");
    // The user can still remove it.
    env.client
        .worktree_remove(WorktreeRemove {
            path: created.path.clone(),
            force: true,
            delete_branch: false,
        })
        .await
        .unwrap();
    assert!(!created.path.exists());
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_teardown_the_user_never_confirmed_does_not_run() {
    let env = Env::start().await;
    env.repo
        .config("teardown = [\"touch \\\"$HUTAN_MAIN_CHECKOUT/should-not-exist\\\"\"]\n");
    let created = env.create("plain").await;
    env.client
        .worktree_remove(WorktreeRemove {
            path: created.path.clone(),
            force: false,
            delete_branch: false,
        })
        .await
        .unwrap();
    assert!(!created.path.exists());
    assert!(!env.repo.root.join("should-not-exist").exists());
    env.stop().await;
}

// waiting for setup ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_task_placed_in_a_worktree_waits_for_its_setup_unless_told_not_to() {
    let env = Env::start_with(&format!("{OWN_SETUP}setup = [\"cat\"]\n")).await;
    let task = env
        .client
        .task_create(TaskCreate {
            workspace_id: env.workspace,
            title: "Needs deps".into(),
            prompt: None,
            depends_on: vec![],
            placement: Some(TaskPlacement {
                kind: TaskPlacementKind::Worktree,
                branch: Some("task/needs-deps".into()),
                base: None,
            }),
            run: Some(TaskRun::Terminal {
                argv: vec!["sh".into(), "-c".into(), "sleep 60".into()],
            }),
            auto_start: false,
            after_policy: hutan_core::AfterPolicy::Done,
        })
        .await
        .unwrap();
    let client = env.daemon.connect().await;
    let id = task.id;
    let start = tokio::spawn(async move { client.task_start(id).await });
    // The Setup session shows up first and holds the task back.
    let wt = env.repo.wt("task-needs-deps");
    let session = tokio::time::timeout(common::WAIT, async {
        loop {
            if let Some(found) = env
                .sessions()
                .await
                .into_iter()
                .find(|s| s.title == "Setup" && s.cwd.as_deref() == Some(wt.as_path()))
            {
                return found;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the Setup session starts");
    assert!(!start.is_finished(), "the task waits while the setup runs");
    stop(&env.client, session.id).await.unwrap();
    let started = tokio::time::timeout(Duration::from_secs(15), start)
        .await
        .expect("the task starts once the setup ended")
        .unwrap()
        .unwrap();
    assert_eq!(started.session.cwd.as_deref(), Some(wt.as_path()));
    env.stop().await;

    // With the setting off, it starts at once.
    let env = Env::start_with(&format!(
        "[worktrees]\nwait_for_setup = false\n{OWN_SETUP}setup = [\"cat\"]\n"
    ))
    .await;
    let task = env
        .client
        .task_create(TaskCreate {
            workspace_id: env.workspace,
            title: "Quick".into(),
            prompt: None,
            depends_on: vec![],
            placement: Some(TaskPlacement {
                kind: TaskPlacementKind::Worktree,
                branch: Some("task/quick".into()),
                base: None,
            }),
            run: Some(TaskRun::Terminal {
                argv: vec!["sh".into(), "-c".into(), "sleep 60".into()],
            }),
            auto_start: false,
            after_policy: hutan_core::AfterPolicy::Done,
        })
        .await
        .unwrap();
    let started = env.client.task_start(task.id).await.unwrap();
    let setup = env
        .setup_of(started.session.cwd.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(setup.state, WorktreeSetupState::Running);
    env.stop().await;
}

// retention -----------------------------------------------------------------------------------

fn old(path: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60);
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(when).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_lists_only_clean_merged_worktrees_and_prune_runs_teardown_first() {
    let env = Env::start_with(&format!(
        "[worktrees]\ncleanup = \"merged\"\n{OWN_SETUP}\
         teardown = [\"echo bye > \\\"$HUTAN_MAIN_CHECKOUT/teardown-$(basename \\\"$PWD\\\").log\\\"\"]\n"
    ))
    .await;
    let commit_in = |dir: &Path, file: &str| {
        fs::write(dir.join(file), "x\n").unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "-q", "-m", file]);
    };
    // Merged: its own commit, now in main.
    let merged = env.create("merged").await;
    commit_in(&merged.path, "m.txt");
    // Unmerged: a commit main does not have.
    let unmerged = env.create("unmerged").await;
    commit_in(&unmerged.path, "u.txt");
    // Never used: an ancestor of main, but it has no commit of its own.
    let fresh = env.create("fresh").await;
    // Merged but dirty (an untracked file counts).
    let dirty = env.create("dirty").await;
    commit_in(&dirty.path, "d.txt");
    // Merged but somebody works in it.
    let busy = env.create("busy").await;
    commit_in(&busy.path, "b.txt");
    // Merged but made by the user, not by Hutan.
    let own = env.repo.wt("own");
    git(
        &env.repo.root,
        &["worktree", "add", "-q", "-b", "own", own.to_str().unwrap()],
    );
    commit_in(&own, "o.txt");
    for branch in ["merged", "dirty", "busy", "own"] {
        git(
            &env.repo.root,
            &["merge", "-q", "--no-ff", "-m", branch, branch],
        );
    }
    fs::write(dirty.path.join("scratch.txt"), "wip\n").unwrap();
    let mut shell = sh("sleep 60");
    shell.cwd = Some(busy.path.clone());
    let live = create_in(&env.client, Some(env.workspace), shell).await;

    let listed = env
        .client
        .worktree_cleanup(WorktreeCleanupRequest::default())
        .await
        .unwrap();
    assert_eq!(listed.mode, WorktreeCleanup::Merged);
    assert_eq!(listed.candidates.len(), 1, "{:?}", listed.candidates);
    let candidate = &listed.candidates[0];
    assert_eq!(candidate.path, merged.path);
    assert_eq!(candidate.branch.as_deref(), Some("merged"));
    assert_eq!(candidate.reason, CleanupReason::Merged);
    assert_eq!(candidate.teardown.len(), 1);
    // Listing removes nothing.
    assert!(merged.path.exists() && unmerged.path.exists() && fresh.path.exists());

    // An agent cannot prune.
    let agent = env.agent().await;
    let forbidden = agent.worktree_prune(WorktreePrune::default()).await;
    assert_eq!(remote_code(forbidden), ErrorCode::Forbidden);
    assert!(merged.path.exists());

    // A path that is not a candidate is reported, not removed.
    let pruned = env
        .client
        .worktree_prune(WorktreePrune {
            paths: vec![unmerged.path.clone(), merged.path.clone()],
            ..WorktreePrune::default()
        })
        .await
        .unwrap();
    assert_eq!(pruned.removed, std::slice::from_ref(&merged.path));
    assert_eq!(pruned.failed.len(), 1);
    assert_eq!(pruned.failed[0].path, unmerged.path);
    assert!(!merged.path.exists());
    assert!(unmerged.path.exists() && fresh.path.exists() && dirty.path.exists());
    assert_eq!(
        fs::read_to_string(env.repo.root.join("teardown-merged.log")).unwrap(),
        "bye\n"
    );
    // The merged branch went with `git branch -d`; the others stay.
    let branches = env.repo.branches();
    assert!(!branches.contains(&"merged".to_owned()), "{branches:?}");
    assert!(branches.contains(&"unmerged".to_owned()));
    stop(&env.client, live.id).await.unwrap();
    // Nothing is left to prune (the stopped session's worktree is dirty-free but its commit is
    // merged: it is the next candidate once nobody works in it).
    let again = env
        .client
        .worktree_cleanup(WorktreeCleanupRequest::default())
        .await
        .unwrap();
    let paths: Vec<_> = again.candidates.iter().map(|c| c.path.clone()).collect();
    assert_eq!(paths, std::slice::from_ref(&busy.path));
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn never_lists_nothing_and_idle_lists_worktrees_untouched_for_the_limit() {
    let env = Env::start().await;
    let stale = env.create("stale").await;
    let fresh = env.create("fresh").await;
    // Default policy: never.
    let none = env
        .client
        .worktree_cleanup(WorktreeCleanupRequest::default())
        .await
        .unwrap();
    assert_eq!(none.mode, WorktreeCleanup::Never);
    assert!(none.candidates.is_empty());
    // Age one worktree: its creation record and its git state files (the repository's one
    // commit is from 2020 already).
    let conn = rusqlite::Connection::open(&env.daemon.paths.database).unwrap();
    conn.execute(
        "UPDATE worktrees SET created_at_ms = created_at_ms - ?1 WHERE path = ?2",
        rusqlite::params![
            i64::try_from(10 * 24 * 60 * 60 * 1000u64).unwrap(),
            stale.path.to_string_lossy()
        ],
    )
    .unwrap();
    drop(conn);
    let git_dir = PathBuf::from(
        git(&stale.path, &["rev-parse", "--absolute-git-dir"])
            .trim()
            .to_owned(),
    );
    for name in ["HEAD", "index", "logs/HEAD"] {
        let path = git_dir.join(name);
        if path.exists() {
            old(&path, 10);
        }
    }

    let idle = env
        .client
        .worktree_cleanup(WorktreeCleanupRequest {
            mode: Some(WorktreeCleanup::Idle),
            idle_days: Some(3),
            ..WorktreeCleanupRequest::default()
        })
        .await
        .unwrap();
    assert_eq!(idle.idle_days, 3);
    assert_eq!(idle.candidates.len(), 1, "{:?}", idle.candidates);
    assert_eq!(idle.candidates[0].path, stale.path);
    assert_eq!(idle.candidates[0].reason, CleanupReason::Idle);
    assert!(idle.candidates[0].idle_days >= 9);
    // Pruning with the same policy removes it and keeps the branch (it is not merged).
    let pruned = env
        .client
        .worktree_prune(WorktreePrune {
            mode: Some(WorktreeCleanup::Idle),
            idle_days: Some(3),
            ..WorktreePrune::default()
        })
        .await
        .unwrap();
    assert_eq!(pruned.removed, std::slice::from_ref(&stale.path));
    assert!(fresh.path.exists());
    assert!(env.repo.branches().contains(&"stale".to_owned()));

    // The policy is a live setting: switch it and the default follows.
    env.client
        .daemon_setting_set("worktrees.cleanup", Some(serde_json::json!("idle")))
        .await
        .unwrap();
    env.client
        .daemon_setting_set("worktrees.cleanup_idle_days", Some(serde_json::json!(100)))
        .await
        .unwrap();
    let live = env
        .client
        .worktree_cleanup(WorktreeCleanupRequest::default())
        .await
        .unwrap();
    assert_eq!((live.mode, live.idle_days), (WorktreeCleanup::Idle, 100));
    assert!(live.candidates.is_empty());
    env.stop().await;
}
