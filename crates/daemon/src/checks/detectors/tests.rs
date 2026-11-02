//! Every detector on fixture facts: where it fires, where it stays quiet, and the edges.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use hutan_core::insight::{AgentInsight, Exactness, InsightContext, InsightOutcome, InsightTurn};
use hutan_core::{SessionId, TurnId, WorkspaceId};
use hutan_protocol::checkpoint::CheckpointFileStatus;
use hutan_protocol::checks::{FindingConfidence, FindingSeverity};
use hutan_protocol::review::{ReviewCommand, ReviewToolStatus};

use super::*;
use crate::checks::facts::{BlockedSpan, PlacementFact, RequestFact, TaskFact};

const MIN: u64 = 60_000;
const HOUR: u64 = 3_600_000;

fn meta() -> SessionMeta {
    SessionMeta {
        id: SessionId::new(),
        workspace_id: WorkspaceId::new(),
        title: "api: add health endpoint".into(),
        harness: Some("codex".into()),
        chat: true,
        live: true,
        created_ms: 1_000,
        roots: vec![PathBuf::from("/work/api")],
    }
}

fn turn(number: u32) -> TurnFacts {
    TurnFacts {
        turn_id: Some(TurnId::new()),
        number,
        status: ChatTurnStatus::Completed,
        started_ms: 10_000 * u64::from(number),
        ended_ms: Some(10_000 * u64::from(number) + 5_000),
        error: None,
        final_message: None,
        commands: Vec::new(),
        tool_failures: Vec::new(),
        files: TurnFiles::Checkpoint(Vec::new()),
    }
}

fn cmd(text: &str, code: Option<i32>) -> ReviewCommand {
    ReviewCommand {
        text: text.into(),
        status: match code {
            Some(c) if c != 0 => ReviewToolStatus::Failed,
            _ => ReviewToolStatus::Completed,
        },
        exit_code: code,
        duration_ms: None,
    }
}

fn file(path: &str, added: u64, removed: u64, patch: Option<&str>) -> TurnFile {
    TurnFile {
        seen: SeenFile {
            path: path.into(),
            status: CheckpointFileStatus::Modified,
            added: Some(added),
            removed: Some(removed),
            binary: false,
            journal_only: false,
        },
        patch: patch.map(str::to_owned),
    }
}

fn journal(meta: SessionMeta, turns: Vec<TurnFacts>) -> JournalFacts {
    JournalFacts {
        meta,
        turns_total: turns.len(),
        turns,
        insight: Err("none in this test".into()),
    }
}

fn drafts(turns: Vec<TurnFacts>) -> Vec<Draft> {
    journal_drafts(&journal(meta(), turns), &Thresholds::default())
}

fn ids(drafts: &[Draft]) -> Vec<&'static str> {
    drafts.iter().map(|d| d.detector).collect()
}

fn one<'a>(drafts: &'a [Draft], id: &str) -> &'a Draft {
    let found: Vec<&Draft> = drafts.iter().filter(|d| d.detector == id).collect();
    assert_eq!(found.len(), 1, "{id} in {:?}", ids(drafts));
    found[0]
}

// done_after_failed_command ---------------------------------------------------------------------------

#[test]
fn a_turn_that_ends_after_its_last_test_command_failed_is_an_exact_alert() {
    let mut t = turn(3);
    t.commands = vec![cmd("cargo nextest run -p api", Some(101))];
    t.final_message = Some("Done, all tests pass.".into());
    let found = drafts(vec![t.clone()]);
    let d = one(&found, detector::DONE_AFTER_FAILED_COMMAND);
    assert_eq!(d.severity, FindingSeverity::Alert);
    assert_eq!(d.confidence, FindingConfidence::Exact);
    assert_eq!(d.turn, t.turn_id);
    assert!(d.title.contains("Turn 3"), "{}", d.title);
    assert!(d.evidence[0].excerpt.contains("exit 101: cargo nextest"));
    // The claim the turn made is part of the evidence, and the claim alone is not a second finding.
    assert!(d.evidence[1].excerpt.contains("all tests pass"));
    assert!(!ids(&found).contains(&detector::CLAIMED_PASS_WITHOUT_TEST));
}

#[test]
fn a_test_command_that_was_run_again_and_passed_is_not_a_failed_ending() {
    let mut t = turn(1);
    t.commands = vec![
        cmd("cargo test", Some(101)),
        cmd("cargo test", Some(0)),
        cmd("git status", Some(0)),
    ];
    t.final_message = Some("Done.".into());
    let found = drafts(vec![t]);
    assert!(!ids(&found).contains(&detector::DONE_AFTER_FAILED_COMMAND));
}

#[test]
fn a_turn_that_did_not_complete_is_not_a_done() {
    for status in [ChatTurnStatus::Failed, ChatTurnStatus::Cancelled] {
        let mut t = turn(1);
        t.status = status;
        t.commands = vec![cmd("cargo test", Some(101))];
        t.final_message = Some("Done.".into());
        assert!(!ids(&drafts(vec![t])).contains(&detector::DONE_AFTER_FAILED_COMMAND));
    }
    let mut running = turn(1);
    running.status = ChatTurnStatus::Running;
    running.commands = vec![cmd("cargo test", Some(101))];
    assert!(drafts(vec![running]).is_empty());
}

#[test]
fn another_failed_command_and_a_done_message_is_a_heuristic_with_the_same_id_of_detector() {
    let mut t = turn(2);
    t.commands = vec![cmd("cargo build", Some(101)), cmd("cargo test", Some(0))];
    t.final_message = Some("All fixed.".into());
    let found = drafts(vec![t]);
    let d = one(&found, detector::DONE_AFTER_FAILED_COMMAND);
    assert_eq!(d.confidence, FindingConfidence::Heuristic);
    assert!(d.evidence[0].excerpt.contains("cargo build"));
    // Without a message that sounds done there is nothing to say.
    let mut quiet = turn(2);
    quiet.commands = vec![cmd("cargo build", Some(101))];
    quiet.final_message = Some("I could not get it to build.".into());
    assert!(drafts(vec![quiet]).is_empty());
}

// claimed_pass_without_test ---------------------------------------------------------------------------

#[test]
fn a_claim_that_tests_pass_without_a_passing_test_command_is_a_heuristic_warning() {
    let mut t = turn(1);
    t.commands = vec![cmd("rg health", Some(0)), cmd("ls", Some(0))];
    t.final_message = Some("Added the endpoint. All tests pass.".into());
    let found = drafts(vec![t]);
    let d = one(&found, detector::CLAIMED_PASS_WITHOUT_TEST);
    assert_eq!(d.severity, FindingSeverity::Warn);
    assert_eq!(d.confidence, FindingConfidence::Heuristic);
    assert!(d.evidence[0].excerpt.contains("All tests pass"));
    assert!(d.evidence[1].excerpt.contains("2 commands ran"));
}

#[test]
fn a_claim_with_a_passing_or_unreported_test_run_is_not_flagged() {
    let say = |commands: Vec<ReviewCommand>, message: &str| {
        let mut t = turn(1);
        t.commands = commands;
        t.final_message = Some(message.into());
        ids(&drafts(vec![t])).contains(&detector::CLAIMED_PASS_WITHOUT_TEST)
    };
    assert!(!say(vec![cmd("cargo test", Some(0))], "All tests pass."));
    // A harness that reports no exit code proves nothing either way: no claim is made about it.
    assert!(!say(vec![cmd("cargo test", None)], "All tests pass."));
    // A build claim is satisfied by a build, a check or a test that passed.
    assert!(!say(
        vec![cmd("cargo clippy", Some(0))],
        "The build passes."
    ));
    assert!(!say(vec![cmd("cargo test", Some(0))], "The build passes."));
    assert!(say(vec![cmd("ls", Some(0))], "The build passes."));
    // No claim, no finding.
    assert!(!say(Vec::new(), "Added the endpoint."));
    assert!(!say(Vec::new(), "the tests don't pass yet"));
}

// Files ---------------------------------------------------------------------------------------------

#[test]
fn test_files_edited_together_with_source_code_are_an_exact_info() {
    let mut t = turn(4);
    t.files = TurnFiles::Checkpoint(vec![
        file("src/health.rs", 20, 0, None),
        file("tests/it/health.rs", 12, 1, None),
        file("tests/it/other.rs", 3, 0, None),
    ]);
    let found = drafts(vec![t]);
    let d = one(&found, detector::TESTS_EDITED);
    assert_eq!(d.severity, FindingSeverity::Info);
    assert_eq!(d.confidence, FindingConfidence::Exact);
    assert_eq!(d.count, 2);
    assert!(
        d.title.contains("2 test files") && d.title.contains("1 source file"),
        "{}",
        d.title
    );
    assert_eq!(
        d.evidence[0].reference.as_deref(),
        Some("tests/it/health.rs")
    );
}

#[test]
fn tests_alone_or_code_alone_or_snapshots_are_not_tests_edited() {
    for files in [
        vec![file("tests/it/health.rs", 12, 1, None)],
        vec![file("src/health.rs", 20, 0, None)],
        vec![
            file("src/health.rs", 20, 0, None),
            file("tests/snapshots/health.snap", 4, 4, None),
        ],
        vec![
            file("src/health.rs", 20, 0, None),
            file("README.md", 2, 0, None),
        ],
    ] {
        let mut t = turn(1);
        t.files = TurnFiles::Checkpoint(files);
        assert!(!ids(&drafts(vec![t])).contains(&detector::TESTS_EDITED));
    }
    // Files the journal only named count too: the files are known, the lines are not.
    let mut t = turn(1);
    t.files = TurnFiles::Journal(vec![
        file("src/a.rs", 0, 0, None),
        file("tests/a.rs", 0, 0, None),
    ]);
    assert!(ids(&drafts(vec![t])).contains(&detector::TESTS_EDITED));
}

#[test]
fn a_skipped_test_is_an_alert_and_removed_assertions_a_warning() {
    let skip = "@@ -1,3 +1,4 @@\n+#[ignore = \"flaky\"]\n #[test]\n fn a() {}\n";
    let mut t = turn(1);
    t.files = TurnFiles::Checkpoint(vec![file("tests/a.rs", 1, 0, Some(skip))]);
    let found = drafts(vec![t]);
    let d = one(&found, detector::TESTS_WEAKENED);
    assert_eq!(d.severity, FindingSeverity::Alert);
    assert_eq!(d.confidence, FindingConfidence::Heuristic);
    assert!(
        d.evidence[0].excerpt.contains("#[ignore"),
        "{:?}",
        d.evidence
    );

    let removed = "@@ -1,4 +1,2 @@\n-    assert_eq!(a(), 1);\n-    assert_eq!(b(), 2);\n+    assert_eq!(a(), 1);\n";
    let mut t = turn(2);
    t.files = TurnFiles::Checkpoint(vec![file("tests/b.rs", 1, 2, Some(removed))]);
    let found = drafts(vec![t]);
    let d = one(&found, detector::TESTS_WEAKENED);
    assert_eq!(d.severity, FindingSeverity::Warn);
    assert!(
        d.evidence[0].excerpt.contains("1 assertion removed")
            || d.evidence[0].excerpt.contains("assertion")
    );
}

#[test]
fn a_rewritten_assertion_a_source_file_or_a_missing_patch_do_not_weaken_a_test() {
    let rewrite = "@@ -1 +1 @@\n-    assert_eq!(a(), 1);\n+    assert_eq!(a(), 2);\n";
    let mut t = turn(1);
    t.files = TurnFiles::Checkpoint(vec![
        file("tests/a.rs", 1, 1, Some(rewrite)),
        // `#[ignore]` in a source file is not a test.
        file("src/lib.rs", 1, 0, Some("+#[ignore]\n")),
        file("tests/nopatch.rs", 5, 5, None),
    ]);
    assert!(!ids(&drafts(vec![t])).contains(&detector::TESTS_WEAKENED));
}

#[test]
fn rewritten_snapshots_goldens_and_fixtures_are_an_exact_warning() {
    let mut t = turn(5);
    t.files = TurnFiles::Checkpoint(vec![
        file("web/__snapshots__/app.snap", 40, 38, None),
        file(
            "crates/protocol/tests/fixtures/v0/response_x.json",
            1,
            1,
            None,
        ),
        file("src/lib.rs", 2, 2, None),
    ]);
    let found = drafts(vec![t]);
    let d = one(&found, detector::SNAPSHOT_UPDATED);
    assert_eq!(d.severity, FindingSeverity::Warn);
    assert_eq!(d.count, 2);
    let mut none = turn(5);
    none.files = TurnFiles::Checkpoint(vec![file("src/lib.rs", 2, 2, None)]);
    assert!(!ids(&drafts(vec![none])).contains(&detector::SNAPSHOT_UPDATED));
}

#[test]
fn the_signs_of_the_review_packet_become_findings_of_their_own_names() {
    let mut t = turn(1);
    let mut deleted = file("src/old.rs", 0, 30, None);
    deleted.seen.status = CheckpointFileStatus::Deleted;
    let mut outside = file("/etc/hosts", 0, 0, None);
    outside.seen.journal_only = true;
    t.files = TurnFiles::Checkpoint(vec![
        file("Cargo.toml", 1, 0, None),
        deleted,
        file(".env", 1, 0, None),
        file("src/big.rs", 600, 10, None),
        outside,
    ]);
    t.commands = vec![cmd("ls", Some(0))];
    let found = drafts(vec![t]);
    for id in [
        detector::DEPENDENCY_FILE_CHANGED,
        detector::FILE_DELETED,
        detector::SECRET_LIKE_PATH,
        detector::LARGE_DIFF,
        detector::PATH_OUTSIDE_FOLDER,
        detector::NO_TEST_COMMAND,
    ] {
        let d = one(&found, id);
        assert_eq!(d.confidence, FindingConfidence::Exact, "{id}");
        assert!(!d.evidence.is_empty(), "{id}");
    }
    assert_eq!(
        one(&found, detector::PATH_OUTSIDE_FOLDER).severity,
        FindingSeverity::Warn
    );
    assert_eq!(
        one(&found, detector::FILE_DELETED).severity,
        FindingSeverity::Info
    );
    // A test run silences `no_test_command`; no files, nothing to say about them.
    let mut ran = turn(1);
    ran.files = TurnFiles::Checkpoint(vec![file("src/a.rs", 1, 0, None)]);
    ran.commands = vec![cmd("cargo test", Some(0))];
    assert!(drafts(vec![ran]).is_empty());
    let mut missing = turn(1);
    missing.files = TurnFiles::Missing("no checkpoint".into());
    assert!(drafts(vec![missing]).is_empty());
}

// repeated_failure ----------------------------------------------------------------------------------

#[test]
fn the_same_failing_command_three_times_is_a_repeated_failure_with_an_exact_count() {
    let mut turns = Vec::new();
    for n in 1..=3 {
        let mut t = turn(n);
        // The numbers differ; the failure reads the same.
        t.commands = vec![cmd(&format!("cargo test -p api --jobs {n}"), Some(101))];
        turns.push(t);
    }
    let found = drafts(turns.clone());
    let d = one(&found, detector::REPEATED_FAILURE);
    assert_eq!(d.count, 3);
    assert_eq!(d.confidence, FindingConfidence::Heuristic);
    assert_eq!(d.evidence.len(), 3);
    assert_eq!(d.first_at_ms, turns[0].at_ms());
    assert_eq!(d.last_at_ms, turns[2].at_ms());
    // Two are not enough; the threshold is a setting.
    let two = journal(meta(), turns[..2].to_vec());
    assert!(
        journal_drafts(&two, &Thresholds::default())
            .iter()
            .all(|d| d.detector != detector::REPEATED_FAILURE)
    );
    let lenient = Thresholds {
        repeated_failure_min: 2,
        ..Thresholds::default()
    };
    assert!(
        journal_drafts(&two, &lenient)
            .iter()
            .any(|d| d.detector == detector::REPEATED_FAILURE)
    );
}

#[test]
fn failing_tools_and_failed_turns_with_the_same_error_are_counted_too() {
    let mut turns = Vec::new();
    for n in 1..=3 {
        let mut t = turn(n);
        t.status = ChatTurnStatus::Failed;
        t.error = Some(format!("rate limited, retry in {n}0 s"));
        t.tool_failures = vec![crate::checks::facts::ToolFailure {
            name: "edit".into(),
            text: format!("no match for the text at line {n}"),
        }];
        turns.push(t);
    }
    let found = drafts(turns);
    let titles: Vec<&str> = found
        .iter()
        .filter(|d| d.detector == detector::REPEATED_FAILURE)
        .map(|d| d.title.as_str())
        .collect();
    assert_eq!(titles.len(), 2, "{titles:?}");
    assert!(titles.iter().any(|t| t.contains("tool call")));
    assert!(titles.iter().any(|t| t.contains("error ended 3 turns")));
}

#[test]
fn different_failures_and_declined_commands_are_not_one_repeated_failure() {
    let mut turns = Vec::new();
    for (n, text) in [(1, "cargo test"), (2, "npm test"), (3, "pytest -q")] {
        let mut t = turn(n);
        t.commands = vec![cmd(text, Some(1))];
        turns.push(t);
    }
    assert!(
        drafts(turns)
            .iter()
            .all(|d| d.detector != detector::REPEATED_FAILURE)
    );
    let mut declined = Vec::new();
    for n in 1..=3 {
        let mut t = turn(n);
        t.commands = vec![ReviewCommand {
            text: "rm -rf x".into(),
            status: ReviewToolStatus::Declined,
            exit_code: None,
            duration_ms: None,
        }];
        declined.push(t);
    }
    assert!(
        drafts(declined)
            .iter()
            .all(|d| d.detector != detector::REPEATED_FAILURE)
    );
}

// thrash --------------------------------------------------------------------------------------------

#[test]
fn a_file_changed_in_three_turns_with_a_net_change_near_zero_is_thrash() {
    let mut turns = Vec::new();
    for (n, (a, r)) in [(1, (30, 2)), (2, (4, 30)), (3, (28, 3))] {
        let mut t = turn(n);
        t.files = TurnFiles::Checkpoint(vec![file("src/x.rs", a, r, None)]);
        turns.push(t);
    }
    // +62 −35 is a net of 27 over 97 lines of churn: not near zero.
    assert!(
        drafts(turns.clone())
            .iter()
            .all(|d| !d.key.starts_with("net:"))
    );
    turns[2].files = TurnFiles::Checkpoint(vec![file("src/x.rs", 4, 3, None)]);
    // +38 −35: a net of 3 over 73.
    let found = drafts(turns);
    let d = found
        .iter()
        .find(|d| d.key == "net:src/x.rs")
        .expect("thrash");
    assert_eq!(d.detector, detector::THRASH);
    assert_eq!(d.count, 3);
}

#[test]
fn a_turn_that_undoes_the_one_before_is_thrash_and_growth_is_not() {
    let mut a = turn(1);
    a.files = TurnFiles::Checkpoint(vec![file("src/x.rs", 12, 3, None)]);
    let mut b = turn(2);
    b.files = TurnFiles::Checkpoint(vec![file("src/x.rs", 3, 12, None)]);
    let found = drafts(vec![a.clone(), b]);
    let d = found
        .iter()
        .find(|d| d.detector == detector::THRASH)
        .expect("revert");
    assert!(
        d.title.contains("Turn 2 undid the change of turn 1"),
        "{}",
        d.title
    );
    // The same numbers two turns apart are not an undo; a growing file is no thrash.
    let mut c = turn(3);
    c.files = TurnFiles::Checkpoint(vec![file("src/x.rs", 3, 12, None)]);
    assert!(
        drafts(vec![a, turn(2), c])
            .iter()
            .all(|d| d.detector != detector::THRASH)
    );
    let growing: Vec<TurnFacts> = (1..=4)
        .map(|n| {
            let mut t = turn(n);
            t.files = TurnFiles::Checkpoint(vec![file("src/x.rs", 40, 1, None)]);
            t
        })
        .collect();
    assert!(
        drafts(growing)
            .iter()
            .all(|d| d.detector != detector::THRASH)
    );
    // Without exact lines there is no thrash to see.
    let journal_only: Vec<TurnFacts> = (1..=3)
        .map(|n| {
            let mut t = turn(n);
            t.files = TurnFiles::Journal(vec![file("src/x.rs", 0, 0, None)]);
            t
        })
        .collect();
    assert!(
        drafts(journal_only)
            .iter()
            .all(|d| d.detector != detector::THRASH)
    );
}

// Insight ------------------------------------------------------------------------------------------

fn insight_turn(n: u64, tokens: Option<u64>, context_after: Option<u64>) -> InsightTurn {
    InsightTurn {
        n,
        turn_id: Some(TurnId::new()),
        started_ms: 1_000 * n,
        duration_ms: Some(500),
        outcome: InsightOutcome::Completed,
        input: tokens,
        output: tokens.map(|_| 0),
        cache_read: None,
        cache_write: None,
        reasoning: None,
        cost_usd: None,
        context_after,
        tool_calls: 0,
        tool_failed: 0,
        files: 0,
        checkpoint: None,
    }
}

fn insight(used: u64, window: Option<u64>, turns: Vec<InsightTurn>) -> AgentInsight {
    let mut insight = AgentInsight::empty(SessionId::new(), 1, 99_000);
    insight.context = Some(InsightContext {
        used,
        window,
        exactness: Exactness::Reported,
        as_of_ms: 98_000,
        breakdown: Vec::new(),
        breakdown_as_of_ms: None,
        breakdown_exactness: None,
        compact_at: None,
    });
    insight.turns = turns;
    insight
}

fn with_insight(insight: AgentInsight) -> Vec<Draft> {
    let mut j = journal(meta(), Vec::new());
    j.insight = Ok(insight);
    journal_drafts(&j, &Thresholds::default())
}

#[test]
fn a_context_at_85_percent_of_the_window_is_pressure_and_below_it_is_not() {
    let found = with_insight(insight(180_000, Some(200_000), Vec::new()));
    let d = one(&found, detector::CONTEXT_PRESSURE);
    assert_eq!(d.confidence, FindingConfidence::Exact);
    assert!(d.title.contains("90%"), "{}", d.title);
    assert!(with_insight(insight(169_000, Some(200_000), Vec::new())).is_empty());
    // A window nobody reported says nothing; coverage says why.
    assert!(with_insight(insight(900_000, None, Vec::new())).is_empty());
}

#[test]
fn a_context_that_shrank_between_turns_is_a_heuristic_compaction() {
    let turns = vec![
        insight_turn(1, None, Some(150_000)),
        insight_turn(2, None, Some(40_000)),
        insight_turn(3, None, Some(60_000)),
    ];
    let found = with_insight(insight(60_000, Some(200_000), turns));
    let d = one(&found, detector::CONTEXT_PRESSURE);
    assert_eq!(d.confidence, FindingConfidence::Heuristic);
    assert_eq!(d.severity, FindingSeverity::Warn);
    assert!(d.title.contains("turn 2"), "{}", d.title);
    // A small context that shrinks is not worth a word.
    let small = vec![
        insight_turn(1, None, Some(15_000)),
        insight_turn(2, None, Some(2_000)),
    ];
    assert!(with_insight(insight(2_000, Some(200_000), small)).is_empty());
}

#[test]
fn a_turn_of_three_times_the_median_and_50k_tokens_is_costly() {
    let tokens = [30_000, 40_000, 35_000, 400_000, 38_000];
    let turns: Vec<InsightTurn> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| insight_turn(i as u64 + 1, Some(*t), None))
        .collect();
    let found = with_insight(insight(10_000, Some(200_000), turns));
    let d = one(&found, detector::COSTLY_TURN);
    assert_eq!(d.confidence, FindingConfidence::Exact);
    assert!(
        d.evidence[0].excerpt.contains("400k"),
        "{}",
        d.evidence[0].excerpt
    );
    assert!(d.title.starts_with("Turn 4"), "{}", d.title);

    // Three times the median but under 50k is not costly; fewer than four turns have no median.
    let small: Vec<InsightTurn> = [1_000, 1_200, 1_100, 9_000]
        .iter()
        .enumerate()
        .map(|(i, t)| insight_turn(i as u64 + 1, Some(*t), None))
        .collect();
    assert!(with_insight(insight(1, Some(200_000), small)).is_empty());
    let few: Vec<InsightTurn> = [10_000, 10_000, 900_000]
        .iter()
        .enumerate()
        .map(|(i, t)| insight_turn(i as u64 + 1, Some(*t), None))
        .collect();
    assert!(with_insight(insight(1, Some(200_000), few)).is_empty());
}

// Live ---------------------------------------------------------------------------------------------

fn live(meta: SessionMeta, now: u64) -> LiveFacts {
    LiveFacts {
        meta,
        now_ms: now,
        turn_running: false,
        last_event_ms: None,
        working_since_ms: None,
        pending_request: false,
        requests: Vec::new(),
        blocked_spans: Vec::new(),
        state_log: true,
        task: None,
        changed_at_ms: None,
        placement: None,
        files: None,
    }
}

fn request(asked: u64, resolved: Option<u64>) -> RequestFact {
    RequestFact {
        id: format!("req{asked}"),
        kind: "approval",
        title: "git push origin main".into(),
        asked_ms: asked,
        resolved_ms: resolved,
    }
}

#[test]
fn a_long_wait_of_a_chat_is_exact_and_a_wait_that_goes_on_is_a_warning() {
    let mut l = live(meta(), 10 * HOUR);
    l.requests = vec![
        request(1_000, Some(1_000 + 42 * MIN)),
        request(2_000_000, Some(2_000_000 + 14 * MIN)),
        request(10 * HOUR - 30 * MIN, None),
    ];
    let found = live_drafts(&l, &Thresholds::default());
    let waits: Vec<&Draft> = found
        .iter()
        .filter(|d| d.detector == detector::LONG_WAIT)
        .collect();
    assert_eq!(waits.len(), 2);
    let resolved = waits
        .iter()
        .find(|d| d.severity == FindingSeverity::Info)
        .unwrap();
    assert!(
        resolved.title.contains("Waited 42 min"),
        "{}",
        resolved.title
    );
    let open = waits
        .iter()
        .find(|d| d.severity == FindingSeverity::Warn)
        .unwrap();
    assert!(open.title.contains("still waiting"), "{}", open.title);
    assert!(
        waits
            .iter()
            .all(|d| d.confidence == FindingConfidence::Exact)
    );
    // A wait of a session that ended with the question open is not counted as going on.
    let mut gone = live(
        {
            let mut m = meta();
            m.live = false;
            m
        },
        10 * HOUR,
    );
    gone.requests = vec![request(1_000, None)];
    assert!(live_drafts(&gone, &Thresholds::default()).is_empty());
}

#[test]
fn a_terminal_agent_blocked_for_long_is_a_heuristic_from_the_state_log_only() {
    let mut m = meta();
    m.chat = false;
    let mut l = live(m, 5 * HOUR);
    l.blocked_spans = vec![
        BlockedSpan {
            start_ms: 1_000,
            end_ms: Some(1_000 + 30 * MIN),
            kind: Some("permission".into()),
        },
        BlockedSpan {
            start_ms: 2_000_000,
            end_ms: Some(2_000_000 + 5 * MIN),
            kind: None,
        },
    ];
    let found = live_drafts(&l, &Thresholds::default());
    let d = one(&found, detector::LONG_WAIT);
    assert_eq!(d.confidence, FindingConfidence::Heuristic);
    assert!(
        d.title.contains("30 min") && d.title.contains("state log"),
        "{}",
        d.title
    );
    // With the log off its spans are not read.
    l.state_log = false;
    assert!(live_drafts(&l, &Thresholds::default()).is_empty());
}

#[test]
fn a_chat_that_works_without_an_event_for_20_minutes_is_stalled_unless_it_waits_for_the_user() {
    let now = 5 * HOUR;
    let mut l = live(meta(), now);
    l.turn_running = true;
    l.last_event_ms = Some(now - 34 * MIN);
    let found = live_drafts(&l, &Thresholds::default());
    let d = one(&found, detector::STALLED);
    assert_eq!(d.severity, FindingSeverity::Warn);
    assert_eq!(d.confidence, FindingConfidence::Exact);
    assert!(d.title.contains("34 min"), "{}", d.title);
    // The id keys on the last event, so a new event is a new stall and the same quiet the same one.
    assert_eq!(d.key, format!("quiet@{}", now - 34 * MIN));

    l.last_event_ms = Some(now - 19 * MIN);
    assert!(live_drafts(&l, &Thresholds::default()).is_empty());
    l.last_event_ms = Some(now - 34 * MIN);
    l.pending_request = true;
    assert!(live_drafts(&l, &Thresholds::default()).is_empty());
    l.pending_request = false;
    l.turn_running = false;
    assert!(live_drafts(&l, &Thresholds::default()).is_empty());
}

#[test]
fn a_terminal_agent_that_says_it_works_is_unconfirmed_never_stuck() {
    let now = 5 * HOUR;
    let mut m = meta();
    m.chat = false;
    let mut l = live(m, now);
    l.working_since_ms = Some(now - 45 * MIN);
    let found = live_drafts(&l, &Thresholds::default());
    let d = one(&found, detector::STALLED);
    assert_eq!(d.severity, FindingSeverity::Info);
    assert_eq!(d.confidence, FindingConfidence::Heuristic);
    assert!(d.title.contains("unconfirmed"), "{}", d.title);
    assert!(!d.title.to_lowercase().contains("stuck"));
    l.working_since_ms = Some(now - 5 * MIN);
    assert!(live_drafts(&l, &Thresholds::default()).is_empty());
}

#[test]
fn changes_of_a_task_that_wait_for_review_for_four_hours_are_unreviewed() {
    let now = 20 * HOUR;
    let mut l = live(meta(), now);
    l.task = Some(TaskFact {
        title: "health".into(),
        awaiting_since_ms: now - 5 * HOUR,
    });
    l.changed_at_ms = Some(now - 6 * HOUR);
    let found = live_drafts(&l, &Thresholds::default());
    let d = one(&found, detector::UNREVIEWED_CHANGES);
    assert_eq!(d.confidence, FindingConfidence::Exact);
    assert!(
        d.title.contains("5 h") || d.title.contains("300 min"),
        "{}",
        d.title
    );
    // Not yet four hours, no finished change, or no task: nothing.
    l.task = Some(TaskFact {
        title: "health".into(),
        awaiting_since_ms: now - 3 * HOUR,
    });
    assert!(live_drafts(&l, &Thresholds::default()).is_empty());
    l.task = Some(TaskFact {
        title: "health".into(),
        awaiting_since_ms: now - 5 * HOUR,
    });
    l.changed_at_ms = None;
    assert!(live_drafts(&l, &Thresholds::default()).is_empty());
    l.task = None;
    l.changed_at_ms = Some(now - 6 * HOUR);
    assert!(live_drafts(&l, &Thresholds::default()).is_empty());
}

// Across sessions -----------------------------------------------------------------------------------

fn session_in(root: &str, files: &[(&str, &str)], shared_with: Vec<SessionId>) -> LiveFacts {
    let mut l = live(meta(), 100_000);
    l.placement = Some(PlacementFact {
        root: PathBuf::from(root),
        shared_with,
    });
    l.files = Some(
        files
            .iter()
            .map(|(repo, path)| ChangedFile {
                repo: (*repo).into(),
                path: (*path).into(),
            })
            .collect(),
    );
    l
}

#[test]
fn two_live_sessions_changing_one_file_overlap_and_in_one_checkout_it_is_an_alert() {
    let a = session_in(
        "/work/api",
        &[("/work/api", "src/health.rs"), ("/work/api", "src/a.rs")],
        Vec::new(),
    );
    let b = session_in("/work/api", &[("/work/api", "src/health.rs")], Vec::new());
    let found = workspace_drafts(&[&a, &b]);
    let d = one(&found, detector::OVERLAP);
    assert_eq!(d.severity, FindingSeverity::Alert);
    assert_eq!(d.confidence, FindingConfidence::Exact);
    assert_eq!(d.count, 2);
    assert_eq!(
        d.members,
        vec![a.meta.id, b.meta.id]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert!(
        d.evidence
            .iter()
            .all(|e| e.excerpt.contains("src/health.rs"))
    );
    assert_eq!(d.workspace, a.meta.workspace_id);

    // A worktree of the same repository names the same file, in another checkout: a warning.
    let c = session_in(
        "/work/api-wt",
        &[("/work/api", "src/health.rs")],
        Vec::new(),
    );
    let found = workspace_drafts(&[&a, &c]);
    assert_eq!(
        one(&found, detector::OVERLAP).severity,
        FindingSeverity::Warn
    );
}

#[test]
fn different_files_other_repositories_and_one_session_alone_do_not_overlap() {
    let a = session_in("/work/api", &[("/work/api", "src/a.rs")], Vec::new());
    let b = session_in("/work/api", &[("/work/api", "src/b.rs")], Vec::new());
    assert!(workspace_drafts(&[&a, &b]).is_empty());
    let other = session_in("/work/web", &[("/work/web", "src/a.rs")], Vec::new());
    assert!(workspace_drafts(&[&a, &other]).is_empty());
    assert!(workspace_drafts(&[&a]).is_empty());
    // A session that reports no files cannot overlap.
    let mut unknown = session_in("/work/api", &[], Vec::new());
    unknown.files = None;
    assert!(workspace_drafts(&[&a, &unknown]).is_empty());
}

#[test]
fn two_agents_in_one_checkout_are_a_shared_checkout() {
    let mut a = session_in("/work/api", &[], Vec::new());
    let b = session_in("/work/api", &[], vec![a.meta.id]);
    a.placement.as_mut().unwrap().shared_with = vec![b.meta.id];
    let alone = session_in("/work/web", &[], Vec::new());
    let found = workspace_drafts(&[&a, &b, &alone]);
    let d = one(&found, detector::SHARED_CHECKOUT);
    assert_eq!(d.severity, FindingSeverity::Warn);
    assert_eq!(d.count, 2);
    assert_eq!(d.key, "/work/api");
    assert!(d.title.contains("2 agents"), "{}", d.title);
}

// Ids and coverage ---------------------------------------------------------------------------------

#[test]
fn the_same_problem_keeps_its_id_across_reads_and_a_new_turn_does_not_change_old_ids() {
    let mut a = turn(1);
    a.commands = vec![cmd("cargo test", Some(101))];
    a.final_message = Some("done".into());
    let mut b = turn(2);
    b.files = TurnFiles::Checkpoint(vec![
        file("src/a.rs", 1, 0, None),
        file("tests/a.rs", 1, 0, None),
    ]);
    let m = meta();
    let read = |turns: Vec<TurnFacts>| -> Vec<String> {
        journal_drafts(&journal(m.clone(), turns), &Thresholds::default())
            .into_iter()
            .map(|d| d.finish().id)
            .collect()
    };
    let before = read(vec![a.clone(), b.clone()]);
    assert_eq!(before, read(vec![a.clone(), b.clone()]));
    let mut c = turn(3);
    c.files = TurnFiles::Checkpoint(vec![file("src/z.rs", 1, 0, None)]);
    let after = read(vec![a, b, c]);
    for id in &before {
        assert!(after.contains(id), "{id} survives a later turn");
    }
    assert!(after.len() > before.len());
}

#[test]
fn coverage_says_what_a_terminal_agent_cannot_be_checked_for() {
    let mut m = meta();
    m.chat = false;
    m.harness = Some("claude".into());
    let l = live(m, 1);
    let c = coverage(&l, JournalCoverage::TerminalAgent);
    assert!(!c.chat);
    for id in [
        detector::TESTS_WEAKENED,
        detector::REPEATED_FAILURE,
        detector::COSTLY_TURN,
        detector::OVERLAP,
    ] {
        assert!(!c.ran.iter().any(|r| r == id), "{id} cannot run");
    }
    for id in [detector::STALLED, detector::LONG_WAIT] {
        assert!(c.ran.iter().any(|r| r == id), "{id} runs on the state log");
    }
    let gap = c
        .gaps
        .iter()
        .find(|g| g.detectors.iter().any(|d| d == detector::TESTS_WEAKENED))
        .unwrap();
    assert!(
        gap.why.contains("terminal agent: no tool events"),
        "{}",
        gap.why
    );
    // With the state log off even its waits are not recorded.
    let mut off = live(
        {
            let mut m = meta();
            m.chat = false;
            m
        },
        1,
    );
    off.state_log = false;
    let c = coverage(&off, JournalCoverage::TerminalAgent);
    assert!(!c.ran.iter().any(|r| r == detector::LONG_WAIT));
}

#[test]
fn coverage_of_a_chat_names_each_thing_that_is_missing_and_why() {
    let mut blind = turn(1);
    blind.files = TurnFiles::Missing("no checkpoint".into());
    let mut with_test = turn(2);
    with_test.files = TurnFiles::Checkpoint(vec![file("tests/a.rs", 1, 0, None)]);
    let j = journal(meta(), vec![blind, with_test]);
    let gaps = journal_gaps(&j);
    // Every reason a detector could not run, joined.
    let why = |id: &str| {
        let reasons: Vec<&str> = gaps
            .iter()
            .filter(|g| g.detectors.iter().any(|d| d == id))
            .map(|g| g.why.as_str())
            .collect();
        (!reasons.is_empty()).then(|| reasons.join(" | "))
    };
    assert!(
        why(detector::TESTS_EDITED)
            .unwrap()
            .contains("1 of 2 turns")
    );
    assert!(
        why(detector::TESTS_WEAKENED)
            .unwrap()
            .contains("without a diff")
    );
    assert!(
        why(detector::COSTLY_TURN)
            .unwrap()
            .contains("none in this test")
    );

    let mut healthy = journal(meta(), vec![turn(1)]);
    healthy.insight = Ok(insight(
        10,
        Some(200_000),
        vec![insight_turn(1, Some(1_000), Some(10))],
    ));
    assert!(journal_gaps(&healthy).is_empty());

    let mut l = live(meta(), 1);
    l.files = Some(Vec::new());
    l.placement = Some(PlacementFact {
        root: "/work/api".into(),
        shared_with: Vec::new(),
    });
    l.task = Some(TaskFact {
        title: "t".into(),
        awaiting_since_ms: 0,
    });
    let c = coverage(&l, JournalCoverage::Read(&[]));
    assert_eq!(c.ran.len(), detector::ALL.len());
    assert!(c.gaps.is_empty());
    let c = coverage(&l, JournalCoverage::Unreadable("disk"));
    assert!(c.gaps[0].why.contains("disk"));
}
