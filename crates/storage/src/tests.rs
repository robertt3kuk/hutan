#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use hutan_core::{
    AfterPolicy, DeliveryActor, DeliveryRecord, DeliveryState, Epoch, SessionId, SessionKind,
    SessionState, Task, TaskId, TaskPlacement, TaskPlacementKind, TaskRun, TaskState, WorkspaceId,
};
use tempfile::TempDir;

use crate::migrate::Migration;
use crate::sql::Fault;
use crate::{
    ApprovalOutcome, ApprovalRow, ChatItemRow, ChatTurnRow, ChatTurnStatus, ClosedSession,
    CreatedBy, ProviderRow, RecentProject, SessionRow, Storage, StorageError, WorkspaceKind,
    WorkspaceRow, WorktreeRow, Write, migrate,
};
use hutan_core::ApprovalId;

fn db(dir: &TempDir) -> PathBuf {
    dir.path().join("hutan.db")
}

/// The schema version a fully migrated database records: the last migration's.
fn newest() -> u32 {
    crate::MIGRATIONS.last().map_or(0, |m| m.version)
}

fn workspace(name: &str, position: i64) -> WorkspaceRow {
    WorkspaceRow {
        id: WorkspaceId::new(),
        name: name.to_owned(),
        roots: vec![PathBuf::from("/work/a"), PathBuf::from("/work/b")],
        kind: WorkspaceKind::Project,
        pinned: false,
        created_by: CreatedBy::User,
        created_at_ms: 1_000 + u64::try_from(position).unwrap(),
        last_active_ms: 2_000 + u64::try_from(position).unwrap(),
        position,
        common_dir: None,
        parent_id: None,
    }
}

fn session(workspace: WorkspaceId, title: &str) -> SessionRow {
    SessionRow {
        id: SessionId::new(),
        workspace_id: workspace,
        kind: SessionKind::Terminal,
        title: title.to_owned(),
        title_locked: false,
        spec_json: r#"{"command":["zsh"]}"#.to_owned(),
        cwd: Some(PathBuf::from("/tmp")),
        state: SessionState::Running,
        created_at_ms: 5_000,
        ended_at_ms: None,
        resume_ref_json: None,
    }
}

/// Schemas before version 8 have no identity or parent column: the rows of those tests are
/// written and read with the statements of the time.
fn seed_legacy_workspace(storage: &Storage, ws: &WorkspaceRow) {
    let ws = ws.clone();
    storage
        .read(move |conn| {
            conn.execute(
                "INSERT INTO workspaces (id, name, roots_json, kind, pinned, created_by, \
                     created_at_ms, last_active_ms, position) \
                 VALUES (?1, ?2, ?3, 'project', ?4, 'user', ?5, ?6, ?7)",
                rusqlite::params![
                    ws.id.to_string(),
                    ws.name,
                    serde_json::to_string(&ws.roots).unwrap(),
                    ws.pinned,
                    i64::try_from(ws.created_at_ms).unwrap(),
                    i64::try_from(ws.last_active_ms).unwrap(),
                    ws.position
                ],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
}

/// Schemas before version 9 have no delivery columns.
fn seed_legacy_task(storage: &Storage, task: &Task) {
    let task = task.clone();
    storage
        .read(move |conn| {
            conn.execute(
                "INSERT INTO tasks (id, workspace_id, title, prompt, state, placement_json, \
                     run_json, auto_start, created_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, 'done', ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    task.id.to_string(),
                    task.workspace_id.to_string(),
                    task.title,
                    task.prompt,
                    serde_json::to_string(&task.placement).unwrap(),
                    task.run.as_ref().map(|r| serde_json::to_string(r).unwrap()),
                    task.auto_start,
                    i64::try_from(task.created_at_ms).unwrap(),
                ],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();
}

fn legacy_workspaces(storage: &Storage) -> Vec<WorkspaceRow> {
    storage
        .read(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, name, roots_json, pinned, created_at_ms, last_active_ms, position \
                     FROM workspaces ORDER BY position",
                )
                .unwrap();
            let rows = stmt
                .query_map([], |row| {
                    let id: String = row.get(0)?;
                    let roots: String = row.get(2)?;
                    Ok(WorkspaceRow {
                        id: id.parse().unwrap(),
                        name: row.get(1)?,
                        roots: serde_json::from_str::<Vec<PathBuf>>(&roots).unwrap(),
                        kind: WorkspaceKind::Project,
                        pinned: row.get(3)?,
                        created_by: CreatedBy::User,
                        created_at_ms: row.get::<_, i64>(4)?.try_into().unwrap(),
                        last_active_ms: row.get::<_, i64>(5)?.try_into().unwrap(),
                        position: row.get(6)?,
                        common_dir: None,
                        parent_id: None,
                    })
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            Ok(rows)
        })
        .unwrap()
}

const V1: Migration = migrate::MIGRATIONS[0];
const V2: Migration = migrate::MIGRATIONS[1];
const V3_OK: Migration = Migration {
    version: 3,
    name: "extra",
    sql: "CREATE TABLE extra (x INTEGER NOT NULL);",
};
const V3_BROKEN: Migration = Migration {
    version: 3,
    name: "broken",
    // The first statement succeeds; the second must roll it back.
    sql: "CREATE TABLE extra (x INTEGER NOT NULL); INSERT INTO no_such_table VALUES (1);",
};

fn table_exists(path: &Path, table: &str) -> bool {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )
    .unwrap()
}

fn integrity_ok(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    let verdict: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(verdict, "ok");
}

fn backups(dir: &TempDir) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir.path().join("backups")) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
    files.sort();
    files
}

#[test]
fn opens_an_empty_database_and_migrates_it() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();

    assert_eq!(storage.app_meta().schema_version().unwrap(), newest());
    assert!(storage.workspaces().list().unwrap().is_empty());
    assert!(storage.sessions().list().unwrap().is_empty());
    assert!(backups(&dir).is_empty(), "a fresh database needs no backup");
    storage.close();

    for table in ["app_meta", "workspaces", "sessions", "recent_projects"] {
        assert!(table_exists(&db(&dir), table), "{table}");
    }
    let conn = rusqlite::Connection::open(db(&dir)).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    use std::os::unix::fs::PermissionsExt;
    let file_mode = fs::metadata(db(&dir)).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600);
}

#[test]
fn pragmas_apply_to_the_writer_connection() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let (foreign_keys, synchronous): (i64, i64) = storage
        .read(|conn| {
            Ok((
                conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                    .unwrap(),
                conn.query_row("PRAGMA synchronous", [], |r| r.get(0))
                    .unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(foreign_keys, 1);
    assert_eq!(synchronous, 1, "NORMAL");
}

#[test]
fn reopening_is_idempotent_and_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    let sess = session(ws.id, "shell");
    {
        let storage = Storage::open(db(&dir)).unwrap();
        storage.workspaces().insert(ws.clone()).unwrap();
        storage.sessions().upsert(sess.clone()).unwrap();
        storage.close();
    }
    for _ in 0..2 {
        let storage = Storage::open(db(&dir)).unwrap();
        assert_eq!(storage.app_meta().schema_version().unwrap(), newest());
        assert_eq!(
            storage.workspaces().list().unwrap(),
            std::slice::from_ref(&ws)
        );
        assert_eq!(
            storage.sessions().list().unwrap(),
            std::slice::from_ref(&sess)
        );
        storage.close();
    }
    assert!(
        backups(&dir).is_empty(),
        "nothing was pending, nothing to back up"
    );
}

#[test]
fn a_backup_is_made_before_a_pending_migration() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &[V1, V2]).unwrap();
        seed_legacy_workspace(&storage, &ws);
        storage.close();
    }
    let storage = Storage::open_with_migrations(db(&dir), &[V1, V2, V3_OK]).unwrap();
    assert_eq!(storage.app_meta().schema_version().unwrap(), 3);
    storage.close();
    assert!(table_exists(&db(&dir), "extra"));

    let files = backups(&dir);
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("hutan-") && name.ends_with("-v2.db"),
        "{name}"
    );
    // The backup is the pre-migration database: version 1, data present, no `extra` table.
    assert!(!table_exists(&files[0], "extra"));
    let old = Storage::open_with_migrations(&files[0], &[V1, V2]).unwrap();
    assert_eq!(legacy_workspaces(&old), [ws]);
    old.close();
}

#[test]
fn a_failed_migration_keeps_the_original_and_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &[V1, V2]).unwrap();
        seed_legacy_workspace(&storage, &ws);
        storage.close();
    }

    let error = Storage::open_with_migrations(db(&dir), &[V1, V2, V3_BROKEN]).unwrap_err();
    let StorageError::Migration {
        version,
        name,
        backup,
        ..
    } = &error
    else {
        panic!("expected a migration error, got {error}");
    };
    assert_eq!((*version, name.as_str()), (3, "broken"));
    assert!(backup.as_ref().is_some_and(|b| b.exists()));
    let text = error.to_string();
    assert!(text.contains("migration 3 (broken) failed"), "{text}");
    assert!(text.contains("left unchanged"), "{text}");

    // Nothing of the failed migration is visible; the data and version are as before.
    assert!(!table_exists(&db(&dir), "extra"));
    let storage = Storage::open_with_migrations(db(&dir), &[V1, V2]).unwrap();
    assert_eq!(storage.app_meta().schema_version().unwrap(), 2);
    assert_eq!(legacy_workspaces(&storage), [ws]);
    storage.close();
    integrity_ok(&db(&dir));
}

#[test]
fn a_database_from_a_newer_build_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    Storage::open_with_migrations(db(&dir), &[V1, V2, V3_OK])
        .unwrap()
        .close();
    let error = Storage::open_with_migrations(db(&dir), &[V1, V2]).unwrap_err();
    assert!(
        matches!(
            error,
            StorageError::SchemaTooNew {
                found: 3,
                supported: 2,
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn only_the_newest_five_backups_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    Storage::open_with_migrations(db(&dir), &[V1, V2])
        .unwrap()
        .close();
    let backup_dir = dir.path().join("backups");
    fs::create_dir_all(&backup_dir).unwrap();
    for stamp in 1..=7_u64 {
        fs::write(backup_dir.join(format!("hutan-{stamp:013}-v1.db")), b"old").unwrap();
    }
    fs::write(backup_dir.join("notes.txt"), b"not a backup").unwrap();

    Storage::open_with_migrations(db(&dir), &[V1, V2, V3_OK])
        .unwrap()
        .close();

    let names: Vec<String> = backups(&dir)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names.iter().filter(|n| n.starts_with("hutan-")).count(),
        5,
        "{names:?}"
    );
    assert!(
        names.contains(&"notes.txt".to_owned()),
        "foreign files stay: {names:?}"
    );
    // The oldest fakes went; the new backup (current time) is the newest.
    assert!(!names.contains(&"hutan-0000000000001-v1.db".to_owned()));
    assert!(names.iter().any(|n| n.as_str() > "hutan-0001000000000"));
}

#[test]
fn workspaces_and_sessions_roundtrip_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let second = workspace("second", 2);
    let first = workspace("first", 1);
    storage.workspaces().insert(second.clone()).unwrap();
    storage.workspaces().insert(first.clone()).unwrap();
    assert_eq!(
        storage.workspaces().list().unwrap(),
        [first.clone(), second.clone()]
    );

    storage
        .workspaces()
        .rename(first.id, "renamed".to_owned())
        .unwrap();
    let renamed = storage.workspaces().list().unwrap();
    assert_eq!(renamed[0].name, "renamed");
    assert_eq!(renamed[0].roots, first.roots);

    let mut a = session(first.id, "a");
    a.created_at_ms = 10;
    let mut b = session(second.id, "b");
    b.created_at_ms = 20;
    b.state = SessionState::Exited {
        code: Some(3),
        signal: None,
    };
    b.ended_at_ms = Some(30);
    b.title_locked = true;
    b.cwd = None;
    b.resume_ref_json = Some(r#"{"argv":["codex","resume","x"]}"#.to_owned());
    storage.sessions().upsert(b.clone()).unwrap();
    storage.sessions().upsert(a.clone()).unwrap();
    assert_eq!(storage.sessions().list().unwrap(), [a.clone(), b.clone()]);
    assert_eq!(
        storage.sessions().list_in_workspace(second.id).unwrap(),
        [b.clone()]
    );

    // An upsert replaces the mutable columns but never the creation time.
    let mut changed = a.clone();
    changed.title = "a2".to_owned();
    changed.state = SessionState::Stopped;
    changed.created_at_ms = 999;
    storage.sessions().upsert(changed).unwrap();
    let stored = &storage.sessions().list_in_workspace(first.id).unwrap()[0];
    assert_eq!(stored.title, "a2");
    assert_eq!(stored.state, SessionState::Stopped);
    assert_eq!(stored.created_at_ms, 10);

    // Deleting a workspace takes its sessions with it.
    storage.workspaces().delete(second.id).unwrap();
    assert_eq!(storage.workspaces().list().unwrap().len(), 1);
    let left = storage.sessions().list().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, a.id);

    storage.sessions().delete(a.id).unwrap();
    assert!(storage.sessions().list().unwrap().is_empty());
}

#[test]
fn equal_timestamp_sessions_keep_creation_order_after_updates_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let ws = workspace("project", 0);
    storage.workspaces().insert(ws.clone()).unwrap();
    let mut first = session(ws.id, "first assistant");
    let mut second = session(ws.id, "second assistant");
    // ULID randomness within a millisecond may put a later session before the first one.
    first.id = "00000000000000000000000002".parse().unwrap();
    second.id = "00000000000000000000000001".parse().unwrap();
    storage.sessions().upsert(first.clone()).unwrap();
    storage.sessions().upsert(second.clone()).unwrap();
    assert_eq!(
        storage.sessions().list_in_workspace(ws.id).unwrap(),
        [first.clone(), second.clone()]
    );
    first.title = "renamed first assistant".into();
    storage.sessions().upsert(first.clone()).unwrap();
    storage.close();
    let reopened = Storage::open(db(&dir)).unwrap();
    assert_eq!(reopened.sessions().list().unwrap(), [first, second]);
    reopened.close();
}

#[test]
fn a_closed_session_is_kept_but_not_listed_until_it_is_reopened() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let ws = workspace("project", 0);
    storage.workspaces().insert(ws.clone()).unwrap();
    let mut open = session(ws.id, "open");
    open.created_at_ms = 10;
    let mut gone = session(ws.id, "gone");
    gone.created_at_ms = 20;
    storage.sessions().upsert(open.clone()).unwrap();
    storage.sessions().upsert(gone.clone()).unwrap();

    storage.sessions().set_closed(gone.id, Some(500)).unwrap();
    assert_eq!(storage.sessions().list().unwrap(), [open.clone()]);
    assert_eq!(
        storage.sessions().list_in_workspace(ws.id).unwrap(),
        [open.clone()]
    );
    assert_eq!(
        storage.sessions().list_closed().unwrap(),
        [ClosedSession {
            session: gone.clone(),
            closed_at_ms: 500
        }]
    );

    // Writing the row again (a late title change) does not reopen it.
    gone.title = "gone, renamed".into();
    storage.sessions().upsert(gone.clone()).unwrap();
    assert_eq!(storage.sessions().list().unwrap(), [open.clone()]);
    storage.close();

    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.sessions().list().unwrap(), [open.clone()]);
    assert_eq!(storage.sessions().list_closed().unwrap()[0].session, gone);
    storage.sessions().set_closed(gone.id, None).unwrap();
    assert_eq!(storage.sessions().list().unwrap(), [open, gone]);
    assert!(storage.sessions().list_closed().unwrap().is_empty());
    storage.close();
}

/// A database as schema v1 wrote it: raw SQL, because the typed API speaks the newest schema.
fn seed_v1(path: &Path, default: WorkspaceId, other: WorkspaceId) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(V1.sql).unwrap();
    conn.execute(
        "INSERT INTO app_meta (key, value) VALUES ('schema_version', '1'), ('default_workspace_id', ?1)",
        [default.to_string()],
    )
    .unwrap();
    for (id, name, roots, created, position) in [
        (default, "default", "[]", 100, 0),
        (other, "api", r#"["/work/api"]"#, 200, 1),
    ] {
        conn.execute(
            "INSERT INTO workspaces (id, name, roots_json, created_at_ms, position) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id.to_string(), name, roots, created, position],
        )
        .unwrap();
    }
    for (workspace, created, ended) in [(default, 500_i64, None), (other, 300, Some(900_i64))] {
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, kind, title, title_locked, spec_json, \
                 state_json, created_at_ms, ended_at_ms) \
             VALUES (?1, ?2, 'terminal', 't', 0, '{}', '{\"state\":\"running\"}', ?3, ?4)",
            rusqlite::params![
                SessionId::new().to_string(),
                workspace.to_string(),
                created,
                ended
            ],
        )
        .unwrap();
    }
}

#[test]
fn migrating_v1_turns_the_default_workspace_into_home() {
    let dir = tempfile::tempdir().unwrap();
    let (default, api) = (WorkspaceId::new(), WorkspaceId::new());
    seed_v1(&db(&dir), default, api);

    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.app_meta().schema_version().unwrap(), newest());
    let rows = storage.workspaces().list().unwrap();
    assert_eq!(rows.len(), 2);
    let home = &rows[0];
    assert_eq!(home.id, default);
    assert_eq!(home.kind, WorkspaceKind::Home);
    assert_eq!(home.name, "Home", "the placeholder name follows");
    assert!(!home.pinned);
    assert_eq!(home.created_by, CreatedBy::User);
    assert_eq!(home.position, 0);
    // Activity: the newest of creation and end of its sessions.
    assert_eq!(home.last_active_ms, 500);
    let project = &rows[1];
    assert_eq!(project.id, api);
    assert_eq!(project.kind, WorkspaceKind::Project);
    assert_eq!(project.name, "api");
    assert_eq!(project.roots, [PathBuf::from("/work/api")]);
    assert_eq!(project.last_active_ms, 900);
    // The pointer is gone: Home is the row with kind `home`.
    assert_eq!(
        storage.app_meta().get("default_workspace_id").unwrap(),
        None
    );
    assert!(storage.recent_projects().list().unwrap().is_empty());
    assert_eq!(
        storage.sessions().list().unwrap().len(),
        2,
        "sessions survive"
    );
    storage.close();

    let files = backups(&dir);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].to_string_lossy().ends_with("-v1.db"));
}

#[test]
fn migrating_keeps_a_renamed_default_workspace_name() {
    let dir = tempfile::tempdir().unwrap();
    let (default, api) = (WorkspaceId::new(), WorkspaceId::new());
    seed_v1(&db(&dir), default, api);
    {
        let conn = rusqlite::Connection::open(db(&dir)).unwrap();
        conn.execute(
            "UPDATE workspaces SET name = 'notes' WHERE id = ?1",
            [default.to_string()],
        )
        .unwrap();
    }
    let storage = Storage::open(db(&dir)).unwrap();
    let rows = storage.workspaces().list().unwrap();
    assert_eq!(
        (rows[0].name.as_str(), rows[0].kind),
        ("notes", WorkspaceKind::Home)
    );
    storage.close();
}

#[test]
fn there_is_at_most_one_home() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let home = |name: &str, position| WorkspaceRow {
        kind: WorkspaceKind::Home,
        ..workspace(name, position)
    };
    let (first, second) = (home("Home", 0), home("Other", 1));
    storage.workspaces().insert(first.clone()).unwrap();
    // Rejected by the unique index; the failed write is logged and nothing is stored.
    storage.workspaces().insert(second).unwrap();
    assert_eq!(storage.workspaces().list().unwrap(), [first]);
}

#[test]
fn workspace_fields_are_updated_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let a = WorkspaceRow {
        created_by: CreatedBy::Agent,
        ..workspace("a", 0)
    };
    let b = workspace("b", 1);
    storage.workspaces().insert(a.clone()).unwrap();
    storage.workspaces().insert(b.clone()).unwrap();

    storage.workspaces().set_pinned(b.id, true).unwrap();
    storage
        .workspaces()
        .set_roots(a.id, vec![PathBuf::from("/x")])
        .unwrap();
    storage.workspaces().set_last_active(a.id, 77_000).unwrap();
    storage
        .batch(vec![
            Write::SetWorkspacePosition {
                id: a.id,
                position: 5,
            },
            Write::SetWorkspacePosition {
                id: b.id,
                position: 3,
            },
        ])
        .unwrap();

    let rows = storage.workspaces().list().unwrap();
    assert_eq!(rows.iter().map(|w| w.id).collect::<Vec<_>>(), [b.id, a.id]);
    assert!(rows[0].pinned && !rows[1].pinned);
    assert_eq!(rows[0].position, 3);
    assert_eq!(rows[1].roots, [PathBuf::from("/x")]);
    assert_eq!(rows[1].last_active_ms, 77_000);
    assert_eq!(rows[1].created_by, CreatedBy::Agent);
    assert_eq!(rows[1].created_at_ms, a.created_at_ms);
}

#[test]
fn projects_belong_to_a_workspace_until_it_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let path = db(&dir);
    let storage = Storage::open(&path).unwrap();
    let container = WorkspaceRow {
        kind: WorkspaceKind::Workspace,
        roots: Vec::new(),
        ..workspace("shop", 0)
    };
    let a = WorkspaceRow {
        common_dir: Some(PathBuf::from("/work/a/.git")),
        parent_id: Some(container.id),
        ..workspace("a", 1)
    };
    let b = workspace("b", 2);
    for row in [&container, &a, &b] {
        storage.workspaces().insert(row.clone()).unwrap();
    }
    storage.sessions().upsert(session(a.id, "in a")).unwrap();

    let rows = storage.workspaces().list().unwrap();
    assert_eq!(rows, [container.clone(), a.clone(), b.clone()]);

    storage
        .workspaces()
        .set_parent(b.id, Some(container.id))
        .unwrap();
    storage
        .workspaces()
        .set_common_dir(b.id, Some(PathBuf::from("/work/b/.git")))
        .unwrap();
    storage.workspaces().set_parent(a.id, None).unwrap();
    let rows = storage.workspaces().list().unwrap();
    assert_eq!(rows[1].parent_id, None);
    assert_eq!(rows[2].parent_id, Some(container.id));
    assert_eq!(
        rows[2].common_dir.as_deref(),
        Some(Path::new("/work/b/.git"))
    );

    // Deleting the workspace leaves its projects, ungrouped, and their sessions.
    storage.workspaces().delete(container.id).unwrap();
    storage.close();
    let storage = Storage::open(&path).unwrap();
    let rows = storage.workspaces().list().unwrap();
    assert_eq!(rows.iter().map(|w| w.id).collect::<Vec<_>>(), [a.id, b.id]);
    assert!(rows.iter().all(|w| w.parent_id.is_none()));
    assert_eq!(storage.sessions().list().unwrap().len(), 1);
}

#[test]
fn migrating_to_projects_rebuilds_the_table_without_losing_dependents() {
    let dir = tempfile::tempdir().unwrap();
    let path = db(&dir);
    let home = WorkspaceRow {
        kind: WorkspaceKind::Home,
        ..workspace("Home", 0)
    };
    let ws = workspace("old", 1);
    {
        let storage = Storage::open_with_migrations(&path, &migrate::MIGRATIONS[..7]).unwrap();
        seed_legacy_workspace(&storage, &ws);
        storage
            .read({
                let home = home.clone();
                move |conn| {
                    conn.execute(
                        "INSERT INTO workspaces (id, name, roots_json, kind, pinned, created_by, \
                         created_at_ms, last_active_ms, position) \
                         VALUES (?1, 'Home', '[]', 'home', 1, 'cli', 5, 6, 0)",
                        [home.id.to_string()],
                    )
                    .unwrap();
                    Ok(())
                }
            })
            .unwrap();
        storage.sessions().upsert(session(ws.id, "kept")).unwrap();
        seed_legacy_task(&storage, &task(ws.id, "kept too", 10, &[]));
        storage.close();
    }

    let storage = Storage::open(&path).unwrap();
    assert_eq!(storage.app_meta().schema_version().unwrap(), newest());
    // The sessions and tasks of the old table's rows were not cascaded away.
    assert_eq!(storage.sessions().list().unwrap().len(), 1);
    assert_eq!(storage.tasks().list().unwrap().len(), 1);
    let rows = storage.workspaces().list().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].kind, WorkspaceKind::Home);
    assert!(rows[0].pinned);
    assert_eq!(rows[0].created_by, CreatedBy::Cli);
    assert_eq!(
        (rows[1].common_dir.clone(), rows[1].parent_id),
        (None, None)
    );
    assert_eq!(rows[1].roots, ws.roots);
    assert_eq!(backups(&dir).len(), 1);

    // The new constraints and the relinked foreign keys hold, and Home stays unique.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    let kinds: Vec<String> = conn
        .prepare("SELECT sql FROM sqlite_master WHERE name IN ('workspaces_one_home', 'workspaces_by_parent')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(kinds.len(), 2, "{kinds:?}");
    assert!(
        conn.execute(
            "UPDATE workspaces SET kind = 'workspace' WHERE kind = 'project'",
            []
        )
        .is_ok()
    );
    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
    conn.execute("DELETE FROM workspaces WHERE kind = 'workspace'", [])
        .unwrap();
    let sessions: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sessions, 0, "sessions still cascade from their workspace");
}

#[test]
fn recent_projects_keep_the_newest_time_and_the_newest_two_hundred() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let project = |path: &str, name: &str, at| RecentProject {
        path: PathBuf::from(path),
        name: name.to_owned(),
        last_opened_ms: at,
    };
    let recent = storage.recent_projects();
    recent.upsert(project("/p/a", "a", 10)).unwrap();
    recent.upsert(project("/p/b", "b", 30)).unwrap();
    recent.upsert(project("/p/c", "c", 20)).unwrap();
    // A refresh replaces the name but an older time never wins.
    recent.upsert(project("/p/b", "b2", 5)).unwrap();
    recent.upsert(project("/p/a", "a", 40)).unwrap();
    assert_eq!(
        recent.list().unwrap(),
        [
            project("/p/a", "a", 40),
            project("/p/b", "b2", 30),
            project("/p/c", "c", 20)
        ]
    );
    recent.delete(PathBuf::from("/p/c")).unwrap();
    assert_eq!(recent.list().unwrap().len(), 2);

    for n in 0..250_u64 {
        recent
            .upsert(project(&format!("/bulk/{n:03}"), "bulk", 1_000 + n))
            .unwrap();
    }
    let all = recent.list().unwrap();
    assert_eq!(all.len(), crate::RECENT_PROJECTS_KEPT);
    assert_eq!(all[0].path, PathBuf::from("/bulk/249"));
    assert_eq!(all.last().unwrap().path, PathBuf::from("/bulk/050"));
    storage.close();

    // Durable.
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(
        storage.recent_projects().list().unwrap().len(),
        crate::RECENT_PROJECTS_KEPT
    );
}

#[test]
fn a_session_needs_its_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    // Rejected by the foreign key; the failed write is logged and nothing is stored.
    storage
        .sessions()
        .upsert(session(WorkspaceId::new(), "orphan"))
        .unwrap();
    assert!(storage.sessions().list().unwrap().is_empty());
}

#[test]
fn app_meta_and_epoch_history() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let meta = storage.app_meta();
    assert_eq!(meta.get("k").unwrap(), None);
    meta.set("k", "v1").unwrap();
    meta.set("k", "v2").unwrap();
    assert_eq!(meta.get("k").unwrap().as_deref(), Some("v2"));

    let epochs: Vec<Epoch> = (0..crate::EPOCH_HISTORY_KEPT + 3)
        .map(|_| Epoch::new())
        .collect();
    for (i, epoch) in epochs.iter().enumerate() {
        meta.record_epoch(*epoch, i as u64).unwrap();
    }
    let history = meta.epoch_history().unwrap();
    assert_eq!(history.len(), crate::EPOCH_HISTORY_KEPT);
    assert_eq!(history[0].epoch, epochs[3]);
    assert_eq!(history.last().unwrap().epoch, *epochs.last().unwrap());
}

#[test]
fn a_clean_stop_is_noted_on_its_epoch_only() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let meta = storage.app_meta();
    let (first, second, unknown) = (Epoch::new(), Epoch::new(), Epoch::new());
    meta.record_epoch(first, 10).unwrap();
    meta.record_epoch(second, 20).unwrap();
    meta.record_epoch_stop(first, 15).unwrap();
    // A stop of a run that is not recorded changes nothing, and neither does a second one.
    meta.record_epoch_stop(unknown, 16).unwrap();
    meta.record_epoch_stop(first, 99).unwrap();
    let history = meta.epoch_history().unwrap();
    assert_eq!(history[0].end, crate::EpochEnd::Clean { at_ms: 15 });
    assert_eq!(
        history[1].end,
        crate::EpochEnd::Open,
        "never stopped: a crash"
    );

    // Records of builds that did not track the end read back as untracked.
    meta.set(
        "epoch_history",
        &format!(r#"[{{"epoch":"{first}","started_at_ms":1}}]"#),
    )
    .unwrap();
    assert_eq!(
        meta.epoch_history().unwrap()[0].end,
        crate::EpochEnd::Untracked
    );
}

#[test]
fn concurrent_submitters_keep_per_entity_order() {
    const THREADS: usize = 8;
    const ROUNDS: usize = 200;

    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let ws = workspace("w", 0);
    storage.workspaces().insert(ws.clone()).unwrap();

    let rows: Vec<SessionRow> = (0..THREADS)
        .map(|i| session(ws.id, &format!("s{i}")))
        .collect();
    std::thread::scope(|scope| {
        for row in &rows {
            let storage = storage.clone();
            scope.spawn(move || {
                let mut row = row.clone();
                for round in 0..ROUNDS {
                    row.title = format!("round-{round}");
                    storage.sessions().upsert(row.clone()).unwrap();
                    if round % 50 == 0 {
                        // A read observes every write this thread queued before it.
                        let seen = storage
                            .sessions()
                            .list_in_workspace(row.workspace_id)
                            .unwrap();
                        let mine = seen.iter().find(|s| s.id == row.id).unwrap();
                        assert_eq!(mine.title, row.title);
                    }
                }
            });
        }
    });
    storage.flush().unwrap();

    let stored = storage.sessions().list().unwrap();
    assert_eq!(stored.len(), THREADS);
    for row in &rows {
        let mine = stored.iter().find(|s| s.id == row.id).unwrap();
        assert_eq!(mine.title, format!("round-{}", ROUNDS - 1));
    }
}

#[test]
fn writes_after_close_fail_cleanly_and_close_drains_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let ws = workspace("w", 0);
    storage.workspaces().insert(ws.clone()).unwrap();
    for i in 0..100 {
        storage
            .sessions()
            .upsert(session(ws.id, &format!("s{i}")))
            .unwrap();
    }
    storage.close();
    storage.close();
    assert!(matches!(
        storage.workspaces().insert(workspace("late", 1)),
        Err(StorageError::Closed)
    ));
    assert!(matches!(
        storage.sessions().list(),
        Err(StorageError::Closed)
    ));

    let reopened = Storage::open(db(&dir)).unwrap();
    assert_eq!(reopened.sessions().list().unwrap().len(), 100);
}

fn batch_around(fault: Fault, ws: WorkspaceId) -> Vec<Write> {
    let mut writes: Vec<Write> = (0..50)
        .map(|i| Write::UpsertSession(Box::new(session(ws, &format!("batch-{i}")))))
        .collect();
    writes.push(Write::Fault(fault));
    writes.push(Write::UpsertSession(Box::new(session(ws, "after-fault"))));
    writes
}

#[test]
fn a_writer_that_dies_mid_batch_leaves_the_database_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("w", 0);
    let committed = session(ws.id, "committed");
    let storage = Storage::open(db(&dir)).unwrap();
    storage.workspaces().insert(ws.clone()).unwrap();
    storage.sessions().upsert(committed.clone()).unwrap();
    storage.flush().unwrap();

    storage.batch(batch_around(Fault::Panic, ws.id)).unwrap();
    // The writer thread unwound; the handle reports it instead of hanging.
    assert!(matches!(storage.flush(), Err(StorageError::Closed)));
    assert!(matches!(
        storage.sessions().upsert(session(ws.id, "late")),
        Err(StorageError::Closed)
    ));
    drop(storage);

    integrity_ok(&db(&dir));
    let reopened = Storage::open(db(&dir)).unwrap();
    assert_eq!(reopened.sessions().list().unwrap(), [committed]);
}

#[test]
fn killing_the_process_mid_batch_leaves_the_database_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = db(&dir);
    let ws_id = WorkspaceId::new();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["tests::crash_child", "--exact", "--nocapture"])
        .env("HUTAN_STORAGE_CRASH_DB", &path)
        .env("HUTAN_STORAGE_CRASH_WS", ws_id.to_string())
        .output()
        .unwrap();
    assert_eq!(
        output.status.signal(),
        Some(6),
        "the child must die of SIGABRT mid-batch: {:?}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    integrity_ok(&path);
    let storage = Storage::open(&path).unwrap();
    let titles: Vec<String> = storage
        .sessions()
        .list()
        .unwrap()
        .into_iter()
        .map(|s| s.title)
        .collect();
    assert_eq!(
        titles,
        ["committed"],
        "the half-applied batch must not be visible"
    );
    assert_eq!(storage.workspaces().list().unwrap().len(), 1);
}

/// Body of the child process of the test above; does nothing in a normal test run.
#[test]
fn crash_child() {
    let (Some(path), Some(ws)) = (
        std::env::var_os("HUTAN_STORAGE_CRASH_DB"),
        std::env::var("HUTAN_STORAGE_CRASH_WS").ok(),
    ) else {
        return;
    };
    let ws: WorkspaceId = ws.parse().unwrap();
    let storage = Storage::open(path).unwrap();
    storage
        .workspaces()
        .insert(WorkspaceRow {
            id: ws,
            ..workspace("w", 0)
        })
        .unwrap();
    storage.sessions().upsert(session(ws, "committed")).unwrap();
    storage.flush().unwrap();
    storage.batch(batch_around(Fault::Abort, ws)).unwrap();
    // The writer aborts the process while the batch transaction is open.
    std::thread::sleep(std::time::Duration::from_secs(30));
}

// Chat journal -----------------------------------------------------------------------------------

fn chat_item(session: SessionId, seq: u64, kind: &str) -> ChatItemRow {
    ChatItemRow {
        session_id: session,
        seq,
        ts_ms: 10_000 + seq,
        turn_id: Some("turn-1".to_owned()),
        item_id: None,
        kind: kind.to_owned(),
        event_json: format!(r#"{{"type":"{kind}","seq":{seq}}}"#),
    }
}

fn chat_session(storage: &Storage) -> SessionId {
    let ws = workspace("chat", 0);
    let mut row = session(ws.id, "codex");
    row.kind = SessionKind::Harness;
    storage.workspaces().insert(ws).unwrap();
    let id = row.id;
    storage.sessions().upsert(row).unwrap();
    id
}

#[test]
fn the_chat_journal_pages_backwards_and_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let id;
    {
        let storage = Storage::open(db(&dir)).unwrap();
        id = chat_session(&storage);
        assert_eq!(storage.chat().last_seq(id).unwrap(), 0);
        // seq has gaps where streaming deltas were.
        for seq in [1, 2, 5, 6, 9] {
            storage
                .chat()
                .append(chat_item(id, seq, "message_end"))
                .unwrap();
        }
        // A repeated seq is ignored, not an error and not an overwrite.
        let mut again = chat_item(id, 5, "other");
        again.event_json = "{}".to_owned();
        storage.chat().append(again).unwrap();
        storage.close();
    }
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.chat().last_seq(id).unwrap(), 9);
    let seqs = |before, limit| -> Vec<u64> {
        storage
            .chat()
            .items(id, before, limit)
            .unwrap()
            .iter()
            .map(|r| r.seq)
            .collect()
    };
    assert_eq!(seqs(None, 100), [1, 2, 5, 6, 9]);
    assert_eq!(seqs(None, 2), [6, 9], "the newest window, oldest first");
    assert_eq!(seqs(Some(6), 2), [2, 5]);
    assert_eq!(seqs(Some(1), 10), Vec::<u64>::new());
    let row = &storage.chat().items(id, Some(6), 1).unwrap()[0];
    assert_eq!(
        (row.kind.as_str(), row.turn_id.as_deref()),
        ("message_end", Some("turn-1"))
    );
    assert!(
        row.event_json.contains("\"seq\":5"),
        "the first write won: {}",
        row.event_json
    );
    storage.close();
}

#[test]
fn chat_turns_are_updated_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let id = chat_session(&storage);
    let mut turn = ChatTurnRow {
        session_id: id,
        turn_id: "t1".to_owned(),
        provider_turn_id: Some("p1".to_owned()),
        status: ChatTurnStatus::Running,
        started_at_ms: 100,
        ended_at_ms: None,
        error: None,
    };
    storage.chat().upsert_turn(turn.clone()).unwrap();
    turn.status = ChatTurnStatus::Failed;
    turn.ended_at_ms = Some(150);
    turn.error = Some("boom".to_owned());
    turn.provider_turn_id = None;
    storage.chat().upsert_turn(turn.clone()).unwrap();
    let turns = storage.chat().turns(id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, ChatTurnStatus::Failed);
    assert_eq!(turns[0].error.as_deref(), Some("boom"));
    assert_eq!(
        turns[0].provider_turn_id.as_deref(),
        Some("p1"),
        "a known provider id is kept"
    );
    storage.close();
}

#[test]
fn a_chat_queue_is_replaced_whole_kept_in_order_and_removed_with_its_session() {
    use crate::QueuedRow;
    let dir = tempfile::tempdir().unwrap();
    let item = |session, item_id: u64, text: &str| QueuedRow {
        session_id: session,
        item_id,
        text: text.to_owned(),
        queued_at_ms: 100 + item_id,
    };
    let id;
    {
        let storage = Storage::open(db(&dir)).unwrap();
        id = chat_session(&storage);
        assert!(storage.chat().queue(id).unwrap().is_empty());
        storage
            .chat()
            .set_queue(id, vec![item(id, 1, "a"), item(id, 2, "b")])
            .unwrap();
        // The order is the list's, not the ids'.
        storage
            .chat()
            .set_queue(id, vec![item(id, 3, "c"), item(id, 1, "a, edited")])
            .unwrap();
        storage.close();
    }
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(
        storage.chat().queue(id).unwrap(),
        [item(id, 3, "c"), item(id, 1, "a, edited")]
    );
    storage.chat().set_queue(id, Vec::new()).unwrap();
    assert!(storage.chat().queue(id).unwrap().is_empty());
    storage
        .chat()
        .set_queue(id, vec![item(id, 4, "d")])
        .unwrap();
    storage.sessions().delete(id).unwrap();
    assert!(storage.chat().queue(id).unwrap().is_empty());
    storage.close();
}

#[test]
fn questions_record_the_request_and_how_it_ended_never_the_answer() {
    use crate::{QuestionOutcome, QuestionRow};
    use hutan_core::QuestionId;

    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let id = chat_session(&storage);
    let open = QuestionRow {
        id: QuestionId::new(),
        session_id: id,
        turn_id: Some("t1".to_owned()),
        title: "Which branch?".to_owned(),
        requested_at_ms: 200,
        resolved_at_ms: None,
        outcome: None,
        decided_by: None,
    };
    let mut decided = open.clone();
    decided.id = QuestionId::new();
    decided.requested_at_ms = 100;
    storage.questions().upsert(open.clone()).unwrap();
    storage.questions().upsert(decided.clone()).unwrap();
    assert_eq!(storage.questions().unresolved().unwrap().len(), 2);

    decided.resolved_at_ms = Some(300);
    decided.outcome = Some(QuestionOutcome::Answered);
    decided.decided_by = Some("desktop".to_owned());
    storage.questions().upsert(decided.clone()).unwrap();
    assert_eq!(
        storage.questions().unresolved().unwrap(),
        std::slice::from_ref(&open)
    );
    assert_eq!(
        storage.questions().list(Some(id)).unwrap(),
        [decided, open],
        "oldest request first, decision recorded"
    );
    assert!(
        storage
            .questions()
            .list(Some(SessionId::new()))
            .unwrap()
            .is_empty()
    );
    storage.close();
}

#[test]
fn approvals_record_the_request_and_the_decision() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let id = chat_session(&storage);
    let open = ApprovalRow {
        id: ApprovalId::new(),
        session_id: id,
        turn_id: Some("t1".to_owned()),
        kind: "command".to_owned(),
        title: "touch hi.txt".to_owned(),
        options_json: r#"[{"id":"accept"}]"#.to_owned(),
        requested_at_ms: 200,
        resolved_at_ms: None,
        outcome: None,
        option_id: None,
        decided_by: None,
    };
    let mut decided = open.clone();
    decided.id = ApprovalId::new();
    decided.requested_at_ms = 100;
    storage.approvals().upsert(open.clone()).unwrap();
    storage.approvals().upsert(decided.clone()).unwrap();
    assert_eq!(storage.approvals().unresolved().unwrap().len(), 2);

    decided.resolved_at_ms = Some(300);
    decided.outcome = Some(ApprovalOutcome::Selected);
    decided.option_id = Some("accept".to_owned());
    decided.decided_by = Some("cli".to_owned());
    storage.approvals().upsert(decided.clone()).unwrap();

    assert_eq!(
        storage.approvals().unresolved().unwrap(),
        std::slice::from_ref(&open)
    );
    assert_eq!(
        storage.approvals().list(Some(id)).unwrap(),
        [decided, open],
        "oldest request first, decision recorded"
    );
    assert!(
        storage
            .approvals()
            .list(Some(SessionId::new()))
            .unwrap()
            .is_empty()
    );
    storage.close();
}

#[test]
fn a_snapshot_event_is_replaced_in_place_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let id = chat_session(&storage);
    storage
        .chat()
        .append(chat_item(id, 1, "turn_started"))
        .unwrap();
    storage
        .chat()
        .append(chat_item(id, 2, "message_partial"))
        .unwrap();
    for (old, new) in [(2, 5), (5, 9)] {
        storage
            .chat()
            .replace(old, chat_item(id, new, "message_partial"))
            .unwrap();
        let seqs: Vec<u64> = storage
            .chat()
            .items(id, None, 10)
            .unwrap()
            .iter()
            .map(|row| row.seq)
            .collect();
        assert_eq!(seqs, [1, new], "one snapshot, never appended");
    }
    storage.chat().delete(id, 9).unwrap();
    assert_eq!(storage.chat().items(id, None, 10).unwrap().len(), 1);
    storage.close();
}

#[test]
fn deleting_a_session_deletes_its_journal_and_approvals() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let id = chat_session(&storage);
    storage
        .chat()
        .append(chat_item(id, 1, "turn_started"))
        .unwrap();
    storage
        .approvals()
        .upsert(ApprovalRow {
            id: ApprovalId::new(),
            session_id: id,
            turn_id: None,
            kind: "command".to_owned(),
            title: "x".to_owned(),
            options_json: "[]".to_owned(),
            requested_at_ms: 1,
            resolved_at_ms: None,
            outcome: None,
            option_id: None,
            decided_by: None,
        })
        .unwrap();
    storage.sessions().delete(id).unwrap();
    assert!(storage.chat().items(id, None, 10).unwrap().is_empty());
    assert!(storage.approvals().list(None).unwrap().is_empty());
    storage.close();
}

#[test]
fn migrating_v2_adds_the_chat_tables_and_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &[V1, V2]).unwrap();
        seed_legacy_workspace(&storage, &ws);
        storage.close();
    }
    assert!(!table_exists(&db(&dir), "chat_items"));
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.workspaces().list().unwrap(), [ws]);
    storage.close();
    for table in ["chat_items", "chat_turns", "approvals"] {
        assert!(table_exists(&db(&dir), table), "{table}");
    }
    integrity_ok(&db(&dir));
}

#[test]
fn agent_trees_are_replaced_listed_and_removed_with_their_session() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let first = chat_session(&storage);
    let second = chat_session(&storage);
    assert!(storage.agent_trees().list().unwrap().is_empty());

    storage
        .agent_trees()
        .upsert(first, "{\"a\":1}".into(), 10)
        .unwrap();
    storage
        .agent_trees()
        .upsert(second, "{\"b\":2}".into(), 11)
        .unwrap();
    storage
        .agent_trees()
        .upsert(first, "{\"a\":3}".into(), 12)
        .unwrap();
    let mut listed = storage.agent_trees().list().unwrap();
    listed.sort_by_key(|(id, _)| id.to_string());
    let mut expected = vec![
        (first, "{\"a\":3}".to_owned()),
        (second, "{\"b\":2}".to_owned()),
    ];
    expected.sort_by_key(|(id, _)| id.to_string());
    assert_eq!(listed, expected);

    storage.agent_trees().delete(second).unwrap();
    assert_eq!(
        storage.agent_trees().list().unwrap(),
        [(first, "{\"a\":3}".to_owned())]
    );
    // A session's tree goes with the session.
    storage.sessions().delete(first).unwrap();
    assert!(storage.agent_trees().list().unwrap().is_empty());
    storage.close();
}

#[test]
fn migrating_v3_adds_the_agent_tree_table_and_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &migrate::MIGRATIONS[..3]).unwrap();
        seed_legacy_workspace(&storage, &ws);
        storage.close();
    }
    assert!(!table_exists(&db(&dir), "agent_trees"));
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.workspaces().list().unwrap(), [ws]);
    storage.close();
    assert!(table_exists(&db(&dir), "agent_trees"));
    integrity_ok(&db(&dir));
}

#[test]
fn chat_events_can_be_read_by_kind() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let id = chat_session(&storage);
    for (seq, kind) in [
        (1, "turn_started"),
        (2, "message_end"),
        (4, "message_end"),
        (5, "usage"),
        (6, "message_end"),
    ] {
        storage.chat().append(chat_item(id, seq, kind)).unwrap();
    }
    let all: Vec<u64> = storage
        .chat()
        .items_of_kind(id, "message_end", 10)
        .unwrap()
        .iter()
        .map(|r| r.seq)
        .collect();
    assert_eq!(all, [2, 4, 6]);
    let newest: Vec<u64> = storage
        .chat()
        .items_of_kind(id, "message_end", 2)
        .unwrap()
        .iter()
        .map(|r| r.seq)
        .collect();
    assert_eq!(newest, [4, 6], "the newest window, oldest first");
    assert!(
        storage
            .chat()
            .items_of_kind(id, "notice", 10)
            .unwrap()
            .is_empty()
    );
    storage.close();
}

fn provider(id: &str, created: u64) -> ProviderRow {
    ProviderRow {
        id: id.to_owned(),
        kind: "openai-compatible".to_owned(),
        name: format!("{id} name"),
        base_url: "http://127.0.0.1:1234/v1".to_owned(),
        default_model: Some("m1".to_owned()),
        keychain_ref: format!("dev.hutan.provider/{id}"),
        created_at_ms: created,
    }
}

#[test]
fn providers_are_stored_updated_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    {
        let storage = Storage::open(db(&dir)).unwrap();
        assert!(storage.providers().list().unwrap().is_empty());
        storage.providers().upsert(provider("b", 20)).unwrap();
        storage.providers().upsert(provider("a", 10)).unwrap();
        let mut changed = provider("b", 999);
        changed.default_model = None;
        changed.base_url = "http://elsewhere/v1".to_owned();
        storage.providers().upsert(changed).unwrap();
        storage.close();
    }
    let storage = Storage::open(db(&dir)).unwrap();
    let rows = storage.providers().list().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], provider("a", 10));
    assert_eq!(rows[1].id, "b");
    assert_eq!(rows[1].created_at_ms, 20, "the creation time is kept");
    assert_eq!(rows[1].default_model, None);
    assert_eq!(rows[1].base_url, "http://elsewhere/v1");
    storage.providers().delete("a").unwrap();
    storage.providers().delete("missing").unwrap();
    assert_eq!(storage.providers().list().unwrap().len(), 1);
    storage.close();
}

#[test]
fn provider_writes_report_a_failing_database() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    storage.providers().upsert(provider("a", 10)).unwrap();
    // Another connection takes the table away, like a disk or schema fault would.
    rusqlite::Connection::open(db(&dir))
        .unwrap()
        .execute_batch("DROP TABLE providers")
        .unwrap();
    let error = storage.providers().upsert(provider("b", 20)).unwrap_err();
    assert!(error.to_string().contains("provider"), "{error}");
    assert!(storage.providers().delete("a").is_err());
    storage.close();
    assert!(
        matches!(
            storage.providers().upsert(provider("c", 30)),
            Err(StorageError::Closed)
        ),
        "a closed storage is an error too"
    );
}

#[test]
fn app_meta_keys_can_be_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    storage.app_meta().set("k", "v").unwrap();
    assert_eq!(storage.app_meta().get("k").unwrap().as_deref(), Some("v"));
    storage.app_meta().delete("k").unwrap();
    storage.app_meta().delete("k").unwrap();
    assert_eq!(storage.app_meta().get("k").unwrap(), None);
    storage.close();
}

#[test]
fn migrating_v4_adds_the_provider_table_and_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &migrate::MIGRATIONS[..4]).unwrap();
        seed_legacy_workspace(&storage, &ws);
        storage.close();
    }
    assert!(!table_exists(&db(&dir), "providers"));
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.workspaces().list().unwrap(), [ws]);
    assert_eq!(storage.app_meta().schema_version().unwrap(), newest());
    storage.close();
    assert!(table_exists(&db(&dir), "providers"));
    integrity_ok(&db(&dir));
}

fn worktree(path: &str, workspace: WorkspaceId, created: u64) -> WorktreeRow {
    WorktreeRow {
        path: PathBuf::from(path),
        workspace_id: workspace,
        repo_root: PathBuf::from("/work/repo"),
        branch: "feature/x".to_owned(),
        base: "HEAD".to_owned(),
        created_at_ms: created,
    }
}

#[test]
fn worktrees_are_remembered_replaced_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceId::new();
    {
        let storage = Storage::open(db(&dir)).unwrap();
        assert!(storage.worktrees().list().unwrap().is_empty());
        storage
            .worktrees()
            .upsert(worktree("/work/repo-wt/b", ws, 20))
            .unwrap();
        storage
            .worktrees()
            .upsert(worktree("/work/repo-wt/a", ws, 10))
            .unwrap();
        let mut changed = worktree("/work/repo-wt/b", ws, 30);
        changed.branch = "other".to_owned();
        storage.worktrees().upsert(changed.clone()).unwrap();
        storage.close();
    }
    let storage = Storage::open(db(&dir)).unwrap();
    let rows = storage.worktrees().list().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], worktree("/work/repo-wt/a", ws, 10));
    assert_eq!(rows[1].path, PathBuf::from("/work/repo-wt/b"));
    assert_eq!(rows[1].branch, "other");
    storage
        .worktrees()
        .delete(PathBuf::from("/work/repo-wt/a"))
        .unwrap();
    storage
        .worktrees()
        .delete(PathBuf::from("/missing"))
        .unwrap();
    assert_eq!(storage.worktrees().list().unwrap().len(), 1);
    storage.close();
}

#[test]
fn migrating_v5_adds_the_worktree_table_and_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &migrate::MIGRATIONS[..5]).unwrap();
        seed_legacy_workspace(&storage, &ws);
        storage.close();
    }
    assert!(!table_exists(&db(&dir), "worktrees"));
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.workspaces().list().unwrap(), [ws]);
    assert_eq!(storage.app_meta().schema_version().unwrap(), newest());
    storage.close();
    assert!(table_exists(&db(&dir), "worktrees"));
    integrity_ok(&db(&dir));
}

fn task(workspace: WorkspaceId, title: &str, created: u64, deps: &[TaskId]) -> Task {
    Task {
        id: TaskId::new(),
        workspace_id: workspace,
        title: title.to_owned(),
        prompt: Some("do it".to_owned()),
        state: TaskState::Todo,
        depends_on: deps.to_vec(),
        placement: TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: Some("task/x".to_owned()),
            base: Some("main".to_owned()),
        },
        run: Some(TaskRun::Terminal {
            argv: vec!["cargo".to_owned(), "test".to_owned()],
        }),
        auto_start: true,
        sessions: Vec::new(),
        created_at_ms: created,
        started_at_ms: None,
        ended_at_ms: None,
        outcome: None,
        delivery: DeliveryState::None,
        after_policy: AfterPolicy::Done,
        delivery_history: Vec::new(),
    }
}

#[test]
fn tasks_are_stored_with_their_dependencies_and_sessions_and_come_back_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    let first = task(ws.id, "first", 10, &[]);
    let mut second = task(ws.id, "second", 20, &[first.id]);
    let mut third = task(ws.id, "third", 20, &[second.id, first.id]);
    third.run = None;
    third.prompt = None;
    third.placement = TaskPlacement::default();
    third.auto_start = false;
    let (s1, s2) = (SessionId::new(), SessionId::new());
    {
        let storage = Storage::open(db(&dir)).unwrap();
        storage.workspaces().insert(ws.clone()).unwrap();
        assert!(storage.tasks().list().unwrap().is_empty());
        // The order is the creation time, then the row's insertion.
        storage.tasks().upsert(first.clone()).unwrap();
        storage.tasks().upsert(second.clone()).unwrap();
        storage.tasks().upsert(third.clone()).unwrap();
        storage.close();
    }
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(
        storage.tasks().list().unwrap(),
        [first.clone(), second.clone(), third.clone()]
    );

    // A change replaces the dependencies and sessions with the new ones; the order stays.
    second.depends_on = Vec::new();
    second.state = TaskState::Running;
    second.started_at_ms = Some(30);
    second.sessions = vec![s1, s2];
    second.outcome = Some("exited 0".to_owned());
    storage.tasks().upsert(second.clone()).unwrap();
    let listed = storage.tasks().list().unwrap();
    assert_eq!(listed, [first.clone(), second.clone(), third.clone()]);

    // Derived states are stored as the states they derive from.
    let mut shown = third.clone();
    shown.state = TaskState::Ready;
    storage.tasks().upsert(shown.clone()).unwrap();
    assert_eq!(storage.tasks().list().unwrap()[2].state, TaskState::Todo);
    shown.state = TaskState::Blocked;
    storage.tasks().upsert(shown).unwrap();
    assert_eq!(storage.tasks().list().unwrap()[2].state, TaskState::Running);

    // A task that names a dependency nobody stored is not written at all.
    let orphan = task(ws.id, "orphan", 40, &[TaskId::new()]);
    storage.tasks().upsert(orphan).unwrap();
    assert_eq!(storage.tasks().list().unwrap().len(), 3);

    // Deleting a task takes the edges that name it, and deleting the workspace the tasks.
    storage.tasks().delete(first.id).unwrap();
    storage.tasks().delete(TaskId::new()).unwrap();
    let listed = storage.tasks().list().unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].depends_on, [second.id]);
    storage.workspaces().delete(ws.id).unwrap();
    assert!(storage.tasks().list().unwrap().is_empty());
    storage.close();
}

#[test]
fn migrating_v6_adds_the_task_tables_and_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &migrate::MIGRATIONS[..6]).unwrap();
        seed_legacy_workspace(&storage, &ws);
        storage.close();
    }
    for table in ["tasks", "task_deps", "task_sessions"] {
        assert!(!table_exists(&db(&dir), table));
    }
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.workspaces().list().unwrap(), [ws]);
    assert_eq!(storage.app_meta().schema_version().unwrap(), newest());
    assert!(storage.tasks().list().unwrap().is_empty());
    storage.close();
    for table in ["tasks", "task_deps", "task_sessions"] {
        assert!(table_exists(&db(&dir), table), "{table}");
    }
    integrity_ok(&db(&dir));
}

#[test]
fn the_delivery_of_a_task_is_stored_and_old_tasks_keep_todays_behaviour() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    let old = task(ws.id, "old", 10, &[]);
    {
        let storage = Storage::open_with_migrations(db(&dir), &migrate::MIGRATIONS[..8]).unwrap();
        seed_legacy_workspace(&storage, &ws);
        seed_legacy_task(&storage, &old);
        storage.close();
    }
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.app_meta().schema_version().unwrap(), newest());
    let migrated = storage.tasks().list().unwrap();
    assert_eq!(migrated.len(), 1);
    assert_eq!(migrated[0].delivery, DeliveryState::None);
    assert_eq!(migrated[0].after_policy, AfterPolicy::Done);
    assert!(migrated[0].delivery_history.is_empty());

    let mut next = task(ws.id, "next", 20, &[old.id]);
    next.after_policy = AfterPolicy::Merged;
    next.state = TaskState::Done;
    let mut asked = DeliveryRecord::new(DeliveryState::ChangesRequested, DeliveryActor::Cli, 5);
    asked.note = Some("rename it".to_owned());
    let mut merged = DeliveryRecord::new(DeliveryState::Merged, DeliveryActor::Desktop, 9);
    merged.commit = Some("abc123".to_owned());
    merged.strategy = Some("squash".to_owned());
    next.record_delivery(DeliveryRecord::new(
        DeliveryState::AwaitingReview,
        DeliveryActor::Daemon,
        1,
    ));
    next.record_delivery(asked);
    next.record_delivery(merged);
    storage.tasks().upsert(next.clone()).unwrap();
    storage.close();

    // It survives a restart.
    let storage = Storage::open(db(&dir)).unwrap();
    let listed = storage.tasks().list().unwrap();
    assert_eq!(listed[1], next);
    assert_eq!(listed[1].delivery, DeliveryState::Merged);
    storage.close();
    integrity_ok(&db(&dir));
}

#[test]
fn control_grants_are_stored_listed_newest_first_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let grant = |id, tool: &str, workspace: bool| crate::GrantRow {
        id,
        tool: tool.to_owned(),
        workspace_id: workspace.then(WorkspaceId::new),
        chat_session_id: (!workspace).then(SessionId::new),
        created_ms: 10 + id,
        created_by: "desktop".to_owned(),
    };
    let (one, two) = (
        grant(1, "session.rename", true),
        grant(2, "task.create", false),
    );
    storage.control_grants().insert(one.clone()).unwrap();
    storage.control_grants().insert(two.clone()).unwrap();
    assert_eq!(storage.control_grants().list().unwrap(), [two, one.clone()]);
    storage.control_grants().delete(2).unwrap();
    assert_eq!(storage.control_grants().list().unwrap(), [one]);
    // A grant needs exactly one of workspace and chat.
    let both = crate::GrantRow {
        workspace_id: Some(WorkspaceId::new()),
        chat_session_id: Some(SessionId::new()),
        ..grant(3, "x", true)
    };
    assert!(storage.control_grants().insert(both).is_err());
    storage.close();
}

// Mawas memory (MW-2) ---------------------------------------------------------------------------

mod memory {
    use super::*;
    use crate::{
        Action, Author, Category, Change, EntryState, MemoryError, Operation, ProposalRow,
        ProposalState, Resolution, Scope, ScopeFilter, SnapshotRow,
    };

    fn open() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(db(&dir)).unwrap();
        (dir, storage)
    }

    fn change(operation_id: &str, action: Action) -> Change {
        Change {
            operation_id: operation_id.to_owned(),
            action,
            author: Author::User,
            provenance_json: r#"{"author":"user"}"#.to_owned(),
            approval_id: None,
            now_ms: 1_000,
            max_entries: 200,
        }
    }

    fn create(id: &str, scope: Scope, category: Category, body: &str) -> Action {
        Action::Create {
            entry_id: id.to_owned(),
            scope,
            category,
            body: body.to_owned(),
        }
    }

    #[test]
    fn revisions_are_immutable_and_every_change_adds_one() {
        let (_dir, storage) = open();
        let memory = storage.memory();
        let made = memory
            .apply(change(
                "op-1",
                create("e1", Scope::Global, Category::Fact, "uses jj"),
            ))
            .unwrap();
        assert_eq!((made.revision, made.scope_revision), (1, 1));
        let updated = memory
            .apply(change(
                "op-2",
                Action::Update {
                    entry_id: "e1".into(),
                    expected_revision: 1,
                    body: "uses git".into(),
                },
            ))
            .unwrap();
        assert_eq!((updated.revision, updated.scope_revision), (2, 2));
        let history = memory.history("e1").unwrap();
        assert_eq!(
            history
                .iter()
                .map(|r| (r.revision, r.body.as_str()))
                .collect::<Vec<_>>(),
            [(2, "uses git"), (1, "uses jj")]
        );
        assert_eq!(history[1].content_hash, crate::hash_text("uses jj"));
        // The schema refuses to rewrite or drop history, even for a direct statement.
        storage
            .read(|conn| {
                let update = conn.execute("UPDATE memory_revisions SET body = 'x'", []);
                assert!(update.unwrap_err().to_string().contains("immutable"));
                let delete = conn.execute("DELETE FROM memory_revisions", []);
                assert!(delete.unwrap_err().to_string().contains("immutable"));
                Ok(())
            })
            .unwrap();
        assert_eq!(memory.history("e1").unwrap().len(), 2);
        storage.close();
    }

    #[test]
    fn a_stale_expected_revision_is_a_conflict_and_changes_nothing() {
        let (_dir, storage) = open();
        let memory = storage.memory();
        memory
            .apply(change(
                "a",
                create("e1", Scope::Global, Category::Profile, "terse"),
            ))
            .unwrap();
        memory
            .apply(change(
                "b",
                Action::Update {
                    entry_id: "e1".into(),
                    expected_revision: 1,
                    body: "very terse".into(),
                },
            ))
            .unwrap();
        let stale = memory.apply(change(
            "c",
            Action::Update {
                entry_id: "e1".into(),
                expected_revision: 1,
                body: "chatty".into(),
            },
        ));
        assert!(matches!(
            stale,
            Err(MemoryError::Conflict {
                expected: 1,
                current: 2
            })
        ));
        let missing = memory.apply(change(
            "d",
            Action::Archive {
                entry_id: "nope".into(),
                expected_revision: 1,
            },
        ));
        assert!(matches!(missing, Err(MemoryError::NotFound(_))));
        let entry = memory.get("e1").unwrap().unwrap();
        assert_eq!(
            (entry.entry.current_revision, entry.current.body.as_str()),
            (2, "very terse")
        );
        assert_eq!(memory.scope_revision(Scope::Global).unwrap(), 2);
        storage.close();
    }

    #[test]
    fn a_repeated_operation_id_returns_the_original_receipt() {
        let (_dir, storage) = open();
        let memory = storage.memory();
        let first = memory
            .apply(change(
                "same",
                create("e1", Scope::Global, Category::Fact, "one"),
            ))
            .unwrap();
        memory
            .apply(change(
                "other",
                create("e2", Scope::Global, Category::Fact, "two"),
            ))
            .unwrap();
        let again = memory
            .apply(change(
                "same",
                create("e3", Scope::Global, Category::Note, "different"),
            ))
            .unwrap();
        assert!(again.duplicate && !first.duplicate);
        assert_eq!(
            (
                again.entry_id.as_str(),
                again.revision,
                again.scope_revision
            ),
            ("e1", 1, 1),
            "the receipt of the first application, not the scope's current revision"
        );
        assert_eq!(memory.list(ScopeFilter::Any, true).unwrap().len(), 2);
        storage.close();
    }

    #[test]
    fn archive_and_restore_are_revisions_that_keep_the_body() {
        let (_dir, storage) = open();
        let memory = storage.memory();
        memory
            .apply(change(
                "a",
                create("e1", Scope::Global, Category::Fact, "kept"),
            ))
            .unwrap();
        let archived = memory
            .apply(change(
                "b",
                Action::Archive {
                    entry_id: "e1".into(),
                    expected_revision: 1,
                },
            ))
            .unwrap();
        assert_eq!(archived.revision, 2);
        let entry = memory.get("e1").unwrap().unwrap();
        assert_eq!(entry.entry.state, EntryState::Archived);
        assert!(memory.list(ScopeFilter::Any, false).unwrap().is_empty());
        assert_eq!(memory.list(ScopeFilter::Any, true).unwrap().len(), 1);
        // An archived entry is not edited, nor archived twice, until it is restored.
        let edit = memory.apply(change(
            "c",
            Action::Update {
                entry_id: "e1".into(),
                expected_revision: 2,
                body: "x".into(),
            },
        ));
        assert!(matches!(edit, Err(MemoryError::InvalidState(_))));
        let twice = memory.apply(change(
            "d",
            Action::Archive {
                entry_id: "e1".into(),
                expected_revision: 2,
            },
        ));
        assert!(matches!(twice, Err(MemoryError::InvalidState(_))));
        memory
            .apply(change(
                "e",
                Action::Restore {
                    entry_id: "e1".into(),
                    expected_revision: 2,
                },
            ))
            .unwrap();
        let history = memory.history("e1").unwrap();
        assert_eq!(
            history
                .iter()
                .map(|r| (r.revision, r.operation))
                .collect::<Vec<_>>(),
            [
                (3, Operation::Restore),
                (2, Operation::Archive),
                (1, Operation::Create)
            ]
        );
        assert!(history.iter().all(|r| r.body == "kept"));
        assert_eq!(
            memory.get("e1").unwrap().unwrap().entry.state,
            EntryState::Active
        );
        storage.close();
    }

    #[test]
    fn the_number_of_active_entries_is_bounded() {
        let (_dir, storage) = open();
        let memory = storage.memory();
        let capped = |id: &str, action| Change {
            max_entries: 2,
            ..change(id, action)
        };
        for n in 0..2 {
            memory
                .apply(capped(
                    &format!("op{n}"),
                    create(&format!("e{n}"), Scope::Global, Category::Note, "x"),
                ))
                .unwrap();
        }
        let third = memory.apply(capped(
            "op2",
            create("e2", Scope::Global, Category::Note, "x"),
        ));
        assert!(matches!(third, Err(MemoryError::Full { max: 2 })));
        assert!(memory.get("e2").unwrap().is_none());
        // Archiving frees a place; restoring needs one again.
        memory
            .apply(capped(
                "arch",
                Action::Archive {
                    entry_id: "e0".into(),
                    expected_revision: 1,
                },
            ))
            .unwrap();
        memory
            .apply(capped(
                "op3",
                create("e3", Scope::Global, Category::Note, "y"),
            ))
            .unwrap();
        let restore = memory.apply(capped(
            "rest",
            Action::Restore {
                entry_id: "e0".into(),
                expected_revision: 2,
            },
        ));
        assert!(matches!(restore, Err(MemoryError::Full { max: 2 })));
        assert_eq!(memory.count_active().unwrap(), 2);
        storage.close();
    }

    #[test]
    fn scopes_count_their_own_changes_and_list_in_snapshot_order() {
        let (_dir, storage) = open();
        let memory = storage.memory();
        let ws = WorkspaceId::new();
        let other = WorkspaceId::new();
        let mut at = 100;
        for (id, scope, category) in [
            ("n1", Scope::Workspace(ws), Category::Note),
            ("f1", Scope::Global, Category::Fact),
            ("p1", Scope::Global, Category::Profile),
            ("f2", Scope::Workspace(ws), Category::Fact),
            ("o1", Scope::Workspace(other), Category::Fact),
        ] {
            at += 1;
            let mut c = change(&format!("op-{id}"), create(id, scope, category, id));
            c.now_ms = at;
            memory.apply(c).unwrap();
        }
        assert_eq!(memory.scope_revision(Scope::Global).unwrap(), 2);
        assert_eq!(memory.scope_revision(Scope::Workspace(ws)).unwrap(), 2);
        assert_eq!(memory.scope_revision(Scope::Workspace(other)).unwrap(), 1);
        let ids = |filter| {
            memory
                .list(filter, false)
                .unwrap()
                .into_iter()
                .map(|v| v.entry.id)
                .collect::<Vec<_>>()
        };
        // Profile, then fact, then note; newest first inside a category.
        assert_eq!(ids(ScopeFilter::Both(ws)), ["p1", "f2", "f1", "n1"]);
        assert_eq!(ids(ScopeFilter::Global), ["p1", "f1"]);
        assert_eq!(ids(ScopeFilter::Workspace(ws)), ["f2", "n1"]);
        assert_eq!(ids(ScopeFilter::Any).len(), 5);
        storage.close();
    }

    fn proposal(id: &str, operation: Operation, action_entry: Option<(&str, u64)>) -> ProposalRow {
        ProposalRow {
            id: id.to_owned(),
            operation_id: format!("prop-{id}"),
            scope: Scope::Global,
            operation,
            entry_id: action_entry.map(|(e, _)| e.to_owned()),
            expected_revision: action_entry.map(|(_, r)| r),
            category: Category::Fact,
            body: "proposed".to_owned(),
            provenance_json: r#"{"author":"agent"}"#.to_owned(),
            state: ProposalState::Pending,
            origin_chat_session_id: Some(SessionId::new()),
            created_ms: 5,
            resolved_ms: None,
        }
    }

    #[test]
    fn a_proposal_is_applied_rejected_or_left_visible_as_a_conflict_in_one_transaction() {
        let (_dir, storage) = open();
        let memory = storage.memory();
        // A duplicate operation id is the same proposal.
        let stored = memory
            .insert_proposal(proposal("p1", Operation::Create, None))
            .unwrap();
        let again = memory
            .insert_proposal(ProposalRow {
                id: "p1-again".into(),
                ..proposal("p1", Operation::Create, None)
            })
            .unwrap();
        assert_eq!(again.id, stored.id);
        assert_eq!(memory.proposals(0).unwrap().len(), 1);

        let approve = |entry: &str| Change {
            approval_id: Some("p1".into()),
            author: Author::Agent,
            ..change(
                "proposal:p1",
                create(entry, Scope::Global, Category::Fact, "proposed"),
            )
        };
        let done = memory
            .resolve_proposal("p1", Resolution::Apply(approve("e1")), 9)
            .unwrap();
        assert_eq!(done.proposal.state, ProposalState::Applied);
        assert_eq!(done.receipt.unwrap().revision, 1);
        let revision = &memory.history("e1").unwrap()[0];
        assert_eq!(
            (revision.author, revision.approval_id.as_deref()),
            (Author::Agent, Some("p1"))
        );
        // Answered once only.
        let twice = memory.resolve_proposal("p1", Resolution::Reject, 10);
        assert!(matches!(twice, Err(MemoryError::InvalidState(_))));

        // A proposal against a revision that moved on stays visible as a conflict, applying nothing.
        memory
            .insert_proposal(proposal("p2", Operation::Update, Some(("e1", 1))))
            .unwrap();
        memory
            .apply(change(
                "user-edit",
                Action::Update {
                    entry_id: "e1".into(),
                    expected_revision: 1,
                    body: "user's".into(),
                },
            ))
            .unwrap();
        let stale = Change {
            author: Author::Agent,
            ..change(
                "proposal:p2",
                Action::Update {
                    entry_id: "e1".into(),
                    expected_revision: 1,
                    body: "proposed".into(),
                },
            )
        };
        let conflict = memory
            .resolve_proposal("p2", Resolution::Apply(stale), 11)
            .unwrap();
        assert_eq!(conflict.proposal.state, ProposalState::Conflict);
        assert!(conflict.receipt.is_none());
        assert_eq!(memory.get("e1").unwrap().unwrap().current.body, "user's");
        assert_eq!(memory.history("e1").unwrap().len(), 2);

        // Rejecting never touches the entries; resolved ones list after the pending.
        memory
            .insert_proposal(proposal("p3", Operation::Create, None))
            .unwrap();
        memory
            .insert_proposal(ProposalRow {
                operation_id: "prop-p4".into(),
                created_ms: 6,
                ..proposal("p4", Operation::Create, None)
            })
            .unwrap();
        let rejected = memory
            .resolve_proposal("p3", Resolution::Reject, 12)
            .unwrap();
        assert_eq!(rejected.proposal.state, ProposalState::Rejected);
        let listed = memory.proposals(10).unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|p| (p.id.as_str(), p.state))
                .collect::<Vec<_>>(),
            [
                ("p4", ProposalState::Pending),
                ("p1", ProposalState::Applied),
                ("p2", ProposalState::Conflict),
                ("p3", ProposalState::Rejected),
            ]
        );
        storage.close();
    }

    #[test]
    fn a_proposal_that_cannot_apply_for_another_reason_stays_pending() {
        let (_dir, storage) = open();
        let memory = storage.memory();
        memory
            .insert_proposal(proposal("p1", Operation::Create, None))
            .unwrap();
        let full = Change {
            max_entries: 0,
            ..change(
                "proposal:p1",
                create("e1", Scope::Global, Category::Fact, "proposed"),
            )
        };
        let refused = memory.resolve_proposal("p1", Resolution::Apply(full), 9);
        assert!(matches!(refused, Err(MemoryError::Full { max: 0 })));
        assert_eq!(
            memory.proposal("p1").unwrap().unwrap().state,
            ProposalState::Pending
        );
        storage.close();
    }

    #[test]
    fn snapshots_are_stored_per_chat_newest_last() {
        let (_dir, storage) = open();
        let memory = storage.memory();
        let chat = SessionId::new();
        let snapshot = |id: &str, at| SnapshotRow {
            id: id.to_owned(),
            chat_session_id: chat,
            backend: "omp".to_owned(),
            manifest_json: "{}".to_owned(),
            rendered: format!("context {id}"),
            content_hash: crate::hash_text(id),
            created_ms: at,
        };
        assert!(memory.latest_snapshot(chat).unwrap().is_none());
        memory.insert_snapshot(snapshot("s1", 1)).unwrap();
        memory.insert_snapshot(snapshot("s2", 2)).unwrap();
        assert_eq!(memory.latest_snapshot(chat).unwrap().unwrap().id, "s2");
        assert_eq!(
            memory
                .snapshots(chat)
                .unwrap()
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["s1", "s2"]
        );
        assert!(memory.snapshots(SessionId::new()).unwrap().is_empty());
        storage.close();
    }
}
