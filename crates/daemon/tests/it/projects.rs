//! Automatic projects and user workspaces (roadmap M18-A, protocol 0.19, ADR 0014) end to end: a
//! real daemon on a temporary `HUTAN_HOME`, real client connections, real PTY sessions, and git
//! repositories the tests create in temporary directories (never this repository, never the
//! user's). Identity rules without a daemon are unit tests in `src/projects.rs`, the conversion
//! of earlier workspaces in `src/project_migration.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, create_in, remote_code, sh, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::{Seq, SessionId, WorkspaceId};
use hutan_daemon::{DaemonConfig, Paths};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::messages::{
    CreatedBy, Empty, Event, Request, SessionInfo, SessionResult, SessionSnapshot, TerminalSpec,
    WorkspaceCreate, WorkspaceInfo, WorkspaceKind, WorkspaceList, WorkspaceOpen, WorkspaceOpened,
    WorkspaceRef, WorkspaceRoot,
};
use hutan_protocol::project::{WorkspaceAssign, WorkspaceDelete};
use hutan_protocol::task::TaskCreate;
use hutan_protocol::worktree::{PlacementKind, WorktreeCreate};
use hutan_storage::{Storage, WorkspaceRow};
use tempfile::TempDir;
use tokio::sync::broadcast;

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

/// A temporary directory (canonical: on macOS `/var` is a symlink) holding repositories and
/// plain folders.
struct Tree {
    _dir: TempDir,
    base: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        Self { _dir: dir, base }
    }

    /// A repository `<base>/<name>` on branch `main` with one commit.
    fn repo(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&root, &["add", "a.txt"]);
        git(&root, &["commit", "-q", "-m", "first"]);
        root
    }

    fn folder(&self, name: &str) -> PathBuf {
        let path = self.base.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }
}

fn common_dir(repo: &Path) -> PathBuf {
    fs::canonicalize(repo.join(".git")).unwrap()
}

fn sleeper(cwd: &Path) -> TerminalSpec {
    TerminalSpec {
        cwd: Some(cwd.to_owned()),
        ..sh("sleep 60")
    }
}

async fn workspaces(client: &Client) -> Vec<WorkspaceInfo> {
    client
        .call::<WorkspaceList>(Request::WorkspaceList(Empty {}))
        .await
        .unwrap()
        .workspaces
}

async fn workspace(client: &Client, id: WorkspaceId) -> WorkspaceInfo {
    workspaces(client)
        .await
        .into_iter()
        .find(|w| w.id == id)
        .unwrap_or_else(|| panic!("workspace {id} not listed"))
}

fn names(list: &[WorkspaceInfo]) -> Vec<&str> {
    list.iter().map(|w| w.name.as_str()).collect()
}

fn of_kind(list: &[WorkspaceInfo], kind: WorkspaceKind) -> Vec<&WorkspaceInfo> {
    list.iter().filter(|w| w.kind == kind).collect()
}

async fn open(client: &Client, path: &Path) -> WorkspaceOpened {
    client
        .call(Request::WorkspaceOpen(WorkspaceOpen {
            path: path.to_owned(),
            created_by: None,
        }))
        .await
        .unwrap()
}

async fn snapshot(client: &Client) -> SessionSnapshot {
    client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap()
}

async fn create_workspace(
    client: &Client,
    name: &str,
    paths: &[&Path],
    project_ids: &[WorkspaceId],
) -> Result<WorkspaceInfo, hutan_client::ClientError> {
    client
        .call(Request::WorkspaceCreate(WorkspaceCreate {
            name: name.into(),
            paths: paths.iter().map(|p| p.to_path_buf()).collect(),
            project_ids: project_ids.to_vec(),
            created_by: Some(CreatedBy::Cli),
        }))
        .await
}

async fn assign(
    client: &Client,
    project: WorkspaceId,
    workspace: Option<WorkspaceId>,
) -> Result<WorkspaceInfo, hutan_client::ClientError> {
    client
        .call(Request::WorkspaceAssign(WorkspaceAssign {
            project_id: project,
            workspace_id: workspace,
        }))
        .await
}

async fn delete(client: &Client, id: WorkspaceId) -> Result<Empty, hutan_client::ClientError> {
    client
        .call(Request::WorkspaceDelete(WorkspaceDelete {
            workspace_id: id,
        }))
        .await
}

async fn close(client: &Client, id: WorkspaceId) -> Result<Empty, hutan_client::ClientError> {
    client
        .call(Request::WorkspaceClose(WorkspaceRef { workspace_id: id }))
        .await
}

/// A session in `cwd` without naming a workspace.
async fn session_in(client: &Client, cwd: &Path) -> SessionInfo {
    create_in(client, None, sleeper(cwd)).await
}

// Where sessions go ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sessions_in_the_main_checkout_and_a_worktree_land_in_one_project() {
    let tree = Tree::new();
    let repo = tree.repo("app");
    fs::create_dir_all(repo.join("src")).unwrap();
    let wt = tree.base.join("app-wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            wt.to_str().unwrap(),
        ],
    );
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;

    let in_main = session_in(&client, &repo).await;
    let in_src = session_in(&client, &repo.join("src")).await;
    let in_wt = session_in(&client, &wt).await;

    assert_eq!(in_src.workspace_id, in_main.workspace_id);
    assert_eq!(in_wt.workspace_id, in_main.workspace_id, "one project");
    let list = workspaces(&client).await;
    assert_eq!(names(&list), ["Home", "app"], "Home and the one project");
    let project = &list[1];
    assert_eq!(project.id, in_main.workspace_id);
    assert_eq!(project.kind, WorkspaceKind::Project);
    assert_eq!(project.roots, std::slice::from_ref(&repo));
    let info = project.project.clone().unwrap();
    assert_eq!(info.root, repo);
    assert_eq!(info.common_dir, Some(common_dir(&repo)));
    assert_eq!(project.parent_id, None);
    assert_eq!((project.live_sessions, project.total_sessions), (3, 3));
    assert_eq!(project.created_by, CreatedBy::User);

    // The folder is where the session started; the checkout says main or worktree.
    assert_eq!(in_wt.folder, Some(wt.clone()));
    assert_eq!(in_main.folder, Some(repo.clone()));
    assert_eq!(
        in_wt.placement.as_ref().unwrap().kind,
        PlacementKind::Worktree
    );
    assert_eq!(
        in_main.placement.as_ref().unwrap().kind,
        PlacementKind::Main
    );

    // The shell's directory moves `folder` (and `cwd`), never the project.
    let sub = repo.join("src/deeper");
    fs::create_dir_all(&sub).unwrap();
    let cd = create_in(
        &client,
        None,
        TerminalSpec {
            cwd: Some(repo.clone()),
            ..sh("cd src/deeper; printf '\\033]7;file://localhost%s\\007' \"$(pwd -P)\"; sleep 60")
        },
    )
    .await;
    assert_eq!(cd.workspace_id, project.id);
    let moved = wait_event(&mut events, "the shell's directory", |e| match e {
        Event::SessionUpdated { session }
            if session.id == cd.id && session.folder.as_deref() == Some(sub.as_path()) =>
        {
            Some(session.clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(
        moved.workspace_id, project.id,
        "the project does not follow"
    );
    assert_eq!(moved.cwd.as_deref(), Some(sub.as_path()));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_repository_or_folder_makes_its_project_and_announces_it() {
    let tree = Tree::new();
    let repo = tree.repo("fresh");
    let notes = tree.folder("notes");
    let notes_sub = tree.folder("notes/2026/october");
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    assert_eq!(names(&workspaces(&client).await), ["Home"]);

    let session = session_in(&client, &repo).await;
    let announced = wait_event(&mut events, "the new project", |e| match e {
        Event::WorkspaceUpdated { workspace } if workspace.kind == WorkspaceKind::Project => {
            Some(workspace.clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(announced.id, session.workspace_id);
    assert_eq!(announced.name, "fresh");
    assert_eq!(announced.created_by, CreatedBy::User);
    assert_eq!(
        announced.project.as_ref().unwrap().common_dir,
        Some(common_dir(&repo))
    );

    // A folder outside git is its own project; folders below it belong to it.
    let in_notes = session_in(&client, &notes).await;
    let in_sub = session_in(&client, &notes_sub).await;
    assert_eq!(in_sub.workspace_id, in_notes.workspace_id);
    let project = workspace(&client, in_notes.workspace_id).await;
    assert_eq!(project.name, "notes");
    assert_eq!(project.roots, std::slice::from_ref(&notes));
    let info = project.project.unwrap();
    assert_eq!((info.common_dir, info.root), (None, notes));
    assert_eq!(
        names(&workspaces(&client).await),
        ["Home", "fresh", "notes"]
    );

    // `workspace.open` is the same lookup, and says whether it made the project.
    let opened = open(&client, &repo.join(".")).await;
    assert!(!opened.created);
    assert_eq!(opened.workspace.id, session.workspace_id);
    let other = tree.repo("other");
    let made = open(&client, &other).await;
    assert!(made.created);
    assert_eq!(made.workspace.kind, WorkspaceKind::Project);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn home_the_home_folder_and_the_root_have_no_project() {
    let tree = Tree::new();
    let user_home = tree.folder("me");
    let docs = tree.folder("me/docs");
    let app = tree.repo("me/code-app");
    let t = TestDaemon::start_with("", {
        let user_home = user_home.clone();
        move |config| config.paths.user_home = Some(user_home)
    })
    .await;
    let client = t.connect().await;
    let home_folder = fs::canonicalize(&t.paths.default_home_folder).unwrap();
    fs::create_dir_all(home_folder.join("scratch")).unwrap();
    let home = workspaces(&client)
        .await
        .into_iter()
        .find(|w| w.kind == WorkspaceKind::Home)
        .unwrap();

    for cwd in [
        user_home.clone(),
        home_folder.clone(),
        home_folder.join("scratch"),
        PathBuf::from("/"),
    ] {
        let session = session_in(&client, &cwd).await;
        assert_eq!(session.workspace_id, home.id, "{}", cwd.display());
    }
    let without_folder = create_in(&client, None, sh("sleep 60")).await;
    assert_eq!(without_folder.workspace_id, home.id, "no folder at all");
    assert_eq!(
        names(&workspaces(&client).await),
        ["Home"],
        "nothing was made"
    );

    // A dotfiles repository in the home directory does not swallow the folders below it.
    git(&user_home, &["init", "-q", "-b", "main"]);
    assert_eq!(session_in(&client, &user_home).await.workspace_id, home.id);
    let in_docs = session_in(&client, &docs).await;
    assert_ne!(in_docs.workspace_id, home.id);
    assert_eq!(workspace(&client, in_docs.workspace_id).await.name, "docs");
    let in_app = session_in(&client, &app).await;
    assert_eq!(
        workspace(&client, in_app.workspace_id).await.name,
        "code-app"
    );

    // `workspace.open` of Home's places answers Home.
    for place in [&user_home, &home_folder] {
        let opened = open(&client, place).await;
        assert_eq!(opened.workspace.id, home.id);
        assert!(!opened.created);
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_workspace_wins_and_auto_can_be_switched_off() {
    let tree = Tree::new();
    let repo = tree.repo("app");
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let shop = create_workspace(&client, "shop", &[], &[]).await.unwrap();

    let named = create_in(&client, Some(shop.id), sleeper(&repo)).await;
    assert_eq!(named.workspace_id, shop.id, "the request names it");
    assert_eq!(
        names(&workspaces(&client).await),
        ["Home", "shop"],
        "no project was made for it"
    );
    let auto = session_in(&client, &repo).await;
    let project = workspace(&client, auto.workspace_id).await;
    assert_eq!(project.kind, WorkspaceKind::Project);
    let in_project = create_in(&client, Some(project.id), sleeper(&tree.base)).await;
    assert_eq!(in_project.workspace_id, project.id);
    t.stop().await;

    // `[projects] auto = false`: Home, as before 0.19; naming a workspace still works.
    let t = TestDaemon::start_with("[projects]\nauto = false\n", |_| {}).await;
    let client = t.connect().await;
    let home = workspaces(&client).await[0].clone();
    let session = session_in(&client, &repo).await;
    assert_eq!(session.workspace_id, home.id);
    assert_eq!(names(&workspaces(&client).await), ["Home"]);
    let shop = create_workspace(&client, "shop", &[], &[]).await.unwrap();
    let named = create_in(&client, Some(shop.id), sleeper(&repo)).await;
    assert_eq!(named.workspace_id, shop.id);
    t.stop().await;
}

// Chats ---------------------------------------------------------------------------------------

fn replay(received: &Path) -> CodexConfig {
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
    let transcript = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/codex-0.162/plain-turn.jsonl");
    CodexConfig {
        program: bin,
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

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_lands_in_the_project_of_its_folder() {
    let tree = Tree::new();
    let repo = tree.repo("app");
    let received = tree.base.join("received.jsonl");
    let config = replay(&received);
    let t = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
    let client = t.connect().await;

    let chat = client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: repo.clone(),
            agent: "codex".to_owned(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: None,
            system_prompt: None,
        }))
        .await
        .unwrap()
        .session;
    let project = workspace(&client, chat.workspace_id).await;
    assert_eq!(
        (project.kind, project.name.as_str()),
        (WorkspaceKind::Project, "app")
    );
    assert_eq!(chat.folder, Some(repo.clone()));
    // A terminal in the same repository shares the project.
    assert_eq!(session_in(&client, &repo).await.workspace_id, project.id);
    t.stop().await;
}

// Workspaces ----------------------------------------------------------------------------------

async fn collect_until(
    events: &mut broadcast::Receiver<(Seq, Event)>,
    what: &str,
    done: impl Fn(&Event) -> bool,
) -> Vec<Event> {
    let mut seen = Vec::new();
    common::wait_event(events, what, |event| {
        seen.push(event.clone());
        done(event).then_some(())
    })
    .await;
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_holds_projects_and_counts_their_sessions() {
    let tree = Tree::new();
    let api = tree.repo("api");
    let web = tree.repo("web");
    let notes = tree.folder("notes");
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let notes_project = open(&client, &notes).await.workspace;

    // One step: a workspace with the projects of two folders and an existing project.
    let shop = create_workspace(&client, "shop", &[&api, &web], &[notes_project.id])
        .await
        .unwrap();
    assert_eq!(shop.kind, WorkspaceKind::Workspace);
    assert_eq!(shop.created_by, CreatedBy::Cli);
    assert_eq!(shop.project, None);
    assert_eq!(shop.parent_id, None);
    let list = workspaces(&client).await;
    assert_eq!(names(&list), ["Home", "notes", "shop", "api", "web"]);
    let shop = workspace(&client, shop.id).await;
    assert_eq!(
        shop.roots,
        [notes.clone(), api.clone(), web.clone()],
        "roots of its projects"
    );
    for project in of_kind(&list, WorkspaceKind::Project) {
        assert_eq!(project.parent_id, Some(shop.id), "{}", project.name);
    }

    // The workspace is announced first, then every project that moved in, then the workspace
    // again with all its roots.
    let seen = collect_until(&mut events, "the workspace with its roots", |e| {
        matches!(e, Event::WorkspaceUpdated { workspace }
            if workspace.id == shop.id && workspace.roots.len() == 3)
    })
    .await;
    let order: Vec<_> = seen
        .iter()
        .filter_map(|e| match e {
            Event::WorkspaceUpdated { workspace } => Some(workspace.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(order[0], "notes", "{order:?}");
    assert!(
        order.contains(&"shop".to_owned()) && order.contains(&"api".to_owned()),
        "{order:?}"
    );

    // Sessions of its projects count in the workspace too.
    let api_project = list.iter().find(|w| w.name == "api").unwrap().clone();
    let in_api = session_in(&client, &api).await;
    let in_web = session_in(&client, &web.join(".")).await;
    assert_eq!(in_api.workspace_id, api_project.id);
    let shop_now = workspace(&client, shop.id).await;
    assert_eq!((shop_now.live_sessions, shop_now.total_sessions), (2, 2));
    assert_eq!(
        shop_now.last_active_ms,
        workspace(&client, api_project.id)
            .await
            .last_active_ms
            .max(workspace(&client, in_web.workspace_id).await.last_active_ms),
        "a workspace is as active as its projects"
    );
    assert_eq!(workspace(&client, api_project.id).await.total_sessions, 1);
    client
        .call::<SessionResult>(Request::SessionStop(hutan_protocol::messages::SessionRef {
            session_id: in_web.id,
        }))
        .await
        .unwrap();
    wait_event(&mut events, "the workspace's counts", |e| match e {
        Event::WorkspaceUpdated { workspace }
            if workspace.id == shop.id && workspace.live_sessions == 1 =>
        {
            Some(())
        }
        _ => None,
    })
    .await;

    // Out of the workspace, into another one; both announce their new roots.
    let lab = create_workspace(&client, "lab", &[], &[]).await.unwrap();
    let out = assign(&client, notes_project.id, None).await.unwrap();
    assert_eq!(out.parent_id, None);
    assert_eq!(
        workspace(&client, shop.id).await.roots,
        [api.clone(), web.clone()]
    );
    let moved = assign(&client, notes_project.id, Some(lab.id))
        .await
        .unwrap();
    assert_eq!(moved.parent_id, Some(lab.id));
    assert_eq!(
        workspace(&client, lab.id).await.roots,
        std::slice::from_ref(&notes)
    );
    // Idempotent.
    assign(&client, notes_project.id, Some(lab.id))
        .await
        .unwrap();

    // Wrong kinds and unknown ids.
    let home = list[0].id;
    for (project, target, code) in [
        (shop.id, None, ErrorCode::InvalidState),
        (home, None, ErrorCode::InvalidState),
        (
            api_project.id,
            Some(api_project.id),
            ErrorCode::InvalidState,
        ),
        (api_project.id, Some(home), ErrorCode::InvalidState),
        (WorkspaceId::new(), None, ErrorCode::NotFound),
        (
            api_project.id,
            Some(WorkspaceId::new()),
            ErrorCode::NotFound,
        ),
    ] {
        assert_eq!(remote_code(assign(&client, project, target).await), code);
    }
    assert_eq!(
        remote_code(create_workspace(&client, "bad", &[], &[shop.id]).await),
        ErrorCode::InvalidState
    );
    assert_eq!(
        remote_code(create_workspace(&client, "  ", &[], &[]).await),
        ErrorCode::BadRequest
    );
    assert_eq!(
        remote_code(create_workspace(&client, "gone", &[&tree.base.join("missing")], &[]).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        names(&workspaces(&client).await)
            .iter()
            .filter(|n| **n == "bad")
            .count(),
        0
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_workspace_keeps_its_projects_and_closing_a_project_leaves_its_workspace() {
    let tree = Tree::new();
    let api = tree.repo("api");
    let web = tree.repo("web");
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let shop = create_workspace(&client, "shop", &[&api, &web], &[])
        .await
        .unwrap();
    let list = workspaces(&client).await;
    let api_project = list.iter().find(|w| w.name == "api").unwrap().clone();
    let web_project = list.iter().find(|w| w.name == "web").unwrap().clone();

    // A project is closed with `close`, which `delete` refuses; the workspace loses a root.
    assert_eq!(
        remote_code(delete(&client, api_project.id).await),
        ErrorCode::InvalidState
    );
    close(&client, web_project.id).await.unwrap();
    assert_eq!(
        workspace(&client, shop.id).await.roots,
        std::slice::from_ref(&api)
    );
    let reopened = open(&client, &web).await;
    assert!(reopened.created, "a closed project is made again");
    assert_eq!(reopened.workspace.name, "web");
    assert_eq!(reopened.workspace.parent_id, None);

    // Live sessions of the workspace's own refuse its deletion; ended ones go with it.
    let own = create_in(&client, Some(shop.id), sleeper(&api)).await;
    assert_eq!(
        remote_code(delete(&client, shop.id).await),
        ErrorCode::InvalidState
    );
    common::stop(&client, own.id).await.unwrap();
    let in_project = session_in(&client, &api).await;
    assert_eq!(in_project.workspace_id, api_project.id);

    delete(&client, shop.id).await.unwrap();
    let seen = collect_until(
        &mut events,
        "the workspace closed",
        |e| matches!(e, Event::WorkspaceClosed { workspace_id } if *workspace_id == shop.id),
    )
    .await;
    let ungrouped = seen.iter().position(|e| {
        matches!(e, Event::WorkspaceUpdated { workspace }
        if workspace.id == api_project.id && workspace.parent_id.is_none())
    });
    assert!(
        ungrouped.is_some(),
        "the project is announced ungrouped: {seen:?}"
    );
    let list = workspaces(&client).await;
    assert_eq!(names(&list), ["Home", "api", "web"]);
    assert!(list.iter().all(|w| w.parent_id.is_none()));
    let snap = snapshot(&client).await;
    assert!(
        snap.sessions.iter().any(|s| s.id == in_project.id),
        "the project's session stays"
    );
    assert!(
        snap.sessions.iter().all(|s| s.id != own.id),
        "the workspace's own went"
    );
    assert_eq!(
        remote_code(delete(&client, shop.id).await),
        ErrorCode::NotFound
    );

    // Closing a workspace deletes it too.
    let lab = create_workspace(&client, "lab", &[&api], &[])
        .await
        .unwrap();
    assert_eq!(
        workspace(&client, api_project.id).await.parent_id,
        Some(lab.id)
    );
    close(&client, lab.id).await.unwrap();
    assert_eq!(workspace(&client, api_project.id).await.parent_id, None);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn workspaces_projects_and_identity_survive_a_restart() {
    let tree = Tree::new();
    let api = tree.repo("api");
    let notes = tree.folder("notes");
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let shop = create_workspace(&client, "shop", &[&api, &notes], &[])
        .await
        .unwrap();
    client
        .call::<WorkspaceInfo>(Request::WorkspacePin(
            hutan_protocol::messages::WorkspacePin {
                workspace_id: shop.id,
                pinned: true,
            },
        ))
        .await
        .unwrap();
    let before = workspaces(&client).await;
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    let after = workspaces(&client).await;
    let strip = |list: &[WorkspaceInfo]| {
        list.iter()
            .map(|w| {
                (
                    w.id,
                    w.name.clone(),
                    w.kind,
                    w.parent_id,
                    w.project.clone(),
                    w.roots.clone(),
                    w.pinned,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(strip(&after), strip(&before));
    // The stored identity is what new sessions are matched against: no second project.
    let again = session_in(&client, &api.join(".")).await;
    assert_eq!(workspaces(&client).await.len(), before.len());
    assert_eq!(
        workspace(&client, again.workspace_id).await.parent_id,
        Some(shop.id)
    );
    t.stop().await;
}

// Restore -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn restore_keeps_assignments_and_follows_a_moved_project() {
    let tree = Tree::new();
    let repo = tree.repo("app");
    let elsewhere = tree.folder("elsewhere");
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let auto = session_in(&client, &repo).await;
    let project = workspace(&client, auto.workspace_id).await;
    // Started in a folder of its own, but put into the project by name.
    let named = create_in(&client, Some(project.id), sleeper(&elsewhere)).await;
    drop(client);

    // Nothing moved: the assignments are as they were.
    let t = t.restart().await;
    let client = t.connect().await;
    let snap = snapshot(&client).await;
    for session in [&auto, &named] {
        let restored = snap.sessions.iter().find(|s| s.id == session.id).unwrap();
        assert_eq!(restored.workspace_id, project.id);
        assert_eq!(restored.folder, session.folder);
    }
    drop(client);

    // The repository goes away. A session whose own folder is gone too keeps its project; the
    // one that started somewhere that still exists follows that folder.
    fs::remove_dir_all(&repo).unwrap();
    let t = t.restart().await;
    let client = t.connect().await;
    let snap = snapshot(&client).await;
    let kept = snap.sessions.iter().find(|s| s.id == auto.id).unwrap();
    assert_eq!(
        kept.workspace_id, project.id,
        "a missing folder keeps the stored project"
    );
    let followed = snap.sessions.iter().find(|s| s.id == named.id).unwrap();
    assert_ne!(followed.workspace_id, project.id);
    let new_project = workspace(&client, followed.workspace_id).await;
    assert_eq!(new_project.name, "elsewhere");
    assert_eq!(new_project.roots, std::slice::from_ref(&elsewhere));
    drop(client);

    // And it stays there afterwards.
    let t = t.restart().await;
    let client = t.connect().await;
    let snap = snapshot(&client).await;
    let again = snap.sessions.iter().find(|s| s.id == named.id).unwrap();
    assert_eq!(again.workspace_id, new_project.id);
    t.stop().await;
}

// Tasks and worktrees -------------------------------------------------------------------------

async fn task_session(
    client: &Client,
    workspace: WorkspaceId,
    worktree: Option<&str>,
) -> SessionInfo {
    use hutan_core::{TaskPlacement, TaskPlacementKind, TaskRun};
    let created = client
        .task_create(TaskCreate {
            workspace_id: workspace,
            title: "work".into(),
            prompt: None,
            depends_on: Vec::new(),
            placement: worktree.map(|branch| TaskPlacement {
                kind: TaskPlacementKind::Worktree,
                branch: Some(branch.to_owned()),
                base: None,
            }),
            run: Some(TaskRun::Terminal {
                argv: vec!["sleep".into(), "60".into()],
            }),
            auto_start: false,
            after_policy: Default::default(),
        })
        .await
        .unwrap();
    let started = client.task_start(created.id).await.unwrap();
    started.session
}

#[tokio::test(flavor = "multi_thread")]
async fn tasks_and_worktree_sessions_land_in_the_project_of_their_folder() {
    let tree = Tree::new();
    let repo = tree.repo("app");
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let shop = create_workspace(&client, "shop", &[&repo], &[])
        .await
        .unwrap();
    let project = workspaces(&client)
        .await
        .into_iter()
        .find(|w| w.kind == WorkspaceKind::Project)
        .unwrap();
    assert_eq!(project.parent_id, Some(shop.id));

    // A task of the workspace runs in the project of its folder; one of the project, there.
    let main = task_session(&client, shop.id, None).await;
    assert_eq!(main.workspace_id, project.id);
    assert_eq!(main.folder, Some(repo.clone()));
    let direct = task_session(&client, project.id, None).await;
    assert_eq!(direct.workspace_id, project.id);

    // A task in a worktree: the worktree is the same project.
    let in_wt = task_session(&client, shop.id, Some("task/one")).await;
    assert_eq!(in_wt.workspace_id, project.id);
    let wt = in_wt.folder.clone().unwrap();
    assert_ne!(wt, repo, "a worktree of its own");
    assert_eq!(in_wt.placement.unwrap().kind, PlacementKind::Worktree);

    // `worktree.create` and a session in it, with no workspace named.
    let created = client
        .worktree_create(WorktreeCreate {
            workspace_id: project.id,
            repo: None,
            branch: "feature/x".into(),
            base: None,
            path: None,
        })
        .await
        .unwrap();
    let session = session_in(&client, &created.path).await;
    assert_eq!(session.workspace_id, project.id);
    assert_eq!(
        workspaces(&client)
            .await
            .iter()
            .filter(|w| w.kind == WorkspaceKind::Project)
            .count(),
        1,
        "still one project"
    );
    t.stop().await;
}

// Migration -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn earlier_workspaces_become_workspaces_with_projects_at_the_first_start() {
    let tree = Tree::new();
    let api = tree.repo("api");
    let web = tree.repo("web");
    let wt = tree.base.join("api-wt");
    git(
        &api,
        &["worktree", "add", "-q", "-b", "f", wt.to_str().unwrap()],
    );
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_home(dir.path());
    paths.prepare().unwrap();

    // What a daemon before protocol 0.19 left behind: workspaces with roots, a session in one.
    let old = |name: &str, roots: Vec<PathBuf>, position: i64, pinned: bool| WorkspaceRow {
        id: WorkspaceId::new(),
        name: name.to_owned(),
        roots,
        kind: hutan_storage::WorkspaceKind::Project,
        pinned,
        created_by: hutan_storage::CreatedBy::Cli,
        created_at_ms: 1_000 + u64::try_from(position).unwrap(),
        last_active_ms: 2_000,
        position,
        common_dir: None,
        parent_id: None,
    };
    let shop = old("shop", vec![wt.clone(), web.clone()], 1, true);
    let solo = old("solo", vec![tree.folder("solo")], 2, false);
    {
        let storage = Storage::open(&paths.database).unwrap();
        storage.workspaces().insert(shop.clone()).unwrap();
        storage.workspaces().insert(solo.clone()).unwrap();
        storage.close();
    }

    let t = TestDaemon {
        daemon: hutan_daemon::start(DaemonConfig::new(paths.clone()))
            .await
            .unwrap(),
        paths: paths.clone(),
        _dir: dir,
    };
    let client = t.connect().await;
    let list = workspaces(&client).await;
    assert_eq!(names(&list), ["Home", "shop", "api", "web", "solo", "solo"]);
    let by_name = |name: &str, kind| {
        list.iter()
            .find(|w| w.name == name && w.kind == kind)
            .unwrap_or_else(|| panic!("{name} {kind:?} in {list:?}"))
    };
    let container = by_name("shop", WorkspaceKind::Workspace);
    assert_eq!(container.id, shop.id, "the workspace keeps its id");
    assert!(container.pinned);
    assert_eq!(container.created_by, CreatedBy::Cli);
    assert_eq!(
        container.roots,
        [api.clone(), web.clone()],
        "roots of its projects"
    );
    let api_project = by_name("api", WorkspaceKind::Project);
    assert_eq!(api_project.parent_id, Some(shop.id));
    assert_eq!(
        api_project.roots,
        std::slice::from_ref(&api),
        "the main checkout, not the worktree"
    );
    assert_eq!(
        api_project.project.as_ref().unwrap().common_dir,
        Some(common_dir(&api))
    );
    let solo_container = by_name("solo", WorkspaceKind::Workspace);
    assert_eq!(solo_container.id, solo.id);
    assert_eq!(solo_container.roots.len(), 1);

    // A session in the repository (from the worktree) finds the migrated project.
    let session = session_in(&client, &wt).await;
    assert_eq!(session.workspace_id, api_project.id);
    assert_eq!(workspaces(&client).await.len(), list.len());
    drop(client);

    // A second start converts nothing again.
    let t = t.restart().await;
    let client = t.connect().await;
    let again = workspaces(&client).await;
    assert_eq!(again.len(), list.len());
    assert_eq!(
        again.iter().map(|w| w.id).collect::<Vec<_>>(),
        list.iter().map(|w| w.id).collect::<Vec<_>>()
    );
    t.stop().await;
}

// The workspace roots a client reads ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn add_and_remove_root_move_projects_between_workspaces() {
    let tree = Tree::new();
    let api = tree.repo("api");
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let one = create_workspace(&client, "one", &[&api], &[])
        .await
        .unwrap();
    let two = create_workspace(&client, "two", &[], &[]).await.unwrap();
    assert_eq!(
        workspace(&client, one.id).await.roots,
        std::slice::from_ref(&api)
    );

    // A project of another workspace moves.
    let moved: WorkspaceInfo = client
        .call(Request::WorkspaceAddRoot(WorkspaceRoot {
            workspace_id: two.id,
            path: api.join("."),
        }))
        .await
        .unwrap();
    assert_eq!(moved.roots, std::slice::from_ref(&api));
    assert!(workspace(&client, one.id).await.roots.is_empty());
    let _: SessionId = session_in(&client, &api).await.id;
    assert_eq!(
        workspace(&client, two.id).await.total_sessions,
        1,
        "counts follow the member"
    );
    assert_eq!(workspace(&client, one.id).await.total_sessions, 0);
    t.stop().await;
}
