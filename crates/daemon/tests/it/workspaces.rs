//! Projects and workspaces (roadmap M12-A, protocol 0.6): Home, `workspace.open` with path
//! dedupe and git-root detection, roots, recent projects, order and pins, activity, and what
//! survives a daemon restart. The daemon runs in-process on a temporary `HUTAN_HOME`; every
//! folder lives in a temporary directory.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use hutan_client::Client;
use hutan_core::WorkspaceId;
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{
    CreatedBy, Empty, Event, RecentProjects, Request, SessionSnapshot, WorkspaceCreate,
    WorkspaceInfo, WorkspaceKind, WorkspaceList, WorkspaceMove, WorkspaceOpen, WorkspaceOpened,
    WorkspacePin, WorkspaceRecent, WorkspaceRef, WorkspaceRename, WorkspaceRoot,
};
use tokio::time::{Instant, sleep, timeout};

fn canon(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap()
}

/// A temporary directory and its canonical path (on macOS `/var` is a symlink).
fn tmp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = canon(dir.path());
    (dir, path)
}

fn mkdir(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap();
    path.to_owned()
}

/// A folder that looks like a git repository to the daemon.
fn repo(path: &Path) -> PathBuf {
    mkdir(&path.join(".git"));
    path.to_owned()
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

async fn names(client: &Client) -> Vec<String> {
    workspaces(client)
        .await
        .into_iter()
        .map(|w| w.name)
        .collect()
}

async fn open_as(client: &Client, path: &Path, created_by: Option<CreatedBy>) -> WorkspaceOpened {
    client
        .call(Request::WorkspaceOpen(WorkspaceOpen {
            path: path.to_owned(),
            created_by,
        }))
        .await
        .unwrap()
}

async fn open(client: &Client, path: &Path) -> WorkspaceOpened {
    open_as(client, path, None).await
}

async fn close(client: &Client, id: WorkspaceId) -> Result<Empty, hutan_client::ClientError> {
    client
        .call(Request::WorkspaceClose(WorkspaceRef { workspace_id: id }))
        .await
}

async fn add_root(
    client: &Client,
    id: WorkspaceId,
    path: &Path,
) -> Result<WorkspaceInfo, hutan_client::ClientError> {
    client
        .call(Request::WorkspaceAddRoot(WorkspaceRoot {
            workspace_id: id,
            path: path.to_owned(),
        }))
        .await
}

async fn remove_root(
    client: &Client,
    id: WorkspaceId,
    path: &Path,
) -> Result<WorkspaceInfo, hutan_client::ClientError> {
    client
        .call(Request::WorkspaceRemoveRoot(WorkspaceRoot {
            workspace_id: id,
            path: path.to_owned(),
        }))
        .await
}

async fn recent(client: &Client, limit: Option<u32>) -> RecentProjects {
    client
        .call(Request::WorkspaceRecent(WorkspaceRecent { limit }))
        .await
        .unwrap()
}

async fn move_before(
    client: &Client,
    id: WorkspaceId,
    before: Option<WorkspaceId>,
) -> Result<WorkspaceList, hutan_client::ClientError> {
    client
        .call(Request::WorkspaceMove(WorkspaceMove {
            workspace_id: id,
            before,
        }))
        .await
}

async fn pin(client: &Client, id: WorkspaceId, pinned: bool) -> WorkspaceInfo {
    client
        .call(Request::WorkspacePin(WorkspacePin {
            workspace_id: id,
            pinned,
        }))
        .await
        .unwrap()
}

async fn create_ws(client: &Client, name: &str, roots: Vec<PathBuf>) -> WorkspaceInfo {
    client
        .call(Request::WorkspaceCreate(WorkspaceCreate {
            name: name.into(),
            paths: roots,
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap()
}

// Home ------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn home_is_bound_to_the_configured_folder_and_follows_the_home_rules() {
    let (_keep, base) = tmp();
    let notes = base.join("notes"); // missing: the daemon creates it
    let t = TestDaemon::start_with(
        &format!("[workspaces]\nhome_folder = \"{}\"\n", notes.display()),
        |_| {},
    )
    .await;
    let mode = fs::metadata(&notes).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "created private");
    assert_eq!(fs::read_dir(&notes).unwrap().count(), 0, "and left empty");

    let client = t.connect().await;
    let list = workspaces(&client).await;
    assert_eq!(list.len(), 1);
    let home = list[0].clone();
    assert_eq!(home.name, "Home");
    assert_eq!(home.kind, WorkspaceKind::Home);
    assert_eq!(home.roots, std::slice::from_ref(&notes));
    assert_eq!((home.pinned, home.created_by), (false, CreatedBy::User));
    assert_eq!((home.live_sessions, home.total_sessions), (0, 0));

    // It cannot be closed or re-rooted, but it can be renamed.
    assert_eq!(
        remote_code(close(&client, home.id).await),
        ErrorCode::InvalidState
    );
    assert_eq!(
        remote_code(add_root(&client, home.id, &base).await),
        ErrorCode::InvalidState
    );
    assert_eq!(
        remote_code(remove_root(&client, home.id, &notes).await),
        ErrorCode::InvalidState
    );
    let renamed: WorkspaceInfo = client
        .call(Request::WorkspaceRename(WorkspaceRename {
            workspace_id: home.id,
            name: "Notes".into(),
        }))
        .await
        .unwrap();
    assert_eq!(renamed.name, "Notes");

    // Sessions without a workspace go to Home; a folder inside it is Home's.
    let session = create(&client, sh("exec cat")).await;
    assert_eq!(session.workspace_id, home.id);
    let sub = mkdir(&notes.join("daily"));
    let opened = open(&client, &sub).await;
    assert!(!opened.created);
    assert_eq!(opened.workspace.id, home.id);
    // Home is not a project: it is not in the recent list.
    assert!(recent(&client, None).await.projects.is_empty());
    drop(client);

    // A changed `home_folder` moves Home; an existing folder is used as is.
    let existing = mkdir(&base.join("existing"));
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        &t.paths.config_file,
        format!("[workspaces]\nhome_folder = \"{}\"\n", existing.display()),
    )
    .unwrap();
    let t = t.restart().await;
    let client = t.connect().await;
    let list = workspaces(&client).await;
    assert_eq!(list.len(), 1, "still exactly one Home");
    assert_eq!(list[0].id, home.id);
    assert_eq!(list[0].name, "Notes", "the rename survives");
    assert_eq!(list[0].roots, std::slice::from_ref(&existing));
    assert_eq!(
        fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(fs::read_dir(&existing).unwrap().count(), 0);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_config_home_is_created_inside_hutan_home() {
    let t = TestDaemon::start().await;
    let folder = &t.paths.default_home_folder;
    assert!(folder.starts_with(t.paths.base.clone()));
    assert!(folder.is_dir());
    let client = t.connect().await;
    assert_eq!(workspaces(&client).await[0].roots, [canon(folder)]);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_home_folder_that_is_not_a_directory_stops_the_daemon_from_starting() {
    let (_keep, base) = tmp();
    let file = base.join("file");
    fs::write(&file, b"x").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let paths = hutan_daemon::Paths::in_home(dir.path());
    fs::write(
        &paths.config_file,
        format!("[workspaces]\nhome_folder = \"{}\"\n", file.display()),
    )
    .unwrap();
    let error = hutan_daemon::start(hutan_daemon::DaemonConfig::new(paths.clone()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Home folder"), "{error}");
    assert!(!paths.socket.exists());

    fs::write(
        &paths.config_file,
        "[workspaces]\nhome_folder = \"relative\"\n",
    )
    .unwrap();
    let error = hutan_daemon::start(hutan_daemon::DaemonConfig::new(paths))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("absolute"), "{error}");
}

// Open ------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn open_creates_once_and_finds_the_workspace_by_any_path_inside_it() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let project = mkdir(&base.join("plain-project"));
    let deep = mkdir(&project.join("a/b"));

    let first = open_as(&client, &project, Some(CreatedBy::Cli)).await;
    assert!(first.created);
    let ws = first.workspace;
    assert_eq!(ws.name, "plain-project");
    assert_eq!(ws.roots, std::slice::from_ref(&project));
    assert_eq!(ws.kind, WorkspaceKind::Project);
    assert_eq!(ws.created_by, CreatedBy::Cli);
    assert!(!ws.pinned);
    assert!(ws.last_active_ms > 0);
    assert_eq!(
        workspaces(&client).await.last().unwrap().id,
        ws.id,
        "new workspaces go last"
    );
    let announced = wait_event(&mut events, "the new workspace", |e| match e {
        Event::WorkspaceUpdated { workspace } if workspace.id == ws.id => Some(workspace.clone()),
        _ => None,
    })
    .await;
    assert_eq!(announced, ws);

    // The same folder, a folder inside it, and a symlink to it all find the workspace.
    let link = base.join("link-to-project");
    std::os::unix::fs::symlink(&project, &link).unwrap();
    for path in [&project, &deep, &link] {
        let again = open_as(&client, path, Some(CreatedBy::Agent)).await;
        assert!(!again.created, "{}", path.display());
        assert_eq!(again.workspace.id, ws.id);
        assert_eq!(again.workspace.created_by, CreatedBy::Cli, "origin is kept");
    }
    assert_eq!(workspaces(&client).await.len(), 2, "Home and the project");

    // Another folder is another workspace.
    let other = open(&client, &mkdir(&base.join("other"))).await;
    assert!(other.created);
    assert_ne!(other.workspace.id, ws.id);
    assert_eq!(other.workspace.created_by, CreatedBy::User, "the default");

    // Bad paths.
    let bad = |path: PathBuf| {
        let client = client.clone();
        async move {
            remote_code(
                client
                    .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
                        path,
                        created_by: None,
                    }))
                    .await,
            )
        }
    };
    assert_eq!(bad("relative/dir".into()).await, ErrorCode::BadRequest);
    assert_eq!(bad(base.join("missing")).await, ErrorCode::NotFound);
    let file = base.join("file");
    fs::write(&file, b"x").unwrap();
    assert_eq!(bad(file).await, ErrorCode::BadRequest);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_inside_a_git_repository_opens_the_repository() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let repo_dir = repo(&base.join("hutan-clone"));
    let inner = mkdir(&repo_dir.join("crates/daemon"));

    let opened = open(&client, &inner).await;
    assert!(opened.created);
    assert_eq!(
        opened.workspace.roots,
        std::slice::from_ref(&repo_dir),
        "the git root"
    );
    assert_eq!(opened.workspace.name, "hutan-clone");

    let again = open(&client, &repo_dir).await;
    assert!(!again.created);
    assert_eq!(again.workspace.id, opened.workspace.id);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_can_hold_several_repositories() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let api = repo(&base.join("api"));
    let web = repo(&base.join("web"));
    let web_src = mkdir(&web.join("src"));

    let ws = create_ws(&client, "shop", vec![]).await;
    assert_eq!(ws.kind, WorkspaceKind::Workspace);
    assert!(ws.roots.is_empty());
    // Adding resolves the repository root like open does, and is idempotent.
    let with_api = add_root(&client, ws.id, &api).await.unwrap();
    assert_eq!(with_api.roots, std::slice::from_ref(&api));
    let with_web = add_root(&client, ws.id, &web_src).await.unwrap();
    assert_eq!(with_web.roots, [api.clone(), web.clone()]);
    assert_eq!(add_root(&client, ws.id, &web).await.unwrap().roots.len(), 2);
    assert_eq!(
        remote_code(add_root(&client, ws.id, &base.join("missing")).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(add_root(&client, ws.id, Path::new("rel")).await),
        ErrorCode::BadRequest
    );
    assert_eq!(
        remote_code(add_root(&client, WorkspaceId::new(), &web).await),
        ErrorCode::NotFound
    );
    // A project is one folder: folders are added to workspaces, not to projects.
    let project = open(&client, &api).await.workspace;
    assert_eq!(project.kind, WorkspaceKind::Project);
    assert_eq!(project.parent_id, Some(ws.id));
    assert_eq!(
        remote_code(add_root(&client, project.id, &web).await),
        ErrorCode::InvalidState
    );

    // A path in either repository finds its project, which the workspace holds.
    let found = open(&client, &web_src).await;
    assert!(!found.created);
    assert_eq!(found.workspace.parent_id, Some(ws.id));
    assert_eq!(found.workspace.roots, std::slice::from_ref(&web));

    // Removing a root takes the project out of the workspace; it stays a project.
    let without = remove_root(&client, ws.id, &web).await.unwrap();
    assert_eq!(without.roots, std::slice::from_ref(&api));
    assert_eq!(
        remote_code(remove_root(&client, ws.id, &web).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(remove_root(&client, project.id, &api).await),
        ErrorCode::InvalidState
    );
    let ungrouped = open(&client, &web_src).await;
    assert!(!ungrouped.created, "the project stays");
    assert_eq!(ungrouped.workspace.id, found.workspace.id);
    assert_eq!(ungrouped.workspace.parent_id, None);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_deepest_matching_root_wins() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let outer = mkdir(&base.join("mono"));
    let inner = mkdir(&outer.join("packages/ui"));
    let file_dir = mkdir(&inner.join("src"));
    let _wide = create_ws(&client, "wide", vec![outer]).await;
    let narrow = create_ws(&client, "narrow", vec![inner]).await;

    // Folders outside git belong to the deepest project folder that contains them.
    let found = open(&client, &file_dir).await;
    assert!(!found.created);
    assert_eq!(found.workspace.name, "ui");
    assert_eq!(found.workspace.parent_id, Some(narrow.id));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn roots_given_through_a_symlink_still_match() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let real = mkdir(&base.join("real"));
    let link = base.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    // The project of a folder given through a symlink is the project of the place it names.
    let ws = create_ws(&client, "linked", vec![link.clone()]).await;
    assert_eq!(ws.roots, std::slice::from_ref(&real));
    let found = open(&client, &mkdir(&real.join("sub"))).await;
    assert!(!found.created);
    assert_eq!(found.workspace.parent_id, Some(ws.id));
    assert_eq!(found.workspace.roots, [real]);
    t.stop().await;
}

// Close and recent ------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_project_keeps_it_in_recent_projects() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let one = mkdir(&base.join("one"));
    let two = mkdir(&base.join("two"));
    let three = mkdir(&base.join("three"));

    let ws_one = open(&client, &one).await.workspace;
    sleep(Duration::from_millis(5)).await;
    let ws_two = open(&client, &two).await.workspace;
    sleep(Duration::from_millis(5)).await;
    let ws_three = open(&client, &three).await.workspace;

    // Open projects are listed too, newest first; Home never is.
    let list = recent(&client, None).await.projects;
    assert_eq!(
        list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["three", "two", "one"]
    );
    assert_eq!(list[0].path, three);
    assert_eq!(list[0].workspace_id, Some(ws_three.id));

    // A project with live sessions stays.
    let session = create_in(&client, Some(ws_one.id), sh("exec cat")).await;
    assert_eq!(
        remote_code(close(&client, ws_one.id).await),
        ErrorCode::InvalidState
    );
    stop(&client, session.id).await.unwrap();

    // Renamed, then closed: gone from the workspaces, kept in recent under its name.
    client
        .call::<WorkspaceInfo>(Request::WorkspaceRename(WorkspaceRename {
            workspace_id: ws_one.id,
            name: "first one".into(),
        }))
        .await
        .unwrap();
    close(&client, ws_one.id).await.unwrap();
    wait_event(&mut events, "the close", |e| {
        matches!(e, Event::WorkspaceClosed { workspace_id } if *workspace_id == ws_one.id)
            .then_some(())
    })
    .await;
    assert!(workspaces(&client).await.iter().all(|w| w.id != ws_one.id));
    let list = recent(&client, None).await.projects;
    let closed = list.iter().find(|p| p.path == one).unwrap();
    assert_eq!(closed.name, "first one");
    assert_eq!(closed.workspace_id, None, "closed");
    assert_eq!(list.len(), 3);
    // Newest first by last use: "one" was last active when its session ran, after "three" opened.
    assert_eq!(list[0].path, one);

    // The limit applies (and is capped at 50).
    assert_eq!(recent(&client, Some(2)).await.projects.len(), 2);
    assert_eq!(recent(&client, Some(0)).await.projects.len(), 0);
    assert_eq!(recent(&client, Some(500)).await.projects.len(), 3);

    // Opening it again creates a new workspace under the remembered name.
    let again = open(&client, &one).await;
    assert!(again.created);
    assert_ne!(again.workspace.id, ws_one.id);
    assert_eq!(again.workspace.name, "first one");
    assert_eq!(
        recent(&client, None)
            .await
            .projects
            .iter()
            .find(|p| p.path == one)
            .unwrap()
            .workspace_id,
        Some(again.workspace.id)
    );

    // Closed projects survive a daemon restart; open ones come back as workspaces.
    close(&client, ws_two.id).await.unwrap();
    drop(client);
    let t = t.restart().await;
    let client = t.connect().await;
    let list = recent(&client, None).await.projects;
    assert_eq!(list.len(), 3);
    let by_path = |path: &Path| list.iter().find(|p| p.path == path).unwrap();
    assert_eq!(by_path(&two).workspace_id, None);
    assert_eq!(by_path(&two).name, "two");
    assert!(by_path(&one).workspace_id.is_some());
    assert!(by_path(&three).workspace_id.is_some());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_removes_ended_sessions_and_counts_follow_the_sessions() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let ws = open(&client, &mkdir(&base.join("proj"))).await.workspace;
    assert_eq!((ws.live_sessions, ws.total_sessions), (0, 0));

    let first = create_in(&client, Some(ws.id), sh("exec cat")).await;
    let counts = wait_event(&mut events, "one live session", |e| match e {
        Event::WorkspaceUpdated { workspace } if workspace.id == ws.id => {
            (workspace.total_sessions == 1).then(|| workspace.clone())
        }
        _ => None,
    })
    .await;
    assert_eq!((counts.live_sessions, counts.total_sessions), (1, 1));
    assert!(counts.last_active_ms >= ws.last_active_ms);

    let second = create_in(&client, Some(ws.id), sh("exec cat")).await;
    assert_eq!(workspace(&client, ws.id).await.live_sessions, 2);
    stop(&client, first.id).await.unwrap();
    let after_stop = wait_event(&mut events, "a stopped session", |e| match e {
        Event::WorkspaceUpdated { workspace }
            if workspace.id == ws.id
                && workspace.live_sessions == 1
                && workspace.total_sessions == 2 =>
        {
            Some(workspace.clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(after_stop.live_sessions, 1);
    // Counts are in the snapshot too.
    let snapshot: SessionSnapshot = client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap();
    let snap_ws = snapshot.workspaces.iter().find(|w| w.id == ws.id).unwrap();
    assert_eq!((snap_ws.live_sessions, snap_ws.total_sessions), (1, 2));

    stop(&client, second.id).await.unwrap();
    close(&client, ws.id).await.unwrap();
    let snapshot: SessionSnapshot = client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap();
    assert!(snapshot.workspaces.iter().all(|w| w.id != ws.id));
    assert!(snapshot.sessions.is_empty(), "ended sessions left with it");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_create_records_who_asked() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let plain = create_ws(&client, "plain", vec![]).await;
    assert_eq!(plain.created_by, CreatedBy::User);
    assert_eq!(plain.kind, WorkspaceKind::Workspace);
    let by_agent: WorkspaceInfo = client
        .call(Request::WorkspaceCreate(WorkspaceCreate {
            name: "by-agent".into(),
            paths: vec![],
            project_ids: Vec::new(),
            created_by: Some(CreatedBy::Agent),
        }))
        .await
        .unwrap();
    assert_eq!(by_agent.created_by, CreatedBy::Agent);
    assert!(by_agent.position > plain.position);
    t.stop().await;
}

// Order and pins --------------------------------------------------------------------------------

fn ids(list: &[WorkspaceInfo]) -> Vec<WorkspaceId> {
    list.iter().map(|w| w.id).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn move_renumbers_announces_and_survives_a_restart_together_with_pins() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let home = workspaces(&client).await[0].clone();
    let a = create_ws(&client, "a", vec![]).await;
    let b = create_ws(&client, "b", vec![]).await;
    let c = create_ws(&client, "c", vec![]).await;
    assert_eq!(ids(&workspaces(&client).await), [home.id, a.id, b.id, c.id]);

    // Before another one.
    let list = move_before(&client, c.id, Some(a.id))
        .await
        .unwrap()
        .workspaces;
    assert_eq!(ids(&list), [home.id, c.id, a.id, b.id]);
    assert_eq!(
        list.iter().map(|w| w.position).collect::<Vec<_>>(),
        [0, 1, 2, 3],
        "positions are renumbered"
    );
    assert_eq!(ids(&workspaces(&client).await), ids(&list));
    // Every workspace whose position changed is announced, the others are not.
    let mut announced = Vec::new();
    while announced.len() < 3 {
        let ws = wait_event(&mut events, "a moved workspace", |e| match e {
            Event::WorkspaceUpdated { workspace } if workspace.position <= 3 => {
                Some(workspace.clone())
            }
            _ => None,
        })
        .await;
        announced.push(ws);
    }
    let moved: Vec<_> = announced.iter().rev().take(3).map(|w| w.id).collect();
    assert!(moved.contains(&c.id) && moved.contains(&a.id) && moved.contains(&b.id));

    // To the end, and a no-op.
    let list = move_before(&client, home.id, None)
        .await
        .unwrap()
        .workspaces;
    assert_eq!(ids(&list), [c.id, a.id, b.id, home.id]);
    let same = move_before(&client, a.id, Some(a.id))
        .await
        .unwrap()
        .workspaces;
    assert_eq!(ids(&same), ids(&list));
    let already = move_before(&client, a.id, Some(b.id))
        .await
        .unwrap()
        .workspaces;
    assert_eq!(ids(&already), ids(&list), "already right before b");

    // Unknown ids.
    assert_eq!(
        remote_code(move_before(&client, WorkspaceId::new(), None).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(move_before(&client, a.id, Some(WorkspaceId::new())).await),
        ErrorCode::NotFound
    );

    // Pins are fields; the list order stays the manual one.
    let pinned = pin(&client, b.id, true).await;
    assert!(pinned.pinned);
    assert!(pin(&client, home.id, true).await.pinned);
    assert!(!pin(&client, home.id, false).await.pinned);
    wait_event(&mut events, "the pin", |e| match e {
        Event::WorkspaceUpdated { workspace } if workspace.id == b.id && workspace.pinned => {
            Some(())
        }
        _ => None,
    })
    .await;
    assert_eq!(
        remote_code(
            client
                .call::<WorkspaceInfo>(Request::WorkspacePin(WorkspacePin {
                    workspace_id: WorkspaceId::new(),
                    pinned: true
                }))
                .await
        ),
        ErrorCode::NotFound
    );
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    let list = workspaces(&client).await;
    assert_eq!(ids(&list), [c.id, a.id, b.id, home.id], "order survives");
    assert_eq!(
        list.iter().map(|w| w.pinned).collect::<Vec<_>>(),
        [false, false, true, false],
        "pins survive"
    );
    assert_eq!(names(&client).await, ["c", "a", "b", "Home"]);
    // New workspaces still go to the end.
    let d = create_ws(&client, "d", vec![]).await;
    assert_eq!(*ids(&workspaces(&client).await).last().unwrap(), d.id);
    t.stop().await;
}

// Activity --------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn output_bursts_update_last_active_at_most_once_per_interval() {
    const INTERVAL: Duration = Duration::from_secs(2);
    let t = TestDaemon::start_with("", |config| config.activity_interval = INTERVAL).await;
    let (client, mut events) = t.connect_subscribed().await;
    let home = workspaces(&client).await[0].clone();
    // Chatty: output every 50 ms.
    let session = create(&client, sh("while :; do echo x; sleep 0.05; done")).await;
    assert_eq!(session.workspace_id, home.id);

    let started = Instant::now();
    let mut stamps: Vec<u64> = Vec::new();
    while let Ok(received) = timeout(
        Duration::from_secs(5).saturating_sub(started.elapsed()),
        events.recv(),
    )
    .await
    {
        if let (_, Event::WorkspaceUpdated { workspace }) = received.unwrap()
            && workspace.id == home.id
            && stamps.last() != Some(&workspace.last_active_ms)
        {
            stamps.push(workspace.last_active_ms);
        }
    }
    // Session creation, then one update per interval: far fewer than the ~100 output bursts.
    assert!(
        (2..=4).contains(&stamps.len()),
        "{} updates in 5 s: {stamps:?}",
        stamps.len()
    );
    let interval_ms = u64::try_from(INTERVAL.as_millis()).unwrap();
    for pair in stamps.windows(2) {
        assert!(
            pair[1] - pair[0] >= interval_ms,
            "updates closer than the interval: {stamps:?}"
        );
    }
    assert!(stamps[0] >= home.last_active_ms);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn input_counts_as_activity() {
    let t = TestDaemon::start_with("", |config| {
        config.activity_interval = Duration::from_millis(300);
    })
    .await;
    let client = t.connect().await;
    let home = workspaces(&client).await[0].clone();
    // Silent after "R": no echo, no output, so only input can move the clock.
    let session = create(&client, sh("stty -echo; printf R; sleep 60")).await;
    let mut view = View::attach(&client, terminal_of(&session), None).await;
    view.wait_for("the marker", |s| s.contains("R")).await;
    sleep(Duration::from_millis(1500)).await;
    let before = workspace(&client, home.id).await.last_active_ms;

    client
        .send_input(terminal_of(&session), b"x")
        .await
        .unwrap();
    let moved = timeout(WAIT, async {
        loop {
            let now = workspace(&client, home.id).await.last_active_ms;
            if now > before {
                return now;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(moved.is_ok(), "typing never moved last_active_ms");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn last_active_survives_a_restart() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let ws = create_ws(&client, "w", vec![]).await;
    let session = create_in(&client, Some(ws.id), sh("exit 0")).await;
    wait_ended(&client, session.id).await;
    let stamped = workspace(&client, ws.id).await;
    assert!(stamped.last_active_ms >= ws.last_active_ms);
    assert_eq!((stamped.live_sessions, stamped.total_sessions), (0, 1));
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    let back = workspace(&client, ws.id).await;
    assert_eq!(back.last_active_ms, stamped.last_active_ms);
    assert_eq!((back.live_sessions, back.total_sessions), (0, 1));
    t.stop().await;
}

async fn wait_ended(client: &Client, id: hutan_core::SessionId) {
    let ended = timeout(WAIT, async {
        loop {
            let snapshot: SessionSnapshot = client
                .call(Request::SessionSnapshot(Empty {}))
                .await
                .unwrap();
            if snapshot
                .sessions
                .iter()
                .any(|s| s.id == id && !s.state.is_live())
            {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    assert!(ended.is_ok(), "session {id} never ended");
}

// Migration -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_v1_database_comes_up_with_its_default_workspace_as_home() {
    let dir = tempfile::tempdir().unwrap();
    let paths = hutan_daemon::Paths::in_home(dir.path());
    let default = WorkspaceId::new();
    let session = hutan_core::SessionId::new();
    {
        let conn = rusqlite::Connection::open(&paths.database).unwrap();
        conn.execute_batch(hutan_storage::MIGRATIONS[0].sql)
            .unwrap();
        conn.execute(
            "INSERT INTO app_meta (key, value) VALUES ('schema_version', '1'), \
             ('default_workspace_id', ?1)",
            [default.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workspaces (id, name, roots_json, created_at_ms, position) \
             VALUES (?1, 'default', '[]', 100, 0)",
            [default.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, kind, title, title_locked, spec_json, \
                 state_json, created_at_ms, ended_at_ms) \
             VALUES (?1, ?2, 'terminal', 'old', 1, '{}', '{\"state\":\"stopped\"}', 150, 160)",
            [session.to_string(), default.to_string()],
        )
        .unwrap();
    }
    let daemon = hutan_daemon::start(hutan_daemon::DaemonConfig::new(paths.clone()))
        .await
        .unwrap();
    let t = TestDaemon {
        _dir: dir,
        paths,
        daemon,
    };
    let client = t.connect().await;
    let list = workspaces(&client).await;
    assert_eq!(list.len(), 1, "no second Home was invented");
    assert_eq!(list[0].id, default);
    assert_eq!(list[0].kind, WorkspaceKind::Home);
    assert_eq!(list[0].name, "Home");
    assert_eq!(list[0].roots, [canon(&t.paths.default_home_folder)]);
    assert_eq!((list[0].live_sessions, list[0].total_sessions), (0, 1));
    assert_eq!(list[0].last_active_ms, 160, "from its newest session");
    // Sessions without a workspace still land in it.
    let new = create(&client, sh("exit 0")).await;
    assert_eq!(new.workspace_id, default);
    t.stop().await;
}
