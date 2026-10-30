//! Every statement the actor runs. Pure functions over a connection, so the actor thread is the
//! only place that decides when they run.

use std::path::{Path, PathBuf};

use hutan_core::{
    AfterPolicy, DeliveryRecord, DeliveryState, Epoch, SessionId, SessionKind, SessionState, Task,
    TaskId, TaskPlacement, TaskRun, TaskState, WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Result, StorageError};
use crate::rows::{
    ApprovalOutcome, ApprovalRow, ChatItemRow, ChatTurnRow, ChatTurnStatus, ClosedSession,
    CreatedBy, EpochEnd, EpochRecord, ForkedJournal, GrantRow, LspTrustRow, ProviderRow,
    QuestionOutcome, QuestionRow, QueuedRow, RecentProject, SessionRow, WorkspaceKind,
    WorkspaceRow, WorktreeRow, WorktreeSetupTrustRow,
};

pub const EPOCH_HISTORY_KEY: &str = "epoch_history";

/// Recent projects remembered; the oldest are dropped when a new one is added.
pub const RECENT_PROJECTS_KEPT: usize = 200;

/// Epochs remembered in `app_meta`.
pub const EPOCH_HISTORY_KEPT: usize = 32;

/// A mutation. Applied in submission order by the single writer thread.
#[derive(Debug, Clone)]
pub enum Write {
    /// Inserts the workspace; an existing row only has its name and roots refreshed.
    InsertWorkspace(WorkspaceRow),
    RenameWorkspace {
        id: WorkspaceId,
        name: String,
    },
    SetWorkspaceRoots {
        id: WorkspaceId,
        roots: Vec<PathBuf>,
    },
    SetWorkspacePinned {
        id: WorkspaceId,
        pinned: bool,
    },
    SetWorkspacePosition {
        id: WorkspaceId,
        position: i64,
    },
    SetWorkspaceActive {
        id: WorkspaceId,
        last_active_ms: u64,
    },
    SetWorkspaceKind {
        id: WorkspaceId,
        kind: WorkspaceKind,
    },
    SetWorkspaceParent {
        id: WorkspaceId,
        parent_id: Option<WorkspaceId>,
    },
    SetWorkspaceCommonDir {
        id: WorkspaceId,
        common_dir: Option<PathBuf>,
    },
    /// Adds or refreshes a recent project (a newer `last_opened_ms` wins; the name is replaced),
    /// keeping the newest [`RECENT_PROJECTS_KEPT`].
    UpsertRecentProject(RecentProject),
    DeleteRecentProject(PathBuf),
    /// Also deletes the workspace's sessions.
    DeleteWorkspace(WorkspaceId),
    /// Inserts or replaces every column except `created_at_ms`.
    UpsertSession(Box<SessionRow>),
    DeleteSession(SessionId),
    /// Closes a session (`Some`: the time) or reopens it (`None`); the row stays either way.
    SetSessionClosed {
        id: SessionId,
        closed_at_ms: Option<u64>,
    },
    /// Stores the observation tree of a session (JSON written by the daemon); the session's row
    /// must have been written before.
    UpsertAgentTree {
        session_id: SessionId,
        tree_json: String,
        updated_at_ms: u64,
    },
    DeleteAgentTree(SessionId),
    SetMeta {
        key: String,
        value: String,
    },
    /// Removes a key (nothing happens for a missing one).
    DeleteMeta {
        key: String,
    },
    /// Stores a grant of the control gate (`control_grants`); an existing id is left alone.
    InsertGrant(Box<GrantRow>),
    /// Removes a grant (nothing happens for a missing one).
    DeleteGrant {
        id: u64,
    },
    /// Records the user's decision about a folder's language servers (`lsp_trust`), replacing
    /// an earlier one for the same folder.
    SetLspTrust(Box<LspTrustRow>),
    /// Forgets the decision about a folder (nothing happens for a missing one).
    DeleteLspTrust {
        folder: PathBuf,
    },
    /// Records that the user confirmed the setup commands with `hash` in a repository
    /// (`worktree_setup_trust`); confirming again changes nothing.
    TrustWorktreeSetup(Box<WorktreeSetupTrustRow>),
    /// Appends to the epoch history, keeping the newest [`EPOCH_HISTORY_KEPT`].
    RecordEpoch(EpochRecord),
    /// Marks a recorded run as cleanly stopped (nothing happens for an unknown or untracked one).
    RecordEpochStop {
        epoch: Epoch,
        stopped_at_ms: u64,
    },
    /// Adds one event to a chat journal (an existing `(session, seq)` is left alone).
    AppendChatItem(Box<ChatItemRow>),
    /// Removes one event of a chat journal (a snapshot that a final event supersedes).
    DeleteChatItem {
        session_id: SessionId,
        seq: u64,
    },
    /// Replaces the whole queue of a chat session with `items`, in their order.
    SetChatQueue {
        session_id: SessionId,
        items: Vec<QueuedRow>,
    },
    /// Inserts a chat turn or replaces its status, end time and error.
    UpsertChatTurn(ChatTurnRow),
    /// Inserts an approval or replaces how it ended.
    UpsertApproval(Box<ApprovalRow>),
    /// Inserts a question of an agent or replaces how it ended.
    UpsertQuestion(Box<QuestionRow>),
    /// Inserts a provider or replaces everything but its creation time.
    UpsertProvider(Box<ProviderRow>),
    DeleteProvider(String),
    /// Remembers a worktree Hutan created, replacing the row of the same path.
    UpsertWorktree(Box<WorktreeRow>),
    DeleteWorktree(PathBuf),
    /// Inserts a task or replaces it, with its dependencies and sessions, in one transaction
    /// (the creation time stays). The state is stored folded (`ready` is `todo`, `blocked` is
    /// `running`). Its dependencies must exist.
    UpsertTask(Box<Task>),
    /// Also deletes the task's dependency edges, both ways, and its sessions' links.
    DeleteTask(TaskId),
    /// Crash-injection points for the tests.
    #[cfg(test)]
    Fault(Fault),
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub enum Fault {
    /// Unwinds the writer thread.
    Panic,
    /// Kills the whole process, like a power cut.
    Abort,
}

pub fn apply(conn: &Connection, write: &Write) -> Result<()> {
    match write {
        Write::InsertWorkspace(row) => {
            conn.execute(
                "INSERT INTO workspaces (id, name, roots_json, kind, pinned, created_by, \
                     created_at_ms, last_active_ms, position, common_dir, parent_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
                 ON CONFLICT (id) DO UPDATE SET name = excluded.name, roots_json = excluded.roots_json",
                params![
                    row.id.to_string(),
                    row.name,
                    encode_paths(&row.roots)?,
                    to_text(&row.kind, "workspace kind")?,
                    row.pinned,
                    to_text(&row.created_by, "workspace origin")?,
                    ms(row.created_at_ms),
                    ms(row.last_active_ms),
                    row.position,
                    row.common_dir.as_deref().map(path_str).transpose()?,
                    row.parent_id.map(|parent| parent.to_string())
                ],
            )
            .map_err(StorageError::sqlite("inserting a workspace"))?;
        }
        Write::SetWorkspaceKind { id, kind } => {
            conn.execute(
                "UPDATE workspaces SET kind = ?2 WHERE id = ?1",
                params![id.to_string(), to_text(kind, "workspace kind")?],
            )
            .map_err(StorageError::sqlite("changing a workspace kind"))?;
        }
        Write::SetWorkspaceParent { id, parent_id } => {
            conn.execute(
                "UPDATE workspaces SET parent_id = ?2 WHERE id = ?1",
                params![id.to_string(), parent_id.map(|parent| parent.to_string())],
            )
            .map_err(StorageError::sqlite("moving a project into a workspace"))?;
        }
        Write::SetWorkspaceCommonDir { id, common_dir } => {
            conn.execute(
                "UPDATE workspaces SET common_dir = ?2 WHERE id = ?1",
                params![
                    id.to_string(),
                    common_dir.as_deref().map(path_str).transpose()?
                ],
            )
            .map_err(StorageError::sqlite("updating a project identity"))?;
        }
        Write::SetWorkspaceRoots { id, roots } => {
            conn.execute(
                "UPDATE workspaces SET roots_json = ?2 WHERE id = ?1",
                params![id.to_string(), encode_paths(roots)?],
            )
            .map_err(StorageError::sqlite("updating workspace roots"))?;
        }
        Write::SetWorkspacePinned { id, pinned } => {
            conn.execute(
                "UPDATE workspaces SET pinned = ?2 WHERE id = ?1",
                params![id.to_string(), pinned],
            )
            .map_err(StorageError::sqlite("pinning a workspace"))?;
        }
        Write::SetWorkspacePosition { id, position } => {
            conn.execute(
                "UPDATE workspaces SET position = ?2 WHERE id = ?1",
                params![id.to_string(), position],
            )
            .map_err(StorageError::sqlite("moving a workspace"))?;
        }
        Write::SetWorkspaceActive { id, last_active_ms } => {
            conn.execute(
                "UPDATE workspaces SET last_active_ms = ?2 WHERE id = ?1",
                params![id.to_string(), ms(*last_active_ms)],
            )
            .map_err(StorageError::sqlite("recording workspace activity"))?;
        }
        Write::UpsertRecentProject(project) => upsert_recent_project(conn, project)?,
        Write::DeleteRecentProject(path) => {
            conn.execute(
                "DELETE FROM recent_projects WHERE path = ?1",
                [path_str(path)?],
            )
            .map_err(StorageError::sqlite("forgetting a recent project"))?;
        }
        Write::RenameWorkspace { id, name } => {
            conn.execute(
                "UPDATE workspaces SET name = ?2 WHERE id = ?1",
                params![id.to_string(), name],
            )
            .map_err(StorageError::sqlite("renaming a workspace"))?;
        }
        Write::DeleteWorkspace(id) => {
            conn.execute("DELETE FROM workspaces WHERE id = ?1", [id.to_string()])
                .map_err(StorageError::sqlite("deleting a workspace"))?;
        }
        Write::UpsertSession(row) => upsert_session(conn, row)?,
        Write::DeleteSession(id) => {
            conn.execute("DELETE FROM sessions WHERE id = ?1", [id.to_string()])
                .map_err(StorageError::sqlite("deleting a session"))?;
        }
        Write::SetSessionClosed { id, closed_at_ms } => {
            conn.execute(
                "UPDATE sessions SET closed_at_ms = ?2 WHERE id = ?1",
                params![id.to_string(), closed_at_ms.map(ms)],
            )
            .map_err(StorageError::sqlite("closing a session"))?;
        }
        Write::UpsertAgentTree {
            session_id,
            tree_json,
            updated_at_ms,
        } => {
            conn.execute(
                "INSERT INTO agent_trees (session_id, tree_json, updated_at_ms) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT (session_id) DO UPDATE SET \
                     tree_json = excluded.tree_json, updated_at_ms = excluded.updated_at_ms",
                params![session_id.to_string(), tree_json, ms(*updated_at_ms)],
            )
            .map_err(StorageError::sqlite("writing an agent tree"))?;
        }
        Write::DeleteAgentTree(session_id) => {
            conn.execute(
                "DELETE FROM agent_trees WHERE session_id = ?1",
                [session_id.to_string()],
            )
            .map_err(StorageError::sqlite("deleting an agent tree"))?;
        }
        Write::SetMeta { key, value } => set_meta(conn, key, value)?,
        Write::DeleteMeta { key } => {
            conn.execute("DELETE FROM app_meta WHERE key = ?1", [key])
                .map_err(StorageError::sqlite("deleting from app_meta"))?;
        }
        Write::InsertGrant(row) => {
            conn.execute(
                "INSERT INTO control_grants \
                     (id, tool, workspace_id, chat_session_id, created_ms, created_by, \
                      target_session_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT (id) DO NOTHING",
                params![
                    ms(row.id),
                    row.tool,
                    row.workspace_id.map(|w| w.to_string()),
                    row.chat_session_id.map(|s| s.to_string()),
                    ms(row.created_ms),
                    row.created_by,
                    row.target_session_id.map(|s| s.to_string()),
                ],
            )
            .map_err(StorageError::sqlite("writing a control grant"))?;
        }
        Write::DeleteGrant { id } => {
            conn.execute("DELETE FROM control_grants WHERE id = ?1", [ms(*id)])
                .map_err(StorageError::sqlite("deleting a control grant"))?;
        }
        Write::SetLspTrust(row) => {
            conn.execute(
                "INSERT INTO lsp_trust (folder, decision, decided_at_ms, decided_by) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT (folder) DO UPDATE SET decision = excluded.decision, \
                     decided_at_ms = excluded.decided_at_ms, decided_by = excluded.decided_by",
                params![
                    row.folder.to_string_lossy(),
                    if row.trusted { "trusted" } else { "denied" },
                    ms(row.decided_at_ms),
                    row.decided_by,
                ],
            )
            .map_err(StorageError::sqlite("writing an LSP trust decision"))?;
        }
        Write::DeleteLspTrust { folder } => {
            conn.execute(
                "DELETE FROM lsp_trust WHERE folder = ?1",
                [folder.to_string_lossy()],
            )
            .map_err(StorageError::sqlite("deleting an LSP trust decision"))?;
        }
        Write::TrustWorktreeSetup(row) => {
            conn.execute(
                "INSERT INTO worktree_setup_trust (repo_root, hash, decided_at_ms, decided_by) \
                 VALUES (?1, ?2, ?3, ?4) ON CONFLICT (repo_root, hash) DO NOTHING",
                params![
                    row.repo_root.to_string_lossy(),
                    row.hash,
                    ms(row.decided_at_ms),
                    row.decided_by,
                ],
            )
            .map_err(StorageError::sqlite(
                "writing a worktree setup confirmation",
            ))?;
        }
        Write::RecordEpoch(record) => record_epoch(conn, *record)?,
        Write::RecordEpochStop {
            epoch,
            stopped_at_ms,
        } => record_epoch_stop(conn, *epoch, *stopped_at_ms)?,
        Write::AppendChatItem(row) => append_chat_item(conn, row)?,
        Write::DeleteChatItem { session_id, seq } => {
            conn.execute(
                "DELETE FROM chat_items WHERE session_id = ?1 AND seq = ?2",
                params![session_id.to_string(), ms(*seq)],
            )
            .map_err(StorageError::sqlite("removing a chat event"))?;
        }
        Write::SetChatQueue { session_id, items } => set_chat_queue(conn, *session_id, items)?,
        Write::UpsertChatTurn(row) => upsert_chat_turn(conn, row)?,
        Write::UpsertApproval(row) => upsert_approval(conn, row)?,
        Write::UpsertQuestion(row) => upsert_question(conn, row)?,
        Write::UpsertProvider(row) => {
            conn.execute(
                "INSERT INTO providers (id, kind, name, base_url, default_model, keychain_ref, \
                     created_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT (id) DO UPDATE SET kind = excluded.kind, name = excluded.name, \
                     base_url = excluded.base_url, default_model = excluded.default_model, \
                     keychain_ref = excluded.keychain_ref",
                params![
                    row.id,
                    row.kind,
                    row.name,
                    row.base_url,
                    row.default_model,
                    row.keychain_ref,
                    ms(row.created_at_ms)
                ],
            )
            .map_err(StorageError::sqlite("writing a provider"))?;
        }
        Write::DeleteProvider(id) => {
            conn.execute("DELETE FROM providers WHERE id = ?1", [id])
                .map_err(StorageError::sqlite("deleting a provider"))?;
        }
        Write::UpsertWorktree(row) => {
            conn.execute(
                "INSERT INTO worktrees (path, workspace_id, repo_root, branch, base, created_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (path) DO UPDATE SET workspace_id = excluded.workspace_id, \
                     repo_root = excluded.repo_root, branch = excluded.branch, \
                     base = excluded.base, created_at_ms = excluded.created_at_ms",
                params![
                    row.path.to_string_lossy(),
                    row.workspace_id.to_string(),
                    row.repo_root.to_string_lossy(),
                    row.branch,
                    row.base,
                    ms(row.created_at_ms)
                ],
            )
            .map_err(StorageError::sqlite("writing a worktree"))?;
        }
        Write::DeleteWorktree(path) => {
            conn.execute(
                "DELETE FROM worktrees WHERE path = ?1",
                [path.to_string_lossy()],
            )
            .map_err(StorageError::sqlite("deleting a worktree"))?;
        }
        Write::UpsertTask(task) => upsert_task(conn, task)?,
        Write::DeleteTask(id) => {
            conn.execute("DELETE FROM tasks WHERE id = ?1", [id.to_string()])
                .map_err(StorageError::sqlite("deleting a task"))?;
        }
        #[cfg(test)]
        Write::Fault(Fault::Panic) => panic!("injected writer panic"),
        #[cfg(test)]
        Write::Fault(Fault::Abort) => std::process::abort(),
    }
    Ok(())
}

fn upsert_session(conn: &Connection, row: &SessionRow) -> Result<()> {
    let state = serde_json::to_string(&row.state).map_err(|e| StorageError::Encode {
        what: "session state",
        detail: e.to_string(),
    })?;
    let cwd = row.cwd.as_deref().map(path_str).transpose()?;
    conn.execute(
        "INSERT INTO sessions (id, workspace_id, kind, title, title_locked, spec_json, cwd, \
             state_json, created_at_ms, ended_at_ms, resume_ref_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT (id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, kind = excluded.kind, title = excluded.title, \
             title_locked = excluded.title_locked, spec_json = excluded.spec_json, \
             cwd = excluded.cwd, state_json = excluded.state_json, \
             ended_at_ms = excluded.ended_at_ms, resume_ref_json = excluded.resume_ref_json",
        params![
            row.id.to_string(),
            row.workspace_id.to_string(),
            to_text(&row.kind, "session kind")?,
            row.title,
            row.title_locked,
            row.spec_json,
            cwd,
            state,
            ms(row.created_at_ms),
            row.ended_at_ms.map(ms),
            row.resume_ref_json,
        ],
    )
    .map_err(StorageError::sqlite("writing a session"))?;
    Ok(())
}

fn upsert_recent_project(conn: &Connection, project: &RecentProject) -> Result<()> {
    let context = "remembering a recent project";
    conn.execute(
        "INSERT INTO recent_projects (path, name, last_opened_ms) VALUES (?1, ?2, ?3) \
         ON CONFLICT (path) DO UPDATE SET name = excluded.name, \
             last_opened_ms = MAX(last_opened_ms, excluded.last_opened_ms)",
        params![
            path_str(&project.path)?,
            project.name,
            ms(project.last_opened_ms)
        ],
    )
    .map_err(StorageError::sqlite(context))?;
    conn.execute(
        "DELETE FROM recent_projects WHERE path NOT IN \
             (SELECT path FROM recent_projects ORDER BY last_opened_ms DESC, path LIMIT ?1)",
        [i64::try_from(RECENT_PROJECTS_KEPT).unwrap_or(i64::MAX)],
    )
    .map_err(StorageError::sqlite(context))?;
    Ok(())
}

pub fn list_recent_projects(conn: &Connection) -> Result<Vec<RecentProject>> {
    let context = "listing recent projects";
    let mut stmt = conn
        .prepare(
            "SELECT path, name, last_opened_ms FROM recent_projects \
             ORDER BY last_opened_ms DESC, path",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(RecentProject {
                path: PathBuf::from(row.get::<_, String>(0)?),
                name: row.get(1)?,
                last_opened_ms: unms(row.get(2)?),
            })
        })
        .map_err(StorageError::sqlite(context))?;
    rows.map(|row| row.map_err(StorageError::sqlite(context)))
        .collect()
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO app_meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [key, value],
    )
    .map_err(StorageError::sqlite("writing app_meta"))?;
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .optional()
    .map_err(StorageError::sqlite("reading app_meta"))
}

fn record_epoch(conn: &Connection, record: EpochRecord) -> Result<()> {
    let mut history = epoch_history(conn)?;
    history.push(record);
    let excess = history.len().saturating_sub(EPOCH_HISTORY_KEPT);
    history.drain(..excess);
    let json = serde_json::to_string(&history).map_err(|e| StorageError::Encode {
        what: "epoch history",
        detail: e.to_string(),
    })?;
    set_meta(conn, EPOCH_HISTORY_KEY, &json)
}

fn record_epoch_stop(conn: &Connection, epoch: Epoch, stopped_at_ms: u64) -> Result<()> {
    let mut history = epoch_history(conn)?;
    let Some(record) = history
        .iter_mut()
        .find(|r| r.epoch == epoch && r.end == EpochEnd::Open)
    else {
        return Ok(());
    };
    record.end = EpochEnd::Clean {
        at_ms: stopped_at_ms,
    };
    let json = serde_json::to_string(&history).map_err(|e| StorageError::Encode {
        what: "epoch history",
        detail: e.to_string(),
    })?;
    set_meta(conn, EPOCH_HISTORY_KEY, &json)
}

pub fn epoch_history(conn: &Connection) -> Result<Vec<EpochRecord>> {
    match get_meta(conn, EPOCH_HISTORY_KEY)? {
        None => Ok(Vec::new()),
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| StorageError::corrupt("app_meta.epoch_history", e)),
    }
}

pub fn list_workspaces(conn: &Connection) -> Result<Vec<WorkspaceRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, roots_json, kind, pinned, created_by, created_at_ms, \
                 last_active_ms, position, common_dir, parent_id FROM workspaces \
             ORDER BY position, created_at_ms, id",
        )
        .map_err(StorageError::sqlite("listing workspaces"))?;
    let rows = stmt
        .query_map([], |row| Ok(workspace_from(row)))
        .map_err(StorageError::sqlite("listing workspaces"))?;
    collect(rows, "listing workspaces")
}

const SESSION_COLUMNS: &str = "id, workspace_id, kind, title, title_locked, spec_json, cwd, \
     state_json, created_at_ms, ended_at_ms, resume_ref_json";

pub fn list_sessions(conn: &Connection, workspace: Option<WorkspaceId>) -> Result<Vec<SessionRow>> {
    let context = "listing sessions";
    // Match the registry's insertion order at timestamp ties. ULID randomness is not a
    // creation-order tie breaker; upsert preserves rowid (also used for provider ordering).
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions \
             WHERE closed_at_ms IS NULL AND (?1 IS NULL OR workspace_id = ?1) \
             ORDER BY created_at_ms, rowid"
        ))
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([workspace.map(|w| w.to_string())], |row| {
            Ok(session_from(row))
        })
        .map_err(StorageError::sqlite(context))?;
    collect(rows, context)
}

/// The closed sessions, most recently closed first.
pub fn list_closed_sessions(conn: &Connection) -> Result<Vec<ClosedSession>> {
    let context = "listing closed sessions";
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {SESSION_COLUMNS}, closed_at_ms FROM sessions \
             WHERE closed_at_ms IS NOT NULL \
             ORDER BY closed_at_ms DESC, rowid DESC"
        ))
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(session_from(row).and_then(|session| {
                Ok(ClosedSession {
                    session,
                    closed_at_ms: unms(row.get(11).map_err(read_err)?),
                })
            }))
        })
        .map_err(StorageError::sqlite(context))?;
    collect(rows, context)
}

/// The stored agent trees: `(session, tree JSON)`.
pub fn list_agent_trees(conn: &Connection) -> Result<Vec<(SessionId, String)>> {
    let context = "listing agent trees";
    let mut stmt = conn
        .prepare("SELECT session_id, tree_json FROM agent_trees ORDER BY session_id")
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let json: String = row.get(1)?;
            Ok(id
                .parse::<SessionId>()
                .map(|id| (id, json))
                .map_err(|e| StorageError::corrupt("agent_trees.session_id", e.to_string())))
        })
        .map_err(StorageError::sqlite(context))?;
    collect(rows, context)
}

fn collect<T>(
    rows: impl Iterator<Item = rusqlite::Result<Result<T>>>,
    context: &str,
) -> Result<Vec<T>> {
    rows.map(|row| row.map_err(StorageError::sqlite(context))?)
        .collect()
}

fn workspace_from(row: &Row<'_>) -> Result<WorkspaceRow> {
    let id: String = row.get(0).map_err(read_err)?;
    let roots_json: String = row.get(2).map_err(read_err)?;
    let kind: String = row.get(3).map_err(read_err)?;
    let created_by: String = row.get(5).map_err(read_err)?;
    let at = |column: &str| format!("workspaces.{column} of {id}");
    Ok(WorkspaceRow {
        id: id
            .parse()
            .map_err(|e| StorageError::corrupt("workspaces.id", e))?,
        name: row.get(1).map_err(read_err)?,
        roots: decode_paths(&roots_json, &id)?,
        kind: from_text::<WorkspaceKind>(&kind)
            .map_err(|e| StorageError::corrupt(at("kind"), e))?,
        pinned: row.get(4).map_err(read_err)?,
        created_by: from_text::<CreatedBy>(&created_by)
            .map_err(|e| StorageError::corrupt(at("created_by"), e))?,
        created_at_ms: unms(row.get(6).map_err(read_err)?),
        last_active_ms: unms(row.get(7).map_err(read_err)?),
        position: row.get(8).map_err(read_err)?,
        common_dir: row
            .get::<_, Option<String>>(9)
            .map_err(read_err)?
            .map(PathBuf::from),
        parent_id: row
            .get::<_, Option<String>>(10)
            .map_err(read_err)?
            .map(|parent| {
                parent
                    .parse()
                    .map_err(|e| StorageError::corrupt(at("parent_id"), e))
            })
            .transpose()?,
    })
}

fn session_from(row: &Row<'_>) -> Result<SessionRow> {
    let id: String = row.get(0).map_err(read_err)?;
    let at = |column: &str| format!("sessions.{column} of {id}");
    let workspace_id: String = row.get(1).map_err(read_err)?;
    let kind: String = row.get(2).map_err(read_err)?;
    let state_json: String = row.get(7).map_err(read_err)?;
    let cwd: Option<String> = row.get(6).map_err(read_err)?;
    Ok(SessionRow {
        id: id
            .parse()
            .map_err(|e| StorageError::corrupt("sessions.id", e))?,
        workspace_id: workspace_id
            .parse()
            .map_err(|e| StorageError::corrupt(at("workspace_id"), e))?,
        kind: from_text::<SessionKind>(&kind).map_err(|e| StorageError::corrupt(at("kind"), e))?,
        title: row.get(3).map_err(read_err)?,
        title_locked: row.get(4).map_err(read_err)?,
        spec_json: row.get(5).map_err(read_err)?,
        cwd: cwd.map(PathBuf::from),
        state: serde_json::from_str::<SessionState>(&state_json)
            .map_err(|e| StorageError::corrupt(at("state_json"), e))?,
        created_at_ms: unms(row.get(8).map_err(read_err)?),
        ended_at_ms: row.get::<_, Option<i64>>(9).map_err(read_err)?.map(unms),
        resume_ref_json: row.get(10).map_err(read_err)?,
    })
}

fn read_err(source: rusqlite::Error) -> StorageError {
    StorageError::Sqlite {
        context: "reading a row".to_owned(),
        source,
    }
}

fn ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn unms(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| StorageError::Encode {
        what: "path",
        detail: format!("{} is not valid UTF-8", path.display()),
    })
}

fn encode_paths(paths: &[PathBuf]) -> Result<String> {
    let strings = paths
        .iter()
        .map(|p| path_str(p))
        .collect::<Result<Vec<_>>>()?;
    serde_json::to_string(&strings).map_err(|e| StorageError::Encode {
        what: "workspace roots",
        detail: e.to_string(),
    })
}

fn decode_paths(json: &str, workspace: &str) -> Result<Vec<PathBuf>> {
    let strings: Vec<String> = serde_json::from_str(json)
        .map_err(|e| StorageError::corrupt(format!("workspaces.roots_json of {workspace}"), e))?;
    Ok(strings.into_iter().map(PathBuf::from).collect())
}

/// A unit enum as its bare serde name (`terminal`), the same text the JSON columns use.
fn to_text<T: Serialize>(value: &T, what: &'static str) -> Result<String> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => Ok(text),
        Ok(other) => Err(StorageError::Encode {
            what,
            detail: format!("expected a string, got {other}"),
        }),
        Err(e) => Err(StorageError::Encode {
            what,
            detail: e.to_string(),
        }),
    }
}

fn from_text<T: DeserializeOwned>(text: &str) -> serde_json::Result<T> {
    serde_json::from_value(serde_json::Value::String(text.to_owned()))
}

// Chat journal ---------------------------------------------------------------------------------

fn append_chat_item(conn: &Connection, row: &ChatItemRow) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO chat_items (session_id, seq, ts_ms, turn_id, item_id, kind, event_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.session_id.to_string(),
            ms(row.seq),
            ms(row.ts_ms),
            row.turn_id,
            row.item_id,
            row.kind,
            row.event_json
        ],
    )
    .map_err(StorageError::sqlite("journaling a chat event"))?;
    Ok(())
}

/// Up to `limit` events with `seq < before` (all when `None`), newest window first picked, then
/// returned oldest first.
pub fn chat_items(
    conn: &Connection,
    session: SessionId,
    before: Option<u64>,
    limit: u32,
) -> Result<Vec<ChatItemRow>> {
    let context = "reading a chat journal";
    let mut stmt = conn
        .prepare(
            "SELECT seq, ts_ms, turn_id, item_id, kind, event_json FROM chat_items \
             WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
        )
        .map_err(StorageError::sqlite(context))?;
    let before = before.map_or(i64::MAX, ms);
    let rows = stmt
        .query_map(
            params![session.to_string(), before, i64::from(limit)],
            |row| {
                Ok(ChatItemRow {
                    session_id: session,
                    seq: unms(row.get(0)?),
                    ts_ms: unms(row.get(1)?),
                    turn_id: row.get(2)?,
                    item_id: row.get(3)?,
                    kind: row.get(4)?,
                    event_json: row.get(5)?,
                })
            },
        )
        .map_err(StorageError::sqlite(context))?;
    let mut items = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::sqlite(context))?;
    items.reverse();
    Ok(items)
}

/// The newest `limit` events of the session that can change its insight (turn boundaries,
/// usage, tools, file changes, and the messages short enough to be a `/context` command or its
/// answer), oldest first. Raw provider payloads and tool output are cut out in SQLite, so a
/// session of long outputs does not have to be read into memory to count its tools.
pub fn chat_insight_items(
    conn: &Connection,
    session: SessionId,
    limit: u32,
) -> Result<Vec<ChatItemRow>> {
    let context = "reading a chat journal for its insight";
    let mut stmt = conn
        .prepare(
            "SELECT seq, ts_ms, turn_id, item_id, kind, \
                    COALESCE(json_remove(event_json, '$.payload', '$.output'), event_json) \
             FROM chat_items \
             WHERE session_id = ?1 AND (kind IN ('turn_started', 'tool_start', 'tool_end', \
                    'file_diff', 'usage', 'turn_completed', 'turn_failed', 'turn_cancelled') \
                OR (kind = 'message_end' AND length(event_json) <= 16384)) \
             ORDER BY seq DESC LIMIT ?2",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(params![session.to_string(), i64::from(limit)], |row| {
            Ok(ChatItemRow {
                session_id: session,
                seq: unms(row.get(0)?),
                ts_ms: unms(row.get(1)?),
                turn_id: row.get(2)?,
                item_id: row.get(3)?,
                kind: row.get(4)?,
                event_json: row.get(5)?,
            })
        })
        .map_err(StorageError::sqlite(context))?;
    let mut items = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::sqlite(context))?;
    items.reverse();
    Ok(items)
}

/// The newest `limit` events of one `kind` of the session, oldest first.
pub fn chat_items_of_kind(
    conn: &Connection,
    session: SessionId,
    kind: &str,
    limit: u32,
) -> Result<Vec<ChatItemRow>> {
    let context = "reading chat events of one kind";
    let mut stmt = conn
        .prepare(
            "SELECT seq, ts_ms, turn_id, item_id, kind, event_json FROM chat_items \
             WHERE session_id = ?1 AND kind = ?2 ORDER BY seq DESC LIMIT ?3",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(
            params![session.to_string(), kind, i64::from(limit)],
            |row| {
                Ok(ChatItemRow {
                    session_id: session,
                    seq: unms(row.get(0)?),
                    ts_ms: unms(row.get(1)?),
                    turn_id: row.get(2)?,
                    item_id: row.get(3)?,
                    kind: row.get(4)?,
                    event_json: row.get(5)?,
                })
            },
        )
        .map_err(StorageError::sqlite(context))?;
    let mut items = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::sqlite(context))?;
    items.reverse();
    Ok(items)
}

/// The newest `limit` events of one `kind` with `seq <= through_seq`, oldest first (what a fork
/// reads of the prefix it keeps).
pub fn chat_items_of_kind_through(
    conn: &Connection,
    session: SessionId,
    kind: &str,
    through_seq: u64,
    limit: u32,
) -> Result<Vec<ChatItemRow>> {
    let context = "reading chat events of one kind through a seq";
    let mut stmt = conn
        .prepare(
            "SELECT seq, ts_ms, turn_id, item_id, kind, event_json FROM chat_items \
             WHERE session_id = ?1 AND kind = ?2 AND seq <= ?3 ORDER BY seq DESC LIMIT ?4",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(
            params![session.to_string(), kind, ms(through_seq), i64::from(limit)],
            |row| {
                Ok(ChatItemRow {
                    session_id: session,
                    seq: unms(row.get(0)?),
                    ts_ms: unms(row.get(1)?),
                    turn_id: row.get(2)?,
                    item_id: row.get(3)?,
                    kind: row.get(4)?,
                    event_json: row.get(5)?,
                })
            },
        )
        .map_err(StorageError::sqlite(context))?;
    let mut items = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::sqlite(context))?;
    items.reverse();
    Ok(items)
}

/// How many events a fork through `through_seq` copies: the session's journal with `seq <=
/// through_seq`, resume locators aside.
pub fn chat_events_through(conn: &Connection, session: SessionId, through_seq: u64) -> Result<u64> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM chat_items \
             WHERE session_id = ?1 AND seq <= ?2 AND kind <> 'resume_locator'",
            params![session.to_string(), ms(through_seq)],
            |row| row.get(0),
        )
        .map_err(StorageError::sqlite("counting chat events"))?;
    Ok(unms(count))
}

/// The highest journaled `seq` of the session (0 for an empty journal).
pub fn chat_last_seq(conn: &Connection, session: SessionId) -> Result<u64> {
    let last: Option<i64> = conn
        .query_row(
            "SELECT MAX(seq) FROM chat_items WHERE session_id = ?1",
            [session.to_string()],
            |row| row.get(0),
        )
        .map_err(StorageError::sqlite("reading the last chat seq"))?;
    Ok(last.map_or(0, unms))
}

/// The highest `seq` journaled for `turn` of the session; `None` when the turn has no event.
pub fn chat_turn_last_seq(
    conn: &Connection,
    session: SessionId,
    turn: &str,
) -> Result<Option<u64>> {
    let last: Option<i64> = conn
        .query_row(
            "SELECT MAX(seq) FROM chat_items WHERE session_id = ?1 AND turn_id = ?2",
            params![session.to_string(), turn],
            |row| row.get(0),
        )
        .map_err(StorageError::sqlite(
            "reading the last event of a chat turn",
        ))?;
    Ok(last.map(unms))
}

/// Copies the journal of `parent` through `through_seq` into `child` in one savepoint, then
/// adds `extra` rows (ADR 0023): every event with `seq <= through_seq` keeps its `seq` (the gaps
/// where deltas were stay) with the event's own `session_id` rewritten to the child's; the
/// parent's resume locators are left out (the child's agent writes its own); the rows of
/// `chat_turns` through `through_turn` are copied. Nothing is written when any step fails.
/// One `INSERT … SELECT` per table, so the rows never leave SQLite.
pub fn fork_chat_journal(
    conn: &Connection,
    parent: SessionId,
    child: SessionId,
    through_seq: u64,
    through_turn: &str,
    extra: &[ChatItemRow],
) -> Result<ForkedJournal> {
    let context = "forking a chat journal";
    conn.execute_batch("SAVEPOINT chat_fork")
        .map_err(StorageError::sqlite(context))?;
    let copied = (|| {
        let events = conn.execute(
            "INSERT INTO chat_items (session_id, seq, ts_ms, turn_id, item_id, kind, event_json) \
             SELECT ?2, seq, ts_ms, turn_id, item_id, kind, \
                    json_set(event_json, '$.session_id', ?2) \
             FROM chat_items WHERE session_id = ?1 AND seq <= ?3 AND kind <> 'resume_locator'",
            params![parent.to_string(), child.to_string(), ms(through_seq)],
        )?;
        let turns = conn.execute(
            "INSERT INTO chat_turns (session_id, turn_id, provider_turn_id, status, started_at_ms, \
                 ended_at_ms, error) \
             SELECT ?2, turn_id, provider_turn_id, status, started_at_ms, ended_at_ms, error \
             FROM chat_turns WHERE session_id = ?1 AND (started_at_ms, turn_id) <= \
                 (SELECT started_at_ms, turn_id FROM chat_turns WHERE session_id = ?1 AND turn_id = ?3)",
            params![parent.to_string(), child.to_string(), through_turn],
        )?;
        Ok::<_, rusqlite::Error>((events, turns))
    })()
    .map_err(StorageError::sqlite(context))
    .and_then(|(events, turns)| {
        for row in extra {
            append_chat_item(conn, row)?;
        }
        Ok(ForkedJournal {
            events: u64::try_from(events).unwrap_or(u64::MAX),
            turns: u64::try_from(turns).unwrap_or(u64::MAX),
        })
    });
    match copied {
        Ok(copied) => {
            conn.execute_batch("RELEASE chat_fork")
                .map_err(StorageError::sqlite(context))?;
            Ok(copied)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK TO chat_fork; RELEASE chat_fork");
            Err(error)
        }
    }
}

/// Deletes and inserts in one savepoint: a crash leaves the old queue or the new one.
fn set_chat_queue(conn: &Connection, session: SessionId, items: &[QueuedRow]) -> Result<()> {
    let context = "writing a chat queue";
    conn.execute_batch("SAVEPOINT chat_queue")
        .map_err(StorageError::sqlite(context))?;
    let written = (|| {
        conn.execute(
            "DELETE FROM chat_queue WHERE session_id = ?1",
            [session.to_string()],
        )?;
        let mut insert = conn.prepare_cached(
            "INSERT INTO chat_queue \
                 (session_id, item_id, position, text, queued_at_ms, origin_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (position, item) in items.iter().enumerate() {
            insert.execute(params![
                session.to_string(),
                ms(item.item_id),
                i64::try_from(position).unwrap_or(i64::MAX),
                item.text,
                ms(item.queued_at_ms),
                item.origin_json
            ])?;
        }
        Ok::<(), rusqlite::Error>(())
    })();
    match written {
        Ok(()) => conn
            .execute_batch("RELEASE chat_queue")
            .map_err(StorageError::sqlite(context)),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK TO chat_queue; RELEASE chat_queue");
            Err(StorageError::sqlite(context)(error))
        }
    }
}

/// The queue of a chat session, in order.
pub fn chat_queue(conn: &Connection, session: SessionId) -> Result<Vec<QueuedRow>> {
    let context = "reading a chat queue";
    let mut stmt = conn
        .prepare(
            "SELECT item_id, text, queued_at_ms, origin_json FROM chat_queue \
             WHERE session_id = ?1 ORDER BY position",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([session.to_string()], |row| {
            Ok(QueuedRow {
                session_id: session,
                item_id: unms(row.get(0)?),
                text: row.get(1)?,
                queued_at_ms: unms(row.get(2)?),
                origin_json: row.get(3)?,
            })
        })
        .map_err(StorageError::sqlite(context))?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(StorageError::sqlite(context))
}

fn upsert_chat_turn(conn: &Connection, row: &ChatTurnRow) -> Result<()> {
    conn.execute(
        "INSERT INTO chat_turns (session_id, turn_id, provider_turn_id, status, started_at_ms, \
             ended_at_ms, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT (session_id, turn_id) DO UPDATE SET \
             provider_turn_id = COALESCE(excluded.provider_turn_id, provider_turn_id), \
             status = excluded.status, ended_at_ms = excluded.ended_at_ms, error = excluded.error",
        params![
            row.session_id.to_string(),
            row.turn_id,
            row.provider_turn_id,
            to_text(&row.status, "turn status")?,
            ms(row.started_at_ms),
            row.ended_at_ms.map(ms),
            row.error
        ],
    )
    .map_err(StorageError::sqlite("writing a chat turn"))?;
    Ok(())
}

pub fn chat_turns(conn: &Connection, session: SessionId) -> Result<Vec<ChatTurnRow>> {
    let context = "reading chat turns";
    let mut stmt = conn
        .prepare(
            "SELECT turn_id, provider_turn_id, status, started_at_ms, ended_at_ms, error \
             FROM chat_turns WHERE session_id = ?1 ORDER BY started_at_ms, turn_id",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([session.to_string()], |row| {
            let status: String = row.get(2)?;
            Ok((
                ChatTurnRow {
                    session_id: session,
                    turn_id: row.get(0)?,
                    provider_turn_id: row.get(1)?,
                    status: ChatTurnStatus::Running,
                    started_at_ms: unms(row.get(3)?),
                    ended_at_ms: row.get::<_, Option<i64>>(4)?.map(unms),
                    error: row.get(5)?,
                },
                status,
            ))
        })
        .map_err(StorageError::sqlite(context))?;
    let mut turns = Vec::new();
    for row in rows {
        let (mut turn, status) = row.map_err(StorageError::sqlite(context))?;
        turn.status = from_text::<ChatTurnStatus>(&status).map_err(|e| {
            StorageError::corrupt(format!("chat_turns.status of {}", turn.turn_id), e)
        })?;
        turns.push(turn);
    }
    Ok(turns)
}

fn upsert_approval(conn: &Connection, row: &ApprovalRow) -> Result<()> {
    let outcome = row
        .outcome
        .as_ref()
        .map(|o| to_text(o, "approval outcome"))
        .transpose()?;
    conn.execute(
        "INSERT INTO approvals (id, session_id, turn_id, kind, title, options_json, \
             requested_at_ms, resolved_at_ms, outcome, option_id, decided_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT (id) DO UPDATE SET resolved_at_ms = excluded.resolved_at_ms, \
             outcome = excluded.outcome, option_id = excluded.option_id, \
             decided_by = excluded.decided_by",
        params![
            row.id.to_string(),
            row.session_id.to_string(),
            row.turn_id,
            row.kind,
            row.title,
            row.options_json,
            ms(row.requested_at_ms),
            row.resolved_at_ms.map(ms),
            outcome,
            row.option_id,
            row.decided_by
        ],
    )
    .map_err(StorageError::sqlite("writing an approval"))?;
    Ok(())
}

/// Approvals of one session (or of all, when `None`), oldest first; `only_open` keeps those
/// not resolved yet.
pub fn approvals(
    conn: &Connection,
    session: Option<SessionId>,
    only_open: bool,
) -> Result<Vec<ApprovalRow>> {
    let context = "reading approvals";
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, turn_id, kind, title, options_json, requested_at_ms, \
                 resolved_at_ms, outcome, option_id, decided_by FROM approvals \
             WHERE (?1 IS NULL OR session_id = ?1) AND (?2 = 0 OR resolved_at_ms IS NULL) \
             ORDER BY requested_at_ms, id",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(params![session.map(|s| s.to_string()), only_open], |row| {
            Ok(approval_from(row))
        })
        .map_err(StorageError::sqlite(context))?;
    collect(rows, context)
}

/// The grants of the control gate, newest first.
pub fn control_grants(conn: &Connection) -> Result<Vec<GrantRow>> {
    let context = "reading control grants";
    let mut stmt = conn
        .prepare(
            "SELECT id, tool, workspace_id, chat_session_id, created_ms, created_by, \
                    target_session_id \
             FROM control_grants ORDER BY id DESC",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| Ok(grant_from(row)))
        .map_err(StorageError::sqlite(context))?;
    collect(rows, context)
}

/// The decisions about folders' language servers, by folder.
pub fn lsp_trust(conn: &Connection) -> Result<Vec<LspTrustRow>> {
    let context = "reading LSP trust decisions";
    let mut stmt = conn
        .prepare(
            "SELECT folder, decision, decided_at_ms, decided_by FROM lsp_trust ORDER BY folder",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            let decision: String = row.get(1)?;
            Ok((
                PathBuf::from(row.get::<_, String>(0)?),
                decision,
                unms(row.get(2)?),
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(StorageError::sqlite(context))?;
    let mut out = Vec::new();
    for row in rows {
        let (folder, decision, decided_at_ms, decided_by) =
            row.map_err(StorageError::sqlite(context))?;
        out.push(LspTrustRow {
            folder,
            trusted: match decision.as_str() {
                "trusted" => true,
                "denied" => false,
                other => return Err(StorageError::corrupt("lsp_trust.decision", other)),
            },
            decided_at_ms,
            decided_by,
        });
    }
    Ok(out)
}

/// The confirmed setup commands, by repository and hash.
pub fn worktree_setup_trust(conn: &Connection) -> Result<Vec<WorktreeSetupTrustRow>> {
    let context = "reading worktree setup confirmations";
    let mut stmt = conn
        .prepare(
            "SELECT repo_root, hash, decided_at_ms, decided_by FROM worktree_setup_trust \
             ORDER BY repo_root, hash",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(WorktreeSetupTrustRow {
                repo_root: PathBuf::from(row.get::<_, String>(0)?),
                hash: row.get(1)?,
                decided_at_ms: unms(row.get(2)?),
                decided_by: row.get(3)?,
            })
        })
        .map_err(StorageError::sqlite(context))?;
    rows.map(|row| row.map_err(StorageError::sqlite(context)))
        .collect()
}

fn grant_from(row: &Row<'_>) -> Result<GrantRow> {
    let id: i64 = row.get(0).map_err(read_err)?;
    let workspace: Option<String> = row.get(2).map_err(read_err)?;
    let chat: Option<String> = row.get(3).map_err(read_err)?;
    let target: Option<String> = row.get(6).map_err(read_err)?;
    Ok(GrantRow {
        id: unms(id),
        tool: row.get(1).map_err(read_err)?,
        workspace_id: workspace
            .map(|text| {
                text.parse()
                    .map_err(|e| StorageError::corrupt("control_grants.workspace_id", e))
            })
            .transpose()?,
        chat_session_id: chat
            .map(|text| {
                text.parse()
                    .map_err(|e| StorageError::corrupt("control_grants.chat_session_id", e))
            })
            .transpose()?,
        target_session_id: target
            .map(|text| {
                text.parse()
                    .map_err(|e| StorageError::corrupt("control_grants.target_session_id", e))
            })
            .transpose()?,
        created_ms: unms(row.get(4).map_err(read_err)?),
        created_by: row.get(5).map_err(read_err)?,
    })
}

pub fn approvals_resolved_since(conn: &Connection, since_ms: u64) -> Result<Vec<ApprovalRow>> {
    let context = "reading approvals";
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, turn_id, kind, title, options_json, requested_at_ms, \
                 resolved_at_ms, outcome, option_id, decided_by FROM approvals \
             WHERE resolved_at_ms >= ?1 ORDER BY requested_at_ms, id",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(params![ms(since_ms)], |row| Ok(approval_from(row)))
        .map_err(StorageError::sqlite(context))?;
    collect(rows, context)
}

fn approval_from(row: &Row<'_>) -> Result<ApprovalRow> {
    let id: String = row.get(0).map_err(read_err)?;
    let session: String = row.get(1).map_err(read_err)?;
    let at = |column: &str| format!("approvals.{column} of {id}");
    let outcome: Option<String> = row.get(8).map_err(read_err)?;
    Ok(ApprovalRow {
        id: id
            .parse()
            .map_err(|e| StorageError::corrupt("approvals.id", e))?,
        session_id: session
            .parse()
            .map_err(|e| StorageError::corrupt(at("session_id"), e))?,
        turn_id: row.get(2).map_err(read_err)?,
        kind: row.get(3).map_err(read_err)?,
        title: row.get(4).map_err(read_err)?,
        options_json: row.get(5).map_err(read_err)?,
        requested_at_ms: unms(row.get(6).map_err(read_err)?),
        resolved_at_ms: row.get::<_, Option<i64>>(7).map_err(read_err)?.map(unms),
        outcome: outcome
            .map(|text| {
                from_text::<ApprovalOutcome>(&text)
                    .map_err(|e| StorageError::corrupt(at("outcome"), e))
            })
            .transpose()?,
        option_id: row.get(9).map_err(read_err)?,
        decided_by: row.get(10).map_err(read_err)?,
    })
}

fn upsert_question(conn: &Connection, row: &QuestionRow) -> Result<()> {
    let outcome = row
        .outcome
        .as_ref()
        .map(|o| to_text(o, "question outcome"))
        .transpose()?;
    conn.execute(
        "INSERT INTO questions (id, session_id, turn_id, title, requested_at_ms, resolved_at_ms, \
             outcome, decided_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT (id) DO UPDATE SET resolved_at_ms = excluded.resolved_at_ms, \
             outcome = excluded.outcome, decided_by = excluded.decided_by",
        params![
            row.id.to_string(),
            row.session_id.to_string(),
            row.turn_id,
            row.title,
            ms(row.requested_at_ms),
            row.resolved_at_ms.map(ms),
            outcome,
            row.decided_by
        ],
    )
    .map_err(StorageError::sqlite("writing a question"))?;
    Ok(())
}

/// Questions of one session (or of all, when `None`), oldest first; `only_open` keeps those
/// not resolved yet.
pub fn questions(
    conn: &Connection,
    session: Option<SessionId>,
    only_open: bool,
) -> Result<Vec<QuestionRow>> {
    let context = "reading questions";
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, turn_id, title, requested_at_ms, resolved_at_ms, outcome, \
                 decided_by FROM questions \
             WHERE (?1 IS NULL OR session_id = ?1) AND (?2 = 0 OR resolved_at_ms IS NULL) \
             ORDER BY requested_at_ms, id",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(params![session.map(|s| s.to_string()), only_open], |row| {
            Ok(question_from(row))
        })
        .map_err(StorageError::sqlite(context))?;
    collect(rows, context)
}

pub fn questions_resolved_since(conn: &Connection, since_ms: u64) -> Result<Vec<QuestionRow>> {
    let context = "reading questions";
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, turn_id, title, requested_at_ms, resolved_at_ms, outcome, \
                 decided_by FROM questions WHERE resolved_at_ms >= ?1 \
             ORDER BY requested_at_ms, id",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(params![ms(since_ms)], |row| Ok(question_from(row)))
        .map_err(StorageError::sqlite(context))?;
    collect(rows, context)
}

fn question_from(row: &Row<'_>) -> Result<QuestionRow> {
    let id: String = row.get(0).map_err(read_err)?;
    let session: String = row.get(1).map_err(read_err)?;
    let at = |column: &str| format!("questions.{column} of {id}");
    let outcome: Option<String> = row.get(6).map_err(read_err)?;
    Ok(QuestionRow {
        id: id
            .parse()
            .map_err(|e| StorageError::corrupt("questions.id", e))?,
        session_id: session
            .parse()
            .map_err(|e| StorageError::corrupt(at("session_id"), e))?,
        turn_id: row.get(2).map_err(read_err)?,
        title: row.get(3).map_err(read_err)?,
        requested_at_ms: unms(row.get(4).map_err(read_err)?),
        resolved_at_ms: row.get::<_, Option<i64>>(5).map_err(read_err)?.map(unms),
        outcome: outcome
            .map(|text| {
                from_text::<QuestionOutcome>(&text)
                    .map_err(|e| StorageError::corrupt(at("outcome"), e))
            })
            .transpose()?,
        decided_by: row.get(7).map_err(read_err)?,
    })
}

/// Every provider, oldest first.
pub fn list_providers(conn: &Connection) -> Result<Vec<ProviderRow>> {
    let context = "listing providers";
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, name, base_url, default_model, keychain_ref, created_at_ms \
             FROM providers ORDER BY created_at_ms, rowid",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ProviderRow {
                id: row.get(0)?,
                kind: row.get(1)?,
                name: row.get(2)?,
                base_url: row.get(3)?,
                default_model: row.get(4)?,
                keychain_ref: row.get(5)?,
                created_at_ms: unms(row.get(6)?),
            })
        })
        .map_err(StorageError::sqlite(context))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::sqlite(context))
}

/// Every remembered worktree, oldest first.
pub fn list_worktrees(conn: &Connection) -> Result<Vec<WorktreeRow>> {
    let context = "listing worktrees";
    let mut stmt = conn
        .prepare(
            "SELECT path, workspace_id, repo_root, branch, base, created_at_ms \
             FROM worktrees ORDER BY created_at_ms, rowid",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(1)?;
            let path: String = row.get(0)?;
            let repo_root: String = row.get(2)?;
            Ok((
                path,
                id,
                repo_root,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(StorageError::sqlite(context))?;
    let mut out = Vec::new();
    for entry in rows {
        let (path, id, repo_root, branch, base, created) =
            entry.map_err(StorageError::sqlite(context))?;
        let workspace_id = id
            .parse()
            .map_err(|e| StorageError::corrupt(format!("worktrees.workspace_id of {path}"), e))?;
        out.push(WorktreeRow {
            path: PathBuf::from(path),
            workspace_id,
            repo_root: PathBuf::from(repo_root),
            branch,
            base,
            created_at_ms: unms(created),
        });
    }
    Ok(out)
}

fn upsert_task(conn: &Connection, task: &Task) -> Result<()> {
    let context = "writing a task";
    let encode = |value: Result<String, serde_json::Error>, what: &'static str| {
        value.map_err(|e| StorageError::Encode {
            what,
            detail: e.to_string(),
        })
    };
    let placement = encode(serde_json::to_string(&task.placement), "task placement")?;
    let run = task
        .run
        .as_ref()
        .map(|run| encode(serde_json::to_string(run), "task run"))
        .transpose()?;
    let history = encode(
        serde_json::to_string(&task.delivery_history),
        "task delivery history",
    )?;
    let state = task.state.stored();
    let tx = conn
        .unchecked_transaction()
        .map_err(StorageError::sqlite(context))?;
    tx.execute(
        "INSERT INTO tasks (id, workspace_id, title, prompt, state, placement_json, run_json, \
             auto_start, created_at_ms, started_at_ms, ended_at_ms, outcome, \
             delivery, after_policy, delivery_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) \
         ON CONFLICT (id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, title = excluded.title, \
             prompt = excluded.prompt, state = excluded.state, \
             placement_json = excluded.placement_json, run_json = excluded.run_json, \
             auto_start = excluded.auto_start, started_at_ms = excluded.started_at_ms, \
             ended_at_ms = excluded.ended_at_ms, outcome = excluded.outcome, \
             delivery = excluded.delivery, after_policy = excluded.after_policy, \
             delivery_json = excluded.delivery_json",
        params![
            task.id.to_string(),
            task.workspace_id.to_string(),
            task.title,
            task.prompt,
            state.as_str(),
            placement,
            run,
            task.auto_start,
            ms(task.created_at_ms),
            task.started_at_ms.map(ms),
            task.ended_at_ms.map(ms),
            task.outcome,
            task.delivery.as_str(),
            task.after_policy.as_str(),
            history,
        ],
    )
    .map_err(StorageError::sqlite(context))?;
    tx.execute(
        "DELETE FROM task_deps WHERE task_id = ?1",
        [task.id.to_string()],
    )
    .map_err(StorageError::sqlite(context))?;
    for (position, dep) in task.depends_on.iter().enumerate() {
        tx.execute(
            "INSERT OR IGNORE INTO task_deps (task_id, depends_on, position) VALUES (?1, ?2, ?3)",
            params![task.id.to_string(), dep.to_string(), ms(position as u64)],
        )
        .map_err(StorageError::sqlite(context))?;
    }
    tx.execute(
        "DELETE FROM task_sessions WHERE task_id = ?1",
        [task.id.to_string()],
    )
    .map_err(StorageError::sqlite(context))?;
    for (position, session) in task.sessions.iter().enumerate() {
        tx.execute(
            "INSERT OR IGNORE INTO task_sessions (task_id, session_id, position) \
             VALUES (?1, ?2, ?3)",
            params![
                task.id.to_string(),
                session.to_string(),
                ms(position as u64)
            ],
        )
        .map_err(StorageError::sqlite(context))?;
    }
    tx.commit().map_err(StorageError::sqlite(context))
}

/// Every task, oldest first, with its dependencies and sessions in the order they were given.
pub fn list_tasks(conn: &Connection) -> Result<Vec<Task>> {
    let context = "listing tasks";
    let mut stmt = conn
        .prepare(
            "SELECT id, workspace_id, title, prompt, state, placement_json, run_json, \
                 auto_start, created_at_ms, started_at_ms, ended_at_ms, outcome, \
                 delivery, after_policy, delivery_json \
             FROM tasks ORDER BY created_at_ms, rowid",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| Ok(task_from(row)))
        .map_err(StorageError::sqlite(context))?;
    let mut tasks = collect(rows, context)?;
    let mut deps = conn
        .prepare("SELECT task_id, depends_on FROM task_deps ORDER BY task_id, position")
        .map_err(StorageError::sqlite(context))?;
    let mut sessions = conn
        .prepare("SELECT task_id, session_id FROM task_sessions ORDER BY task_id, position")
        .map_err(StorageError::sqlite(context))?;
    let edges = |stmt: &mut rusqlite::Statement<'_>| -> Result<Vec<(String, String)>> {
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(StorageError::sqlite(context))?
            .map(|row| row.map_err(StorageError::sqlite(context)))
            .collect()
    };
    let index: std::collections::HashMap<String, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.to_string(), i))
        .collect();
    for (task_id, dep) in edges(&mut deps)? {
        if let Some(&at) = index.get(&task_id) {
            tasks[at].depends_on.push(
                dep.parse()
                    .map_err(|e| StorageError::corrupt(format!("task_deps of {task_id}"), e))?,
            );
        }
    }
    for (task_id, session) in edges(&mut sessions)? {
        if let Some(&at) = index.get(&task_id) {
            tasks[at].sessions.push(
                session
                    .parse()
                    .map_err(|e| StorageError::corrupt(format!("task_sessions of {task_id}"), e))?,
            );
        }
    }
    Ok(tasks)
}

/// A task without its dependencies and sessions (`list_tasks` adds them).
fn task_from(row: &Row<'_>) -> Result<Task> {
    let id: String = row.get(0).map_err(read_err)?;
    let at = |column: &str| format!("tasks.{column} of {id}");
    let workspace_id: String = row.get(1).map_err(read_err)?;
    let state: String = row.get(4).map_err(read_err)?;
    let placement: String = row.get(5).map_err(read_err)?;
    let run: Option<String> = row.get(6).map_err(read_err)?;
    let delivery: String = row.get(12).map_err(read_err)?;
    let after_policy: String = row.get(13).map_err(read_err)?;
    let history: String = row.get(14).map_err(read_err)?;
    Ok(Task {
        id: id
            .parse()
            .map_err(|e| StorageError::corrupt("tasks.id", e))?,
        workspace_id: workspace_id
            .parse()
            .map_err(|e| StorageError::corrupt(at("workspace_id"), e))?,
        title: row.get(2).map_err(read_err)?,
        prompt: row.get(3).map_err(read_err)?,
        state: state
            .parse::<TaskState>()
            .map_err(|e| StorageError::corrupt(at("state"), e))?,
        depends_on: Vec::new(),
        placement: serde_json::from_str::<TaskPlacement>(&placement)
            .map_err(|e| StorageError::corrupt(at("placement_json"), e))?,
        run: run
            .map(|text| serde_json::from_str::<TaskRun>(&text))
            .transpose()
            .map_err(|e| StorageError::corrupt(at("run_json"), e))?,
        auto_start: row.get(7).map_err(read_err)?,
        sessions: Vec::new(),
        created_at_ms: unms(row.get(8).map_err(read_err)?),
        started_at_ms: row.get::<_, Option<i64>>(9).map_err(read_err)?.map(unms),
        ended_at_ms: row.get::<_, Option<i64>>(10).map_err(read_err)?.map(unms),
        outcome: row.get(11).map_err(read_err)?,
        delivery: delivery
            .parse::<DeliveryState>()
            .map_err(|e| StorageError::corrupt(at("delivery"), e))?,
        after_policy: after_policy
            .parse::<AfterPolicy>()
            .map_err(|e| StorageError::corrupt(at("after_policy"), e))?,
        delivery_history: serde_json::from_str::<Vec<DeliveryRecord>>(&history)
            .map_err(|e| StorageError::corrupt(at("delivery_json"), e))?,
    })
}
