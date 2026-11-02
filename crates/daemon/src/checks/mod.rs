//! `agents.findings` (protocol 0.70, capability `agents.findings`, ADR 0025): what is wrong in a
//! session or a workspace, from named deterministic detectors (`docs/assistant.md` § Checks).
//!
//! - [`facts`]: the recorded facts a detector reads, as plain data.
//! - [`detectors`]: one pure function per rule of the design — a finding is a function of
//!   recorded facts, never of a model's opinion, and says how sure it is (`exact` for a record,
//!   `heuristic` for a pattern).
//! - [`collect`]: [`ChecksService`] reads the facts from the registry, the journal, the
//!   checkpoints, the audit tables, the state log, the tasks and the insight reducer, keeps the
//!   findings of a chat by the journal's last sequence, and answers.
//!
//! This module holds what they share: the thresholds of `[checks]`, a finding's stable id, and
//! the assembly of an answer — sorted, dismissed ones marked, bounded to 200 findings and 24 KiB.

pub mod collect;
pub mod detectors;
pub mod facts;

use std::collections::HashMap;

use hutan_core::{SessionId, TurnId, WorkspaceId};
use hutan_index::redact::redact;
use hutan_protocol::checks::{
    AgentsFindings, AgentsFindingsRequest, EvidenceKind, FINDING_EVIDENCE_MAX,
    FINDING_EXCERPT_CHARS, FINDING_TITLE_CHARS, FINDINGS_DEFAULT_LIMIT, FINDINGS_MAX,
    FINDINGS_MAX_BYTES, Finding, FindingConfidence, FindingCounts, FindingEvidence, FindingScope,
    FindingSeverity, SessionCoverage, detector,
};
use hutan_protocol::review::NotRecorded;
use sha2::{Digest, Sha256};

pub use collect::ChecksService;

use crate::signs::line;

/// The thresholds of the detectors: the `[checks]` table of `daemon.toml`, with the defaults of
/// the design (`docs/spikes/2026-11-02-checks.md` § 3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// `repeated_failure`: the same failure this many times.
    pub repeated_failure_min: u32,
    /// `thrash`: the same file changed in this many turns with a net change near zero.
    pub thrash_turns: u32,
    /// `context_pressure`: the context is this share of the window, in percent.
    pub context_pressure_percent: u32,
    /// `costly_turn`: a turn's tokens are this many times the session's median…
    pub costly_turn_factor: u32,
    /// … and at least this many.
    pub costly_turn_min_tokens: u64,
    /// `long_wait`: the agent waited for the user this long, in minutes.
    pub long_wait_minutes: u64,
    /// `stalled`: working with no event for this long, in minutes.
    pub stalled_minutes: u64,
    /// `unreviewed_changes`: finished work waits for review this long, in hours.
    pub unreviewed_hours: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            repeated_failure_min: 3,
            thrash_turns: 3,
            context_pressure_percent: 85,
            costly_turn_factor: 3,
            costly_turn_min_tokens: 50_000,
            long_wait_minutes: 15,
            stalled_minutes: 20,
            unreviewed_hours: 4,
        }
    }
}

/// The newest turns of a chat the detectors read.
pub const TURNS_ANALYSED: usize = 20;

/// A finding before it has an id and a workspace-checked scope: what a detector says.
#[derive(Debug, Clone)]
pub struct Draft {
    pub detector: &'static str,
    pub severity: FindingSeverity,
    pub confidence: FindingConfidence,
    pub workspace: WorkspaceId,
    pub session: Option<SessionId>,
    pub turn: Option<TurnId>,
    /// What the finding is about, inside its detector, session and turn: the same problem has the
    /// same key on every read, so it keeps its id.
    pub key: String,
    pub title: String,
    pub evidence: Vec<FindingEvidence>,
    pub first_at_ms: u64,
    pub last_at_ms: u64,
    pub count: u32,
    /// The sessions a finding across sessions is about (empty for the others).
    pub members: Vec<SessionId>,
}

/// A finding's id: `chk_` and the first 8 bytes of the SHA-256 of what it is about.
pub fn finding_id(
    detector: &str,
    session: Option<SessionId>,
    turn: Option<TurnId>,
    key: &str,
) -> String {
    let mut hash = Sha256::new();
    for part in [
        detector,
        &session.map(|s| s.to_string()).unwrap_or_default(),
        &turn.map(|t| t.to_string()).unwrap_or_default(),
        key,
    ] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let digest = hash.finalize();
    let mut id = String::from("chk_");
    for byte in &digest[..8] {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

/// A piece of evidence: the text redacted, on one line and cut.
pub fn evidence(
    kind: EvidenceKind,
    reference: Option<String>,
    text: impl AsRef<str>,
) -> FindingEvidence {
    let text = text.as_ref();
    let redacted = redact(text);
    let changed = redacted != text;
    FindingEvidence {
        kind,
        reference,
        excerpt: line(&redacted, FINDING_EXCERPT_CHARS),
        redacted: changed,
    }
}

impl Draft {
    /// The finding with its id.
    pub fn finish(self) -> Finding {
        let id = finding_id(self.detector, self.session, self.turn, &self.key);
        let mut evidence = self.evidence;
        evidence.truncate(FINDING_EVIDENCE_MAX);
        Finding {
            id,
            detector: self.detector.to_owned(),
            severity: self.severity,
            confidence: self.confidence,
            scope: FindingScope {
                workspace_id: self.workspace,
                session_id: self.session,
                turn_id: self.turn,
            },
            title: line(&self.title, FINDING_TITLE_CHARS),
            evidence,
            first_at_ms: self.first_at_ms,
            last_at_ms: self.last_at_ms.max(self.first_at_ms),
            count: self.count.max(1),
            dismissed: false,
        }
    }
}

/// What an answer is made of.
#[derive(Debug)]
pub struct AnswerInputs<'a> {
    pub now_ms: u64,
    pub findings: Vec<Finding>,
    /// The ids the user dismissed, with when.
    pub dismissed: &'a HashMap<String, u64>,
    pub req: &'a AgentsFindingsRequest,
    pub coverage: Vec<SessionCoverage>,
    pub not_recorded: Vec<NotRecorded>,
}

/// The sessions whose coverage one answer lists, besides the bound of its size.
const COVERAGE_MAX: usize = hutan_protocol::checks::FINDINGS_COVERAGE_MAX;

fn size(answer: &AgentsFindings) -> usize {
    serde_json::to_vec(answer).map_or(usize::MAX, |bytes| bytes.len())
}

/// The answer: findings marked dismissed or left out, counted, sorted (alerts first, the newest
/// first within a severity), cut to the limit and to [`FINDINGS_MAX_BYTES`], with the coverage.
pub fn assemble(inputs: AnswerInputs<'_>) -> AgentsFindings {
    let AnswerInputs {
        now_ms,
        mut findings,
        dismissed,
        req,
        mut coverage,
        not_recorded,
    } = inputs;
    if let Some(since) = req.since_ms {
        findings.retain(|f| f.last_at_ms >= since);
    }
    for finding in &mut findings {
        finding.dismissed = dismissed.contains_key(&finding.id);
    }
    let mut counts = FindingCounts::default();
    for finding in findings.iter().filter(|f| !f.dismissed) {
        match finding.severity {
            FindingSeverity::Alert => counts.alert += 1,
            FindingSeverity::Warn => counts.warn += 1,
            FindingSeverity::Info => counts.info += 1,
        }
    }
    let dismissed_total = findings.iter().filter(|f| f.dismissed).count();
    if !req.include_dismissed {
        findings.retain(|f| !f.dismissed);
    }
    let dismissed_hidden = if req.include_dismissed {
        0
    } else {
        dismissed_total
    };
    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(b.last_at_ms.cmp(&a.last_at_ms))
            .then_with(|| a.id.cmp(&b.id))
    });
    let limit = req
        .limit
        .unwrap_or(FINDINGS_DEFAULT_LIMIT)
        .clamp(1, FINDINGS_MAX as u32) as usize;
    let total = findings.len();
    findings.truncate(limit);

    let coverage_omitted = coverage.len().saturating_sub(COVERAGE_MAX);
    coverage.truncate(COVERAGE_MAX);
    let mut answer = AgentsFindings {
        generated_at_ms: now_ms,
        findings,
        omitted: 0,
        dismissed_hidden: u32::try_from(dismissed_hidden).unwrap_or(u32::MAX),
        counts,
        coverage,
        coverage_omitted: u32::try_from(coverage_omitted).unwrap_or(u32::MAX),
        not_recorded,
        cut: Vec::new(),
    };
    fit(&mut answer);
    answer.omitted = u32::try_from(total - answer.findings.len()).unwrap_or(u32::MAX);
    answer
}

/// Cuts, least important first, until the answer fits [`FINDINGS_MAX_BYTES`]; says what it cut.
fn fit(answer: &mut AgentsFindings) {
    if size(answer) <= FINDINGS_MAX_BYTES {
        return;
    }
    for finding in &mut answer.findings {
        finding.evidence.truncate(2);
    }
    answer
        .cut
        .push("evidence cut to 2 pieces per finding".to_owned());
    if size(answer) <= FINDINGS_MAX_BYTES {
        return;
    }
    for coverage in &mut answer.coverage {
        coverage.ran.clear();
    }
    answer
        .cut
        .push("the list of detectors that ran left out of the coverage".to_owned());
    if size(answer) <= FINDINGS_MAX_BYTES {
        return;
    }
    // The findings are sorted, the least important last: drop from the end, one at a time.
    while size(answer) > FINDINGS_MAX_BYTES && answer.findings.pop().is_some() {}
    answer.cut.push(format!(
        "findings cut to the {} most important",
        answer.findings.len()
    ));
}

/// The severity of each detector (the design leaves the table to the engine).
pub fn severity_of(detector_id: &str) -> FindingSeverity {
    match detector_id {
        detector::DONE_AFTER_FAILED_COMMAND => FindingSeverity::Alert,
        detector::CLAIMED_PASS_WITHOUT_TEST
        | detector::TESTS_WEAKENED
        | detector::SNAPSHOT_UPDATED
        | detector::PATH_OUTSIDE_FOLDER
        | detector::SECRET_LIKE_PATH
        | detector::REPEATED_FAILURE
        | detector::THRASH
        | detector::CONTEXT_PRESSURE
        | detector::STALLED
        | detector::OVERLAP
        | detector::SHARED_CHECKOUT => FindingSeverity::Warn,
        _ => FindingSeverity::Info,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn draft(detector: &'static str, key: &str, at: u64) -> Draft {
        Draft {
            detector,
            severity: severity_of(detector),
            confidence: FindingConfidence::Exact,
            workspace: WorkspaceId::new(),
            session: None,
            turn: None,
            key: key.to_owned(),
            title: format!("{detector} {key}"),
            evidence: vec![evidence(EvidenceKind::Stat, None, "x")],
            first_at_ms: at,
            last_at_ms: at,
            count: 1,
            members: Vec::new(),
        }
    }

    #[test]
    fn an_id_is_stable_and_depends_on_every_part() {
        let s = SessionId::new();
        let t = TurnId::new();
        let id = finding_id("overlap", Some(s), Some(t), "k");
        assert_eq!(id, finding_id("overlap", Some(s), Some(t), "k"));
        assert!(hutan_protocol::checks::is_finding_id(&id), "{id}");
        assert_ne!(id, finding_id("thrash", Some(s), Some(t), "k"));
        assert_ne!(
            id,
            finding_id("overlap", Some(SessionId::new()), Some(t), "k")
        );
        assert_ne!(id, finding_id("overlap", Some(s), None, "k"));
        assert_ne!(id, finding_id("overlap", Some(s), Some(t), "k2"));
        // The parts cannot run into each other.
        assert_ne!(
            finding_id("ab", None, None, "c"),
            finding_id("a", None, None, "bc")
        );
    }

    #[test]
    fn evidence_is_redacted_flagged_and_cut() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        let e = evidence(EvidenceKind::Command, None, format!("curl -H {secret}"));
        assert!(!e.excerpt.contains(secret) && e.redacted, "{e:?}");
        let long = evidence(EvidenceKind::Event, None, "word ".repeat(200));
        assert!(long.excerpt.chars().count() <= FINDING_EXCERPT_CHARS + 1);
        assert!(!evidence(EvidenceKind::File, None, "src/a.rs").redacted);
    }

    #[test]
    fn the_answer_sorts_counts_hides_dismissed_and_cuts_to_the_limit() {
        let findings: Vec<Finding> = vec![
            draft(detector::LARGE_DIFF, "a", 10).finish(),
            draft(detector::DONE_AFTER_FAILED_COMMAND, "b", 5).finish(),
            draft(detector::THRASH, "c", 20).finish(),
            draft(detector::THRASH, "d", 30).finish(),
        ];
        let dismissed: HashMap<String, u64> = [(findings[3].id.clone(), 99)].into_iter().collect();
        let req = AgentsFindingsRequest {
            limit: Some(2),
            ..AgentsFindingsRequest::default()
        };
        let answer = assemble(AnswerInputs {
            now_ms: 1_000,
            findings: findings.clone(),
            dismissed: &dismissed,
            req: &req,
            coverage: Vec::new(),
            not_recorded: Vec::new(),
        });
        assert_eq!(
            answer.counts,
            FindingCounts {
                alert: 1,
                warn: 1,
                info: 1
            }
        );
        assert_eq!(answer.dismissed_hidden, 1);
        assert_eq!(answer.omitted, 1);
        let ids: Vec<&str> = answer
            .findings
            .iter()
            .map(|f| f.detector.as_str())
            .collect();
        assert_eq!(ids, [detector::DONE_AFTER_FAILED_COMMAND, detector::THRASH]);

        let all = assemble(AnswerInputs {
            now_ms: 1_000,
            findings,
            dismissed: &dismissed,
            req: &AgentsFindingsRequest {
                include_dismissed: true,
                since_ms: Some(10),
                ..AgentsFindingsRequest::default()
            },
            coverage: Vec::new(),
            not_recorded: Vec::new(),
        });
        // `since` drops the one last seen at 5; the dismissed one is listed and marked.
        assert_eq!(all.findings.len(), 3);
        assert_eq!(all.findings.iter().filter(|f| f.dismissed).count(), 1);
        assert_eq!(all.dismissed_hidden, 0);
        // Counts leave out what was dismissed.
        assert_eq!(all.counts.warn, 1);
    }

    #[test]
    fn a_big_answer_is_cut_to_the_bound_and_says_what_was_cut() {
        let findings: Vec<Finding> = (0..200)
            .map(|n| {
                let mut d = draft(detector::THRASH, &n.to_string(), n);
                d.title = "t".repeat(100);
                d.evidence = (0..5)
                    .map(|_| evidence(EvidenceKind::Stat, None, "e".repeat(150)))
                    .collect();
                d.finish()
            })
            .collect();
        let answer = assemble(AnswerInputs {
            now_ms: 1,
            findings,
            dismissed: &HashMap::new(),
            req: &AgentsFindingsRequest {
                limit: Some(200),
                ..AgentsFindingsRequest::default()
            },
            coverage: Vec::new(),
            not_recorded: Vec::new(),
        });
        assert!(size(&answer) <= FINDINGS_MAX_BYTES, "{}", size(&answer));
        assert!(!answer.cut.is_empty());
        assert!(answer.omitted > 0 && answer.findings.len() + answer.omitted as usize == 200);
        // The newest of the same severity survive.
        assert_eq!(answer.findings[0].last_at_ms, 199);
    }
}
