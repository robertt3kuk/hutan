//! Reading the transcript of a terminal agent for its insight (INSIGHT-C, `docs/integrations.md`
//! § Insight, `docs/history-index.md` § Privacy).
//!
//! Claude Code's and Codex's hooks carry no tokens, context or cost; their transcripts carry all
//! of it. This module finds the transcript of one terminal session ([`resolve`]) and says how
//! sure it is of the match ([`Route`]); `crate::insight` tails the file and folds what it says
//! into the session's insight.
//!
//! Consent first: nothing here runs unless the user turned `[history]` on for the agent's source
//! ([`crate::history::HistoryService::transcript_access`]), and no file outside that source's
//! folder is ever opened.
//!
//! Everything in this file is pure (the catalog comes in as rows), so the rules are tested
//! without a daemon: above all that a transcript is never attached to the wrong session.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use hutan_index::Source;

/// A catalog row older than the agent by more than this still counts as written after it
/// started (the daemon notices a program a moment after it began).
pub const START_SLACK_MS: u64 = 10_000;

/// What the registry knows of a terminal agent that finding its transcript needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TailHints {
    /// The agent's own id of its conversation, as its hooks reported it (Claude Code's and
    /// Codex's `session_id`: the transcript's file name, resp. the rollout's `session_meta.id`).
    pub agent_session_id: Option<String>,
    /// The transcript file the agent's hooks named (Claude Code's `transcript_path`).
    pub transcript_path: Option<String>,
    /// When the agent started in this session, as far as the daemon knows.
    pub started_ms: Option<u64>,
    /// Other live terminal sessions running the same agent in the same folder: with them, the
    /// folder alone says nothing about whose transcript a file is.
    pub rivals: u32,
    /// The session's process runs.
    pub live: bool,
}

/// How a transcript was matched to a session, and how sure that is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The agent's hook named the file and the conversation id agrees with its name. Certain.
    HookPath,
    /// The conversation id the agent's hooks reported, found in the history catalog. Certain:
    /// an id is one conversation.
    SessionId,
    /// No id is known: the only transcript of the agent's folder written since the agent
    /// started, with nobody else running the same agent there. Likely, not certain.
    Folder,
}

impl Route {
    /// The route in the words of `hutan integration status` and the docs.
    pub fn label(self) -> &'static str {
        match self {
            Self::HookPath => "the transcript path of the agent's hook",
            Self::SessionId => "the agent's session id, found in the history catalog",
            Self::Folder => "the only transcript of the folder written since the agent started",
        }
    }

    /// `certain` or `likely`.
    pub fn sureness(self) -> &'static str {
        match self {
            Self::HookPath | Self::SessionId => "certain",
            Self::Folder => "likely",
        }
    }
}

/// Why no transcript is attached to a session (yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The agent has no transcript Hutan reads (OMP reports through its extension).
    Unsupported,
    /// The conversation id is known but the history index has not read that transcript yet.
    NotIndexed,
    /// No id, no path and no transcript of the folder written since the agent started.
    NoCandidate,
    /// No id and more than one transcript of the folder fits.
    Ambiguous(usize),
    /// No id, and another session runs the same agent in the same folder.
    Rivals,
    /// The agent named a file outside the folder the user allowed.
    OutsideRoot,
    /// The path the agent named belongs to another conversation than the id it reported.
    Mismatch,
}

impl Why {
    /// A reason for a person.
    pub fn text(&self) -> String {
        match self {
            Self::Unsupported => "this agent keeps no transcript Hutan reads".to_owned(),
            Self::NotIndexed => "the history index has not read its transcript yet".to_owned(),
            Self::NoCandidate => {
                "no transcript of its folder was written since it started".to_owned()
            }
            Self::Ambiguous(n) => format!(
                "{n} transcripts of its folder were written since it started and its hooks name none"
            ),
            Self::Rivals => {
                "another session runs the same agent in the same folder and its hooks name no transcript"
                    .to_owned()
            }
            Self::OutsideRoot => {
                "the transcript its hook named is outside the folder allowed for history".to_owned()
            }
            Self::Mismatch => {
                "the transcript its hook named belongs to another conversation".to_owned()
            }
        }
    }
}

/// The transcript of a session, or why there is none yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Found {
        path: PathBuf,
        /// The agent's id of the conversation (the transcript's key).
        key: Option<String>,
        route: Route,
    },
    /// The decision needs the history catalog's rows; ask again with them.
    NeedsCatalog,
    Missing(Why),
}

/// One transcript the history index knows (a [`hutan_index::CatalogRow`] reduced to what
/// matching needs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub key: String,
    pub path: PathBuf,
    pub cwd: Option<String>,
    /// When the last message in it was written.
    pub updated_ms: Option<u64>,
}

impl Candidate {
    /// `None` for a row without a transcript file.
    pub fn of(row: &hutan_index::CatalogRow) -> Option<Self> {
        Some(Self {
            key: row.key.clone(),
            path: PathBuf::from(row.path.as_deref()?),
            cwd: row.cwd.clone(),
            updated_ms: row.updated_ms.or(row.modified_ms),
        })
    }
}

/// The source whose transcripts an agent writes, when Hutan reads them for insight.
pub fn source_of(agent: &str) -> Option<Source> {
    match agent {
        "claude" => Some(Source::Claude),
        "codex" => Some(Source::Codex),
        _ => None,
    }
}

/// Whether `path` is a file below `root` — and stays below it when it is a link. A path with
/// `..` or a relative one is never inside.
pub fn inside(root: &Path, path: &Path) -> bool {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return false;
    }
    let canonical_root = root.canonicalize().ok();
    let lexically = path.starts_with(root)
        || canonical_root
            .as_deref()
            .is_some_and(|root| path.starts_with(root));
    if !lexically {
        return false;
    }
    match path.canonicalize() {
        // The file exists: where it really is decides.
        Ok(real) => canonical_root
            .as_deref()
            .is_some_and(|root| real.starts_with(root)),
        // Not written yet (the agent creates it with its first message).
        Err(_) => true,
    }
}

fn trim_slash(dir: &str) -> &str {
    if dir.len() > 1 {
        dir.trim_end_matches('/')
    } else {
        dir
    }
}

/// Whether two folder names are one folder: equal text, or equal once links are resolved.
pub fn same_folder(a: &str, b: &str) -> bool {
    let (a, b) = (trim_slash(a), trim_slash(b));
    if a == b {
        return true;
    }
    // Only folders with the same name can be one folder through a link (`/var` →
    // `/private/var`): the file system is not asked about the rest.
    if Path::new(a).file_name() != Path::new(b).file_name() {
        return false;
    }
    match (Path::new(a).canonicalize(), Path::new(b).canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// What [`resolve`] reads.
#[derive(Debug)]
pub struct Lookup<'a> {
    pub agent: &'a str,
    pub hints: &'a TailHints,
    /// The session's folder.
    pub cwd: Option<&'a str>,
    /// The folder the source's transcripts are below.
    pub root: &'a Path,
    /// The catalog's rows of the agent's source; `None` while they were not asked for.
    pub rows: Option<&'a [Candidate]>,
    /// Transcripts that belong to another session already.
    pub claimed: &'a HashSet<PathBuf>,
}

/// Finds the transcript of the session `lookup` describes.
///
/// In order of how sure the match is:
/// 1. the path the agent's hook named, when it is inside `root` and its file name is the
///    conversation id the hooks reported ([`Route::HookPath`]);
/// 2. the conversation id, looked up in the catalog ([`Route::SessionId`]);
/// 3. with neither: the one transcript of the session's folder that was written since the agent
///    started, when nobody else runs the same agent in that folder ([`Route::Folder`]).
///
/// A transcript another session has claimed is never offered; with an id or a path the answer
/// never depends on the folder.
pub fn resolve(lookup: &Lookup<'_>) -> Resolution {
    let Some(source) = source_of(lookup.agent) else {
        return Resolution::Missing(Why::Unsupported);
    };
    let hints = lookup.hints;
    let id = hints.agent_session_id.as_deref();
    if let Some(named) = hints.transcript_path.as_deref() {
        let path = PathBuf::from(named);
        if !inside(lookup.root, &path) {
            return Resolution::Missing(Why::OutsideRoot);
        }
        // Claude Code's file is `<conversation id>.jsonl`.
        let stem = path.file_stem().and_then(|stem| stem.to_str());
        if source == Source::Claude && id.is_some() && stem != id {
            return Resolution::Missing(Why::Mismatch);
        }
        if lookup.claimed.contains(&path) {
            return Resolution::Missing(Why::NoCandidate);
        }
        return Resolution::Found {
            key: id.map(str::to_owned).or_else(|| stem.map(str::to_owned)),
            path,
            route: Route::HookPath,
        };
    }
    let Some(rows) = lookup.rows else {
        return Resolution::NeedsCatalog;
    };
    if let Some(id) = id {
        return match rows
            .iter()
            .find(|row| row.key == id && inside(lookup.root, &row.path))
        {
            Some(row) if !lookup.claimed.contains(&row.path) => Resolution::Found {
                path: row.path.clone(),
                key: Some(row.key.clone()),
                route: Route::SessionId,
            },
            Some(_) => Resolution::Missing(Why::NoCandidate),
            None => Resolution::Missing(Why::NotIndexed),
        };
    }
    if hints.rivals > 0 {
        return Resolution::Missing(Why::Rivals);
    }
    let (Some(cwd), Some(started)) = (lookup.cwd, hints.started_ms) else {
        return Resolution::Missing(Why::NoCandidate);
    };
    let fits: Vec<&Candidate> = rows
        .iter()
        .filter(|row| row.cwd.as_deref().is_some_and(|dir| same_folder(dir, cwd)))
        .filter(|row| {
            row.updated_ms
                .is_some_and(|at| at.saturating_add(START_SLACK_MS) >= started)
        })
        .filter(|row| !lookup.claimed.contains(&row.path) && inside(lookup.root, &row.path))
        .collect();
    match fits.as_slice() {
        [] => Resolution::Missing(Why::NoCandidate),
        [only] => Resolution::Found {
            path: only.path.clone(),
            key: Some(only.key.clone()),
            route: Route::Folder,
        },
        many => Resolution::Missing(Why::Ambiguous(many.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/t/claude/projects";

    fn row(key: &str, cwd: &str, updated: u64) -> Candidate {
        Candidate {
            key: key.to_owned(),
            path: PathBuf::from(format!("{ROOT}/-w-app/{key}.jsonl")),
            cwd: Some(cwd.to_owned()),
            updated_ms: Some(updated),
        }
    }

    fn hints(id: Option<&str>, path: Option<&str>, started: u64, rivals: u32) -> TailHints {
        TailHints {
            agent_session_id: id.map(str::to_owned),
            transcript_path: path.map(str::to_owned),
            started_ms: Some(started),
            rivals,
            live: true,
        }
    }

    fn find(
        agent: &str,
        hints: &TailHints,
        rows: Option<&[Candidate]>,
        claimed: &HashSet<PathBuf>,
    ) -> Resolution {
        resolve(&Lookup {
            agent,
            hints,
            cwd: Some("/w/app"),
            root: Path::new(ROOT),
            rows,
            claimed,
        })
    }

    fn found(path: &str, key: &str, route: Route) -> Resolution {
        Resolution::Found {
            path: PathBuf::from(path),
            key: Some(key.to_owned()),
            route,
        }
    }

    #[test]
    fn the_path_a_hook_named_is_certain_and_needs_no_catalog() {
        let path = format!("{ROOT}/-w-app/s1.jsonl");
        let none = HashSet::new();
        assert_eq!(
            find("claude", &hints(Some("s1"), Some(&path), 0, 0), None, &none),
            found(&path, "s1", Route::HookPath)
        );
        // Even when another session runs the same agent in the same folder.
        assert_eq!(
            find("claude", &hints(Some("s1"), Some(&path), 0, 3), None, &none),
            found(&path, "s1", Route::HookPath)
        );
    }

    #[test]
    fn a_path_outside_the_allowed_folder_or_of_another_conversation_is_refused() {
        let none = HashSet::new();
        assert_eq!(
            find(
                "claude",
                &hints(Some("s1"), Some("/etc/passwd"), 0, 0),
                None,
                &none
            ),
            Resolution::Missing(Why::OutsideRoot)
        );
        let sneaky = format!("{ROOT}/../elsewhere/s1.jsonl");
        assert_eq!(
            find(
                "claude",
                &hints(Some("s1"), Some(&sneaky), 0, 0),
                None,
                &none
            ),
            Resolution::Missing(Why::OutsideRoot)
        );
        let other = format!("{ROOT}/-w-app/s2.jsonl");
        assert_eq!(
            find(
                "claude",
                &hints(Some("s1"), Some(&other), 0, 0),
                None,
                &none
            ),
            Resolution::Missing(Why::Mismatch)
        );
    }

    #[test]
    fn a_session_id_is_looked_up_in_the_catalog() {
        let rows = [row("s1", "/w/app", 100), row("s2", "/w/app", 200)];
        let none = HashSet::new();
        let h = hints(Some("s2"), None, 0, 0);
        assert_eq!(find("codex", &h, None, &none), Resolution::NeedsCatalog);
        assert_eq!(
            find("codex", &h, Some(&rows), &none),
            found(&format!("{ROOT}/-w-app/s2.jsonl"), "s2", Route::SessionId)
        );
        assert_eq!(
            find("codex", &hints(Some("s9"), None, 0, 0), Some(&rows), &none),
            Resolution::Missing(Why::NotIndexed),
            "an id the index has not read yet waits; the folder is not guessed from"
        );
    }

    #[test]
    fn two_agents_in_one_folder_never_get_each_others_transcript() {
        // Two Claude Codes in /w/app, one transcript each, both written after both started.
        let rows = [row("a", "/w/app", 1_000), row("b", "/w/app", 1_100)];
        let none = HashSet::new();
        // With their ids each gets its own.
        assert_eq!(
            find(
                "claude",
                &hints(Some("a"), None, 500, 1),
                Some(&rows),
                &none
            ),
            found(&format!("{ROOT}/-w-app/a.jsonl"), "a", Route::SessionId)
        );
        assert_eq!(
            find(
                "claude",
                &hints(Some("b"), None, 500, 1),
                Some(&rows),
                &none
            ),
            found(&format!("{ROOT}/-w-app/b.jsonl"), "b", Route::SessionId)
        );
        // Without ids the folder says nothing: the rival is what stops the guess, and so would
        // the two candidates.
        assert_eq!(
            find("claude", &hints(None, None, 500, 1), Some(&rows), &none),
            Resolution::Missing(Why::Rivals)
        );
        assert_eq!(
            find("claude", &hints(None, None, 500, 0), Some(&rows), &none),
            Resolution::Missing(Why::Ambiguous(2))
        );
    }

    #[test]
    fn the_only_transcript_written_since_the_agent_started_is_a_likely_match() {
        let rows = [
            row("old", "/w/app", 1_000),
            row("elsewhere", "/w/other", 90_000),
            row("mine", "/w/app/", 90_000),
        ];
        let none = HashSet::new();
        assert_eq!(
            find("claude", &hints(None, None, 80_000, 0), Some(&rows), &none),
            found(&format!("{ROOT}/-w-app/mine.jsonl"), "mine", Route::Folder)
        );
        assert_eq!(Route::Folder.sureness(), "likely");
        // Nothing written since the agent started.
        assert_eq!(
            find("claude", &hints(None, None, 200_000, 0), Some(&rows), &none),
            Resolution::Missing(Why::NoCandidate)
        );
    }

    #[test]
    fn a_transcript_another_session_has_is_not_offered() {
        let rows = [row("mine", "/w/app", 90_000)];
        let claimed: HashSet<PathBuf> =
            HashSet::from([PathBuf::from(format!("{ROOT}/-w-app/mine.jsonl"))]);
        assert_eq!(
            find(
                "claude",
                &hints(None, None, 80_000, 0),
                Some(&rows),
                &claimed
            ),
            Resolution::Missing(Why::NoCandidate)
        );
        assert_eq!(
            find(
                "claude",
                &hints(Some("mine"), None, 80_000, 0),
                Some(&rows),
                &claimed
            ),
            Resolution::Missing(Why::NoCandidate)
        );
    }

    #[test]
    fn an_agent_without_transcripts_has_none() {
        let none = HashSet::new();
        assert_eq!(
            find("omp", &hints(None, None, 0, 0), Some(&[]), &none),
            Resolution::Missing(Why::Unsupported)
        );
    }

    #[test]
    fn inside_refuses_relative_dotted_and_foreign_paths() {
        let root = Path::new("/t/r");
        assert!(inside(root, Path::new("/t/r/a/b.jsonl")));
        assert!(!inside(root, Path::new("/t/other/b.jsonl")));
        assert!(!inside(root, Path::new("/t/r/../b.jsonl")));
        assert!(!inside(root, Path::new("b.jsonl")));
        assert!(!inside(root, Path::new("/t/rx/b.jsonl")));
    }
}
