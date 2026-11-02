//! The detectors: one pure function per rule of `docs/spikes/2026-11-02-checks.md` § 3, over the
//! recorded facts of [`super::facts`]. A detector that cannot see what it needs does not run,
//! and [`coverage`] says so; it never guesses.
//!
//! - per turn ([`journal_drafts`]): `done_after_failed_command`, `claimed_pass_without_test`,
//!   `tests_edited`, `tests_weakened`, `snapshot_updated`, and the signs the review packet has
//!   always carried (`no_test_command`, `dependency_file_changed`, `file_deleted`,
//!   `path_outside_folder`, `secret_like_path`, `large_diff`);
//! - per session over its turns ([`journal_drafts`]): `repeated_failure`, `thrash`,
//!   `context_pressure`, `costly_turn`;
//! - per session over what changes with time ([`live_drafts`]): `long_wait`, `stalled`,
//!   `unreviewed_changes`;
//! - per workspace ([`workspace_drafts`]): `overlap`, `shared_checkout`.

use std::collections::{BTreeMap, BTreeSet};

use hutan_core::SessionId;
use hutan_core::insight::{AgentInsight, Exactness};
use hutan_protocol::checkpoint::CheckpointFileStatus;
use hutan_protocol::checks::{
    CoverageGap, EvidenceKind, FindingConfidence, FindingEvidence, FindingSeverity,
    SessionCoverage, detector,
};
use hutan_protocol::review::{
    REVIEW_LARGE_DIFF_LINES, ReviewCommand, ReviewSignalId, ReviewToolStatus,
};
use hutan_storage::ChatTurnStatus;

use super::facts::{
    ChangedFile, JournalFacts, LiveFacts, SessionMeta, TurnFacts, TurnFile, TurnFiles,
};
use super::{Draft, Thresholds, evidence, severity_of};
use crate::agents_stats::normalise;
use crate::signs::{
    ClaimKind, FileRole, SeenFile, claims_pass, file_role, is_failed, is_passed, is_source_code,
    is_unknown, looks_like_a_check, looks_like_a_test, signals, sounds_done, unrecovered_failures,
    weakening,
};

/// The detectors that read a turn's commands, message or files: they need a chat's journal.
const TURN_DETECTORS: &[&str] = &[
    detector::DONE_AFTER_FAILED_COMMAND,
    detector::CLAIMED_PASS_WITHOUT_TEST,
    detector::TESTS_EDITED,
    detector::TESTS_WEAKENED,
    detector::SNAPSHOT_UPDATED,
    detector::NO_TEST_COMMAND,
    detector::DEPENDENCY_FILE_CHANGED,
    detector::FILE_DELETED,
    detector::PATH_OUTSIDE_FOLDER,
    detector::SECRET_LIKE_PATH,
    detector::LARGE_DIFF,
    detector::REPEATED_FAILURE,
    detector::THRASH,
];
/// The detectors that need to know which files a turn changed.
const FILE_DETECTORS: &[&str] = &[
    detector::TESTS_EDITED,
    detector::TESTS_WEAKENED,
    detector::SNAPSHOT_UPDATED,
    detector::NO_TEST_COMMAND,
    detector::DEPENDENCY_FILE_CHANGED,
    detector::FILE_DELETED,
    detector::PATH_OUTSIDE_FOLDER,
    detector::SECRET_LIKE_PATH,
    detector::LARGE_DIFF,
    detector::THRASH,
];

// Words ---------------------------------------------------------------------------------------------

/// `1 h 5 min`, `42 min`.
fn duration(ms: u64) -> String {
    let minutes = ms / 60_000;
    if minutes >= 120 {
        format!("{} h", minutes / 60)
    } else {
        format!("{minutes} min")
    }
}

/// `412k`, `1.3M`.
fn tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn outcome(command: &ReviewCommand) -> String {
    match (command.exit_code, command.status) {
        (Some(code), _) => format!("exit {code}"),
        (None, ReviewToolStatus::Failed) => "failed".to_owned(),
        (None, ReviewToolStatus::Declined) => "declined".to_owned(),
        (None, ReviewToolStatus::Completed) => "no exit code".to_owned(),
        (None, ReviewToolStatus::Unfinished) => "unfinished".to_owned(),
    }
}

fn command_evidence(turn: Option<&str>, command: &ReviewCommand) -> FindingEvidence {
    evidence(
        EvidenceKind::Command,
        turn.map(str::to_owned),
        format!("{}: {}", outcome(command), command.text),
    )
}

fn file_evidence(file: &TurnFile) -> FindingEvidence {
    let status = match file.seen.status {
        CheckpointFileStatus::Added => "added",
        CheckpointFileStatus::Modified => "modified",
        CheckpointFileStatus::Deleted => "deleted",
    };
    let lines = match (file.seen.added, file.seen.removed) {
        (Some(added), Some(removed)) => format!(" +{added} −{removed}"),
        _ => String::new(),
    };
    evidence(
        EvidenceKind::File,
        Some(file.seen.path.clone()),
        format!("{status}{lines}: {}", file.seen.path),
    )
}

/// A draft of `meta`'s session with the defaults of a finding about one moment.
fn draft(
    meta: &SessionMeta,
    detector: &'static str,
    confidence: FindingConfidence,
    key: impl Into<String>,
    title: impl Into<String>,
) -> Draft {
    Draft {
        detector,
        severity: severity_of(detector),
        confidence,
        workspace: meta.workspace_id,
        session: Some(meta.id),
        turn: None,
        key: key.into(),
        title: title.into(),
        evidence: Vec::new(),
        first_at_ms: 0,
        last_at_ms: 0,
        count: 1,
        members: Vec::new(),
    }
}

// Per turn ----------------------------------------------------------------------------------------

/// The findings of the chat's journal: of each turn, then across its turns, then of its insight.
pub fn journal_drafts(journal: &JournalFacts, thresholds: &Thresholds) -> Vec<Draft> {
    let mut out = Vec::new();
    for turn in &journal.turns {
        turn_drafts(&journal.meta, turn, &mut out);
    }
    repeated_failures(&journal.meta, &journal.turns, thresholds, &mut out);
    thrash(&journal.meta, &journal.turns, thresholds, &mut out);
    if let Ok(insight) = &journal.insight {
        insight_drafts(&journal.meta, insight, thresholds, &mut out);
    }
    out
}

fn turn_drafts(meta: &SessionMeta, turn: &TurnFacts, out: &mut Vec<Draft>) {
    if turn.status == ChatTurnStatus::Running {
        return;
    }
    let n = turn.number;
    let at = turn.at_ms();
    let tref = turn.turn_id.map(|t| t.to_string());
    let message = turn.final_message.as_deref();
    let claim = message.and_then(claims_pass);
    let mut found: Vec<Draft> = Vec::new();
    let mut push = |mut d: Draft| {
        d.turn = turn.turn_id;
        d.first_at_ms = at;
        d.last_at_ms = at;
        found.push(d);
    };

    // The turn ended well after its last test command failed; or sounded done with a command
    // that failed and was not run again.
    let mut last_test_failed = false;
    if turn.status == ChatTurnStatus::Completed {
        let last_test = turn
            .commands
            .iter()
            .rev()
            .find(|c| looks_like_a_test(&c.text) && c.status != ReviewToolStatus::Declined);
        if let Some(command) = last_test.filter(|c| is_failed(c)) {
            last_test_failed = true;
            let mut d = draft(
                meta,
                detector::DONE_AFTER_FAILED_COMMAND,
                FindingConfidence::Exact,
                "last-test",
                format!("Turn {n} ended after its last test command failed"),
            );
            d.evidence.push(command_evidence(tref.as_deref(), command));
            if let Some(text) = claim.as_ref().map(|c| c.sentence.as_str()).or(message) {
                d.evidence.push(evidence(
                    EvidenceKind::Event,
                    tref.clone(),
                    format!("final message: {text}"),
                ));
            }
            push(d);
        } else {
            let unrecovered = unrecovered_failures(&turn.commands);
            if !unrecovered.is_empty()
                && let Some(text) = message.filter(|m| sounds_done(m))
            {
                let mut d = draft(
                    meta,
                    detector::DONE_AFTER_FAILED_COMMAND,
                    FindingConfidence::Heuristic,
                    "unrecovered",
                    format!(
                        "Turn {n} sounds done, and a command it ran failed and was not run again with success"
                    ),
                );
                d.evidence.extend(
                    unrecovered
                        .iter()
                        .take(4)
                        .map(|c| command_evidence(tref.as_deref(), c)),
                );
                d.evidence.push(evidence(
                    EvidenceKind::Event,
                    tref.clone(),
                    format!("final message: {text}"),
                ));
                push(d);
            }
        }
    }

    // The final message says tests or a build pass, and no such command exited 0.
    if turn.status == ChatTurnStatus::Completed
        && !last_test_failed
        && let Some(claim) = &claim
    {
        let relevant = |c: &&ReviewCommand| match claim.kind {
            ClaimKind::Tests => looks_like_a_test(&c.text),
            ClaimKind::Build => looks_like_a_check(&c.text),
        };
        let ran: Vec<&ReviewCommand> = turn.commands.iter().filter(relevant).collect();
        if !ran.iter().any(|c| is_passed(c) || is_unknown(c)) {
            let (what, kind) = match claim.kind {
                ClaimKind::Tests => ("the tests", "test"),
                ClaimKind::Build => ("the build or the checks", "build or check"),
            };
            let mut d = draft(
                meta,
                detector::CLAIMED_PASS_WITHOUT_TEST,
                FindingConfidence::Heuristic,
                "claim",
                format!("Turn {n} says {what} pass, and no {kind} command exited 0"),
            );
            d.evidence.push(evidence(
                EvidenceKind::Event,
                tref.clone(),
                format!("final message: {}", claim.sentence),
            ));
            if ran.is_empty() {
                d.evidence.push(evidence(
                    EvidenceKind::Stat,
                    tref.clone(),
                    format!(
                        "{} ran in the turn, none looks like a {kind} run",
                        plural(turn.commands.len(), "command", "commands")
                    ),
                ));
            } else {
                d.evidence.extend(
                    ran.iter()
                        .take(3)
                        .map(|c| command_evidence(tref.as_deref(), c)),
                );
            }
            push(d);
        }
    }

    // Files.
    let files = turn.files.files();
    let by_role = |role: FileRole| -> Vec<&TurnFile> {
        files
            .iter()
            .filter(|f| file_role(&f.seen.path) == Some(role))
            .collect()
    };
    let tests = by_role(FileRole::Test);
    let snapshots = by_role(FileRole::Snapshot);
    let code: Vec<&TurnFile> = files
        .iter()
        .filter(|f| is_source_code(&f.seen.path))
        .collect();
    if !tests.is_empty() && !code.is_empty() {
        let mut d = draft(
            meta,
            detector::TESTS_EDITED,
            FindingConfidence::Exact,
            "turn",
            format!(
                "Turn {n} changed {} and {}",
                plural(tests.len(), "test file", "test files"),
                plural(code.len(), "source file", "source files")
            ),
        );
        d.count = u32::try_from(tests.len()).unwrap_or(u32::MAX);
        d.evidence
            .extend(tests.iter().take(3).map(|f| file_evidence(f)));
        d.evidence
            .extend(code.iter().take(2).map(|f| file_evidence(f)));
        push(d);
    }
    let mut weakened: Vec<FindingEvidence> = Vec::new();
    let mut skipped = false;
    for file in &tests {
        let Some(patch) = &file.patch else { continue };
        let w = weakening(patch);
        if !w.is_weakened() {
            continue;
        }
        if let Some(line) = w.skips.first() {
            skipped = true;
            weakened.push(evidence(
                EvidenceKind::Diff,
                Some(file.seen.path.clone()),
                format!("{}: +{line}", file.seen.path),
            ));
        } else {
            weakened.push(evidence(
                EvidenceKind::Diff,
                Some(file.seen.path.clone()),
                format!(
                    "{}: {} removed, {} added",
                    file.seen.path,
                    plural(w.removed_assertions, "assertion", "assertions"),
                    w.added_assertions
                ),
            ));
        }
    }
    if !weakened.is_empty() {
        let mut d = draft(
            meta,
            detector::TESTS_WEAKENED,
            FindingConfidence::Heuristic,
            "turn",
            if skipped {
                format!("Turn {n} skipped, ignored or focused a test")
            } else {
                format!("Turn {n} removed more assertions than it added in test files")
            },
        );
        d.severity = if skipped {
            FindingSeverity::Alert
        } else {
            FindingSeverity::Warn
        };
        d.count = u32::try_from(weakened.len()).unwrap_or(u32::MAX);
        d.evidence = weakened;
        push(d);
    }
    if !snapshots.is_empty() {
        let mut d = draft(
            meta,
            detector::SNAPSHOT_UPDATED,
            FindingConfidence::Exact,
            "turn",
            format!(
                "Turn {n} rewrote {}",
                plural(
                    snapshots.len(),
                    "snapshot, golden or fixture file",
                    "snapshot, golden or fixture files"
                )
            ),
        );
        d.count = u32::try_from(snapshots.len()).unwrap_or(u32::MAX);
        d.evidence
            .extend(snapshots.iter().take(5).map(|f| file_evidence(f)));
        push(d);
    }

    // The signs the review packet carries.
    let seen: Vec<SeenFile> = files.iter().map(|f| f.seen.clone()).collect();
    let totals = match &turn.files {
        TurnFiles::Checkpoint(files) => Some((
            files.iter().filter_map(|f| f.seen.added).sum::<u64>(),
            files.iter().filter_map(|f| f.seen.removed).sum::<u64>(),
        )),
        _ => None,
    };
    for sign in signals(&seen, &turn.commands, message, &meta.roots, totals) {
        let (id, title, count, kind) = match sign.id {
            ReviewSignalId::DependencyFileChanged => (
                detector::DEPENDENCY_FILE_CHANGED,
                format!("Turn {n} changed a lockfile or a package manifest"),
                seen.iter()
                    .filter(|f| crate::signs::is_dependency_file(&f.path))
                    .count(),
                EvidenceKind::File,
            ),
            ReviewSignalId::FileDeleted => (
                detector::FILE_DELETED,
                format!("Turn {n} deleted a file"),
                seen.iter()
                    .filter(|f| f.status == CheckpointFileStatus::Deleted)
                    .count(),
                EvidenceKind::File,
            ),
            ReviewSignalId::PathOutsideFolder => (
                detector::PATH_OUTSIDE_FOLDER,
                format!("Turn {n} changed a path outside the session's folders"),
                sign.evidence.len(),
                EvidenceKind::File,
            ),
            ReviewSignalId::SecretLikePath => (
                detector::SECRET_LIKE_PATH,
                format!("Turn {n} changed a path named like a secret"),
                sign.evidence.len(),
                EvidenceKind::File,
            ),
            ReviewSignalId::LargeDiff => (
                detector::LARGE_DIFF,
                format!("Turn {n} changed more than {REVIEW_LARGE_DIFF_LINES} lines"),
                1,
                EvidenceKind::Stat,
            ),
            ReviewSignalId::NoTestCommand => (
                detector::NO_TEST_COMMAND,
                format!("Turn {n} changed files and ran no test command"),
                1,
                EvidenceKind::Stat,
            ),
            // Handled above, with the exact rule first.
            ReviewSignalId::FailedCommandThenDone => continue,
        };
        let mut d = draft(meta, id, FindingConfidence::Exact, "turn", title);
        d.count = u32::try_from(count).unwrap_or(u32::MAX).max(1);
        d.evidence = sign
            .evidence
            .iter()
            .map(|text| {
                let reference = (kind == EvidenceKind::File).then(|| text.clone());
                evidence(kind, reference, text)
            })
            .collect();
        push(d);
    }
    out.extend(found);
}

// Across the turns of a session ---------------------------------------------------------------------

/// One failure of the session: where, when and what.
struct Occurrence {
    turn: u32,
    turn_id: Option<hutan_core::TurnId>,
    at_ms: u64,
    text: String,
}

/// The same normalised failing command, failing tool or turn error at least
/// `repeated_failure_min` times: the count is exact, the grouping a heuristic.
fn repeated_failures(
    meta: &SessionMeta,
    turns: &[TurnFacts],
    thresholds: &Thresholds,
    out: &mut Vec<Draft>,
) {
    // (what failed, its normalised words) → every time it did.
    let mut groups: BTreeMap<(&'static str, String), Vec<Occurrence>> = BTreeMap::new();
    for turn in turns {
        let at_ms = turn.at_ms();
        for command in turn
            .commands
            .iter()
            .filter(|c| is_failed(c) && c.status != ReviewToolStatus::Declined)
        {
            groups
                .entry(("command", normalise(&command.text)))
                .or_default()
                .push(Occurrence {
                    turn: turn.number,
                    turn_id: turn.turn_id,
                    at_ms,
                    text: format!("{}: {}", outcome(command), command.text),
                });
        }
        for failure in &turn.tool_failures {
            groups
                .entry((
                    "tool",
                    format!("{} {}", failure.name, normalise(&failure.text)),
                ))
                .or_default()
                .push(Occurrence {
                    turn: turn.number,
                    turn_id: turn.turn_id,
                    at_ms,
                    text: format!("{}: {}", failure.name, failure.text),
                });
        }
        if turn.status == ChatTurnStatus::Failed
            && let Some(error) = turn.error.as_deref().filter(|e| !e.trim().is_empty())
        {
            groups
                .entry(("turn", normalise(error)))
                .or_default()
                .push(Occurrence {
                    turn: turn.number,
                    turn_id: turn.turn_id,
                    at_ms,
                    text: error.to_owned(),
                });
        }
    }
    let min = usize::try_from(thresholds.repeated_failure_min.max(2)).unwrap_or(usize::MAX);
    for ((kind, key), occurrences) in groups {
        if occurrences.len() < min {
            continue;
        }
        let n = occurrences.len();
        let title = match kind {
            "command" => format!("The same command failed {n} times"),
            "tool" => format!("The same tool call failed {n} times"),
            _ => format!("The same error ended {n} turns"),
        };
        let mut d = draft(
            meta,
            detector::REPEATED_FAILURE,
            FindingConfidence::Heuristic,
            format!("{kind}:{key}"),
            title,
        );
        d.count = u32::try_from(n).unwrap_or(u32::MAX);
        d.first_at_ms = occurrences.first().map_or(0, |o| o.at_ms);
        d.last_at_ms = occurrences.last().map_or(0, |o| o.at_ms);
        let kind_of = if kind == "command" {
            EvidenceKind::Command
        } else {
            EvidenceKind::Event
        };
        d.evidence = occurrences
            .iter()
            .rev()
            .take(5)
            .rev()
            .map(|o| {
                evidence(
                    kind_of,
                    o.turn_id.map(|t| t.to_string()),
                    format!("turn {}: {}", o.turn, o.text),
                )
            })
            .collect();
        out.push(d);
    }
}

/// A file changed back and forth: in several turns with a net change near zero, or a turn that
/// undoes the one before. Only where a checkpoint gave exact lines.
fn thrash(meta: &SessionMeta, turns: &[TurnFacts], thresholds: &Thresholds, out: &mut Vec<Draft>) {
    // path → (turn, added, removed, at) in turn order.
    let mut history: BTreeMap<&str, Vec<(u32, u64, u64, u64)>> = BTreeMap::new();
    for turn in turns {
        let TurnFiles::Checkpoint(files) = &turn.files else {
            continue;
        };
        for file in files {
            if let (Some(added), Some(removed)) = (file.seen.added, file.seen.removed) {
                history.entry(file.seen.path.as_str()).or_default().push((
                    turn.number,
                    added,
                    removed,
                    turn.at_ms(),
                ));
            }
        }
    }
    let min_turns = usize::try_from(thresholds.thrash_turns.max(2)).unwrap_or(usize::MAX);
    for (path, history) in history {
        let churn: u64 = history.iter().map(|(_, a, r, _)| a + r).sum();
        let added: u64 = history.iter().map(|(_, a, _, _)| a).sum();
        let removed: u64 = history.iter().map(|(_, _, r, _)| r).sum();
        if history.len() >= min_turns && churn >= 20 && added.abs_diff(removed) * 10 <= churn {
            let mut d = draft(
                meta,
                detector::THRASH,
                FindingConfidence::Heuristic,
                format!("net:{path}"),
                format!(
                    "A file changed in {} with a net change near zero",
                    plural(history.len(), "turn", "turns")
                ),
            );
            d.count = u32::try_from(history.len()).unwrap_or(u32::MAX);
            d.first_at_ms = history.first().map_or(0, |h| h.3);
            d.last_at_ms = history.last().map_or(0, |h| h.3);
            d.evidence = history
                .iter()
                .rev()
                .take(5)
                .rev()
                .map(|(turn, a, r, _)| {
                    evidence(
                        EvidenceKind::File,
                        Some(path.to_owned()),
                        format!("turn {turn}: +{a} −{r}: {path}"),
                    )
                })
                .collect();
            out.push(d);
        }
        for pair in history.windows(2) {
            let (before, after) = (pair[0], pair[1]);
            if after.0 == before.0 + 1
                && after.1 == before.2
                && after.2 == before.1
                && before.1 + before.2 >= 4
            {
                let mut d = draft(
                    meta,
                    detector::THRASH,
                    FindingConfidence::Heuristic,
                    format!("revert:{}:{path}", after.0),
                    format!(
                        "Turn {} undid the change of turn {} in a file",
                        after.0, before.0
                    ),
                );
                d.first_at_ms = before.3;
                d.last_at_ms = after.3;
                d.count = 2;
                d.evidence = vec![
                    evidence(
                        EvidenceKind::File,
                        Some(path.to_owned()),
                        format!("turn {}: +{} −{}: {path}", before.0, before.1, before.2),
                    ),
                    evidence(
                        EvidenceKind::File,
                        Some(path.to_owned()),
                        format!("turn {}: +{} −{}: {path}", after.0, after.1, after.2),
                    ),
                ];
                out.push(d);
            }
        }
    }
}

/// `context_pressure` and `costly_turn` from the chat's insight.
fn insight_drafts(
    meta: &SessionMeta,
    insight: &AgentInsight,
    thresholds: &Thresholds,
    out: &mut Vec<Draft>,
) {
    if let Some(context) = &insight.context
        && let Some(window) = context.window.filter(|w| *w > 0)
    {
        let percent = context.used.saturating_mul(100) / window;
        if percent >= u64::from(thresholds.context_pressure_percent) {
            let mut d = draft(
                meta,
                detector::CONTEXT_PRESSURE,
                FindingConfidence::Exact,
                "window",
                format!("The context is at {percent}% of the window"),
            );
            d.first_at_ms = context.as_of_ms;
            d.last_at_ms = context.as_of_ms;
            d.evidence = vec![evidence(
                EvidenceKind::Stat,
                None,
                format!(
                    "{} of {} tokens ({})",
                    tokens(context.used),
                    tokens(window),
                    match context.exactness {
                        Exactness::Reported => "reported",
                        Exactness::Estimated => "estimated",
                    }
                ),
            )];
            out.push(d);
        }
    }
    // The context shrank a lot between two turns: the harness probably compacted it.
    for pair in insight.turns.windows(2) {
        if let (Some(before), Some(after)) = (pair[0].context_after, pair[1].context_after)
            && before >= 20_000
            && after.saturating_mul(10) <= before.saturating_mul(6)
        {
            let mut d = draft(
                meta,
                detector::CONTEXT_PRESSURE,
                FindingConfidence::Heuristic,
                format!("compacted:{}", pair[1].n),
                format!(
                    "The context shrank after turn {}: the harness probably compacted it",
                    pair[1].n
                ),
            );
            d.turn = pair[1].turn_id;
            d.first_at_ms = pair[1].started_ms;
            d.last_at_ms = pair[1].started_ms;
            d.evidence = vec![evidence(
                EvidenceKind::Stat,
                None,
                format!(
                    "{} tokens after turn {}, {} after turn {}",
                    tokens(before),
                    pair[0].n,
                    tokens(after),
                    pair[1].n
                ),
            )];
            out.push(d);
        }
    }
    // A turn that spent far more than the session's usual.
    let spent: Vec<(usize, u64)> = insight
        .turns
        .iter()
        .enumerate()
        .filter(|(_, t)| t.input.is_some() || t.output.is_some())
        .map(|(at, t)| (at, t.input.unwrap_or(0) + t.output.unwrap_or(0)))
        .collect();
    if spent.len() >= 4 {
        let mut sorted: Vec<u64> = spent.iter().map(|(_, t)| *t).collect();
        sorted.sort_unstable();
        let median = sorted[(sorted.len() - 1) / 2];
        if median > 0 {
            for (at, total) in &spent {
                let turn = &insight.turns[*at];
                if *total >= thresholds.costly_turn_min_tokens
                    && *total >= u64::from(thresholds.costly_turn_factor) * median
                {
                    let mut d = draft(
                        meta,
                        detector::COSTLY_TURN,
                        FindingConfidence::Exact,
                        format!("turn:{}", turn.n),
                        format!(
                            "Turn {} spent {} times the session's usual tokens",
                            turn.n,
                            total / median
                        ),
                    );
                    d.turn = turn.turn_id;
                    d.first_at_ms = turn.started_ms;
                    d.last_at_ms = turn.started_ms + turn.duration_ms.unwrap_or(0);
                    d.evidence = vec![evidence(
                        EvidenceKind::Stat,
                        turn.turn_id.map(|t| t.to_string()),
                        format!(
                            "turn {}: {} tokens; the median turn of the session: {}",
                            turn.n,
                            tokens(*total),
                            tokens(median)
                        ),
                    )];
                    out.push(d);
                }
            }
        }
    }
}

// What changes with time ----------------------------------------------------------------------------

/// `long_wait`, `stalled` and `unreviewed_changes` of one session at `facts.now_ms`.
pub fn live_drafts(facts: &LiveFacts, thresholds: &Thresholds) -> Vec<Draft> {
    let meta = &facts.meta;
    let now = facts.now_ms;
    let mut out = Vec::new();

    // Waits for the user: the audit tables for a chat, the state log for a terminal agent.
    let wait_ms = thresholds.long_wait_minutes * 60_000;
    for request in &facts.requests {
        let (end, open) = match request.resolved_ms {
            Some(at) => (at, false),
            None if meta.live => (now, true),
            None => continue,
        };
        let waited = end.saturating_sub(request.asked_ms);
        if waited < wait_ms {
            continue;
        }
        let mut d = draft(
            meta,
            detector::LONG_WAIT,
            FindingConfidence::Exact,
            format!("request:{}", request.id),
            if open {
                format!(
                    "Has waited {} for the user's answer to {} (still waiting)",
                    duration(waited),
                    request.kind
                )
            } else {
                format!(
                    "Waited {} for the user's answer to {}",
                    duration(waited),
                    request.kind
                )
            },
        );
        d.severity = if open {
            FindingSeverity::Warn
        } else {
            FindingSeverity::Info
        };
        d.first_at_ms = request.asked_ms;
        d.last_at_ms = end;
        d.evidence = vec![evidence(
            EvidenceKind::Event,
            Some(request.id.clone()),
            format!("{}: {}", request.kind, request.title),
        )];
        out.push(d);
    }
    if !meta.chat && facts.state_log {
        for span in &facts.blocked_spans {
            let (end, open) = match span.end_ms {
                Some(at) => (at, false),
                None if meta.live => (now, true),
                None => continue,
            };
            let waited = end.saturating_sub(span.start_ms);
            if waited < wait_ms {
                continue;
            }
            let mut d = draft(
                meta,
                detector::LONG_WAIT,
                FindingConfidence::Heuristic,
                format!("blocked@{}", span.start_ms),
                format!(
                    "Said it was blocked for {}{} (the state log only)",
                    duration(waited),
                    if open { " and still is" } else { "" }
                ),
            );
            d.severity = if open {
                FindingSeverity::Warn
            } else {
                FindingSeverity::Info
            };
            d.first_at_ms = span.start_ms;
            d.last_at_ms = end;
            d.evidence = vec![evidence(
                EvidenceKind::Event,
                None,
                format!(
                    "blocked{} since the agent's report",
                    span.kind
                        .as_deref()
                        .map(|k| format!(" on {k}"))
                        .unwrap_or_default()
                ),
            )];
            out.push(d);
        }
    }

    // Working and quiet.
    let quiet_ms = thresholds.stalled_minutes * 60_000;
    if meta.live {
        if meta.chat {
            if facts.turn_running
                && !facts.pending_request
                && let Some(last) = facts.last_event_ms
                && now.saturating_sub(last) >= quiet_ms
            {
                let mut d = draft(
                    meta,
                    detector::STALLED,
                    FindingConfidence::Exact,
                    format!("quiet@{last}"),
                    format!(
                        "Working, with no event for {}",
                        duration(now.saturating_sub(last))
                    ),
                );
                d.first_at_ms = last;
                d.last_at_ms = now;
                d.evidence = vec![evidence(
                    EvidenceKind::Stat,
                    None,
                    format!(
                        "a turn runs; the last event of the journal was {} ago (a long command is quiet too)",
                        duration(now.saturating_sub(last))
                    ),
                )];
                out.push(d);
            }
        } else if let Some(since) = facts.working_since_ms
            && now.saturating_sub(since) >= quiet_ms
        {
            let mut d = draft(
                meta,
                detector::STALLED,
                FindingConfidence::Heuristic,
                format!("working@{since}"),
                format!(
                    "Says it works since {}, and nothing else is known: its state is unconfirmed",
                    duration(now.saturating_sub(since))
                ),
            );
            d.severity = FindingSeverity::Info;
            d.first_at_ms = since;
            d.last_at_ms = now;
            d.evidence = vec![evidence(
                EvidenceKind::Stat,
                None,
                "a terminal agent reports its state only; Hutan sees no tool or file event",
            )];
            out.push(d);
        }
    }

    // Finished work nobody reviewed.
    if let (Some(task), Some(changed)) = (&facts.task, facts.changed_at_ms) {
        let waited = now.saturating_sub(task.awaiting_since_ms);
        if waited >= thresholds.unreviewed_hours * 3_600_000 {
            let mut d = draft(
                meta,
                detector::UNREVIEWED_CHANGES,
                FindingConfidence::Exact,
                format!("since@{}", task.awaiting_since_ms),
                format!("Changes wait for review for {}", duration(waited)),
            );
            d.first_at_ms = changed.min(task.awaiting_since_ms);
            d.last_at_ms = now;
            d.evidence = vec![evidence(
                EvidenceKind::Stat,
                None,
                format!(
                    "task \"{}\" awaits review since its work finished",
                    task.title
                ),
            )];
            out.push(d);
        }
    }

    // Hutan's own run of the tests exited non-zero after the agent said it was done. A later
    // run that passes, or any work of the agent after the run, ends it: the newest run decides.
    if let (Some(run), Some(done)) = (&facts.verify, facts.done_since_ms)
        && run.status == "failed"
        && run.started_ms >= done
    {
        let mut d = draft(
            meta,
            detector::VERIFY_FAILED,
            FindingConfidence::Exact,
            format!("run:{}", run.run_id),
            "Hutan's own run of the tests failed after the agent said it was done",
        );
        d.first_at_ms = run.started_ms;
        d.last_at_ms = run.ended_ms.unwrap_or(run.started_ms);
        let code = run
            .exit_code
            .map_or_else(|| "failed".to_owned(), |code| format!("exit {code}"));
        d.evidence = vec![evidence(
            EvidenceKind::Verify,
            Some(run.run_id.to_string()),
            format!("{code}: {} (asked by {})", run.command, run.requested_by),
        )];
        if let Some(line) = &run.last_line {
            d.evidence.push(evidence(
                EvidenceKind::Verify,
                Some(run.run_id.to_string()),
                format!("the last line of its output: {line}"),
            ));
        }
        out.push(d);
    }
    out
}

// Across sessions -----------------------------------------------------------------------------------

/// `shared_checkout` and `overlap` over the live sessions of the daemon. `live` lists the
/// sessions of the scope first: the finding's workspace is the first member's.
pub fn workspace_drafts(live: &[&LiveFacts]) -> Vec<Draft> {
    let mut out = Vec::new();
    let now = live.first().map_or(0, |l| l.now_ms);
    let label = |l: &LiveFacts| match &l.meta.harness {
        Some(harness) => format!("{} ({harness})", l.meta.title),
        None => l.meta.title.clone(),
    };

    // Two agents writing one checkout: the placement already says who shares it.
    let mut checkouts: BTreeMap<&std::path::Path, Vec<&LiveFacts>> = BTreeMap::new();
    for session in live {
        if let Some(placement) = &session.placement
            && !placement.shared_with.is_empty()
        {
            checkouts
                .entry(placement.root.as_path())
                .or_default()
                .push(session);
        }
    }
    for (root, sessions) in checkouts {
        let Some(first) = sessions.first() else {
            continue;
        };
        let mut d = Draft {
            detector: detector::SHARED_CHECKOUT,
            severity: severity_of(detector::SHARED_CHECKOUT),
            confidence: FindingConfidence::Exact,
            workspace: first.meta.workspace_id,
            session: None,
            turn: None,
            key: root.display().to_string(),
            title: format!(
                "{} write to one checkout",
                plural(sessions.len(), "agent", "agents")
            ),
            evidence: Vec::new(),
            first_at_ms: sessions
                .iter()
                .map(|s| s.meta.created_ms)
                .max()
                .unwrap_or(now),
            last_at_ms: now,
            count: u32::try_from(sessions.len()).unwrap_or(u32::MAX),
            members: sessions.iter().map(|s| s.meta.id).collect(),
        };
        d.evidence = sessions
            .iter()
            .map(|s| {
                evidence(
                    EvidenceKind::Event,
                    Some(s.meta.id.to_string()),
                    format!("{} in {}", label(s), root.display()),
                )
            })
            .collect();
        out.push(d);
    }

    // The same file changed by two live sessions.
    let mut by_file: BTreeMap<&ChangedFile, Vec<&LiveFacts>> = BTreeMap::new();
    for session in live {
        for file in session.files.iter().flatten() {
            by_file.entry(file).or_default().push(session);
        }
    }
    for (file, sessions) in by_file {
        let distinct: BTreeSet<SessionId> = sessions.iter().map(|s| s.meta.id).collect();
        if distinct.len() < 2 {
            continue;
        }
        let Some(first) = sessions.first() else {
            continue;
        };
        let same_checkout = sessions.iter().all(|s| {
            s.placement.as_ref().map(|p| &p.root) == first.placement.as_ref().map(|p| &p.root)
        }) && first.placement.is_some();
        let mut d = Draft {
            detector: detector::OVERLAP,
            severity: if same_checkout {
                FindingSeverity::Alert
            } else {
                FindingSeverity::Warn
            },
            confidence: FindingConfidence::Exact,
            workspace: first.meta.workspace_id,
            session: None,
            turn: None,
            key: format!("{}\0{}", file.repo, file.path),
            title: format!(
                "{} changed the same file{}",
                plural(distinct.len(), "live session", "live sessions"),
                if same_checkout {
                    " in one checkout"
                } else {
                    ""
                }
            ),
            evidence: Vec::new(),
            first_at_ms: sessions
                .iter()
                .map(|s| s.meta.created_ms)
                .max()
                .unwrap_or(now),
            last_at_ms: now,
            count: u32::try_from(distinct.len()).unwrap_or(u32::MAX),
            members: distinct.iter().copied().collect(),
        };
        d.evidence = sessions
            .iter()
            .map(|s| {
                evidence(
                    EvidenceKind::File,
                    Some(s.meta.id.to_string()),
                    format!("{} ({})", file.path, label(s)),
                )
            })
            .collect();
        out.push(d);
    }
    out
}

// Coverage ------------------------------------------------------------------------------------------

/// Detectors that could not run for a journal, and why.
pub fn journal_gaps(journal: &JournalFacts) -> Vec<CoverageGap> {
    let mut gaps = Vec::new();
    let unknown = journal
        .turns
        .iter()
        .filter(|t| matches!(t.files, TurnFiles::Missing(_)) && t.status != ChatTurnStatus::Running)
        .count();
    if unknown > 0 {
        gaps.push(gap(
            FILE_DETECTORS,
            format!(
                "the files of {unknown} of {} turns are unknown: no checkpoint is kept (they are kept 30 days, and a chat may have them off) and the journal names no file change",
                journal.turns.len()
            ),
        ));
    }
    let unread = journal
        .turns
        .iter()
        .flat_map(|t| t.files.files())
        .filter(|f| file_role(&f.seen.path) == Some(FileRole::Test) && f.patch.is_none())
        .count();
    if unread > 0 {
        gaps.push(gap(
            &[detector::TESTS_WEAKENED],
            format!(
                "{} without a diff to read (no checkpoint patch, and the journal carries none)",
                plural(unread, "changed test file", "changed test files")
            ),
        ));
    }
    match &journal.insight {
        Err(why) => gaps.push(gap(
            &[detector::CONTEXT_PRESSURE, detector::COSTLY_TURN],
            format!("the insight of this chat is not available: {why}"),
        )),
        Ok(insight) => {
            if !insight.context.as_ref().is_some_and(|c| c.window.is_some()) {
                gaps.push(gap(
                    &[detector::CONTEXT_PRESSURE],
                    "the harness reports no context window".to_owned(),
                ));
            }
            if !insight
                .turns
                .iter()
                .any(|t| t.input.is_some() || t.output.is_some())
            {
                gaps.push(gap(
                    &[detector::COSTLY_TURN],
                    "the harness reports no token counts per turn".to_owned(),
                ));
            }
        }
    }
    gaps
}

fn gap(detectors: &[&str], why: String) -> CoverageGap {
    CoverageGap {
        detectors: detectors.iter().map(|d| (*d).to_owned()).collect(),
        why,
    }
}

/// What is known of a session's journal when its coverage is told.
#[derive(Debug, Clone, Copy)]
pub enum JournalCoverage<'a> {
    /// A chat whose journal was read: the detectors that could not run over it.
    Read(&'a [CoverageGap]),
    /// A chat whose journal could not be read; why.
    Unreadable(&'a str),
    /// A terminal agent: it has no journal.
    TerminalAgent,
}

/// Which detectors ran over a session and which could not.
pub fn coverage(live: &LiveFacts, journal: JournalCoverage<'_>) -> SessionCoverage {
    let meta = &live.meta;
    let mut gaps: Vec<CoverageGap> = Vec::new();
    match journal {
        JournalCoverage::Read(read) => gaps.extend_from_slice(read),
        JournalCoverage::Unreadable(why) => gaps.push(gap(
            TURN_DETECTORS,
            format!("the journal could not be read: {why}"),
        )),
        JournalCoverage::TerminalAgent => {
            let mut all: Vec<&str> = TURN_DETECTORS.to_vec();
            all.extend([detector::CONTEXT_PRESSURE, detector::COSTLY_TURN]);
            gaps.push(gap(
                &all,
                "terminal agent: no tool events — install the integration (`hutan integration install`)"
                    .to_owned(),
            ));
        }
    }
    if !meta.chat && !live.state_log {
        gaps.push(gap(
            &[detector::LONG_WAIT],
            "the state log is off (`[agents] state_log` in daemon.toml): a terminal agent's waits are not recorded"
                .to_owned(),
        ));
    }
    if live.task.is_none() {
        gaps.push(gap(
            &[detector::UNREVIEWED_CHANGES],
            "not part of a task awaiting review: review and merge are recorded for tasks only"
                .to_owned(),
        ));
    }
    if live.files.is_none() {
        gaps.push(gap(
            &[detector::OVERLAP],
            if meta.chat {
                "the chat's changed files are not known".to_owned()
            } else {
                "terminal agent: no file events — install the integration".to_owned()
            },
        ));
    }
    if live.placement.is_none() {
        gaps.push(gap(
            &[detector::SHARED_CHECKOUT],
            "the session is not in a git checkout".to_owned(),
        ));
    }
    let missing: BTreeSet<&str> = gaps
        .iter()
        .flat_map(|g| g.detectors.iter().map(String::as_str))
        .collect();
    SessionCoverage {
        session_id: meta.id,
        title: crate::signs::line(&meta.title, 120),
        harness: meta.harness.clone(),
        chat: meta.chat,
        ran: detector::ALL
            .iter()
            .filter(|d| !missing.contains(**d))
            .map(|d| (*d).to_owned())
            .collect(),
        gaps,
    }
}

#[cfg(test)]
mod tests;
