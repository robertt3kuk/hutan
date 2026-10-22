//! The `checkpoints` table (schema v14, v16, v18, ADR 0019): one row per checkpoint of a chat
//! session.
//!
//! Writes are applied before they return (they go through the writer like a read): the daemon
//! writes a turn's row before the turn's first ref and reconciles rows with refs at start, so
//! a row must be on disk when its ref is.

use std::path::PathBuf;

use hutan_core::{
    CheckpointId, CheckpointInfo, CheckpointKind, CheckpointState, SessionId, SkippedFile, TurnId,
    WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Storage;
use crate::error::{Result, StorageError};

/// One checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRow {
    pub id: CheckpointId,
    pub session_id: SessionId,
    pub turn_id: Option<TurnId>,
    pub kind: CheckpointKind,
    pub state: CheckpointState,
    pub reason: Option<String>,
    /// The checkout's top level, or the session's folder when it is not in git.
    pub checkout: Option<PathBuf>,
    /// The repository's common git directory (where the refs and objects are); `None` when the
    /// folder is not in git.
    pub common_dir: Option<PathBuf>,
    /// The hidden ref that keeps the checkpoint's commits, while there is one.
    pub reference: Option<String>,
    /// The end commit (the ref's target once the turn ended).
    pub commit: Option<String>,
    pub start_commit: Option<String>,
    /// The end commit's tree.
    pub tree: Option<String>,
    /// The checkpoint this one follows: what a restore restored (CK-4), the first of its chain.
    pub parent_id: Option<CheckpointId>,
    /// The last checkpoint of a restore's chain (`through`, schema v18); `None` for one.
    pub through_id: Option<CheckpointId>,
    pub created_ms: u64,
    pub ended_ms: Option<u64>,
    pub files_changed: u64,
    pub lines_added: u64,
    pub lines_removed: u64,
    /// New content the checkpoint's snapshots hashed into the object store.
    pub bytes: u64,
    pub skipped: Vec<SkippedFile>,
    /// Files the user saved while the turn ran, relative to the checkout (schema v16).
    pub during: Vec<String>,
}

impl CheckpointRow {
    /// A checkpoint that has nothing recorded yet.
    pub fn new(
        id: CheckpointId,
        session_id: SessionId,
        kind: CheckpointKind,
        state: CheckpointState,
        created_ms: u64,
    ) -> Self {
        Self {
            id,
            session_id,
            turn_id: None,
            kind,
            state,
            reason: None,
            checkout: None,
            common_dir: None,
            reference: None,
            commit: None,
            start_commit: None,
            tree: None,
            parent_id: None,
            through_id: None,
            created_ms,
            ended_ms: None,
            files_changed: 0,
            lines_added: 0,
            lines_removed: 0,
            bytes: 0,
            skipped: Vec::new(),
            during: Vec::new(),
        }
    }

    /// What clients are told about it.
    pub fn info(&self) -> CheckpointInfo {
        CheckpointInfo {
            id: self.id,
            session_id: self.session_id,
            turn_id: self.turn_id,
            kind: self.kind,
            state: self.state,
            reason: self.reason.clone(),
            created_ms: self.created_ms,
            checkout: self.checkout.clone(),
            files_changed: self.files_changed,
            added: self.lines_added,
            removed: self.lines_removed,
            bytes: self.bytes,
            skipped: self.skipped.clone(),
            restores: self
                .parent_id
                .filter(|_| self.kind == CheckpointKind::Restore),
            restores_through: self
                .through_id
                .filter(|_| self.kind == CheckpointKind::Restore),
        }
    }
}

/// Which checkpoints a listing covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointFilter {
    All,
    Session(SessionId),
    /// The checkpoints of one repository (`Some(common_dir)`), or of folders outside git.
    Repository(Option<PathBuf>),
}

/// Whose checkpoints a page covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointScope {
    All,
    Session(SessionId),
    /// The sessions of a workspace, and of the projects whose workspace it is (sessions that
    /// were closed included, as long as their row is kept).
    Workspace(WorkspaceId),
}

/// One page of [`Checkpoints::page`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointPage<'a> {
    pub scope: CheckpointScope,
    /// Only checkpoints in these states.
    pub states: &'a [CheckpointState],
    /// Older than this one (by creation, then id).
    pub before: Option<CheckpointId>,
    pub limit: usize,
}

/// The checkpoints of chat sessions.
#[derive(Debug, Clone, Copy)]
pub struct Checkpoints<'a>(pub(crate) &'a Storage);

impl Checkpoints<'_> {
    /// Inserts the row or replaces it whole; applied before it returns.
    pub fn upsert(&self, row: CheckpointRow) -> Result<()> {
        self.0.read(move |conn| upsert(conn, &row))
    }

    /// Deletes the rows (missing ones are no error) in one transaction.
    pub fn delete(&self, ids: Vec<CheckpointId>) -> Result<()> {
        self.0.read(move |conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(StorageError::sqlite("deleting checkpoints"))?;
            for id in &ids {
                tx.execute("DELETE FROM checkpoints WHERE id = ?1", [id.to_string()])
                    .map_err(StorageError::sqlite("deleting a checkpoint"))?;
            }
            tx.commit()
                .map_err(StorageError::sqlite("deleting checkpoints"))
        })
    }

    pub fn get(&self, id: CheckpointId) -> Result<Option<CheckpointRow>> {
        self.0.read(move |conn| {
            conn.query_row(
                &format!("{SELECT} WHERE id = ?1"),
                [id.to_string()],
                |row| Ok(from_row(row)),
            )
            .optional()
            .map_err(StorageError::sqlite("reading a checkpoint"))?
            .transpose()
        })
    }

    /// Oldest first.
    pub fn list(&self, filter: CheckpointFilter) -> Result<Vec<CheckpointRow>> {
        self.0.read(move |conn| list(conn, &filter))
    }

    /// The repositories that have checkpoints.
    pub fn repositories(&self) -> Result<Vec<PathBuf>> {
        self.0.read(|conn| {
            let context = "reading checkpoint repositories";
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT common_dir FROM checkpoints \
                     WHERE common_dir IS NOT NULL ORDER BY common_dir",
                )
                .map_err(StorageError::sqlite(context))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(StorageError::sqlite(context))?;
            rows.map(|r| r.map(PathBuf::from).map_err(StorageError::sqlite(context)))
                .collect()
        })
    }

    /// Newest first: at most `page.limit` of the checkpoints of `page.scope` in `page.states`
    /// that are older than `page.before` (none when `before` names no checkpoint).
    pub fn page(&self, page: CheckpointPage<'_>) -> Result<Vec<CheckpointRow>> {
        let states: Vec<&'static str> = page.states.iter().map(|s| s.as_str()).collect();
        let scope = page.scope;
        let before = page.before.map(|id| id.to_string());
        let limit = i64::try_from(page.limit).unwrap_or(i64::MAX);
        self.0.read(move |conn| {
            let context = "reading checkpoints";
            let mut sql = format!("{SELECT} WHERE 1 = 1");
            let mut args: Vec<rusqlite::types::Value> = Vec::new();
            let arg = |value: rusqlite::types::Value, args: &mut Vec<_>| {
                args.push(value);
                format!("?{}", args.len())
            };
            match scope {
                CheckpointScope::All => {}
                CheckpointScope::Session(session) => {
                    let p = arg(session.to_string().into(), &mut args);
                    sql.push_str(&format!(" AND session_id = {p}"));
                }
                CheckpointScope::Workspace(workspace) => {
                    let p = arg(workspace.to_string().into(), &mut args);
                    sql.push_str(&format!(
                        " AND session_id IN (SELECT s.id FROM sessions s \
                         LEFT JOIN workspaces w ON w.id = s.workspace_id \
                         WHERE s.workspace_id = {p} OR w.parent_id = {p})"
                    ));
                }
            }
            if !states.is_empty() {
                let list: Vec<String> = states
                    .iter()
                    .map(|state| arg((*state).to_owned().into(), &mut args))
                    .collect();
                sql.push_str(&format!(" AND state IN ({})", list.join(", ")));
            }
            if let Some(before) = before {
                let p = arg(before.into(), &mut args);
                sql.push_str(&format!(
                    " AND (created_ms, id) < (SELECT created_ms, id FROM checkpoints WHERE id = {p})"
                ));
            }
            let p = arg(limit.into(), &mut args);
            sql.push_str(&format!(" ORDER BY created_ms DESC, id DESC LIMIT {p}"));
            let mut stmt = conn.prepare(&sql).map_err(StorageError::sqlite(context))?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(args.iter()), |row| {
                    Ok(from_row(row))
                })
                .map_err(StorageError::sqlite(context))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(StorageError::sqlite(context))??);
            }
            Ok(out)
        })
    }
}

const SELECT: &str = "SELECT id, session_id, turn_id, kind, state, reason, checkout, common_dir, \
     ref, commit_id, start_commit, tree, parent_id, created_ms, ended_ms, files_changed, \
     lines_added, lines_removed, bytes, skipped_json, during_json, through_id FROM checkpoints";

fn list(conn: &Connection, filter: &CheckpointFilter) -> Result<Vec<CheckpointRow>> {
    let context = "reading checkpoints";
    let order = "ORDER BY created_ms, id";
    let (sql, arg): (String, Option<String>) = match filter {
        CheckpointFilter::All => (format!("{SELECT} {order}"), None),
        CheckpointFilter::Session(session) => (
            format!("{SELECT} WHERE session_id = ?1 {order}"),
            Some(session.to_string()),
        ),
        CheckpointFilter::Repository(Some(dir)) => (
            format!("{SELECT} WHERE common_dir = ?1 {order}"),
            Some(dir.to_string_lossy().into_owned()),
        ),
        CheckpointFilter::Repository(None) => {
            (format!("{SELECT} WHERE common_dir IS NULL {order}"), None)
        }
    };
    let mut stmt = conn.prepare(&sql).map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(arg.iter()), |row| {
            Ok(from_row(row))
        })
        .map_err(StorageError::sqlite(context))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(StorageError::sqlite(context))??);
    }
    Ok(out)
}

fn upsert(conn: &Connection, row: &CheckpointRow) -> Result<()> {
    let skipped = serde_json::to_string(&row.skipped).map_err(|e| StorageError::Encode {
        what: "checkpoint skipped files",
        detail: e.to_string(),
    })?;
    let during = serde_json::to_string(&row.during).map_err(|e| StorageError::Encode {
        what: "checkpoint files saved during the turn",
        detail: e.to_string(),
    })?;
    let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.to_string_lossy().into_owned());
    conn.execute(
        "INSERT OR REPLACE INTO checkpoints (id, session_id, turn_id, kind, state, reason, \
             checkout, common_dir, ref, commit_id, start_commit, tree, parent_id, created_ms, \
             ended_ms, files_changed, lines_added, lines_removed, bytes, skipped_json, \
             during_json, through_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
             ?19, ?20, ?21, ?22)",
        params![
            row.id.to_string(),
            row.session_id.to_string(),
            row.turn_id.map(|t| t.to_string()),
            row.kind.as_str(),
            row.state.as_str(),
            row.reason,
            path(&row.checkout),
            path(&row.common_dir),
            row.reference,
            row.commit,
            row.start_commit,
            row.tree,
            row.parent_id.map(|p| p.to_string()),
            ms(row.created_ms),
            row.ended_ms.map(ms),
            ms(row.files_changed),
            ms(row.lines_added),
            ms(row.lines_removed),
            ms(row.bytes),
            skipped,
            during,
            row.through_id.map(|p| p.to_string()),
        ],
    )
    .map_err(StorageError::sqlite("writing a checkpoint"))?;
    Ok(())
}

fn from_row(row: &Row<'_>) -> Result<CheckpointRow> {
    let text = |index: usize| -> Result<Option<String>> {
        row.get::<_, Option<String>>(index)
            .map_err(StorageError::sqlite("reading a checkpoint"))
    };
    let int = |index: usize| -> Result<u64> {
        row.get::<_, i64>(index)
            .map(unms)
            .map_err(StorageError::sqlite("reading a checkpoint"))
    };
    let required = |index: usize, what: &str| -> Result<String> {
        text(index)?.ok_or_else(|| StorageError::corrupt(format!("checkpoints.{what}"), "null"))
    };
    let id =
        |what: &str, value: String| StorageError::corrupt(format!("checkpoints.{what}"), value);
    let kind = required(3, "kind")?;
    let state = required(4, "state")?;
    let skipped = required(19, "skipped_json")?;
    let during = required(20, "during_json")?;
    Ok(CheckpointRow {
        id: required(0, "id")?
            .parse()
            .map_err(|e| StorageError::corrupt("checkpoints.id", e))?,
        session_id: required(1, "session_id")?
            .parse()
            .map_err(|e| StorageError::corrupt("checkpoints.session_id", e))?,
        turn_id: text(2)?
            .map(|t| {
                t.parse()
                    .map_err(|e| StorageError::corrupt("checkpoints.turn_id", e))
            })
            .transpose()?,
        kind: CheckpointKind::parse(&kind).ok_or_else(|| id("kind", kind.clone()))?,
        state: CheckpointState::parse(&state).ok_or_else(|| id("state", state.clone()))?,
        reason: text(5)?,
        checkout: text(6)?.map(PathBuf::from),
        common_dir: text(7)?.map(PathBuf::from),
        reference: text(8)?,
        commit: text(9)?,
        start_commit: text(10)?,
        tree: text(11)?,
        parent_id: text(12)?
            .map(|p| {
                p.parse()
                    .map_err(|e| StorageError::corrupt("checkpoints.parent_id", e))
            })
            .transpose()?,
        through_id: text(21)?
            .map(|p| {
                p.parse()
                    .map_err(|e| StorageError::corrupt("checkpoints.through_id", e))
            })
            .transpose()?,
        created_ms: int(13)?,
        ended_ms: row
            .get::<_, Option<i64>>(14)
            .map_err(StorageError::sqlite("reading a checkpoint"))?
            .map(unms),
        files_changed: int(15)?,
        lines_added: int(16)?,
        lines_removed: int(17)?,
        bytes: int(18)?,
        skipped: serde_json::from_str(&skipped)
            .map_err(|e| StorageError::corrupt("checkpoints.skipped_json", e))?,
        during: serde_json::from_str(&during)
            .map_err(|e| StorageError::corrupt("checkpoints.during_json", e))?,
    })
}

fn ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn unms(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::SkipReason;

    fn row(session: SessionId, created_ms: u64, dir: Option<&str>) -> CheckpointRow {
        let mut row = CheckpointRow::new(
            CheckpointId::new(),
            session,
            CheckpointKind::Turn,
            CheckpointState::Ok,
            created_ms,
        );
        row.common_dir = dir.map(PathBuf::from);
        row
    }

    #[test]
    fn rows_round_trip_and_list_by_session_and_repository() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("hutan.sqlite")).unwrap();
        let (a, b) = (SessionId::new(), SessionId::new());
        let mut full = row(a, 20, Some("/r/.git"));
        full.turn_id = Some(TurnId::new());
        full.reason = Some("why".to_owned());
        full.checkout = Some(PathBuf::from("/r"));
        full.reference = Some("refs/hutan/checkpoints/a/t".to_owned());
        full.commit = Some("c".repeat(40));
        full.start_commit = Some("d".repeat(40));
        full.tree = Some("e".repeat(40));
        full.parent_id = Some(CheckpointId::new());
        full.through_id = Some(CheckpointId::new());
        full.ended_ms = Some(25);
        full.files_changed = 3;
        full.lines_added = 4;
        full.lines_removed = 5;
        full.bytes = 6;
        full.skipped = vec![SkippedFile {
            path: "big.bin".to_owned(),
            reason: SkipReason::TooLarge,
            bytes: Some(11),
        }];
        full.during = vec!["src/lib.rs".to_owned()];
        let older = row(a, 10, Some("/r/.git"));
        let elsewhere = row(b, 15, None);
        for r in [&full, &older, &elsewhere] {
            storage.checkpoints().upsert(r.clone()).unwrap();
        }
        let checkpoints = storage.checkpoints();
        assert_eq!(checkpoints.get(full.id).unwrap().as_ref(), Some(&full));
        assert_eq!(
            checkpoints.list(CheckpointFilter::Session(a)).unwrap(),
            vec![older.clone(), full.clone()]
        );
        assert_eq!(
            checkpoints
                .list(CheckpointFilter::Repository(None))
                .unwrap(),
            vec![elsewhere.clone()]
        );
        assert_eq!(
            checkpoints
                .list(CheckpointFilter::Repository(Some("/r/.git".into())))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            checkpoints.repositories().unwrap(),
            vec![PathBuf::from("/r/.git")]
        );

        let mut ended = older.clone();
        ended.state = CheckpointState::Unchanged;
        checkpoints.upsert(ended.clone()).unwrap();
        assert_eq!(checkpoints.get(older.id).unwrap(), Some(ended));
        checkpoints.delete(vec![older.id, full.id]).unwrap();
        assert_eq!(
            checkpoints.list(CheckpointFilter::All).unwrap(),
            vec![elsewhere]
        );
        assert_eq!(full.info().added, 4);
    }

    #[test]
    fn a_page_is_newest_first_per_scope_and_state_and_continues_before_a_checkpoint() {
        use crate::{CreatedBy, SessionRow, WorkspaceKind, WorkspaceRow};
        use hutan_core::{SessionKind, SessionState};
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("hutan.sqlite")).unwrap();
        let workspace = |kind, parent| WorkspaceRow {
            id: WorkspaceId::new(),
            name: "w".to_owned(),
            roots: Vec::new(),
            kind,
            pinned: false,
            created_by: CreatedBy::User,
            created_at_ms: 1,
            last_active_ms: 1,
            position: 0,
            common_dir: None,
            parent_id: parent,
        };
        let container = workspace(WorkspaceKind::Workspace, None);
        let project = workspace(WorkspaceKind::Project, Some(container.id));
        let elsewhere = workspace(WorkspaceKind::Project, None);
        for w in [&container, &project, &elsewhere] {
            storage.workspaces().insert(w.clone()).unwrap();
        }
        let session = |workspace: WorkspaceId| SessionRow {
            id: SessionId::new(),
            workspace_id: workspace,
            kind: SessionKind::Harness,
            title: "chat".to_owned(),
            title_locked: false,
            spec_json: "{}".to_owned(),
            cwd: None,
            state: SessionState::Idle,
            created_at_ms: 1,
            ended_at_ms: None,
            resume_ref_json: None,
        };
        let (a, b) = (session(project.id), session(elsewhere.id));
        storage.sessions().upsert(a.clone()).unwrap();
        storage.sessions().upsert(b.clone()).unwrap();
        storage.flush().unwrap();

        let mut rows = Vec::new();
        for (n, state) in [
            CheckpointState::Ok,
            CheckpointState::Unchanged,
            CheckpointState::Late,
            CheckpointState::Failed,
            CheckpointState::Ok,
        ]
        .into_iter()
        .enumerate()
        {
            let mut r = row(a.id, 10 * (n as u64 + 1), Some("/r/.git"));
            r.state = state;
            rows.push(r);
        }
        rows.push(row(b.id, 100, None));
        for r in &rows {
            storage.checkpoints().upsert(r.clone()).unwrap();
        }
        let listed = [
            CheckpointState::Ok,
            CheckpointState::Late,
            CheckpointState::Failed,
        ];
        let page = |scope, before, limit| {
            storage
                .checkpoints()
                .page(CheckpointPage {
                    scope,
                    states: &listed,
                    before,
                    limit,
                })
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            page(CheckpointScope::All, None, 10),
            [rows[5].id, rows[4].id, rows[3].id, rows[2].id, rows[0].id]
        );
        assert_eq!(
            page(CheckpointScope::Session(a.id), None, 2),
            [rows[4].id, rows[3].id]
        );
        assert_eq!(
            page(CheckpointScope::Session(a.id), Some(rows[3].id), 2),
            [rows[2].id, rows[0].id],
            "the next page starts below the cursor"
        );
        // The workspace takes the sessions of its projects.
        assert_eq!(
            page(CheckpointScope::Workspace(container.id), None, 10).len(),
            4
        );
        assert_eq!(
            page(CheckpointScope::Workspace(elsewhere.id), None, 10),
            [rows[5].id]
        );
        assert!(page(CheckpointScope::All, Some(CheckpointId::new()), 10).is_empty());
    }

    #[test]
    fn a_restore_tells_what_it_restored_and_a_turn_does_not() {
        let (turn, last) = (CheckpointId::new(), CheckpointId::new());
        let mut restore = row(SessionId::new(), 30, None);
        restore.kind = CheckpointKind::Restore;
        restore.parent_id = Some(turn);
        let info = restore.info();
        assert_eq!((info.restores, info.restores_through), (Some(turn), None));
        restore.through_id = Some(last);
        let info = restore.info();
        assert_eq!(
            (info.restores, info.restores_through),
            (Some(turn), Some(last))
        );

        let mut other = row(SessionId::new(), 30, None);
        other.parent_id = Some(turn);
        let info = other.info();
        assert_eq!((info.restores, info.restores_through), (None, None));
    }
}
