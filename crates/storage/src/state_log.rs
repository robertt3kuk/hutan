//! The `agent_state_log` table (schema v23, `docs/assistant.md` § Flow): the states agents
//! reported about themselves, so that a terminal agent's waits can be told afterwards.
//!
//! Appending is fire and forget (it goes through the writer like every write); reads and the
//! emptying go through the writer too, after everything queued before them.

use hutan_core::SessionId;
use rusqlite::{Connection, params};

use crate::Storage;
use crate::error::{Result, StorageError};
use crate::sql::Write;

/// How long a row is kept.
pub const STATE_LOG_KEPT_DAYS: u64 = 14;

/// One change of an agent's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStateRow {
    pub session_id: SessionId,
    pub at_ms: u64,
    /// Who said it: a hook's source (`omp-extension`, `claude-hooks`, …) or `osc7501`.
    pub source: String,
    /// `working`, `blocked`, `idle`, `done`, `error` or `unknown`.
    pub state: String,
    /// What a blocked agent waits for, in the agent's words (`permission`, `question`, …).
    pub kind: Option<String>,
    /// Redacted, at most 120 characters.
    pub message: Option<String>,
}

/// The log of agents' states.
#[derive(Debug, Clone, Copy)]
pub struct AgentStateLog<'a>(pub(crate) &'a Storage);

impl AgentStateLog<'_> {
    /// Queues a row (and the deletion of what is older than [`STATE_LOG_KEPT_DAYS`]).
    pub fn append(&self, row: AgentStateRow) -> Result<()> {
        self.0.write(Write::AppendAgentState(Box::new(row)))
    }

    /// Empties the log, applied before it returns.
    pub fn clear(&self) -> Result<()> {
        self.0.write_checked(Write::ClearAgentStates)
    }

    /// The rows written in `since_ms..=until_ms`, oldest first.
    pub fn in_window(&self, since_ms: u64, until_ms: u64) -> Result<Vec<AgentStateRow>> {
        self.0.read(move |conn| {
            read(
                conn,
                "SELECT session_id, at_ms, source, state, kind, message FROM agent_state_log \
                 WHERE at_ms BETWEEN ?1 AND ?2 ORDER BY at_ms, rowid",
                params![ms(since_ms), ms(until_ms)],
            )
        })
    }

    /// For every session that has one, the newest row before `before_ms`: what each agent was
    /// doing when a window began.
    pub fn latest_before(&self, before_ms: u64) -> Result<Vec<AgentStateRow>> {
        self.0.read(move |conn| {
            read(
                conn,
                "SELECT a.session_id, a.at_ms, a.source, a.state, a.kind, a.message \
                 FROM agent_state_log a WHERE a.at_ms < ?1 AND a.rowid = ( \
                     SELECT b.rowid FROM agent_state_log b \
                     WHERE b.session_id = a.session_id AND b.at_ms < ?1 \
                     ORDER BY b.at_ms DESC, b.rowid DESC LIMIT 1) \
                 ORDER BY a.at_ms, a.rowid",
                params![ms(before_ms)],
            )
        })
    }

    /// How many rows the log holds.
    pub fn count(&self) -> Result<u64> {
        self.0.read(|conn| {
            conn.query_row("SELECT COUNT(*) FROM agent_state_log", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|n| u64::try_from(n).unwrap_or(0))
            .map_err(StorageError::sqlite("counting the state log"))
        })
    }
}

pub(crate) fn append(conn: &Connection, row: &AgentStateRow) -> Result<()> {
    let context = "writing the state log";
    conn.execute(
        "INSERT INTO agent_state_log (session_id, at_ms, source, state, kind, message) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            row.session_id.to_string(),
            ms(row.at_ms),
            row.source,
            row.state,
            row.kind,
            row.message
        ],
    )
    .map_err(StorageError::sqlite(context))?;
    let cutoff = row.at_ms.saturating_sub(STATE_LOG_KEPT_DAYS * 86_400_000);
    conn.execute("DELETE FROM agent_state_log WHERE at_ms < ?1", [ms(cutoff)])
        .map_err(StorageError::sqlite(context))?;
    Ok(())
}

pub(crate) fn clear(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM agent_state_log", [])
        .map_err(StorageError::sqlite("emptying the state log"))?;
    Ok(())
}

fn read(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Vec<AgentStateRow>> {
    let context = "reading the state log";
    let mut stmt = conn.prepare(sql).map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(params, |row| {
            Ok((
                row.get::<_, String>(0)?,
                AgentStateRow {
                    session_id: SessionId::new(),
                    at_ms: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                    source: row.get(2)?,
                    state: row.get(3)?,
                    kind: row.get(4)?,
                    message: row.get(5)?,
                },
            ))
        })
        .map_err(StorageError::sqlite(context))?;
    let mut out = Vec::new();
    for row in rows {
        let (session, mut row) = row.map_err(StorageError::sqlite(context))?;
        row.session_id = session
            .parse()
            .map_err(|e| StorageError::corrupt("agent_state_log.session_id", e))?;
        out.push(row);
    }
    Ok(out)
}

fn ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
