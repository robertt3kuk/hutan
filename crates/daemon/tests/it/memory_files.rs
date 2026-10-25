//! Mawas's memory as Markdown files (protocol 0.46, `docs/assistant.md` § Memory files): a real
//! daemon on a temporary `HUTAN_HOME`, the Home folder inside it. The files are a mirror of the
//! memory (SQLite stays the source of truth); an edit of one becomes a proposal and never a
//! change; `memory.import` makes proposals from a file or text.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::{TestDaemon, WAIT, wait_event};

use std::fs;
use std::path::{Path, PathBuf};

use hutan_client::Client;
use hutan_core::WorkspaceId;
use hutan_protocol::ErrorCode;
use hutan_protocol::memory::{
    MemoryAuthor, MemoryCategory, MemoryEntry, MemoryFilesStatus, MemoryFilesStatusRequest,
    MemoryImport, MemoryImported, MemoryList, MemoryProposal, MemoryProposalResolve, MemoryScope,
    MemoryState, MemoryUpdate, MemoryWrite, ORIGIN_FILE, ORIGIN_IMPORT, ProposalDecision,
    ProposalState,
};
use hutan_protocol::memory_files as format;
use hutan_protocol::messages::{Event, Origin, Request, WorkspaceOpen, WorkspaceOpened};
use serde_json::json;
use tokio::sync::broadcast;

const SECRET: &str = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD";

struct F {
    daemon: TestDaemon,
    user: Client,
    events: broadcast::Receiver<(hutan_core::Seq, Event)>,
}

impl F {
    async fn start() -> Self {
        Self::start_with("").await
    }

    async fn start_with(toml: &str) -> Self {
        let daemon = TestDaemon::start_with(toml, |_| {}).await;
        let (user, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            user,
            events,
        }
    }

    /// The global memory files' folder (the Home folder is inside the temporary home).
    fn global(&self) -> PathBuf {
        self.daemon
            .paths
            .default_home_folder
            .join(format::GLOBAL_DIR)
    }

    async fn add(&self, category: MemoryCategory, body: &str) -> MemoryEntry {
        self.add_in(MemoryScope::Global, None, category, body).await
    }

    async fn add_in(
        &self,
        scope: MemoryScope,
        workspace: Option<WorkspaceId>,
        category: MemoryCategory,
        body: &str,
    ) -> MemoryEntry {
        let receipt = self
            .user
            .memory_write(MemoryWrite {
                operation_id: ulid::Ulid::generate().to_string(),
                scope,
                workspace_id: workspace,
                category,
                body: body.to_owned(),
            })
            .await
            .unwrap();
        self.user
            .memory_history(&receipt.entry_id)
            .await
            .unwrap()
            .entry
    }

    async fn entries(&self) -> Vec<MemoryEntry> {
        self.user
            .memory_list(MemoryList {
                include_archived: true,
                ..MemoryList::default()
            })
            .await
            .unwrap()
    }

    async fn status(&self) -> MemoryFilesStatus {
        self.user
            .memory_files_status(MemoryFilesStatusRequest {
                scope: MemoryScope::Global,
                workspace_id: None,
            })
            .await
            .unwrap()
    }

    async fn pending(&self) -> Vec<MemoryProposal> {
        self.user.memory_proposals(false).await.unwrap()
    }

    async fn import_text(&self, text: &str) -> MemoryImported {
        self.user
            .memory_import(MemoryImport {
                scope: MemoryScope::Global,
                workspace_id: None,
                path: None,
                text: Some(text.to_owned()),
                category: None,
            })
            .await
            .unwrap()
    }
}

fn read(folder: &Path, name: &str) -> String {
    fs::read_to_string(folder.join(name)).unwrap()
}

fn leftovers(folder: &Path) -> Vec<String> {
    fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".hutan-write-"))
        .collect()
}

/// The text a scope's file should have for `entries` of one category.
fn expected(category: MemoryCategory, entries: &[&MemoryEntry]) -> String {
    let items: Vec<_> = entries
        .iter()
        .map(|e| format::Item {
            id: Some(&e.id),
            revision: e.revision,
            body: &e.body,
        })
        .collect();
    format::render_file(MemoryScope::Global, category, &items)
}

#[tokio::test]
async fn entries_are_mirrored_byte_for_byte_and_regenerated_after_every_change() {
    let f = F::start().await;
    let folder = f.global();
    assert!(!folder.exists(), "no files for an empty memory");

    let profile = f
        .add(MemoryCategory::Profile, "Prefers short answers.")
        .await;
    let fact = f
        .add(MemoryCategory::Fact, "Uses jj.\n\nSecond paragraph.")
        .await;
    let other = f
        .add(MemoryCategory::Fact, "Staging is stage.example.com.")
        .await;

    assert_eq!(
        read(&folder, "profile.md"),
        expected(MemoryCategory::Profile, &[&profile])
    );
    assert_eq!(
        read(&folder, "facts.md"),
        expected(MemoryCategory::Fact, &[&fact, &other]),
        "creation order, with the id comments"
    );
    assert_eq!(
        read(&folder, "notes.md"),
        expected(MemoryCategory::Note, &[]),
        "a file for every category once there is any"
    );
    assert!(
        read(&folder, "facts.md").starts_with("<!-- Written by Hutan"),
        "the first line says who writes the files"
    );
    assert!(leftovers(&folder).is_empty(), "no temporary file is left");

    // An update is a new revision in the file, before the receipt is answered.
    f.user
        .memory_update(MemoryUpdate {
            operation_id: ulid::Ulid::generate().to_string(),
            entry_id: fact.id.clone(),
            expected_revision: fact.revision,
            body: "Uses jj for version control.".into(),
        })
        .await
        .unwrap();
    let updated = f.user.memory_history(&fact.id).await.unwrap().entry;
    assert_eq!(updated.revision, 2);
    assert_eq!(
        read(&folder, "facts.md"),
        expected(MemoryCategory::Fact, &[&updated, &other])
    );

    // Byte-stable: asking again changes nothing, not even the file's modification time.
    let before = fs::metadata(folder.join("facts.md"))
        .unwrap()
        .modified()
        .unwrap();
    let status = f.status().await;
    assert!(status.problems.is_empty() && status.pending == 0);
    assert_eq!(
        fs::metadata(folder.join("facts.md"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );

    // Archiving takes the section out.
    f.user
        .memory_archive(hutan_protocol::memory::MemoryTransition {
            operation_id: ulid::Ulid::generate().to_string(),
            entry_id: other.id.clone(),
            expected_revision: other.revision,
        })
        .await
        .unwrap();
    assert_eq!(
        read(&folder, "facts.md"),
        expected(MemoryCategory::Fact, &[&updated])
    );
}

#[tokio::test]
async fn status_says_where_the_files_are_and_whether_the_folder_is_tracked() {
    let f = F::start().await;
    let nothing = f.status().await;
    assert!(nothing.enabled && nothing.files.is_empty() && nothing.note.is_some());

    f.add(MemoryCategory::Profile, "Prefers short answers.")
        .await;
    let status = f.status().await;
    assert!(status.enabled);
    assert_eq!(
        status.folder.as_deref().map(|p| p.canonicalize().unwrap()),
        Some(f.global().canonicalize().unwrap())
    );
    let names: Vec<_> = status.files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, ["profile.md", "facts.md", "notes.md"]);
    assert!(status.last_write_ms.is_some());
    assert!(!status.tracked);

    // After "Track changes" the Home folder is a repository: the mirror shows in Changes.
    let home = f.daemon.paths.default_home_folder.canonicalize().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&home)
        .status()
        .unwrap();
    assert!(f.status().await.tracked);
}

#[tokio::test]
async fn an_edited_file_becomes_proposals_and_nothing_is_applied() {
    let f = F::start().await;
    let folder = f.global();
    let keep = f.add(MemoryCategory::Fact, "Kept as it is.").await;
    let edit = f.add(MemoryCategory::Fact, "Uses jj.").await;
    let drop = f
        .add(MemoryCategory::Fact, "Staging is stage.example.com.")
        .await;

    let facts = read(&folder, "facts.md");
    let edited = facts.replace("Uses jj.", "Uses jj, never git.").replace(
        &format!(
            "<!-- mawas:id={} rev=1 -->\nStaging is stage.example.com.\n",
            drop.id
        ),
        "",
    ) + "\n<!-- mawas:new -->\nThe build is `just check`.\n";
    fs::write(folder.join("facts.md"), &edited).unwrap();

    let status = f.status().await;
    assert!(status.problems.is_empty(), "{:?}", status.problems);
    assert_eq!(status.pending, 3);
    let pending = f.pending().await;
    assert_eq!(pending.len(), 3);
    for proposal in &pending {
        assert_eq!(proposal.state, ProposalState::Pending);
        assert_eq!(proposal.provenance.origin.as_deref(), Some(ORIGIN_FILE));
        assert_eq!(proposal.provenance.author, MemoryAuthor::User);
        assert!(proposal.provenance.session_id.is_none());
    }
    use hutan_protocol::memory::MemoryOp::{Archive, Create, Update};
    let by_op = |op| pending.iter().find(|p| p.operation == op).unwrap();
    assert_eq!(by_op(Update).entry_id.as_deref(), Some(edit.id.as_str()));
    assert_eq!(by_op(Update).body, "Uses jj, never git.");
    assert_eq!(by_op(Update).expected_revision, Some(1));
    assert_eq!(by_op(Archive).entry_id.as_deref(), Some(drop.id.as_str()));
    assert_eq!(by_op(Create).body, "The build is `just check`.");
    assert_eq!(by_op(Create).category, MemoryCategory::Fact);

    // Never applied without approval.
    let entries = f.entries().await;
    assert_eq!(entries.len(), 3);
    assert!(
        entries
            .iter()
            .all(|e| e.state == MemoryState::Active && e.revision == 1)
    );
    assert_eq!(
        entries.iter().find(|e| e.id == edit.id).unwrap().body,
        "Uses jj."
    );

    // Until they are answered the file shows the user's text, plainly and without a loop:
    // reading again makes no new proposals.
    let shown = read(&folder, "facts.md");
    assert!(shown.contains("Uses jj, never git."));
    assert!(!shown.contains("stage.example.com"));
    assert!(shown.contains("<!-- mawas:new -->\nThe build is `just check`."));
    assert!(shown.contains(&format!("id={} rev=1", keep.id)));
    f.status().await;
    assert_eq!(f.pending().await.len(), 3);

    // Approving applies exactly the reviewed text, as the user's own words.
    let update = by_op(Update).clone();
    let resolved = f
        .user
        .memory_resolve(MemoryProposalResolve {
            proposal_id: update.id,
            decision: ProposalDecision::Approve,
            edited_body: None,
        })
        .await
        .unwrap();
    assert_eq!(resolved.proposal.state, ProposalState::Applied);
    let applied = f.user.memory_history(&edit.id).await.unwrap().entry;
    assert_eq!(applied.body, "Uses jj, never git.");
    assert_eq!(applied.revision, 2);
    assert!(!applied.inferred, "the user wrote it");
    assert_eq!(applied.provenance.origin.as_deref(), Some(ORIGIN_FILE));
    assert_eq!(applied.provenance.client.as_deref(), Some("cli"));
    assert!(
        read(&folder, "facts.md").contains(&format!("id={} rev=2", edit.id)),
        "the file shows the new revision"
    );

    // Rejecting puts the file back to what the memory says.
    let archive = by_op(Archive).clone();
    f.user
        .memory_resolve(MemoryProposalResolve {
            proposal_id: archive.id,
            decision: ProposalDecision::Reject,
            edited_body: None,
        })
        .await
        .unwrap();
    assert!(read(&folder, "facts.md").contains("stage.example.com"));
    assert_eq!(
        f.entries()
            .await
            .iter()
            .find(|e| e.id == drop.id)
            .unwrap()
            .state,
        MemoryState::Active
    );

    // Approving a new section makes the entry, with an id the file then shows.
    let create = by_op(Create).clone();
    f.user
        .memory_resolve(MemoryProposalResolve {
            proposal_id: create.id,
            decision: ProposalDecision::Approve,
            edited_body: None,
        })
        .await
        .unwrap();
    let made = f
        .entries()
        .await
        .into_iter()
        .find(|e| e.body == "The build is `just check`.")
        .unwrap();
    assert!(read(&folder, "facts.md").contains(&format!("id={} rev=1 -->\nThe build", made.id)));
    assert!(f.pending().await.is_empty());
}

#[tokio::test]
async fn deleting_a_section_proposes_archiving_it_and_a_missing_file_is_just_written_again() {
    let f = F::start().await;
    let folder = f.global();
    let fact = f.add(MemoryCategory::Fact, "Uses jj.").await;
    let note = f.add(MemoryCategory::Note, "Decision: monorepo.").await;

    // A file that is gone is no deletion of what it held.
    fs::remove_file(folder.join("facts.md")).unwrap();
    let status = f.status().await;
    assert_eq!(status.pending, 0);
    assert!(folder.join("facts.md").is_file(), "written again");
    assert_eq!(
        read(&folder, "facts.md"),
        expected(MemoryCategory::Fact, &[&fact])
    );

    // A section taken out of a file that is there is.
    fs::write(
        folder.join("notes.md"),
        format::render_file(MemoryScope::Global, MemoryCategory::Note, &[]),
    )
    .unwrap();
    assert_eq!(f.status().await.pending, 1);
    let pending = f.pending().await;
    assert_eq!(
        pending[0].operation,
        hutan_protocol::memory::MemoryOp::Archive
    );
    assert_eq!(pending[0].entry_id.as_deref(), Some(note.id.as_str()));
    // Putting it back withdraws the proposal.
    fs::write(
        folder.join("notes.md"),
        expected(MemoryCategory::Note, &[&note]),
    )
    .unwrap();
    assert_eq!(f.status().await.pending, 0);
    assert!(f.pending().await.is_empty());
    let all = f.user.memory_proposals(true).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].state, ProposalState::Cancelled);
}

#[tokio::test]
async fn an_edit_that_cannot_be_used_is_a_problem_not_a_proposal_and_the_file_stays_as_written() {
    let f = F::start().await;
    let folder = f.global();
    let fact = f.add(MemoryCategory::Fact, "Uses jj.").await;
    let other = f.add(MemoryCategory::Fact, "Another.").await;

    let facts = read(&folder, "facts.md");
    let with_secret = facts.replace("Uses jj.", &format!("Uses jj. The token is {SECRET}"));
    fs::write(folder.join("facts.md"), &with_secret).unwrap();

    let status = f.status().await;
    assert!(f.pending().await.is_empty(), "a secret is refused as ever");
    assert_eq!(status.problems.len(), 1);
    let problem = &status.problems[0];
    assert_eq!(problem.file, "facts.md");
    assert_eq!(problem.section.as_deref(), Some(fact.id.as_str()));
    assert!(
        problem.reason.contains("not a secret store"),
        "{}",
        problem.reason
    );
    assert!(
        !problem.reason.contains(SECRET),
        "the text is never repeated"
    );
    assert_eq!(
        read(&folder, "facts.md"),
        with_secret,
        "the user's text stays until it is fixed"
    );
    // Nothing is archived on the strength of a file that has a problem, nor is the secret
    // stored anywhere.
    assert_eq!(f.entries().await.len(), 2);

    // Too long and an unreadable marker are named too.
    let long = "x".repeat(2 * 1024 + 1);
    fs::write(
        folder.join("facts.md"),
        format!(
            "# Facts\n\n<!-- mawas:id={} rev=1 -->\n{long}\n\n<!-- mawas:id={} rev=1 -->\nA section that is fine.\n\n<!-- mawas:bogus -->\nlost\n",
            fact.id, other.id
        ),
    )
    .unwrap();
    let problems = f.status().await.problems;
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems.iter().any(|p| p.reason.contains("2 KiB")));
    assert!(problems.iter().any(|p| p.reason.contains("bogus")));
    // The sections that read cleanly are proposed all the same; the broken ones are not, and
    // nothing is archived because of a file that has a problem.
    let pending = f.pending().await;
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].body, "A section that is fine.");
    assert_eq!(pending[0].entry_id.as_deref(), Some(other.id.as_str()));

    // Fixing the file clears the problems; the file is the mirror again.
    fs::write(folder.join("facts.md"), &facts).unwrap();
    let status = f.status().await;
    assert!(status.problems.is_empty() && status.pending == 0);
    assert_eq!(read(&folder, "facts.md"), facts);
}

#[tokio::test]
async fn the_watcher_turns_an_edit_into_a_proposal_without_being_asked() {
    let mut f = F::start().await;
    let folder = f.global();
    f.add(MemoryCategory::Profile, "Prefers short answers.")
        .await;
    // The write above announced itself; what follows must be the edit's own event.
    while f.events.try_recv().is_ok() {}
    let edited = read(&folder, "profile.md").replace("short", "very short");
    fs::write(folder.join("profile.md"), edited).unwrap();
    wait_event(&mut f.events, "a memory.changed for the edit", |event| {
        matches!(event, Event::MemoryChanged { .. }).then_some(())
    })
    .await;
    let pending = f.pending().await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].body, "Prefers very short answers.");
    let _ = WAIT;
}

#[tokio::test]
async fn a_file_from_a_teammates_repository_proposes_new_entries_and_a_copy_of_ours_proposes_nothing()
 {
    let f = F::start().await;
    let folder = f.global();
    let mine = f.add(MemoryCategory::Fact, "Mine.").await;
    let text = read(&folder, "facts.md")
        + "\n<!-- mawas:id=01TEAMMATE0000000000000000 rev=4 -->\nThe teammate's fact.\n\n<!-- mawas:id=01TEAMMATE0000000000000001 rev=1 -->\nMine.\n";
    fs::write(folder.join("facts.md"), text).unwrap();
    f.status().await;
    let pending = f.pending().await;
    assert_eq!(
        pending.len(),
        1,
        "an entry we already have is not proposed twice"
    );
    assert_eq!(pending[0].body, "The teammate's fact.");
    assert_eq!(
        pending[0].operation,
        hutan_protocol::memory::MemoryOp::Create
    );
    assert!(f.entries().await.iter().all(|e| e.id == mine.id));
}

#[tokio::test]
async fn a_restart_leaves_the_mirror_alone_and_reads_an_edit_made_meanwhile() {
    let f = F::start().await;
    let folder = f.global();
    let entry = f
        .add(MemoryCategory::Profile, "Prefers short answers.")
        .await;
    let before = read(&folder, "profile.md");
    let edited = before.replace("short", "terse");
    let F { daemon, user, .. } = f;
    drop(user);

    let daemon = daemon.restart().await;
    let (user, _events) = daemon.connect_subscribed().await;
    let status = user
        .memory_files_status(MemoryFilesStatusRequest {
            scope: MemoryScope::Global,
            workspace_id: None,
        })
        .await
        .unwrap();
    assert_eq!(status.pending, 0);
    assert_eq!(
        read(&folder, "profile.md"),
        before,
        "not rewritten into something else"
    );
    drop(user);

    // Edited while no daemon runs: the new daemon reads it at start.
    let daemon = daemon
        .restart_with({
            let (folder, edited) = (folder.clone(), edited.clone());
            move |_| fs::write(folder.join("profile.md"), &edited).unwrap()
        })
        .await;
    let (user, _events) = daemon.connect_subscribed().await;
    let status = user
        .memory_files_status(MemoryFilesStatusRequest {
            scope: MemoryScope::Global,
            workspace_id: None,
        })
        .await
        .unwrap();
    assert_eq!(status.pending, 1);
    let pending = user.memory_proposals(false).await.unwrap();
    assert_eq!(pending[0].entry_id.as_deref(), Some(entry.id.as_str()));
    assert_eq!(pending[0].body, "Prefers terse answers.");
    assert_eq!(read(&folder, "profile.md"), edited);
}

// Export / import -------------------------------------------------------------------------------

#[tokio::test]
async fn an_import_makes_pending_proposals_and_never_applies_anything() {
    let f = F::start().await;
    let known = f.add(MemoryCategory::Fact, "Uses jj.").await;
    let text = format!(
        "# Facts\n\n<!-- mawas:id={id} rev=1 -->\nUses jj, never git.\n\n<!-- mawas:new -->\nThe build is `just check`.\n\n<!-- mawas:new -->\nUses jj.\n\n# Profile\n\n<!-- mawas:new -->\nPrefers short answers.\n\n<!-- mawas:new -->\nThe key is {SECRET}\n",
        id = known.id
    );
    let imported = f.import_text(&text).await;
    assert_eq!(
        (imported.created, imported.updated, imported.unchanged),
        (2, 1, 1),
        "{imported:?}"
    );
    assert_eq!(imported.proposals.len(), 3);
    assert_eq!(imported.skipped.len(), 1);
    assert!(imported.skipped[0].reason.contains("not a secret store"));
    assert!(!imported.skipped[0].reason.contains(SECRET));

    let pending = f.pending().await;
    assert_eq!(pending.len(), 3);
    for proposal in &pending {
        assert_eq!(proposal.provenance.origin.as_deref(), Some(ORIGIN_IMPORT));
        assert_eq!(proposal.provenance.author, MemoryAuthor::User);
    }
    assert_eq!(f.entries().await.len(), 1, "nothing applied");
    assert_eq!(f.entries().await[0].body, "Uses jj.");
    // Importing the same text again adds nothing.
    let again = f.import_text(&text).await;
    assert_eq!((again.created, again.updated), (0, 0));
    assert_eq!(f.pending().await.len(), 3);
}

#[tokio::test]
async fn export_in_either_format_imports_back_into_an_empty_memory() {
    let source = F::start().await;
    source
        .add(MemoryCategory::Profile, "Prefers short answers.")
        .await;
    source
        .add(MemoryCategory::Fact, "Uses jj.\n\nSecond paragraph.")
        .await;
    source
        .add(MemoryCategory::Note, "Decision: monorepo.")
        .await;
    let entries = source.entries().await;

    let markdown = format::render_entries(MemoryScope::Global, &entries);
    let json = format::render_json(MemoryScope::Global, &entries);

    for (name, text) in [("md", markdown), ("json", json)] {
        let target = F::start().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("export.{name}"));
        fs::write(&path, text).unwrap();
        let imported = target
            .user
            .memory_import(MemoryImport {
                scope: MemoryScope::Global,
                workspace_id: None,
                path: Some(path),
                text: None,
                category: None,
            })
            .await
            .unwrap();
        assert_eq!(imported.created, 3, "{name}: {imported:?}");
        assert!(
            imported.skipped.is_empty(),
            "{name}: {:?}",
            imported.skipped
        );
        for proposal in target.pending().await {
            target
                .user
                .memory_resolve(MemoryProposalResolve {
                    proposal_id: proposal.id,
                    decision: ProposalDecision::Approve,
                    edited_body: None,
                })
                .await
                .unwrap();
        }
        let mut got: Vec<_> = target
            .entries()
            .await
            .into_iter()
            .map(|e| (e.category, e.body))
            .collect();
        let mut want: Vec<_> = entries
            .iter()
            .map(|e| (e.category, e.body.clone()))
            .collect();
        got.sort();
        want.sort();
        assert_eq!(got, want, "{name}");
        // And the new daemon's own mirror matches what was exported.
        assert!(read(&target.global(), "facts.md").contains("Second paragraph."));
    }
}

#[tokio::test]
async fn import_refuses_what_it_cannot_read_and_agents() {
    let f = F::start().await;
    let both = f
        .user
        .memory_import(MemoryImport {
            scope: MemoryScope::Global,
            workspace_id: None,
            path: Some("/nonexistent".into()),
            text: Some("x".into()),
            category: None,
        })
        .await;
    assert_eq!(crate::common::remote_code(both), ErrorCode::BadRequest);
    let missing = f
        .user
        .memory_import(MemoryImport {
            scope: MemoryScope::Global,
            workspace_id: None,
            path: Some("/nonexistent/memory.md".into()),
            text: None,
            category: None,
        })
        .await;
    assert_eq!(crate::common::remote_code(missing), ErrorCode::BadRequest);
    let broken = f
        .user
        .memory_import(MemoryImport {
            scope: MemoryScope::Global,
            workspace_id: None,
            path: None,
            text: Some("{ \"entries\": ".into()),
            category: None,
        })
        .await;
    assert_eq!(crate::common::remote_code(broken), ErrorCode::BadRequest);
    // Without a heading a section has no category.
    let none = f.import_text("<!-- mawas:new -->\nNo heading.\n").await;
    assert_eq!((none.created, none.skipped.len()), (0, 1));
    // With the `category` of the request it does.
    let some = f
        .user
        .memory_import(MemoryImport {
            scope: MemoryScope::Global,
            workspace_id: None,
            path: None,
            text: Some("<!-- mawas:new -->\nNo heading.\n".into()),
            category: Some(MemoryCategory::Note),
        })
        .await
        .unwrap();
    assert_eq!(some.created, 1);

    // Agents may not import or ask where the files are.
    let session = crate::common::create(&f.user, crate::common::sh("sleep 60"))
        .await
        .id;
    let applied: hutan_protocol::messages::AgentApplied = f
        .user
        .call(Request::AgentReport(
            hutan_protocol::messages::AgentReportRequest {
                session_id: session,
                source: "test-hooks".into(),
                agent: "fake".into(),
                state: Some(hutan_protocol::messages::AgentState::Working),
                message: None,
                seq: 1,
                agent_session_id: None,
                resume_argv: None,
                pid: None,
                node: None,
            },
        ))
        .await
        .unwrap();
    assert!(applied.applied);
    let mut options = f.daemon.options();
    options.client.origin = Some(Origin {
        session_id: session,
    });
    let agent = Client::connect(options).await.unwrap();
    let denied = agent
        .call::<MemoryImported>(Request::MemoryImport(MemoryImport {
            scope: MemoryScope::Global,
            workspace_id: None,
            path: None,
            text: Some("<!-- mawas:new -->\nx\n".into()),
            category: Some(MemoryCategory::Note),
        }))
        .await;
    assert_eq!(crate::common::remote_code(denied), ErrorCode::Forbidden);
    let denied = agent
        .memory_files_status(MemoryFilesStatusRequest {
            scope: MemoryScope::Global,
            workspace_id: None,
        })
        .await;
    assert_eq!(crate::common::remote_code(denied), ErrorCode::Forbidden);
}

// A workspace's files ---------------------------------------------------------------------------

async fn project(f: &F) -> (WorkspaceId, PathBuf) {
    let root = f.daemon._dir.path().canonicalize().unwrap().join("proj");
    fs::create_dir_all(&root).unwrap();
    let opened: WorkspaceOpened = f
        .user
        .call(Request::WorkspaceOpen(WorkspaceOpen {
            path: root.clone(),
            created_by: None,
        }))
        .await
        .unwrap();
    (opened.workspace.id, root)
}

#[tokio::test]
async fn a_workspaces_memory_stays_out_of_its_project_until_the_user_turns_that_on() {
    let f = F::start().await;
    let (ws, root) = project(&f).await;
    f.add_in(
        MemoryScope::Workspace,
        Some(ws),
        MemoryCategory::Note,
        "Decision: monorepo.",
    )
    .await;
    assert!(
        !root.join(".hutan").exists(),
        "off by default: it is a file in their repo"
    );
    let status = f
        .user
        .memory_files_status(MemoryFilesStatusRequest {
            scope: MemoryScope::Workspace,
            workspace_id: Some(ws),
        })
        .await
        .unwrap();
    assert!(!status.enabled && status.folder.is_none());
    assert!(status.note.unwrap().contains("memory_files_in_project"));

    f.user
        .daemon_setting_set("assistant.memory_files_in_project", Some(json!(true)))
        .await
        .unwrap();
    let folder = root.join(format::PROJECT_DIR);
    // Turning it on writes the files (in the background); asking waits for the same work.
    let status = loop {
        let status = f
            .user
            .memory_files_status(MemoryFilesStatusRequest {
                scope: MemoryScope::Workspace,
                workspace_id: Some(ws),
            })
            .await
            .unwrap();
        if status.enabled {
            break status;
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(status.folder.as_deref(), Some(folder.as_path()));
    let notes = read(&folder, "notes.md");
    assert!(notes.contains("Decision: monorepo."));
    assert!(notes.starts_with("<!-- Written by Hutan: Mawas memory (this workspace)"));
    // The global memory is unaffected.
    assert!(f.status().await.enabled);
}

#[tokio::test]
async fn the_setting_is_on_in_daemon_toml_too() {
    let f = F::start_with("[assistant]\nmemory_files_in_project = true\n").await;
    let (ws, root) = project(&f).await;
    f.add_in(
        MemoryScope::Workspace,
        Some(ws),
        MemoryCategory::Note,
        "Decision: monorepo.",
    )
    .await;
    assert!(root.join(format::PROJECT_DIR).join("notes.md").is_file());
}

// Versions --------------------------------------------------------------------------------------

/// Runs git in `dir` with the daemon test's identity; returns stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[tokio::test]
async fn in_a_tracked_home_folder_the_mirror_is_a_change_git_commits_and_diffs() {
    let config = tempfile::tempdir().unwrap();
    let gitconfig = config.path().join("gitconfig");
    fs::write(
        &gitconfig,
        "[user]\n\tname = Test\n\temail = test@example.com\n[commit]\n\tgpgsign = false\n",
    )
    .unwrap();
    let daemon = TestDaemon::start_with("", |c| {
        c.checkpoints.git_env.extend([
            (
                "GIT_CONFIG_GLOBAL".to_owned(),
                gitconfig.display().to_string(),
            ),
            ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
        ]);
    })
    .await;
    let user = daemon.connect().await;
    let home = user
        .call::<hutan_protocol::messages::WorkspaceList>(Request::WorkspaceList(
            hutan_protocol::messages::Empty {},
        ))
        .await
        .unwrap()
        .workspaces
        .into_iter()
        .find(|w| w.kind == hutan_protocol::messages::WorkspaceKind::Home)
        .unwrap();
    let folder = home.roots[0].clone();
    user.project_git_init(home.id).await.unwrap();
    assert_eq!(git(&folder, &["status", "--porcelain"]), "");

    let write = |body: &str| {
        user.memory_write(MemoryWrite {
            operation_id: ulid::Ulid::generate().to_string(),
            scope: MemoryScope::Global,
            workspace_id: None,
            category: MemoryCategory::Fact,
            body: body.to_owned(),
        })
    };
    let first = write("Uses jj.").await.unwrap();
    // The mirror is new files of the repository: Changes lists them, git commits them.
    let status = git(&folder, &["status", "--porcelain", "--untracked-files=all"]);
    assert!(status.contains(".mawas/memory/facts.md"), "{status}");
    let listed = user.worktree_list(Some(home.id)).await.unwrap();
    assert!(listed[0].dirty, "Changes sees the mirror");
    git(&folder, &["add", "-A"]);
    git(&folder, &["commit", "-q", "-m", "memory"]);
    assert_eq!(git(&folder, &["status", "--porcelain"]), "");

    // A change of an entry is a diff of one line, in the file git tracks.
    user.memory_update(MemoryUpdate {
        operation_id: ulid::Ulid::generate().to_string(),
        entry_id: first.entry_id.clone(),
        expected_revision: 1,
        body: "Uses jj, never git.".into(),
    })
    .await
    .unwrap();
    let diff = git(&folder, &["diff", "--", ".mawas/memory/facts.md"]);
    assert!(
        diff.contains("-Uses jj.") && diff.contains("+Uses jj, never git."),
        "{diff}"
    );
    assert!(diff.contains("rev=1") && diff.contains("rev=2"), "{diff}");
    // Restoring the committed text with git is an edit: a proposal, never silently the memory.
    git(&folder, &["checkout", "--", ".mawas/memory/facts.md"]);
    let status = user
        .memory_files_status(MemoryFilesStatusRequest {
            scope: MemoryScope::Global,
            workspace_id: None,
        })
        .await
        .unwrap();
    assert!(status.tracked);
    let pending = user.memory_proposals(false).await.unwrap();
    assert_eq!(
        pending.len(),
        1,
        "the checked-out older text is an edit to review"
    );
    assert_eq!(pending[0].body, "Uses jj.");
    assert_eq!(
        user.memory_history(&first.entry_id)
            .await
            .unwrap()
            .entry
            .body,
        "Uses jj, never git.",
        "the memory did not move"
    );
    daemon.stop().await;
}
