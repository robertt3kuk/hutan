//! A terminal agent's reports folded by the reducer (`apply_report`).

use super::*;
use crate::insight::{
    InsightField, ReportedContext, ReportedFile, ReportedTool, ReportedTurn, TurnPhase,
};

const T0: u64 = 5_000_000;

fn reducer(harness: &str) -> InsightReducer {
    InsightReducer::new_terminal(SessionId::new(), harness, Some("/work/app"))
}

fn report(source: InsightSourceKind) -> InsightReport {
    InsightReport {
        source,
        ..InsightReport::default()
    }
}

fn start() -> Option<ReportedTurn> {
    Some(ReportedTurn {
        phase: TurnPhase::Start,
        outcome: None,
        duration_ms: None,
    })
}

fn end(duration_ms: Option<u64>) -> Option<ReportedTurn> {
    Some(ReportedTurn {
        phase: TurnPhase::End,
        outcome: None,
        duration_ms,
    })
}

fn call(input: u64, output: u64, cache_read: u64) -> TokenCounts {
    TokenCounts {
        input_tokens: Some(input),
        output_tokens: Some(output),
        cached_input_tokens: Some(cache_read),
        total_tokens: Some(input + output + cache_read),
        ..TokenCounts::default()
    }
}

fn tool(name: &str, calls: u32, failed: u32, ms: Option<u64>) -> ReportedTool {
    ReportedTool {
        name: name.to_owned(),
        calls,
        failed,
        ms,
    }
}

fn source_of(insight: &AgentInsight, block: InsightBlock) -> Option<InsightSourceKind> {
    insight
        .sources
        .iter()
        .find(|source| source.block == block)
        .map(|source| source.source)
}

#[test]
fn an_omp_turn_from_the_extension_is_a_turn_with_tokens_tools_files_and_cost() {
    let mut reducer = reducer("omp");
    // session_start: the context before any message, and the cost so far (zero).
    reducer.apply_report(
        &InsightReport {
            context: Some(ReportedContext {
                used: 10_678,
                window: Some(200_000),
                fixed: Some(10_678),
                estimated: false,
            }),
            cost_usd: Some(0.0),
            ..report(InsightSourceKind::Extension)
        },
        T0,
    );
    // agent_start, then two model calls and their tools.
    reducer.apply_report(
        &InsightReport {
            turn: start(),
            ..report(InsightSourceKind::Extension)
        },
        T0 + 100,
    );
    reducer.apply_report(
        &InsightReport {
            tokens: Some(call(8_200, 30, 0)),
            tools: vec![tool("read", 2, 0, Some(40))],
            ..report(InsightSourceKind::Extension)
        },
        T0 + 600,
    );
    reducer.apply_report(
        &InsightReport {
            turn: end(Some(2_500)),
            tokens: Some(call(40, 22, 14_000)),
            context: Some(ReportedContext {
                used: 11_200,
                window: Some(200_000),
                fixed: None,
                estimated: false,
            }),
            cost_usd: Some(0.0296),
            tools: vec![tool("edit", 1, 1, Some(5)), tool("read", 1, 0, Some(10))],
            files: vec![
                ReportedFile {
                    path: "/work/app/src/lib.rs".to_owned(),
                    kind: crate::agent::FileChangeKind::Update,
                },
                ReportedFile {
                    path: "/elsewhere/notes.md".to_owned(),
                    kind: crate::agent::FileChangeKind::Add,
                },
            ],
            ..report(InsightSourceKind::Extension)
        },
        T0 + 2_700,
    );
    let insight = reducer.snapshot(1, T0 + 3_000, None);

    let context = insight.context.as_ref().expect("a context");
    assert_eq!((context.used, context.window), (11_200, Some(200_000)));
    assert_eq!(context.exactness, Exactness::Reported);
    // The fixed part stays what the start said; the messages are the rest.
    let rows: Vec<(&str, u64)> = context
        .breakdown
        .iter()
        .map(|row| (row.id.as_str(), row.tokens))
        .collect();
    assert_eq!(rows, vec![("fixed", 10_678), ("messages", 522)]);
    assert_eq!(context.breakdown_exactness, Some(Exactness::Estimated));

    assert_eq!(insight.turns.len(), 1);
    let turn = &insight.turns[0];
    assert_eq!(turn.outcome, InsightOutcome::Completed);
    assert_eq!(turn.input, Some(8_240));
    assert_eq!(turn.output, Some(52));
    assert_eq!(turn.cache_read, Some(14_000));
    assert_eq!(turn.duration_ms, Some(2_500));
    assert_eq!((turn.tool_calls, turn.tool_failed, turn.files), (4, 1, 2));
    assert_eq!(turn.context_after, Some(11_200));
    // The cost of the turn is the growth of the cumulative cost over it.
    assert_eq!(turn.cost_usd, Some(0.0296));

    let totals = insight.totals.as_ref().expect("totals");
    assert_eq!((totals.turns, totals.cost_usd), (1, Some(0.0296)));
    let tools = insight.tools.as_ref().expect("tools");
    assert_eq!((tools.calls, tools.failed), (4, 1));
    let read = tools
        .by_name
        .iter()
        .find(|t| t.name == "read")
        .expect("read");
    assert_eq!((read.calls, read.ms), (3, Some(50)));
    let files = insight.files.as_ref().expect("files");
    let paths: Vec<&str> = files.items.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["src/lib.rs", "/elsewhere/notes.md"]);

    for block in [
        InsightBlock::Context,
        InsightBlock::Breakdown,
        InsightBlock::Turns,
        InsightBlock::Tools,
        InsightBlock::Files,
        InsightBlock::Time,
    ] {
        assert_eq!(
            source_of(&insight, block),
            Some(InsightSourceKind::Extension),
            "{block:?}"
        );
    }
    assert!(
        !insight.can_measure,
        "a terminal's agent is not asked for /context"
    );
    assert!(!insight.unsupported.contains(&InsightField::Breakdown));
}

#[test]
fn claude_hooks_and_the_status_line_each_fill_their_blocks() {
    let mut reducer = reducer("claude");
    reducer.apply_report(
        &InsightReport {
            turn: start(),
            ..report(InsightSourceKind::Hooks)
        },
        T0,
    );
    reducer.apply_report(
        &InsightReport {
            tools: vec![tool("Bash", 1, 0, Some(1_200))],
            ..report(InsightSourceKind::Hooks)
        },
        T0 + 1_000,
    );
    reducer.apply_report(
        &InsightReport {
            context: Some(ReportedContext {
                used: 52_000,
                window: Some(200_000),
                fixed: None,
                estimated: false,
            }),
            cost_usd: Some(0.41),
            api_ms: Some(9_000),
            ..report(InsightSourceKind::Statusline)
        },
        T0 + 1_500,
    );
    // A worse source never overwrites a better one for the block it reported.
    reducer.apply_report(
        &InsightReport {
            context: Some(ReportedContext {
                used: 1,
                window: None,
                fixed: None,
                estimated: true,
            }),
            ..report(InsightSourceKind::Hooks)
        },
        T0 + 1_600,
    );
    reducer.apply_report(
        &InsightReport {
            turn: end(None),
            ..report(InsightSourceKind::Hooks)
        },
        T0 + 2_000,
    );
    let insight = reducer.snapshot(1, T0 + 3_000, None);

    assert_eq!(insight.context.as_ref().map(|c| c.used), Some(52_000));
    assert_eq!(
        source_of(&insight, InsightBlock::Context),
        Some(InsightSourceKind::Statusline)
    );
    assert_eq!(
        source_of(&insight, InsightBlock::Tools),
        Some(InsightSourceKind::Hooks)
    );
    assert_eq!(
        source_of(&insight, InsightBlock::Turns),
        Some(InsightSourceKind::Hooks)
    );
    assert_eq!(
        source_of(&insight, InsightBlock::Time),
        Some(InsightSourceKind::Statusline)
    );
    assert_eq!(insight.time.and_then(|time| time.api_ms), Some(9_000));
    assert_eq!(insight.turns[0].duration_ms, Some(2_000));
    // No source gave this agent's tokens: absent, never zero.
    assert_eq!(insight.turns[0].input, None);
    assert_eq!(
        insight.totals.and_then(|totals| totals.cost_usd),
        Some(0.41)
    );
    // Claude Code gives no breakdown to a terminal.
    assert!(insight.unsupported.contains(&InsightField::Breakdown));
    assert!(insight.context.is_some_and(|c| c.breakdown.is_empty()));
}

#[test]
fn tools_and_files_outside_a_turn_count_for_the_session_and_a_restart_cancels_the_turn_before() {
    let mut reducer = reducer("codex");
    reducer.apply_report(
        &InsightReport {
            tools: vec![tool("Bash", 1, 0, None)],
            ..report(InsightSourceKind::Hooks)
        },
        T0,
    );
    assert!(reducer.snapshot(1, T0, None).turns.is_empty());

    reducer.apply_report(
        &InsightReport {
            turn: start(),
            ..report(InsightSourceKind::Hooks)
        },
        T0 + 10,
    );
    reducer.apply_report(
        &InsightReport {
            turn: start(),
            ..report(InsightSourceKind::Hooks)
        },
        T0 + 20,
    );
    let insight = reducer.snapshot(2, T0 + 30, None);
    let outcomes: Vec<InsightOutcome> = insight.turns.iter().map(|turn| turn.outcome).collect();
    assert_eq!(
        outcomes,
        vec![InsightOutcome::Cancelled, InsightOutcome::Running]
    );
    assert_eq!(insight.tools.map(|tools| tools.calls), Some(1));
}

#[test]
fn tokens_of_a_call_without_a_turn_begin_one_and_an_empty_report_changes_nothing() {
    let mut reducer = reducer("omp");
    assert!(!reducer.apply_report(&report(InsightSourceKind::Extension), T0));
    assert!(reducer.snapshot(1, T0, None).sources.is_empty());
    assert!(reducer.apply_report(
        &InsightReport {
            tokens: Some(call(100, 10, 0)),
            ..report(InsightSourceKind::Extension)
        },
        T0 + 5,
    ));
    let insight = reducer.snapshot(2, T0 + 10, None);
    assert_eq!(insight.turns.len(), 1);
    assert_eq!(insight.turns[0].outcome, InsightOutcome::Running);
}

fn one_call(source: InsightSourceKind, input: u64, tool_name: &str) -> InsightReport {
    InsightReport {
        tokens: Some(call(input, 10, 0)),
        tools: vec![tool(tool_name, 1, 0, None)],
        ..report(source)
    }
}

#[test]
fn opentelemetry_ranks_between_hooks_and_the_transcript() {
    assert!(InsightSourceKind::Hooks.rank() < InsightSourceKind::Otlp.rank());
    assert!(InsightSourceKind::Otlp.rank() < InsightSourceKind::Transcript.rank());
    assert_eq!(
        serde_json::to_string(&InsightSourceKind::Otlp).unwrap(),
        "\"otlp\""
    );
}

#[test]
fn a_worse_source_never_adds_to_the_counters_of_a_better_one() {
    let mut reducer = reducer("claude");
    reducer.apply_report(&one_call(InsightSourceKind::Hooks, 100, "Read"), T0);
    // OpenTelemetry says the same call again: it ranks below hooks, so it adds nothing.
    assert!(!reducer.apply_report(&one_call(InsightSourceKind::Otlp, 100, "Read"), T0 + 1));
    let insight = reducer.snapshot(1, T0 + 2, None);
    assert_eq!(insight.tools.as_ref().map(|tools| tools.calls), Some(1));
    assert_eq!(
        source_of(&insight, InsightBlock::Tools),
        Some(InsightSourceKind::Hooks)
    );
}

#[test]
fn a_better_source_that_takes_over_counters_starts_them_again_instead_of_adding() {
    let mut reducer = reducer("claude");
    // OpenTelemetry arrives first and counts a call and a tool.
    reducer.apply_report(&one_call(InsightSourceKind::Otlp, 100, "Read"), T0);
    reducer.apply_report(
        &InsightReport {
            turn: end(None),
            ..report(InsightSourceKind::Otlp)
        },
        T0 + 1,
    );
    // Hooks (a better source) then report the same call: they replace, never add.
    reducer.apply_report(&one_call(InsightSourceKind::Hooks, 100, "Read"), T0 + 2);
    reducer.apply_report(
        &InsightReport {
            turn: end(None),
            ..report(InsightSourceKind::Hooks)
        },
        T0 + 3,
    );
    let insight = reducer.snapshot(1, T0 + 4, None);
    assert_eq!(insight.tools.as_ref().map(|tools| tools.calls), Some(1));
    assert_eq!(insight.totals.as_ref().map(|totals| totals.turns), Some(1));
    assert_eq!(
        insight.totals.as_ref().and_then(|totals| totals.input),
        Some(100)
    );
    assert_eq!(insight.turns.len(), 1);
    assert_eq!(
        source_of(&insight, InsightBlock::Turns),
        Some(InsightSourceKind::Hooks)
    );
}

#[test]
fn a_worse_sources_cost_does_not_replace_a_better_ones() {
    let mut reducer = reducer("claude");
    reducer.apply_report(
        &InsightReport {
            cost_usd: Some(12.5),
            ..report(InsightSourceKind::Statusline)
        },
        T0,
    );
    reducer.apply_report(
        &InsightReport {
            cost_usd: Some(0.03),
            ..report(InsightSourceKind::Otlp)
        },
        T0 + 1,
    );
    let insight = reducer.snapshot(1, T0 + 2, None);
    assert_eq!(
        insight.totals.and_then(|totals| totals.cost_usd),
        Some(12.5)
    );
}
