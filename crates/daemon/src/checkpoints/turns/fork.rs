//! What a fork asks of the checkpoints of a chat (protocol 0.58, capability `chat.fork.worktree`,
//! ADR 0023 decision 3): the files of a turn, and the checkpoints that rewind the folder to them.
//!
//! - [`TurnCheckpoints::turn_files`]: the commit whose tree is the checkout as turn N left it
//!   (its checkpoint's end), for a new worktree. A turn that changed nothing has no end ref: the
//!   latest earlier turn that did change something stands for it, else no turn has and the base
//!   commit as it is does. A turn whose checkpoint failed, was off or unsupported is a refusal:
//!   what the files were is unknown, and a worktree is never made from a guess.
//! - [`TurnCheckpoints::restore_span`]: the first and last checkpoint of the turns after turn N,
//!   which `checkpoint.restore` rewinds to bring the folder back to turn N (the same restore, the
//!   same preview, the same Undo).

use std::path::PathBuf;

use hutan_core::{CheckpointId, CheckpointKind, CheckpointState, SessionId, SkippedFile, TurnId};
use hutan_protocol::chat::ForkFiles;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_storage::{CheckpointFilter, CheckpointRow};
use serde_json::json;

use super::TurnCheckpoints;
use super::requests::invalid_state;
use super::restore::order;

/// The files of a turn, as a commit of the chat's repository.
#[derive(Debug, Clone)]
pub struct TurnFiles {
    /// The checkout the chat worked in (its top level).
    pub checkout: PathBuf,
    /// The repository's common git directory.
    pub common_dir: PathBuf,
    /// The commit whose tree is the checkout after the turn; `None`: no turn changed a file, so
    /// the base is what the files were.
    pub commit: Option<String>,
    pub from: ForkFiles,
    /// The checkpoint `commit` is the end of.
    pub checkpoint: Option<CheckpointId>,
    /// The commit the user's HEAD was at when that snapshot was taken (its `Hutan-Head`
    /// trailer), or when the turn started for [`ForkFiles::Base`].
    pub head: Option<String>,
    /// What the snapshot left out.
    pub skipped: Vec<SkippedFile>,
}

fn unsupported(reason: &str, message: impl Into<String>) -> ErrorBody {
    let mut error = ErrorBody::new(ErrorCode::Unsupported, message);
    error.data = Some(json!({ "reason": reason }));
    error
}

/// A checkpoint that keeps the files a turn ended with.
fn keeps_end(row: &CheckpointRow) -> bool {
    matches!(row.state, CheckpointState::Ok | CheckpointState::Late)
        && row.commit.is_some()
        && row.reference.is_some()
}

/// A turn that changed no file (no end ref was written).
fn changed_nothing(row: &CheckpointRow) -> bool {
    row.state == CheckpointState::Unchanged
        || (row.state == CheckpointState::Late && row.commit.is_none())
}

impl TurnCheckpoints {
    /// The session's turn checkpoints and restores, oldest first, once the end of a turn that
    /// just ended is recorded.
    async fn settled_rows(&self, session: SessionId) -> Result<Vec<CheckpointRow>, ErrorBody> {
        let ending = self
            .slot(session)
            .and_then(|slot| slot.state.lock().ending.clone());
        if let Some(mut ending) = ending {
            let _ = ending.wait_for(|done| *done).await;
        }
        let mut rows = self
            .query("reading checkpoints", move |s| {
                s.checkpoints().list(CheckpointFilter::Session(session))
            })
            .await?;
        rows.sort_by_key(order);
        Ok(rows)
    }

    /// Why a turn has no files to build on.
    fn no_checkpoint(&self, session: SessionId, turn: TurnId) -> ErrorBody {
        let why = self
            .status(session)
            .and_then(|status| status.reason)
            .map(|reason| format!(": {reason}"))
            .unwrap_or_default();
        unsupported(
            "no_checkpoint",
            format!(
                "no checkpoint was recorded for turn {turn}{why}: checkpoints are needed to know \
                 the files of a turn (they are taken for agent chats in git checkouts, unless \
                 turned off)"
            ),
        )
    }

    /// The files of `turn` of `session` (see the module). `unsupported` with `data.reason`
    /// `no_checkpoint` when the turn has none, `failed_checkpoint` when its checkpoint (or an
    /// earlier turn's that it depends on) holds none; `invalid_state` for a turn that still runs.
    pub async fn turn_files(
        &self,
        session: SessionId,
        turn: TurnId,
    ) -> Result<TurnFiles, ErrorBody> {
        let rows = self.settled_rows(session).await?;
        let turns: Vec<&CheckpointRow> = rows
            .iter()
            .filter(|row| row.kind == CheckpointKind::Turn)
            .collect();
        let Some(at) = turns.iter().position(|row| row.turn_id == Some(turn)) else {
            return Err(self.no_checkpoint(session, turn));
        };
        let row = turns[at];
        let (Some(checkout), Some(common_dir)) = (row.checkout.clone(), row.common_dir.clone())
        else {
            return Err(self.no_checkpoint(session, turn));
        };

        let (source, from) = if row.state == CheckpointState::Running {
            return Err(invalid_state(format!("turn {turn} is still running")));
        } else if keeps_end(row) {
            (Some(row), ForkFiles::Turn)
        } else if changed_nothing(row) {
            // The latest earlier turn that changed something has the files; one that left no
            // record puts the files in doubt.
            let mut found = None;
            for earlier in turns[..at].iter().rev() {
                if keeps_end(earlier) {
                    found = Some(*earlier);
                    break;
                }
                if !changed_nothing(earlier) {
                    return Err(unsupported(
                        "failed_checkpoint",
                        format!(
                            "turn {turn} changed no file, and the files before it are unknown: \
                             the checkpoint of an earlier turn holds none ({}{})",
                            earlier.state.as_str(),
                            earlier
                                .reason
                                .as_deref()
                                .map(|reason| format!(": {reason}"))
                                .unwrap_or_default()
                        ),
                    ));
                }
            }
            match found {
                Some(earlier) => (Some(earlier), ForkFiles::Earlier),
                None => (None, ForkFiles::Base),
            }
        } else {
            return Err(unsupported(
                "failed_checkpoint",
                format!(
                    "the checkpoint of turn {turn} holds no files ({}{})",
                    row.state.as_str(),
                    row.reason
                        .as_deref()
                        .map(|reason| format!(": {reason}"))
                        .unwrap_or_default()
                ),
            ));
        };

        let Some(source) = source else {
            let head = match &row.start_commit {
                Some(start) => self.engine.head_at(&common_dir, start).await.ok().flatten(),
                None => None,
            };
            return Ok(TurnFiles {
                checkout,
                common_dir,
                commit: None,
                from,
                checkpoint: None,
                head,
                skipped: Vec::new(),
            });
        };
        let commit = source.commit.clone();
        let head = match &commit {
            Some(commit) => self
                .engine
                .head_at(&common_dir, commit)
                .await
                .ok()
                .flatten(),
            None => None,
        };
        Ok(TurnFiles {
            checkout,
            common_dir,
            commit,
            from,
            checkpoint: Some(source.id),
            head,
            skipped: source.skipped.clone(),
        })
    }

    /// The checkpoints a restore rewinds to bring the folder back to the end of `turn`: from
    /// the first one after it that keeps files through the last. `None` when no turn after it
    /// changed a file. `unsupported` when `turn` has no checkpoint at all.
    pub async fn restore_span(
        &self,
        session: SessionId,
        turn: TurnId,
    ) -> Result<Option<(CheckpointId, CheckpointId)>, ErrorBody> {
        let rows = self.settled_rows(session).await?;
        let Some(at) = rows
            .iter()
            .position(|row| row.kind == CheckpointKind::Turn && row.turn_id == Some(turn))
        else {
            return Err(self.no_checkpoint(session, turn));
        };
        let mut after = rows[at + 1..]
            .iter()
            .filter(|row| matches!(row.state, CheckpointState::Ok | CheckpointState::Late));
        let first = after.next().map(|row| row.id);
        let last = after.next_back().map(|row| row.id).or(first);
        Ok(first.zip(last))
    }
}
