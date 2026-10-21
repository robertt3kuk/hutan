//! Restoring files to before agent turns (ADR 0019 Decision 8, spike
//! `docs/spikes/2026-10-21-checkpoints.md` §5.2): the plan. Pure: the daemon reads the trees,
//! this decides what each file gets, and the daemon writes.
//!
//! Turn `i` has a start tree `S_i` and an end tree `E_i`; to rewind before turn `k` over the
//! turns `k…n` (a restore is a turn too, from the checkout before it to the checkout after
//! it, so Undo is the same plan over that one turn):
//! - the paths are `F = ⋃ diff(S_i, E_i)`; no other file is ever touched;
//! - per path, `want` is its start in the first turn that touched it, `last` its end in the
//!   last one, `now` what the checkout holds now;
//! - `now == want`: nothing to do (left out of the plan);
//! - `now == last`, nobody else touched it: **clean**, written back to `want` (or deleted when
//!   `want` has no such file);
//! - otherwise a **conflict**, left alone unless the user names it: **after** (`now` differs
//!   from `last`: changed since the turns), **gap** (changed between two of the turns that
//!   touched it: `E_j ≠ S_j'`), **during** (the user saved it while a turn that touched it ran);
//! - **uncaptured**: a turn or the checkout now left the path out of its snapshot (too large, a
//!   secret's name, a `filter=` attribute, …): nothing to restore it from or to undo with, so it
//!   is never written. Every file a covered turn left out is listed so, touched or not;
//! - symbolic links and submodules are never written either (listed with the reason).
//!
//! The plan's digest covers every entry with the content it would write and the content it
//! would replace: a restore carries the digest of the preview it confirms, and a file that
//! changed in between changes the digest.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::checkpoint::{SkipReason, SkippedFile};
use crate::ids::CheckpointId;

/// How a tree records a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileMode {
    /// `100644`.
    Regular,
    /// `100755`.
    Executable,
    /// `120000`: the blob is the link's target.
    Symlink,
    /// `160000`: a submodule's commit.
    Gitlink,
}

impl FileMode {
    /// A tree entry's mode in octal (`100664`, an old git's group-writable file, is regular).
    pub fn from_octal(text: &str) -> Option<Self> {
        match text {
            "100644" | "100664" => Some(Self::Regular),
            "100755" => Some(Self::Executable),
            "120000" => Some(Self::Symlink),
            "160000" => Some(Self::Gitlink),
            _ => None,
        }
    }

    pub const fn as_octal(self) -> &'static str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
            Self::Symlink => "120000",
            Self::Gitlink => "160000",
        }
    }

    /// A regular file, executable or not: what a restore writes.
    pub const fn is_file(self) -> bool {
        matches!(self, Self::Regular | Self::Executable)
    }
}

/// A file as a tree records it: its mode and its blob.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TreeFile {
    pub mode: FileMode,
    /// The blob's object id (hex).
    pub oid: String,
}

impl fmt::Display for TreeFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.mode.as_octal(), self.oid)
    }
}

/// One path a turn changed: what its start and its end hold (`None`: no such file).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Change {
    pub start: Option<TreeFile>,
    pub end: Option<TreeFile>,
}

/// One turn as a restore sees it (a restore counts as a turn: the checkout before it and
/// after it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnChanges {
    pub checkpoint: CheckpointId,
    /// The paths whose start and end differ, relative to the checkout, `/`-separated.
    pub changes: BTreeMap<String, Change>,
    /// What its snapshots left out.
    pub skipped: Vec<SkippedFile>,
    /// The paths the user saved while it ran.
    pub during: BTreeSet<String>,
}

impl TurnChanges {
    pub fn new(checkpoint: CheckpointId) -> Self {
        Self {
            checkpoint,
            changes: BTreeMap::new(),
            skipped: Vec::new(),
            during: BTreeSet::new(),
        }
    }
}

/// The checkout now, from a fresh snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Present {
    /// The files of the snapshot's tree: at least every path the turns changed (a path that
    /// is not here does not exist, unless `skipped` names it).
    pub files: BTreeMap<String, TreeFile>,
    /// What the snapshot left out.
    pub skipped: Vec<SkippedFile>,
}

/// What a restore does to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreAction {
    /// Write the file as it was before the turns (it is created when it is missing).
    Write,
    /// Delete the file: the turns created it.
    Delete,
    /// Leave it alone (`reason` says why).
    Skip,
}

impl RestoreAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Delete => "delete",
            Self::Skip => "skip",
        }
    }
}

/// Where a file stands against the turns being rewound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreState {
    /// As the last turn left it, and nobody else touched it in between: restored.
    Clean,
    /// Changed since the turns (by the user or another agent): restoring overwrites that.
    After,
    /// Changed between two of the turns that touched it.
    Gap,
    /// The user saved it while a turn that touched it ran.
    During,
    /// A snapshot left it out: there is nothing to restore it from, nor to undo with.
    Uncaptured,
}

impl RestoreState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::After => "after",
            Self::Gap => "gap",
            Self::During => "during",
            Self::Uncaptured => "uncaptured",
        }
    }

    /// Restoring it overwrites a change that is not the turns': only when the user names it.
    pub const fn is_conflict(self) -> bool {
        matches!(self, Self::After | Self::Gap | Self::During)
    }
}

/// One file of a restore's plan, as the preview shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreEntry {
    /// Relative to the checkout.
    pub path: String,
    pub action: RestoreAction,
    pub state: RestoreState,
    /// Why it is not clean, or why it is skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// A conflict: what naming it in `accept_conflicts` does (`write` or `delete`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept: Option<RestoreAction>,
}

/// One file of a plan with the content it gets and the content it replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub entry: RestoreEntry,
    /// What a write puts there (`None` for a delete, and for most skips).
    pub want: Option<TreeFile>,
    /// What the checkout holds there now (`None`: no file, or not captured).
    pub now: Option<TreeFile>,
}

/// What a restore will do, file by file, sorted by path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    pub files: Vec<PlannedFile>,
    /// SHA-256 (hex) over every entry with `want` and `now`: what a restore confirms.
    pub digest: String,
}

/// A path named in `accept_conflicts` that the plan cannot accept.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcceptError {
    #[error("`{0}` is not in the plan: the turns did not change it, or it is as it was")]
    NotInPlan(String),
    #[error("`{0}` is not a conflict: it is restored anyway")]
    NotAConflict(String),
    #[error("`{path}` cannot be restored: {reason}")]
    Refused { path: String, reason: String },
}

impl RestorePlan {
    /// The entries as the preview shows them.
    pub fn entries(&self) -> Vec<RestoreEntry> {
        self.files.iter().map(|f| f.entry.clone()).collect()
    }

    /// Whether the plan writes or deletes anything.
    pub fn changes_anything(&self) -> bool {
        self.files
            .iter()
            .any(|f| f.entry.action != RestoreAction::Skip)
    }

    /// Turns the conflicts named in `paths` into what accepting them does. Nothing changes when
    /// one of them cannot be accepted.
    pub fn accept(&mut self, paths: &[String]) -> Result<(), AcceptError> {
        let mut picked = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(at) = self.files.iter().position(|f| f.entry.path == *path) else {
                return Err(AcceptError::NotInPlan(path.clone()));
            };
            let entry = &self.files[at].entry;
            if !entry.state.is_conflict() {
                return Err(AcceptError::NotAConflict(path.clone()));
            }
            let Some(action) = entry.accept else {
                return Err(AcceptError::Refused {
                    path: path.clone(),
                    reason: entry.reason.clone().unwrap_or_default(),
                });
            };
            picked.push((at, action));
        }
        for (at, action) in picked {
            self.files[at].entry.action = action;
        }
        Ok(())
    }
}

impl SkipReason {
    /// The reason in a few words.
    pub fn describe(&self) -> String {
        match self {
            Self::TooLarge => "too large".to_owned(),
            Self::OverBudget => "past the snapshot's byte budget".to_owned(),
            Self::TooManyFiles => "past the snapshot's file count".to_owned(),
            Self::SecretName => "its name looks like a secret's".to_owned(),
            Self::Filter { driver } => format!("a `filter={driver}` attribute"),
            Self::NestedRepository => "a nested repository or submodule".to_owned(),
            Self::NotRegular => "not a regular file".to_owned(),
            Self::Unreadable => "unreadable".to_owned(),
        }
    }
}

/// Whether `path` is `scope` or inside it.
fn within(path: &str, scope: &str) -> bool {
    let scope = scope.trim_end_matches('/');
    path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The plan to rewind the checkout before the first of `turns` (oldest first) through the last,
/// from the checkout `now`; only paths within `scope` (files or directories) when it is not
/// empty.
pub fn restore_plan(turns: &[TurnChanges], now: &Present, scope: &[String]) -> RestorePlan {
    let in_scope = |path: &str| scope.is_empty() || scope.iter().any(|s| within(path, s));
    // What the turns' snapshots left out, the earliest reason first; then what the checkout's
    // left out (only where it matters: on a path the turns changed).
    let mut left_out: BTreeMap<&str, &SkipReason> = BTreeMap::new();
    for skip in turns.iter().flat_map(|t| &t.skipped) {
        left_out.entry(skip.path.as_str()).or_insert(&skip.reason);
    }
    let left_out_now: Vec<&SkippedFile> = now.skipped.iter().collect();
    let uncaptured = |path: &str| -> Option<String> {
        left_out
            .iter()
            .find(|(skipped, _)| within(path, skipped))
            .map(|(_, reason)| reason.describe())
            .or_else(|| {
                left_out_now
                    .iter()
                    .find(|skip| within(path, &skip.path))
                    .map(|skip| format!("{} now", skip.reason.describe()))
            })
    };

    let mut paths: BTreeSet<&str> = turns
        .iter()
        .flat_map(|t| t.changes.keys().map(String::as_str))
        .filter(|p| in_scope(p))
        .collect();
    paths.extend(left_out.keys().copied().filter(|p| in_scope(p)));

    let mut files = Vec::new();
    for path in paths {
        let touching: Vec<(&TurnChanges, &Change)> = turns
            .iter()
            .filter_map(|t| t.changes.get(path).map(|c| (t, c)))
            .collect();
        let current = now.files.get(path).cloned();
        if let Some(reason) = uncaptured(path) {
            files.push(PlannedFile {
                entry: RestoreEntry {
                    path: path.to_owned(),
                    action: RestoreAction::Skip,
                    state: RestoreState::Uncaptured,
                    reason: Some(format!("not captured: {reason}")),
                    accept: None,
                },
                want: None,
                now: None,
            });
            continue;
        }
        let (Some((_, first)), Some((_, last))) = (touching.first(), touching.last()) else {
            continue;
        };
        let want = first.start.clone();
        let last = last.end.clone();
        if current == want {
            continue;
        }
        let gap = touching
            .windows(2)
            .any(|pair| pair[0].1.end != pair[1].1.start);
        let during = touching.iter().any(|(t, _)| t.during.contains(path));
        let after = current != last;
        let mut reasons = Vec::new();
        if after {
            reasons.push("changed since the turn");
        }
        if gap {
            reasons.push("changed between the turns");
        }
        if during {
            reasons.push("you saved it while the turn ran");
        }
        let state = if after {
            RestoreState::After
        } else if gap {
            RestoreState::Gap
        } else if during {
            RestoreState::During
        } else {
            RestoreState::Clean
        };
        let restores = if want.is_some() {
            RestoreAction::Write
        } else {
            RestoreAction::Delete
        };
        let link = [&want, &current, &last]
            .into_iter()
            .flatten()
            .find(|f| !f.mode.is_file())
            .map(|f| f.mode);
        let (action, accept, reason) = match link {
            Some(mode) => {
                let what = if mode == FileMode::Gitlink {
                    "a submodule: restore leaves submodules alone"
                } else {
                    "a symbolic link: restore leaves links alone"
                };
                reasons.insert(0, what);
                (RestoreAction::Skip, None, Some(reasons.join("; ")))
            }
            None if state.is_conflict() => (
                RestoreAction::Skip,
                Some(restores),
                Some(reasons.join("; ")),
            ),
            None => (restores, None, None),
        };
        files.push(PlannedFile {
            entry: RestoreEntry {
                path: path.to_owned(),
                action,
                state,
                reason,
                accept,
            },
            want,
            now: current,
        });
    }
    let digest = digest(&files);
    RestorePlan { files, digest }
}

fn digest(files: &[PlannedFile]) -> String {
    let mut hasher = Sha256::new();
    let show = |file: &Option<TreeFile>| {
        file.as_ref()
            .map_or_else(|| "-".to_owned(), ToString::to_string)
    };
    for file in files {
        let entry = &file.entry;
        for part in [
            entry.path.as_str(),
            entry.action.as_str(),
            entry.state.as_str(),
            &show(&file.want),
            &show(&file.now),
        ] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(content: &str) -> TreeFile {
        TreeFile {
            mode: FileMode::Regular,
            oid: format!("{:0>40}", content.len()) + content,
        }
    }

    fn tree(files: &[(&str, &str)]) -> BTreeMap<String, TreeFile> {
        files
            .iter()
            .map(|(path, content)| ((*path).to_owned(), blob(content)))
            .collect()
    }

    /// The turn from `start` to `end`.
    fn turn(start: &BTreeMap<String, TreeFile>, end: &BTreeMap<String, TreeFile>) -> TurnChanges {
        let mut turn = TurnChanges::new(CheckpointId::new());
        for path in start.keys().chain(end.keys()) {
            let change = Change {
                start: start.get(path).cloned(),
                end: end.get(path).cloned(),
            };
            if change.start != change.end {
                turn.changes.insert(path.clone(), change);
            }
        }
        turn
    }

    fn present(files: BTreeMap<String, TreeFile>) -> Present {
        Present {
            files,
            skipped: Vec::new(),
        }
    }

    fn summary(plan: &RestorePlan) -> Vec<(&str, RestoreAction, RestoreState)> {
        plan.files
            .iter()
            .map(|f| (f.entry.path.as_str(), f.entry.action, f.entry.state))
            .collect()
    }

    #[test]
    fn a_turn_that_edits_adds_and_deletes_is_written_back_deleted_and_recreated() {
        let start = tree(&[("a", "1"), ("b", "1"), ("c", "1")]);
        let end = tree(&[("a", "2"), ("b", "1"), ("new/d", "1")]);
        let plan = restore_plan(&[turn(&start, &end)], &present(end.clone()), &[]);
        assert_eq!(
            summary(&plan),
            [
                ("a", RestoreAction::Write, RestoreState::Clean),
                ("c", RestoreAction::Write, RestoreState::Clean),
                ("new/d", RestoreAction::Delete, RestoreState::Clean),
            ]
        );
        assert_eq!(plan.files[0].want, Some(blob("1")));
        assert!(plan.changes_anything());
        assert_eq!(plan.digest.len(), 64);
    }

    #[test]
    fn a_file_changed_after_the_turn_is_a_conflict_until_accepted() {
        let start = tree(&[("a", "1"), ("b", "1")]);
        let end = tree(&[("a", "2"), ("b", "2")]);
        let now = tree(&[("a", "3"), ("b", "2"), ("c", "user")]);
        let mut plan = restore_plan(&[turn(&start, &end)], &present(now), &[]);
        assert_eq!(
            summary(&plan),
            [
                ("a", RestoreAction::Skip, RestoreState::After),
                ("b", RestoreAction::Write, RestoreState::Clean),
            ],
            "c was never the turn's"
        );
        assert_eq!(plan.files[0].entry.accept, Some(RestoreAction::Write));
        let digest = plan.digest.clone();
        assert_eq!(
            plan.accept(&["b".to_owned()]),
            Err(AcceptError::NotAConflict("b".to_owned()))
        );
        assert_eq!(
            plan.accept(&["c".to_owned()]),
            Err(AcceptError::NotInPlan("c".to_owned()))
        );
        plan.accept(&["a".to_owned()]).unwrap();
        assert_eq!(plan.files[0].entry.action, RestoreAction::Write);
        assert_eq!(plan.digest, digest, "accepting keeps the preview's digest");
    }

    #[test]
    fn a_file_already_as_it_was_is_left_out() {
        let start = tree(&[("a", "1")]);
        let end = tree(&[("a", "2")]);
        let plan = restore_plan(&[turn(&start, &end)], &present(start.clone()), &[]);
        assert!(plan.files.is_empty());
        assert!(!plan.changes_anything());
    }

    #[test]
    fn a_chain_restores_each_path_to_its_first_turns_start_and_flags_gaps() {
        let s1 = tree(&[("a", "1"), ("b", "1")]);
        let e1 = tree(&[("a", "2"), ("b", "2")]);
        // Between the turns the user changes b; turn 2 changes a and b again.
        let s2 = tree(&[("a", "2"), ("b", "user")]);
        let e2 = tree(&[("a", "3"), ("b", "3"), ("c", "3")]);
        let turns = [turn(&s1, &e1), turn(&s2, &e2)];
        let plan = restore_plan(&turns, &present(e2.clone()), &[]);
        assert_eq!(
            summary(&plan),
            [
                ("a", RestoreAction::Write, RestoreState::Clean),
                ("b", RestoreAction::Skip, RestoreState::Gap),
                ("c", RestoreAction::Delete, RestoreState::Clean),
            ]
        );
        assert_eq!(
            plan.files[0].want,
            Some(blob("1")),
            "the first turn's start"
        );
        assert_eq!(plan.files[1].entry.accept, Some(RestoreAction::Write));
        assert!(
            plan.files[1]
                .entry
                .reason
                .as_deref()
                .unwrap()
                .contains("between"),
            "{:?}",
            plan.files[1].entry.reason
        );
    }

    #[test]
    fn a_save_during_the_turn_is_a_conflict() {
        let start = tree(&[("a", "1")]);
        let end = tree(&[("a", "2")]);
        let mut t = turn(&start, &end);
        t.during.insert("a".to_owned());
        let plan = restore_plan(&[t], &present(end), &[]);
        assert_eq!(
            summary(&plan),
            [("a", RestoreAction::Skip, RestoreState::During)]
        );
    }

    #[test]
    fn what_a_snapshot_left_out_is_listed_and_never_written() {
        let start = tree(&[("a", "1"), ("grown", "1")]);
        // The turn grew a file past the cap (its end left it out) and made a big new one.
        let end = tree(&[("a", "2")]);
        let mut t = turn(&start, &end);
        for path in ["grown", "big.bin", "vendor"] {
            t.skipped.push(SkippedFile {
                path: path.to_owned(),
                reason: if path == "vendor" {
                    SkipReason::NestedRepository
                } else {
                    SkipReason::TooLarge
                },
                bytes: None,
            });
        }
        t.changes.insert(
            "vendor/x".to_owned(),
            Change {
                start: None,
                end: Some(blob("x")),
            },
        );
        let mut now = present(end);
        now.skipped.push(SkippedFile {
            path: "a".to_owned(),
            reason: SkipReason::SecretName,
            bytes: None,
        });
        let mut plan = restore_plan(&[t], &now, &[]);
        assert_eq!(
            summary(&plan),
            [
                ("a", RestoreAction::Skip, RestoreState::Uncaptured),
                ("big.bin", RestoreAction::Skip, RestoreState::Uncaptured),
                ("grown", RestoreAction::Skip, RestoreState::Uncaptured),
                ("vendor", RestoreAction::Skip, RestoreState::Uncaptured),
                ("vendor/x", RestoreAction::Skip, RestoreState::Uncaptured),
            ]
        );
        assert_eq!(
            plan.files[1].entry.reason.as_deref(),
            Some("not captured: too large")
        );
        assert!(
            plan.files[0]
                .entry
                .reason
                .as_deref()
                .unwrap()
                .ends_with("now")
        );
        assert!(matches!(
            plan.accept(&["grown".to_owned()]),
            Err(AcceptError::NotAConflict(_))
        ));
    }

    #[test]
    fn links_and_submodules_are_skipped_with_the_reason() {
        let mut start = tree(&[("a", "1")]);
        let mut end = tree(&[("a", "2")]);
        end.insert(
            "link".to_owned(),
            TreeFile {
                mode: FileMode::Symlink,
                oid: "target".to_owned(),
            },
        );
        start.insert(
            "exec".to_owned(),
            TreeFile {
                mode: FileMode::Regular,
                oid: "x".to_owned(),
            },
        );
        end.insert(
            "exec".to_owned(),
            TreeFile {
                mode: FileMode::Executable,
                oid: "x".to_owned(),
            },
        );
        let mut plan = restore_plan(&[turn(&start, &end)], &present(end), &[]);
        assert_eq!(
            summary(&plan),
            [
                ("a", RestoreAction::Write, RestoreState::Clean),
                ("exec", RestoreAction::Write, RestoreState::Clean),
                ("link", RestoreAction::Skip, RestoreState::Clean),
            ],
            "a mode change alone is restored"
        );
        assert!(
            plan.files[2]
                .entry
                .reason
                .as_deref()
                .unwrap()
                .contains("symbolic link")
        );
        assert_eq!(
            plan.accept(&["link".to_owned()]),
            Err(AcceptError::NotAConflict("link".to_owned()))
        );
    }

    #[test]
    fn a_scope_keeps_its_files_and_directories() {
        let start = tree(&[("src/a", "1"), ("src/b", "1"), ("srcx", "1"), ("t", "1")]);
        let end = tree(&[("src/a", "2"), ("src/b", "2"), ("srcx", "2"), ("t", "2")]);
        let turns = [turn(&start, &end)];
        let plan = restore_plan(&turns, &present(end.clone()), &["src/".to_owned()]);
        let paths: Vec<&str> = plan.files.iter().map(|f| f.entry.path.as_str()).collect();
        assert_eq!(paths, ["src/a", "src/b"]);
        let plan = restore_plan(&turns, &present(end), &["t".to_owned(), "src/b".to_owned()]);
        let paths: Vec<&str> = plan.files.iter().map(|f| f.entry.path.as_str()).collect();
        assert_eq!(paths, ["src/b", "t"]);
    }

    #[test]
    fn the_digest_follows_the_checkout() {
        let start = tree(&[("a", "1"), ("b", "1")]);
        let end = tree(&[("a", "2"), ("b", "2")]);
        let turns = [turn(&start, &end)];
        let first = restore_plan(&turns, &present(end.clone()), &[]);
        assert_eq!(
            first.digest,
            restore_plan(&turns, &present(end.clone()), &[]).digest
        );
        let mut edited = end;
        edited.insert("b".to_owned(), blob("user"));
        assert_ne!(
            first.digest,
            restore_plan(&turns, &present(edited), &[]).digest
        );
    }

    #[test]
    fn wire_names() {
        let entry = RestoreEntry {
            path: "a".to_owned(),
            action: RestoreAction::Skip,
            state: RestoreState::After,
            reason: Some("changed since the turn".to_owned()),
            accept: Some(RestoreAction::Delete),
        };
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            serde_json::json!({"path": "a", "action": "skip", "state": "after",
                "reason": "changed since the turn", "accept": "delete"})
        );
        for mode in [
            FileMode::Regular,
            FileMode::Executable,
            FileMode::Symlink,
            FileMode::Gitlink,
        ] {
            assert_eq!(FileMode::from_octal(mode.as_octal()), Some(mode));
        }
        assert_eq!(FileMode::from_octal("040000"), None);
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;
        use proptest::test_runner::{Config, RngSeed};

        const PATHS: &[&str] = &["a", "b", "d/c", "d/e", "f"];

        fn config() -> Config {
            Config {
                cases: 512,
                failure_persistence: None,
                rng_seed: RngSeed::Fixed(0x4855_5441_4e00_0004),
                ..Config::default()
            }
        }

        /// A tree over [`PATHS`]: each absent or one of a few contents.
        fn trees() -> impl Strategy<Value = BTreeMap<String, TreeFile>> {
            proptest::collection::vec(proptest::option::of(0_u8..3), PATHS.len()).prop_map(
                |contents| {
                    PATHS
                        .iter()
                        .zip(contents)
                        .filter_map(|(path, content)| {
                            content.map(|c| ((*path).to_owned(), blob(&c.to_string())))
                        })
                        .collect()
                },
            )
        }

        /// Applies the writes and deletes of `plan` to `tree`.
        fn apply(
            plan: &RestorePlan,
            tree: &BTreeMap<String, TreeFile>,
        ) -> BTreeMap<String, TreeFile> {
            let mut out = tree.clone();
            for file in &plan.files {
                match file.entry.action {
                    RestoreAction::Write => {
                        out.insert(file.entry.path.clone(), file.want.clone().unwrap());
                    }
                    RestoreAction::Delete => {
                        out.remove(&file.entry.path);
                    }
                    RestoreAction::Skip => {}
                }
            }
            out
        }

        proptest! {
            #![proptest_config(config())]

            /// Turns with nothing between or after them: the restore is exactly the first
            /// start, and undoing it (the restore as one turn) is exactly the last end.
            #[test]
            fn an_untouched_chain_restores_its_start_and_undo_restores_its_end(
                states in proptest::collection::vec(trees(), 2..6),
            ) {
                let turns: Vec<TurnChanges> =
                    states.windows(2).map(|w| turn(&w[0], &w[1])).collect();
                let (first, last) = (&states[0], &states[states.len() - 1]);
                let plan = restore_plan(&turns, &present(last.clone()), &[]);
                prop_assert!(plan.files.iter().all(|f| f.entry.state == RestoreState::Clean));
                let restored = apply(&plan, last);
                prop_assert_eq!(&restored, first);

                let undo = restore_plan(&[turn(last, &restored)], &present(restored.clone()), &[]);
                let redone = apply(&undo, &restored);
                prop_assert_eq!(&redone, last);
                let again = restore_plan(&[turn(&restored, &redone)], &present(redone.clone()), &[]);
                prop_assert_eq!(&apply(&again, &redone), first);
            }

            /// Whatever happened between and after the turns: a path no turn changed is never
            /// touched, a conflict is never written unless accepted, a clean path gets its
            /// first start, and accepting every conflict gives the first start everywhere the
            /// turns changed.
            #[test]
            fn only_the_turns_paths_change_and_conflicts_wait_for_the_user(
                pairs in proptest::collection::vec((trees(), trees()), 1..4),
                now in trees(),
            ) {
                let turns: Vec<TurnChanges> = pairs.iter().map(|(s, e)| turn(s, e)).collect();
                let plan = restore_plan(&turns, &present(now.clone()), &[]);
                let restored = apply(&plan, &now);
                let changed: BTreeSet<&String> =
                    turns.iter().flat_map(|t| t.changes.keys()).collect();
                for path in PATHS.iter().map(|p| (*p).to_owned()) {
                    if !changed.contains(&path) {
                        prop_assert_eq!(restored.get(&path), now.get(&path));
                    }
                }
                for file in &plan.files {
                    let path = &file.entry.path;
                    let want = turns
                        .iter()
                        .find_map(|t| t.changes.get(path))
                        .and_then(|c| c.start.clone());
                    if file.entry.state.is_conflict() {
                        prop_assert_eq!(file.entry.action, RestoreAction::Skip);
                        prop_assert_eq!(restored.get(path), now.get(path));
                    } else {
                        prop_assert_eq!(restored.get(path).cloned(), want);
                    }
                }
                let mut all = plan.clone();
                let conflicts: Vec<String> = plan
                    .files
                    .iter()
                    .filter(|f| f.entry.state.is_conflict())
                    .map(|f| f.entry.path.clone())
                    .collect();
                all.accept(&conflicts).unwrap();
                let forced = apply(&all, &now);
                for path in changed {
                    let want = turns.iter().find_map(|t| t.changes.get(path)).and_then(|c| c.start.clone());
                    prop_assert_eq!(forced.get(path).cloned(), want);
                }
            }

            /// Same turns and checkout, same digest; a different file on a planned path, a
            /// different digest.
            #[test]
            fn the_digest_changes_with_the_planned_files(
                start in trees(),
                end in trees(),
                now in trees(),
                other in 0_u8..3,
            ) {
                let turns = [turn(&start, &end)];
                let plan = restore_plan(&turns, &present(now.clone()), &[]);
                prop_assert_eq!(&plan.digest, &restore_plan(&turns, &present(now.clone()), &[]).digest);
                if let Some(file) = plan.files.first() {
                    let mut edited = now.clone();
                    let replacement = blob(&format!("other {other}"));
                    edited.insert(file.entry.path.clone(), replacement);
                    let changed = restore_plan(&turns, &present(edited), &[]);
                    prop_assert_ne!(&plan.digest, &changed.digest);
                }
            }
        }
    }
}
