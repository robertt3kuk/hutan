//! The index database: schema, and the statements the scanner and the API run.
//!
//! ```text
//! sources   one row per transcript file: path, size, mtime, offset, tail hash, format version,
//!           adapter state, counters
//! sessions  one row per indexed session (a transcript file, or a Hutan session), with the
//!           summary the session catalog shows (title, first prompt, latest turns, counts)
//! entries   one row per indexed chunk: role, timestamp, optional external key, and for a
//!           Hutan block row the terminal and block it came from
//! fts       FTS5 (body, idents), rowid = entries.id
//! provider_titles  titles an agent keeps outside its transcripts (Codex `session_index.jsonl`)
//! meta      small facts (last Hutan write)
//! ```
//!
//! The database is a derived cache: a schema version this build does not know drops it and
//! starts empty (the next scan rebuilds it from the transcripts). Versions 1 (before protocol
//! 0.15) and 2 (before the session catalog, protocol 0.29) are upgraded in place: the missing
//! columns are added, Hutan rows stay (nothing can derive them again; their catalog summary is
//! rebuilt from their rows), and the rows of the transcripts are dropped so that the next scan
//! reads every file again.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use tracing::{info, warn};

use crate::adapters::ParseState;
use crate::error::{IndexError, Result};
use crate::summary::{Summary, Turn};
use crate::types::{BlockRef, CatalogRow, Role, SessionText, Source, SourceStatus};

pub const SCHEMA_VERSION: u32 = 3;

/// The oldest schema [`open`] upgrades in place instead of rebuilding.
const UPGRADABLE_FROM: u32 = 1;

/// Columns of `sessions` the session catalog added (schema 3).
const SUMMARY_COLUMNS: &str = "
    ALTER TABLE sessions ADD COLUMN ptitle TEXT;
    ALTER TABLE sessions ADD COLUMN first_prompt TEXT;
    ALTER TABLE sessions ADD COLUMN latest TEXT;
    ALTER TABLE sessions ADD COLUMN messages INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE sessions ADD COLUMN model TEXT;
    ALTER TABLE sessions ADD COLUMN branch TEXT;
    ALTER TABLE sessions ADD COLUMN created_ms INTEGER;
    ALTER TABLE sessions ADD COLUMN updated_ms INTEGER;
    ALTER TABLE sessions ADD COLUMN modified_ms INTEGER;
    ALTER TABLE sessions ADD COLUMN subagent INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE provider_titles (
        source TEXT NOT NULL,
        key TEXT NOT NULL,
        title TEXT NOT NULL,
        PRIMARY KEY (source, key)
    );";

const SCHEMA: &str = "
CREATE TABLE sources (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    source TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    offset INTEGER NOT NULL,
    tail_hash INTEGER NOT NULL,
    format_version INTEGER NOT NULL,
    state TEXT NOT NULL,
    rows INTEGER NOT NULL DEFAULT 0,
    errors INTEGER NOT NULL DEFAULT 0,
    last_indexed_ms INTEGER NOT NULL
);
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    source TEXT NOT NULL,
    key TEXT NOT NULL,
    source_id INTEGER UNIQUE REFERENCES sources(id) ON DELETE CASCADE,
    title TEXT,
    cwd TEXT,
    ptitle TEXT,
    first_prompt TEXT,
    latest TEXT,
    messages INTEGER NOT NULL DEFAULT 0,
    model TEXT,
    branch TEXT,
    created_ms INTEGER,
    updated_ms INTEGER,
    modified_ms INTEGER,
    subagent INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX sessions_hutan_key ON sessions(source, key) WHERE source_id IS NULL;
CREATE TABLE entries (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    ts_ms INTEGER,
    ext_key TEXT,
    terminal_id INTEGER,
    block_id INTEGER
);
CREATE INDEX entries_session ON entries(session_id);
CREATE INDEX entries_ts ON entries(ts_ms);
CREATE UNIQUE INDEX entries_ext ON entries(session_id, ext_key) WHERE ext_key IS NOT NULL;
CREATE VIRTUAL TABLE fts USING fts5(body, idents, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE provider_titles (
    source TEXT NOT NULL,
    key TEXT NOT NULL,
    title TEXT NOT NULL,
    PRIMARY KEY (source, key)
);
";

/// Companion files SQLite keeps next to the database in WAL mode.
fn sidecars(path: &Path) -> [PathBuf; 2] {
    let with = |suffix: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    [with("-wal"), with("-shm")]
}

/// Deletes the database file and its WAL companions; missing files are fine.
pub fn remove_database(path: &Path) -> Result<()> {
    for file in std::iter::once(path.to_owned()).chain(sidecars(path)) {
        match fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(IndexError::io(format!("removing {}", file.display()))(e)),
        }
    }
    Ok(())
}

/// Opens (creating when missing) the index database at `path`, writable.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(IndexError::io(format!("creating {}", parent.display())))?;
    }
    let mut conn = open_raw(path)?;
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(IndexError::sqlite("reading the index schema version"))?;
    if (UPGRADABLE_FROM..SCHEMA_VERSION).contains(&version) {
        info!(
            found = version,
            wanted = SCHEMA_VERSION,
            "history index schema changed; upgrading it and re-reading the transcripts"
        );
        upgrade(&mut conn, version)?;
    } else if version != SCHEMA_VERSION {
        if version != 0 {
            info!(
                found = version,
                wanted = SCHEMA_VERSION,
                "history index schema changed; rebuilding it"
            );
            drop(conn);
            remove_database(path)?;
            conn = open_raw(path)?;
        }
        conn.execute_batch(SCHEMA)
            .map_err(IndexError::sqlite("creating the index schema"))?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(IndexError::sqlite("recording the index schema version"))?;
    }
    // The index holds the text of the user's conversations: private to the user.
    for file in std::iter::once(path.to_owned()).chain(sidecars(path)) {
        match fs::set_permissions(&file, fs::Permissions::from_mode(0o600)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(IndexError::io(format!("securing {}", file.display()))(e)),
        }
    }
    Ok(conn)
}

/// Schema 1 or 2 → 3: version 1 entries gain `terminal_id` and `block_id`; sessions gain the
/// catalog summary. Rows of transcripts are dropped (with their `sources` rows, so the stat
/// pass finds every file new and reads it again); Hutan rows are kept, without a block they
/// can point at, and their summaries are rebuilt from those rows.
fn upgrade(conn: &mut Connection, from: u32) -> Result<()> {
    let context = "upgrading the index schema";
    let tx = conn.transaction().map_err(IndexError::sqlite(context))?;
    if from == 1 {
        tx.execute_batch(
            "ALTER TABLE entries ADD COLUMN terminal_id INTEGER;
             ALTER TABLE entries ADD COLUMN block_id INTEGER;",
        )
        .map_err(IndexError::sqlite(context))?;
    }
    tx.execute_batch(SUMMARY_COLUMNS)
        .map_err(IndexError::sqlite(context))?;
    delete_external(&tx)?;
    rebuild_hutan_summaries(&tx)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(IndexError::sqlite(context))?;
    tx.commit().map_err(IndexError::sqlite(context))
}

/// Folds the stored rows of every Hutan session into its catalog summary again (what the
/// daemon's records would have built had the session catalog existed).
fn rebuild_hutan_summaries(conn: &Connection) -> Result<()> {
    let context = "rebuilding the summaries of Hutan sessions";
    let ids: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT id FROM sessions WHERE source = 'hutan'")
            .map_err(IndexError::sqlite(context))?;
        stmt.query_map([], |row| row.get(0))
            .map_err(IndexError::sqlite(context))?
            .collect::<Result<_, _>>()
            .map_err(IndexError::sqlite(context))?
    };
    for id in ids {
        let mut summary = Summary::default();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT e.role, e.ts_ms, f.body FROM entries e JOIN fts f ON f.rowid = e.id
                     WHERE e.session_id = ?1 AND (e.ext_key IS NULL OR e.ext_key NOT LIKE '%#%')
                     ORDER BY COALESCE(e.ts_ms, 0), e.id",
                )
                .map_err(IndexError::sqlite(context))?;
            let mut rows = stmt.query([id]).map_err(IndexError::sqlite(context))?;
            while let Some(row) = rows.next().map_err(IndexError::sqlite(context))? {
                let role: String = row.get(0).map_err(IndexError::sqlite(context))?;
                let ts: Option<i64> = row.get(1).map_err(IndexError::sqlite(context))?;
                let body: String = row.get(2).map_err(IndexError::sqlite(context))?;
                let ts = ts.and_then(|t| u64::try_from(t).ok());
                match role.parse().unwrap_or(Role::Tool) {
                    role @ (Role::User | Role::Assistant | Role::Command) => {
                        summary.note_message(role, ts, &body);
                    }
                    Role::Tool | Role::Output => summary.note_time(ts),
                }
            }
        }
        save_summary(conn, id, &summary, None)?;
    }
    Ok(())
}

fn open_raw(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .map_err(IndexError::sqlite(format!("opening {}", path.display())))?;
    let context = "configuring the index database";
    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(IndexError::sqlite(context))?;
    if !mode.eq_ignore_ascii_case("wal") {
        warn!(mode, "the index database did not enter WAL mode");
    }
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(IndexError::sqlite(context))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(IndexError::sqlite(context))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(IndexError::sqlite(context))?;
    Ok(conn)
}

/// A read-only connection for searches (WAL lets it run beside the writer).
pub fn open_read_only(path: &Path) -> Result<Connection> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(IndexError::sqlite(format!("opening {}", path.display())))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(IndexError::sqlite("configuring a search connection"))?;
    Ok(conn)
}

/// Size of the database plus its WAL, in bytes.
pub fn database_bytes(path: &Path) -> u64 {
    std::iter::once(path.to_owned())
        .chain([sidecars(path)[0].clone()])
        .filter_map(|file| fs::metadata(file).ok())
        .map(|meta| meta.len())
        .sum()
}

/// One row of `sources`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub id: i64,
    pub path: String,
    pub source: Source,
    pub size: u64,
    pub mtime_ns: i64,
    pub offset: u64,
    pub tail_hash: u64,
    pub format_version: u32,
    pub state: ParseState,
    pub rows: u64,
    pub errors: u64,
    pub last_indexed_ms: u64,
}

/// What the startup stat pass compares against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub mtime_ns: i64,
    pub format_version: u32,
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub fn source_row(conn: &Connection, path: &str) -> Result<Option<SourceRow>> {
    conn.query_row(
        "SELECT id, path, source, size, mtime_ns, offset, tail_hash, format_version, state, rows, errors, last_indexed_ms
         FROM sources WHERE path = ?1",
        [path],
        |row| {
            let source: String = row.get(2)?;
            let state: String = row.get(8)?;
            Ok(SourceRow {
                id: row.get(0)?,
                path: row.get(1)?,
                source: source.parse().unwrap_or(Source::Claude),
                size: to_u64(row.get(3)?),
                mtime_ns: row.get(4)?,
                offset: to_u64(row.get(5)?),
                // Stored as the signed reinterpretation of the unsigned hash.
                tail_hash: u64::from_ne_bytes(row.get::<_, i64>(6)?.to_ne_bytes()),
                format_version: u32::try_from(row.get::<_, i64>(7)?).unwrap_or(0),
                state: serde_json::from_str(&state).unwrap_or_default(),
                rows: to_u64(row.get(9)?),
                errors: to_u64(row.get(10)?),
                last_indexed_ms: to_u64(row.get(11)?),
            })
        },
    )
    .optional()
    .map_err(IndexError::sqlite("reading a source row"))
}

/// Every known file's stamp, for the stat pass.
pub fn all_stamps(conn: &Connection) -> Result<HashMap<String, Stamp>> {
    let context = "reading the sources table";
    let mut stmt = conn
        .prepare("SELECT path, size, mtime_ns, format_version FROM sources")
        .map_err(IndexError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Stamp {
                    size: to_u64(row.get(1)?),
                    mtime_ns: row.get(2)?,
                    format_version: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                },
            ))
        })
        .map_err(IndexError::sqlite(context))?;
    rows.collect::<Result<_, _>>()
        .map_err(IndexError::sqlite(context))
}

/// Inserts or replaces the row of `row.path`; returns its id.
pub fn upsert_source(conn: &Connection, row: &SourceRow) -> Result<i64> {
    let state = serde_json::to_string(&row.state).unwrap_or_else(|_| "{}".to_owned());
    conn.execute(
        "INSERT INTO sources (path, source, size, mtime_ns, offset, tail_hash, format_version, state, rows, errors, last_indexed_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(path) DO UPDATE SET
             source = excluded.source, size = excluded.size, mtime_ns = excluded.mtime_ns,
             offset = excluded.offset, tail_hash = excluded.tail_hash,
             format_version = excluded.format_version, state = excluded.state,
             rows = excluded.rows, errors = excluded.errors,
             last_indexed_ms = excluded.last_indexed_ms",
        params![
            row.path,
            row.source.as_str(),
            to_i64(row.size),
            row.mtime_ns,
            to_i64(row.offset),
            i64::from_ne_bytes(row.tail_hash.to_ne_bytes()),
            row.format_version,
            state,
            to_i64(row.rows),
            to_i64(row.errors),
            to_i64(row.last_indexed_ms),
        ],
    )
    .map_err(IndexError::sqlite("writing a source row"))?;
    conn.query_row("SELECT id FROM sources WHERE path = ?1", [&row.path], |r| {
        r.get(0)
    })
    .map_err(IndexError::sqlite("reading a source id"))
}

/// Removes every row indexed from one file (a rewritten or truncated transcript).
pub fn delete_file_rows(conn: &Connection, source_id: i64) -> Result<()> {
    let context = "removing the rows of a file";
    conn.execute(
        "DELETE FROM fts WHERE rowid IN
            (SELECT e.id FROM entries e JOIN sessions s ON s.id = e.session_id WHERE s.source_id = ?1)",
        [source_id],
    )
    .map_err(IndexError::sqlite(context))?;
    conn.execute(
        "DELETE FROM entries WHERE session_id IN (SELECT id FROM sessions WHERE source_id = ?1)",
        [source_id],
    )
    .map_err(IndexError::sqlite(context))?;
    conn.execute("DELETE FROM sessions WHERE source_id = ?1", [source_id])
        .map_err(IndexError::sqlite(context))?;
    Ok(())
}

/// Removes everything that came from transcript files (what `rebuild` re-creates); Hutan rows
/// stay, they cannot be derived again.
pub fn delete_external(conn: &Connection) -> Result<()> {
    let context = "clearing the transcript rows";
    conn.execute(
        "DELETE FROM fts WHERE rowid IN
            (SELECT e.id FROM entries e JOIN sessions s ON s.id = e.session_id WHERE s.source_id IS NOT NULL)",
        [],
    )
    .map_err(IndexError::sqlite(context))?;
    conn.execute(
        "DELETE FROM entries WHERE session_id IN (SELECT id FROM sessions WHERE source_id IS NOT NULL)",
        [],
    )
    .map_err(IndexError::sqlite(context))?;
    conn.execute("DELETE FROM sessions WHERE source_id IS NOT NULL", [])
        .map_err(IndexError::sqlite(context))?;
    conn.execute("DELETE FROM sources", [])
        .map_err(IndexError::sqlite(context))?;
    Ok(())
}

/// The session of `(source, key)` for a file (`source_id` given) or for Hutan (none); created
/// when missing. A given `title`/`cwd` replaces the stored one.
pub fn ensure_session(
    conn: &Connection,
    source: Source,
    key: &str,
    source_id: Option<i64>,
    title: Option<&str>,
    cwd: Option<&str>,
) -> Result<i64> {
    let context = "writing a session row";
    let existing: Option<i64> = match source_id {
        Some(id) => conn.query_row("SELECT id FROM sessions WHERE source_id = ?1", [id], |r| {
            r.get(0)
        }),
        None => conn.query_row(
            "SELECT id FROM sessions WHERE source = ?1 AND key = ?2 AND source_id IS NULL",
            params![source.as_str(), key],
            |r| r.get(0),
        ),
    }
    .optional()
    .map_err(IndexError::sqlite(context))?;
    match existing {
        Some(id) => {
            conn.execute(
                "UPDATE sessions SET key = ?2, title = COALESCE(?3, title), cwd = COALESCE(?4, cwd) WHERE id = ?1",
                params![id, key, title, cwd],
            )
            .map_err(IndexError::sqlite(context))?;
            Ok(id)
        }
        None => {
            conn.execute(
                "INSERT INTO sessions (source, key, source_id, title, cwd) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![source.as_str(), key, source_id, title, cwd],
            )
            .map_err(IndexError::sqlite(context))?;
            Ok(conn.last_insert_rowid())
        }
    }
}

/// Writes the catalog summary of a session (the session row exists). `modified_ms` is when the
/// transcript was last read; `None` keeps the stored value.
pub fn save_summary(
    conn: &Connection,
    session_id: i64,
    summary: &Summary,
    modified_ms: Option<u64>,
) -> Result<()> {
    let latest = if summary.latest.is_empty() {
        None
    } else {
        serde_json::to_string(&summary.latest).ok()
    };
    conn.execute(
        "UPDATE sessions SET ptitle = ?2, first_prompt = ?3, latest = ?4, messages = ?5,
             model = ?6, branch = ?7, created_ms = ?8, updated_ms = ?9,
             modified_ms = COALESCE(?10, modified_ms), subagent = ?11
         WHERE id = ?1",
        params![
            session_id,
            summary.provider_title(),
            summary.first_prompt,
            latest,
            summary.messages,
            summary.model,
            summary.branch,
            summary.created_ms.map(to_i64),
            summary.updated_ms.map(to_i64),
            modified_ms.map(to_i64),
            summary.subagent,
        ],
    )
    .map_err(IndexError::sqlite("writing a session summary"))?;
    Ok(())
}

/// The summary stored for a session (what a Hutan record is folded into).
pub fn load_summary(conn: &Connection, session_id: i64) -> Result<Summary> {
    conn.query_row(
        "SELECT ptitle, first_prompt, latest, messages, model, branch, created_ms, updated_ms, subagent
         FROM sessions WHERE id = ?1",
        [session_id],
        |row| {
            let latest: Option<String> = row.get(2)?;
            let time = |index| -> rusqlite::Result<Option<u64>> {
                Ok(row.get::<_, Option<i64>>(index)?.map(to_u64))
            };
            Ok(Summary {
                custom_title: None,
                generated_title: row.get(0)?,
                first_prompt: row.get(1)?,
                latest: latest
                    .and_then(|json| serde_json::from_str::<Vec<Turn>>(&json).ok())
                    .unwrap_or_default(),
                messages: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                model: row.get(4)?,
                branch: row.get(5)?,
                created_ms: time(6)?,
                updated_ms: time(7)?,
                subagent: row.get(8)?,
            })
        },
    )
    .map_err(IndexError::sqlite("reading a session summary"))
}

/// Conversation records of a Hutan session: chat messages and terminal commands, one per
/// record (the chunks of a long record are not counted again, a record pushed twice is one).
pub fn count_hutan_messages(conn: &Connection, session_id: i64) -> Result<u32> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entries
             WHERE session_id = ?1 AND role IN ('user', 'assistant', 'command')
               AND (ext_key IS NULL OR ext_key NOT LIKE '%#%')",
            [session_id],
            |row| row.get(0),
        )
        .map_err(IndexError::sqlite("counting the messages of a session"))?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Replaces the titles `source` keeps outside its transcripts (Codex `session_index.jsonl`).
pub fn replace_provider_titles(
    conn: &mut Connection,
    source: Source,
    titles: &[(String, String)],
) -> Result<()> {
    let context = "writing provider titles";
    let tx = conn.transaction().map_err(IndexError::sqlite(context))?;
    tx.execute(
        "DELETE FROM provider_titles WHERE source = ?1",
        [source.as_str()],
    )
    .map_err(IndexError::sqlite(context))?;
    {
        let mut insert = tx
            .prepare(
                "INSERT OR REPLACE INTO provider_titles (source, key, title) VALUES (?1, ?2, ?3)",
            )
            .map_err(IndexError::sqlite(context))?;
        for (key, title) in titles {
            insert
                .execute(params![source.as_str(), key, title])
                .map_err(IndexError::sqlite(context))?;
        }
    }
    tx.commit().map_err(IndexError::sqlite(context))
}

/// One row per session of `sources` that has something to show (a message, or a title the
/// agent gave), sub-agents left out. Two files that claim the same session id (a forked Codex
/// rollout copies its parent's header) are one session: the most recently updated wins.
pub fn catalog(conn: &Connection, sources: &[Source]) -> Result<Vec<CatalogRow>> {
    let context = "reading the session catalog";
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT s.source, s.key, s.title, s.ptitle, pt.title, s.first_prompt, s.latest, s.messages,
                s.model, s.branch, s.cwd, s.created_ms, s.updated_ms, s.modified_ms, src.path
         FROM sessions s
         LEFT JOIN sources src ON src.id = s.source_id
         LEFT JOIN provider_titles pt ON pt.source = s.source AND pt.key = s.key
         WHERE s.subagent = 0 AND s.source IN ({})
           AND (s.messages > 0
                OR (s.source <> 'hutan' AND (s.ptitle IS NOT NULL OR pt.title IS NOT NULL)))",
        vec!["?"; sources.len()].join(",")
    );
    let mut stmt = conn.prepare(&sql).map_err(IndexError::sqlite(context))?;
    let mut rows = stmt
        .query(rusqlite::params_from_iter(
            sources.iter().map(|s| s.as_str()),
        ))
        .map_err(IndexError::sqlite(context))?;
    let mut by_key: HashMap<(Source, String), CatalogRow> = HashMap::new();
    while let Some(row) = rows.next().map_err(IndexError::sqlite(context))? {
        let get = |e| IndexError::sqlite(context)(e);
        let source: String = row.get(0).map_err(get)?;
        let source: Source = source.parse().unwrap_or(Source::Claude);
        let key: String = row.get(1).map_err(get)?;
        let session_title: Option<String> = row.get(2).map_err(get)?;
        let provider: Option<String> = row.get(3).map_err(get)?;
        let indexed_title: Option<String> = row.get(4).map_err(get)?;
        let first_prompt: Option<String> = row.get(5).map_err(get)?;
        let latest: Option<String> = row.get(6).map_err(get)?;
        let time = |index: usize| -> Result<Option<u64>> {
            Ok(row.get::<_, Option<i64>>(index).map_err(get)?.map(to_u64))
        };
        let title = if source == Source::Hutan {
            session_title
        } else {
            // The title the agent keeps outside the transcript, the one in the transcript,
            // else what the first prompt says.
            indexed_title.or(provider).or_else(|| {
                first_prompt
                    .as_deref()
                    .and_then(crate::adapters::title_from)
            })
        }
        .filter(|title| !title.trim().is_empty());
        let item = CatalogRow {
            source,
            key: key.clone(),
            title,
            cwd: row.get(10).map_err(get)?,
            branch: row.get(9).map_err(get)?,
            model: row.get(8).map_err(get)?,
            messages: u32::try_from(row.get::<_, i64>(7).map_err(get)?).unwrap_or(0),
            created_ms: time(11)?,
            updated_ms: time(12)?,
            modified_ms: time(13)?,
            first_prompt,
            latest: latest
                .and_then(|json| serde_json::from_str::<Vec<Turn>>(&json).ok())
                .unwrap_or_default(),
            path: row.get(14).map_err(get)?,
        };
        let recency = |row: &CatalogRow| row.updated_ms.or(row.modified_ms).unwrap_or(0);
        match by_key.entry((source, key)) {
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                if recency(&item) > recency(slot.get()) {
                    slot.insert(item);
                }
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(item);
            }
        }
    }
    Ok(by_key.into_values().collect())
}

/// Adds one chunk. With an `ext_key`, an earlier row of the session with the same key is
/// replaced. `block` says which terminal block a Hutan command or output row came from.
#[expect(clippy::too_many_arguments, reason = "one row, one column each")]
pub fn insert_entry(
    conn: &Connection,
    session_id: i64,
    role: Role,
    ts_ms: Option<u64>,
    ext_key: Option<&str>,
    block: Option<BlockRef>,
    body: &str,
    idents: &str,
) -> Result<()> {
    let context = "writing an index row";
    conn.execute(
        "INSERT INTO entries (session_id, role, ts_ms, ext_key, terminal_id, block_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            session_id,
            role.as_str(),
            ts_ms.map(to_i64),
            ext_key,
            block.map(|b| to_i64(b.terminal_id)),
            block.map(|b| to_i64(b.block_id)),
        ],
    )
    .map_err(IndexError::sqlite(context))?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO fts (rowid, body, idents) VALUES (?1, ?2, ?3)",
        params![id, body, idents],
    )
    .map_err(IndexError::sqlite(context))?;
    Ok(())
}

/// Removes the rows of `(session, ext_key)`; a following insert replaces them.
pub fn delete_ext(conn: &Connection, session_id: i64, ext_key: &str) -> Result<()> {
    let context = "replacing index rows";
    // A record is stored as `ext_key`, `ext_key#1`, … so every chunk has a unique key.
    let like = format!(
        "{}#%",
        ext_key
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    conn.execute(
        "DELETE FROM fts WHERE rowid IN (SELECT id FROM entries WHERE session_id = ?1 AND (ext_key = ?2 OR ext_key LIKE ?3 ESCAPE '\\'))",
        params![session_id, ext_key, like],
    )
    .map_err(IndexError::sqlite(context))?;
    conn.execute(
        "DELETE FROM entries WHERE session_id = ?1 AND (ext_key = ?2 OR ext_key LIKE ?3 ESCAPE '\\')",
        params![session_id, ext_key, like],
    )
    .map_err(IndexError::sqlite(context))?;
    Ok(())
}

/// Drops rows older than `cutoff_ms` (the history range) and sessions left without rows.
pub fn prune_before(conn: &Connection, cutoff_ms: u64) -> Result<u64> {
    let context = "pruning old index rows";
    let cutoff = to_i64(cutoff_ms);
    conn.execute(
        "DELETE FROM fts WHERE rowid IN (SELECT id FROM entries WHERE ts_ms IS NOT NULL AND ts_ms < ?1)",
        [cutoff],
    )
    .map_err(IndexError::sqlite(context))?;
    let removed = conn
        .execute(
            "DELETE FROM entries WHERE ts_ms IS NOT NULL AND ts_ms < ?1",
            [cutoff],
        )
        .map_err(IndexError::sqlite(context))?;
    conn.execute(
        "DELETE FROM sessions WHERE NOT EXISTS (SELECT 1 FROM entries WHERE session_id = sessions.id)",
        [],
    )
    .map_err(IndexError::sqlite(context))?;
    Ok(u64::try_from(removed).unwrap_or(0))
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(IndexError::sqlite("writing index metadata"))?;
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
        .map_err(IndexError::sqlite("reading index metadata"))
}

/// How many transcript files each source has in `sources`.
pub fn file_counts(conn: &Connection) -> Result<HashMap<Source, u64>> {
    let context = "counting transcript files";
    let mut stmt = conn
        .prepare("SELECT source, COUNT(*) FROM sources GROUP BY source")
        .map_err(IndexError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(IndexError::sqlite(context))?;
    let mut counts = HashMap::new();
    for row in rows {
        let (source, count) = row.map_err(IndexError::sqlite(context))?;
        if let Ok(source) = source.parse() {
            counts.insert(source, to_u64(count));
        }
    }
    Ok(counts)
}

/// Per-source counters for `status`, one entry for each of `sources`.
pub fn source_status(conn: &Connection, sources: &[Source]) -> Result<Vec<SourceStatus>> {
    let context = "counting index rows";
    let mut rows_by_source: HashMap<String, u64> = HashMap::new();
    let mut stmt = conn
        .prepare("SELECT s.source, COUNT(*) FROM entries e JOIN sessions s ON s.id = e.session_id GROUP BY s.source")
        .map_err(IndexError::sqlite(context))?;
    let mut rows = stmt.query([]).map_err(IndexError::sqlite(context))?;
    while let Some(row) = rows.next().map_err(IndexError::sqlite(context))? {
        let source: String = row.get(0).map_err(IndexError::sqlite(context))?;
        let count: i64 = row.get(1).map_err(IndexError::sqlite(context))?;
        rows_by_source.insert(source, to_u64(count));
    }
    let mut files: HashMap<String, (u64, u64, u64)> = HashMap::new();
    let mut stmt = conn
        .prepare("SELECT source, COUNT(*), COALESCE(SUM(errors), 0), COALESCE(MAX(last_indexed_ms), 0) FROM sources GROUP BY source")
        .map_err(IndexError::sqlite(context))?;
    let mut rows = stmt.query([]).map_err(IndexError::sqlite(context))?;
    while let Some(row) = rows.next().map_err(IndexError::sqlite(context))? {
        let source: String = row.get(0).map_err(IndexError::sqlite(context))?;
        files.insert(
            source,
            (
                to_u64(row.get(1).map_err(IndexError::sqlite(context))?),
                to_u64(row.get(2).map_err(IndexError::sqlite(context))?),
                to_u64(row.get(3).map_err(IndexError::sqlite(context))?),
            ),
        );
    }
    let hutan_last = get_meta(conn, "hutan_last_indexed_ms")?.and_then(|v| v.parse::<u64>().ok());
    Ok(sources
        .iter()
        .map(|&source| {
            let (file_count, errors, last) =
                files.get(source.as_str()).copied().unwrap_or_default();
            let last_indexed_ms = if source == Source::Hutan {
                hutan_last
            } else {
                (last > 0).then_some(last)
            };
            SourceStatus {
                source,
                files: file_count,
                rows: rows_by_source.get(source.as_str()).copied().unwrap_or(0),
                last_indexed_ms,
                errors,
            }
        })
        .collect())
}

/// The newest `limit` rows of the sessions with `key` (of `sources` and `roles`, when given),
/// oldest first. Rows of one message are consecutive chunks of the same role.
pub fn session_text(
    conn: &Connection,
    key: &str,
    sources: &[Source],
    roles: &[Role],
    limit: usize,
) -> Result<Vec<SessionText>> {
    let context = "reading the rows of a session";
    let mut params: Vec<rusqlite::types::Value> =
        vec![rusqlite::types::Value::Text(key.to_owned())];
    let mut filters = String::new();
    if !sources.is_empty() {
        filters.push_str(&format!(
            " AND s.source IN ({})",
            vec!["?"; sources.len()].join(",")
        ));
        params.extend(
            sources
                .iter()
                .map(|s| rusqlite::types::Value::Text(s.as_str().to_owned())),
        );
    }
    if !roles.is_empty() {
        filters.push_str(&format!(
            " AND e.role IN ({})",
            vec!["?"; roles.len()].join(",")
        ));
        params.extend(
            roles
                .iter()
                .map(|r| rusqlite::types::Value::Text(r.as_str().to_owned())),
        );
    }
    params.push(rusqlite::types::Value::Integer(
        i64::try_from(limit.max(1)).unwrap_or(i64::MAX),
    ));
    let sql = format!(
        "SELECT s.source, s.title, s.cwd, e.role, e.ts_ms, f.body
         FROM entries e
         JOIN sessions s ON s.id = e.session_id
         JOIN fts f ON f.rowid = e.id
         WHERE s.key = ?{filters}
         ORDER BY COALESCE(e.ts_ms, 0) DESC, e.id DESC
         LIMIT ?"
    );
    let mut stmt = conn.prepare(&sql).map_err(IndexError::sqlite(context))?;
    let mut rows = stmt
        .query(rusqlite::params_from_iter(params))
        .map_err(IndexError::sqlite(context))?;
    let mut found = Vec::new();
    while let Some(row) = rows.next().map_err(IndexError::sqlite(context))? {
        let source: String = row.get(0).map_err(IndexError::sqlite(context))?;
        let role: String = row.get(3).map_err(IndexError::sqlite(context))?;
        let ts_ms: Option<i64> = row.get(4).map_err(IndexError::sqlite(context))?;
        found.push(SessionText {
            source: source.parse().unwrap_or(Source::Claude),
            title: row.get(1).map_err(IndexError::sqlite(context))?,
            cwd: row.get(2).map_err(IndexError::sqlite(context))?,
            role: role.parse().unwrap_or(Role::Tool),
            ts_ms: ts_ms.and_then(|t| u64::try_from(t).ok()),
            text: row.get(5).map_err(IndexError::sqlite(context))?,
        });
    }
    found.reverse();
    Ok(found)
}
