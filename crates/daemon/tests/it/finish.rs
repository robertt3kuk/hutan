//! `worktree.finish` (protocol 0.35, capability `finish`, ADR 0020) end to end: a real daemon on
//! a temporary `HUTAN_HOME`, real client connections, a repository with a local bare `origin`
//! the tests create, and the bare repository's reflog as the witness that nothing was forced.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, create, remote_code, sh};
use hutan_client::{Client, ClientError};
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Origin, Request, WorkspaceOpen, WorkspaceOpened,
};
use hutan_protocol::worktree::{
    MergeStrategy, StepReason, StepStatus, WorktreeCreate, WorktreeFinish, WorktreeFinished,
    WorktreeInfo,
};
use tempfile::TempDir;

/// A daemon whose push runs in its own environment, not a login shell's (the tests do not
/// depend on the account's shell); the login environment has its own test below.
const DAEMON_ENV: &str = "[terminal]\nenv_inherit = \"daemon\"\n";

// Git fixtures --------------------------------------------------------------------------------

fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}

/// Runs git in `dir` without the user's configuration; returns stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = run(dir, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn identity(dir: &Path) {
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(dir, &["config", key, value]);
    }
}

/// `<tmp>/origin.git` (bare, every ref update in its reflog) and `<tmp>/repo` on `main`,
/// pushed there with `main` tracking `origin/main`.
struct Remote {
    dir: TempDir,
    bare: PathBuf,
    root: PathBuf,
}

impl Remote {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let bare = base.join("origin.git");
        git(&base, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        git(&bare, &["config", "core.logAllRefUpdates", "always"]);
        git(
            &bare,
            &[
                "config",
                "core.hooksPath",
                bare.join("hooks").to_str().unwrap(),
            ],
        );
        let root = base.join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        identity(&root);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "first"]);
        git(&root, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&root, &["push", "-q", "-u", "origin", "main"]);
        Self { dir, bare, root }
    }

    fn head(&self, dir: &Path, rev: &str) -> String {
        git(dir, &["rev-parse", rev])
    }

    fn remote_main(&self) -> String {
        self.head(&self.bare, "refs/heads/main")
    }

    /// A second clone that pushes a commit of its own: `origin/main` moves past the repository.
    fn someone_else_pushes(&self) -> String {
        let other = fs::canonicalize(self.dir.path()).unwrap().join("other");
        git(
            self.dir.path(),
            &[
                "clone",
                "-q",
                self.bare.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        identity(&other);
        fs::write(other.join("theirs.txt"), "theirs\n").unwrap();
        git(&other, &["add", "-A"]);
        git(&other, &["commit", "-q", "-m", "theirs"]);
        git(&other, &["push", "-q", "origin", "main"]);
        self.head(&other, "HEAD")
    }

    /// Every update of the bare repository's `main` was a fast-forward: the reflog, oldest last,
    /// has each entry descend from the one before it.
    fn assert_never_forced(&self) {
        let log = git(
            &self.bare,
            &["reflog", "show", "--format=%H", "refs/heads/main"],
        );
        let entries: Vec<&str> = log.lines().collect();
        assert!(!entries.is_empty(), "the bare repository keeps no reflog");
        for pair in entries.windows(2) {
            let (newer, older) = (pair[0], pair[1]);
            assert!(
                run(&self.bare, &["merge-base", "--is-ancestor", older, newer])
                    .status
                    .success(),
                "{older} -> {newer} was not a fast-forward: {entries:?}"
            );
        }
    }
}

struct Env {
    daemon: TestDaemon,
    client: Client,
    remote: Remote,
    workspace: WorkspaceId,
}

impl Env {
    async fn start(toml: &str) -> Self {
        Self::start_with(toml, |_| {}).await
    }

    async fn start_with(toml: &str, tweak: impl FnOnce(&mut hutan_daemon::DaemonConfig)) -> Self {
        let daemon = TestDaemon::start_with(toml, tweak).await;
        let remote = Remote::new();
        let client = daemon.connect().await;
        let workspace = client
            .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
                path: remote.root.clone(),
                created_by: None,
            }))
            .await
            .unwrap()
            .workspace
            .id;
        Self {
            daemon,
            client,
            remote,
            workspace,
        }
    }

    async fn worktree(&self, branch: &str) -> WorktreeInfo {
        let created = self
            .client
            .worktree_create(WorktreeCreate {
                workspace_id: self.workspace,
                repo: None,
                branch: branch.to_owned(),
                base: None,
                path: None,
            })
            .await
            .unwrap();
        self.client
            .worktree_list(Some(self.workspace))
            .await
            .unwrap()
            .into_iter()
            .find(|w| w.path == created.path)
            .unwrap()
    }

    async fn finish(
        &self,
        checkout: &Path,
        message: &str,
        push: Option<bool>,
    ) -> Result<WorktreeFinished, ClientError> {
        self.client
            .worktree_finish(WorktreeFinish {
                checkout: checkout.to_path_buf(),
                message: message.to_owned(),
                push,
                into: None,
                strategy: MergeStrategy::Merge,
            })
            .await
    }

    async fn stop(self) {
        drop(self.client);
        self.daemon.stop().await;
    }
}

fn subject(dir: &Path, rev: &str) -> String {
    git(dir, &["log", "-1", "--format=%s", rev])
}

// Main checkout --------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_main_checkout_finish_commits_everything_and_pushes_it() {
    let env = Env::start(DAEMON_ENV).await;
    assert!(env.client.supports_finish());
    let root = env.remote.root.clone();
    fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    fs::write(root.join("new.txt"), "new\n").unwrap();

    let done = env.finish(&root, "Add two", None).await.unwrap();
    assert_eq!(done.commit.status, StepStatus::Ok, "{done:?}");
    assert_eq!(done.commit.branch, "main");
    assert_eq!(done.commit.files, 2);
    assert!(done.merge.is_none(), "a main checkout has nothing to merge");
    let push = done.push.as_ref().expect("push is on by default");
    assert_eq!(push.status, StepStatus::Ok, "{push:?}");
    assert_eq!(push.upstream.as_deref(), Some("origin/main"));
    assert_eq!((push.ahead, push.behind), (Some(1), Some(0)));

    let head = env.remote.head(&root, "HEAD");
    assert_eq!(done.commit.commit.as_deref(), Some(head.as_str()));
    assert_eq!(push.commit.as_deref(), Some(head.as_str()));
    assert_eq!(env.remote.remote_main(), head, "the remote has the commit");
    assert_eq!(subject(&env.remote.bare, "main"), "Add two");
    assert_eq!(git(&root, &["status", "--porcelain"]), "");

    // Again with nothing new: nothing to commit, nothing to push.
    let again = env.finish(&root, "", None).await.unwrap();
    assert_eq!(again.commit.status, StepStatus::Skipped);
    assert_eq!(again.commit.reason, Some(StepReason::NothingToCommit));
    let push = again.push.unwrap();
    assert_eq!(push.status, StepStatus::Skipped);
    assert_eq!(push.reason, Some(StepReason::UpToDate));
    env.remote.assert_never_forced();
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_push_and_push_off_keep_the_commit_local() {
    // `push: false` in the request.
    let env = Env::start(DAEMON_ENV).await;
    let before = env.remote.remote_main();
    fs::write(env.remote.root.join("a.txt"), "local\n").unwrap();
    let done = env
        .finish(&env.remote.root, "Local only", Some(false))
        .await
        .unwrap();
    assert_eq!(done.commit.status, StepStatus::Ok);
    assert!(done.push.is_none(), "{done:?}");
    assert_eq!(env.remote.remote_main(), before);
    assert_eq!(subject(&env.remote.root, "HEAD"), "Local only");
    env.stop().await;

    // `[finish] push = false` in daemon.toml, and a request that says nothing.
    let env = Env::start(&format!("{DAEMON_ENV}[finish]\npush = false\n")).await;
    let before = env.remote.remote_main();
    fs::write(env.remote.root.join("a.txt"), "local\n").unwrap();
    let done = env.finish(&env.remote.root, "Kept", None).await.unwrap();
    assert_eq!(done.commit.status, StepStatus::Ok);
    assert!(done.push.is_none(), "{done:?}");
    assert_eq!(env.remote.remote_main(), before);
    // The request's own word wins over the setting.
    let pushed = env.finish(&env.remote.root, "", Some(true)).await.unwrap();
    assert_eq!(pushed.push.unwrap().status, StepStatus::Ok);
    assert_eq!(subject(&env.remote.bare, "main"), "Kept");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_behind_its_upstream_is_rejected_and_the_commit_stays() {
    let env = Env::start(DAEMON_ENV).await;
    let theirs = env.remote.someone_else_pushes();
    fs::write(env.remote.root.join("a.txt"), "mine\n").unwrap();

    let done = env.finish(&env.remote.root, "Mine", None).await.unwrap();
    assert_eq!(done.commit.status, StepStatus::Ok);
    let push = done.push.unwrap();
    assert_eq!(push.status, StepStatus::Failed, "{push:?}");
    assert_eq!(push.reason, Some(StepReason::Rejected));
    assert_eq!((push.ahead, push.behind), (Some(1), Some(1)));
    let why = push.message.unwrap();
    assert!(
        why.contains("origin/main has 1 commit that main does not have")
            && why.contains("never force-pushes"),
        "{why}"
    );
    // The commit is kept, the remote keeps theirs, and nothing was rebased or merged.
    assert_eq!(subject(&env.remote.root, "HEAD"), "Mine");
    assert_eq!(
        done.commit.commit.as_deref(),
        Some(env.remote.head(&env.remote.root, "HEAD").as_str())
    );
    assert_eq!(env.remote.remote_main(), theirs);
    assert_eq!(git(&env.remote.root, &["rev-list", "--count", "HEAD"]), "2");
    env.remote.assert_never_forced();
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_the_remote_refuses_is_rejected_with_its_reason() {
    let env = Env::start(DAEMON_ENV).await;
    let hooks = env.remote.bare.join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-receive");
    fs::write(&hook, "#!/bin/sh\necho 'main is protected' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let before = env.remote.remote_main();
    fs::write(env.remote.root.join("a.txt"), "blocked\n").unwrap();

    let done = env.finish(&env.remote.root, "Blocked", None).await.unwrap();
    assert_eq!(done.commit.status, StepStatus::Ok);
    let push = done.push.unwrap();
    assert_eq!(push.status, StepStatus::Failed, "{push:?}");
    assert_eq!(push.reason, Some(StepReason::Rejected));
    assert!(
        push.message
            .as_deref()
            .unwrap()
            .contains("pre-receive hook declined"),
        "{push:?}"
    );
    assert_eq!(env.remote.remote_main(), before);
    assert_eq!(subject(&env.remote.root, "HEAD"), "Blocked");
    env.remote.assert_never_forced();
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_branch_without_upstream_is_reported_and_the_commit_kept() {
    let env = Env::start(DAEMON_ENV).await;
    git(&env.remote.root, &["switch", "-q", "-c", "topic"]);
    fs::write(env.remote.root.join("a.txt"), "topic\n").unwrap();

    let done = env.finish(&env.remote.root, "Topic", None).await.unwrap();
    assert_eq!(done.commit.status, StepStatus::Ok);
    assert_eq!(done.commit.branch, "topic");
    let push = done.push.unwrap();
    assert_eq!(push.status, StepStatus::Failed);
    assert_eq!(push.reason, Some(StepReason::NoUpstream));
    assert!(push.message.unwrap().contains("git push -u"));
    assert_eq!(subject(&env.remote.root, "topic"), "Topic");
    assert!(
        !run(
            &env.remote.bare,
            &["rev-parse", "--verify", "refs/heads/topic"]
        )
        .status
        .success()
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repository_without_a_remote_finishes_commit_only() {
    let env = Env::start(DAEMON_ENV).await;
    git(&env.remote.root, &["remote", "remove", "origin"]);
    fs::write(env.remote.root.join("a.txt"), "local\n").unwrap();

    let done = env
        .finish(&env.remote.root, "Local work", Some(true))
        .await
        .unwrap();
    assert_eq!(done.commit.status, StepStatus::Ok);
    assert!(!done.failed(), "no remote is no failure");
    let push = done.push.unwrap();
    assert_eq!(push.status, StepStatus::Skipped, "{push:?}");
    assert_eq!(push.reason, Some(StepReason::NoUpstream));
    assert!(push.message.unwrap().contains("commit only"));
    assert_eq!(subject(&env.remote.root, "main"), "Local work");
    env.stop().await;
}

// Worktree --------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_finish_commits_merges_and_pushes_the_target() {
    let env = Env::start(DAEMON_ENV).await;
    let wt = env.worktree("feature/login").await;
    fs::write(wt.path.join("login.txt"), "login\n").unwrap();

    let done = env
        .client
        .worktree_finish(WorktreeFinish {
            checkout: wt.path.clone(),
            message: "Add login".into(),
            push: None,
            into: None,
            strategy: MergeStrategy::Squash,
        })
        .await
        .unwrap();
    assert_eq!(done.commit.status, StepStatus::Ok, "{done:?}");
    assert_eq!(done.commit.branch, "feature/login");
    let merge = done.merge.as_ref().unwrap();
    assert_eq!(merge.status, StepStatus::Ok, "{merge:?}");
    assert_eq!(merge.target, "main");
    assert_eq!(merge.strategy, MergeStrategy::Squash);
    let push = done.push.as_ref().unwrap();
    assert_eq!(push.status, StepStatus::Ok, "{push:?}");
    assert_eq!(
        push.branch, "main",
        "the target is pushed, not the worktree's branch"
    );

    let main_head = env.remote.head(&env.remote.root, "main");
    assert_eq!(merge.commit.as_deref(), Some(main_head.as_str()));
    assert_eq!(env.remote.remote_main(), main_head);
    // A squash takes the Finish's message.
    assert_eq!(subject(&env.remote.bare, "main"), "Add login");
    assert_eq!(git(&env.remote.bare, &["show", "main:login.txt"]), "login");
    assert!(
        !run(
            &env.remote.bare,
            &["rev-parse", "--verify", "refs/heads/feature/login"]
        )
        .status
        .success(),
        "only the target is pushed"
    );
    assert_eq!(git(&wt.path, &["status", "--porcelain"]), "");
    env.remote.assert_never_forced();
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_finish_that_cannot_merge_changes_nothing() {
    let env = Env::start(DAEMON_ENV).await;
    let wt = env.worktree("feature/x").await;
    fs::write(wt.path.join("x.txt"), "x\n").unwrap();
    // The main checkout is dirty: the merge would be refused, so nothing is committed.
    fs::write(env.remote.root.join("scratch.txt"), "mine\n").unwrap();
    let before = env.remote.head(&wt.path, "HEAD");
    let refused = env.finish(&wt.path, "X", None).await;
    assert_eq!(remote_code(refused), ErrorCode::InvalidState);
    assert_eq!(env.remote.head(&wt.path, "HEAD"), before);
    assert!(git(&wt.path, &["status", "--porcelain"]).contains("x.txt"));

    // An empty message with something to commit is a bad request.
    fs::remove_file(env.remote.root.join("scratch.txt")).unwrap();
    let refused = env.finish(&wt.path, "  ", None).await;
    assert_eq!(remote_code(refused), ErrorCode::BadRequest);
    assert_eq!(env.remote.head(&wt.path, "HEAD"), before);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conflicting_worktree_finish_keeps_its_commit_and_pushes_nothing() {
    let env = Env::start(DAEMON_ENV).await;
    let wt = env.worktree("feature/clash").await;
    fs::write(env.remote.root.join("a.txt"), "main side\n").unwrap();
    git(&env.remote.root, &["commit", "-q", "-am", "main side"]);
    let before = env.remote.remote_main();
    fs::write(wt.path.join("a.txt"), "worktree side\n").unwrap();

    let done = env.finish(&wt.path, "Clash", None).await.unwrap();
    assert_eq!(done.commit.status, StepStatus::Ok);
    let merge = done.merge.unwrap();
    assert_eq!(merge.status, StepStatus::Failed);
    assert_eq!(merge.reason, Some(StepReason::Conflict));
    assert_eq!(merge.conflicts, vec!["a.txt".to_owned()]);
    let push = done.push.unwrap();
    assert_eq!(push.status, StepStatus::Skipped);
    assert_eq!(push.reason, Some(StepReason::EarlierStepFailed));
    assert_eq!(subject(&wt.path, "HEAD"), "Clash");
    assert_eq!(git(&env.remote.root, &["status", "--porcelain"]), "");
    assert_eq!(env.remote.remote_main(), before);
    env.stop().await;
}

// Who may finish ------------------------------------------------------------------------------

async fn report_agent(t: &TestDaemon, session: SessionId) {
    let client = t.connect().await;
    let applied: AgentApplied = client
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
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_may_never_finish_whatever_the_control_policy() {
    for policy in ["ask", "allow", "read_only"] {
        let env = Env::start(&format!(
            "{DAEMON_ENV}[control]\nagents = \"{policy}\"\napproval_timeout_secs = 20\n"
        ))
        .await;
        let agent_session = create(&env.client, sh("sleep 60")).await;
        report_agent(&env.daemon, agent_session.id).await;
        let mut options = env.daemon.options();
        options.client.origin = Some(Origin {
            session_id: agent_session.id,
        });
        let agent = Client::connect(options).await.unwrap();
        let before = env.remote.head(&env.remote.root, "HEAD");
        fs::write(env.remote.root.join("a.txt"), "agent\n").unwrap();

        let refused = agent
            .worktree_finish(WorktreeFinish {
                checkout: env.remote.root.clone(),
                message: "Agent".into(),
                push: Some(true),
                into: None,
                strategy: MergeStrategy::Merge,
            })
            .await;
        match &refused {
            Err(ClientError::Remote(error)) => {
                assert_eq!(error.code, ErrorCode::Forbidden, "{policy}");
                assert_eq!(
                    error
                        .data
                        .as_ref()
                        .and_then(|d| d.pointer("/control/decision")),
                    Some(&serde_json::json!("user_only")),
                    "{policy}"
                );
            }
            other => panic!("{policy}: expected forbidden, got {other:?}"),
        }
        // No approval was raised and nothing ran.
        let pending = env
            .client
            .call::<hutan_protocol::chat::ApprovalList>(Request::ApprovalList(
                hutan_protocol::chat::ApprovalListRequest::default(),
            ))
            .await
            .unwrap();
        assert!(pending.approvals.is_empty(), "{policy}");
        assert_eq!(
            env.remote.head(&env.remote.root, "HEAD"),
            before,
            "{policy}"
        );
        assert_eq!(env.remote.remote_main(), before, "{policy}");
        drop(agent);
        env.stop().await;
    }
}

// The login environment --------------------------------------------------------------------------

/// git's credentials and URL rewrites come from the user's login environment: the origin's URL
/// only resolves through a `url.insteadOf` in a global git config that the stand-in login shell
/// names, so the push goes through only when it ran in that environment.
#[tokio::test(flavor = "multi_thread")]
async fn the_push_runs_in_the_users_login_environment() {
    let scratch = tempfile::tempdir().unwrap();
    let scratch_dir = fs::canonicalize(scratch.path()).unwrap();
    let alias = "/nonexistent/hutan-finish-origin.git";
    let gitconfig = scratch_dir.join("gitconfig");
    let shell = scratch_dir.join("login-shell");
    let path = std::env::var("PATH").unwrap();
    fs::write(
        &shell,
        format!(
            "#!/bin/sh\nexport PATH='{path}' GIT_CONFIG_GLOBAL='{}'\nexec /usr/bin/env -0\n",
            gitconfig.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).unwrap();
    // macOS vets the first run of a new executable; do it outside the daemon's timeouts.
    let _ = Command::new(&shell).output();

    let env = Env::start_with("[terminal]\nenv_inherit = \"login\"\n", |config| {
        config.login_shell = Some(shell.clone());
    })
    .await;
    fs::write(
        &gitconfig,
        format!(
            "[url \"{}\"]\n\tinsteadOf = {alias}\n",
            env.remote.bare.display()
        ),
    )
    .unwrap();
    git(&env.remote.root, &["remote", "set-url", "origin", alias]);
    fs::write(env.remote.root.join("a.txt"), "login env\n").unwrap();

    let mut done = env
        .finish(&env.remote.root, "Via login", None)
        .await
        .unwrap();
    assert_eq!(done.commit.status, StepStatus::Ok);
    // The login shell is captured in the background and a first use waits for it only so
    // long; Finish again (nothing left to commit) until the capture is there.
    for _ in 0..20 {
        if done.push.as_ref().unwrap().status == StepStatus::Ok {
            break;
        }
        done = env.finish(&env.remote.root, "", None).await.unwrap();
    }
    let push = done.push.unwrap();
    assert_eq!(push.status, StepStatus::Ok, "{push:?}");
    assert_eq!(subject(&env.remote.bare, "main"), "Via login");
    env.stop().await;
}
