//! `hutan memory ls|add|edit|archive|restore|history|proposals`: Mawas's memory (protocol 0.32,
//! `docs/assistant.md` § Memory). Everything here is the user's own: a client that runs inside
//! an agent's session is refused by the daemon. Nothing is reported as saved without the
//! daemon's receipt.

use std::io::Read as _;
use std::path::Path;

use hutan_client::Client;
use hutan_core::{ApprovalId, WorkspaceId};
use hutan_protocol::memory::{
    MemoryCategory, MemoryEntries, MemoryEntry, MemoryFilesStatus, MemoryFilesStatusRequest,
    MemoryHistory, MemoryImport, MemoryImported, MemoryList, MemoryProposal, MemoryProposalResolve,
    MemoryProposals, MemoryReceipt, MemoryScope, MemoryState, MemoryTransition, MemoryUpdate,
    MemoryWrite, ProposalDecision, ProposalState,
};
use hutan_protocol::memory_files;
use serde::Serialize;

use super::Ctx;
use crate::args::{
    MemoryAddArgs, MemoryCategoryArg, MemoryCommand, MemoryEditArgs, MemoryExportArgs,
    MemoryFilesArgs, MemoryFormatArg, MemoryIdArgs, MemoryImportArgs, MemoryLsArgs,
    MemoryProposalsArgs, MemoryScopeArg,
};
use crate::error::CliError;

/// The shortest id prefix `hutan memory` accepts.
const MIN_PREFIX: usize = 6;
/// Longest text shown in a listing line, in characters.
const SHOWN_CHARS: usize = 90;

impl Ctx {
    pub(super) async fn memory(&self, command: MemoryCommand) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_memory() {
            return Err(CliError::Failed(
                "this hutand has no memory (it is older than protocol 0.32); restart it with \
                 `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        match command {
            MemoryCommand::Ls(args) => self.memory_ls(&client, args).await,
            MemoryCommand::Add(args) => self.memory_add(&client, args).await,
            MemoryCommand::Edit(args) => self.memory_edit(&client, args).await,
            MemoryCommand::Archive(args) => self.memory_transition(&client, args, true).await,
            MemoryCommand::Restore(args) => self.memory_transition(&client, args, false).await,
            MemoryCommand::History(args) => self.memory_history(&client, args).await,
            MemoryCommand::Proposals(args) => self.memory_proposals(&client, args).await,
            MemoryCommand::Export(args) => self.memory_export(&client, args).await,
            MemoryCommand::Import(args) => self.memory_import(&client, args).await,
            MemoryCommand::Files(args) => self.memory_files(&client, args).await,
        }
    }

    /// The scope `--scope` and `--ws` name: global, or one workspace.
    async fn memory_scope(
        &self,
        client: &Client,
        scope: Option<MemoryScopeArg>,
        workspace: Option<String>,
    ) -> Result<(MemoryScope, Option<WorkspaceId>), CliError> {
        match (scope, workspace) {
            (Some(MemoryScopeArg::Global), Some(_)) => Err(CliError::Usage(
                "--ws names a workspace: drop `--scope global`".to_owned(),
            )),
            (Some(MemoryScopeArg::Workspace), None) => Err(CliError::Usage(
                "`--scope workspace` needs --ws ID|NAME".to_owned(),
            )),
            (_, Some(target)) => Ok((
                MemoryScope::Workspace,
                Some(self.resolve_workspace_id(client, &target).await?),
            )),
            (_, None) => Ok((MemoryScope::Global, None)),
        }
    }

    fn require_files(&self, client: &Client) -> Result<(), CliError> {
        if client.supports_memory_files() {
            return Ok(());
        }
        Err(CliError::Failed(
            "this hutand does not keep the memory in files (it is older than protocol 0.46); \
             restart it with `hutan daemon restart`"
                .to_owned(),
        ))
    }

    async fn memory_export(&self, client: &Client, args: MemoryExportArgs) -> Result<(), CliError> {
        let (scope, workspace_id) = self
            .memory_scope(client, args.scope, args.workspace)
            .await?;
        let format =
            args.format
                .unwrap_or_else(|| match args.path.extension().and_then(|e| e.to_str()) {
                    Some(ext) if ext.eq_ignore_ascii_case("json") => MemoryFormatArg::Json,
                    _ => MemoryFormatArg::Md,
                });
        let entries = client
            .memory_list(MemoryList {
                scope: Some(scope),
                workspace_id,
                category: None,
                include_archived: false,
                query: None,
            })
            .await
            .map_err(|e| self.fail(e))?;
        let text = match format {
            MemoryFormatArg::Md => memory_files::render_entries(scope, &entries),
            MemoryFormatArg::Json => memory_files::render_json(scope, &entries),
        };
        let to_stdout = args.path == Path::new("-");
        if to_stdout {
            print!("{text}");
            return Ok(());
        }
        if args.path.exists() && !args.force {
            return Err(CliError::Failed(format!(
                "{} exists: choose another path or add --force to replace it",
                args.path.display()
            )));
        }
        std::fs::write(&args.path, &text).map_err(|e| {
            CliError::Failed(format!("could not write {}: {e}", args.path.display()))
        })?;
        let done = Exported {
            path: args.path.display().to_string(),
            format: match format {
                MemoryFormatArg::Md => "md",
                MemoryFormatArg::Json => "json",
            },
            scope: scope_label(scope, workspace_id),
            entries: entries.len(),
        };
        self.print(&done, || {
            format!(
                "Exported {} entr{} ({}, {}) to {}",
                done.entries,
                if done.entries == 1 { "y" } else { "ies" },
                done.scope,
                done.format,
                done.path
            )
        })
    }

    async fn memory_import(&self, client: &Client, args: MemoryImportArgs) -> Result<(), CliError> {
        self.require_files(client)?;
        let (scope, workspace_id) = self
            .memory_scope(client, args.scope, args.workspace)
            .await?;
        let (path, text) = if args.path == Path::new("-") {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| CliError::Failed(format!("could not read standard input: {e}")))?;
            (None, Some(text))
        } else {
            // The daemon reads the file, so it gets a path it can resolve from anywhere.
            let path = std::path::absolute(&args.path)
                .map_err(|e| CliError::Failed(format!("{}: {e}", args.path.display())))?;
            (Some(path), None)
        };
        let imported = client
            .memory_import(MemoryImport {
                scope,
                workspace_id,
                path,
                text,
                category: args.category.map(category),
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&imported, || imported_text(&imported))
    }

    async fn memory_files(&self, client: &Client, args: MemoryFilesArgs) -> Result<(), CliError> {
        self.require_files(client)?;
        let (scope, workspace_id) = self
            .memory_scope(client, args.scope, args.workspace)
            .await?;
        let status = client
            .memory_files_status(MemoryFilesStatusRequest {
                scope,
                workspace_id,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&status, || status_text(&status))
    }

    async fn memory_workspace(
        &self,
        client: &Client,
        target: Option<String>,
    ) -> Result<Option<WorkspaceId>, CliError> {
        match target {
            Some(target) => Ok(Some(self.resolve_workspace_id(client, &target).await?)),
            None => Ok(None),
        }
    }

    /// The entry `target` names: a full id, or a unique prefix of at least [`MIN_PREFIX`]
    /// characters, among all entries (archived ones too).
    async fn memory_entry(&self, client: &Client, target: &str) -> Result<MemoryEntry, CliError> {
        let entries = client
            .memory_list(MemoryList {
                include_archived: true,
                ..MemoryList::default()
            })
            .await
            .map_err(|e| self.fail(e))?;
        if let Some(entry) = entries.iter().find(|e| e.id == target) {
            return Ok(entry.clone());
        }
        let wanted = target.to_ascii_uppercase();
        let matching: Vec<&MemoryEntry> = entries
            .iter()
            .filter(|e| wanted.len() >= MIN_PREFIX && e.id.starts_with(&wanted))
            .collect();
        match matching.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(CliError::Failed(format!(
                "no memory entry has the id `{target}` (`hutan memory ls --archived` lists them; \
                 an id prefix needs at least {MIN_PREFIX} characters)"
            ))),
            many => Err(CliError::Usage(format!(
                "`{target}` matches {} entries: {}",
                many.len(),
                many.iter()
                    .map(|e| e.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    async fn memory_ls(&self, client: &Client, args: MemoryLsArgs) -> Result<(), CliError> {
        let workspace_id = self.memory_workspace(client, args.workspace).await?;
        let entries = client
            .memory_list(MemoryList {
                scope: None,
                workspace_id,
                category: args.category.map(category),
                include_archived: args.archived,
                query: None,
            })
            .await
            .map_err(|e| self.fail(e))?;
        let list = MemoryEntries { entries };
        self.print(&list, || entry_table(&list.entries))
    }

    async fn memory_add(&self, client: &Client, args: MemoryAddArgs) -> Result<(), CliError> {
        let workspace_id = self.memory_workspace(client, args.workspace).await?;
        let (scope, default) = match workspace_id {
            Some(_) => (MemoryScope::Workspace, MemoryCategory::Note),
            None => (MemoryScope::Global, MemoryCategory::Profile),
        };
        let receipt = client
            .memory_write(MemoryWrite {
                operation_id: operation_id(),
                scope,
                workspace_id,
                category: args.category.map_or(default, category),
                body: args.text,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&receipt, || receipt_line("Saved", &receipt))
    }

    async fn memory_edit(&self, client: &Client, args: MemoryEditArgs) -> Result<(), CliError> {
        let entry = self.memory_entry(client, &args.id).await?;
        let receipt = client
            .memory_update(MemoryUpdate {
                operation_id: operation_id(),
                entry_id: entry.id,
                expected_revision: entry.revision,
                body: args.text,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&receipt, || receipt_line("Saved", &receipt))
    }

    async fn memory_transition(
        &self,
        client: &Client,
        args: MemoryIdArgs,
        archive: bool,
    ) -> Result<(), CliError> {
        let entry = self.memory_entry(client, &args.id).await?;
        let req = MemoryTransition {
            operation_id: operation_id(),
            entry_id: entry.id,
            expected_revision: entry.revision,
        };
        let receipt = if archive {
            client.memory_archive(req).await
        } else {
            client.memory_restore(req).await
        }
        .map_err(|e| self.fail(e))?;
        let verb = if archive { "Archived" } else { "Restored" };
        self.print(&receipt, || receipt_line(verb, &receipt))
    }

    async fn memory_history(&self, client: &Client, args: MemoryIdArgs) -> Result<(), CliError> {
        let entry = self.memory_entry(client, &args.id).await?;
        let history = client
            .memory_history(&entry.id)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&history, || history_text(&history))
    }

    async fn memory_proposals(
        &self,
        client: &Client,
        args: MemoryProposalsArgs,
    ) -> Result<(), CliError> {
        let answer = match (&args.approve, &args.reject) {
            (Some(target), _) => Some((target, ProposalDecision::Approve)),
            (None, Some(target)) => Some((target, ProposalDecision::Reject)),
            (None, None) => None,
        };
        let proposals = client
            .memory_proposals(args.all)
            .await
            .map_err(|e| self.fail(e))?;
        let Some((target, decision)) = answer else {
            let list = MemoryProposals { proposals };
            return self.print(&list, || proposal_table(&list.proposals));
        };
        let proposal = find_proposal(&proposals, target)?;
        let resolved = client
            .memory_resolve(MemoryProposalResolve {
                proposal_id: proposal.id,
                decision,
                edited_body: None,
            })
            .await
            .map_err(|e| self.fail(e))?;
        let conflict = resolved.proposal.state == ProposalState::Conflict;
        self.print(&resolved, || match (&resolved.receipt, decision) {
            (Some(receipt), _) => receipt_line("Approved and saved", receipt),
            (None, ProposalDecision::Reject) => {
                format!(
                    "Rejected proposal {}: nothing was saved",
                    resolved.proposal.id
                )
            }
            (None, _) => format!(
                "Proposal {} could not be applied ({}): the entry changed since it was made, \
                 nothing was saved",
                resolved.proposal.id,
                state_word(resolved.proposal.state)
            ),
        })?;
        if conflict {
            // The proposal is kept, visible, as a conflict; the command did not do what was asked.
            return Err(if self.json {
                CliError::Reported
            } else {
                CliError::Failed(
                    "conflict: the entry changed since the proposal was made; nothing was applied"
                        .to_owned(),
                )
            });
        }
        Ok(())
    }
}

/// What `hutan memory export --json` prints.
#[derive(Debug, Serialize)]
struct Exported {
    path: String,
    format: &'static str,
    scope: String,
    entries: usize,
}

fn imported_text(imported: &MemoryImported) -> String {
    let mut out = format!(
        "Imported as proposals (nothing is saved until you approve them): {} new, {} changes, {} already there",
        imported.created, imported.updated, imported.unchanged
    );
    if imported.created + imported.updated > 0 {
        out.push_str("\nReview them with `hutan memory proposals`, or in the Memory page.");
    }
    for skipped in &imported.skipped {
        out.push_str(&format!(
            "\nSkipped {}{}: {}",
            skipped.file,
            skipped
                .section
                .as_ref()
                .map_or_else(String::new, |s| format!(" ({s})")),
            skipped.reason
        ));
    }
    out
}

fn status_text(status: &MemoryFilesStatus) -> String {
    let mut out = String::new();
    match (&status.folder, &status.note) {
        (Some(folder), _) => out.push_str(&format!("Folder: {}\n", folder.display())),
        (None, Some(note)) => {
            out.push_str(&format!("No files: {note}"));
            return out;
        }
        (None, None) => out.push_str("No files.\n"),
    }
    if let Some(note) = &status.note {
        out.push_str(&format!("{note}\n"));
    }
    for file in &status.files {
        out.push_str(&format!("  {}  {} bytes\n", file.name, file.bytes));
    }
    out.push_str(&format!(
        "In a git repository: {}\n",
        if status.tracked {
            "yes (changes show in Changes and can be committed)"
        } else {
            "no"
        }
    ));
    out.push_str(&format!(
        "Proposals from edited files waiting: {}",
        status.pending
    ));
    for problem in &status.problems {
        out.push_str(&format!(
            "\nProblem in {}{}: {}",
            problem.file,
            problem
                .section
                .as_ref()
                .map_or_else(String::new, |s| format!(" ({s})")),
            problem.reason
        ));
    }
    out
}

fn operation_id() -> String {
    ulid::Ulid::generate().to_string()
}

fn category(arg: MemoryCategoryArg) -> MemoryCategory {
    match arg {
        MemoryCategoryArg::Profile => MemoryCategory::Profile,
        MemoryCategoryArg::Fact => MemoryCategory::Fact,
        MemoryCategoryArg::Note => MemoryCategory::Note,
    }
}

fn find_proposal<'a>(
    proposals: &'a [MemoryProposal],
    target: &str,
) -> Result<&'a MemoryProposal, CliError> {
    let pending = || {
        proposals
            .iter()
            .filter(|p| p.state == ProposalState::Pending)
    };
    if let Ok(id) = target.parse::<ApprovalId>()
        && let Some(found) = pending().find(|p| p.id == id)
    {
        return Ok(found);
    }
    let wanted = target.to_ascii_uppercase();
    let matching: Vec<&MemoryProposal> = pending()
        .filter(|p| wanted.len() >= MIN_PREFIX && p.id.to_string().starts_with(&wanted))
        .collect();
    match matching.as_slice() {
        [one] => Ok(one),
        [] => Err(CliError::Failed(format!(
            "no pending proposal has the id `{target}` (`hutan memory proposals` lists them)"
        ))),
        many => Err(CliError::Usage(format!(
            "`{target}` matches {} proposals: {}",
            many.len(),
            many.iter()
                .map(|p| p.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn receipt_line(verb: &str, receipt: &MemoryReceipt) -> String {
    format!(
        "{verb}: entry {} revision {} (receipt {}, scope revision {})",
        receipt.entry_id, receipt.revision, receipt.operation_id, receipt.scope_revision
    )
}

fn one_line(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= SHOWN_CHARS {
        return line;
    }
    let cut: String = line.chars().take(SHOWN_CHARS - 1).collect();
    format!("{cut}…")
}

fn scope_label(scope: MemoryScope, workspace: Option<WorkspaceId>) -> String {
    match (scope, workspace) {
        (MemoryScope::Workspace, Some(id)) => format!("workspace {id}"),
        (MemoryScope::Workspace, None) => "workspace".to_owned(),
        (MemoryScope::Global, _) => "global".to_owned(),
    }
}

fn state_word(state: ProposalState) -> &'static str {
    state.as_str()
}

fn table(rows: &[Vec<String>]) -> String {
    let columns = rows.first().map_or(0, Vec::len);
    let widths: Vec<usize> = (0..columns)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (at, (cell, width)) in row.iter().zip(&widths).enumerate() {
                if at + 1 == columns {
                    line.push_str(cell);
                } else {
                    line.push_str(&format!("{cell:<width$}  "));
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn entry_table(entries: &[MemoryEntry]) -> String {
    if entries.is_empty() {
        return "no memory entries: add one with `hutan memory add \"…\"`".to_owned();
    }
    let mut rows = vec![vec![
        "ID".to_owned(),
        "CATEGORY".to_owned(),
        "SCOPE".to_owned(),
        "REV".to_owned(),
        "FLAGS".to_owned(),
        "TEXT".to_owned(),
    ]];
    for entry in entries {
        let mut flags = Vec::new();
        if entry.inferred {
            flags.push("inferred");
        }
        if entry.state == MemoryState::Archived {
            flags.push("archived");
        }
        rows.push(vec![
            entry.id.clone(),
            entry.category.as_str().to_owned(),
            scope_label(entry.scope, entry.workspace_id),
            entry.revision.to_string(),
            if flags.is_empty() {
                "-".to_owned()
            } else {
                flags.join(",")
            },
            one_line(&entry.body),
        ]);
    }
    table(&rows)
}

fn history_text(history: &MemoryHistory) -> String {
    let entry = &history.entry;
    let mut out = format!(
        "{} · {} · {} · {}\n",
        entry.id,
        entry.category.as_str(),
        scope_label(entry.scope, entry.workspace_id),
        if entry.state == MemoryState::Archived {
            "archived"
        } else if entry.inferred {
            "active, inferred"
        } else {
            "active"
        }
    );
    let rows: Vec<Vec<String>> = history
        .revisions
        .iter()
        .map(|r| {
            vec![
                format!("r{}", r.revision),
                r.operation.as_str().to_owned(),
                r.provenance.summary(),
                one_line(&r.body),
            ]
        })
        .collect();
    out.push_str(&table(&rows));
    out
}

fn proposal_table(proposals: &[MemoryProposal]) -> String {
    if proposals.is_empty() {
        return "no proposals wait for you".to_owned();
    }
    let mut rows = vec![vec![
        "ID".to_owned(),
        "STATE".to_owned(),
        "PROPOSES".to_owned(),
        "TEXT".to_owned(),
    ]];
    for proposal in proposals {
        let what = format!(
            "{} {} · {}{}",
            proposal.operation.as_str(),
            proposal.category.as_str(),
            scope_label(proposal.scope, proposal.workspace_id),
            proposal
                .entry_id
                .as_ref()
                .map_or_else(String::new, |entry| format!(" · entry {entry}")),
        );
        let mut text = one_line(&proposal.body);
        if let Some(current) = &proposal.current_body {
            text.push_str(&format!("   (now: {})", one_line(current)));
        }
        rows.push(vec![
            proposal.id.to_string(),
            state_word(proposal.state).to_owned(),
            what,
            text,
        ]);
    }
    table(&rows)
}
