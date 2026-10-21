//! `hutan checkpoint restore|undo` (protocol 0.38, ADR 0019 Decision 11): the preview first,
//! then a question whose default is No (`--yes` without a terminal); a conflict is overwritten
//! only when `--accept-conflicts` names it. A restore is a checkpoint of its own: `undo`
//! restores the latest one, and undoing again undoes the undo.

use std::collections::BTreeSet;
use std::io::{BufRead as _, IsTerminal as _, Write as _};

use hutan_client::{Client, ClientError};
use hutan_core::{CheckpointId, CheckpointKind, CheckpointState, RestoreAction, RestoreState};
use hutan_protocol::ErrorCode;
use hutan_protocol::checkpoint::{
    CheckpointListRequest, CheckpointPreview, CheckpointPreviewRequest, CheckpointRestore,
    CheckpointRestored, LIST_MAX_LIMIT, restore_label,
};

use super::super::Ctx;
use super::super::workspaces::table;
use crate::args::{CheckpointRestoreArgs, CheckpointUndoArgs};
use crate::error::CliError;

/// What to restore.
struct Wanted {
    checkpoint: CheckpointId,
    through: Option<CheckpointId>,
    paths: Vec<String>,
    accept: Vec<String>,
    yes: bool,
    /// `restore` or `undo`, for the words.
    verb: &'static str,
}

impl Ctx {
    pub(in crate::commands) async fn checkpoint_restore(
        &self,
        args: CheckpointRestoreArgs,
    ) -> Result<(), CliError> {
        let client = self.connect().await?;
        let checkpoint = self.resolve_checkpoint(&client, &args.checkpoint).await?;
        let through = match &args.through {
            Some(target) => Some(self.resolve_checkpoint(&client, target).await?),
            None => None,
        };
        self.restore_files(
            &client,
            Wanted {
                checkpoint,
                through,
                paths: args.paths,
                accept: args.accept_conflicts,
                yes: args.yes,
                verb: "restore",
            },
        )
        .await
    }

    pub(in crate::commands) async fn checkpoint_undo(
        &self,
        args: CheckpointUndoArgs,
    ) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session_id = match &args.session {
            Some(target) => Some(self.checkpoint_session(&client, target).await?),
            None => None,
        };
        let list = client
            .checkpoint_list(CheckpointListRequest {
                session_id,
                limit: Some(LIST_MAX_LIMIT),
                ..CheckpointListRequest::default()
            })
            .await
            .map_err(|e| self.fail(e))?;
        let Some(latest) = list
            .checkpoints
            .iter()
            .find(|c| c.kind == CheckpointKind::Restore && c.state == CheckpointState::Ok)
        else {
            return Err(CliError::Failed(match session_id {
                Some(session) => format!("session {session} has no restore to undo"),
                None => "there is no restore to undo".to_owned(),
            }));
        };
        self.restore_files(
            &client,
            Wanted {
                checkpoint: latest.id,
                through: None,
                paths: Vec::new(),
                accept: args.accept_conflicts,
                yes: args.yes,
                verb: "undo",
            },
        )
        .await
    }

    async fn restore_files(&self, client: &Client, wanted: Wanted) -> Result<(), CliError> {
        let preview = client
            .checkpoint_preview(CheckpointPreviewRequest {
                checkpoint_id: wanted.checkpoint,
                through: wanted.through,
                paths: wanted.paths.clone(),
            })
            .await
            .map_err(|e| self.fail(e))?;
        let accepted = accepted(&preview, &wanted.accept)?;
        let count = changes(&preview, &accepted);
        if self.json {
            if count == 0 || !wanted.yes {
                self.print(&preview, String::new)?;
                return if count == 0 {
                    Ok(())
                } else {
                    Err(CliError::Usage(format!(
                        "nothing was restored: pass --yes to {} (the preview is printed)",
                        wanted.verb
                    )))
                };
            }
        } else {
            println!("{}", preview_text(&preview, &accepted, wanted.verb));
            if count == 0 {
                println!("nothing to {}", wanted.verb);
                return Ok(());
            }
        }
        if !preview.busy.is_empty() {
            let busy: Vec<String> = preview.busy.iter().map(ToString::to_string).collect();
            return Err(CliError::Failed(format!(
                "a turn is running in {} (session {}): interrupt it (`hutan chat interrupt \
                 SESSION`) or let it end, then {} again",
                preview.checkout.display(),
                busy.join(", "),
                wanted.verb
            )));
        }
        if !wanted.yes {
            confirm(count, wanted.verb)?;
        }
        let restored = match client
            .checkpoint_restore(CheckpointRestore {
                checkpoint_id: wanted.checkpoint,
                through: wanted.through,
                paths: wanted.paths,
                accept_conflicts: wanted.accept,
                plan_digest: preview.plan_digest.clone(),
            })
            .await
        {
            Ok(restored) => restored,
            Err(ClientError::Remote(error)) if error.code == ErrorCode::Conflict => {
                let fresh = error
                    .data
                    .clone()
                    .and_then(|data| serde_json::from_value::<CheckpointPreview>(data).ok());
                if let Some(fresh) = fresh
                    && !self.json
                {
                    eprintln!("{}", preview_text(&fresh, &BTreeSet::new(), wanted.verb));
                }
                return Err(CliError::Failed(format!(
                    "{}; run the command again to see the new preview",
                    error.message
                )));
            }
            Err(error) => return Err(self.fail(error)),
        };
        self.print(&restored, || restored_text(&restored, wanted.verb))
    }
}

/// The conflicts of `preview` that `paths` name; a usage error for any other path.
fn accepted<'a>(
    preview: &CheckpointPreview,
    paths: &'a [String],
) -> Result<BTreeSet<&'a str>, CliError> {
    let mut accepted = BTreeSet::new();
    for path in paths {
        match preview.entries.iter().find(|e| e.path == *path) {
            Some(entry) if entry.state.is_conflict() && entry.accept.is_some() => {
                accepted.insert(path.as_str());
            }
            Some(entry) if entry.state.is_conflict() || entry.state == RestoreState::Uncaptured => {
                return Err(CliError::Usage(format!(
                    "`{path}` cannot be restored: {}",
                    entry.reason.as_deref().unwrap_or("it is left alone")
                )));
            }
            Some(_) => {
                return Err(CliError::Usage(format!(
                    "`{path}` is not a conflict: it is restored without --accept-conflicts"
                )));
            }
            None => {
                return Err(CliError::Usage(format!(
                    "`{path}` is not in the preview: the turns did not change it, or it is as it \
                     was"
                )));
            }
        }
    }
    Ok(accepted)
}

/// How many files the restore writes or deletes.
fn changes(preview: &CheckpointPreview, accepted: &BTreeSet<&str>) -> usize {
    preview
        .entries
        .iter()
        .filter(|e| e.action != RestoreAction::Skip || accepted.contains(e.path.as_str()))
        .count()
}

fn files(count: usize) -> String {
    if count == 1 {
        "1 file".to_owned()
    } else {
        format!("{count} files")
    }
}

/// Asks on the terminal; anything but `y` or `yes` is No.
fn confirm(count: usize, verb: &str) -> Result<(), CliError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(CliError::Usage(format!(
            "nothing was restored: stdin is not a terminal; pass --yes to {verb} {}",
            files(count)
        )));
    }
    let mut stderr = std::io::stderr().lock();
    let word = if verb == "undo" {
        "Undo: restore"
    } else {
        "Restore"
    };
    write!(stderr, "{word} {}? [y/N] ", files(count))?;
    stderr.flush()?;
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliError::Failed("nothing was restored".to_owned()))
    }
}

/// The preview: what happens to each file, the conflicts and how to accept them, what cannot be
/// restored, and the honest label.
fn preview_text(preview: &CheckpointPreview, accepted: &BTreeSet<&str>, verb: &str) -> String {
    let checkpoints = match preview.checkpoints.as_slice() {
        [one] => format!("checkpoint {one}"),
        [first, .., last] => format!(
            "checkpoints {first} through {last} ({} in all)",
            preview.checkpoints.len()
        ),
        [] => "no checkpoint".to_owned(),
    };
    let what = if verb == "undo" {
        format!(
            "Undo: files of {} back to before {checkpoints}",
            preview.checkout.display()
        )
    } else {
        format!(
            "Files of {} back to before {checkpoints}",
            preview.checkout.display()
        )
    };
    let mut lines = vec![what, format!("session {}", preview.session_id)];
    if !preview.shared_with.is_empty() {
        let others: Vec<String> = preview
            .shared_with
            .iter()
            .map(ToString::to_string)
            .collect();
        lines.push(format!(
            "shared with {} (what they changed since shows as a conflict)",
            others.join(", ")
        ));
    }
    if !preview.busy.is_empty() {
        let busy: Vec<String> = preview.busy.iter().map(ToString::to_string).collect();
        lines.push(format!(
            "a turn is running in the checkout: {}",
            busy.join(", ")
        ));
    }
    lines.push(String::new());
    if preview.entries.is_empty() {
        lines.push("every file the turns changed is as it was before them".to_owned());
    } else {
        let rows = preview
            .entries
            .iter()
            .map(|entry| {
                let taken = accepted.contains(entry.path.as_str());
                let action = match (entry.action, entry.accept) {
                    (RestoreAction::Skip, Some(accept)) if taken => {
                        format!("{} (accepted)", accept.as_str())
                    }
                    (action, _) => action.as_str().to_owned(),
                };
                let note = match (entry.state, &entry.reason) {
                    (RestoreState::Clean, None) => String::new(),
                    (state, reason) => {
                        let mut note = state.as_str().to_owned();
                        if let Some(reason) = reason {
                            note.push_str(": ");
                            note.push_str(reason);
                        }
                        if state.is_conflict() && entry.accept.is_some() && !taken {
                            note.push_str(&format!(
                                " (--accept-conflicts {} overwrites it)",
                                entry.path
                            ));
                        }
                        note
                    }
                };
                vec![action, entry.path.clone(), note]
            })
            .collect();
        lines.push(table(&["ACTION", "FILE", "NOTE"], rows));
        let conflicts = preview
            .entries
            .iter()
            .filter(|e| e.state.is_conflict() && !accepted.contains(e.path.as_str()))
            .count();
        let left = preview
            .entries
            .iter()
            .filter(|e| e.action == RestoreAction::Skip && !e.state.is_conflict())
            .count();
        let mut summary = vec![format!("{} to {verb}", files(changes(preview, accepted)))];
        if conflicts > 0 {
            summary.push(format!("{conflicts} left alone (conflict)"));
        }
        if left > 0 {
            summary.push(format!("{left} not restorable"));
        }
        lines.push(summary.join(", "));
    }
    lines.push(String::new());
    lines.push(restore_label(
        preview.agent.as_deref(),
        &preview.checkout,
        preview.head_moved.as_ref(),
    ));
    lines.join("\n")
}

/// What was written, deleted and left, and how to undo it.
fn restored_text(restored: &CheckpointRestored, verb: &str) -> String {
    let mut lines = Vec::new();
    for path in &restored.written {
        lines.push(format!("wrote    {path}"));
    }
    for path in &restored.deleted {
        lines.push(format!("deleted  {path}"));
    }
    for entry in &restored.skipped {
        let reason = entry.reason.as_deref().unwrap_or(entry.state.as_str());
        lines.push(format!("left     {} ({reason})", entry.path));
    }
    let done = if verb == "undo" { "undone" } else { "restored" };
    lines.push(format!(
        "{done}: {}; the {verb} is checkpoint {}, undo it with `hutan checkpoint undo {}`",
        files(restored.written.len() + restored.deleted.len()),
        restored.checkpoint.id,
        restored.checkpoint.session_id
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hutan_core::{RestoreEntry, SessionId};

    use super::*;

    fn entry(path: &str, action: RestoreAction, state: RestoreState) -> RestoreEntry {
        RestoreEntry {
            path: path.to_owned(),
            action,
            state,
            reason: (state != RestoreState::Clean).then(|| "why".to_owned()),
            accept: state.is_conflict().then_some(RestoreAction::Write),
        }
    }

    fn preview() -> CheckpointPreview {
        CheckpointPreview {
            plan_digest: "d".repeat(64),
            entries: vec![
                entry("a.txt", RestoreAction::Write, RestoreState::Clean),
                entry("b.txt", RestoreAction::Skip, RestoreState::After),
                entry("big.bin", RestoreAction::Skip, RestoreState::Uncaptured),
                entry("new.txt", RestoreAction::Delete, RestoreState::Clean),
            ],
            session_id: SessionId::new(),
            agent: Some("codex".to_owned()),
            checkout: PathBuf::from("/work/repo"),
            checkpoints: vec![CheckpointId::new()],
            head_moved: None,
            shared_with: vec![],
            busy: vec![],
        }
    }

    #[test]
    fn the_preview_lists_each_file_the_conflicts_and_the_label() {
        let preview = preview();
        let text = preview_text(&preview, &BTreeSet::new(), "restore");
        assert!(
            text.starts_with("Files of /work/repo back to before checkpoint"),
            "{text}"
        );
        assert!(text.contains("write   a.txt"), "{text}");
        assert!(text.contains("delete  new.txt"), "{text}");
        assert!(
            text.contains("after: why (--accept-conflicts b.txt overwrites it)"),
            "{text}"
        );
        assert!(text.contains("uncaptured: why"), "{text}");
        assert!(
            text.contains("2 files to restore, 1 left alone (conflict), 1 not restorable"),
            "{text}"
        );
        assert!(
            text.ends_with(
                "Restores files only. Codex still remembers this turn and may mention or redo the \
                 changes. Git history (commits, branches, stash) and anything outside /work/repo \
                 are not rewound."
            ),
            "{text}"
        );
        let accept = ["b.txt".to_owned()];
        let taken = accepted(&preview, &accept).unwrap();
        assert_eq!(changes(&preview, &taken), 3);
        let text = preview_text(&preview, &taken, "restore");
        assert!(text.contains("write (accepted)  b.txt"), "{text}");
    }

    #[test]
    fn only_conflicts_of_the_preview_can_be_accepted() {
        let preview = preview();
        for (path, says) in [
            ("a.txt", "is not a conflict"),
            ("big.bin", "cannot be restored"),
            ("c.txt", "is not in the preview"),
        ] {
            let error = accepted(&preview, &[path.to_owned()]).unwrap_err();
            assert!(error.to_string().contains(says), "{path}: {error}");
        }
    }
}
