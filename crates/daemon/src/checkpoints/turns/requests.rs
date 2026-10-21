//! The requests of clients (protocol 0.37, capability `checkpoint`): `checkpoint.list`,
//! `checkpoint.diff` and `checkpoint.remove`. Reads go to SQLite and to the repository's object
//! store; `against: "now"` takes a snapshot of the checkout through Hutan's own index (a
//! warm-up: no commit, no ref), never the user's index; a removal deletes the refs of each
//! repository in one `update-ref` transaction, then the rows in one SQLite transaction.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use hutan_core::{CheckpointId, CheckpointState};
use hutan_protocol::checkpoint::{
    CheckpointDiff, CheckpointDiffRequest, CheckpointList, CheckpointListRequest, CheckpointRemove,
    CheckpointRemoved, DiffAgainst, LIST_DEFAULT_LIMIT, LIST_MAX_LIMIT,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_storage::{CheckpointFilter, CheckpointPage, CheckpointRow, CheckpointScope, Storage};

use super::{CheckpointError, Label, TurnCheckpoints};
use crate::config::CheckpointMode;

/// The states `checkpoint.list` shows (and `checkpoint.created` announces): a checkpoint that
/// holds files, or one that says why it does not. A running turn, a turn that changed nothing
/// and a turn whose folder cannot have checkpoints are left out.
pub const LISTED: &[CheckpointState] = &[
    CheckpointState::Ok,
    CheckpointState::Late,
    CheckpointState::Failed,
    CheckpointState::Off,
];

fn bad_request(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::BadRequest, message)
}

fn invalid_state(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidState, message)
}

/// What a client is told about a failure of the engine.
fn engine_error(error: CheckpointError) -> ErrorBody {
    let code = match &error {
        CheckpointError::NotARepository { .. }
        | CheckpointError::Unsupported(_)
        | CheckpointError::GitMissing => ErrorCode::Unsupported,
        CheckpointError::SwitchedOff { .. }
        | CheckpointError::LowDisk { .. }
        | CheckpointError::GitFailed { .. }
        | CheckpointError::TimedOut { .. } => ErrorCode::InvalidState,
        CheckpointError::InvalidLabel(_) => ErrorCode::BadRequest,
        CheckpointError::Io { .. } | CheckpointError::Internal(_) => ErrorCode::Internal,
    };
    ErrorBody::new(code, error.to_string())
}

/// A file or directory inside a checkout: relative, no `..`, no NUL.
fn validate_relative(path: &str) -> Result<(), ErrorBody> {
    let plain = !path.is_empty()
        && !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
    if plain {
        Ok(())
    } else {
        Err(bad_request(format!(
            "`{path}` is not a path inside the checkout"
        )))
    }
}

/// The start and end commits a checkpoint keeps, and its repository.
struct Recorded {
    common_dir: PathBuf,
    start: String,
    end: String,
}

/// The commits of `row`; `invalid_state` for a checkpoint that keeps none, saying why.
fn recorded(row: &CheckpointRow) -> Result<Recorded, ErrorBody> {
    if row.state == CheckpointState::Running {
        return Err(invalid_state(format!(
            "the turn of checkpoint {} is still running",
            row.id
        )));
    }
    match (&row.common_dir, &row.start_commit, &row.commit) {
        (Some(common_dir), Some(start), Some(end)) if row.reference.is_some() => Ok(Recorded {
            common_dir: common_dir.clone(),
            start: start.clone(),
            end: end.clone(),
        }),
        _ => Err(invalid_state(format!(
            "checkpoint {} keeps no files ({}{})",
            row.id,
            row.state.as_str(),
            row.reason
                .as_deref()
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        ))),
    }
}

impl TurnCheckpoints {
    /// Runs `op` on the database; a failure is the client's `internal` error.
    async fn query<T: Send + 'static>(
        &self,
        what: &'static str,
        op: impl FnOnce(&Storage) -> Result<T, hutan_storage::StorageError> + Send + 'static,
    ) -> Result<T, ErrorBody> {
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || op(&storage))
            .await
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("{what}: {e}")))?
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("{what}: {e}")))
    }

    /// The checkpoint `id`; `not_found` when there is none.
    async fn row(&self, id: CheckpointId) -> Result<CheckpointRow, ErrorBody> {
        self.query("reading a checkpoint", move |s| s.checkpoints().get(id))
            .await?
            .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("no checkpoint {id}")))
    }

    /// `checkpoint.list`.
    pub async fn list(&self, req: CheckpointListRequest) -> Result<CheckpointList, ErrorBody> {
        let scope = match (req.session_id, req.workspace_id) {
            (Some(_), Some(_)) => {
                return Err(bad_request("name a session or a workspace, not both"));
            }
            (Some(session), None) => CheckpointScope::Session(session),
            (None, Some(workspace)) => CheckpointScope::Workspace(workspace),
            (None, None) => CheckpointScope::All,
        };
        let limit = req.limit.unwrap_or(LIST_DEFAULT_LIMIT);
        if limit == 0 || limit > LIST_MAX_LIMIT {
            return Err(bad_request(format!(
                "limit must be 1 to {LIST_MAX_LIMIT}, not {limit}"
            )));
        }
        if let Some(before) = req.before {
            self.row(before).await?;
        }
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let mut rows = self
            .query("listing checkpoints", move |s| {
                s.checkpoints().page(CheckpointPage {
                    scope,
                    states: LISTED,
                    before: req.before,
                    // One more says whether there is a next page.
                    limit: limit + 1,
                })
            })
            .await?;
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|row| row.id)
        } else {
            None
        };
        Ok(CheckpointList {
            checkpoints: rows.iter().map(CheckpointRow::info).collect(),
            next,
        })
    }

    /// `checkpoint.diff`.
    pub async fn diff(&self, req: CheckpointDiffRequest) -> Result<CheckpointDiff, ErrorBody> {
        if let Some(path) = &req.path {
            validate_relative(path)?;
        }
        let row = self.row(req.checkpoint_id).await?;
        let from = recorded(&row)?;
        if !from.common_dir.is_dir() {
            return Err(invalid_state(format!(
                "the repository of checkpoint {} ({}) is gone",
                row.id,
                from.common_dir.display()
            )));
        }
        let to = match req.against {
            DiffAgainst::End => from.end.clone(),
            DiffAgainst::Now => self.tree_now(&row, &from.common_dir).await?,
            DiffAgainst::Checkpoint(other) => {
                let other = self.row(other).await?;
                let through = recorded(&other)?;
                if through.common_dir != from.common_dir {
                    return Err(bad_request(format!(
                        "checkpoints {} and {} are of different repositories",
                        row.id, other.id
                    )));
                }
                through.end
            }
        };
        self.engine
            .diff(
                &from.common_dir,
                &from.start,
                &to,
                req.path.as_deref(),
                req.stat,
            )
            .await
            .map_err(engine_error)
    }

    /// The tree of `row`'s checkout as it is now: a warm-up snapshot through Hutan's private
    /// index (it writes objects, never a commit, a ref or the user's index).
    async fn tree_now(&self, row: &CheckpointRow, common_dir: &Path) -> Result<String, ErrorBody> {
        let checkout = row
            .checkout
            .clone()
            .ok_or_else(|| invalid_state(format!("checkpoint {} names no checkout", row.id)))?;
        if !checkout.is_dir() {
            return Err(invalid_state(format!(
                "the checkout of checkpoint {} ({}) is gone",
                row.id,
                checkout.display()
            )));
        }
        let project = match self.slot(row.session_id) {
            Some(slot) => self.project(&slot),
            None => self.settings.read().for_project("", ""),
        };
        if project.mode == CheckpointMode::Off {
            return Err(invalid_state(
                "checkpoints are turned off in the settings, and comparing with the checkout now \
                 needs a snapshot of it",
            ));
        }
        let snapshot = self
            .engine
            .snapshot_with(
                &checkout,
                Label::warm(row.session_id.to_string()),
                self.caps(&project),
            )
            .await
            .map_err(engine_error)?;
        if snapshot.common_dir != common_dir {
            return Err(invalid_state(format!(
                "{} is no longer a checkout of the repository at {}",
                checkout.display(),
                common_dir.display()
            )));
        }
        Ok(snapshot.tree)
    }

    /// `checkpoint.remove`.
    pub async fn remove(&self, req: CheckpointRemove) -> Result<CheckpointRemoved, ErrorBody> {
        // Retention does not run meanwhile.
        let _one = self.upkeep.lock().await;
        let rows = match (req.checkpoint_id, req.session_id) {
            (Some(id), None) => vec![self.row(id).await?],
            (None, Some(session)) => {
                let rows = self
                    .query("reading checkpoints", move |s| {
                        s.checkpoints().list(CheckpointFilter::Session(session))
                    })
                    .await?;
                if rows.is_empty() {
                    return Err(ErrorBody::new(
                        ErrorCode::NotFound,
                        format!("session {session} has no checkpoints"),
                    ));
                }
                rows
            }
            _ => {
                return Err(bad_request(
                    "name a checkpoint (`checkpoint_id`) or a session (`session_id`), one of them",
                ));
            }
        };
        if let Some(running) = rows.iter().find(|r| r.state == CheckpointState::Running) {
            return Err(invalid_state(format!(
                "the turn of checkpoint {} is running; remove it once the turn ended",
                running.id
            )));
        }
        let mut refs: BTreeMap<&Path, Vec<String>> = BTreeMap::new();
        for row in &rows {
            if let (Some(dir), Some(reference)) = (&row.common_dir, &row.reference) {
                refs.entry(dir.as_path())
                    .or_default()
                    .push(reference.clone());
            }
        }
        // Refs first: a ref without a row would be adopted again by the next reconcile.
        for (dir, names) in refs {
            if dir.is_dir() {
                self.engine
                    .delete_refs(dir, &names)
                    .await
                    .map_err(engine_error)?;
            }
        }
        let ids: Vec<CheckpointId> = rows.iter().map(|row| row.id).collect();
        let gone = ids.clone();
        self.query("removing checkpoints", move |s| {
            s.checkpoints().delete(gone)
        })
        .await?;
        tracing::info!(count = ids.len(), "checkpoints removed");
        self.announce_removed(ids.clone());
        Ok(CheckpointRemoved { removed: ids })
    }
}
