//! Durable state end to end (roadmap M3.1, M3.2): the daemon runs in-process on a temporary
//! `HUTAN_HOME`, is stopped, and a new daemon started on the same directory must bring back
//! the workspaces and sessions. Real shells run in real PTYs.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use hutan_client::Client;
use hutan_core::{SessionId, SessionKind, SessionState, WorkspaceId};
use hutan_daemon::{DaemonConfig, DaemonError, Paths};
use hutan_protocol::messages::{
    DaemonShutdown, Empty, Request, SessionInfo, SessionRef, SessionRename, SessionResult,
    SessionSnapshot, TerminalSpec, WorkspaceCreate, WorkspaceInfo,
};
use hutan_storage::{Migration, SessionRow, Storage, WorkspaceKind};
use tokio::time::{sleep, timeout};

async fn snapshot(client: &Client) -> SessionSnapshot {
    client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap()
}

fn find(snapshot: &SessionSnapshot, id: SessionId) -> SessionInfo {
    snapshot
        .sessions
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("session {id} missing after restart"))
        .clone()
}

async fn workspace(client: &Client, name: &str, roots: Vec<PathBuf>) -> WorkspaceInfo {
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

async fn rename(client: &Client, id: SessionId, title: &str) {
    client
        .call::<SessionResult>(Request::SessionRename(SessionRename {
            session_id: id,
            title: title.into(),
        }))
        .await
        .unwrap();
}

/// Polls the registry until `pred` holds for the session.
async fn wait_session(
    client: &Client,
    id: SessionId,
    what: &str,
    pred: impl Fn(&SessionInfo) -> bool,
) -> SessionInfo {
    let found = timeout(WAIT, async {
        loop {
            let snap = snapshot(client).await;
            if let Some(session) = snap.sessions.into_iter().find(|s| s.id == id)
                && pred(&session)
            {
                return session;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    found.unwrap_or_else(|_| panic!("session {id}: timed out waiting for {what}"))
}

async fn exited(client: &Client, spec: TerminalSpec, workspace: &WorkspaceInfo) -> SessionInfo {
    let session = create_in(client, Some(workspace.id), spec).await;
    wait_session(client, session.id, "the exit", |s| {
        matches!(s.state, SessionState::Exited { .. })
    })
    .await
}

/// Retries `check` against the database (read through a second handle while the daemon runs)
/// until it holds.
async fn eventually_stored(paths: &Paths, what: &str, check: impl Fn(&[SessionRow]) -> bool) {
    let outcome = timeout(WAIT, async {
        loop {
            let storage = Storage::open(&paths.database).unwrap();
            let rows = storage.sessions().list().unwrap();
            storage.close();
            if check(&rows) {
                return;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(outcome.is_ok(), "{what}: never reached the database");
}

fn zsh_available() -> bool {
    Path::new("/bin/zsh").exists()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_restart_restores_workspaces_and_respawns_live_sessions() {
    if !zsh_available() {
        eprintln!("skipping: zsh is not installed");
        return;
    }
    let t = TestDaemon::start().await;
    let first_epoch = t.daemon.epoch();
    let client = t.connect().await;

    // Canonical, so the shell's `pwd` and the OSC 7 it reports match what the test expects.
    let work = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(work.path()).unwrap();
    let sub = root.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let zsh_home = tempfile::tempdir().unwrap();

    let proj = workspace(&client, "proj", vec![root.clone()]).await;
    let scratch = workspace(&client, "scratch", vec![]).await;

    // A: zsh with the installed shell integration, moved into `sub`, named by the user.
    let mut zsh = spec(&["/bin/zsh", "-i"]);
    zsh.cwd = Some(root.clone());
    zsh.size = Some(size(400, 10));
    zsh.env.insert(
        "HOME".into(),
        zsh_home.path().to_string_lossy().into_owned(),
    );
    let a = create_in(&client, Some(proj.id), zsh).await;
    rename(&client, a.id, "build").await;
    let mut view = View::attach(&client, terminal_of(&a), None).await;
    view.wait_for("a prompt", |s| s.contains("%")).await;
    client
        .send_input(terminal_of(&a), b"cd sub\r")
        .await
        .unwrap();
    wait_session(&client, a.id, "the shell reporting its cwd", |s| {
        s.cwd.as_deref() == Some(sub.as_path())
    })
    .await;

    // B: `cat`, default workspace and title. C: ended by itself. D: stopped by the user.
    let b = create(&client, spec(&["/bin/cat"])).await;
    let c = exited(&client, sh("exit 3"), &scratch).await;
    let d = create(&client, spec(&["/bin/cat"])).await;
    stop(&client, d.id).await.unwrap();

    let before = snapshot(&client).await;
    assert_eq!(before.sessions.len(), 4);
    drop(view);
    drop(client);

    let t = t.restart().await;
    let second_epoch = t.daemon.epoch();
    assert_ne!(first_epoch, second_epoch, "a restart is a new epoch");
    let client = t.connect().await;
    let after = snapshot(&client).await;

    // Same workspaces, same session ids, titles, and workspaces.
    assert_eq!(after.workspaces, before.workspaces);
    let mut ids: Vec<_> = after.sessions.iter().map(|s| s.id).collect();
    let mut expected: Vec<_> = before.sessions.iter().map(|s| s.id).collect();
    ids.sort();
    expected.sort();
    assert_eq!(ids, expected);
    for old in &before.sessions {
        let new = find(&after, old.id);
        assert_eq!(new.title, old.title, "{}", old.id);
        assert_eq!(new.workspace_id, old.workspace_id, "{}", old.id);
        assert_eq!(new.created_at_ms, old.created_at_ms, "{}", old.id);
        assert_eq!(new.kind, SessionKind::Terminal);
    }
    assert_eq!(find(&after, a.id).title, "build");
    assert_eq!(find(&after, a.id).workspace_id, proj.id);

    // Live ones run again in new terminals, in their last cwd; they are not "restored" ended ones.
    let a2 = find(&after, a.id);
    assert_eq!(a2.state, SessionState::Running);
    assert!(a2.terminal_id.is_some());
    assert!(!a2.restored);
    assert_eq!(a2.cwd.as_deref(), Some(sub.as_path()));
    let mut view = View::attach(&client, terminal_of(&a2), None).await;
    client.send_input(terminal_of(&a2), b"pwd\r").await.unwrap();
    let sub_text = sub.to_string_lossy().into_owned();
    view.wait_for("pwd printing the saved cwd", |s| {
        s.text().lines().any(|l| l.trim() == sub_text)
    })
    .await;

    let b2 = find(&after, b.id);
    assert_eq!(b2.state, SessionState::Running);
    assert!(!b2.restored);
    let mut view_b = View::attach(&client, terminal_of(&b2), None).await;
    client
        .send_input(terminal_of(&b2), b"alive again\r")
        .await
        .unwrap();
    view_b
        .wait_for("cat echoing", |s| s.contains("alive again"))
        .await;

    // Ended ones stay ended and say their screen is gone.
    let c2 = find(&after, c.id);
    assert_eq!(
        c2.state,
        SessionState::Exited {
            code: Some(3),
            signal: None
        }
    );
    assert!(c2.restored);
    assert_eq!(c2.terminal_id, None);
    assert_eq!(c2.workspace_id, scratch.id);
    let d2 = find(&after, d.id);
    assert_eq!(d2.state, SessionState::Stopped);
    assert!(d2.restored);
    assert_eq!(d2.terminal_id, None);

    // The epoch history records both runs.
    drop((view, view_b, client));
    t.daemon.shutdown();
    timeout(WAIT, t.daemon.wait()).await.unwrap().unwrap();
    let storage = Storage::open(t.paths.database.clone()).unwrap();
    let epochs: Vec<_> = storage
        .app_meta()
        .epoch_history()
        .unwrap()
        .into_iter()
        .map(|e| e.epoch)
        .collect();
    assert_eq!(epochs, [first_epoch, second_epoch]);
}

#[tokio::test(flavor = "multi_thread")]
async fn restoring_in_stopped_mode_lists_live_sessions_as_stopped_until_restarted() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let live = create(&client, spec(&["/bin/cat"])).await;
    let gone = {
        let ws = workspace(&client, "w", vec![]).await;
        exited(&client, sh("exit 0"), &ws).await
    };
    drop(client);

    std::fs::write(
        &t.paths.config_file,
        "[daemon]\nrestore_sessions = \"stopped\"\n",
    )
    .unwrap();
    let t = t.restart().await;
    let client = t.connect().await;
    let snap = snapshot(&client).await;

    let live2 = find(&snap, live.id);
    assert_eq!(live2.state, SessionState::Stopped);
    assert!(live2.restored);
    assert_eq!(live2.terminal_id, None);
    assert!(find(&snap, gone.id).restored);

    // The restart action brings it back as a fresh, unrestored session with the same id.
    let restarted = client
        .call::<SessionResult>(Request::SessionRestart(SessionRef {
            session_id: live.id,
        }))
        .await
        .unwrap()
        .session;
    assert_eq!(restarted.id, live.id);
    assert_eq!(restarted.state, SessionState::Running);
    assert!(!restarted.restored);
    assert!(restarted.terminal_id.is_some());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shutdown_request_keeps_stopped_sessions_for_the_next_start_unless_forgetting() {
    for forget in [false, true] {
        let t = TestDaemon::start().await;
        let client = t.connect().await;
        let session = create(&client, spec(&["/bin/cat"])).await;
        client
            .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
                stop_sessions: true,
                forget,
            }))
            .await
            .unwrap();
        drop(client);

        let t = t.restart_after_shutdown_request().await;
        let client = t.connect().await;
        let restored = find(&snapshot(&client).await, session.id);
        if forget {
            assert_eq!(restored.state, SessionState::Stopped, "forget");
            assert!(restored.restored);
            assert_eq!(restored.terminal_id, None);
        } else {
            assert_eq!(restored.state, SessionState::Running, "keep");
            assert!(!restored.restored);
            assert!(restored.terminal_id.is_some());
        }

        // A second restart changes nothing for the ended ones; live ones keep coming back.
        let t = t.restart().await;
        let client = t.connect().await;
        let again = find(&snapshot(&client).await, session.id);
        assert_eq!(again.state, restored.state, "forget={forget}");
        t.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn user_changes_survive_a_restart() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let keep = workspace(&client, "keep", vec![PathBuf::from("/tmp")]).await;
    let drop_me = workspace(&client, "temporary", vec![]).await;
    client
        .call::<Empty>(Request::WorkspaceRename(
            hutan_protocol::messages::WorkspaceRename {
                workspace_id: keep.id,
                name: "kept".into(),
            },
        ))
        .await
        .unwrap();
    let in_dropped = exited(&client, sh("exit 0"), &drop_me).await;
    client
        .call::<Empty>(Request::WorkspaceClose(
            hutan_protocol::messages::WorkspaceRef {
                workspace_id: drop_me.id,
            },
        ))
        .await
        .unwrap();
    let named = create_in(&client, Some(keep.id), spec(&["/bin/cat"])).await;
    rename(&client, named.id, "mine").await;
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    let snap = snapshot(&client).await;

    let names: Vec<_> = snap.workspaces.iter().map(|w| w.name.as_str()).collect();
    // Creation order, the closed one gone; the folder of `kept` is a project in it.
    assert_eq!(names, ["Home", "kept", "tmp"]);
    // Folders are stored canonical: `/tmp` is `/private/tmp` on macOS.
    let tmp = std::fs::canonicalize("/tmp").unwrap();
    assert_eq!(snap.workspaces[1].roots, [tmp]);
    assert_eq!(snap.workspaces[2].parent_id, Some(keep.id));
    assert!(snap.sessions.iter().all(|s| s.id != in_dropped.id));
    assert_eq!(find(&snap, named.id).title, "mine");

    // The name the user chose is still locked against program titles.
    client
        .send_input(
            terminal_of(&find(&snap, named.id)),
            b"\x1b]2;from-program\x07\r",
        )
        .await
        .unwrap();
    sleep(Duration::from_millis(500)).await;
    assert_eq!(find(&snapshot(&client).await, named.id).title, "mine");

    // Sessions created with no workspace still land in the same default workspace.
    let fresh = create(&client, spec(&["/bin/cat"])).await;
    assert_eq!(fresh.workspace_id, snap.workspaces[0].id);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cwd_and_title_changes_reach_the_database_without_a_shutdown() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(&client, spec(&["/bin/cat"])).await;
    // `cat` echoes the escape sequences back, so the terminal sees an OSC 2 title and an OSC 7 cwd.
    client
        .send_input(
            terminal_of(&session),
            b"\x1b]2;from-program\x07\r\x1b]7;file://h/tmp/hutan%20x\x07\r",
        )
        .await
        .unwrap();
    eventually_stored(&t.paths, "the title and cwd of the session", |rows| {
        rows.iter().any(|r| {
            r.id == session.id
                && r.title == "from-program"
                && r.cwd.as_deref() == Some(Path::new("/tmp/hutan x"))
                && r.state == SessionState::Running
        })
    })
    .await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_references_replace_the_spec_command_and_unusable_rows_fail_cleanly() {
    // First run only creates the database and the default workspace.
    let t = TestDaemon::start().await;
    let paths = t.paths.clone();
    t.daemon.shutdown();
    timeout(WAIT, t.daemon.wait()).await.unwrap().unwrap();

    let storage = Storage::open(&paths.database).unwrap();
    let default_workspace: WorkspaceId = storage
        .workspaces()
        .list()
        .unwrap()
        .into_iter()
        .find(|w| w.kind == WorkspaceKind::Home)
        .expect("the Home workspace is stored")
        .id;
    let cat_spec = serde_json::to_string(&TerminalSpec {
        command: vec!["/bin/cat".into()],
        env: [("PS1".to_owned(), "$ ".to_owned())].into(),
        ..TerminalSpec::default()
    })
    .unwrap();
    let row = |title: &str, spec_json: &str, resume: Option<&str>, cwd: Option<&str>| SessionRow {
        id: SessionId::new(),
        workspace_id: default_workspace,
        kind: SessionKind::Terminal,
        title: title.to_owned(),
        title_locked: true,
        spec_json: spec_json.to_owned(),
        cwd: cwd.map(PathBuf::from),
        state: SessionState::Running,
        created_at_ms: 1_000,
        ended_at_ms: None,
        resume_ref_json: resume.map(str::to_owned),
    };
    let resumed = row(
        "agent",
        &cat_spec,
        Some(r#"{"argv":["/bin/sh","-c","echo RESUMED-$HUTAN_SESSION_ID; exec cat"]}"#),
        None,
    );
    let broken = row("broken", "this is not json", None, None);
    let vanished = row("vanished", &cat_spec, None, Some("/definitely/not/here"));
    for r in [&resumed, &broken, &vanished] {
        storage.sessions().upsert(r.clone()).unwrap();
    }
    storage.close();

    let daemon = hutan_daemon::start(DaemonConfig::new(paths.clone()))
        .await
        .unwrap();
    let t = TestDaemon {
        _dir: t._dir,
        paths,
        daemon,
    };
    let client = t.connect().await;
    let snap = snapshot(&client).await;

    let resumed2 = find(&snap, resumed.id);
    assert_eq!(resumed2.state, SessionState::Running);
    assert_eq!(
        resumed2.command,
        ["/bin/cat"],
        "the spec stays what restart uses"
    );
    let mut view = View::attach(&client, terminal_of(&resumed2), None).await;
    let marker = format!("RESUMED-{}", resumed.id);
    view.wait_for("the resume command running", |s| s.contains(&marker))
        .await;

    let broken2 = find(&snap, broken.id);
    assert!(
        matches!(&broken2.state, SessionState::Failed { reason } if reason.contains("unreadable")),
        "{:?}",
        broken2.state
    );
    assert!(broken2.restored);

    // A directory that no longer exists does not keep the session from starting.
    let vanished2 = find(&snap, vanished.id);
    assert_eq!(vanished2.state, SessionState::Running);
    assert_eq!(vanished2.cwd, None);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_refuses_to_start_on_a_database_it_cannot_use() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_home(dir.path());

    // A database written by a newer build: every migration this build knows, and one more.
    let newest = hutan_storage::MIGRATIONS
        .last()
        .map_or(0, |migration| migration.version);
    let newer = Migration {
        version: newest + 1,
        name: "from the future",
        sql: "CREATE TABLE future (x INTEGER NOT NULL);",
    };
    let mut written = hutan_storage::MIGRATIONS.to_vec();
    written.push(newer);
    Storage::open_with_migrations(&paths.database, &written)
        .unwrap()
        .close();
    let error = hutan_daemon::start(DaemonConfig::new(paths.clone()))
        .await
        .unwrap_err();
    assert!(matches!(error, DaemonError::Storage { .. }), "{error}");
    let message = error.to_string();
    assert!(message.contains("hutan.db"), "{message}");
    assert!(message.contains("newer"), "{message}");
    assert!(!paths.socket.exists(), "no socket must be left behind");

    // A config the daemon cannot read is refused the same way, so a typo never changes what
    // happens to the user's sessions.
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_home(dir.path());
    std::fs::write(
        &paths.config_file,
        "[daemon]\nrestore_sessions = \"maybe\"\n",
    )
    .unwrap();
    let error = hutan_daemon::start(DaemonConfig::new(paths.clone()))
        .await
        .unwrap_err();
    assert!(matches!(error, DaemonError::Config { .. }), "{error}");
    assert!(error.to_string().contains("daemon.toml"), "{error}");
}
