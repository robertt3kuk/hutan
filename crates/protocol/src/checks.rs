//! Findings: what is wrong in a session or a workspace (protocol 0.72, capability
//! `agents.findings`, ADR 0025; `docs/protocol.md` § Findings, `docs/assistant.md` § Checks).
//!
//! A **finding** is a deterministic, evidence-backed statement produced by a named **detector**:
//! the turn ended "done" after its last test command failed, test files were edited, a test was
//! skipped, the same failure came three times, two agents changed the same file. No model call
//! produces a finding. Each carries its **confidence** — `exact` (it rests on a record: an exit
//! code, a diff line, a count) or `heuristic` (it rests on a pattern, and says so) — and at most
//! five pieces of evidence. The answer also carries the **coverage** of every session in scope:
//! which detectors ran and which could not, and why, so that "no findings" is never mistaken for
//! "nothing wrong".
//!
//! `agents.findings` is a read: bounded ([`FINDINGS_MAX`] findings, [`FINDINGS_MAX_BYTES`]),
//! redacted, text of agents marked as data. `agents.findings.dismiss` is the user's alone: an
//! agent, Mawas included, cannot hide a problem.

use hutan_core::{SessionId, TurnId, WorkspaceId};
use serde::{Deserialize, Serialize};

use crate::review::NotRecorded;

/// The most findings one answer lists.
pub const FINDINGS_MAX: usize = 200;
/// The findings listed when the request names no `limit`.
pub const FINDINGS_DEFAULT_LIMIT: u32 = 100;
/// The most bytes of JSON one answer takes: findings are cut, least important first, until it fits.
pub const FINDINGS_MAX_BYTES: usize = 24 * 1024;
/// The most pieces of evidence one finding lists.
pub const FINDING_EVIDENCE_MAX: usize = 5;
/// The most characters of one excerpt of evidence.
pub const FINDING_EXCERPT_CHARS: usize = 200;
/// The most characters of a finding's title.
pub const FINDING_TITLE_CHARS: usize = 120;
/// The most sessions whose coverage one answer lists.
pub const FINDINGS_COVERAGE_MAX: usize = 60;

/// The ids of the detectors: a closed list the daemon grows by one id per detector.
pub mod detector {
    /// The turn ended successfully after its last test-like command failed.
    pub const DONE_AFTER_FAILED_COMMAND: &str = "done_after_failed_command";
    /// The final message claims tests or a build pass and no such command exited 0.
    pub const CLAIMED_PASS_WITHOUT_TEST: &str = "claimed_pass_without_test";
    /// Test files changed in a turn that also changed non-test code.
    pub const TESTS_EDITED: &str = "tests_edited";
    /// The diff skips or focuses a test, or removes more assertions than it adds.
    pub const TESTS_WEAKENED: &str = "tests_weakened";
    /// Snapshot, golden or fixture files were rewritten.
    pub const SNAPSHOT_UPDATED: &str = "snapshot_updated";
    /// Files changed and no command that looks like a test ran.
    pub const NO_TEST_COMMAND: &str = "no_test_command";
    /// A lockfile or a package manifest changed.
    pub const DEPENDENCY_FILE_CHANGED: &str = "dependency_file_changed";
    /// A file was deleted.
    pub const FILE_DELETED: &str = "file_deleted";
    /// The agent changed a path outside the session's folders.
    pub const PATH_OUTSIDE_FOLDER: &str = "path_outside_folder";
    /// A changed path has the name of a likely secret.
    pub const SECRET_LIKE_PATH: &str = "secret_like_path";
    /// More lines added plus removed than the large-diff threshold.
    pub const LARGE_DIFF: &str = "large_diff";
    /// The same failure again and again.
    pub const REPEATED_FAILURE: &str = "repeated_failure";
    /// The same file changed back and forth.
    pub const THRASH: &str = "thrash";
    /// The context is nearly full, or the harness compacted it.
    pub const CONTEXT_PRESSURE: &str = "context_pressure";
    /// A turn spent far more tokens than the session's usual.
    pub const COSTLY_TURN: &str = "costly_turn";
    /// The agent waited for the user for long.
    pub const LONG_WAIT: &str = "long_wait";
    /// Working with no event for long.
    pub const STALLED: &str = "stalled";
    /// Finished turns with changes nobody reviewed or merged.
    pub const UNREVIEWED_CHANGES: &str = "unreviewed_changes";
    /// Two live sessions changed the same file.
    pub const OVERLAP: &str = "overlap";
    /// Two agents write to one checkout.
    pub const SHARED_CHECKOUT: &str = "shared_checkout";
    /// Hutan's own run of the session's tests (`agents.verify`) failed after the agent said it
    /// was done.
    pub const VERIFY_FAILED: &str = "verify_failed";

    /// Every detector the daemon runs, in the order coverage lists them.
    pub const ALL: &[&str] = &[
        DONE_AFTER_FAILED_COMMAND,
        CLAIMED_PASS_WITHOUT_TEST,
        TESTS_EDITED,
        TESTS_WEAKENED,
        SNAPSHOT_UPDATED,
        NO_TEST_COMMAND,
        DEPENDENCY_FILE_CHANGED,
        FILE_DELETED,
        PATH_OUTSIDE_FOLDER,
        SECRET_LIKE_PATH,
        LARGE_DIFF,
        REPEATED_FAILURE,
        THRASH,
        CONTEXT_PRESSURE,
        COSTLY_TURN,
        LONG_WAIT,
        STALLED,
        UNREVIEWED_CHANGES,
        OVERLAP,
        SHARED_CHECKOUT,
        VERIFY_FAILED,
    ];
}

/// `agents.findings`: the findings of a workspace or of one session. Read-only.
///
/// With neither `workspace_id` nor `session_id` the scope is everything. `workspace_id` takes a
/// workspace with its projects (or a project, or Home); `session_id` one session (`not_found`
/// when the daemon does not list it). Both: the session must be in the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentsFindingsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Only findings that were seen again at or after this time, ms since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// Include what the user dismissed (each with `dismissed: true`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_dismissed: bool,
    /// The most findings to list, 1 to [`FINDINGS_MAX`]. Absent: [`FINDINGS_DEFAULT_LIMIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// How serious a finding is. `alert` outranks `warn`, which outranks `info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Info,
    Warn,
    Alert,
}

impl FindingSeverity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alert => "alert",
            Self::Warn => "warn",
            Self::Info => "info",
        }
    }
}

/// What a finding rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingConfidence {
    /// A record: an exit code, a diff line, a count, a timestamp.
    Exact,
    /// A pattern in words or paths; labelled so wherever it is shown.
    Heuristic,
}

impl FindingConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Heuristic => "heuristic",
        }
    }
}

/// Where one piece of evidence comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// A command of a turn, with its exit code.
    Command,
    /// A file path.
    File,
    /// A line of a diff.
    Diff,
    /// An event of the journal, the state log or the audit tables (a message, a failure).
    Event,
    /// A number the daemon recorded (tokens, minutes, counts).
    Stat,
    /// A run of Hutan's own tests (`agents.verify`).
    Verify,
}

/// One piece of evidence: what was seen and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingEvidence {
    pub kind: EvidenceKind,
    /// Where to look it up: a turn id, a file path, a session id.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// At most [`FINDING_EXCERPT_CHARS`] characters, on one line.
    pub excerpt: String,
    /// The excerpt had something that looked like a secret, and it was replaced.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
}

/// What a finding is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingScope {
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable: a hash of the detector, the session, the turn and what it is about, so the
    /// same problem keeps its id across reads (and across a daemon restart).
    pub id: String,
    /// One of [`detector`]'s ids.
    pub detector: String,
    pub severity: FindingSeverity,
    pub confidence: FindingConfidence,
    pub scope: FindingScope,
    /// Deterministic words, at most [`FINDING_TITLE_CHARS`] characters; never the agent's text.
    pub title: String,
    /// At most [`FINDING_EVIDENCE_MAX`].
    pub evidence: Vec<FindingEvidence>,
    /// When the problem began and when it was last seen, ms since the epoch.
    pub first_at_ms: u64,
    pub last_at_ms: u64,
    /// How many times it happened (turns, failures, minutes where the title says so); 1 for a
    /// single event.
    pub count: u32,
    /// The user dismissed it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dismissed: bool,
}

/// Detectors that could not run for a session, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageGap {
    pub detectors: Vec<String>,
    /// For example `terminal agent: no tool events — install the integration` or `Codex
    /// reports no cost`.
    pub why: String,
}

/// What ran over one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCoverage {
    pub session_id: SessionId,
    pub title: String,
    /// `codex`, `omp`, `claude`, `direct`, or the program of a terminal agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// A chat has a journal; a terminal agent only what it reports.
    pub chat: bool,
    /// The detectors that ran, in [`detector::ALL`]'s order.
    pub ran: Vec<String>,
    pub gaps: Vec<CoverageGap>,
}

/// How many findings of each severity are in scope (before `limit` and the size bound).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FindingCounts {
    pub alert: u32,
    pub warn: u32,
    pub info: u32,
}

/// The answer to `agents.findings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsFindings {
    pub generated_at_ms: u64,
    /// Alerts first, then warnings, then information; the newest first within each.
    pub findings: Vec<Finding>,
    /// Findings in scope that are not listed (the `limit` or the size bound): the least
    /// important ones.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: u32,
    /// Findings the user dismissed that are not listed (`include_dismissed` lists them).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dismissed_hidden: u32,
    /// The findings in scope that are not dismissed, by severity.
    pub counts: FindingCounts,
    /// Every session in scope, with which detectors ran over it and which could not.
    pub coverage: Vec<SessionCoverage>,
    /// Sessions in scope whose coverage is not listed ([`FINDINGS_COVERAGE_MAX`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub coverage_omitted: u32,
    /// What no detector can see on this daemon, in words (the state log is off, …).
    pub not_recorded: Vec<NotRecorded>,
    /// What the size bound cut, in words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cut: Vec<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// `agents.findings.dismiss`: the user dismisses a finding. User-only: an agent, Mawas
/// included, cannot hide a problem. The id is remembered (across restarts); a finding that
/// ceases and comes back later has the same id and stays dismissed. Idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsFindingsDismiss {
    /// A finding's `id`; `bad_request` when it is not the shape of one.
    pub id: String,
}

/// The answer to `agents.findings.dismiss`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingDismissed {
    pub id: String,
    /// When the user dismissed it (the first time, when asked again).
    pub dismissed_at_ms: u64,
}

/// Whether `id` has the shape of a finding's id: `chk_` and 16 lowercase hex digits.
pub fn is_finding_id(id: &str) -> bool {
    id.strip_prefix("chk_").is_some_and(|hex| {
        hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finding_id_has_a_fixed_shape() {
        assert!(is_finding_id("chk_0123456789abcdef"));
        assert!(!is_finding_id("chk_0123456789ABCDEF"));
        assert!(!is_finding_id("chk_0123"));
        assert!(!is_finding_id("0123456789abcdef"));
        assert!(!is_finding_id(""));
    }

    #[test]
    fn severities_order_alert_first() {
        assert!(FindingSeverity::Alert > FindingSeverity::Warn);
        assert!(FindingSeverity::Warn > FindingSeverity::Info);
    }

    #[test]
    fn detector_ids_are_unique_and_snake_case() {
        let mut seen = std::collections::BTreeSet::new();
        for id in detector::ALL {
            assert!(seen.insert(*id), "{id} twice");
            assert!(
                id.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{id}"
            );
        }
    }
}
