//! Durable storage for the daemon (ADR 0006): SQLite behind one writer thread.
//!
//! [`Storage`] is a cheap-to-clone handle. One dedicated OS thread owns the only connection and
//! executes commands in the order they were submitted, so writes to one entity can never be
//! reordered. Writes are fire-and-forget (they only wait when the bounded command queue is
//! full); reads wait for their answer, which also proves every earlier write has been applied.
//!
//! The public API uses no async runtime; async callers run the blocking calls ([`Storage::open`],
//! reads, [`Storage::flush`], [`Storage::close`]) on a blocking thread.

mod checkpoints;
mod error;
mod memory;
mod migrate;
mod rows;
mod sql;
mod state_log;
#[cfg(test)]
mod tests;
mod usage;

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread::JoinHandle;

use hutan_core::{Epoch, SessionId, Task, TaskId, WorkspaceId};
use parking_lot::Mutex;
use rusqlite::Connection;
use tracing::{debug, error};

pub use checkpoints::{
    CheckpointFilter, CheckpointPage, CheckpointRow, CheckpointScope, Checkpoints,
};
pub use error::{Result, StorageError};
pub use memory::{
    Action, Author, Category, Change, EntryRow, EntryState, EntryView, Memory, MemoryError,
    Operation, ProposalRow, ProposalState, Receipt, Resolution, Resolved, RevisionRow, Scope,
    ScopeFilter, SnapshotRow, hash_text,
};
pub use migrate::{BACKUPS_KEPT, MIGRATIONS, Migration};
pub use rows::{
    ApprovalOutcome, ApprovalRow, ChatItemRow, ChatTurnRow, ChatTurnStatus, ClosedSession,
    CreatedBy, EpochEnd, EpochRecord, ForkedJournal, GrantRow, LspTrustRow, ProviderRow,
    QuestionOutcome, QuestionRow, QueuedRow, RecentProject, SessionRow, WorkspaceKind,
    WorkspaceRow, WorktreeRow, WorktreeSetupTrustRow,
};
pub use sql::{EPOCH_HISTORY_KEPT, RECENT_PROJECTS_KEPT, Write};
pub use state_log::{AgentStateLog, AgentStateRow, STATE_LOG_KEPT_DAYS};
pub use usage::{USAGE_KEPT_DAYS, Usage, UsageLimitRow, UsageSum, UsageTurnRow};

/// Commands waiting for the writer before submitters wait their turn.
const QUEUE_CAPACITY: usize = 1024;

enum Cmd {
    Write(Write),
    /// Applied like `Write`; the outcome goes back to the submitter.
    WriteChecked(Write, SyncSender<Result<()>>),
    /// All or nothing: one transaction.
    Batch(Vec<Write>),
    Read(Box<dyn FnOnce(&Connection) + Send>),
    Shutdown,
}

struct Inner {
    path: PathBuf,
    tx: SyncSender<Cmd>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage").field("path", &self.path).finish()
    }
}

impl Inner {
    /// Queues a shutdown behind every pending command and waits for the thread to finish.
    fn close(&self) {
        let Some(thread) = self.thread.lock().take() else {
            return;
        };
        // A failed send means the writer already died; joining reports nothing more.
        let _ = self.tx.send(Cmd::Shutdown);
        if thread.join().is_err() {
            error!("the storage writer panicked");
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.close();
    }
}

/// Handle to the database and its writer thread.
#[derive(Debug, Clone)]
pub struct Storage {
    inner: Arc<Inner>,
}

impl Storage {
    /// Opens (creating when missing) the database at `path`, migrates it to the newest schema,
    /// and starts the writer thread. A failed migration leaves the file as it was and returns an
    /// error; callers must not continue without storage.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_migrations(path, MIGRATIONS)
    }

    /// [`open`](Self::open) with an explicit migration list (tests and tools).
    pub fn open_with_migrations(path: impl AsRef<Path>, migrations: &[Migration]) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && !parent.exists()
        {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(StorageError::io(format!("creating {}", parent.display())))?;
        }
        let mut conn = Connection::open(path)
            .map_err(StorageError::sqlite(format!("opening {}", path.display())))?;
        configure(&conn)?;
        migrate::run(&mut conn, path, migrations)?;
        // The database holds session metadata; keep it private even under a looser umask.
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(StorageError::io(format!("securing {}", path.display())))?;

        let (tx, rx) = sync_channel(QUEUE_CAPACITY);
        let thread = std::thread::Builder::new()
            .name("hutan-storage".to_owned())
            .spawn(move || run_writer(conn, rx))
            .map_err(StorageError::io("starting the storage writer"))?;
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_owned(),
                tx,
                thread: Mutex::new(Some(thread)),
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn workspaces(&self) -> Workspaces<'_> {
        Workspaces(self)
    }

    pub fn sessions(&self) -> Sessions<'_> {
        Sessions(self)
    }

    pub fn recent_projects(&self) -> RecentProjects<'_> {
        RecentProjects(self)
    }

    pub fn chat(&self) -> Chat<'_> {
        Chat(self)
    }

    pub fn agent_trees(&self) -> AgentTrees<'_> {
        AgentTrees(self)
    }

    pub fn approvals(&self) -> Approvals<'_> {
        Approvals(self)
    }

    pub fn questions(&self) -> Questions<'_> {
        Questions(self)
    }

    pub fn providers(&self) -> Providers<'_> {
        Providers(self)
    }

    pub fn worktrees(&self) -> Worktrees<'_> {
        Worktrees(self)
    }

    pub fn tasks(&self) -> Tasks<'_> {
        Tasks(self)
    }

    pub fn app_meta(&self) -> AppMeta<'_> {
        AppMeta(self)
    }

    pub fn control_grants(&self) -> ControlGrants<'_> {
        ControlGrants(self)
    }

    pub fn lsp_trust(&self) -> LspTrust<'_> {
        LspTrust(self)
    }

    /// The setup commands of repositories the user confirmed (protocol 0.49).
    pub fn worktree_setup_trust(&self) -> WorktreeSetupTrust<'_> {
        WorktreeSetupTrust(self)
    }

    pub fn memory(&self) -> Memory<'_> {
        Memory(self)
    }

    pub fn checkpoints(&self) -> Checkpoints<'_> {
        Checkpoints(self)
    }

    pub fn agent_state_log(&self) -> AgentStateLog<'_> {
        AgentStateLog(self)
    }

    pub fn usage(&self) -> Usage<'_> {
        Usage(self)
    }

    /// Queues one write. Returns once it is queued, not once it is durable.
    pub fn write(&self, write: Write) -> Result<()> {
        self.send(Cmd::Write(write))
    }

    /// Applies one write on the writer thread, after everything queued before it, and returns
    /// its outcome: an SQLite failure is the result, not only a log line. Blocks until then.
    pub fn write_checked(&self, write: Write) -> Result<()> {
        let (reply, answer) = sync_channel(1);
        self.send(Cmd::WriteChecked(write, reply))?;
        answer.recv().map_err(|_| StorageError::Closed)?
    }

    /// Queues writes that are applied in one transaction: after a crash either all of them
    /// are there or none.
    pub fn batch(&self, writes: Vec<Write>) -> Result<()> {
        self.send(Cmd::Batch(writes))
    }

    /// Returns once every write queued before this call has been applied.
    pub fn flush(&self) -> Result<()> {
        self.read(|_| Ok(()))
    }

    /// Applies everything still queued, then stops the writer thread and closes the database.
    /// Later writes fail with [`StorageError::Closed`]. Idempotent.
    pub fn close(&self) {
        self.inner.close();
    }

    fn send(&self, cmd: Cmd) -> Result<()> {
        self.inner.tx.send(cmd).map_err(|_| StorageError::Closed)
    }

    fn read<T: Send + 'static>(
        &self,
        query: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.read_raw(query)
    }

    /// [`read`](Self::read) for a query with an error type of its own (the memory repository's
    /// refusals): it runs on the writer thread, so a transaction it opens is atomic against
    /// every other command.
    fn read_raw<T: Send + 'static, E: From<StorageError> + Send + 'static>(
        &self,
        query: impl FnOnce(&Connection) -> std::result::Result<T, E> + Send + 'static,
    ) -> std::result::Result<T, E> {
        let (reply, answer) = sync_channel(1);
        self.send(Cmd::Read(Box::new(move |conn| {
            // The asker may have gone away; nothing to do then.
            let _ = reply.send(query(conn));
        })))?;
        answer.recv().map_err(|_| StorageError::Closed)?
    }
}

fn configure(conn: &Connection) -> Result<()> {
    let context = "configuring the database";
    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(StorageError::sqlite(context))?;
    if !mode.eq_ignore_ascii_case("wal") {
        error!(mode, "the database did not enter WAL mode");
    }
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(StorageError::sqlite(context))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(StorageError::sqlite(context))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(StorageError::sqlite(context))?;
    Ok(())
}

fn run_writer(mut conn: Connection, commands: std::sync::mpsc::Receiver<Cmd>) {
    while let Ok(cmd) = commands.recv() {
        match cmd {
            Cmd::Write(write) => {
                if let Err(error) = sql::apply(&conn, &write) {
                    error!(%error, ?write, "storage write failed");
                }
            }
            Cmd::WriteChecked(write, reply) => {
                let result = sql::apply(&conn, &write);
                if let Err(error) = &result {
                    error!(%error, ?write, "storage write failed");
                }
                // The submitter may have gone away; nothing to do then.
                let _ = reply.send(result);
            }
            Cmd::Batch(writes) => {
                if let Err(error) = apply_batch(&mut conn, &writes) {
                    error!(%error, count = writes.len(), "storage batch failed; none of it was applied");
                }
            }
            Cmd::Read(query) => query(&conn),
            Cmd::Shutdown => break,
        }
    }
    debug!("storage writer stopped");
}

fn apply_batch(conn: &mut Connection, writes: &[Write]) -> Result<()> {
    let tx = conn
        .transaction()
        .map_err(StorageError::sqlite("starting a batch"))?;
    for write in writes {
        sql::apply(&tx, write)?;
    }
    tx.commit()
        .map_err(StorageError::sqlite("committing a batch"))
}

/// Workspace repository.
#[derive(Debug, Clone, Copy)]
pub struct Workspaces<'a>(&'a Storage);

impl Workspaces<'_> {
    /// Adds a workspace (an existing id only has its name and roots refreshed).
    pub fn insert(&self, row: WorkspaceRow) -> Result<()> {
        self.0.write(Write::InsertWorkspace(row))
    }

    pub fn rename(&self, id: WorkspaceId, name: String) -> Result<()> {
        self.0.write(Write::RenameWorkspace { id, name })
    }

    /// Removes the workspace and, by cascade, its sessions.
    pub fn delete(&self, id: WorkspaceId) -> Result<()> {
        self.0.write(Write::DeleteWorkspace(id))
    }

    /// Replaces the workspace's roots.
    pub fn set_roots(&self, id: WorkspaceId, roots: Vec<PathBuf>) -> Result<()> {
        self.0.write(Write::SetWorkspaceRoots { id, roots })
    }

    pub fn set_pinned(&self, id: WorkspaceId, pinned: bool) -> Result<()> {
        self.0.write(Write::SetWorkspacePinned { id, pinned })
    }

    pub fn set_position(&self, id: WorkspaceId, position: i64) -> Result<()> {
        self.0.write(Write::SetWorkspacePosition { id, position })
    }

    pub fn set_last_active(&self, id: WorkspaceId, last_active_ms: u64) -> Result<()> {
        self.0
            .write(Write::SetWorkspaceActive { id, last_active_ms })
    }

    /// Puts a project into a workspace, or out of every workspace.
    pub fn set_parent(&self, id: WorkspaceId, parent_id: Option<WorkspaceId>) -> Result<()> {
        self.0.write(Write::SetWorkspaceParent { id, parent_id })
    }

    /// Sets a project's repository identity.
    pub fn set_common_dir(&self, id: WorkspaceId, common_dir: Option<PathBuf>) -> Result<()> {
        self.0
            .write(Write::SetWorkspaceCommonDir { id, common_dir })
    }

    /// In display order.
    pub fn list(&self) -> Result<Vec<WorkspaceRow>> {
        self.0.read(sql::list_workspaces)
    }
}

/// Projects remembered after their workspace closed, keyed by primary root path.
#[derive(Debug, Clone, Copy)]
pub struct RecentProjects<'a>(&'a Storage);

impl RecentProjects<'_> {
    /// Adds the project or refreshes its name and (when newer) its time. Only the newest
    /// [`RECENT_PROJECTS_KEPT`] are kept.
    pub fn upsert(&self, project: RecentProject) -> Result<()> {
        self.0.write(Write::UpsertRecentProject(project))
    }

    pub fn delete(&self, path: PathBuf) -> Result<()> {
        self.0.write(Write::DeleteRecentProject(path))
    }

    /// Newest first.
    pub fn list(&self) -> Result<Vec<RecentProject>> {
        self.0.read(sql::list_recent_projects)
    }
}

/// Session repository.
#[derive(Debug, Clone, Copy)]
pub struct Sessions<'a>(&'a Storage);

impl Sessions<'_> {
    /// Inserts the session or replaces everything but its creation time. The workspace must
    /// have been written before.
    pub fn upsert(&self, row: SessionRow) -> Result<()> {
        self.0.write(Write::UpsertSession(Box::new(row)))
    }

    pub fn delete(&self, id: SessionId) -> Result<()> {
        self.0.write(Write::DeleteSession(id))
    }

    /// Every open session, oldest first (closed ones are not listed).
    pub fn list(&self) -> Result<Vec<SessionRow>> {
        self.0.read(|conn| sql::list_sessions(conn, None))
    }

    /// The closed sessions, most recently closed first.
    pub fn list_closed(&self) -> Result<Vec<ClosedSession>> {
        self.0.read(sql::list_closed_sessions)
    }

    /// Marks the session closed at `closed_at_ms`, or open again (`None`); its row and everything
    /// that belongs to it stay.
    pub fn set_closed(&self, id: SessionId, closed_at_ms: Option<u64>) -> Result<()> {
        self.0.write(Write::SetSessionClosed { id, closed_at_ms })
    }

    pub fn list_in_workspace(&self, workspace: WorkspaceId) -> Result<Vec<SessionRow>> {
        self.0
            .read(move |conn| sql::list_sessions(conn, Some(workspace)))
    }
}

/// The observation tree of agent sessions (JSON written by the daemon, `docs/integrations.md`).
/// Removed with the session.
#[derive(Debug, Clone, Copy)]
pub struct AgentTrees<'a>(&'a Storage);

impl AgentTrees<'_> {
    /// Stores (or replaces) the tree of a session. The session's row must have been written
    /// before.
    pub fn upsert(&self, session: SessionId, tree_json: String, updated_at_ms: u64) -> Result<()> {
        self.0.write(Write::UpsertAgentTree {
            session_id: session,
            tree_json,
            updated_at_ms,
        })
    }

    pub fn delete(&self, session: SessionId) -> Result<()> {
        self.0.write(Write::DeleteAgentTree(session))
    }

    /// Every stored tree, by session id.
    pub fn list(&self) -> Result<Vec<(SessionId, String)>> {
        self.0.read(sql::list_agent_trees)
    }
}

/// The journal of chat sessions: events and turns. Removed with the session.
#[derive(Debug, Clone, Copy)]
pub struct Chat<'a>(&'a Storage);

impl Chat<'_> {
    /// Appends an event. The session's row must have been written before.
    pub fn append(&self, row: ChatItemRow) -> Result<()> {
        self.0.write(Write::AppendChatItem(Box::new(row)))
    }

    /// Removes one event: a snapshot that a final event supersedes.
    pub fn delete(&self, session: SessionId, seq: u64) -> Result<()> {
        self.0.write(Write::DeleteChatItem {
            session_id: session,
            seq,
        })
    }

    /// Replaces the event at `old_seq` with `row` (at another `seq`) in one transaction: a
    /// snapshot that is kept up to date without the journal growing.
    pub fn replace(&self, old_seq: u64, row: ChatItemRow) -> Result<()> {
        self.0.batch(vec![
            Write::DeleteChatItem {
                session_id: row.session_id,
                seq: old_seq,
            },
            Write::AppendChatItem(Box::new(row)),
        ])
    }

    /// Up to `limit` events with `seq < before` (the newest of them), oldest first.
    pub fn items(
        &self,
        session: SessionId,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<ChatItemRow>> {
        self.0
            .read(move |conn| sql::chat_items(conn, session, before, limit))
    }

    /// The newest `limit` events of the session that can change its insight, oldest first, with
    /// raw provider payloads and tool output left out (see [`sql::chat_insight_items`]).
    pub fn insight_items(&self, session: SessionId, limit: u32) -> Result<Vec<ChatItemRow>> {
        self.0
            .read(move |conn| sql::chat_insight_items(conn, session, limit))
    }

    /// The newest `limit` events whose `type` is `kind`, oldest first (a direct chat reads its
    /// messages this way).
    pub fn items_of_kind(
        &self,
        session: SessionId,
        kind: &str,
        limit: u32,
    ) -> Result<Vec<ChatItemRow>> {
        let kind = kind.to_owned();
        self.0
            .read(move |conn| sql::chat_items_of_kind(conn, session, &kind, limit))
    }

    /// The highest journaled `seq`; 0 when nothing was journaled.
    pub fn last_seq(&self, session: SessionId) -> Result<u64> {
        self.0.read(move |conn| sql::chat_last_seq(conn, session))
    }

    /// The newest `limit` events of one `kind` with `seq <= through_seq`, oldest first.
    pub fn items_of_kind_through(
        &self,
        session: SessionId,
        kind: &str,
        through_seq: u64,
        limit: u32,
    ) -> Result<Vec<ChatItemRow>> {
        let kind = kind.to_owned();
        self.0.read(move |conn| {
            sql::chat_items_of_kind_through(conn, session, &kind, through_seq, limit)
        })
    }

    /// How many events the journal holds with `seq <= through_seq`.
    pub fn events_through(&self, session: SessionId, through_seq: u64) -> Result<u64> {
        self.0
            .read(move |conn| sql::chat_events_through(conn, session, through_seq))
    }

    /// The highest `seq` journaled for one turn; `None` when the turn has no event.
    pub fn turn_last_seq(&self, session: SessionId, turn: &str) -> Result<Option<u64>> {
        let turn = turn.to_owned();
        self.0
            .read(move |conn| sql::chat_turn_last_seq(conn, session, &turn))
    }

    /// Copies the journal of `parent` through `through_seq` into `child` (ADR 0023) and appends
    /// `extra` to it, in one transaction on the writer thread: the child's rows are all there
    /// or none. Events keep their `seq` and have their `session_id` rewritten; resume locators
    /// are not copied; the `chat_turns` rows through `through_turn` are. The copy is
    /// independent: deleting either session never touches the other. Both rows must exist.
    pub fn fork_journal(
        &self,
        parent: SessionId,
        child: SessionId,
        through_seq: u64,
        through_turn: &str,
        extra: Vec<ChatItemRow>,
    ) -> Result<ForkedJournal> {
        let through_turn = through_turn.to_owned();
        self.0.read(move |conn| {
            sql::fork_chat_journal(conn, parent, child, through_seq, &through_turn, &extra)
        })
    }

    pub fn upsert_turn(&self, row: ChatTurnRow) -> Result<()> {
        self.0.write(Write::UpsertChatTurn(row))
    }

    /// Oldest first.
    pub fn turns(&self, session: SessionId) -> Result<Vec<ChatTurnRow>> {
        self.0.read(move |conn| sql::chat_turns(conn, session))
    }

    /// The turns of every session that started or ended in `since_ms..=until_ms`, oldest first.
    pub fn turns_in_window(&self, since_ms: u64, until_ms: u64) -> Result<Vec<ChatTurnRow>> {
        self.0
            .read(move |conn| sql::chat_turns_in_window(conn, since_ms, until_ms))
    }

    /// Replaces the session's queue with `items`, in their order (an empty list clears it). The
    /// session's row must have been written before.
    pub fn set_queue(&self, session: SessionId, items: Vec<QueuedRow>) -> Result<()> {
        self.0.write(Write::SetChatQueue {
            session_id: session,
            items,
        })
    }

    /// The session's queue, in order.
    pub fn queue(&self, session: SessionId) -> Result<Vec<QueuedRow>> {
        self.0.read(move |conn| sql::chat_queue(conn, session))
    }
}

/// Permission requests and their outcomes (the audit trail).
#[derive(Debug, Clone, Copy)]
pub struct Approvals<'a>(&'a Storage);

impl Approvals<'_> {
    /// Records a request, or how an earlier recorded one ended.
    pub fn upsert(&self, row: ApprovalRow) -> Result<()> {
        self.0.write(Write::UpsertApproval(Box::new(row)))
    }

    /// Oldest first; `session` limits to one session.
    pub fn list(&self, session: Option<SessionId>) -> Result<Vec<ApprovalRow>> {
        self.0
            .read(move |conn| sql::approvals(conn, session, false))
    }

    /// Requests resolved at or after `since_ms`, oldest first.
    pub fn resolved_since(&self, since_ms: u64) -> Result<Vec<ApprovalRow>> {
        self.0
            .read(move |conn| sql::approvals_resolved_since(conn, since_ms))
    }

    /// Requests asked or resolved in `since_ms..=until_ms`, oldest first.
    pub fn in_window(&self, since_ms: u64, until_ms: u64) -> Result<Vec<ApprovalRow>> {
        self.0
            .read(move |conn| sql::approvals_in_window(conn, since_ms, until_ms))
    }

    /// Requests never resolved (what a crashed daemon left behind).
    pub fn unresolved(&self) -> Result<Vec<ApprovalRow>> {
        self.0.read(|conn| sql::approvals(conn, None, true))
    }
}

/// Grants of the control gate: what the user allowed Mawas to change without asking again.
#[derive(Debug, Clone, Copy)]
pub struct ControlGrants<'a>(&'a Storage);

impl ControlGrants<'_> {
    /// Stores a grant, applied before it returns (a failing database is the error).
    pub fn insert(&self, row: GrantRow) -> Result<()> {
        self.0.write_checked(Write::InsertGrant(Box::new(row)))
    }

    /// Removes a grant, applied before it returns.
    pub fn delete(&self, id: u64) -> Result<()> {
        self.0.write_checked(Write::DeleteGrant { id })
    }

    /// Newest first.
    pub fn list(&self) -> Result<Vec<GrantRow>> {
        self.0.read(sql::control_grants)
    }
}

/// What the user decided about folders' language servers (protocol 0.39).
#[derive(Debug, Clone, Copy)]
pub struct LspTrust<'a>(&'a Storage);

impl LspTrust<'_> {
    /// Records a decision, applied before it returns (a failing database is the error).
    pub fn set(&self, row: LspTrustRow) -> Result<()> {
        self.0.write_checked(Write::SetLspTrust(Box::new(row)))
    }

    /// Forgets the decision about `folder`, applied before it returns.
    pub fn forget(&self, folder: PathBuf) -> Result<()> {
        self.0.write_checked(Write::DeleteLspTrust { folder })
    }

    /// Every decision, folders sorted.
    pub fn list(&self) -> Result<Vec<LspTrustRow>> {
        self.0.read(sql::lsp_trust)
    }
}

/// Which setup and teardown commands of a repository's `.hutan/worktree.toml` the user confirmed
/// (protocol 0.49).
#[derive(Debug, Clone, Copy)]
pub struct WorktreeSetupTrust<'a>(&'a Storage);

impl WorktreeSetupTrust<'_> {
    /// Records a confirmation, applied before it returns (a failing database is the error).
    pub fn trust(&self, row: WorktreeSetupTrustRow) -> Result<()> {
        self.0
            .write_checked(Write::TrustWorktreeSetup(Box::new(row)))
    }

    /// Every confirmation, by repository and hash.
    pub fn list(&self) -> Result<Vec<WorktreeSetupTrustRow>> {
        self.0.read(sql::worktree_setup_trust)
    }
}

/// Questions of agents and their outcomes (the audit trail; never the answers).
#[derive(Debug, Clone, Copy)]
pub struct Questions<'a>(&'a Storage);

impl Questions<'_> {
    /// Records a question, or how an earlier recorded one ended.
    pub fn upsert(&self, row: QuestionRow) -> Result<()> {
        self.0.write(Write::UpsertQuestion(Box::new(row)))
    }

    /// Oldest first; `session` limits to one session.
    pub fn list(&self, session: Option<SessionId>) -> Result<Vec<QuestionRow>> {
        self.0
            .read(move |conn| sql::questions(conn, session, false))
    }

    /// Questions resolved at or after `since_ms`, oldest first.
    pub fn resolved_since(&self, since_ms: u64) -> Result<Vec<QuestionRow>> {
        self.0
            .read(move |conn| sql::questions_resolved_since(conn, since_ms))
    }

    /// Questions asked or resolved in `since_ms..=until_ms`, oldest first.
    pub fn in_window(&self, since_ms: u64, until_ms: u64) -> Result<Vec<QuestionRow>> {
        self.0
            .read(move |conn| sql::questions_in_window(conn, since_ms, until_ms))
    }

    /// Questions never resolved (what a crashed daemon left behind).
    pub fn unresolved(&self) -> Result<Vec<QuestionRow>> {
        self.0.read(|conn| sql::questions(conn, None, true))
    }
}

/// Model provider configuration (never the credential: that is in the Keychain).
#[derive(Debug, Clone, Copy)]
pub struct Providers<'a>(&'a Storage);

impl Providers<'_> {
    /// Inserts the provider or replaces everything but its creation time. Applied before it
    /// returns: a failing database is the error (blocks, like a read).
    pub fn upsert(&self, row: ProviderRow) -> Result<()> {
        self.0.write_checked(Write::UpsertProvider(Box::new(row)))
    }

    /// Applied before it returns, like [`upsert`](Self::upsert).
    pub fn delete(&self, id: &str) -> Result<()> {
        self.0.write_checked(Write::DeleteProvider(id.to_owned()))
    }

    /// Oldest first.
    pub fn list(&self) -> Result<Vec<ProviderRow>> {
        self.0.read(sql::list_providers)
    }
}

/// The worktrees Hutan created, keyed by path.
#[derive(Debug, Clone, Copy)]
pub struct Worktrees<'a>(&'a Storage);

impl Worktrees<'_> {
    /// Remembers a worktree (an existing path is replaced).
    pub fn upsert(&self, row: WorktreeRow) -> Result<()> {
        self.0.write(Write::UpsertWorktree(Box::new(row)))
    }

    pub fn delete(&self, path: PathBuf) -> Result<()> {
        self.0.write(Write::DeleteWorktree(path))
    }

    /// Oldest first.
    pub fn list(&self) -> Result<Vec<WorktreeRow>> {
        self.0.read(sql::list_worktrees)
    }
}

/// Tasks with their dependencies and sessions (roadmap M7-B).
#[derive(Debug, Clone, Copy)]
pub struct Tasks<'a>(&'a Storage);

impl Tasks<'_> {
    /// Writes the task with its dependencies and sessions, replacing what was stored. Every
    /// dependency must have been written before (or the write fails and is logged).
    pub fn upsert(&self, task: Task) -> Result<()> {
        self.0.write(Write::UpsertTask(Box::new(task)))
    }

    /// Deletes the task; the edges that name it go with it.
    pub fn delete(&self, id: TaskId) -> Result<()> {
        self.0.write(Write::DeleteTask(id))
    }

    /// Oldest first.
    pub fn list(&self) -> Result<Vec<Task>> {
        self.0.read(sql::list_tasks)
    }
}

/// Key/value metadata: schema version, epoch history, small daemon facts.
#[derive(Debug, Clone, Copy)]
pub struct AppMeta<'a>(&'a Storage);

impl AppMeta<'_> {
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_owned();
        self.0.read(move |conn| sql::get_meta(conn, &key))
    }

    /// Removes a key, applied before it returns (a failing database is the error).
    pub fn delete(&self, key: &str) -> Result<()> {
        self.0.write_checked(Write::DeleteMeta {
            key: key.to_owned(),
        })
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.0.write(Write::SetMeta {
            key: key.to_owned(),
            value: value.to_owned(),
        })
    }

    pub fn schema_version(&self) -> Result<u32> {
        self.0.read(migrate::current_version)
    }

    /// Appends a daemon run to the epoch history (the newest [`EPOCH_HISTORY_KEPT`] are kept).
    pub fn record_epoch(&self, epoch: Epoch, started_at_ms: u64) -> Result<()> {
        self.0.write(Write::RecordEpoch(EpochRecord {
            epoch,
            started_at_ms,
            end: EpochEnd::Open,
        }))
    }

    /// Notes that the run `epoch` shut itself down cleanly at `stopped_at_ms`. A run that never
    /// did is, once another one started, a crash or a kill.
    pub fn record_epoch_stop(&self, epoch: Epoch, stopped_at_ms: u64) -> Result<()> {
        self.0.write(Write::RecordEpochStop {
            epoch,
            stopped_at_ms,
        })
    }

    /// Oldest first.
    pub fn epoch_history(&self) -> Result<Vec<EpochRecord>> {
        self.0.read(sql::epoch_history)
    }
}
