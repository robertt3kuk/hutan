//! The `digest_runs` table (schema v24, `docs/assistant.md` § Catching up): the runs of the
//! scheduled model digest.

use hutan_core::SessionId;
use rusqlite::{Connection, params};

use crate::Storage;
use crate::error::{Result, StorageError};
use crate::sql::Write;

/// How many runs are kept.
pub const DIGEST_RUNS_KEPT: u32 = 50;

/// One run of the digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestRunRow {
    pub started_ms: u64,
    pub ended_ms: u64,
    /// `done`, `skipped`, `failed` or `timeout`.
    pub outcome: String,
    pub reason: Option<String>,
    pub session_id: Option<SessionId>,
    pub tokens: Option<u64>,
}

/// The runs of the scheduled model digest.
#[derive(Debug, Clone, Copy)]
pub struct DigestRuns<'a>(pub(crate) &'a Storage);

impl DigestRuns<'_> {
    /// Records a run that is over, applied before it returns (the schedule reads it next).
    pub fn append(&self, row: DigestRunRow) -> Result<()> {
        self.0.write_checked(Write::AppendDigestRun(Box::new(row)))
    }

    /// The newest `limit` runs, newest first.
    pub fn newest(&self, limit: u32) -> Result<Vec<DigestRunRow>> {
        self.0.read(move |conn| {
            let context = "reading the digest runs";
            let mut stmt = conn
                .prepare(
                    "SELECT started_ms, ended_ms, outcome, reason, session_id, tokens \
                     FROM digest_runs ORDER BY started_ms DESC, rowid DESC LIMIT ?1",
                )
                .map_err(StorageError::sqlite(context))?;
            let rows = stmt
                .query_map([i64::from(limit)], |row| {
                    Ok(DigestRunRow {
                        started_ms: u64::try_from(row.get::<_, i64>(0)?).unwrap_or(0),
                        ended_ms: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                        outcome: row.get(2)?,
                        reason: row.get(3)?,
                        session_id: row
                            .get::<_, Option<String>>(4)?
                            .and_then(|id| id.parse().ok()),
                        tokens: row
                            .get::<_, Option<i64>>(5)?
                            .map(|n| u64::try_from(n).unwrap_or(0)),
                    })
                })
                .map_err(StorageError::sqlite(context))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(StorageError::sqlite(context))
        })
    }
}

fn ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn append(conn: &Connection, row: &DigestRunRow) -> Result<()> {
    let context = "writing the digest runs";
    conn.execute(
        "INSERT INTO digest_runs (started_ms, ended_ms, outcome, reason, session_id, tokens) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            ms(row.started_ms),
            ms(row.ended_ms),
            row.outcome,
            row.reason,
            row.session_id.map(|id| id.to_string()),
            row.tokens.map(ms),
        ],
    )
    .map_err(StorageError::sqlite(context))?;
    conn.execute(
        "DELETE FROM digest_runs WHERE rowid NOT IN \
         (SELECT rowid FROM digest_runs ORDER BY started_ms DESC, rowid DESC LIMIT ?1)",
        [i64::from(DIGEST_RUNS_KEPT)],
    )
    .map_err(StorageError::sqlite(context))?;
    Ok(())
}
