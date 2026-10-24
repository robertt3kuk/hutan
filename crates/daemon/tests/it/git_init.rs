//! `project.git_init` (protocol 0.44, capability `project.git_init`) end to end: a real daemon
//! on a temporary `HUTAN_HOME` whose Home folder the tests fill, real client connections, and
//! the repository git itself reports afterwards. The daemon's git runs with a configuration
//! file the test writes (`Tuning::git_env`), never the account's.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, create, remote_code, sh, wait_event};
use hutan_client::Client;
use hutan_core::{SessionId, WorkspaceId};
use hutan_daemon::DaemonConfig;
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Empty, Event, Origin, Request, WorkspaceCreate,
    WorkspaceKind, WorkspaceList,
};
use tempfile::TempDir;

/// Runs git in `dir` with an empty configuration; returns stdout.
fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A global git configuration file: with an identity, or one that insists on a configured
/// identity (`useConfigOnly`) and has none.
fn git_config(dir: &Path, identity: bool) -> PathBuf {
    let path = dir.join(if identity {
        "gitconfig-with-identity"
    } else {
        "gitconfig-without-identity"
    });
    let text = if identity {
        "[user]\n\tname = Test\n\temail = test@example.com\n[commit]\n\tgpgsign = false\n"
    } else {
        "[user]\n\tuseConfigOnly = true\n[commit]\n\tgpgsign = false\n"
    };
    fs::write(&path, text).unwrap();
    path
}

/// The daemon's git environment: the configuration file, no system configuration.
fn tune(config: &mut DaemonConfig, git_config: PathBuf) {
    config.checkpoints.git_env.extend([
        (
            "GIT_CONFIG_GLOBAL".to_owned(),
            git_config.display().to_string(),
        ),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
    ]);
}

struct Env {
    daemon: TestDaemon,
    client: Client,
    /// The Home workspace and its folder (canonical).
    home: WorkspaceId,
    folder: PathBuf,
    _config: TempDir,
}

impl Env {
    async fn start(toml: &str, identity: bool) -> Self {
        Self::start_with(toml, identity, |_| {}).await
    }

    async fn start_with(toml: &str, identity: bool, tweak: impl FnOnce(&mut DaemonConfig)) -> Self {
        let config_dir = tempfile::tempdir().unwrap();
        let file = git_config(config_dir.path(), identity);
        let daemon = TestDaemon::start_with(toml, |config| {
            tune(config, file);
            tweak(config);
        })
        .await;
        let client = daemon.connect().await;
        let home = home_of(&client).await;
        // A test that moves the Home folder (`tweak`) has its own path to it.
        let folder = fs::canonicalize(&daemon.paths.default_home_folder).unwrap_or_default();
        Self {
            daemon,
            client,
            home,
            folder,
            _config: config_dir,
        }
    }

    async fn stop(self) {
        drop(self.client);
        self.daemon.stop().await;
    }
}

async fn home_of(client: &Client) -> WorkspaceId {
    client
        .call::<WorkspaceList>(Request::WorkspaceList(Empty {}))
        .await
        .unwrap()
        .workspaces
        .into_iter()
        .find(|w| w.kind == WorkspaceKind::Home)
        .unwrap()
        .id
}

fn fill(folder: &Path, files: &[(&str, &str)]) {
    for (name, text) in files {
        let path = folder.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_home_folder_becomes_a_tracked_repository_with_one_commit() {
    let env = Env::start("", true).await;
    fill(
        &env.folder,
        &[
            ("notes.md", "# notes\n"),
            ("projects/plan.txt", "plan\n"),
            (".hutan-write-01.tmp", "scratch\n"),
        ],
    );
    let (client, mut events) = {
        let c = env.daemon.connect_subscribed().await;
        (c.0, c.1)
    };

    let done = client.project_git_init(env.home).await.unwrap();
    assert_eq!(done.root, env.folder);
    assert!(!done.branch.is_empty());
    assert_eq!(done.commit, git(&env.folder, &["rev-parse", "HEAD"]));
    assert_eq!(done.files, 3, "notes.md, projects/plan.txt and .gitignore");
    assert!(done.gitignore_written);

    // What git itself says: one commit, a clean tree, the scratch file left out.
    assert_eq!(git(&env.folder, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(
        git(&env.folder, &["log", "-1", "--format=%s"]),
        "Start tracking changes with Hutan"
    );
    assert_eq!(
        git(&env.folder, &["log", "-1", "--format=%an <%ae>"]),
        "Test <test@example.com>",
        "the user's identity, not one of Hutan's"
    );
    assert_eq!(git(&env.folder, &["status", "--porcelain"]), "");
    let tracked = git(&env.folder, &["ls-files"]);
    assert!(!tracked.contains(".hutan-write"), "{tracked}");
    let ignore = fs::read_to_string(env.folder.join(".gitignore")).unwrap();
    assert!(ignore.contains(".hutan-write-*.tmp"), "{ignore}");

    // Home has a checkout now, and the workspaces hear of it.
    wait_event(
        &mut events,
        "worktree.changed of Home",
        |event| match event {
            Event::WorktreeChanged { workspace_id } if *workspace_id == env.home => Some(()),
            _ => None,
        },
    )
    .await;
    let listed = client.worktree_list(Some(env.home)).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].main);
    assert_eq!(listed[0].path, env.folder);
    assert_eq!(listed[0].branch.as_deref(), Some(done.branch.as_str()));
    assert!(!listed[0].dirty);

    // Home stays Home: a session in a subfolder is not a project of its own.
    let session = create(
        &client,
        hutan_protocol::messages::TerminalSpec {
            cwd: Some(env.folder.join("projects")),
            ..sh("sleep 60")
        },
    )
    .await;
    assert_eq!(session.workspace_id, env.home);

    // A second call finds a repository and refuses.
    let again = client.project_git_init(env.home).await;
    assert_eq!(remote_code(again), ErrorCode::InvalidState);
    assert_eq!(git(&env.folder, &["rev-list", "--count", "HEAD"]), "1");
    drop(client);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_gitignore_keeps_its_lines_and_a_nested_repository_stays_out() {
    let env = Env::start("", true).await;
    fill(
        &env.folder,
        &[
            (".gitignore", "secret.txt"),
            ("secret.txt", "s\n"),
            ("a.txt", "a\n"),
        ],
    );
    let inner = env.folder.join("clone");
    fs::create_dir_all(&inner).unwrap();
    git(&inner, &["init", "-q", "-b", "main"]);
    fill(&inner, &[("inner.txt", "inner\n")]);

    let done = env.client.project_git_init(env.home).await.unwrap();
    assert!(done.gitignore_written);
    let ignore = fs::read_to_string(env.folder.join(".gitignore")).unwrap();
    assert!(ignore.starts_with("secret.txt\n"), "{ignore}");
    assert!(ignore.contains(".DS_Store"), "{ignore}");
    let tracked = git(&env.folder, &["ls-files"]);
    assert_eq!(tracked.lines().collect::<Vec<_>>(), [".gitignore", "a.txt"]);
    assert_eq!(git(&env.folder, &["status", "--porcelain"]), "");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_home_directory_a_workspace_and_an_unknown_id_are_refused() {
    // The Home folder *is* the user's home directory here.
    let env = Env::start_with("", true, |config| {
        config.paths.user_home = Some(config.paths.default_home_folder.clone());
    })
    .await;
    fill(&env.folder, &[("a.txt", "a\n")]);
    let refused = env.client.project_git_init(env.home).await;
    assert_eq!(remote_code(refused), ErrorCode::InvalidState);
    assert!(!env.folder.join(".git").exists());
    assert!(
        !env.folder.join(".gitignore").exists(),
        "nothing was written"
    );

    let workspace = env
        .client
        .workspace_create(WorkspaceCreate {
            name: "lab".into(),
            paths: vec![],
            project_ids: vec![],
            created_by: None,
        })
        .await
        .unwrap();
    let refused = env.client.project_git_init(workspace.id).await;
    assert_eq!(
        remote_code(refused),
        ErrorCode::InvalidState,
        "no folder of its own"
    );

    let unknown = env.client.project_git_init(WorkspaceId::new()).await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_inside_another_repository_is_refused() {
    let outer_dir = tempfile::tempdir().unwrap();
    let outer = fs::canonicalize(outer_dir.path()).unwrap();
    git(&outer, &["init", "-q", "-b", "main"]);
    let home_folder = outer.join("Hutan");
    fs::create_dir(&home_folder).unwrap();
    let env = Env::start_with("", true, {
        let home_folder = home_folder.clone();
        move |config| config.paths.default_home_folder = home_folder
    })
    .await;
    fill(&home_folder, &[("a.txt", "a\n")]);
    let refused = env.client.project_git_init(env.home).await;
    assert_eq!(remote_code(refused), ErrorCode::InvalidState);
    assert!(!home_folder.join(".git").exists());
    assert!(!home_folder.join(".gitignore").exists());
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_over_the_checkpoint_limit_is_refused_and_left_as_it_was() {
    let env = Env::start("[checkpoints]\nmax_untracked_files = 3\n", true).await;
    fill(
        &env.folder,
        &[("1", "1"), ("2", "2"), ("3", "3"), ("4", "4"), ("5", "5")],
    );
    let refused = env.client.project_git_init(env.home).await;
    assert_eq!(remote_code(refused), ErrorCode::InvalidState);
    assert!(
        !env.folder.join(".git").exists(),
        "the new .git was removed"
    );
    assert!(
        !env.folder.join(".gitignore").exists(),
        "so was the .gitignore"
    );
    assert_eq!(fs::read_dir(&env.folder).unwrap().count(), 5);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_git_identity_nothing_is_left_behind() {
    let env = Env::start("", false).await;
    fill(&env.folder, &[(".gitignore", "x\n"), ("a.txt", "a\n")]);
    let refused = env.client.project_git_init(env.home).await.unwrap_err();
    let hutan_client::ClientError::Remote(error) = refused else {
        panic!("expected a remote error, got {refused:?}");
    };
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(error.message.contains("git config"), "{}", error.message);
    assert!(!env.folder.join(".git").exists());
    assert_eq!(
        fs::read_to_string(env.folder.join(".gitignore")).unwrap(),
        "x\n",
        "the user's .gitignore is as it was"
    );
    env.stop().await;
}

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
async fn an_agent_may_never_put_a_folder_under_version_control() {
    for policy in ["ask", "allow", "read_only"] {
        let env = Env::start(&format!("[control]\nagents = \"{policy}\"\n"), true).await;
        fill(&env.folder, &[("a.txt", "a\n")]);
        let agent_session = create(&env.client, sh("sleep 60")).await;
        report_agent(&env.daemon, agent_session.id).await;
        let mut options = env.daemon.options();
        options.client.origin = Some(Origin {
            session_id: agent_session.id,
        });
        let agent = Client::connect(options).await.unwrap();
        let refused = agent.project_git_init(env.home).await;
        assert_eq!(remote_code(refused), ErrorCode::Forbidden, "{policy}");
        assert!(!env.folder.join(".git").exists(), "{policy}");
        assert!(!env.folder.join(".gitignore").exists(), "{policy}");
        drop(agent);
        env.stop().await;
    }
}
