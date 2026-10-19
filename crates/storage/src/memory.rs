//! Mawas memory (slice MW-2, protocol 0.32, `docs/assistant.md` § Memory): curated entries, their
//! immutable revisions, the proposals of agents and the snapshots a backend was given.
//!
//! The rules that must hold whatever calls in live here, in one transaction per change:
//!
//! - a revision is never changed (the schema's triggers refuse it too);
//! - an update, archive or restore names the revision it was reviewed against and fails with
//!   [`MemoryError::Conflict`] when the entry has moved on;
//! - the same `operation_id` returns the receipt of the revision it created, whatever else it
//!   says;
//! - the change, its receipt (the revision row) and the scope's revision counter are written
//!   together;
//! - a proposal is applied (or rejected) only while it is pending, in the transaction that
//!   applies it.
//!
//! What is allowed to change what (the user, an approved proposal), the size of a body and what
//! may not be remembered are the daemon's rules, checked before a [`Change`] gets here.

use hutan_core::{SessionId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

use crate::Storage;
use crate::error::{Result, StorageError};

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            fn parse(text: &str, what: &'static str) -> Result<Self> {
                match text {
                    $($text => Ok(Self::$variant),)+
                    other => Err(StorageError::corrupt(what, other)),
                }
            }
        }
    };
}

text_enum!(
    /// What an entry is about.
    Category { Profile => "profile", Fact => "fact", Note => "note" }
);
text_enum!(
    /// Whether an entry is used.
    EntryState { Active => "active", Archived => "archived" }
);
text_enum!(
    /// What a revision did.
    Operation { Create => "create", Update => "update", Archive => "archive", Restore => "restore" }
);
text_enum!(
    /// Who wrote a revision.
    Author { User => "user", Agent => "agent" }
);
text_enum!(
    /// Where a proposal stands.
    ProposalState {
        Pending => "pending",
        Applied => "applied",
        Rejected => "rejected",
        Conflict => "conflict",
        Cancelled => "cancelled",
    }
);

/// Where an entry lives. Each scope has its own revision counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Global,
    Workspace(WorkspaceId),
}

impl Scope {
    fn key(self) -> String {
        match self {
            Self::Global => "global".to_owned(),
            Self::Workspace(id) => format!("workspace:{id}"),
        }
    }

    fn kind(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace(_) => "workspace",
        }
    }

    fn workspace(self) -> Option<String> {
        match self {
            Self::Global => None,
            Self::Workspace(id) => Some(id.to_string()),
        }
    }

    fn from_columns(kind: &str, workspace: Option<String>) -> Result<Self> {
        match (kind, workspace) {
            ("global", None) => Ok(Self::Global),
            ("workspace", Some(id)) => id
                .parse()
                .map(Self::Workspace)
                .map_err(|e| StorageError::corrupt("memory scope workspace", e)),
            (kind, _) => Err(StorageError::corrupt("memory scope", kind)),
        }
    }
}

/// Which scopes a listing covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeFilter {
    Any,
    Global,
    Workspace(WorkspaceId),
    /// The global entries and those of one workspace: what a Mawas chat of it is given.
    Both(WorkspaceId),
}

/// An entry as it is now (`memory_entries`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRow {
    pub id: String,
    pub scope: Scope,
    pub category: Category,
    pub current_revision: u64,
    pub state: EntryState,
    pub created_ms: u64,
    pub updated_ms: u64,
}

/// One immutable revision (`memory_revisions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionRow {
    pub entry_id: String,
    pub revision: u64,
    pub operation: Operation,
    pub body: String,
    /// SHA-256 of `body`, hex.
    pub content_hash: String,
    pub provenance_json: String,
    pub author: Author,
    pub approval_id: Option<String>,
    pub operation_id: String,
    /// The scope's revision counter right after this change.
    pub scope_revision: u64,
    pub created_ms: u64,
}

/// An entry with its current revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryView {
    pub entry: EntryRow,
    pub current: RevisionRow,
}

/// What to do to the memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Create {
        entry_id: String,
        scope: Scope,
        category: Category,
        body: String,
    },
    Update {
        entry_id: String,
        expected_revision: u64,
        body: String,
    },
    /// Stop using an entry; its body is kept in the new revision.
    Archive {
        entry_id: String,
        expected_revision: u64,
    },
    Restore {
        entry_id: String,
        expected_revision: u64,
    },
}

/// One change, ready to apply (already checked by the daemon).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Makes the change idempotent: repeating it returns the original receipt.
    pub operation_id: String,
    pub action: Action,
    pub author: Author,
    pub provenance_json: String,
    /// The approval (proposal) that allowed it.
    pub approval_id: Option<String>,
    pub now_ms: u64,
    /// Most active entries (a create or a restore past it is refused with [`MemoryError::Full`]).
    pub max_entries: u32,
}

/// What a committed change reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub entry_id: String,
    pub revision: u64,
    pub scope: Scope,
    pub scope_revision: u64,
    pub operation_id: String,
    /// The `operation_id` had been applied before: nothing changed now.
    pub duplicate: bool,
}

/// Why a change was not applied.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("memory entry {0} does not exist")]
    NotFound(String),
    #[error("the entry is at revision {current}, not {expected}: it changed since it was read")]
    Conflict { expected: u64, current: u64 },
    #[error("{0}")]
    InvalidState(String),
    #[error("memory is full: at most {max} active entries; archive one first")]
    Full { max: u32 },
    #[error(transparent)]
    Storage(#[from] StorageError),
}

impl From<rusqlite::Error> for MemoryError {
    fn from(source: rusqlite::Error) -> Self {
        StorageError::sqlite("memory")(source).into()
    }
}

/// A proposal of an agent (`memory_proposals`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalRow {
    pub id: String,
    pub operation_id: String,
    pub scope: Scope,
    pub operation: Operation,
    pub entry_id: Option<String>,
    pub expected_revision: Option<u64>,
    pub category: Category,
    pub body: String,
    pub provenance_json: String,
    pub state: ProposalState,
    pub origin_chat_session_id: Option<SessionId>,
    pub created_ms: u64,
    pub resolved_ms: Option<u64>,
}

/// What the user decided about a proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Reject,
    /// Apply this change (built by the daemon from the reviewed payload, possibly edited).
    Apply(Change),
}

/// What a proposal's resolution came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub proposal: ProposalRow,
    /// Present when it was applied.
    pub receipt: Option<Receipt>,
}

/// What a backend was given (`memory_snapshots`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRow {
    pub id: String,
    pub chat_session_id: SessionId,
    pub backend: String,
    pub manifest_json: String,
    pub rendered: String,
    pub content_hash: String,
    pub created_ms: u64,
}

/// SHA-256 of `text`, hex.
pub fn hash_text(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn unms(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// The memory repository.
#[derive(Debug, Clone, Copy)]
pub struct Memory<'a>(pub(crate) &'a Storage);

impl Memory<'_> {
    /// Applies `change` in one transaction.
    pub fn apply(&self, change: Change) -> std::result::Result<Receipt, MemoryError> {
        self.0.read_raw(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let receipt = apply_in(&tx, &change)?;
            tx.commit()?;
            Ok(receipt)
        })
    }

    /// Entries of the scopes `filter` covers, in the order a snapshot renders them: profile,
    /// then fact, then note; most recently changed first inside a category (insertion order breaks
    /// ties, so the order is deterministic). Archived entries only with `include_archived`.
    pub fn list(&self, filter: ScopeFilter, include_archived: bool) -> Result<Vec<EntryView>> {
        self.0
            .read(move |conn| list_entries(conn, filter, include_archived))
    }

    pub fn get(&self, entry_id: &str) -> Result<Option<EntryView>> {
        let id = entry_id.to_owned();
        self.0.read(move |conn| get_entry(conn, &id))
    }

    /// Every revision of an entry, newest first.
    pub fn history(&self, entry_id: &str) -> Result<Vec<RevisionRow>> {
        let id = entry_id.to_owned();
        self.0.read(move |conn| revisions_of(conn, &id))
    }

    /// The revision counter of a scope (0 before its first change).
    pub fn scope_revision(&self, scope: Scope) -> Result<u64> {
        self.0.read(move |conn| scope_revision(conn, scope))
    }

    /// Entries in use, of every scope.
    pub fn count_active(&self) -> Result<u32> {
        self.0.read(count_active)
    }

    /// Stores a proposal; one whose `operation_id` was stored before is returned as it is
    /// (and nothing is stored).
    pub fn insert_proposal(&self, row: ProposalRow) -> Result<ProposalRow> {
        self.0.read(move |conn| {
            if let Some(existing) = proposal_by_operation(conn, &row.operation_id)? {
                return Ok(existing);
            }
            conn.execute(
                "INSERT INTO memory_proposals (id, operation_id, scope_kind, workspace_id, \
                 operation, entry_id, expected_revision, category, body, provenance_json, state, \
                 origin_chat_session_id, created_ms, resolved_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    row.id,
                    row.operation_id,
                    row.scope.kind(),
                    row.scope.workspace(),
                    row.operation.as_str(),
                    row.entry_id,
                    row.expected_revision.map(ms),
                    row.category.as_str(),
                    row.body,
                    row.provenance_json,
                    row.state.as_str(),
                    row.origin_chat_session_id.map(|id| id.to_string()),
                    ms(row.created_ms),
                    row.resolved_ms.map(ms),
                ],
            )
            .map_err(StorageError::sqlite("storing a memory proposal"))?;
            Ok(row)
        })
    }

    /// The proposal stored for an `operation_id`, in any state.
    pub fn proposal_by_operation(&self, operation_id: &str) -> Result<Option<ProposalRow>> {
        let operation_id = operation_id.to_owned();
        self.0
            .read(move |conn| proposal_by_operation(conn, &operation_id))
    }

    pub fn proposal(&self, id: &str) -> Result<Option<ProposalRow>> {
        let id = id.to_owned();
        self.0.read(move |conn| proposal_by_id(conn, &id))
    }

    /// The pending proposals, oldest first; with `resolved` also the `resolved` most recently
    /// answered ones after them.
    pub fn proposals(&self, resolved: u32) -> Result<Vec<ProposalRow>> {
        self.0.read(move |conn| {
            let mut rows =
                proposals_where(conn, "state = 'pending' ORDER BY created_ms, id", params![])?;
            if resolved > 0 {
                let mut done = proposals_where(
                    conn,
                    "state <> 'pending' ORDER BY resolved_ms DESC, id DESC LIMIT ?1",
                    params![i64::from(resolved)],
                )?;
                done.reverse();
                rows.extend(done);
            }
            Ok(rows)
        })
    }

    /// Answers a pending proposal in one transaction: it is rejected, or its change is applied
    /// and the proposal marked applied. A change that cannot apply because the entry moved on
    /// ([`MemoryError::Conflict`]) or is gone ([`MemoryError::NotFound`]) leaves the proposal
    /// in state `conflict` / `cancelled`, visible; every other refusal (a full memory, an
    /// archived entry) leaves it pending and is the error.
    pub fn resolve_proposal(
        &self,
        id: &str,
        resolution: Resolution,
        now_ms: u64,
    ) -> std::result::Result<Resolved, MemoryError> {
        let id = id.to_owned();
        self.0.read_raw(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let Some(mut proposal) = proposal_by_id(&tx, &id)? else {
                return Err(MemoryError::NotFound(id));
            };
            if proposal.state != ProposalState::Pending {
                return Err(MemoryError::InvalidState(format!(
                    "the proposal was already answered ({})",
                    proposal.state.as_str()
                )));
            }
            let (state, receipt) = match resolution {
                Resolution::Reject => (ProposalState::Rejected, None),
                Resolution::Apply(change) => match apply_in(&tx, &change) {
                    Ok(receipt) => (ProposalState::Applied, Some(receipt)),
                    Err(MemoryError::Conflict { .. }) => (ProposalState::Conflict, None),
                    Err(MemoryError::NotFound(_)) => (ProposalState::Cancelled, None),
                    Err(other) => return Err(other),
                },
            };
            tx.execute(
                "UPDATE memory_proposals SET state = ?2, resolved_ms = ?3 WHERE id = ?1",
                params![id, state.as_str(), ms(now_ms)],
            )?;
            tx.commit()?;
            proposal.state = state;
            proposal.resolved_ms = Some(now_ms);
            Ok(Resolved { proposal, receipt })
        })
    }

    /// Stores what a backend was given.
    pub fn insert_snapshot(&self, row: SnapshotRow) -> Result<()> {
        self.0.read(move |conn| insert_snapshot(conn, &row))
    }

    /// The newest snapshot of a chat: its start context or the latest delta.
    pub fn latest_snapshot(&self, chat: SessionId) -> Result<Option<SnapshotRow>> {
        self.0.read(move |conn| {
            conn.query_row(
                "SELECT id, chat_session_id, backend, manifest_json, rendered, content_hash, \
                 created_ms FROM memory_snapshots WHERE chat_session_id = ?1 \
                 ORDER BY created_ms DESC, rowid DESC LIMIT 1",
                [chat.to_string()],
                |row| Ok(snapshot_from(row)),
            )
            .optional()
            .map_err(StorageError::sqlite("reading a memory snapshot"))?
            .transpose()
        })
    }

    /// Every snapshot of a chat, oldest first.
    pub fn snapshots(&self, chat: SessionId) -> Result<Vec<SnapshotRow>> {
        self.0.read(move |conn| {
            let context = "reading memory snapshots";
            let mut stmt = conn
                .prepare(
                    "SELECT id, chat_session_id, backend, manifest_json, rendered, content_hash, \
                     created_ms FROM memory_snapshots WHERE chat_session_id = ?1 \
                     ORDER BY created_ms, rowid",
                )
                .map_err(StorageError::sqlite(context))?;
            let rows = stmt
                .query_map([chat.to_string()], |row| Ok(snapshot_from(row)))
                .map_err(StorageError::sqlite(context))?;
            rows.map(|row| {
                row.map_err(StorageError::sqlite(context))
                    .and_then(|row| row)
            })
            .collect()
        })
    }
}

fn insert_snapshot(conn: &Connection, row: &SnapshotRow) -> Result<()> {
    conn.execute(
        "INSERT INTO memory_snapshots (id, chat_session_id, backend, manifest_json, rendered, \
         content_hash, created_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.id,
            row.chat_session_id.to_string(),
            row.backend,
            row.manifest_json,
            row.rendered,
            row.content_hash,
            ms(row.created_ms),
        ],
    )
    .map_err(StorageError::sqlite("storing a memory snapshot"))?;
    Ok(())
}

fn snapshot_from(row: &Row<'_>) -> Result<SnapshotRow> {
    let read = |e| StorageError::sqlite("reading a memory snapshot")(e);
    let chat: String = row.get(1).map_err(read)?;
    Ok(SnapshotRow {
        id: row.get(0).map_err(read)?,
        chat_session_id: chat
            .parse()
            .map_err(|e| StorageError::corrupt("memory_snapshots.chat_session_id", e))?,
        backend: row.get(2).map_err(read)?,
        manifest_json: row.get(3).map_err(read)?,
        rendered: row.get(4).map_err(read)?,
        content_hash: row.get(5).map_err(read)?,
        created_ms: unms(row.get(6).map_err(read)?),
    })
}

// Applying a change ----------------------------------------------------------------------------

fn revision_by_operation(
    conn: &Connection,
    operation_id: &str,
) -> Result<Option<(RevisionRow, Scope)>> {
    let revision = conn
        .query_row(
            &format!("{REVISION_SELECT} WHERE operation_id = ?1"),
            [operation_id],
            |row| Ok(revision_from(row)),
        )
        .optional()
        .map_err(StorageError::sqlite("reading a memory revision"))?
        .transpose()?;
    let Some(revision) = revision else {
        return Ok(None);
    };
    let entry = get_entry(conn, &revision.entry_id)?
        .ok_or_else(|| StorageError::corrupt("memory_revisions.entry_id", &revision.entry_id))?;
    Ok(Some((revision, entry.entry.scope)))
}

/// Applies `change` inside the transaction `conn` is in. Every check runs before the first
/// write, so a refusal leaves nothing behind.
fn apply_in(conn: &Connection, change: &Change) -> std::result::Result<Receipt, MemoryError> {
    if let Some((revision, scope)) = revision_by_operation(conn, &change.operation_id)? {
        return Ok(Receipt {
            entry_id: revision.entry_id,
            revision: revision.revision,
            scope,
            scope_revision: revision.scope_revision,
            operation_id: revision.operation_id,
            duplicate: true,
        });
    }
    let existing = |entry_id: &str, expected: u64| -> std::result::Result<EntryView, MemoryError> {
        let view =
            get_entry(conn, entry_id)?.ok_or_else(|| MemoryError::NotFound(entry_id.to_owned()))?;
        if view.entry.current_revision != expected {
            return Err(MemoryError::Conflict {
                expected,
                current: view.entry.current_revision,
            });
        }
        Ok(view)
    };
    let full = |conn: &Connection| -> std::result::Result<(), MemoryError> {
        if count_active(conn)? >= change.max_entries {
            return Err(MemoryError::Full {
                max: change.max_entries,
            });
        }
        Ok(())
    };
    let (operation, entry, body, revision) = match &change.action {
        Action::Create {
            entry_id,
            scope,
            category,
            body,
        } => {
            full(conn)?;
            let entry = EntryRow {
                id: entry_id.clone(),
                scope: *scope,
                category: *category,
                current_revision: 1,
                state: EntryState::Active,
                created_ms: change.now_ms,
                updated_ms: change.now_ms,
            };
            (Operation::Create, entry, body.clone(), 1)
        }
        Action::Update {
            entry_id,
            expected_revision,
            body,
        } => {
            let view = existing(entry_id, *expected_revision)?;
            if view.entry.state != EntryState::Active {
                return Err(MemoryError::InvalidState(
                    "the entry is archived: restore it before changing it".to_owned(),
                ));
            }
            let mut entry = view.entry;
            entry.current_revision += 1;
            entry.updated_ms = change.now_ms;
            let revision = entry.current_revision;
            (Operation::Update, entry, body.clone(), revision)
        }
        Action::Archive {
            entry_id,
            expected_revision,
        } => {
            let view = existing(entry_id, *expected_revision)?;
            if view.entry.state != EntryState::Active {
                return Err(MemoryError::InvalidState(
                    "the entry is already archived".to_owned(),
                ));
            }
            let mut entry = view.entry;
            entry.current_revision += 1;
            entry.state = EntryState::Archived;
            entry.updated_ms = change.now_ms;
            let revision = entry.current_revision;
            (Operation::Archive, entry, view.current.body, revision)
        }
        Action::Restore {
            entry_id,
            expected_revision,
        } => {
            let view = existing(entry_id, *expected_revision)?;
            if view.entry.state != EntryState::Archived {
                return Err(MemoryError::InvalidState(
                    "the entry is not archived".to_owned(),
                ));
            }
            full(conn)?;
            let mut entry = view.entry;
            entry.current_revision += 1;
            entry.state = EntryState::Active;
            entry.updated_ms = change.now_ms;
            let revision = entry.current_revision;
            (Operation::Restore, entry, view.current.body, revision)
        }
    };
    let scope_revision = bump_scope(conn, entry.scope)?;
    if operation == Operation::Create {
        conn.execute(
            "INSERT INTO memory_entries (id, scope_kind, workspace_id, category, \
             current_revision, state, created_ms, updated_ms) \
             VALUES (?1, ?2, ?3, ?4, 1, 'active', ?5, ?5)",
            params![
                entry.id,
                entry.scope.kind(),
                entry.scope.workspace(),
                entry.category.as_str(),
                ms(change.now_ms),
            ],
        )?;
    } else {
        conn.execute(
            "UPDATE memory_entries SET current_revision = ?2, state = ?3, updated_ms = ?4 \
             WHERE id = ?1",
            params![
                entry.id,
                ms(entry.current_revision),
                entry.state.as_str(),
                ms(change.now_ms),
            ],
        )?;
    }
    conn.execute(
        "INSERT INTO memory_revisions (entry_id, revision, operation, body, content_hash, \
         provenance_json, author_kind, approval_id, operation_id, scope_revision, created_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            entry.id,
            ms(revision),
            operation.as_str(),
            body,
            hash_text(&body),
            change.provenance_json,
            change.author.as_str(),
            change.approval_id,
            change.operation_id,
            ms(scope_revision),
            ms(change.now_ms),
        ],
    )?;
    Ok(Receipt {
        entry_id: entry.id,
        revision,
        scope: entry.scope,
        scope_revision,
        operation_id: change.operation_id.clone(),
        duplicate: false,
    })
}

fn bump_scope(conn: &Connection, scope: Scope) -> Result<u64> {
    let key = scope.key();
    conn.execute(
        "INSERT INTO memory_scopes (scope_key, revision) VALUES (?1, 1) \
         ON CONFLICT (scope_key) DO UPDATE SET revision = revision + 1",
        [&key],
    )
    .map_err(StorageError::sqlite("bumping a memory scope"))?;
    scope_revision(conn, scope)
}

fn scope_revision(conn: &Connection, scope: Scope) -> Result<u64> {
    let revision: Option<i64> = conn
        .query_row(
            "SELECT revision FROM memory_scopes WHERE scope_key = ?1",
            [scope.key()],
            |row| row.get(0),
        )
        .optional()
        .map_err(StorageError::sqlite("reading a memory scope"))?;
    Ok(revision.map_or(0, unms))
}

fn count_active(conn: &Connection) -> Result<u32> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_entries WHERE state = 'active'",
            [],
            |row| row.get(0),
        )
        .map_err(StorageError::sqlite("counting memory entries"))?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

// Reading --------------------------------------------------------------------------------------

const ENTRY_SELECT: &str = "SELECT e.id, e.scope_kind, e.workspace_id, e.category, \
    e.current_revision, e.state, e.created_ms, e.updated_ms, \
    r.entry_id, r.revision, r.operation, r.body, r.content_hash, r.provenance_json, \
    r.author_kind, r.approval_id, r.operation_id, r.scope_revision, r.created_ms \
    FROM memory_entries e JOIN memory_revisions r \
    ON r.entry_id = e.id AND r.revision = e.current_revision";

const REVISION_SELECT: &str = "SELECT entry_id, revision, operation, body, content_hash, \
    provenance_json, author_kind, approval_id, operation_id, scope_revision, created_ms \
    FROM memory_revisions";

fn entry_from(row: &Row<'_>) -> Result<EntryView> {
    let read = |e| StorageError::sqlite("reading a memory entry")(e);
    let kind: String = row.get(1).map_err(read)?;
    let workspace: Option<String> = row.get(2).map_err(read)?;
    let category: String = row.get(3).map_err(read)?;
    let state: String = row.get(5).map_err(read)?;
    Ok(EntryView {
        entry: EntryRow {
            id: row.get(0).map_err(read)?,
            scope: Scope::from_columns(&kind, workspace)?,
            category: Category::parse(&category, "memory_entries.category")?,
            current_revision: unms(row.get(4).map_err(read)?),
            state: EntryState::parse(&state, "memory_entries.state")?,
            created_ms: unms(row.get(6).map_err(read)?),
            updated_ms: unms(row.get(7).map_err(read)?),
        },
        current: revision_at(row, 8)?,
    })
}

fn revision_from(row: &Row<'_>) -> Result<RevisionRow> {
    revision_at(row, 0)
}

fn revision_at(row: &Row<'_>, at: usize) -> Result<RevisionRow> {
    let read = |e| StorageError::sqlite("reading a memory revision")(e);
    let operation: String = row.get(at + 2).map_err(read)?;
    let author: String = row.get(at + 6).map_err(read)?;
    Ok(RevisionRow {
        entry_id: row.get(at).map_err(read)?,
        revision: unms(row.get(at + 1).map_err(read)?),
        operation: Operation::parse(&operation, "memory_revisions.operation")?,
        body: row.get(at + 3).map_err(read)?,
        content_hash: row.get(at + 4).map_err(read)?,
        provenance_json: row.get(at + 5).map_err(read)?,
        author: Author::parse(&author, "memory_revisions.author_kind")?,
        approval_id: row.get(at + 7).map_err(read)?,
        operation_id: row.get(at + 8).map_err(read)?,
        scope_revision: unms(row.get(at + 9).map_err(read)?),
        created_ms: unms(row.get(at + 10).map_err(read)?),
    })
}

fn list_entries(
    conn: &Connection,
    filter: ScopeFilter,
    include_archived: bool,
) -> Result<Vec<EntryView>> {
    let context = "reading memory entries";
    let mut sql = ENTRY_SELECT.to_owned();
    let mut clauses: Vec<String> = Vec::new();
    let mut args: Vec<String> = Vec::new();
    match filter {
        ScopeFilter::Any => {}
        ScopeFilter::Global => clauses.push("e.scope_kind = 'global'".to_owned()),
        ScopeFilter::Workspace(id) => {
            clauses.push("e.workspace_id = ?1".to_owned());
            args.push(id.to_string());
        }
        ScopeFilter::Both(id) => {
            clauses.push("(e.scope_kind = 'global' OR e.workspace_id = ?1)".to_owned());
            args.push(id.to_string());
        }
    }
    if !include_archived {
        clauses.push("e.state = 'active'".to_owned());
    }
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(
        " ORDER BY CASE e.category WHEN 'profile' THEN 0 WHEN 'fact' THEN 1 ELSE 2 END, \
         e.updated_ms DESC, e.rowid DESC",
    );
    let mut stmt = conn.prepare(&sql).map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args.iter()), |row| {
            Ok(entry_from(row))
        })
        .map_err(StorageError::sqlite(context))?;
    rows.map(|row| {
        row.map_err(StorageError::sqlite(context))
            .and_then(|row| row)
    })
    .collect()
}

fn get_entry(conn: &Connection, id: &str) -> Result<Option<EntryView>> {
    conn.query_row(&format!("{ENTRY_SELECT} WHERE e.id = ?1"), [id], |row| {
        Ok(entry_from(row))
    })
    .optional()
    .map_err(StorageError::sqlite("reading a memory entry"))?
    .transpose()
}

fn revisions_of(conn: &Connection, id: &str) -> Result<Vec<RevisionRow>> {
    let context = "reading memory revisions";
    let mut stmt = conn
        .prepare(&format!(
            "{REVISION_SELECT} WHERE entry_id = ?1 ORDER BY revision DESC"
        ))
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([id], |row| Ok(revision_from(row)))
        .map_err(StorageError::sqlite(context))?;
    rows.map(|row| {
        row.map_err(StorageError::sqlite(context))
            .and_then(|row| row)
    })
    .collect()
}

const PROPOSAL_SELECT: &str = "SELECT id, operation_id, scope_kind, workspace_id, operation, \
    entry_id, expected_revision, category, body, provenance_json, state, \
    origin_chat_session_id, created_ms, resolved_ms FROM memory_proposals";

fn proposal_from(row: &Row<'_>) -> Result<ProposalRow> {
    let read = |e| StorageError::sqlite("reading a memory proposal")(e);
    let kind: String = row.get(2).map_err(read)?;
    let workspace: Option<String> = row.get(3).map_err(read)?;
    let operation: String = row.get(4).map_err(read)?;
    let expected: Option<i64> = row.get(6).map_err(read)?;
    let category: String = row.get(7).map_err(read)?;
    let state: String = row.get(10).map_err(read)?;
    let chat: Option<String> = row.get(11).map_err(read)?;
    let resolved: Option<i64> = row.get(13).map_err(read)?;
    Ok(ProposalRow {
        id: row.get(0).map_err(read)?,
        operation_id: row.get(1).map_err(read)?,
        scope: Scope::from_columns(&kind, workspace)?,
        operation: Operation::parse(&operation, "memory_proposals.operation")?,
        entry_id: row.get(5).map_err(read)?,
        expected_revision: expected.map(unms),
        category: Category::parse(&category, "memory_proposals.category")?,
        body: row.get(8).map_err(read)?,
        provenance_json: row.get(9).map_err(read)?,
        state: ProposalState::parse(&state, "memory_proposals.state")?,
        origin_chat_session_id: chat
            .map(|text| {
                text.parse().map_err(|e| {
                    StorageError::corrupt("memory_proposals.origin_chat_session_id", e)
                })
            })
            .transpose()?,
        created_ms: unms(row.get(12).map_err(read)?),
        resolved_ms: resolved.map(unms),
    })
}

fn proposal_by_id(conn: &Connection, id: &str) -> Result<Option<ProposalRow>> {
    conn.query_row(&format!("{PROPOSAL_SELECT} WHERE id = ?1"), [id], |row| {
        Ok(proposal_from(row))
    })
    .optional()
    .map_err(StorageError::sqlite("reading a memory proposal"))?
    .transpose()
}

fn proposal_by_operation(conn: &Connection, operation_id: &str) -> Result<Option<ProposalRow>> {
    conn.query_row(
        &format!("{PROPOSAL_SELECT} WHERE operation_id = ?1"),
        [operation_id],
        |row| Ok(proposal_from(row)),
    )
    .optional()
    .map_err(StorageError::sqlite("reading a memory proposal"))?
    .transpose()
}

fn proposals_where(
    conn: &Connection,
    clause: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<ProposalRow>> {
    let context = "reading memory proposals";
    let mut stmt = conn
        .prepare(&format!("{PROPOSAL_SELECT} WHERE {clause}"))
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(args, |row| Ok(proposal_from(row)))
        .map_err(StorageError::sqlite(context))?;
    rows.map(|row| {
        row.map_err(StorageError::sqlite(context))
            .and_then(|row| row)
    })
    .collect()
}
