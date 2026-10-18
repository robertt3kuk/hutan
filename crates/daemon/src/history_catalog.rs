//! The session catalog (`history.sessions`, protocol 0.29): what the index folded per session
//! (`hutan_index::CatalogRow`) is attributed to Hutan projects, narrowed by scope, source and
//! filter, sorted and cut to `limit`.
//!
//! - **Attribution.** A session's `cwd` belongs to the project (or Home, or workspace) with the
//!   longest root that contains it; equal roots prefer a project, then Home, then a workspace.
//!   A `cwd` inside no root but inside a linked git worktree of a project's repository belongs
//!   to that project (the checkout's git common directory identifies it). A Hutan session that
//!   is alive belongs to the workspace it runs in.
//! - **Scopes.** `project`: attributed to that id. `workspace`: the `cwd` is inside a root of
//!   the workspace or of one of its member projects (or in a worktree of a member). `all`: no
//!   restriction.
//! - **Filter.** Case-insensitive substrings, all required, over the title, first prompt, latest
//!   turns, agent, model, branch, working directory and session id; `repo:` the attributed
//!   project's name or folder name, `path:` the working directory or transcript path. Double or
//!   single quotes keep a value with spaces together. A filter over 2 KiB matches nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use hutan_core::{SessionId, WorkspaceId};
use hutan_index::{CatalogRow, ScanInfo, Source};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::history::{
    HistoryRole, HistoryScanInfo, HistoryScope, HistorySessionRow, HistorySessionSort,
    HistorySessions, HistorySessionsRequest, HistorySource, HistorySourceFiles, HistoryTurn,
    SESSIONS_MAX_LIMIT,
};
use hutan_protocol::messages::{SessionInfo, WorkspaceInfo, WorkspaceKind};

use crate::agent::validate_resume_argv;
use crate::projects;
use crate::registry::ClosedInfo;

/// A filter longer than this (bytes) matches nothing.
pub const FILTER_MAX_BYTES: usize = 2048;

/// A workspace, project or Home as the catalog sees it.
#[derive(Debug, Clone)]
struct Place {
    id: WorkspaceId,
    name: String,
    kind: WorkspaceKind,
    parent: Option<WorkspaceId>,
    roots: Vec<PathBuf>,
    common_dir: Option<PathBuf>,
}

impl Place {
    fn new(info: &WorkspaceInfo) -> Self {
        Self {
            id: info.id,
            name: info.name.clone(),
            kind: info.kind,
            parent: info.parent_id,
            roots: info.roots.clone(),
            common_dir: info
                .project
                .as_ref()
                .and_then(|project| project.common_dir.clone()),
        }
    }

    /// Lower wins a tie between equal roots.
    fn precedence(&self) -> u8 {
        match self.kind {
            WorkspaceKind::Project => 0,
            WorkspaceKind::Home => 1,
            WorkspaceKind::Workspace => 2,
        }
    }
}

/// The place a `cwd` is attributed to, and the folder that made it so.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attribution {
    id: WorkspaceId,
    name: String,
    /// The name of the folder (a root of the place) that holds the `cwd`.
    folder: String,
}

/// Where each distinct `cwd` belongs, computed once per request.
struct Resolver {
    places: Vec<Place>,
    cache: HashMap<String, Option<Attribution>>,
    common_dirs: HashMap<String, Option<PathBuf>>,
}

/// The directory `text` names, when it is absolute.
fn absolute(text: &str) -> Option<PathBuf> {
    let path = PathBuf::from(text);
    path.is_absolute().then_some(path)
}

fn folder_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| root.display().to_string())
}

/// `cwd` is `root` or below it, as written or with symlinks resolved on either side.
fn inside(root: &Path, cwd: &Path, real_cwd: &mut Option<Option<PathBuf>>) -> bool {
    if cwd.starts_with(root) {
        return true;
    }
    let real = real_cwd.get_or_insert_with(|| std::fs::canonicalize(cwd).ok());
    match real {
        Some(real) => {
            real.starts_with(root)
                || std::fs::canonicalize(root).is_ok_and(|canonical| real.starts_with(canonical))
        }
        None => false,
    }
}

impl Resolver {
    fn new(workspaces: &[WorkspaceInfo]) -> Self {
        Self {
            places: workspaces.iter().map(Place::new).collect(),
            cache: HashMap::new(),
            common_dirs: HashMap::new(),
        }
    }

    fn place(&self, id: WorkspaceId) -> Option<&Place> {
        self.places.iter().find(|place| place.id == id)
    }

    /// The git common directory of the repository around `cwd` (a worktree shares it with its
    /// main checkout), cached.
    fn common_dir(&mut self, cwd: &str) -> Option<PathBuf> {
        if let Some(known) = self.common_dirs.get(cwd) {
            return known.clone();
        }
        let found = absolute(cwd)
            .filter(|path| path.is_dir())
            .and_then(|path| projects::identify(&path))
            .and_then(|ident| ident.common_dir);
        self.common_dirs.insert(cwd.to_owned(), found.clone());
        found
    }

    fn attribute(&mut self, cwd: &str) -> Option<Attribution> {
        if let Some(known) = self.cache.get(cwd) {
            return known.clone();
        }
        let found = self.attribute_uncached(cwd);
        self.cache.insert(cwd.to_owned(), found.clone());
        found
    }

    fn attribute_uncached(&mut self, cwd: &str) -> Option<Attribution> {
        let path = absolute(cwd)?;
        let mut real = None;
        let mut best: Option<(usize, u8, &Place, &PathBuf)> = None;
        for place in &self.places {
            for root in &place.roots {
                if !inside(root, &path, &mut real) {
                    continue;
                }
                let key = (root.components().count(), place.precedence());
                let better = best.is_none_or(|(depth, precedence, _, _)| {
                    key.0 > depth || (key.0 == depth && key.1 < precedence)
                });
                if better {
                    best = Some((key.0, key.1, place, root));
                }
            }
        }
        if let Some((_, _, place, root)) = best {
            return Some(Attribution {
                id: place.id,
                name: place.name.clone(),
                folder: folder_name(root),
            });
        }
        // Not in any folder of ours: a linked worktree of a project's repository.
        let common = self.common_dir(cwd)?;
        let place = self.places.iter().find(|place| {
            place.kind == WorkspaceKind::Project && place.common_dir.as_ref() == Some(&common)
        })?;
        Some(Attribution {
            id: place.id,
            name: place.name.clone(),
            folder: place
                .roots
                .first()
                .map_or_else(String::new, |r| folder_name(r)),
        })
    }

    /// Whether `cwd` is inside a root of `workspace` or of one of its member projects, or in a
    /// worktree of one of their repositories.
    fn in_workspace(&mut self, workspace: WorkspaceId, cwd: &str) -> bool {
        let Some(path) = absolute(cwd) else {
            return false;
        };
        let members: Vec<&Place> = self
            .places
            .iter()
            .filter(|place| place.id == workspace || place.parent == Some(workspace))
            .collect();
        let mut real = None;
        if members
            .iter()
            .flat_map(|place| &place.roots)
            .any(|root| inside(root, &path, &mut real))
        {
            return true;
        }
        let common_dirs: Vec<PathBuf> = members
            .iter()
            .filter_map(|place| place.common_dir.clone())
            .collect();
        if common_dirs.is_empty() {
            return false;
        }
        self.common_dir(cwd)
            .is_some_and(|common| common_dirs.contains(&common))
    }
}

// ---- the filter --------------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Term {
    /// Anywhere in the row's text fields.
    Any(String),
    Repo(String),
    Path(String),
}

/// Splits `filter` into terms: words, `"quoted values"`, `repo:value`, `path:value` (values may
/// be quoted). Lower-cased; empty values are dropped. A quote opens only at the start of a word
/// or of a `repo:` / `path:` value, so an apostrophe inside a word is just a letter.
fn parse_filter(filter: &str) -> Vec<Term> {
    let mut terms = Vec::new();
    let mut chars = filter.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if chars.peek().is_none() {
            break;
        }
        let mut word = String::new();
        let mut quote: Option<char> = None;
        let mut started_quoted = false;
        while let Some(&c) = chars.peek() {
            match quote {
                Some(open) if c == open => {
                    quote = None;
                    chars.next();
                    continue;
                }
                Some(_) => {}
                None if c.is_whitespace() => break,
                None if (c == '"' || c == '\'')
                    && (word.is_empty()
                        || word.eq_ignore_ascii_case("repo:")
                        || word.eq_ignore_ascii_case("path:")) =>
                {
                    started_quoted |= word.is_empty();
                    quote = Some(c);
                    chars.next();
                    continue;
                }
                None => {}
            }
            word.push(c);
            chars.next();
        }
        let lower = word.to_lowercase();
        let term = match (
            started_quoted,
            lower.strip_prefix("repo:"),
            lower.strip_prefix("path:"),
        ) {
            (false, Some(value), _) => Term::Repo(value.to_owned()),
            (false, None, Some(value)) => Term::Path(value.to_owned()),
            _ => Term::Any(lower),
        };
        let empty = match &term {
            Term::Any(text) | Term::Repo(text) | Term::Path(text) => text.is_empty(),
        };
        if !empty {
            terms.push(term);
        }
    }
    terms
}

fn matches_filter(
    terms: &[Term],
    row: &CatalogRow,
    title: Option<&str>,
    cwd: Option<&str>,
    attribution: Option<&Attribution>,
) -> bool {
    if terms.is_empty() {
        return true;
    }
    // One lower-cased text of every searched field; U+0000 keeps a term from spanning two.
    let mut haystack = String::new();
    let mut push = |text: Option<&str>| {
        if let Some(text) = text {
            haystack.push_str(&text.to_lowercase());
            haystack.push('\0');
        }
    };
    push(title);
    push(row.first_prompt.as_deref());
    for turn in &row.latest {
        push(Some(&turn.text));
    }
    push(Some(row.source.as_str()));
    push(row.model.as_deref());
    push(row.branch.as_deref());
    push(cwd);
    push(Some(&row.key));
    let cwd_lower = cwd.map(str::to_lowercase);
    let path_lower = row.path.as_deref().map(str::to_lowercase);
    terms.iter().all(|term| match term {
        Term::Any(text) => haystack.contains(text.as_str()),
        Term::Repo(text) => attribution.is_some_and(|a| {
            a.name.to_lowercase().contains(text.as_str())
                || a.folder.to_lowercase().contains(text.as_str())
        }),
        Term::Path(text) => {
            cwd_lower
                .as_deref()
                .is_some_and(|c| c.contains(text.as_str()))
                || path_lower
                    .as_deref()
                    .is_some_and(|p| p.contains(text.as_str()))
        }
    })
}

// ---- the answer --------------------------------------------------------------------------------

fn source_out(source: Source) -> HistorySource {
    match source {
        Source::Hutan => HistorySource::Hutan,
        Source::Claude => HistorySource::Claude,
        Source::Codex => HistorySource::Codex,
        Source::Omp => HistorySource::Omp,
    }
}

fn source_in(source: HistorySource) -> Source {
    match source {
        HistorySource::Hutan => Source::Hutan,
        HistorySource::Claude => Source::Claude,
        HistorySource::Codex => Source::Codex,
        HistorySource::Omp => Source::Omp,
    }
}

fn role_out(role: hutan_index::Role) -> HistoryRole {
    match role {
        hutan_index::Role::User => HistoryRole::User,
        hutan_index::Role::Assistant => HistoryRole::Assistant,
        hutan_index::Role::Tool => HistoryRole::Tool,
        hutan_index::Role::Command => HistoryRole::Command,
        hutan_index::Role::Output => HistoryRole::Output,
    }
}

/// Checks a request before the index is asked; returns the limit to apply.
pub fn check_request(req: &HistorySessionsRequest) -> Result<usize, ErrorBody> {
    if req.limit == 0 {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("limit must be between 1 and {SESSIONS_MAX_LIMIT}"),
        ));
    }
    if req.scope != HistoryScope::All && req.workspace_id.is_none() {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            "workspace_id is required for the workspace and project scopes",
        ));
    }
    Ok(req.limit.min(SESSIONS_MAX_LIMIT) as usize)
}

/// What a row is sorted by (milliseconds): the time of the last (or first) message, else when
/// the transcript was last read.
fn sort_key(row: &CatalogRow, sort: HistorySessionSort) -> u64 {
    match sort {
        HistorySessionSort::Updated => row.updated_ms.or(row.modified_ms).unwrap_or(0),
        HistorySessionSort::Created => row
            .created_ms
            .or(row.updated_ms)
            .or(row.modified_ms)
            .unwrap_or(0),
    }
}

/// One row on its way to the answer.
struct Candidate {
    row: CatalogRow,
    title: Option<String>,
    cwd: Option<String>,
    attribution: Option<Attribution>,
    session_id: Option<SessionId>,
    /// The Hutan session was closed (`session.close`).
    closed: bool,
}

/// Answers `req` from the catalog `rows` of the enabled sources (`limit` as
/// [`check_request`] returned it). `workspaces` are the daemon's workspaces and projects,
/// `sessions` its own sessions and `closed` the ones it closed (a Hutan row takes the title,
/// folder and workspace of its session; a closed one is marked so).
pub fn answer(
    req: &HistorySessionsRequest,
    limit: usize,
    rows: Vec<CatalogRow>,
    scan: ScanInfo,
    workspaces: &[WorkspaceInfo],
    sessions: &[SessionInfo],
    closed: &[ClosedInfo],
) -> Result<HistorySessions, ErrorBody> {
    let mut resolver = Resolver::new(workspaces);
    if req.scope != HistoryScope::All
        && let Some(id) = req.workspace_id
        && resolver.place(id).is_none()
    {
        return Err(ErrorBody::new(
            ErrorCode::NotFound,
            format!("no workspace or project {id}"),
        ));
    }
    let loaded = rows.len() as u64;
    let live: HashMap<SessionId, &SessionInfo> = sessions.iter().map(|s| (s.id, s)).collect();
    let gone: HashMap<SessionId, &ClosedInfo> = closed.iter().map(|s| (s.id, s)).collect();
    let sources: Vec<Source> = req.sources.iter().map(|s| source_in(*s)).collect();
    let terms = if req.filter.len() > FILTER_MAX_BYTES {
        None
    } else {
        Some(parse_filter(&req.filter))
    };

    let mut matched: Vec<Candidate> = Vec::new();
    for row in rows {
        if !sources.is_empty() && !sources.contains(&row.source) {
            continue;
        }
        let Some(terms) = &terms else { continue };
        let mut title = row.title.clone();
        let mut cwd = row.cwd.clone();
        let mut session_id = None;
        let mut owner: Option<WorkspaceId> = None;
        let mut is_closed = false;
        if row.source == Source::Hutan {
            session_id = row.key.parse::<SessionId>().ok();
            if let Some(session) = session_id.and_then(|id| live.get(&id)) {
                if !session.title.trim().is_empty() {
                    title = Some(session.title.clone());
                }
                if let Some(dir) = &session.cwd {
                    cwd = Some(dir.to_string_lossy().into_owned());
                }
                owner = Some(session.workspace_id);
            } else if let Some(session) = session_id.and_then(|id| gone.get(&id)) {
                is_closed = true;
                if !session.title.trim().is_empty() {
                    title = Some(session.title.clone());
                }
                if let Some(dir) = &session.cwd {
                    cwd = Some(dir.to_string_lossy().into_owned());
                }
                owner = Some(session.workspace_id);
            }
        }
        let attribution = match owner.and_then(|id| resolver.place(id)) {
            Some(place) => Some(Attribution {
                id: place.id,
                name: place.name.clone(),
                folder: place
                    .roots
                    .first()
                    .map_or_else(String::new, |r| folder_name(r)),
            }),
            None => cwd.as_deref().and_then(|dir| resolver.attribute(dir)),
        };
        let in_scope = match (req.scope, req.workspace_id) {
            (HistoryScope::Project, Some(id)) => attribution.as_ref().is_some_and(|a| a.id == id),
            (HistoryScope::Workspace, Some(id)) => {
                owner == Some(id)
                    || attribution.as_ref().is_some_and(|a| a.id == id)
                    || cwd
                        .as_deref()
                        .is_some_and(|dir| resolver.in_workspace(id, dir))
                    || owner
                        .and_then(|o| resolver.place(o))
                        .is_some_and(|place| place.parent == Some(id))
            }
            _ => true,
        };
        if !in_scope
            || !matches_filter(
                terms,
                &row,
                title.as_deref(),
                cwd.as_deref(),
                attribution.as_ref(),
            )
        {
            continue;
        }
        matched.push(Candidate {
            row,
            title,
            cwd,
            attribution,
            session_id,
            closed: is_closed,
        });
    }

    let count = matched.len() as u64;
    matched.sort_by(|a, b| {
        sort_key(&b.row, req.sort)
            .cmp(&sort_key(&a.row, req.sort))
            .then_with(|| a.row.key.cmp(&b.row.key))
    });
    matched.truncate(limit);

    Ok(HistorySessions {
        sessions: matched.into_iter().map(row_out).collect(),
        matched: count,
        loaded,
        scan: scan_out(scan),
    })
}

fn row_out(candidate: Candidate) -> HistorySessionRow {
    let Candidate {
        row,
        title,
        cwd,
        attribution,
        session_id,
        closed,
    } = candidate;
    // The same rules as `agent.report` resume commands: a command that would not pass is not
    // offered. OMP resumes by transcript path; when that fails the rules, by id.
    let resume_argv = hutan_index::resume_argv_at(row.source, &row.key, row.path.as_deref())
        .filter(|argv| validate_resume_argv(argv).is_ok())
        .or_else(|| {
            hutan_index::resume_argv(row.source, &row.key)
                .filter(|argv| validate_resume_argv(argv).is_ok())
        });
    HistorySessionRow {
        source: source_out(row.source),
        session_key: row.key,
        title,
        cwd,
        branch: row.branch,
        model: row.model,
        messages: row.messages,
        created_ms: row.created_ms,
        updated_ms: row.updated_ms.or(row.modified_ms),
        first_prompt: row.first_prompt,
        latest: row
            .latest
            .into_iter()
            .map(|turn| HistoryTurn {
                role: role_out(turn.role),
                text: turn.text,
                ts_ms: turn.ts_ms,
            })
            .collect(),
        workspace_id: attribution.map(|a| a.id),
        resume_argv,
        path: row.path,
        session_id,
        closed,
    }
}

/// The scan state as the protocol names it.
pub fn scan_out(scan: ScanInfo) -> HistoryScanInfo {
    HistoryScanInfo {
        files: scan.files,
        done: scan.done,
        scanning: scan.scanning,
        finished_ms: scan.finished_ms,
        per_source: scan
            .per_source
            .into_iter()
            .map(|(source, files)| HistorySourceFiles {
                name: source.as_str().to_owned(),
                files,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_index::{Role, Turn};

    fn term_set(filter: &str) -> Vec<Term> {
        parse_filter(filter)
    }

    #[test]
    fn the_filter_splits_words_quotes_and_qualifiers() {
        assert_eq!(
            term_set(r#"Retry  "two words" repo:Hutan path:'a b/c' repo: x"#),
            vec![
                Term::Any("retry".into()),
                Term::Any("two words".into()),
                Term::Repo("hutan".into()),
                Term::Path("a b/c".into()),
                Term::Any("x".into()),
            ]
        );
        assert_eq!(
            term_set(r#"repo:"my repo""#),
            vec![Term::Repo("my repo".into())]
        );
        assert_eq!(
            term_set(r#"don't "repo:x""#),
            vec![Term::Any("don't".into()), Term::Any("repo:x".into())],
            "an apostrophe inside a word is a letter; a quoted qualifier is plain text"
        );
        assert!(term_set("   ").is_empty());
    }

    fn row(key: &str, cwd: &str) -> CatalogRow {
        CatalogRow {
            source: Source::Claude,
            key: key.to_owned(),
            title: Some("Fix the Build".to_owned()),
            cwd: Some(cwd.to_owned()),
            branch: Some("feature/x".to_owned()),
            model: Some("claude-opus-5".to_owned()),
            messages: 3,
            created_ms: Some(1),
            updated_ms: Some(2),
            modified_ms: None,
            first_prompt: Some("please help".to_owned()),
            latest: vec![Turn {
                role: Role::Assistant,
                text: "Patched the Makefile".to_owned(),
                ts_ms: Some(2),
            }],
            path: Some("/Users/me/.claude/projects/-work-app/s1.jsonl".to_owned()),
        }
    }

    #[test]
    fn every_term_must_match_a_field_case_insensitively() {
        let r = row("s1", "/work/app");
        let check = |filter: &str, attribution: Option<&Attribution>| {
            matches_filter(
                &parse_filter(filter),
                &r,
                r.title.as_deref(),
                r.cwd.as_deref(),
                attribution,
            )
        };
        assert!(check("", None));
        assert!(check("fix BUILD", None));
        assert!(
            check("makefile claude feature", None),
            "latest, agent, branch"
        );
        assert!(check("OPUS /work/app s1", None), "model, cwd, session id");
        assert!(!check("fix nothing", None), "all terms");
        assert!(
            !check("projects", None),
            "the transcript path is only for path:"
        );
        assert!(check("path:projects/-work", None));
        assert!(check("path:/work/app", None));
        let attribution = Attribution {
            id: WorkspaceId::new(),
            name: "Hutan Core".to_owned(),
            folder: "hutan".to_owned(),
        };
        assert!(check("repo:core", Some(&attribution)));
        assert!(check("repo:HUTAN", Some(&attribution)));
        assert!(!check("repo:other", Some(&attribution)));
        assert!(
            !check("repo:hutan", None),
            "an unattributed row has no repo"
        );
    }
}
