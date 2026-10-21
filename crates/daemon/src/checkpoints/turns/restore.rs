//! Restoring files to before turns (protocol 0.38, capability `checkpoint.restore`, ADR 0019
//! Decision 8): `checkpoint.preview` and `checkpoint.restore`.
//!
//! Both rewind the checkpoints of one session from the one named through `through` (the turns
//! and the earlier restores in between, oldest first). Each one's changes are read from its
//! start and end commits; the checkout now is read through a snapshot of Hutan's own index,
//! never the user's; [`hutan_core::restore_plan`] decides.
//!
//! A restore holds the checkout ([`crate::checkpoints::Exclusive`]: no turn's snapshot runs
//! meanwhile) and is a checkpoint of its own (kind `restore`, its parent the checkpoint
//! restored): its start (`restore-pre`, ref `<session>/<id>-start`) is taken first and the
//! plan is made again from it; a digest other than the preview's answers `conflict` with the
//! new preview and nothing is written. Then the files are deleted and written (each announced
//! to the file service first, so `fs.changed` says `origin: restore`), and its end
//! (`restore-post`, ref `<session>/<id>`, whose parent is the start) is taken. Restoring that
//! checkpoint is Undo; restoring the Undo's is Undo of Undo.
//!
//! Refused: an agent's restore (`router`: user-only), a turn running in the checkout
//! (`invalid_state`), a chain that holds a `late` checkpoint (its start may hold some of its
//! turn's edits), checkpoints turned off (the restore's own checkpoint is what Undo needs).

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hutan_core::{
    CheckpointId, CheckpointKind, CheckpointState, FileMode, Present, RestoreAction, RestoreEntry,
    RestorePlan, SessionId, SkipReason, SkippedFile, TurnChanges, restore_plan,
};
use hutan_protocol::checkpoint::{
    CheckpointPreview, CheckpointPreviewRequest, CheckpointRestore, CheckpointRestored, HeadMoved,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{AgentState, SessionInfo};
use hutan_storage::{CheckpointFilter, CheckpointRow};
use tracing::{info, warn};

use super::requests::{bad_request, engine_error, invalid_state, recorded, validate_relative};
use super::{TurnCheckpoints, now_ms, skipped};
use crate::checkpoints::restore::Writer;
use crate::checkpoints::{Caps, Kind, Label, Snapshot};

/// Told of each path a restore changes (a file it writes or deletes, a folder it makes or
/// removes), before it changes: the file service's watchers attribute the change.
pub type Announce = Arc<dyn Fn(CheckpointId, &Path) + Send + Sync>;

/// What the rest of the daemon tells a preview or a restore.
#[derive(Clone)]
pub struct RestoreContext {
    /// The daemon's sessions (`session.list`): who works in the checkout, whose turn runs.
    pub sessions: Vec<SessionInfo>,
    pub announce: Announce,
}

impl Default for RestoreContext {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            announce: Arc::new(|_, _| {}),
        }
    }
}

impl fmt::Debug for RestoreContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestoreContext")
            .field("sessions", &self.sessions.len())
            .finish_non_exhaustive()
    }
}

/// The checkpoints a restore rewinds, oldest first, and where they are.
#[derive(Debug, Clone)]
struct Chain {
    session: SessionId,
    /// Those that keep files.
    rows: Vec<CheckpointRow>,
    checkout: PathBuf,
    common_dir: PathBuf,
}

/// Who else is in the checkout.
#[derive(Debug, Default)]
struct Occupants {
    /// The chat's agent.
    agent: Option<String>,
    shared_with: Vec<SessionId>,
    busy: Vec<SessionId>,
}

/// What a restore did.
#[derive(Debug, Default)]
struct Applied {
    written: Vec<String>,
    deleted: Vec<String>,
    /// Planned writes or deletes that could not be done, with the reason.
    failed: Vec<RestoreEntry>,
}

fn same_place(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
    a == b || canonical(a) == canonical(b)
}

fn within(path: &Path, dir: &Path) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
    path.starts_with(dir) || canonical(path).starts_with(canonical(dir))
}

/// The sessions of `sessions` in `checkout`, and the agent of `session`.
fn occupants(sessions: &[SessionInfo], checkout: &Path, session: SessionId) -> Occupants {
    let mut found = Occupants::default();
    for s in sessions {
        if s.id == session {
            found.agent = s.chat.as_ref().map(|chat| chat.agent.clone());
        }
        let here = match &s.placement {
            Some(placement) => same_place(&placement.root, checkout),
            None => s.cwd.as_deref().is_some_and(|cwd| within(cwd, checkout)),
        };
        if !here || !s.state.is_live() {
            continue;
        }
        // Mawas and direct model chats edit no files.
        let edits = s.agent.is_some()
            || s.chat
                .as_ref()
                .is_some_and(|chat| !chat.is_assistant() && chat.provider_id.is_none());
        if !edits {
            continue;
        }
        let running = s.chat.as_ref().is_some_and(|chat| chat.turn_active)
            || s.agent
                .as_ref()
                .is_some_and(|a| a.state == AgentState::Working);
        if running {
            found.busy.push(s.id);
        }
        if s.id != session {
            found.shared_with.push(s.id);
        }
    }
    found
}

fn conflict(message: impl Into<String>, preview: &CheckpointPreview) -> ErrorBody {
    let mut error = ErrorBody::new(ErrorCode::Conflict, message);
    error.data = serde_json::to_value(preview).ok();
    error
}

fn internal(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, message)
}

/// Orders checkpoints the way they were made.
fn order(row: &CheckpointRow) -> (u64, CheckpointId) {
    (row.created_ms, row.id)
}

impl TurnCheckpoints {
    /// The checkpoints of the session of `first` from it through `through`, oldest first.
    async fn chain(
        &self,
        first: CheckpointId,
        through: Option<CheckpointId>,
    ) -> Result<Chain, ErrorBody> {
        let first = self.row(first).await?;
        let last = match through {
            Some(id) => self.row(id).await?,
            None => first.clone(),
        };
        if last.session_id != first.session_id {
            return Err(bad_request(format!(
                "checkpoints {} and {} are of different sessions",
                first.id, last.id
            )));
        }
        if order(&last) < order(&first) {
            return Err(bad_request(format!(
                "`through` ({}) is older than the checkpoint ({})",
                last.id, first.id
            )));
        }
        let session = first.session_id;
        let rows = self
            .query("reading checkpoints", move |s| {
                s.checkpoints().list(CheckpointFilter::Session(session))
            })
            .await?;
        let mut chain = Vec::new();
        for row in rows
            .into_iter()
            .filter(|r| order(r) >= order(&first) && order(r) <= order(&last))
        {
            match row.state {
                CheckpointState::Late => {
                    return Err(invalid_state(format!(
                        "checkpoint {} was recorded late: its start may hold some of its turn's \
                         edits, so it cannot be restored",
                        row.id
                    )));
                }
                CheckpointState::Running => {
                    return Err(invalid_state(format!(
                        "the turn of checkpoint {} is still running",
                        row.id
                    )));
                }
                CheckpointState::Ok => {
                    recorded(&row)?;
                    chain.push(row);
                }
                // Nothing kept: a turn that changed nothing, failed or was not recorded (what
                // it changed shows as a gap between its neighbours).
                CheckpointState::Unchanged
                | CheckpointState::Failed
                | CheckpointState::Off
                | CheckpointState::Unsupported => {}
            }
        }
        for end in [&first, &last] {
            recorded(end)?;
        }
        let (Some(checkout), Some(common_dir)) = (first.checkout.clone(), first.common_dir.clone())
        else {
            return Err(invalid_state(format!(
                "checkpoint {} names no checkout",
                first.id
            )));
        };
        if let Some(other) = chain
            .iter()
            .find(|r| r.common_dir.as_ref() != Some(&common_dir))
        {
            return Err(bad_request(format!(
                "checkpoints {} and {} are of different repositories",
                first.id, other.id
            )));
        }
        if !checkout.is_dir() {
            return Err(invalid_state(format!(
                "the checkout of checkpoint {} ({}) is gone",
                first.id,
                checkout.display()
            )));
        }
        Ok(Chain {
            session,
            rows: chain,
            checkout,
            common_dir,
        })
    }

    /// What each checkpoint of `chain` changed, with what it left out and what the user saved
    /// meanwhile.
    async fn turn_changes(&self, chain: &Chain) -> Result<Vec<TurnChanges>, ErrorBody> {
        let mut turns = Vec::with_capacity(chain.rows.len());
        for row in &chain.rows {
            let kept = recorded(row)?;
            let changes = self
                .engine
                .tree_changes(&chain.common_dir, &kept.start, &kept.end)
                .await
                .map_err(engine_error)?;
            let mut turn = TurnChanges::new(row.id);
            turn.changes = changes.files;
            turn.skipped.clone_from(&row.skipped);
            turn.skipped
                .extend(changes.unnamed.into_iter().map(|path| SkippedFile {
                    path,
                    reason: SkipReason::Unreadable,
                    bytes: None,
                }));
            turn.during = row.during.iter().cloned().collect();
            turns.push(turn);
        }
        Ok(turns)
    }

    /// The checkout as `snapshot` saw it, at the paths `turns` changed.
    async fn present(
        &self,
        chain: &Chain,
        snapshot: &Snapshot,
        turns: &[TurnChanges],
    ) -> Result<Present, ErrorBody> {
        let paths: BTreeSet<&str> = turns
            .iter()
            .flat_map(|t| t.changes.keys().map(String::as_str))
            .collect();
        let paths: Vec<&str> = paths.into_iter().collect();
        let files = self
            .engine
            .tree_files(&chain.common_dir, &snapshot.tree, &paths)
            .await
            .map_err(engine_error)?;
        let skipped = snapshot
            .skipped
            .iter()
            .map(|skip| SkippedFile {
                path: skip.path.to_string_lossy().into_owned(),
                reason: skip.reason.clone(),
                bytes: skip.bytes,
            })
            .collect();
        Ok(Present { files, skipped })
    }

    /// The bounds of a restore's snapshots; `invalid_state` while checkpoints are off.
    fn restore_caps(&self, session: SessionId) -> Result<Caps, ErrorBody> {
        let project = match self.slot(session) {
            Some(slot) => self.project(&slot),
            None => self.settings.read().for_project("", ""),
        };
        if project.mode == crate::config::CheckpointMode::Off {
            return Err(invalid_state(
                "checkpoints are turned off in the settings; a restore needs them (what it \
                 replaces is kept in a checkpoint of its own, which Undo restores)",
            ));
        }
        Ok(self.caps(&project))
    }

    /// HEAD at the start of the first checkpoint and at the end of the last, when they differ.
    async fn head_moved(&self, chain: &Chain) -> Option<HeadMoved> {
        let (first, last) = (chain.rows.first()?, chain.rows.last()?);
        let at = |commit: Option<&String>| {
            let commit = commit.cloned();
            async move {
                let commit = commit?;
                self.engine
                    .head_at(&chain.common_dir, &commit)
                    .await
                    .ok()
                    .flatten()
            }
        };
        let from = at(first.start_commit.as_ref()).await;
        let to = at(last.commit.as_ref()).await;
        (from != to).then_some(HeadMoved { from, to })
    }

    async fn preview_of(
        &self,
        chain: &Chain,
        plan: &RestorePlan,
        ctx: &RestoreContext,
    ) -> CheckpointPreview {
        let occupants = occupants(&ctx.sessions, &chain.checkout, chain.session);
        CheckpointPreview {
            plan_digest: plan.digest.clone(),
            entries: plan.entries(),
            session_id: chain.session,
            agent: occupants.agent,
            checkout: chain.checkout.clone(),
            checkpoints: chain.rows.iter().map(|r| r.id).collect(),
            head_moved: self.head_moved(chain).await,
            shared_with: occupants.shared_with,
            busy: occupants.busy,
        }
    }

    /// `checkpoint.preview`.
    pub async fn preview(
        &self,
        req: CheckpointPreviewRequest,
        ctx: &RestoreContext,
    ) -> Result<CheckpointPreview, ErrorBody> {
        for path in &req.paths {
            validate_relative(path)?;
        }
        let chain = self.chain(req.checkpoint_id, req.through).await?;
        let caps = self.restore_caps(chain.session)?;
        let turns = self.turn_changes(&chain).await?;
        let now = self
            .engine
            .snapshot_with(
                &chain.checkout,
                Label::warm(chain.session.to_string()),
                caps,
            )
            .await
            .map_err(engine_error)?;
        if now.common_dir != chain.common_dir {
            return Err(invalid_state(format!(
                "{} is no longer a checkout of the repository at {}",
                chain.checkout.display(),
                chain.common_dir.display()
            )));
        }
        let present = self.present(&chain, &now, &turns).await?;
        let plan = restore_plan(&turns, &present, &req.paths);
        Ok(self.preview_of(&chain, &plan, ctx).await)
    }

    /// `checkpoint.restore`. Runs on its own task: a client that goes away does not stop it
    /// halfway.
    pub async fn restore(
        self: &Arc<Self>,
        req: CheckpointRestore,
        ctx: RestoreContext,
    ) -> Result<CheckpointRestored, ErrorBody> {
        for path in req.paths.iter().chain(&req.accept_conflicts) {
            validate_relative(path)?;
        }
        let chain = self.chain(req.checkpoint_id, req.through).await?;
        let caps = self.restore_caps(chain.session)?;
        let busy = occupants(&ctx.sessions, &chain.checkout, chain.session).busy;
        if !busy.is_empty() {
            let names: Vec<String> = busy.iter().map(ToString::to_string).collect();
            return Err(invalid_state(format!(
                "a turn is running in {} (session {}): interrupt it or let it end, then restore",
                chain.checkout.display(),
                names.join(", ")
            )));
        }
        let turns = self.turn_changes(&chain).await?;
        let this = Arc::clone(self);
        tokio::spawn(async move { this.restore_held(req, chain, turns, caps, ctx).await })
            .await
            .map_err(|error| internal(format!("the restore stopped: {error}")))?
    }

    async fn restore_held(
        &self,
        req: CheckpointRestore,
        chain: Chain,
        turns: Vec<TurnChanges>,
        caps: Caps,
        ctx: RestoreContext,
    ) -> Result<CheckpointRestored, ErrorBody> {
        let mut held = self
            .engine
            .exclusive(&chain.checkout)
            .await
            .map_err(engine_error)?;
        if held.common_dir() != chain.common_dir {
            return Err(invalid_state(format!(
                "{} is no longer a checkout of the repository at {}",
                chain.checkout.display(),
                chain.common_dir.display()
            )));
        }
        let id = CheckpointId::new();
        let session = chain.session;
        let mut row = CheckpointRow::new(
            id,
            session,
            CheckpointKind::Restore,
            CheckpointState::Running,
            now_ms(),
        );
        row.parent_id = Some(req.checkpoint_id);
        row.checkout = Some(held.checkout().to_owned());
        row.common_dir = Some(chain.common_dir.clone());
        let pre_label = Label::restore(session.to_string(), id.to_string(), Kind::RestorePre);
        row.reference = pre_label.reference();
        // Without its row no ref may be written (reconcile would take it for a leftover).
        if !self.write(row.clone()).await {
            return Err(internal("the restore could not be recorded"));
        }
        let pre = match held.snapshot(&pre_label, &caps).await {
            Ok(pre) => pre,
            Err(error) => {
                self.abandon(&row, None).await;
                return Err(engine_error(error));
            }
        };
        let Some(pre_commit) = pre.commit.clone() else {
            self.abandon(&row, pre.reference.clone()).await;
            return Err(internal("the snapshot before the restore wrote no commit"));
        };
        row.start_commit = Some(pre_commit.clone());

        let present = match self.present(&chain, &pre, &turns).await {
            Ok(present) => present,
            Err(error) => {
                self.abandon(&row, pre.reference.clone()).await;
                return Err(error);
            }
        };
        let mut plan = restore_plan(&turns, &present, &req.paths);
        if plan.digest != req.plan_digest {
            let preview = self.preview_of(&chain, &plan, &ctx).await;
            self.abandon(&row, pre.reference.clone()).await;
            return Err(conflict(
                "the checkout changed since the preview: nothing was restored; review the new \
                 plan",
                &preview,
            ));
        }
        if let Err(error) = plan.accept(&req.accept_conflicts) {
            self.abandon(&row, pre.reference.clone()).await;
            return Err(bad_request(error.to_string()));
        }
        if !plan.changes_anything() {
            self.abandon(&row, pre.reference.clone()).await;
            return Err(invalid_state(
                "nothing to restore: every file is as it was, or left alone (see the preview)",
            ));
        }

        let applied = self
            .apply(&chain, held.checkout(), id, &plan, &caps, &ctx.announce)
            .await;
        let post_label = Label::restore(session.to_string(), id.to_string(), Kind::RestorePost)
            .with_parent(pre_commit.clone());
        let post = held.snapshot(&post_label, &caps).await;
        drop(held);
        row.ended_ms = Some(now_ms());
        let post = match post {
            Ok(post) => post,
            Err(error) => {
                // The files are written: what they replaced stays in the start's ref.
                row.state = CheckpointState::Failed;
                row.reason = Some(format!("the snapshot after the restore failed: {error}"));
                row.skipped = skipped(&pre, None);
                self.record(row.clone()).await;
                return Err(internal(format!(
                    "{} files were restored, but the snapshot after the restore failed ({error}); \
                     the checkout before it is kept in {}",
                    applied.written.len() + applied.deleted.len(),
                    row.reference.as_deref().unwrap_or("its checkpoint")
                )));
            }
        };
        row.state = CheckpointState::Ok;
        row.commit.clone_from(&post.commit);
        row.tree = Some(post.tree.clone());
        row.reference.clone_from(&post.reference);
        row.bytes = pre.bytes_hashed.saturating_add(post.bytes_hashed);
        row.skipped = skipped(&pre, Some(&post));
        if let Some(commit) = &post.commit {
            match self
                .engine
                .diff_stat(&chain.common_dir, &pre_commit, commit)
                .await
            {
                Ok(stat) => {
                    row.files_changed = stat.files;
                    row.lines_added = stat.added;
                    row.lines_removed = stat.removed;
                }
                Err(error) => warn!(%error, "checkpoints: counting a restore's changes"),
            }
        }
        info!(
            checkpoint = %id,
            restored = %req.checkpoint_id,
            written = applied.written.len(),
            deleted = applied.deleted.len(),
            failed = applied.failed.len(),
            "files restored"
        );
        // The row names the end ref before the start ref goes.
        self.record(row.clone()).await;
        self.delete_refs(&chain.common_dir, pre.reference.clone())
            .await;
        let mut left: Vec<RestoreEntry> = plan
            .files
            .iter()
            .filter(|f| f.entry.action == RestoreAction::Skip)
            .map(|f| f.entry.clone())
            .collect();
        left.extend(applied.failed);
        left.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(CheckpointRestored {
            checkpoint: row.info(),
            written: applied.written,
            deleted: applied.deleted,
            skipped: left,
        })
    }

    /// A restore that wrote nothing: its row goes, then the start's ref.
    async fn abandon(&self, row: &CheckpointRow, reference: Option<String>) {
        let id = row.id;
        self.store("removing a restore that wrote nothing", move |s| {
            s.checkpoints().delete(vec![id])
        })
        .await;
        if let Some(common_dir) = &row.common_dir {
            self.delete_refs(common_dir, reference).await;
        }
    }

    /// Deletes, then writes, what `plan` says (deletions first: a file may take the place of a
    /// folder the restore empties).
    async fn apply(
        &self,
        chain: &Chain,
        checkout: &Path,
        id: CheckpointId,
        plan: &RestorePlan,
        caps: &Caps,
        announce: &Announce,
    ) -> Applied {
        let mut applied = Applied::default();
        let fail = |entry: &RestoreEntry, why: String| RestoreEntry {
            action: RestoreAction::Skip,
            reason: Some(format!("not restored: {why}")),
            accept: None,
            ..entry.clone()
        };
        let root = checkout.to_owned();
        let writer = match tokio::task::spawn_blocking(move || Writer::open(&root)).await {
            Ok(Ok(writer)) => Arc::new(writer),
            Ok(Err(error)) => {
                applied.failed = plan
                    .files
                    .iter()
                    .filter(|f| f.entry.action != RestoreAction::Skip)
                    .map(|f| fail(&f.entry, format!("the checkout cannot be opened: {error}")))
                    .collect();
                return applied;
            }
            Err(error) => {
                applied.failed = plan
                    .files
                    .iter()
                    .filter(|f| f.entry.action != RestoreAction::Skip)
                    .map(|f| fail(&f.entry, error.to_string()))
                    .collect();
                return applied;
            }
        };
        let tell = {
            let announce = Arc::clone(announce);
            Arc::new(move |path: &Path| announce(id, path))
        };
        for file in plan
            .files
            .iter()
            .filter(|f| f.entry.action == RestoreAction::Delete)
        {
            let (writer, tell, path) = (
                Arc::clone(&writer),
                Arc::clone(&tell),
                file.entry.path.clone(),
            );
            let deleted = tokio::task::spawn_blocking(move || writer.delete(&path, &*tell)).await;
            match deleted {
                Ok(Ok(())) => applied.deleted.push(file.entry.path.clone()),
                Ok(Err(why)) => applied.failed.push(fail(&file.entry, why)),
                Err(error) => applied.failed.push(fail(&file.entry, error.to_string())),
            }
        }
        let writes: Vec<_> = plan
            .files
            .iter()
            .filter(|f| f.entry.action == RestoreAction::Write)
            .collect();
        if writes.is_empty() {
            return applied;
        }
        let mut blobs = match self.engine.blobs(&chain.common_dir) {
            Ok(blobs) => blobs,
            Err(error) => {
                applied
                    .failed
                    .extend(writes.iter().map(|f| fail(&f.entry, error.to_string())));
                return applied;
            }
        };
        for file in writes {
            let Some(want) = file.want.clone().filter(|w| w.mode.is_file()) else {
                applied
                    .failed
                    .push(fail(&file.entry, "nothing to write it from".to_owned()));
                continue;
            };
            let bytes = match blobs.read(&want.oid, caps.max_file_bytes).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    applied.failed.push(fail(&file.entry, error.to_string()));
                    continue;
                }
            };
            let (writer, tell, path) = (
                Arc::clone(&writer),
                Arc::clone(&tell),
                file.entry.path.clone(),
            );
            let mode: FileMode = want.mode;
            let written =
                tokio::task::spawn_blocking(move || writer.write(&path, &bytes, mode, &*tell))
                    .await;
            match written {
                Ok(Ok(())) => applied.written.push(file.entry.path.clone()),
                Ok(Err(why)) => applied.failed.push(fail(&file.entry, why)),
                Err(error) => applied.failed.push(fail(&file.entry, error.to_string())),
            }
        }
        applied
    }
}
