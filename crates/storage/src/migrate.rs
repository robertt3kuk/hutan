//! Forward-only embedded migrations with a pre-migration backup (ADR 0006).
//!
//! The schema version lives in `app_meta` (`schema_version`). Pending migrations run one per
//! transaction together with the version bump, so a failing migration rolls back completely and
//! the database stays at the last good version.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};
use tracing::{info, warn};

use crate::error::{Result, StorageError};

pub const SCHEMA_VERSION_KEY: &str = "schema_version";

/// Backups kept in `backups/`; older ones are deleted after a new one is made.
pub const BACKUPS_KEPT: usize = 5;

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// 1-based and consecutive.
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Migrations shipped in this build.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "projects",
        sql: include_str!("../migrations/0002_projects.sql"),
    },
    Migration {
        version: 3,
        name: "chat",
        sql: include_str!("../migrations/0003_chat.sql"),
    },
    Migration {
        version: 4,
        name: "agent_trees",
        sql: include_str!("../migrations/0004_agent_trees.sql"),
    },
    Migration {
        version: 5,
        name: "providers",
        sql: include_str!("../migrations/0005_providers.sql"),
    },
    Migration {
        version: 6,
        name: "worktrees",
        sql: include_str!("../migrations/0006_worktrees.sql"),
    },
    Migration {
        version: 7,
        name: "tasks",
        sql: include_str!("../migrations/0007_tasks.sql"),
    },
    Migration {
        version: 8,
        name: "projects",
        sql: include_str!("../migrations/0008_projects.sql"),
    },
    Migration {
        version: 9,
        name: "questions",
        sql: include_str!("../migrations/0009_questions.sql"),
    },
    Migration {
        version: 10,
        name: "task_delivery",
        sql: include_str!("../migrations/0010_task_delivery.sql"),
    },
    Migration {
        version: 11,
        name: "session_closed",
        sql: include_str!("../migrations/0011_session_closed.sql"),
    },
    Migration {
        version: 12,
        name: "control_grants",
        sql: include_str!("../migrations/0012_control_grants.sql"),
    },
    Migration {
        version: 13,
        name: "memory",
        sql: include_str!("../migrations/0013_memory.sql"),
    },
    Migration {
        version: 14,
        name: "checkpoints",
        sql: include_str!("../migrations/0014_checkpoints.sql"),
    },
    Migration {
        version: 15,
        name: "chat_queue",
        sql: include_str!("../migrations/0015_chat_queue.sql"),
    },
    Migration {
        version: 16,
        name: "checkpoint_during",
        sql: include_str!("../migrations/0016_checkpoint_during.sql"),
    },
    Migration {
        version: 17,
        name: "lsp_trust",
        sql: include_str!("../migrations/0017_lsp_trust.sql"),
    },
    Migration {
        version: 18,
        name: "checkpoint_through",
        sql: include_str!("../migrations/0018_checkpoint_through.sql"),
    },
    Migration {
        version: 19,
        name: "usage",
        sql: include_str!("../migrations/0019_usage.sql"),
    },
];

/// Schema version recorded in the database; 0 for a database without `app_meta`.
pub fn current_version(conn: &Connection) -> Result<u32> {
    let context = "reading the schema version";
    let has_meta: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'app_meta')",
            [],
            |row| row.get(0),
        )
        .map_err(StorageError::sqlite(context))?;
    if !has_meta {
        return Ok(0);
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [SCHEMA_VERSION_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(StorageError::sqlite(context))?;
    match value {
        None => Ok(0),
        Some(text) => text
            .parse()
            .map_err(|_| StorageError::corrupt("app_meta.schema_version", format!("`{text}`"))),
    }
}

/// Brings the database at `path` up to the newest of `migrations`, backing it up first when
/// there is anything to migrate.
pub fn run(conn: &mut Connection, path: &Path, migrations: &[Migration]) -> Result<()> {
    let supported = migrations.last().map_or(0, |m| m.version);
    debug_assert!(
        migrations
            .iter()
            .zip(1..)
            .all(|(migration, expected)| migration.version == expected),
        "migrations must be numbered 1..n without gaps"
    );
    let from = current_version(conn)?;
    if from > supported {
        return Err(StorageError::SchemaTooNew {
            path: path.to_owned(),
            found: from,
            supported,
        });
    }
    if from == supported {
        return Ok(());
    }

    // A fresh database has nothing worth keeping.
    let backup = if from > 0 {
        Some(backup(conn, path, from)?)
    } else {
        None
    };
    // A migration may rebuild a table other tables refer to (SQLite cannot change a check any
    // other way); with foreign keys on, dropping the old table would cascade into its
    // dependents. The pragma is a no-op inside a transaction, so it is set around them all.
    let failed = |version, name: &str, source| StorageError::Migration {
        version,
        name: name.to_owned(),
        backup: backup.clone(),
        source,
    };
    let migrated = (|| {
        conn.pragma_update(None, "foreign_keys", false)
            .map_err(|source| failed(from + 1, "switching foreign keys off", source))?;
        for migration in migrations.iter().filter(|m| m.version > from) {
            apply(conn, migration)
                .map_err(|source| failed(migration.version, migration.name, source))?;
            info!(
                version = migration.version,
                name = migration.name,
                "migrated"
            );
        }
        Ok(())
    })();
    let restored = conn
        .pragma_update(None, "foreign_keys", true)
        .map_err(StorageError::sqlite("switching foreign keys back on"));
    migrated?;
    restored
}

fn apply(conn: &mut Connection, migration: &Migration) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(migration.sql)?;
    tx.execute(
        "INSERT INTO app_meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        (SCHEMA_VERSION_KEY, migration.version.to_string()),
    )?;
    tx.commit()
}

/// Consistent copy (`VACUUM INTO`, so WAL content is included) in `backups/` next to the
/// database; keeps the newest [`BACKUPS_KEPT`].
fn backup(conn: &Connection, path: &Path, from: u32) -> Result<PathBuf> {
    let dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("backups");
    fs::create_dir_all(&dir).map_err(StorageError::io(format!(
        "creating backup directory {}",
        dir.display()
    )))?;
    let mut stamp = now_ms();
    let target = loop {
        let candidate = dir.join(backup_name(stamp, from));
        if !candidate.exists() {
            break candidate;
        }
        stamp += 1;
    };
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy()])
        .map_err(StorageError::sqlite(format!(
            "backing up the database to {}",
            target.display()
        )))?;
    info!(backup = %target.display(), from, "database backed up before migrating");
    prune_backups(&dir);
    Ok(target)
}

fn backup_name(stamp_ms: u64, from: u32) -> String {
    // Zero-padded so names sort chronologically.
    format!("hutan-{stamp_ms:013}-v{from}.db")
}

/// The stamp of a backup this module made (`hutan-<stamp>-v<version>.db`); `None` for any
/// other file, which pruning then leaves alone: a database a newer Hutan wrote, moved aside by
/// "start with empty data" (`hutan-<stamp>-v<found>-newer.db`), is never deleted.
fn backup_stamp(name: &str) -> Option<u64> {
    let (stamp, version) = name
        .strip_prefix("hutan-")?
        .strip_suffix(".db")?
        .split_once("-v")?;
    version.parse::<u32>().ok()?;
    stamp.parse().ok()
}

fn prune_backups(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut backups: Vec<(u64, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let stamp = backup_stamp(&entry.file_name().to_string_lossy())?;
            Some((stamp, entry.path()))
        })
        .collect();
    backups.sort();
    let excess = backups.len().saturating_sub(BACKUPS_KEPT);
    for (_, old) in backups.drain(..excess) {
        if let Err(error) = fs::remove_file(&old) {
            warn!(backup = %old.display(), %error, "could not remove an old backup");
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_names_roundtrip_and_sort_by_time() {
        let name = backup_name(1_791_894_164_332, 3);
        assert_eq!(name, "hutan-1791894164332-v3.db");
        assert_eq!(backup_stamp(&name), Some(1_791_894_164_332));
        assert!(backup_name(999, 1) < backup_name(1_000, 1));
        assert_eq!(backup_stamp("other.db"), None);
    }

    #[test]
    fn pruning_never_counts_a_database_moved_aside() {
        assert_eq!(backup_stamp("hutan-0001791894164332-v13-newer.db"), None);
        assert_eq!(backup_stamp("hutan-0001791894164332-reset.db"), None);
        let dir = tempfile::tempdir().unwrap();
        let newer = dir.path().join("hutan-0000000000001-v13-newer.db");
        std::fs::write(&newer, "kept").unwrap();
        for stamp in 2..=(2 + 2 * 5_u64) {
            std::fs::write(dir.path().join(backup_name(stamp, 12)), "").unwrap();
        }
        prune_backups(dir.path());
        assert!(newer.exists(), "the moved-aside database survives pruning");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            BACKUPS_KEPT + 1
        );
    }
}
