//! What the Claude Code and Codex adapters read of a transcript for insight (INSIGHT-C,
//! `Adapter::usage_line`): tokens counted once, the context, cost, tools with times and
//! failures, files, turn edges — from synthetic transcripts in the observed shapes
//! (`tests/fixtures/usage/`; `docs/integrations.md` § Insight says which shapes were recorded
//! and which are built from the agents' public source).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hutan_index::Source;
use hutan_index::adapters::adapter_for;
use hutan_index::adapters::usage::{Counts, FileKind, UsageEvent, UsageState};

const CLAUDE: &str = include_str!("../fixtures/usage/claude-session.jsonl");
const CODEX: &str = include_str!("../fixtures/usage/codex-rollout.jsonl");

/// 2026-11-03T10:00:00Z.
const T0: u64 = 1_793_700_000_000;

fn read(source: Source, text: &str) -> (Vec<UsageEvent>, UsageState) {
    let adapter = adapter_for(source).unwrap();
    let mut state = UsageState::default();
    let mut events = Vec::new();
    for line in text.lines() {
        assert!(adapter.usage_line(line, &mut state, &mut events), "{line}");
    }
    (events, state)
}

fn tokens(events: &[UsageEvent]) -> Vec<Counts> {
    events
        .iter()
        .filter_map(|event| match event {
            UsageEvent::Tokens { counts, .. } => Some(*counts),
            _ => None,
        })
        .collect()
}

fn sum(counts: &[Counts], field: fn(&Counts) -> Option<u64>) -> u64 {
    counts.iter().filter_map(field).sum()
}

fn tools(events: &[UsageEvent]) -> Vec<(String, bool, Option<u64>)> {
    events
        .iter()
        .filter_map(|event| match event {
            UsageEvent::Tool {
                name, failed, ms, ..
            } => Some((name.clone(), *failed, *ms)),
            _ => None,
        })
        .collect()
}

fn files(events: &[UsageEvent]) -> Vec<(String, FileKind)> {
    events
        .iter()
        .filter_map(|event| match event {
            UsageEvent::File { path, kind, .. } => Some((path.clone(), *kind)),
            _ => None,
        })
        .collect()
}

fn contexts(events: &[UsageEvent]) -> Vec<(u64, Option<u64>)> {
    events
        .iter()
        .filter_map(|event| match event {
            UsageEvent::Context { used, window, .. } => Some((*used, *window)),
            _ => None,
        })
        .collect()
}

fn edges(events: &[UsageEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            UsageEvent::TurnStart { ts_ms } => Some(format!("start {}", ts_ms.unwrap() - T0)),
            UsageEvent::TurnEnd {
                outcome,
                duration_ms,
                ..
            } => Some(format!("end {outcome:?} {}", duration_ms.unwrap_or(0))),
            _ => None,
        })
        .collect()
}

#[test]
fn a_claude_transcript_gives_tokens_once_the_context_tools_with_times_files_and_turns() {
    let (events, state) = read(Source::Claude, CLAUDE);
    let counts = tokens(&events);
    // Six messages; the first was split over two lines (the second adds 28 output tokens to
    // what the first said) and a subagent's line is not the main agent's.
    assert_eq!(counts.len(), 6, "{counts:?}");
    assert_eq!(sum(&counts, |c| c.input), 3 + 4 + 4 + 5 + 3);
    assert_eq!(sum(&counts, |c| c.output), 12 + 28 + 80 + 150 + 200 + 30);
    assert_eq!(
        sum(&counts, |c| c.cache_read),
        12_000 + 12_503 + 12_600 + 12_760 + 13_000
    );
    assert_eq!(sum(&counts, |c| c.cache_write), 500);
    assert!(
        !counts.iter().any(|c| c.output == Some(9_000)),
        "a sidechain's usage is not counted"
    );
    assert_eq!(
        counts[1],
        Counts {
            input: Some(0),
            cache_read: Some(0),
            cache_write: Some(0),
            output: Some(28),
            reasoning: None,
            total: Some(28),
        },
        "the second line of a message is what it adds"
    );

    // The context is what the request read; `/context`'s own record gives the window.
    let contexts = contexts(&events);
    assert_eq!(contexts[0], (12_503, None));
    assert_eq!(contexts[contexts.len() - 2], (13_003, None));
    assert_eq!(contexts[contexts.len() - 1], (16_964, Some(1_000_000)));

    assert_eq!(
        tools(&events),
        [
            ("Bash".to_owned(), false, Some(2_300)),
            ("Edit".to_owned(), false, Some(400)),
            ("Write".to_owned(), false, Some(100)),
            ("Bash".to_owned(), true, Some(3_000)),
        ]
    );
    assert_eq!(
        files(&events),
        [
            ("/work/app/src/lib.rs".to_owned(), FileKind::Update),
            ("/work/app/src/new.rs".to_owned(), FileKind::Add),
        ]
    );
    assert_eq!(
        edges(&events),
        [
            "start 0",
            "end Completed 12000",
            "start 60000",
            "end Cancelled 6000"
        ]
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UsageEvent::Cost { usd, .. } if (*usd - 0.42).abs() < 1e-9))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UsageEvent::ApiTime { ms: 9_100, .. }))
    );
    let breakdown = events
        .iter()
        .find_map(|event| match event {
            UsageEvent::Breakdown { rows, .. } => Some(rows),
            _ => None,
        })
        .expect("the /context record");
    assert_eq!(breakdown.len(), 6);
    assert_eq!(
        (
            breakdown[4].name.as_str(),
            breakdown[4].tokens,
            breakdown[4].kind.as_str()
        ),
        ("Autocompact buffer", 33_000, "buffer")
    );
    // Who the conversation is, said once each.
    let identity: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            UsageEvent::Identity {
                session_id,
                cwd,
                model,
            } => Some((session_id.clone(), cwd.clone(), model.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(identity.len(), 3, "{identity:?}");
    assert_eq!(
        identity[0].0.as_deref(),
        Some("5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60")
    );
    assert_eq!(identity[1].1.as_deref(), Some("/work/app"));
    assert_eq!(identity[2].2.as_deref(), Some("claude-opus-5-5"));
    assert!(!state.turn_open(), "the last turn was interrupted");
}

#[test]
fn a_codex_rollout_gives_the_growth_of_the_total_the_last_request_as_context_and_exit_codes() {
    let (events, state) = read(Source::Codex, CODEX);
    let counts = tokens(&events);
    // Four `token_count` events carry usage; the repeat of the second adds nothing and the
    // one without `info` says nothing.
    assert_eq!(counts.len(), 3, "{counts:?}");
    assert_eq!(
        counts[1],
        Counts {
            input: Some(1_400),
            cache_read: Some(1_000),
            cache_write: None,
            output: Some(70),
            reasoning: Some(20),
            total: Some(1_470),
        }
    );
    // Together what the thread's cumulative total ended at.
    assert_eq!(sum(&counts, |c| c.input), 3_900);
    assert_eq!(sum(&counts, |c| c.cache_read), 2_400);
    assert_eq!(sum(&counts, |c| c.total), 4_100);
    // The context is the latest request (`last`), not the thread's total, with Codex's window.
    assert_eq!(
        contexts(&events),
        [
            (1_050, Some(272_000)),
            (1_470, Some(272_000)),
            (1_470, Some(272_000)),
            (1_580, Some(272_000)),
        ]
    );
    assert_eq!(
        tools(&events),
        [
            ("Bash".to_owned(), false, Some(1_500)),
            ("apply_patch".to_owned(), false, None),
            ("Bash".to_owned(), true, Some(3_000)),
            ("mcp__docs__search".to_owned(), false, Some(250)),
        ]
    );
    assert_eq!(
        files(&events),
        [
            ("/work/app/src/lib.rs".to_owned(), FileKind::Update),
            ("/work/app/src/retry.rs".to_owned(), FileKind::Add),
        ]
    );
    assert_eq!(
        edges(&events),
        [
            "start 1000",
            "end Completed 13000",
            "start 60000",
            "end Cancelled 4000"
        ]
    );
    assert!(events.iter().any(|event| matches!(
        event,
        UsageEvent::Identity { model: Some(model), .. } if model == "gpt-5-codex"
    )));
    assert!(!state.turn_open());
}

#[test]
fn a_codex_without_turn_events_has_the_user_message_begin_a_turn() {
    let (events, _) = read(
        Source::Codex,
        r#"{"timestamp":"2026-11-03T10:00:00.000Z","type":"event_msg","payload":{"type":"user_message","message":"a"}}
{"timestamp":"2026-11-03T10:00:05.000Z","type":"event_msg","payload":{"type":"web_search_end","call_id":"w"}}
{"timestamp":"2026-11-03T10:00:09.000Z","type":"event_msg","payload":{"type":"user_message","message":"b"}}
"#,
    );
    // The turn before ended at its last activity.
    assert_eq!(
        edges(&events),
        ["start 0", "end Completed 5000", "start 9000"]
    );
}

#[test]
fn a_claude_read_that_begins_inside_a_turn_opens_one_for_the_call() {
    let (events, _) = read(
        Source::Claude,
        r#"{"type":"assistant","timestamp":"2026-11-03T10:00:02.000Z","message":{"id":"m","model":"claude-opus-5-5","content":[],"stop_reason":"end_turn","usage":{"input_tokens":1,"cache_read_input_tokens":100,"output_tokens":5}}}
"#,
    );
    assert_eq!(edges(&events), ["start 2000", "end Completed 0"]);
    assert_eq!(tokens(&events).len(), 1);
}

#[test]
fn zero_costs_unknown_lines_and_the_harness_s_own_notices_say_nothing() {
    let (events, _) = read(
        Source::Claude,
        r#"{"type":"cost-state","totalCostUSD":0,"totalAPIDuration":0}
{"type":"summary","summary":"x"}
{"type":"user","message":{"role":"user","content":"<system-reminder>be brief</system-reminder>"}}
{"type":"assistant","message":{"id":"e","model":"<synthetic>","content":[{"type":"text","text":"API Error"}],"usage":{"input_tokens":0,"output_tokens":0}}}
"#,
    );
    assert_eq!(events, Vec::<UsageEvent>::new());
}

#[test]
fn malformed_lines_are_refused_and_other_sources_read_nothing() {
    for source in [Source::Claude, Source::Codex] {
        let adapter = adapter_for(source).unwrap();
        let mut state = UsageState::default();
        let mut events = Vec::new();
        assert!(!adapter.usage_line("{nope", &mut state, &mut events));
        assert!(!adapter.usage_line("[1,2]", &mut state, &mut events));
        assert!(events.is_empty());
    }
    let (events, _) = read(Source::Omp, r#"{"type":"message"}"#);
    assert!(events.is_empty(), "OMP reports through its extension");
}

#[test]
fn counts_that_went_down_are_taken_as_they_are() {
    let before = Counts {
        input: Some(10),
        output: Some(5),
        ..Counts::default()
    };
    let now = Counts {
        input: Some(4),
        output: Some(9),
        cache_read: Some(3),
        ..Counts::default()
    };
    let grown = now.grown_from(&before);
    assert_eq!(
        (grown.input, grown.output, grown.cache_read),
        (Some(4), Some(4), Some(3))
    );
    assert!(!Counts::default().any());
}
