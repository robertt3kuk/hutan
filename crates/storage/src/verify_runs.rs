//! The `verify_runs` table (schema v25, `docs/assistant.md` § Checks › Verification): the runs of
//! `agents.verify`, Hutan's own run of a session's tests.
//!
//! A row is written when the run starts and completed when it ends, so a run that was in progress
//! when the daemon stopped is found again ([`VerifyRuns::abandon_running`]). Nothing here redacts:
//! the output tail arrives redacted from the daemon.

use hutan_core::SessionId;
use rusqlite::{Connection, Row, params};

use crate::Storage;
use crate::error::{Result, StorageError};

/// How many runs are kept per session.
pub const VERIFY_RUNS_KEPT_PER_SESSION: u32 = 20;

/// One run, as stored. `status` is `running`, `passed`, `failed`, `timed_out`, `cancelled` or
/// `error`; `requested_by` is `user`, `mawas` or `agent`; `source` is `caller`, `project`,
/// `setting` or `detected`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyRunRow {
    pub id: u64,
    pub session_id: SessionId,
    pub command: String,
    pub cwd: String,
    pub source: String,
    pub why: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub output_tail: String,
    pub output_truncated: bool,
    pub requested_by: String,
    pub forced: bool,
    pub error: Option<String>,
}

/// What a run ended with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyRunEnd {
    pub status: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub ended_ms: u64,
    pub duration_ms: u64,
    pub output_tail: String,
    pub output_truncated: bool,
    pub error: Option<String>,
}

/// The verification runs.
#[derive(Debug, Clone, Copy)]
pub struct VerifyRuns<'a>(pub(crate) &'a Storage);

const COLUMNS: &str = "id, session_id, command, cwd, source, why, status, exit_code, signal, \
                       started_ms, ended_ms, duration_ms, output_tail, output_truncated, \
                       requested_by, forced, error";

impl VerifyRuns<'_> {
    /// Records a run that starts now (`row.id` is ignored) and returns its id. Only the newest
    /// [`VERIFY_RUNS_KEPT_PER_SESSION`] runs of the session stay.
    pub fn start(&self, row: VerifyRunRow) -> Result<u64> {
        self.0.read(move |conn| start(conn, &row))
    }

    /// Completes the run `id`: applied before it returns.
    pub fn finish(&self, id: u64, end: VerifyRunEnd) -> Result<()> {
        self.0.read(move |conn| finish(conn, id, &end))
    }

    /// Completes every run still `running` as `error` ("the daemon stopped"), for a start of the
    /// daemon: nothing survives a restart. Returns how many there were.
    pub fn abandon_running(&self, now_ms: u64) -> Result<usize> {
        self.0.read(move |conn| {
            conn.execute(
                "UPDATE verify_runs SET status = 'error', ended_ms = ?1, \
                 duration_ms = MAX(0, ?1 - started_ms), \
                 error = 'the daemon stopped while the run was in progress' \
                 WHERE status = 'running'",
                [ms(now_ms)],
            )
            .map_err(StorageError::sqlite("closing the interrupted verify runs"))
        })
    }

    /// One run by its id.
    pub fn get(&self, id: u64) -> Result<Option<VerifyRunRow>> {
        self.0.read(move |conn| {
            let sql = format!("SELECT {COLUMNS} FROM verify_runs WHERE id = ?1");
            query(conn, &sql, params![ms(id)]).map(|mut rows| rows.pop())
        })
    }

    /// The newest `limit` runs of a session, newest first.
    pub fn of_session(&self, session: SessionId, limit: u32) -> Result<Vec<VerifyRunRow>> {
        self.0.read(move |conn| {
            let sql = format!(
                "SELECT {COLUMNS} FROM verify_runs WHERE session_id = ?1 \
                 ORDER BY started_ms DESC, id DESC LIMIT ?2"
            );
            query(conn, &sql, params![session.to_string(), i64::from(limit)])
        })
    }

    /// The newest run of a session, if it ever ran one.
    pub fn latest(&self, session: SessionId) -> Result<Option<VerifyRunRow>> {
        self.of_session(session, 1).map(|mut rows| rows.pop())
    }

    /// The newest run of each of `sessions` that has one (the others are absent), in no
    /// particular order: what a findings pass over many sessions asks.
    pub fn newest_for_sessions(&self, sessions: &[SessionId]) -> Result<Vec<VerifyRunRow>> {
        let sessions: Vec<String> = sessions.iter().map(ToString::to_string).collect();
        self.0.read(move |conn| {
            let mut found = Vec::new();
            let sql = format!(
                "SELECT {COLUMNS} FROM verify_runs WHERE session_id = ?1 \
                 ORDER BY started_ms DESC, id DESC LIMIT 1"
            );
            for session in sessions {
                found.extend(query(conn, &sql, params![session])?);
            }
            Ok(found)
        })
    }
}

fn ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn unms(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn query(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Vec<VerifyRunRow>> {
    let context = "reading the verify runs";
    let mut stmt = conn.prepare(sql).map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(args, row_of)
        .map_err(StorageError::sqlite(context))?;
    let mut out = Vec::new();
    for row in rows {
        // A row whose session id does not parse cannot be ours: skipped, never fatal.
        if let Some(row) = row.map_err(StorageError::sqlite(context))? {
            out.push(row);
        }
    }
    Ok(out)
}

fn row_of(row: &Row<'_>) -> rusqlite::Result<Option<VerifyRunRow>> {
    let Ok(session_id) = row.get::<_, String>(1)?.parse::<SessionId>() else {
        return Ok(None);
    };
    Ok(Some(VerifyRunRow {
        id: unms(row.get(0)?),
        session_id,
        command: row.get(2)?,
        cwd: row.get(3)?,
        source: row.get(4)?,
        why: row.get(5)?,
        status: row.get(6)?,
        exit_code: row.get(7)?,
        signal: row.get(8)?,
        started_ms: unms(row.get(9)?),
        ended_ms: row.get::<_, Option<i64>>(10)?.map(unms),
        duration_ms: row.get::<_, Option<i64>>(11)?.map(unms),
        output_tail: row.get(12)?,
        output_truncated: row.get(13)?,
        requested_by: row.get(14)?,
        forced: row.get(15)?,
        error: row.get(16)?,
    }))
}

fn start(conn: &Connection, row: &VerifyRunRow) -> Result<u64> {
    let context = "writing a verify run";
    conn.execute(
        "INSERT INTO verify_runs (session_id, command, cwd, source, why, status, started_ms, \
         requested_by, forced) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            row.session_id.to_string(),
            row.command,
            row.cwd,
            row.source,
            row.why,
            row.status,
            ms(row.started_ms),
            row.requested_by,
            row.forced,
        ],
    )
    .map_err(StorageError::sqlite(context))?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "DELETE FROM verify_runs WHERE session_id = ?1 AND id NOT IN \
         (SELECT id FROM verify_runs WHERE session_id = ?1 \
          ORDER BY started_ms DESC, id DESC LIMIT ?2)",
        params![
            row.session_id.to_string(),
            i64::from(VERIFY_RUNS_KEPT_PER_SESSION)
        ],
    )
    .map_err(StorageError::sqlite(context))?;
    Ok(unms(id))
}

fn finish(conn: &Connection, id: u64, end: &VerifyRunEnd) -> Result<()> {
    conn.execute(
        "UPDATE verify_runs SET status = ?2, exit_code = ?3, signal = ?4, ended_ms = ?5, \
         duration_ms = ?6, output_tail = ?7, output_truncated = ?8, error = ?9 WHERE id = ?1",
        params![
            ms(id),
            end.status,
            end.exit_code,
            end.signal,
            ms(end.ended_ms),
            ms(end.duration_ms),
            end.output_tail,
            end.output_truncated,
            end.error,
        ],
    )
    .map_err(StorageError::sqlite("completing a verify run"))?;
    Ok(())
}
