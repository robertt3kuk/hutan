use super::*;
use hutan_core::SessionId;
use hutan_core::insight::{
    InsightFile, InsightFiles, InsightSource, InsightTime, InsightTools, InsightTotals, ToolCount,
};

fn context(used: u64, window: Option<u64>) -> InsightContext {
    InsightContext {
        used,
        window,
        exactness: Exactness::Reported,
        as_of_ms: 1,
        breakdown: Vec::new(),
        breakdown_as_of_ms: None,
        breakdown_exactness: None,
        compact_at: None,
    }
}

fn row(id: &str, tokens: u64, kind: BreakdownKind) -> BreakdownRow {
    BreakdownRow {
        id: id.to_owned(),
        label: id.replace('_', " "),
        tokens,
        kind,
    }
}

fn turn(n: u64, context_after: Option<u64>) -> InsightTurn {
    InsightTurn {
        n,
        turn_id: Some(TurnId::new()),
        started_ms: 1_000 * n,
        duration_ms: Some(38_000),
        outcome: InsightOutcome::Completed,
        input: None,
        output: None,
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

fn insight() -> AgentInsight {
    AgentInsight::empty(SessionId::new(), 1, 1)
}

#[test]
fn the_meter_says_used_of_window_with_a_percent_and_marks_an_estimate() {
    let mut c = context(82_000, Some(200_000));
    assert_eq!(meter_text(&c), "82k / 200k · 41 %");
    c.exactness = Exactness::Estimated;
    assert_eq!(meter_text(&c), "≈ 82k / 200k · 41 %");
    assert_eq!(
        meter_text(&context(300, Some(200_000))),
        "300 / 200k · <1 %"
    );
    // No window: a share of nothing is not known.
    assert_eq!(meter_text(&context(11_200, None)), "11k tokens");
    assert_eq!(fill(&context(11_200, None)), None);
    assert_eq!(fill(&context(100_000, Some(200_000))), Some(0.5));
    // Over the window the bar stops at full.
    assert_eq!(fill(&context(300_000, Some(200_000))), Some(1.0));
}

#[test]
fn the_compact_marker_is_a_share_of_the_window_and_absent_outside_it() {
    let mut c = context(1, Some(200_000));
    assert_eq!(compact_marker(&c), None);
    c.compact_at = Some(170_000);
    assert_eq!(compact_marker(&c), Some(0.85));
    c.compact_at = Some(200_000);
    assert_eq!(compact_marker(&c), None);
}

#[test]
fn the_breakdown_is_shares_of_the_window_and_skips_empty_rows() {
    let mut c = context(11_200, Some(200_000));
    assert_eq!(breakdown(&c), None, "nothing measured, nothing drawn");
    c.breakdown = vec![
        row("system_prompt", 2_559, BreakdownKind::Used),
        row("tools", 3_976, BreakdownKind::Used),
        row("skills", 0, BreakdownKind::Used),
        row("messages", 4_552, BreakdownKind::Used),
        row("autocompact_buffer", 30_000, BreakdownKind::Buffer),
        row("free", 158_800, BreakdownKind::Free),
        row("something_new", 5, BreakdownKind::Used),
    ];
    c.breakdown_exactness = Some(Exactness::Estimated);
    let b = breakdown(&c).expect("a breakdown");
    assert!(b.estimated);
    let words: Vec<_> = b
        .lines
        .iter()
        .map(|l| (l.label.as_str(), l.tokens_text.as_str(), l.swatch))
        .collect();
    assert_eq!(
        words,
        [
            ("system prompt", "2.6k", Swatch::System),
            ("tools", "4k", Swatch::Tools),
            ("messages", "4.6k", Swatch::Messages),
            ("autocompact buffer", "30k", Swatch::Buffer),
            ("free", "159k", Swatch::Free),
            ("something new", "5", Swatch::Other),
        ]
    );
    assert_eq!(b.lines[0].share_text, "1 %");
    assert!((b.lines[3].share - 0.15).abs() < 1e-6);
    // Without a window the rows are shares of their own sum.
    c.window = None;
    let b = breakdown(&c).expect("a breakdown");
    let sum: f32 = b.lines.iter().map(|l| l.share).sum();
    assert!((sum - 1.0).abs() < 1e-3, "{sum}");
}

#[test]
fn turn_rows_are_newest_first_and_say_what_each_turn_added() {
    let mut i = insight();
    let mut t1 = turn(1, Some(12_000));
    t1.input = Some(8_200);
    t1.output = Some(52);
    let mut t2 = turn(2, Some(24_400));
    t2.tool_calls = 3;
    t2.tool_failed = 1;
    t2.files = 2;
    t2.cost_usd = Some(0.21);
    let t3 = turn(3, Some(20_000)); // a compaction: the context shrank
    i.turns = vec![t1, t2, t3];
    let ids: Vec<_> = i.turns.iter().map(|t| t.turn_id.unwrap()).collect();
    let lines = turn_lines(&i, &|id| (id == ids[1]).then(|| "Rotate tokens".to_owned()));
    let labels: Vec<_> = lines.iter().map(|l| l.label.as_str()).collect();
    assert_eq!(labels, ["T3", "T2", "T1"]);
    // The first turn has no earlier context to grow from: its own counts.
    assert_eq!(lines[2].tokens.as_deref(), Some("8.2k in · 52 out"));
    assert_eq!(lines[1].tokens.as_deref(), Some("+12k context"));
    assert_eq!(lines[1].prompt.as_deref(), Some("Rotate tokens"));
    assert_eq!(lines[1].tools.as_deref(), Some("3 tool calls, 1 failed"));
    assert_eq!(lines[1].files.as_deref(), Some("2 files changed"));
    assert_eq!(lines[1].cost.as_deref(), Some("$0.21"));
    assert_eq!(lines[1].duration.as_deref(), Some("38.0 s"));
    // Nothing was reported for the compacted turn's tokens: absent, not zero.
    assert_eq!(lines[0].tokens, None);
    assert_eq!(lines[0].tools, None);
    assert_eq!(lines[0].files, None);
    assert_eq!(lines[0].cost, None);
}

#[test]
fn the_fill_chart_is_the_context_after_each_turn_and_needs_two_points() {
    let mut i = insight();
    i.turns = vec![turn(1, Some(10_000))];
    assert_eq!(fill_chart(&i, 40), None);
    i.turns = vec![turn(1, Some(10_000)), turn(2, None), turn(3, Some(40_000))];
    let chart = fill_chart(&i, 40).expect("a chart");
    assert_eq!(chart.peak, 40_000);
    assert_eq!(chart.bars, [0.25, 1.0]);
    i.context = Some(context(40_000, Some(200_000)));
    let chart = fill_chart(&i, 40).expect("a chart");
    assert_eq!(
        chart.bars,
        [0.25, 1.0],
        "the shape of the conversation, not the window"
    );
    let chart = fill_chart(&i, 1).expect("a chart");
    assert_eq!(chart.bars.len(), 1);
}

#[test]
fn a_field_a_harness_never_reports_is_named_not_drawn_as_zero() {
    assert_eq!(unsupported_note("codex", &[]), None);
    assert_eq!(
        unsupported_note("codex", &[InsightField::Breakdown, InsightField::Cost]).as_deref(),
        Some("Codex does not report the context by category or cost.")
    );
    assert_eq!(
        unsupported_note("omp", &[InsightField::Reasoning]).as_deref(),
        Some("OMP does not report reasoning tokens.")
    );
    let mut i = insight();
    i.harness = Some("codex".to_owned());
    i.unsupported = vec![InsightField::Cost];
    i.totals = Some(InsightTotals {
        turns: 1,
        input: Some(43_732),
        output: Some(55),
        cache_read: Some(33_920),
        cache_write: Some(0),
        reasoning: None,
        total: Some(43_787),
        cost_usd: None,
        cost_kind: None,
        cache_hit_ratio: None,
    });
    let pairs = totals_pairs(&i);
    assert_eq!(
        pairs,
        [
            ("Input".to_owned(), "44k".to_owned()),
            ("Output".to_owned(), "55".to_owned()),
            ("Cache read".to_owned(), "34k".to_owned()),
            ("Cache write".to_owned(), "0".to_owned()),
            ("Cost".to_owned(), "not reported by Codex".to_owned()),
        ]
    );
}

#[test]
fn a_cost_is_the_harness_s_own_figure_and_labelled() {
    let mut i = insight();
    i.totals = Some(InsightTotals {
        turns: 2,
        input: None,
        output: None,
        cache_read: None,
        cache_write: None,
        reasoning: None,
        total: None,
        cost_usd: Some(1.42),
        cost_kind: Some(CostKind::Estimate),
        cache_hit_ratio: Some(0.71),
    });
    assert_eq!(
        totals_pairs(&i),
        [
            ("Cache hit".to_owned(), "71 %".to_owned()),
            ("Cost".to_owned(), "≈ $1.42 est.".to_owned()),
        ]
    );
    assert_eq!(money(0.004), "<$0.01");
    assert_eq!(money(250.0), "$250");
}

#[test]
fn time_tools_files_and_sources_read_as_written() {
    let mut i = insight();
    i.time = Some(InsightTime {
        started_ms: 1,
        wall_ms: 14 * 60_000,
        active_ms: Some(6 * 60_000),
        api_ms: None,
        tool_ms: Some(12_000),
    });
    assert_eq!(
        time_pairs(&i),
        [
            ("Wall".to_owned(), "14 min".to_owned()),
            ("Working".to_owned(), "6 min".to_owned()),
            ("Tools".to_owned(), "12 s".to_owned()),
        ]
    );
    assert_eq!(duration_long(3_900_000), "1 h 5 min");
    i.tools = Some(InsightTools {
        calls: 31,
        failed: 2,
        by_name: vec![
            ToolCount {
                name: "read".to_owned(),
                calls: 14,
                failed: 0,
                ms: None,
            },
            ToolCount {
                name: "bash".to_owned(),
                calls: 7,
                failed: 2,
                ms: Some(1_200),
            },
        ],
    });
    assert_eq!(tools_summary(&i).as_deref(), Some("31 calls · 2 failed"));
    let tools = tool_lines(&i);
    assert_eq!(tools[1].detail.as_deref(), Some("2 failed · 1.2 s"));
    assert_eq!(tools[0].detail, None);
    i.files = Some(InsightFiles {
        count: 2,
        added: 120,
        removed: 34,
        items: vec![
            InsightFile {
                path: "src/auth/refresh.rs".to_owned(),
                kind: FileChangeKind::Update,
                added: Some(100),
                removed: Some(30),
            },
            InsightFile {
                path: "tests/new.rs".to_owned(),
                kind: FileChangeKind::Add,
                added: None,
                removed: None,
            },
        ],
        truncated: false,
    });
    assert_eq!(files_summary(&i).as_deref(), Some("2 touched · +120 −34"));
    let files = file_lines(&i);
    assert_eq!(files[0].kind, "edited");
    assert_eq!(files[0].stat.as_deref(), Some("+100 −30"));
    assert_eq!((files[1].kind, files[1].stat.clone()), ("created", None));
    i.sources = vec![
        InsightSource {
            block: InsightBlock::Context,
            source: InsightSourceKind::Chat,
            as_of_ms: 1,
        },
        InsightSource {
            block: InsightBlock::Breakdown,
            source: InsightSourceKind::Chat,
            as_of_ms: 1,
        },
        InsightSource {
            block: InsightBlock::Turns,
            source: InsightSourceKind::Chat,
            as_of_ms: 1,
        },
    ];
    assert_eq!(
        source_lines(&i),
        ["the chat — context, breakdown (from /context), turns"]
    );
}

#[test]
fn exact_figures_are_grouped() {
    assert_eq!(thousands(7), "7");
    assert_eq!(thousands(11_200), "11 200");
    assert_eq!(thousands(1_234_567), "1 234 567");
}
