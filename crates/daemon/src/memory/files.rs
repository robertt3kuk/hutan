//! The memory as Markdown files (protocol 0.46, capability `memory.files`; `docs/assistant.md`
//! § Memory files, ADR 0017 amendment).
//!
//! **SQLite stays the source of truth.** The files are a mirror the daemon writes after every
//! change of a scope — Global under the Home folder's `.mawas/memory/`, a workspace under its
//! project's `.hutan/memory/` only with `assistant.memory_files_in_project` — one file per
//! category, each entry a section behind an id comment (`hutan_protocol::memory_files` is the
//! format). Writes are atomic (a temporary file beside the target, then a rename).
//!
//! **An edit of a file is a proposal, never a change.** A watcher (one per folder, and the
//! daemon reads the folder again whenever `memory.files.status` asks) compares what is on disk
//! with what the daemon last read or wrote; when it differs the files are parsed and each
//! difference from the memory becomes a *pending proposal* with provenance `origin: file`: a
//! changed section an `update`, a section without an id a `create`, a section that is gone an
//! `archive`. The proposals wait in the Memory page like an agent's; until they are answered
//! the files show the user's text (the mirror is rendered from the memory plus the pending
//! proposals of files), afterwards the files show what the memory became.
//!
//! What cannot be used — a section that looks like a credential, a body over 2 KiB, a marker
//! that cannot be read — is a *problem*: listed by `memory.files.status`
//! (file, section, reason; never the text), and the file is left as the user wrote it until it
//! parses cleanly. Nothing is archived on the strength of a file that has a problem.
//!
//! A file that is missing is not a deletion: it is written again. A folder, file or parent that
//! is a symbolic link is not followed.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use hutan_core::ApprovalId;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::memory::{
    IMPORT_MAX_BYTES, IMPORT_MAX_SECTIONS, MemoryAuthor, MemoryCategory, MemoryFileInfo,
    MemoryFileProblem, MemoryFilesStatus, MemoryFilesStatusRequest, MemoryImport, MemoryImported,
    MemoryProvenance, MemoryScope, ORIGIN_FILE, ORIGIN_IMPORT,
};
use hutan_protocol::memory_files::{self as format, Item, Parsed};
use hutan_protocol::messages::{ClientKind, WorkspaceKind};
use hutan_storage as st;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::{
    MemoryService, bad_request, category_of, clean_body, client_label, db_category, error,
    internal, scope_parts,
};
use crate::registry;

/// Wait after the platform reports a change, so a save that writes twice is read once.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Largest file the daemon reads.
const FILE_MAX_BYTES: u64 = 1024 * 1024;
/// Problems kept per scope.
const PROBLEMS_MAX: usize = 100;
/// Signals the platform watchers may leave unread (a flood only needs to be remembered once).
const SIGNALS: usize = 64;

/// The files of every scope: what the daemon last read or wrote, and what it refused.
#[derive(Debug, Default)]
pub(super) struct Mirror {
    state: Mutex<State>,
    wake: OnceLock<mpsc::Sender<PathBuf>>,
}

#[derive(Debug, Default)]
struct State {
    scopes: HashMap<st::Scope, ScopeFiles>,
    watchers: HashMap<PathBuf, Held>,
}

struct Held(#[allow(dead_code)] RecommendedWatcher);

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Watcher")
    }
}

#[derive(Debug, Default)]
struct ScopeFiles {
    folder: Option<PathBuf>,
    /// Per file: what the daemon last saw there (see [`Observed::key`]).
    seen: HashMap<MemoryCategory, String>,
    last_write_ms: Option<u64>,
    problems: Vec<MemoryFileProblem>,
}

/// What is on disk at one file's path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Observed {
    Missing,
    Text(String),
    /// Present but not usable (a link, too large, not text): the reason.
    Unreadable(String),
}

impl Observed {
    fn key(&self) -> String {
        match self {
            Self::Missing => "missing".to_owned(),
            Self::Text(text) => st::hash_text(text),
            Self::Unreadable(reason) => format!("unreadable:{reason}"),
        }
    }
}

fn observe(path: &Path) -> Observed {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Observed::Missing,
        Err(e) => Observed::Unreadable(format!("could not be read: {e}")),
        Ok(meta) if meta.file_type().is_symlink() => {
            Observed::Unreadable("is a symbolic link: Hutan does not follow it".to_owned())
        }
        Ok(meta) if !meta.is_file() => Observed::Unreadable("is not a file".to_owned()),
        Ok(meta) if meta.len() > FILE_MAX_BYTES => {
            Observed::Unreadable("is larger than 1 MiB".to_owned())
        }
        Ok(_) => match fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => Observed::Text(text),
                Err(_) => Observed::Unreadable("is not UTF-8 text".to_owned()),
            },
            Err(e) => Observed::Unreadable(format!("could not be read: {e}")),
        },
    }
}

fn observe_all(folder: &Path) -> Vec<(MemoryCategory, Observed)> {
    MemoryCategory::ALL
        .into_iter()
        .map(|category| (category, observe(&folder.join(format::file_name(category)))))
        .collect()
}

/// Where a scope's files go.
enum Folder {
    At(PathBuf),
    /// There are none, and why.
    Off(String),
}

/// Whether `folder` or its parent is a symbolic link (the files must stay where they are said
/// to be).
fn through_link(folder: &Path) -> Option<&Path> {
    [Some(folder), folder.parent()]
        .into_iter()
        .flatten()
        .find(|path| {
            fs::symlink_metadata(path)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
        })
}

/// `body` as comparisons see it: trailing blanks and line endings do not count.
fn norm(body: &str) -> String {
    body.trim()
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn scope_key(scope: st::Scope) -> String {
    match scope {
        st::Scope::Global => "global".to_owned(),
        st::Scope::Workspace(id) => format!("ws-{id}"),
    }
}

/// One proposal the memory should hold because of a file or an import.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Want {
    op: st::Operation,
    entry_id: Option<String>,
    expected: Option<u64>,
    category: st::Category,
    body: String,
}

impl Want {
    fn matches(&self, row: &st::ProposalRow) -> bool {
        row.operation == self.op
            && row.entry_id == self.entry_id
            && row.expected_revision == self.expected
            && row.category == self.category
            && norm(&row.body) == norm(&self.body)
    }
}

/// What to do with the proposals that wait, given what the files (or the import) want.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    /// Wants a pending proposal already covers.
    covered: u32,
    /// Pending proposals nothing wants any more.
    cancel: Vec<String>,
    insert: Vec<Want>,
}

fn plan(wants: &[Want], pending: &[st::ProposalRow], cancel_the_rest: bool) -> Plan {
    let mut plan = Plan::default();
    let mut used = HashSet::new();
    for want in wants {
        match pending
            .iter()
            .find(|row| !used.contains(&row.id) && want.matches(row))
        {
            Some(row) => {
                used.insert(row.id.clone());
                plan.covered += 1;
            }
            None => plan.insert.push(want.clone()),
        }
    }
    if cancel_the_rest {
        plan.cancel = pending
            .iter()
            .filter(|row| !used.contains(&row.id))
            .map(|row| row.id.clone())
            .collect();
    }
    plan
}

/// The row of a proposal a file or an import makes.
fn row_of(
    scope: st::Scope,
    want: &Want,
    origin: &str,
    operation_id: String,
    now_ms: u64,
) -> Result<st::ProposalRow, ErrorBody> {
    let provenance = MemoryProvenance {
        author: MemoryAuthor::User,
        client: None,
        session_id: None,
        turn_id: None,
        at_ms: now_ms,
        origin: Some(origin.to_owned()),
    };
    Ok(st::ProposalRow {
        id: ApprovalId::new().to_string(),
        operation_id,
        scope,
        operation: want.op,
        entry_id: want.entry_id.clone(),
        expected_revision: want.expected,
        category: want.category,
        body: want.body.clone(),
        provenance_json: serde_json::to_string(&provenance).map_err(internal)?,
        state: st::ProposalState::Pending,
        origin_chat_session_id: None,
        created_ms: now_ms,
        resolved_ms: None,
    })
}

fn operation_id(scope: st::Scope, want: &Want, origin: &str) -> String {
    let text = format!(
        "{}|{}|{:?}|{}|{}",
        want.op.as_str(),
        want.entry_id.as_deref().unwrap_or(""),
        want.expected,
        want.category.as_str(),
        norm(&want.body)
    );
    format!("{origin}:{}:{}", scope_key(scope), st::hash_text(&text))
}

/// Applies `plan` in the storage (blocking): cancels, then stores each want as a new pending
/// proposal. An `operation_id` stored before is reused when its proposal still waits, and
/// replaced by a fresh one when it was answered (the user may ask again for what they rejected).
fn store(
    storage: &st::Storage,
    scope: st::Scope,
    plan: &Plan,
    origin: &str,
    now_ms: u64,
) -> Result<Vec<(Want, String)>, st::MemoryError> {
    let memory = storage.memory();
    for id in &plan.cancel {
        memory.cancel_proposal(id, now_ms)?;
    }
    let mut stored = Vec::new();
    for want in &plan.insert {
        let mut operation = operation_id(scope, want, origin);
        if let Some(existing) = memory.proposal_by_operation(&operation)? {
            if existing.state == st::ProposalState::Pending {
                continue;
            }
            operation = format!("{operation}:{now_ms}");
        }
        let row = row_of(scope, want, origin, operation, now_ms)
            .map_err(|e| st::MemoryError::InvalidState(e.message))?;
        let id = memory.insert_proposal(row)?.id;
        stored.push((want.clone(), id));
    }
    Ok(stored)
}

fn origin_of(row: &st::ProposalRow) -> Option<String> {
    serde_json::from_str::<MemoryProvenance>(&row.provenance_json)
        .ok()
        .and_then(|p| p.origin)
}

/// The active entries of a scope in file order, and the ids that are archived.
struct Entries {
    active: Vec<st::EntryView>,
    archived: HashSet<String>,
}

fn entries_of(storage: &st::Storage, scope: st::Scope) -> Result<Entries, st::MemoryError> {
    let filter = match scope {
        st::Scope::Global => st::ScopeFilter::Global,
        st::Scope::Workspace(id) => st::ScopeFilter::Workspace(id),
    };
    let mut active = Vec::new();
    let mut archived = HashSet::new();
    for view in storage.memory().list(filter, true)? {
        if view.entry.state == st::EntryState::Active {
            active.push(view);
        } else {
            archived.insert(view.entry.id.clone());
        }
    }
    active
        .sort_by(|a, b| (a.entry.created_ms, &a.entry.id).cmp(&(b.entry.created_ms, &b.entry.id)));
    Ok(Entries { active, archived })
}

fn file_pending(
    storage: &st::Storage,
    scope: st::Scope,
) -> Result<Vec<st::ProposalRow>, st::MemoryError> {
    Ok(storage
        .memory()
        .proposals(0)?
        .into_iter()
        .filter(|row| row.scope == scope && origin_of(row).as_deref() == Some(ORIGIN_FILE))
        .collect())
}

/// What the files say, against the memory: the proposals they call for and the problems.
struct Reading {
    wants: Vec<Want>,
    problems: Vec<(String, Option<String>, String)>,
}

fn read_files(entries: &Entries, observed: &[(MemoryCategory, Observed)]) -> Reading {
    let by_id: HashMap<&str, &st::EntryView> = entries
        .active
        .iter()
        .map(|view| (view.entry.id.as_str(), view))
        .collect();
    let mut wants: Vec<Want> = Vec::new();
    let mut problems = Vec::new();
    let mut present: HashSet<String> = HashSet::new();
    // Categories whose file parsed cleanly: only their entries may be called deleted.
    let mut clean: HashSet<MemoryCategory> = HashSet::new();
    for (category, seen) in observed {
        let name = format::file_name(*category).to_owned();
        let text = match seen {
            Observed::Missing => continue,
            Observed::Unreadable(reason) => {
                problems.push((name, None, format!("the file {reason}")));
                continue;
            }
            Observed::Text(text) => text,
        };
        let Parsed {
            sections,
            problems: marks,
        } = format::parse(text, Some(*category));
        let mut file_clean = marks.is_empty();
        for mark in marks {
            problems.push((
                name.clone(),
                Some(format!("line {}", mark.line)),
                mark.reason,
            ));
        }
        let mut ids = HashSet::new();
        for section in &sections {
            let label = section.label();
            let mut refuse = |reason: String| {
                file_clean = false;
                problems.push((name.clone(), Some(label.clone()), reason));
            };
            if let Some(id) = &section.id {
                if !ids.insert(id.clone()) || present.contains(id) {
                    refuse("this id appears twice in the files".to_owned());
                    continue;
                }
                present.insert(id.clone());
                if entries.archived.contains(id) {
                    refuse("the entry is archived: restore it in the Memory page first".to_owned());
                    continue;
                }
            }
            let body = match clean_body(&section.body) {
                Ok(body) => body,
                Err(e) => {
                    refuse(e.message);
                    continue;
                }
            };
            let section_category = section.category.unwrap_or(*category);
            match section.id.as_deref().and_then(|id| by_id.get(id)) {
                Some(view) => {
                    if norm(&view.current.body) == norm(&body) {
                        continue;
                    }
                    wants.push(Want {
                        op: st::Operation::Update,
                        entry_id: Some(view.entry.id.clone()),
                        expected: Some(view.entry.current_revision),
                        category: view.entry.category,
                        body,
                    });
                }
                None => {
                    // No id, or an id of another memory (a file pulled from a teammate's
                    // repository): a new entry, once.
                    let category = db_category(section_category);
                    let duplicate = entries.active.iter().any(|view| {
                        view.entry.category == category && norm(&view.current.body) == norm(&body)
                    }) || wants.iter().any(|w| {
                        w.op == st::Operation::Create
                            && w.category == category
                            && norm(&w.body) == norm(&body)
                    });
                    if !duplicate {
                        wants.push(Want {
                            op: st::Operation::Create,
                            entry_id: None,
                            expected: None,
                            category,
                            body,
                        });
                    }
                }
            }
        }
        if file_clean {
            clean.insert(*category);
        }
    }
    for view in &entries.active {
        let category = category_of(view.entry.category);
        if clean.contains(&category) && !present.contains(&view.entry.id) {
            wants.push(Want {
                op: st::Operation::Archive,
                entry_id: Some(view.entry.id.clone()),
                expected: Some(view.entry.current_revision),
                category: view.entry.category,
                body: view.current.body.clone(),
            });
        }
    }
    Reading { wants, problems }
}

/// The text of each category's file: the active entries, with what the pending proposals of
/// files would make of them.
fn render(
    scope: MemoryScope,
    entries: &Entries,
    pending: &[st::ProposalRow],
) -> Vec<(MemoryCategory, String)> {
    let mut by_category: HashMap<MemoryCategory, Vec<(Option<String>, u64, String)>> =
        HashMap::new();
    for view in &entries.active {
        let archived_by_edit = pending.iter().any(|row| {
            row.operation == st::Operation::Archive
                && row.entry_id.as_deref() == Some(view.entry.id.as_str())
                && row.expected_revision == Some(view.entry.current_revision)
        });
        if archived_by_edit {
            continue;
        }
        let body = pending
            .iter()
            .find(|row| {
                row.operation == st::Operation::Update
                    && row.entry_id.as_deref() == Some(view.entry.id.as_str())
                    && row.expected_revision == Some(view.entry.current_revision)
            })
            .map_or_else(|| view.current.body.clone(), |row| row.body.clone());
        by_category
            .entry(category_of(view.entry.category))
            .or_default()
            .push((
                Some(view.entry.id.clone()),
                view.entry.current_revision,
                body,
            ));
    }
    for row in pending
        .iter()
        .filter(|row| row.operation == st::Operation::Create)
    {
        by_category
            .entry(category_of(row.category))
            .or_default()
            .push((None, 0, row.body.clone()));
    }
    MemoryCategory::ALL
        .into_iter()
        .map(|category| {
            let owned = by_category.remove(&category).unwrap_or_default();
            let items: Vec<Item<'_>> = owned
                .iter()
                .map(|(id, revision, body)| Item {
                    id: id.as_deref(),
                    revision: *revision,
                    body,
                })
                .collect();
            (category, format::render_file(scope, category, &items))
        })
        .collect()
}

/// Writes `text` to `path` through a temporary file beside it (blocking).
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = folder.join(format!(".hutan-write-{}.tmp", ulid::Ulid::generate()));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// What writing the files did.
struct Written {
    /// What each written file now holds.
    seen: Vec<(MemoryCategory, String)>,
}

fn has_git(folder: &Path) -> bool {
    folder.ancestors().any(|dir| dir.join(".git").exists())
}

// The service ----------------------------------------------------------------------------------

impl MemoryService {
    /// Where `scope`'s files are, or why there are none.
    async fn folder_of(&self, scope: st::Scope) -> Folder {
        let workspaces = match self.registry.list_workspaces().await {
            Ok(list) => list.workspaces,
            Err(e) => {
                return Folder::Off(format!("the workspaces could not be read: {}", e.message));
            }
        };
        let (root, rel) = match scope {
            st::Scope::Global => {
                let home = workspaces.iter().find(|w| w.kind == WorkspaceKind::Home);
                (home.and_then(|w| w.roots.first()), format::GLOBAL_DIR)
            }
            st::Scope::Workspace(id) => {
                if !self.assistant.read().memory_files_in_project {
                    return Folder::Off(
                        "Keep memory in the project is off: turn on `assistant.memory_files_in_project` to write this workspace's memory into its project (`.hutan/memory/`).".to_owned(),
                    );
                }
                let workspace = workspaces.iter().find(|w| w.id == id);
                match workspace {
                    Some(w) if w.kind == WorkspaceKind::Project => {
                        (w.roots.first(), format::PROJECT_DIR)
                    }
                    Some(_) => {
                        return Folder::Off(
                            "only a project keeps memory files in its folder".to_owned(),
                        );
                    }
                    None => return Folder::Off("the workspace is gone".to_owned()),
                }
            }
        };
        let Some(root) = root else {
            return Folder::Off("there is no folder to keep the files in".to_owned());
        };
        let folder = root.join(rel);
        if let Some(link) = through_link(&folder) {
            return Folder::Off(format!(
                "{} is a symbolic link: Hutan does not write through it",
                link.display()
            ));
        }
        Folder::At(folder)
    }

    /// Called with the gate held after a change of the memory: writes the mirror again.
    pub(super) async fn mirror_changed(&self, scope: st::Scope) {
        if let Err(e) = self.refresh(scope, true).await {
            warn!(message = %e.message, "could not write the memory files");
        }
    }

    /// The files of `scope` are read again and, when something changed — on disk or in the
    /// memory (`db_changed`) — written. Called with the gate held.
    async fn refresh(&self, scope: st::Scope, db_changed: bool) -> Result<(), ErrorBody> {
        let Folder::At(folder) = self.folder_of(scope).await else {
            return Ok(());
        };
        let observed = {
            let folder = folder.clone();
            tokio::task::spawn_blocking(move || observe_all(&folder))
                .await
                .map_err(internal)?
        };
        let edited = {
            let mut state = self.files.state.lock();
            let files = state.scopes.entry(scope).or_default();
            files.folder = Some(folder.clone());
            observed
                .iter()
                .any(|(category, seen)| files.seen.get(category) != Some(&seen.key()))
        };
        let mut changed = false;
        if edited {
            changed = self.reconcile(scope, &observed).await?;
            let mut state = self.files.state.lock();
            let files = state.scopes.entry(scope).or_default();
            for (category, seen) in &observed {
                files.seen.insert(*category, seen.key());
            }
        }
        if edited || db_changed {
            changed |= self.write_mirror(scope, &folder, &observed).await?;
        }
        if changed {
            let revision = self
                .db(move |s| Ok(s.memory().scope_revision(scope)?))
                .await?;
            self.publish_changed(scope, revision);
        }
        Ok(())
    }

    /// Turns what the files say into proposals and problems. `true` when either changed.
    async fn reconcile(
        &self,
        scope: st::Scope,
        observed: &[(MemoryCategory, Observed)],
    ) -> Result<bool, ErrorBody> {
        let now = registry::now_ms();
        let observed = observed.to_vec();
        let (reading, stored, cancelled) = self
            .db(move |s| {
                let entries = entries_of(s, scope)?;
                let pending = file_pending(s, scope)?;
                let reading = read_files(&entries, &observed);
                let plan = plan(&reading.wants, &pending, true);
                let cancelled = plan.cancel.len();
                let stored = store(s, scope, &plan, ORIGIN_FILE, now)?;
                Ok((reading, stored, cancelled))
            })
            .await?;
        for (want, id) in &stored {
            info!(
                target: "hutan::audit", actor = "user", action = "memory.file_edit", proposal = %id,
                operation = want.op.as_str(), "memory proposal made from an edited file"
            );
        }
        let mut state = self.files.state.lock();
        let files = state.scopes.entry(scope).or_default();
        let mut problems: Vec<MemoryFileProblem> = reading
            .problems
            .into_iter()
            .map(|(file, section, reason)| {
                let at_ms = files
                    .problems
                    .iter()
                    .find(|p| p.file == file && p.section == section && p.reason == reason)
                    .map_or(now, |p| p.at_ms);
                MemoryFileProblem {
                    file,
                    section,
                    reason,
                    at_ms,
                }
            })
            .collect();
        problems.truncate(PROBLEMS_MAX);
        let problems_changed = problems != files.problems;
        files.problems = problems;
        Ok(problems_changed || !stored.is_empty() || cancelled > 0)
    }

    /// Writes the files that differ from what the memory says (blocking work off the runtime).
    /// `true` when it wrote one.
    async fn write_mirror(
        &self,
        scope: st::Scope,
        folder: &Path,
        observed: &[(MemoryCategory, Observed)],
    ) -> Result<bool, ErrorBody> {
        let (entries, pending) = self
            .db(move |s| Ok((entries_of(s, scope)?, file_pending(s, scope)?)))
            .await?;
        let skip: HashSet<MemoryCategory> = {
            let state = self.files.state.lock();
            state
                .scopes
                .get(&scope)
                .map(|files| {
                    MemoryCategory::ALL
                        .into_iter()
                        .filter(|category| {
                            let name = format::file_name(*category);
                            files.problems.iter().any(|p| p.file == name)
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let texts = render(scope_parts(scope).0, &entries, &pending);
        let empty = entries.active.is_empty() && pending.is_empty();
        let folder_owned = folder.to_path_buf();
        let observed: HashMap<MemoryCategory, Observed> = observed.iter().cloned().collect();
        let written = tokio::task::spawn_blocking(move || -> std::io::Result<Written> {
            let mut written = Written { seen: Vec::new() };
            if empty && !folder_owned.exists() {
                return Ok(written);
            }
            fs::create_dir_all(&folder_owned)?;
            for (category, text) in texts {
                if skip.contains(&category) {
                    continue;
                }
                let path = folder_owned.join(format::file_name(category));
                let before = observed.get(&category);
                match before {
                    Some(Observed::Unreadable(_)) => continue,
                    Some(Observed::Text(current)) if *current == text => continue,
                    _ => {}
                }
                // Somebody wrote the file since it was read: that edit is theirs to keep, and
                // the watcher reads it.
                if before.map(Observed::key) != Some(observe(&path).key()) {
                    continue;
                }
                write_atomic(&path, &text)?;
                written.seen.push((category, Observed::Text(text).key()));
            }
            Ok(written)
        })
        .await
        .map_err(internal)?
        .map_err(|e| {
            error(
                ErrorCode::Internal,
                format!("could not write the memory files: {e}"),
            )
        })?;
        let wrote = !written.seen.is_empty();
        {
            let mut state = self.files.state.lock();
            let files = state.scopes.entry(scope).or_default();
            for (category, key) in written.seen {
                files.seen.insert(category, key);
            }
            if wrote {
                files.last_write_ms = Some(registry::now_ms());
            }
        }
        if folder.exists() {
            self.watch(folder);
        }
        Ok(wrote)
    }

    /// Starts watching `folder` (once).
    fn watch(&self, folder: &Path) {
        let Some(wake) = self.files.wake.get().cloned() else {
            return;
        };
        let mut state = self.files.state.lock();
        if state.watchers.contains_key(folder) {
            return;
        }
        let target = folder.to_path_buf();
        let signal = target.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event
                && matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                )
            {
                // Full: a signal is already waiting, which is all this one would say.
                let _ = wake.try_send(signal.clone());
            }
        });
        match watcher.and_then(|mut watcher| {
            watcher.watch(&target, RecursiveMode::NonRecursive)?;
            Ok(watcher)
        }) {
            Ok(watcher) => {
                debug!(folder = %target.display(), "watching the memory files");
                state.watchers.insert(target, Held(watcher));
            }
            Err(e) => {
                warn!(folder = %target.display(), error = %e, "could not watch the memory files")
            }
        }
    }

    /// The files of `scope` were maybe edited: reads them (the watcher's and the status's work).
    pub(super) async fn sync_scope(&self, scope: st::Scope) -> Result<(), ErrorBody> {
        let _gate = self.gate.lock().await;
        self.refresh(scope, false).await
    }

    async fn sync_folder(&self, folder: &Path) {
        let scope = {
            let state = self.files.state.lock();
            state
                .scopes
                .iter()
                .find(|(_, files)| files.folder.as_deref() == Some(folder))
                .map(|(scope, _)| *scope)
        };
        if let Some(scope) = scope
            && let Err(e) = self.sync_scope(scope).await
        {
            warn!(message = %e.message, "could not read the memory files");
        }
    }

    /// Reads the files of the global scope, and of every project when they are kept in the
    /// projects: what the daemon does at start and when the setting changes.
    async fn bootstrap(&self) {
        let mut scopes = vec![st::Scope::Global];
        if self.assistant.read().memory_files_in_project
            && let Ok(list) = self.registry.list_workspaces().await
        {
            scopes.extend(
                list.workspaces
                    .iter()
                    .filter(|w| w.kind == WorkspaceKind::Project)
                    .map(|w| st::Scope::Workspace(w.id)),
            );
        }
        for scope in scopes {
            let _gate = self.gate.lock().await;
            if let Err(e) = self.refresh(scope, true).await {
                warn!(message = %e.message, "could not write the memory files");
            }
        }
    }

    // memory.files.status --------------------------------------------------------------------

    pub async fn files_status(
        &self,
        req: MemoryFilesStatusRequest,
    ) -> Result<MemoryFilesStatus, ErrorBody> {
        let scope = self.target_scope(req.scope, req.workspace_id, None).await?;
        let _gate = self.gate.lock().await;
        let folder = match self.folder_of(scope).await {
            Folder::At(folder) => folder,
            Folder::Off(note) => {
                return Ok(MemoryFilesStatus {
                    scope: req.scope,
                    workspace_id: req.workspace_id,
                    enabled: false,
                    folder: None,
                    note: Some(note),
                    files: Vec::new(),
                    last_write_ms: None,
                    tracked: false,
                    pending: 0,
                    problems: Vec::new(),
                });
            }
        };
        self.refresh(scope, false).await?;
        let pending = self.db(move |s| Ok(file_pending(s, scope)?.len())).await?;
        let listing = {
            let folder = folder.clone();
            tokio::task::spawn_blocking(move || {
                let files: Vec<MemoryFileInfo> = MemoryCategory::ALL
                    .into_iter()
                    .filter_map(|category| {
                        let name = format::file_name(category);
                        let meta = fs::symlink_metadata(folder.join(name)).ok()?;
                        meta.is_file().then(|| MemoryFileInfo {
                            name: name.to_owned(),
                            bytes: meta.len(),
                        })
                    })
                    .collect();
                (files, has_git(&folder))
            })
            .await
            .map_err(internal)?
        };
        let (files, tracked) = listing;
        let (last_write_ms, problems) = {
            let state = self.files.state.lock();
            state.scopes.get(&scope).map_or((None, Vec::new()), |f| {
                (f.last_write_ms, f.problems.clone())
            })
        };
        let note = files.is_empty().then(|| {
            "Nothing is remembered in this scope yet: the files appear with its first entry."
                .to_owned()
        });
        Ok(MemoryFilesStatus {
            scope: req.scope,
            workspace_id: req.workspace_id,
            enabled: true,
            folder: Some(folder),
            note,
            files,
            last_write_ms,
            tracked,
            pending: u32::try_from(pending).unwrap_or(u32::MAX),
            problems,
        })
    }

    // memory.import --------------------------------------------------------------------------

    pub async fn import(
        &self,
        req: MemoryImport,
        client: Option<ClientKind>,
    ) -> Result<MemoryImported, ErrorBody> {
        let (label, text, default_category) = match (&req.path, &req.text) {
            (Some(_), Some(_)) | (None, None) => {
                return Err(bad_request("give exactly one of `path` and `text`"));
            }
            (Some(path), None) => {
                let path = path.clone();
                let read = tokio::task::spawn_blocking(move || -> Result<String, String> {
                    let meta =
                        fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                    if !meta.is_file() {
                        return Err(format!("{} is not a file", path.display()));
                    }
                    if meta.len() > IMPORT_MAX_BYTES as u64 {
                        return Err(format!("{} is larger than 1 MiB", path.display()));
                    }
                    let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                    String::from_utf8(bytes)
                        .map_err(|_| format!("{} is not UTF-8 text", path.display()))
                })
                .await
                .map_err(internal)?
                .map_err(bad_request)?;
                let name = req
                    .path
                    .as_deref()
                    .and_then(|p| p.file_name())
                    .map_or_else(|| "import".to_owned(), |n| n.to_string_lossy().into_owned());
                let category = req.category.or_else(|| {
                    req.path
                        .as_deref()
                        .and_then(|p| format::category_of_file(&p.to_string_lossy()))
                });
                (name, read, category)
            }
            (None, Some(text)) => {
                if text.len() > IMPORT_MAX_BYTES {
                    return Err(bad_request("the text is larger than 1 MiB"));
                }
                ("import".to_owned(), text.clone(), req.category)
            }
        };
        let scope = self.target_scope(req.scope, req.workspace_id, None).await?;
        let parsed = if format::looks_like_json(&text) {
            format::parse_json(&text).map_err(|e| bad_request(format!("{label}: {e}")))?
        } else {
            format::parse(&text, default_category)
        };
        if parsed.sections.len() > IMPORT_MAX_SECTIONS {
            return Err(bad_request(format!(
                "{label} has {} sections: at most {IMPORT_MAX_SECTIONS} can be imported at once",
                parsed.sections.len()
            )));
        }
        let now = registry::now_ms();
        let mut skipped: Vec<MemoryFileProblem> = parsed
            .problems
            .iter()
            .map(|p| MemoryFileProblem {
                file: label.clone(),
                section: Some(format!("line {}", p.line)),
                reason: p.reason.clone(),
                at_ms: now,
            })
            .collect();
        let _gate = self.gate.lock().await;
        let sections = parsed.sections;
        let (outcome, mut refused, stored) = {
            let label = label.clone();
            self.db(move |s| {
                let entries = entries_of(s, scope)?;
                let by_id: HashMap<&str, &st::EntryView> = entries
                    .active
                    .iter()
                    .map(|view| (view.entry.id.as_str(), view))
                    .collect();
                let mut wants: Vec<Want> = Vec::new();
                let mut refused = Vec::new();
                let mut unchanged = 0_u32;
                for section in &sections {
                    let mut refuse = |reason: String| {
                        refused.push(MemoryFileProblem {
                            file: label.clone(),
                            section: Some(section.label()),
                            reason,
                            at_ms: now,
                        });
                    };
                    let Some(category) = section.category else {
                        refuse("no category: put it under a `# Profile`, `# Facts` or `# Notes` heading, or choose one".to_owned());
                        continue;
                    };
                    let body = match clean_body(&section.body) {
                        Ok(body) => body,
                        Err(e) => {
                            refuse(e.message);
                            continue;
                        }
                    };
                    let category = db_category(category);
                    let target = section.id.as_deref().and_then(|id| by_id.get(id));
                    let want = match target {
                        Some(view) if norm(&view.current.body) == norm(&body) => {
                            unchanged += 1;
                            continue;
                        }
                        Some(view) => Want {
                            op: st::Operation::Update,
                            entry_id: Some(view.entry.id.clone()),
                            expected: Some(view.entry.current_revision),
                            category: view.entry.category,
                            body,
                        },
                        None => {
                            if entries.active.iter().any(|view| {
                                view.entry.category == category
                                    && norm(&view.current.body) == norm(&body)
                            }) || wants.iter().any(|w| {
                                w.op == st::Operation::Create
                                    && w.category == category
                                    && norm(&w.body) == norm(&body)
                            }) {
                                unchanged += 1;
                                continue;
                            }
                            Want {
                                op: st::Operation::Create,
                                entry_id: None,
                                expected: None,
                                category,
                                body,
                            }
                        }
                    };
                    wants.push(want);
                }
                let pending = s.memory().proposals(0)?;
                let pending: Vec<st::ProposalRow> =
                    pending.into_iter().filter(|row| row.scope == scope).collect();
                let plan = plan(&wants, &pending, false);
                let stored = store(s, scope, &plan, ORIGIN_IMPORT, now)?;
                Ok(((plan.covered + unchanged), refused, stored))
            })
            .await?
        };
        skipped.append(&mut refused);
        let created = stored
            .iter()
            .filter(|(want, _)| want.op == st::Operation::Create)
            .count();
        let updated = stored.len() - created;
        info!(
            target: "hutan::audit", actor = "user", action = "memory.import", client = client_label(client),
            created, updated, skipped = skipped.len(), "memory entries imported as proposals"
        );
        if !stored.is_empty() {
            let revision = self
                .db(move |s| Ok(s.memory().scope_revision(scope)?))
                .await?;
            self.publish_changed(scope, revision);
        }
        let proposals = stored
            .iter()
            .filter_map(|(_, id)| id.parse::<ApprovalId>().ok())
            .collect();
        Ok(MemoryImported {
            created: u32::try_from(created).unwrap_or(u32::MAX),
            updated: u32::try_from(updated).unwrap_or(u32::MAX),
            unchanged: outcome,
            proposals,
            skipped,
        })
    }
}

/// Starts the watcher task and reads the files once (the daemon is starting).
pub(super) fn start(service: &Arc<MemoryService>) {
    let (tx, mut rx) = mpsc::channel::<PathBuf>(SIGNALS);
    if service.files.wake.set(tx).is_err() {
        return;
    }
    let me = Arc::downgrade(service);
    tokio::spawn(async move {
        while let Some(first) = rx.recv().await {
            tokio::time::sleep(DEBOUNCE).await;
            let mut folders = vec![first];
            while let Ok(more) = rx.try_recv() {
                if !folders.contains(&more) {
                    folders.push(more);
                }
            }
            let Some(service) = me.upgrade() else { break };
            for folder in folders {
                service.sync_folder(&folder).await;
            }
        }
    });
    restart(service);
}

/// Reads and writes the files of every scope again (the setting changed).
pub(super) fn restart(service: &MemoryService) {
    let Some(service) = service.me.upgrade() else {
        return;
    };
    tokio::spawn(async move { service.bootstrap().await });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn want(op: st::Operation, entry: Option<&str>, expected: Option<u64>, body: &str) -> Want {
        Want {
            op,
            entry_id: entry.map(str::to_owned),
            expected,
            category: st::Category::Fact,
            body: body.to_owned(),
        }
    }

    fn row(id: &str, want: &Want) -> st::ProposalRow {
        st::ProposalRow {
            id: id.to_owned(),
            operation_id: format!("op-{id}"),
            scope: st::Scope::Global,
            operation: want.op,
            entry_id: want.entry_id.clone(),
            expected_revision: want.expected,
            category: want.category,
            body: want.body.clone(),
            provenance_json: String::new(),
            state: st::ProposalState::Pending,
            origin_chat_session_id: None,
            created_ms: 0,
            resolved_ms: None,
        }
    }

    #[test]
    fn a_plan_keeps_what_still_stands_and_withdraws_the_rest() {
        let kept = want(st::Operation::Update, Some("A"), Some(2), "new  \n");
        let stale = want(st::Operation::Update, Some("B"), Some(1), "old");
        let fresh = want(st::Operation::Create, None, None, "brand new");
        let pending = vec![row("1", &kept), row("2", &stale)];
        let planned = plan(&[kept, fresh.clone()], &pending, true);
        assert_eq!(
            planned,
            Plan {
                covered: 1,
                cancel: vec!["2".to_owned()],
                insert: vec![fresh],
            }
        );
    }

    #[test]
    fn an_import_plan_never_withdraws() {
        let other = want(st::Operation::Update, Some("B"), Some(1), "x");
        let planned = plan(&[], &[row("1", &other)], false);
        assert!(planned.cancel.is_empty() && planned.insert.is_empty());
    }

    #[test]
    fn bodies_compare_without_trailing_blanks_and_line_endings() {
        assert_eq!(norm("a  \r\nb\n"), norm("a\nb"));
    }
}
