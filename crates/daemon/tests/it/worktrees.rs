//! Worktrees and session placement (protocol 0.14) end to end: a real daemon on a temporary
//! `HUTAN_HOME`, real client connections, real PTY sessions, and git repositories created in
//! temporary directories by the tests themselves (never this repository, never the user's).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, create_in, sh, stop, wait_event};
use hutan_client::{Client, ClientError};
use hutan_core::{SessionId, SessionState, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{
    AgentApplied, AgentReleaseRequest, AgentReportRequest, AgentState, Event, Request, SessionInfo,
    WorkspaceOpen, WorkspaceOpened,
};
use hutan_protocol::worktree::{
    PlacementKind, WorktreeCreate, WorktreeInfo, WorktreeRemove, WorktreeRemoved,
};
use tempfile::TempDir;
use tokio::sync::broadcast;

// Git fixtures --------------------------------------------------------------------------------

/// Runs git in `dir` without the user's configuration; returns stdout.
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

/// A repository `<tmp>/repo` on branch `main` with one commit; the temp dir also receives the
/// default worktree directory `<tmp>/repo-wt`.
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

struct Env {
    daemon: TestDaemon,
    client: Client,
    events: broadcast::Receiver<(hutan_core::Seq, Event)>,
    repo: Repo,
    workspace: WorkspaceId,
}

impl Env {
    async fn start() -> Self {
        let daemon = TestDaemon::start().await;
        let repo = Repo::new();
        let (client, events) = daemon.connect_subscribed().await;
        let workspace = open(&client, &repo.root).await;
        Self {
            daemon,
            client,
            events,
            repo,
            workspace,
        }
    }

    fn create_request(&self, branch: &str) -> WorktreeCreate {
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
            .worktree_create(self.create_request(branch))
            .await
            .unwrap()
    }

    async fn list(&self) -> Vec<WorktreeInfo> {
        self.client
            .worktree_list(Some(self.workspace))
            .await
            .unwrap()
    }

    async fn remove(
        &self,
        path: &Path,
        force: bool,
        delete_branch: bool,
    ) -> Result<WorktreeRemoved, ClientError> {
        self.client
            .worktree_remove(WorktreeRemove {
                path: path.to_owned(),
                force,
                delete_branch,
            })
            .await
    }

    async fn session_in(&self, dir: &Path) -> SessionInfo {
        let mut s = sh("sleep 120");
        s.cwd = Some(dir.to_owned());
        create_in(&self.client, Some(self.workspace), s).await
    }

    async fn report(&self, session: SessionId, source: &str, seq: u64) {
        let applied: AgentApplied = self
            .client
            .call(Request::AgentReport(AgentReportRequest {
                pid: None,
                session_id: session,
                source: source.into(),
                agent: "fake".into(),
                state: Some(AgentState::Working),
                message: None,
                seq,
                agent_session_id: None,
                resume_argv: None,
                node: None,
            }))
            .await
            .unwrap();
        assert!(applied.applied);
    }

    async fn changed(&mut self) {
        let workspace = self.workspace;
        wait_event(&mut self.events, "worktree.changed", |event| match event {
            Event::WorktreeChanged { workspace_id } if *workspace_id == workspace => Some(()),
            _ => None,
        })
        .await;
    }

    /// The next `session.updated` of `id` whose placement satisfies `pick`.
    async fn placement_update(
        &mut self,
        id: SessionId,
        what: &str,
        mut pick: impl FnMut(&SessionInfo) -> bool,
    ) -> SessionInfo {
        wait_event(&mut self.events, what, |event| match event {
            Event::SessionUpdated { session } if session.id == id && pick(session) => {
                Some(session.clone())
            }
            _ => None,
        })
        .await
    }

    async fn stop(self) {
        self.daemon.stop().await;
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

// create / list / remove ----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn create_list_and_remove_a_worktree_at_the_default_path() {
    let mut env = Env::start().await;

    let created = env.create("feature/login").await;
    let expected = env.repo.wt("feature-login");
    assert_eq!(created.path, expected);
    assert_eq!(created.repo_root, env.repo.root);
    assert_eq!(created.branch.as_deref(), Some("feature/login"));
    assert!(!created.main);
    assert!(created.created_by_hutan);
    assert!(!created.dirty && !created.locked && !created.prunable);
    assert!(created.live_sessions.is_empty());
    assert_eq!(created.head.len(), 40);
    assert!(expected.join("a.txt").is_file());
    env.changed().await;

    let listed = env.list().await;
    assert_eq!(listed.len(), 2);
    assert!(listed[0].main, "the main worktree comes first");
    assert_eq!(listed[0].path, env.repo.root);
    assert_eq!(listed[0].branch.as_deref(), Some("main"));
    assert!(!listed[0].created_by_hutan);
    assert_eq!(listed[1], created);

    let removed = env.remove(&expected, false, false).await.unwrap();
    assert_eq!(removed, WorktreeRemoved::default());
    env.changed().await;
    assert!(!expected.exists());
    assert_eq!(env.list().await.len(), 1);
    assert!(
        env.repo.branches().contains(&"feature/login".to_owned()),
        "the branch stays unless delete_branch is set"
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_branch_is_checked_out_and_base_starts_a_new_one() {
    let env = Env::start().await;
    git(&env.repo.root, &["branch", "old", "HEAD"]);
    fs::write(env.repo.root.join("b.txt"), "two\n").unwrap();
    git(&env.repo.root, &["add", "b.txt"]);
    git(&env.repo.root, &["commit", "-q", "-m", "second"]);

    let existing = env.create("old").await;
    assert!(
        !existing.path.join("b.txt").exists(),
        "`old` is at the first commit"
    );

    let mut req = env.create_request("from-base");
    req.base = Some("old".into());
    let based = env.client.worktree_create(req).await.unwrap();
    assert_eq!(based.head, existing.head, "the new branch started at `old`");

    let mut req = env.create_request("explicit");
    req.path = Some(env.repo.root.parent().unwrap().join("elsewhere"));
    let placed = env.client.worktree_create(req).await.unwrap();
    assert_eq!(
        placed.path,
        env.repo.root.parent().unwrap().join("elsewhere")
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn create_refuses_what_cannot_work() {
    let env = Env::start().await;
    let first = env.create("one").await;

    // The path exists.
    let mut req = env.create_request("two");
    req.path = Some(first.path.clone());
    let (code, text) = message(env.client.worktree_create(req).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert!(text.contains("already exists"), "{text}");

    // The branch is checked out elsewhere: in another worktree, and in the main one.
    let (code, text) = message(env.client.worktree_create(env.create_request("one")).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert!(
        text.contains(first.path.to_str().unwrap()),
        "names where: {text}"
    );
    let (code, text) = message(env.client.worktree_create(env.create_request("main")).await);
    assert_eq!(code, ErrorCode::Conflict);
    assert!(text.contains(env.repo.root.to_str().unwrap()), "{text}");

    // Names git would not take, and refs that are no commit.
    for bad in [
        "",
        "-x",
        "a..b",
        "has space",
        "x.lock",
        "@{-1}",
        "HEAD",
        "back\\slash",
    ] {
        let (code, _) = message(env.client.worktree_create(env.create_request(bad)).await);
        assert_eq!(code, ErrorCode::BadRequest, "branch `{bad}`");
    }
    let mut req = env.create_request("fine");
    req.base = Some("no-such-ref".into());
    assert_eq!(
        message(env.client.worktree_create(req).await).0,
        ErrorCode::BadRequest
    );
    let mut req = env.create_request("fine");
    req.base = Some("--orphan".into());
    assert_eq!(
        message(env.client.worktree_create(req).await).0,
        ErrorCode::BadRequest
    );
    let mut req = env.create_request("fine");
    req.path = Some(PathBuf::from("relative/dir"));
    assert_eq!(
        message(env.client.worktree_create(req).await).0,
        ErrorCode::BadRequest
    );
    assert!(
        !env.repo.wt("fine").exists(),
        "nothing was created for a refused request"
    );

    // Unknown workspace; a workspace without a git repository.
    let mut req = env.create_request("fine");
    req.workspace_id = WorkspaceId::new();
    assert_eq!(
        message(env.client.worktree_create(req).await).0,
        ErrorCode::NotFound
    );
    let plain = tempfile::tempdir().unwrap();
    let plain_ws = open(&env.client, &fs::canonicalize(plain.path()).unwrap()).await;
    let mut req = env.create_request("fine");
    req.workspace_id = plain_ws;
    assert_eq!(
        message(env.client.worktree_create(req.clone()).await).0,
        ErrorCode::BadRequest
    );
    // ... unless a repository is named.
    req.repo = Some(env.repo.root.join("."));
    assert_eq!(
        env.client.worktree_create(req).await.unwrap().path,
        env.repo.wt("fine")
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_refuses_the_main_worktree_live_sessions_and_unsaved_work() {
    let env = Env::start().await;
    let wt = env.create("work").await;

    // The main worktree.
    let (code, text) = message(env.remove(&env.repo.root, true, false).await);
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(text.contains("main worktree"), "{text}");

    // A live session in it, even with force.
    let session = env.session_in(&wt.path).await;
    let listed = env.list().await;
    assert_eq!(listed[1].live_sessions, vec![session.id]);
    for force in [false, true] {
        let (code, text) = message(env.remove(&wt.path, force, false).await);
        assert_eq!(code, ErrorCode::InvalidState);
        assert!(text.contains("live session"), "{text}");
        assert!(text.contains(&session.id.to_string()), "{text}");
    }
    stop(&env.client, session.id).await.unwrap();

    // Uncommitted changes, tracked and untracked.
    fs::write(wt.path.join("a.txt"), "changed\n").unwrap();
    assert!(env.list().await[1].dirty);
    let (code, text) = message(env.remove(&wt.path, false, false).await);
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(text.contains("uncommitted"), "{text}");
    assert!(wt.path.exists());
    git(&wt.path, &["checkout", "--", "a.txt"]);
    fs::write(wt.path.join("new.txt"), "untracked\n").unwrap();
    assert!(env.list().await[1].dirty, "untracked files count");
    env.remove(&wt.path, true, false).await.unwrap();
    assert!(!wt.path.exists());

    // Locked: git's own lock is respected.
    let locked = env.create("locked").await;
    git(
        &env.repo.root,
        &["worktree", "lock", locked.path.to_str().unwrap()],
    );
    assert!(env.list().await[1].locked);
    let (code, text) = message(env.remove(&locked.path, true, false).await);
    assert_eq!(code, ErrorCode::InvalidState);
    assert!(text.contains("locked"), "{text}");

    // A path git does not list.
    let (code, _) = message(env.remove(&env.repo.wt("nothing"), false, false).await);
    assert_eq!(code, ErrorCode::NotFound);
    let (code, _) = message(env.remove(Path::new("relative"), false, false).await);
    assert_eq!(code, ErrorCode::BadRequest);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_branch_removes_a_merged_branch_and_reports_an_unmerged_one() {
    let env = Env::start().await;

    // Merged: nothing was committed on it.
    let merged = env.create("merged").await;
    let removed = env.remove(&merged.path, false, true).await.unwrap();
    assert_eq!(removed.branch_kept, None);
    assert!(!env.repo.branches().contains(&"merged".to_owned()));

    // Unmerged: the worktree goes, the branch and its commit stay, and the answer says why.
    let unmerged = env.create("unmerged").await;
    fs::write(unmerged.path.join("c.txt"), "work\n").unwrap();
    git(&unmerged.path, &["add", "c.txt"]);
    git(&unmerged.path, &["commit", "-q", "-m", "work"]);
    let removed = env.remove(&unmerged.path, false, true).await.unwrap();
    assert!(!unmerged.path.exists());
    let reason = removed
        .branch_kept
        .expect("the unmerged branch is reported");
    assert!(reason.contains("not fully merged"), "{reason}");
    assert!(env.repo.branches().contains(&"unmerged".to_owned()));
    assert_eq!(env.list().await.len(), 1);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_whose_directory_vanished_is_prunable_and_can_be_removed() {
    let env = Env::start().await;
    let wt = env.create("gone").await;
    fs::remove_dir_all(&wt.path).unwrap();

    let listed = env.list().await;
    assert_eq!(listed.len(), 2);
    assert!(listed[1].prunable);
    assert!(!listed[1].dirty);
    env.remove(&wt.path, false, false).await.unwrap();
    assert_eq!(env.list().await.len(), 1);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn listing_is_per_workspace_and_a_linked_root_shows_the_same_repository_once() {
    let env = Env::start().await;
    let wt = env.create("linked").await;

    // Another repository in its own workspace.
    let other = Repo::new();
    let other_ws = open(&env.client, &other.root).await;

    // A second workspace whose root is the worktree itself: same repository.
    let linked_ws = open(&env.client, &wt.path).await;

    let mine = env.client.worktree_list(Some(env.workspace)).await.unwrap();
    assert_eq!(mine.len(), 2);
    let theirs = env.client.worktree_list(Some(other_ws)).await.unwrap();
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].path, other.root);
    let through_link = env.client.worktree_list(Some(linked_ws)).await.unwrap();
    assert_eq!(
        through_link, mine,
        "the repository is found through its linked worktree"
    );
    let all = env.client.worktree_list(None).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(
        message(env.client.worktree_list(Some(WorkspaceId::new())).await).0,
        ErrorCode::NotFound
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn created_by_hutan_survives_a_daemon_restart() {
    let env = Env::start().await;
    let wt = env.create("durable").await;
    // One made outside Hutan is not Hutan's.
    git(
        &env.repo.root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "mine",
            env.repo.wt("mine").to_str().unwrap(),
        ],
    );
    let Env {
        daemon,
        repo,
        workspace,
        client,
        events,
    } = env;
    drop((client, events));
    let daemon = daemon.restart().await;
    let client = daemon.connect().await;

    let listed = client.worktree_list(Some(workspace)).await.unwrap();
    assert_eq!(listed.len(), 3);
    let by_path = |path: &Path| listed.iter().find(|w| w.path == path).unwrap();
    assert!(by_path(&wt.path).created_by_hutan);
    assert!(!by_path(&repo.wt("mine")).created_by_hutan);
    assert!(!by_path(&repo.root).created_by_hutan);

    // Removal forgets it: a new worktree at the same path is Hutan's again, nothing stale.
    client
        .worktree_remove(WorktreeRemove {
            path: wt.path.clone(),
            force: false,
            delete_branch: false,
        })
        .await
        .unwrap();
    let listed = client.worktree_list(Some(workspace)).await.unwrap();
    assert!(listed.iter().all(|w| w.path != wt.path));
    daemon.stop().await;
}

// Placement -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sessions_in_the_main_checkout_and_in_a_worktree_say_where_they_are() {
    let env = Env::start().await;
    let wt = env.create("topic").await;
    let plain = tempfile::tempdir().unwrap();

    let in_main = env.session_in(&env.repo.root).await;
    let placement = in_main
        .placement
        .expect("a session in the repository has a placement");
    assert_eq!(placement.kind, PlacementKind::Main);
    assert_eq!(placement.root, env.repo.root);
    assert_eq!(placement.branch.as_deref(), Some("main"));
    assert!(placement.shared_with.is_empty());

    // A subdirectory belongs to the checkout that contains it.
    fs::create_dir_all(wt.path.join("src/deep")).unwrap();
    let in_wt = env.session_in(&wt.path.join("src/deep")).await;
    let placement = in_wt.placement.unwrap();
    assert_eq!(placement.kind, PlacementKind::Worktree);
    assert_eq!(placement.root, wt.path);
    assert_eq!(placement.branch.as_deref(), Some("topic"));

    // Outside git there is none.
    let outside = env
        .session_in(&fs::canonicalize(plain.path()).unwrap())
        .await;
    assert_eq!(outside.placement, None);

    // The list shows the same, and the worktree knows its sessions.
    let listed = env.list().await;
    assert_eq!(listed[0].live_sessions, vec![in_main.id]);
    assert_eq!(listed[1].live_sessions, vec![in_wt.id]);
    let snapshot: hutan_protocol::messages::SessionSnapshot = env
        .client
        .call(Request::SessionSnapshot(hutan_protocol::messages::Empty {}))
        .await
        .unwrap();
    let of = |id| snapshot.sessions.iter().find(|s| s.id == id).unwrap();
    assert_eq!(
        of(in_main.id).placement.as_ref().unwrap().kind,
        PlacementKind::Main
    );
    assert_eq!(of(in_wt.id).placement.as_ref().unwrap().root, wt.path);
    assert_eq!(of(outside.id).placement, None);
    env.stop().await;
}

/// Prints where it goes to the terminal as OSC 7 after `cd`.
const CD_AND_REPORT: &str = r#"
sleep 1
cd "$TARGET" || exit 1
printf '\033]7;file://localhost%s\007' "$(pwd -P)"
exec sleep 120
"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_session_that_changes_directory_changes_placement() {
    let mut env = Env::start().await;
    let wt = env.create("moved").await;

    let mut script = sh(CD_AND_REPORT);
    script.cwd = Some(env.repo.root.clone());
    script
        .env
        .insert("TARGET".into(), wt.path.display().to_string());
    let session = create_in(&env.client, Some(env.workspace), script).await;
    assert_eq!(session.placement.unwrap().kind, PlacementKind::Main);

    let moved = env
        .placement_update(session.id, "placement in the worktree", |s| {
            s.placement
                .as_ref()
                .is_some_and(|p| p.kind == PlacementKind::Worktree)
        })
        .await;
    assert_eq!(moved.cwd.as_deref(), Some(wt.path.as_path()));
    assert_eq!(moved.placement.unwrap().branch.as_deref(), Some("moved"));
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_worktree_drops_the_placement_of_its_stopped_sessions() {
    let mut env = Env::start().await;
    let wt = env.create("short-lived").await;
    let session = env.session_in(&wt.path).await;
    assert_eq!(session.placement.unwrap().kind, PlacementKind::Worktree);
    stop(&env.client, session.id).await.unwrap();

    env.remove(&wt.path, false, false).await.unwrap();
    let gone = env
        .placement_update(session.id, "placement dropped", |s| s.placement.is_none())
        .await;
    assert!(matches!(gone.state, SessionState::Stopped));
    env.stop().await;
}

// shared_with -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn two_agents_in_one_checkout_share_it_until_one_stops() {
    let mut env = Env::start().await;
    let wt = env.create("busy").await;
    let first = env.session_in(&env.repo.root).await;
    let second = env.session_in(&env.repo.root).await;
    let in_wt = env.session_in(&wt.path).await;
    let shell_only = env.session_in(&env.repo.root).await;

    // One agent alone shares nothing; neither does a shell next to it.
    env.report(first.id, "claude-hooks", 1).await;
    assert!(
        env.session_info(first.id)
            .await
            .placement
            .unwrap()
            .shared_with
            .is_empty()
    );

    // The second agent arrives: both lists name the other, announced with session.updated.
    env.report(second.id, "omp-hooks", 1).await;
    let updated = env
        .placement_update(first.id, "first shares with second", |s| {
            s.placement
                .as_ref()
                .is_some_and(|p| p.shared_with == vec![second.id])
        })
        .await;
    assert!(updated.agent.is_some());
    let info = env.session_info(second.id).await;
    assert_eq!(info.placement.unwrap().shared_with, vec![first.id]);

    // An agent in the worktree, a shell in main: not part of it.
    env.report(in_wt.id, "codex", 1).await;
    assert!(
        env.session_info(in_wt.id)
            .await
            .placement
            .unwrap()
            .shared_with
            .is_empty()
    );
    assert!(
        env.session_info(shell_only.id)
            .await
            .placement
            .unwrap()
            .shared_with
            .is_empty()
    );
    assert_eq!(
        env.session_info(first.id)
            .await
            .placement
            .unwrap()
            .shared_with,
        vec![second.id]
    );

    // The second one stops: the first is alone again, and says so.
    stop(&env.client, second.id).await.unwrap();
    env.placement_update(first.id, "first alone", |s| {
        s.placement
            .as_ref()
            .is_some_and(|p| p.shared_with.is_empty())
    })
    .await;
    assert!(matches!(
        env.session_info(second.id).await.state,
        SessionState::Stopped
    ));
    assert!(
        env.session_info(second.id)
            .await
            .placement
            .unwrap()
            .shared_with
            .is_empty()
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn releasing_an_agent_clears_the_sharing_and_a_new_report_brings_it_back() {
    let mut env = Env::start().await;
    let a = env.session_in(&env.repo.root).await;
    let b = env.session_in(&env.repo.root).await;
    env.report(a.id, "x", 1).await;
    env.report(b.id, "y", 1).await;
    env.placement_update(a.id, "shared", |s| {
        s.placement
            .as_ref()
            .is_some_and(|p| p.shared_with == vec![b.id])
    })
    .await;

    let released: AgentApplied = env
        .client
        .call(Request::AgentRelease(AgentReleaseRequest {
            session_id: b.id,
            source: "y".into(),
            seq: 2,
            node: None,
            outcome: None,
        }))
        .await
        .unwrap();
    assert!(released.applied);
    env.placement_update(a.id, "alone after the release", |s| {
        s.placement
            .as_ref()
            .is_some_and(|p| p.shared_with.is_empty())
    })
    .await;

    env.report(b.id, "y", 3).await;
    env.placement_update(a.id, "shared again", |s| {
        s.placement
            .as_ref()
            .is_some_and(|p| p.shared_with == vec![b.id])
    })
    .await;
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_moving_out_of_a_shared_checkout_ends_the_sharing() {
    let mut env = Env::start().await;
    let wt = env.create("away").await;
    let a = env.session_in(&env.repo.root).await;
    let mut script = sh(CD_AND_REPORT);
    script.cwd = Some(env.repo.root.clone());
    script
        .env
        .insert("TARGET".into(), wt.path.display().to_string());
    let b = create_in(&env.client, Some(env.workspace), script).await;
    env.report(a.id, "x", 1).await;
    env.report(b.id, "y", 1).await;
    env.placement_update(a.id, "shared", |s| {
        s.placement
            .as_ref()
            .is_some_and(|p| p.shared_with == vec![b.id])
    })
    .await;
    // `b` changes into the worktree: `a` is alone again.
    env.placement_update(a.id, "alone after the move", |s| {
        s.placement
            .as_ref()
            .is_some_and(|p| p.shared_with.is_empty())
    })
    .await;
    let b_info = env.session_info(b.id).await;
    let placement = b_info.placement.unwrap();
    assert_eq!(placement.kind, PlacementKind::Worktree);
    assert!(placement.shared_with.is_empty());
    env.stop().await;
}

impl Env {
    async fn session_info(&self, id: SessionId) -> SessionInfo {
        self.client
            .call::<hutan_protocol::messages::SessionList>(Request::SessionList(Default::default()))
            .await
            .unwrap()
            .sessions
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
    }
}
