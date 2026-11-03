//! The `dismissed_findings` table (schema v26, `docs/assistant.md` § Checks): the ids of the
//! findings the user dismissed.

use std::collections::HashMap;

use rusqlite::{Connection, params};

use crate::Storage;
use crate::error::{Result, StorageError};
use crate::sql::Write;

/// How many dismissed ids are kept (the newest).
pub const DISMISSED_FINDINGS_KEPT: u32 = 2000;

/// The findings the user dismissed.
#[derive(Debug, Clone, Copy)]
pub struct DismissedFindings<'a>(pub(crate) &'a Storage);

impl DismissedFindings<'_> {
    /// Remembers that the user dismissed `id` at `at_ms`, applied before it returns. A finding
    /// that is already dismissed keeps its first time. Answers the time it was dismissed.
    pub fn dismiss(&self, id: &str, at_ms: u64) -> Result<u64> {
        self.0.write_checked(Write::DismissFinding {
            id: id.to_owned(),
            at_ms,
        })?;
        let id = id.to_owned();
        self.0.read(move |conn| {
            conn.query_row(
                "SELECT dismissed_at_ms FROM dismissed_findings WHERE id = ?1",
                [&id],
                |row| row.get::<_, i64>(0),
            )
            .map(|ms| u64::try_from(ms).unwrap_or(0))
            .map_err(StorageError::sqlite("reading a dismissed finding"))
        })
    }

    /// Forgets the dismissal of `id`, applied before it returns: the finding is listed again.
    /// Answers whether it was dismissed.
    pub fn restore(&self, id: &str) -> Result<bool> {
        let key = id.to_owned();
        let was_dismissed = self.0.read(move |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM dismissed_findings WHERE id = ?1",
                [&key],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)
            .map_err(StorageError::sqlite("reading a dismissed finding"))
        })?;
        self.0
            .write_checked(Write::RestoreFinding { id: id.to_owned() })?;
        Ok(was_dismissed)
    }

    /// Every dismissed id with the time it was dismissed.
    pub fn all(&self) -> Result<HashMap<String, u64>> {
        self.0.read(|conn| {
            let context = "reading the dismissed findings";
            let mut stmt = conn
                .prepare("SELECT id, dismissed_at_ms FROM dismissed_findings")
                .map_err(StorageError::sqlite(context))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                    ))
                })
                .map_err(StorageError::sqlite(context))?;
            rows.collect::<std::result::Result<HashMap<_, _>, _>>()
                .map_err(StorageError::sqlite(context))
        })
    }
}

fn ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn dismiss(conn: &Connection, id: &str, at_ms: u64) -> Result<()> {
    let context = "writing a dismissed finding";
    conn.execute(
        "INSERT OR IGNORE INTO dismissed_findings (id, dismissed_at_ms) VALUES (?1, ?2)",
        params![id, ms(at_ms)],
    )
    .map_err(StorageError::sqlite(context))?;
    conn.execute(
        "DELETE FROM dismissed_findings WHERE id NOT IN \
         (SELECT id FROM dismissed_findings ORDER BY dismissed_at_ms DESC, id LIMIT ?1)",
        [i64::from(DISMISSED_FINDINGS_KEPT)],
    )
    .map_err(StorageError::sqlite(context))?;
    Ok(())
}

pub(crate) fn restore(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM dismissed_findings WHERE id = ?1", [id])
        .map_err(StorageError::sqlite("restoring a dismissed finding"))?;
    Ok(())
}
