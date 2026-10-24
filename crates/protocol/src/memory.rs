//! Mawas memory (protocol 0.32, capability `memory`; ADR 0017 decisions 3 and 4,
//! `docs/assistant.md` § Memory).
//!
//! Curated, revisioned knowledge the user approved: **profile** entries (about the user),
//! **facts** (environments, projects) and **notes** (decisions of one workspace), each in the
//! global scope or in one workspace. Nothing a model writes is saved without the user's
//! approval and a daemon receipt:
//!
//! - `memory.write`, `.update`, `.archive` and `.restore` are the user's: an agent-originated
//!   connection (Mawas included) is refused with `forbidden` (`user_only`). Each answers with a
//!   [`MemoryReceipt`], written in the same transaction as the change.
//! - `memory.propose` is what an agent may do. It answers [`MemoryProposed`] with
//!   `status: "proposed"` and `applied: false` and raises an approval the user answers with
//!   *Approve*, *Edit & approve* or *Reject* (`memory.proposals.resolve`, or `approval.respond`
//!   with `approve` / `reject`). Approval applies the exact reviewed payload against
//!   `expected_revision`; a stale revision leaves the proposal in state `conflict`, visible.
//! - Every change carries daemon-stamped provenance ([`MemoryProvenance`]); the model cannot
//!   claim a source. Entries a model wrote are `inferred` until the user edits or confirms
//!   them (an `update` with the same body).
//! - A body is at most [`BODY_MAX_BYTES`] and may not look like a credential ("Memory is not a
//!   secret store").
//!
//! `operation_id` (any text up to [`OPERATION_ID_MAX_BYTES`], a UUID or ULID by convention) makes
//! a request idempotent: repeating it returns the original receipt.

use hutan_core::{ApprovalId, SessionId, TurnId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// Longest body of an entry or proposal, in bytes.
pub const BODY_MAX_BYTES: usize = 2 * 1024;
/// Longest `operation_id`, in bytes.
pub const OPERATION_ID_MAX_BYTES: usize = 128;
/// What the title of a memory proposal's approval starts with: clients that show approvals (the
/// desktop's Today) tell a memory proposal from another request by it and offer *Edit & approve*.
pub const PROPOSAL_TITLE_PREFIX: &str = "Memory proposal:";
/// The option of a memory approval that approves the proposal as it is (`approval.respond`).
pub const OPTION_APPROVE: &str = "approve";
/// The option of a memory approval that rejects the proposal.
pub const OPTION_REJECT: &str = "reject";

/// [`MemoryProvenance::origin`] of a proposal the daemon made from an edit of a memory file.
pub const ORIGIN_FILE: &str = "file";
/// [`MemoryProvenance::origin`] of a proposal `memory.import` made.
pub const ORIGIN_IMPORT: &str = "import";
/// Largest text `memory.import` reads, in bytes.
pub const IMPORT_MAX_BYTES: usize = 1024 * 1024;
/// Most sections one `memory.import` turns into proposals.
pub const IMPORT_MAX_SECTIONS: usize = 500;

/// Where an entry lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    /// This daemon's cross-workspace knowledge.
    Global,
    /// One workspace's (`workspace_id` names it).
    Workspace,
}

/// What an entry is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    /// Communication preferences and user-approved personal facts.
    Profile,
    /// Stable facts about an environment or a project.
    Fact,
    /// A decision or working note of one workspace.
    Note,
}

impl MemoryCategory {
    pub const ALL: [Self; 3] = [Self::Profile, Self::Fact, Self::Note];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Profile => "profile",
            Self::Fact => "fact",
            Self::Note => "note",
        }
    }
}

/// Whether an entry is used (`active`) or kept only as history (`archived`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryState {
    Active,
    Archived,
}

/// Who wrote a revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAuthor {
    /// The user, directly or by editing a proposal.
    User,
    /// A model, through a proposal the user approved as it was.
    Agent,
}

/// What a revision or a proposal does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOp {
    Create,
    Update,
    Archive,
    Restore,
}

impl MemoryOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Archive => "archive",
            Self::Restore => "restore",
        }
    }
}

/// Where a proposal stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    Pending,
    Applied,
    Rejected,
    /// Approved, but the entry had moved on since: nothing was applied.
    Conflict,
    /// The entry or scope it was about is gone.
    Cancelled,
}

impl ProposalState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
            Self::Conflict => "conflict",
            Self::Cancelled => "cancelled",
        }
    }
}

/// How the user answers a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalDecision {
    /// Apply the proposal as reviewed.
    Approve,
    /// Apply it with `edited_body` instead (the user then is the author).
    Edit,
    Reject,
}

/// Where a revision or a proposal came from, stamped by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProvenance {
    /// Who wrote it: the user's own client, or an agent.
    pub author: MemoryAuthor,
    /// The client that sent the request (`desktop`, `cli`, ...): the user's write, or the
    /// user's approval of a proposal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// The chat (or agent session) a proposal came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// The turn that session was in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub at_ms: u64,
    /// Protocol 0.46: where a change the user made outside the Memory page came from —
    /// [`ORIGIN_FILE`] (an edit of a memory file) or [`ORIGIN_IMPORT`] (`memory.import`). Such a
    /// change is a proposal the user approves, so `client` is the client that approved it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl MemoryProvenance {
    /// One plain line: "entered by you in the desktop", "proposed by an agent in session 01K7…,
    /// approved by you in the cli", "edited by you in a memory file, approved in the desktop".
    pub fn summary(&self) -> String {
        let approved = |client: &Option<String>| {
            client
                .as_ref()
                .map_or_else(String::new, |client| format!(", approved in the {client}"))
        };
        match self.origin.as_deref() {
            Some(ORIGIN_FILE) => {
                return format!("edited by you in a memory file{}", approved(&self.client));
            }
            Some(ORIGIN_IMPORT) => {
                return format!("imported by you{}", approved(&self.client));
            }
            _ => {}
        }
        let place = |client: &Option<String>| {
            client
                .as_ref()
                .map_or_else(String::new, |client| format!(" in the {client}"))
        };
        match (self.author, self.session_id) {
            (MemoryAuthor::User, Some(_)) => {
                format!(
                    "edited by you{} from an agent's proposal",
                    place(&self.client)
                )
            }
            (MemoryAuthor::User, None) => format!("entered by you{}", place(&self.client)),
            (MemoryAuthor::Agent, Some(session)) => format!(
                "proposed by an agent in session {session}, approved by you{}",
                place(&self.client)
            ),
            (MemoryAuthor::Agent, None) => {
                format!(
                    "proposed by an agent, approved by you{}",
                    place(&self.client)
                )
            }
        }
    }
}

/// One entry as it is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub scope: MemoryScope,
    /// The workspace of a `workspace` entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    pub category: MemoryCategory,
    pub state: MemoryState,
    /// The current revision (1 for a new entry); what `expected_revision` names.
    pub revision: u64,
    /// The body of the current revision.
    pub body: String,
    /// A model wrote the current revision and the user has not edited or confirmed it since.
    pub inferred: bool,
    pub provenance: MemoryProvenance,
    pub created_ms: u64,
    pub updated_ms: u64,
}

/// One revision of an entry. Revisions are never changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRevision {
    pub revision: u64,
    pub operation: MemoryOp,
    pub body: String,
    /// SHA-256 of the body, hex.
    pub content_hash: String,
    pub author: MemoryAuthor,
    pub provenance: MemoryProvenance,
    /// The approval (proposal) that applied it, for a revision a model proposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<ApprovalId>,
    pub operation_id: String,
    pub scope_revision: u64,
    pub created_ms: u64,
}

/// The daemon's receipt for a change that was committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryReceipt {
    /// Always `"applied"`: nothing is "saved" without one of these.
    pub status: String,
    pub operation_id: String,
    pub entry_id: String,
    pub revision: u64,
    /// The scope's revision counter after the change.
    pub scope_revision: u64,
}

/// `memory.list`: entries, newest update first within a category. Archived entries only with
/// `include_archived`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<MemoryScope>,
    /// With `scope: workspace`, the workspace; without `scope`, entries of that workspace and
    /// the global ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<MemoryCategory>,
    #[serde(default)]
    pub include_archived: bool,
    /// Only entries whose body contains this text (case-insensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

/// The answer of `memory.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntries {
    pub entries: Vec<MemoryEntry>,
}

/// `memory.history`: every revision of an entry, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryHistoryRequest {
    pub entry_id: String,
}

/// The answer of `memory.history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryHistory {
    pub entry: MemoryEntry,
    pub revisions: Vec<MemoryRevision>,
}

/// `memory.write`: a new entry, entered by the user. Answered with [`MemoryReceipt`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryWrite {
    pub operation_id: String,
    pub scope: MemoryScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    pub category: MemoryCategory,
    pub body: String,
}

/// `memory.update`: the user replaces the body of an entry as they saw it at
/// `expected_revision`; `conflict` when it moved on. Answered with [`MemoryReceipt`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryUpdate {
    pub operation_id: String,
    pub entry_id: String,
    pub expected_revision: u64,
    pub body: String,
}

/// `memory.archive` / `memory.restore`: the user stops (or resumes) using an entry; history is
/// kept and the change is a revision of its own. Answered with [`MemoryReceipt`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTransition {
    pub operation_id: String,
    pub entry_id: String,
    pub expected_revision: u64,
}

/// `memory.propose`: what an agent asks to remember (`op: create`) or to change. The
/// fields an operation needs: `create` — `scope`, `workspace_id` (for a workspace entry),
/// `category`, `body`; `update` — `entry_id`, `expected_revision`, `body`; `archive` and
/// `restore` — `entry_id`, `expected_revision`. Other fields are ignored (the entry decides
/// scope and category).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryPropose {
    pub operation_id: String,
    pub op: MemoryOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<MemoryScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<MemoryCategory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// The answer of `memory.propose`. `status` is `"proposed"` with `applied: false` — never
/// "saved". Repeating an `operation_id` whose proposal was answered meanwhile reports that
/// state (`applied`, `rejected`, `conflict`, `cancelled`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProposed {
    pub status: String,
    pub proposal_id: ApprovalId,
    pub applied: bool,
}

/// One proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProposal {
    /// Also the id of its approval (`approval.list`, `approval.respond`).
    pub id: ApprovalId,
    pub operation: MemoryOp,
    pub state: ProposalState,
    pub scope: MemoryScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    pub category: MemoryCategory,
    /// The entry an `update`, `archive` or `restore` is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    /// The proposed body (for `archive` and `restore`, the entry's body as it was proposed).
    pub body: String,
    /// What the entry says now, for the user to compare with an `update` (absent for `create`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_body: Option<String>,
    pub provenance: MemoryProvenance,
    pub created_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ms: Option<u64>,
}

/// `memory.proposals.list`: the proposals waiting for the user, oldest first; with
/// `include_resolved` also the most recent answered ones (newest last).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryProposalsList {
    #[serde(default)]
    pub include_resolved: bool,
}

/// The answer of `memory.proposals.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProposals {
    pub proposals: Vec<MemoryProposal>,
}

/// `memory.proposals.resolve`: the user's answer. `edited_body` goes with `decision: edit` (and
/// only then).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProposalResolve {
    pub proposal_id: ApprovalId,
    pub decision: ProposalDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_body: Option<String>,
}

/// The answer of `memory.proposals.resolve`: the proposal as it is now (`applied`, `rejected`
/// or `conflict`) and, when it was applied, the receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProposalResolved {
    pub proposal: MemoryProposal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<MemoryReceipt>,
}

/// `memory.import` (protocol 0.46, capability `memory.files`; user-only): entries from a file
/// or from text become **pending proposals** (provenance origin [`ORIGIN_IMPORT`]) the user
/// approves like any other; nothing is applied, and a body that looks like a credential is
/// refused as ever. Exactly one of `path` (a file on the daemon's machine) and `text`.
///
/// The text is Markdown as the memory files are written (`crate::memory_files`), or JSON as
/// `hutan memory export --format json` writes it. A section whose `id` names an active entry
/// of the target scope with another body becomes an `update` of that entry, a section whose
/// body is already an active entry's is skipped, any other becomes a `create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryImport {
    pub scope: MemoryScope,
    /// With `scope: workspace`, the workspace the entries go to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The category of Markdown sections that no `# Profile` / `# Facts` / `# Notes` heading
    /// precedes (and of JSON entries without one); a `path` named `profile.md`, `facts.md` or
    /// `notes.md` supplies it when this is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<MemoryCategory>,
}

/// One thing a file or an import could not use: shown in the Memory page's Problems card and by
/// `hutan memory files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFileProblem {
    /// The file (`facts.md`), or what was imported.
    pub file: String,
    /// The section: its entry id, or `line N` for one without.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// What is wrong, in words ("Memory is not a secret store: …"). Never the text itself.
    pub reason: String,
    pub at_ms: u64,
}

/// The answer of `memory.import`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryImported {
    /// New entries proposed (`create`).
    pub created: u32,
    /// Changes to existing entries proposed (`update`).
    pub updated: u32,
    /// Sections that already say what the memory says, or that a pending proposal covers.
    pub unchanged: u32,
    /// The proposals made, oldest first; they wait in `memory.proposals.list`.
    pub proposals: Vec<ApprovalId>,
    /// Sections refused, with the reason; the rest was imported.
    #[serde(default)]
    pub skipped: Vec<MemoryFileProblem>,
}

/// `memory.files.status` (protocol 0.46, capability `memory.files`; user-only): the mirror of
/// one scope's memory. The daemon reads the files for edits before it answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFilesStatusRequest {
    pub scope: MemoryScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// One file of the mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFileInfo {
    /// `profile.md`, `facts.md` or `notes.md`.
    pub name: String,
    pub bytes: u64,
}

/// The answer of `memory.files.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFilesStatus {
    pub scope: MemoryScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// Whether the daemon keeps this scope's memory in files. A workspace's are written only
    /// with the setting `assistant.memory_files_in_project`.
    pub enabled: bool,
    /// The folder of the files (absent when there is none: the setting is off, or the
    /// workspace has no folder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<std::path::PathBuf>,
    /// Why there are no files, in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub files: Vec<MemoryFileInfo>,
    /// When the daemon last wrote the files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_write_ms: Option<u64>,
    /// The folder is inside a git repository: its changes show in Changes and can be committed.
    #[serde(default)]
    pub tracked: bool,
    /// Proposals made from edits of the files that still wait for the user.
    #[serde(default)]
    pub pending: u32,
    /// Edits the daemon could not use. A file with a problem is left as the user wrote it until
    /// the problem is fixed.
    #[serde(default)]
    pub problems: Vec<MemoryFileProblem>,
}
