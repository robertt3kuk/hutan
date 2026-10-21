//! `hutan checkpoint ls|diff|rm`: the checkpoints of agent turns (protocol 0.37, ADR 0019).

use std::collections::HashMap;

use hutan_client::{Client, ClientError};
use hutan_core::{CheckpointId, CheckpointInfo, CheckpointState, SessionId};
use hutan_protocol::ErrorCode;
use hutan_protocol::checkpoint::{
    CheckpointDiff, CheckpointDiffRequest, CheckpointFileStatus, CheckpointList,
    CheckpointListRequest, CheckpointRemove, CheckpointRemoved, DiffAgainst, LIST_MAX_LIMIT,
};

use super::Ctx;
use super::sessions::{age_text, now_ms, with_source};
use super::workspaces::{table, workspace_target};
use crate::args::{CheckpointCommand, CheckpointDiffArgs, CheckpointLsArgs, CheckpointRmArgs};
use crate::error::CliError;

/// Shortest checkpoint id prefix accepted.
const MIN_PREFIX: usize = 4;

impl Ctx {
    pub(super) async fn checkpoint(&self, command: CheckpointCommand) -> Result<(), CliError> {
        match command {
            CheckpointCommand::Ls(args) => self.checkpoint_ls(args).await,
            CheckpointCommand::Diff(args) => self.checkpoint_diff(args).await,
            CheckpointCommand::Rm(args) => self.checkpoint_rm(args).await,
        }
    }

    async fn checkpoint_ls(&self, args: CheckpointLsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session_id = match &args.session {
            Some(target) => Some(self.checkpoint_session(&client, target).await?),
            None => None,
        };
        let workspace_id = match (session_id, workspace_target(args.workspace)) {
            (None, Some((target, source))) => Some(
                self.resolve_workspace_id(&client, &target)
                    .await
                    .map_err(|e| with_source(e, &target, source))?,
            ),
            _ => None,
        };
        let before = match &args.before {
            Some(target) => Some(self.resolve_checkpoint(&client, target).await?),
            None => None,
        };
        let list = client
            .checkpoint_list(CheckpointListRequest {
                session_id,
                workspace_id,
                limit: args.limit,
                before,
            })
            .await
            .map_err(|e| self.fail(e))?;
        let titles: HashMap<SessionId, String> = if session_id.is_none() && !self.json {
            self.session_list(&client, None)
                .await?
                .sessions
                .into_iter()
                .map(|s| (s.id, s.title))
                .collect()
        } else {
            HashMap::new()
        };
        self.print(&list, || {
            ls_text(&list, session_id.is_none().then_some(&titles), now_ms())
        })
    }

    async fn checkpoint_diff(&self, args: CheckpointDiffArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let checkpoint_id = self.resolve_checkpoint(&client, &args.checkpoint).await?;
        let against = match args.against.as_str() {
            "end" => DiffAgainst::End,
            "now" => DiffAgainst::Now,
            other => DiffAgainst::Checkpoint(self.resolve_checkpoint(&client, other).await?),
        };
        let diff = client
            .checkpoint_diff(CheckpointDiffRequest {
                checkpoint_id,
                against,
                path: args.path,
                stat: args.stat,
            })
            .await
            .map_err(|e| self.fail(e))?;
        if !self.json {
            let cut: Vec<&str> = diff
                .files
                .iter()
                .filter(|f| f.truncated)
                .map(|f| f.path.as_str())
                .collect();
            if !args.stat && !cut.is_empty() {
                eprintln!(
                    "note: the patch of {} is cut (2 MiB per file, 20 MiB in all)",
                    cut.join(", ")
                );
            } else if diff.truncated && cut.is_empty() {
                eprintln!("note: the diff has more files than the daemon shows (5000)");
            }
        }
        self.print(&diff, || {
            if args.stat {
                stat_text(&diff)
            } else {
                match diff.patch.as_deref().map(|p| p.trim_end_matches('\n')) {
                    Some(patch) if !patch.is_empty() => patch.to_owned(),
                    _ => "no changes".to_owned(),
                }
            }
        })
    }

    async fn checkpoint_rm(&self, args: CheckpointRmArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let target = args.target.as_str();
        let (removed, of_session) = if let Ok(id) = target.parse::<CheckpointId>() {
            // A full id: a checkpoint's, else a session's.
            match self.remove_checkpoint(&client, id).await {
                Err(ClientError::Remote(error)) if error.code == ErrorCode::NotFound => {
                    let session = SessionId::from_ulid(id.as_ulid());
                    (self.remove_session(&client, session).await?, Some(session))
                }
                other => (other.map_err(|e| self.fail(e))?, None),
            }
        } else if let Some(id) = self.find_checkpoint(&client, target).await? {
            let removed = self
                .remove_checkpoint(&client, id)
                .await
                .map_err(|e| self.fail(e))?;
            (removed, None)
        } else {
            let session = self.resolve(&client, target).await?.id;
            (self.remove_session(&client, session).await?, Some(session))
        };
        self.print(&removed, || {
            let count = removed.removed.len();
            let noun = if count == 1 {
                "checkpoint"
            } else {
                "checkpoints"
            };
            match of_session {
                Some(session) => format!("removed {count} {noun} of session {session}"),
                None => format!("removed {count} {noun}"),
            }
        })
    }

    async fn remove_checkpoint(
        &self,
        client: &Client,
        id: CheckpointId,
    ) -> Result<CheckpointRemoved, ClientError> {
        client
            .checkpoint_remove(CheckpointRemove {
                checkpoint_id: Some(id),
                session_id: None,
            })
            .await
    }

    async fn remove_session(
        &self,
        client: &Client,
        session: SessionId,
    ) -> Result<CheckpointRemoved, CliError> {
        client
            .checkpoint_remove(CheckpointRemove {
                checkpoint_id: None,
                session_id: Some(session),
            })
            .await
            .map_err(|e| self.fail(e))
    }

    /// The session `target` names among the listed ones; a closed session by its full id.
    async fn checkpoint_session(
        &self,
        client: &Client,
        target: &str,
    ) -> Result<SessionId, CliError> {
        match self.resolve(client, target).await {
            Ok(session) => Ok(session.id),
            Err(error) => target.parse::<SessionId>().map_err(|_| error),
        }
    }

    /// The checkpoint `target` names: a full id, or a unique prefix among the newest 200.
    async fn resolve_checkpoint(
        &self,
        client: &Client,
        target: &str,
    ) -> Result<CheckpointId, CliError> {
        if let Ok(id) = target.parse::<CheckpointId>() {
            return Ok(id);
        }
        self.find_checkpoint(client, target).await?.ok_or_else(|| {
            CliError::Failed(format!(
                "no checkpoint `{target}` among the newest {LIST_MAX_LIMIT} (see `hutan checkpoint \
                 ls`; a full id always works)"
            ))
        })
    }

    /// The checkpoint whose id starts with `target` among the newest 200; `None` when none
    /// does, a usage error when several do.
    async fn find_checkpoint(
        &self,
        client: &Client,
        target: &str,
    ) -> Result<Option<CheckpointId>, CliError> {
        if target.len() < MIN_PREFIX {
            return Ok(None);
        }
        let newest = client
            .checkpoint_list(CheckpointListRequest {
                limit: Some(LIST_MAX_LIMIT),
                ..CheckpointListRequest::default()
            })
            .await
            .map_err(|e| self.fail(e))?;
        let prefix = target.to_ascii_uppercase();
        let matches: Vec<CheckpointId> = newest
            .checkpoints
            .iter()
            .map(|c| c.id)
            .filter(|id| id.to_string().starts_with(&prefix))
            .collect();
        match matches.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(*one)),
            several => Err(CliError::Usage(format!(
                "`{target}` matches {} checkpoints: {}",
                several.len(),
                several
                    .iter()
                    .map(CheckpointId::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }
}

/// One line per checkpoint; `titles` (the sessions' titles) when the list is not one
/// session's, which adds a SESSION column.
fn ls_text(
    list: &CheckpointList,
    titles: Option<&HashMap<SessionId, String>>,
    now_ms: u64,
) -> String {
    if list.checkpoints.is_empty() {
        return "no checkpoints".to_owned();
    }
    let mut header = vec!["CHECKPOINT"];
    if titles.is_some() {
        header.push("SESSION");
    }
    header.extend(["WHEN", "FILES", "LINES", "STATE", "NOTE"]);
    let rows = list
        .checkpoints
        .iter()
        .map(|c| {
            let mut row = vec![c.id.to_string()];
            if let Some(titles) = titles {
                row.push(
                    titles
                        .get(&c.session_id)
                        .cloned()
                        .unwrap_or_else(|| c.session_id.to_string()),
                );
            }
            row.extend([
                age_text(now_ms.saturating_sub(c.created_ms) / 1000),
                c.files_changed.to_string(),
                format!("+{} -{}", c.added, c.removed),
                c.state.as_str().to_owned(),
                note(c),
            ]);
            row
        })
        .collect();
    let mut text = table(&header, rows);
    if let Some(next) = list.next {
        text.push_str(&format!("\nmore: --before {next}"));
    }
    text
}

/// Why a checkpoint is not plain `ok`, and what it left out.
fn note(c: &CheckpointInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if c.state != CheckpointState::Ok
        && let Some(reason) = &c.reason
    {
        parts.push(reason.clone());
    }
    match c.skipped.len() {
        0 => {}
        1 => parts.push(format!("{} not captured", c.skipped[0].path)),
        n => parts.push(format!("{n} files not captured")),
    }
    parts.join("; ")
}

/// `M path  +1 -2` per file and a total.
fn stat_text(diff: &CheckpointDiff) -> String {
    if diff.files.is_empty() {
        return "no changes".to_owned();
    }
    let width = diff
        .files
        .iter()
        .map(|f| f.path.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<String> = diff
        .files
        .iter()
        .map(|file| {
            let status = match file.status {
                CheckpointFileStatus::Added => 'A',
                CheckpointFileStatus::Modified => 'M',
                CheckpointFileStatus::Deleted => 'D',
            };
            let counts = if file.binary {
                "binary".to_owned()
            } else {
                format!("+{} -{}", file.added, file.removed)
            };
            format!("{status} {:<width$}  {counts}", file.path)
        })
        .collect();
    let (added, removed) = diff
        .files
        .iter()
        .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
    let count = diff.files.len();
    lines.push(format!(
        "{count} {} changed, +{added} -{removed}",
        if count == 1 { "file" } else { "files" }
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::{CheckpointKind, SkipReason, SkippedFile};
    use hutan_protocol::checkpoint::CheckpointFile;

    fn info(state: CheckpointState, created_ms: u64) -> CheckpointInfo {
        CheckpointInfo {
            id: CheckpointId::new(),
            session_id: SessionId::new(),
            turn_id: None,
            kind: CheckpointKind::Turn,
            state,
            reason: None,
            created_ms,
            checkout: None,
            files_changed: 2,
            added: 3,
            removed: 1,
            bytes: 10,
            skipped: Vec::new(),
        }
    }

    #[test]
    fn the_list_says_when_how_much_and_what_was_left_out() {
        let mut late = info(CheckpointState::Late, 0);
        late.reason = Some("the start was recorded late".into());
        late.skipped = vec![SkippedFile {
            path: "big.bin".into(),
            reason: SkipReason::TooLarge,
            bytes: Some(20),
        }];
        let ok = info(CheckpointState::Ok, 60_000);
        let list = CheckpointList {
            checkpoints: vec![ok.clone(), late.clone()],
            next: Some(late.id),
        };
        let text = ls_text(&list, None, 120_000);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("CHECKPOINT") && !lines[0].contains("SESSION"));
        assert!(lines[1].contains("1m") && lines[1].contains("+3 -1") && lines[1].contains("ok"));
        assert!(
            lines[2].contains("late")
                && lines[2].contains("the start was recorded late; big.bin not captured"),
            "{text}"
        );
        assert_eq!(lines[3], format!("more: --before {}", late.id));
        let titles = HashMap::from([(ok.session_id, "fix bug".to_owned())]);
        let text = ls_text(&list, Some(&titles), 120_000);
        assert!(text.contains("SESSION") && text.contains("fix bug"));
        assert!(
            text.contains(&late.session_id.to_string()),
            "an unknown session by id"
        );
        assert_eq!(
            ls_text(
                &CheckpointList {
                    checkpoints: vec![],
                    next: None
                },
                None,
                0
            ),
            "no checkpoints"
        );
    }

    #[test]
    fn the_stat_lists_each_file_and_a_total() {
        let file = |path: &str, status, added, removed, binary| CheckpointFile {
            path: path.into(),
            status,
            added,
            removed,
            binary,
            truncated: false,
        };
        let diff = CheckpointDiff {
            files: vec![
                file("a.txt", CheckpointFileStatus::Modified, 1, 1, false),
                file("logo.png", CheckpointFileStatus::Added, 0, 0, true),
                file("old.rs", CheckpointFileStatus::Deleted, 0, 7, false),
            ],
            patch: None,
            truncated: false,
        };
        assert_eq!(
            stat_text(&diff),
            "M a.txt     +1 -1\nA logo.png  binary\nD old.rs    +0 -7\n3 files changed, +1 -8"
        );
    }
}
