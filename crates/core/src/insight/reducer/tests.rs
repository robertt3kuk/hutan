use super::*;
use crate::agent::{Delivery, ProviderIds, ToolKind};
use crate::insight::InsightField;

/// Builds the events of one session in order.
struct Script {
    session: SessionId,
    seq: u64,
    ts: u64,
}

impl Script {
    fn new() -> Self {
        Self {
            session: SessionId::new(),
            seq: 0,
            ts: 1_000_000,
        }
    }

    fn at(
        &mut self,
        step_ms: u64,
        turn: Option<TurnId>,
        item: Option<ItemId>,
        kind: AgentEventKind,
    ) -> AgentEvent {
        self.seq += 1;
        self.ts += step_ms;
        AgentEvent {
            session_id: self.session,
            seq: self.seq,
            ts_ms: self.ts,
            delivery: Delivery::Live,
            turn_id: turn,
            item_id: item,
            provider: ProviderIds::default(),
            kind,
            payload: None,
        }
    }

    fn event(&mut self, turn: TurnId, kind: AgentEventKind) -> AgentEvent {
        self.at(10, Some(turn), None, kind)
    }

    fn reducer(&self, harness: &str) -> InsightReducer {
        InsightReducer::new(self.session, harness, Some("/work/app"), true)
    }
}

fn counts(input: u64, output: u64, cached: u64, total: u64) -> TokenCounts {
    TokenCounts {
        input_tokens: Some(input),
        output_tokens: Some(output),
        cached_input_tokens: Some(cached),
        total_tokens: Some(total),
        ..TokenCounts::default()
    }
}

fn only_total(total: u64) -> TokenCounts {
    TokenCounts {
        total_tokens: Some(total),
        ..TokenCounts::default()
    }
}

fn usage(
    total: Option<TokenCounts>,
    last: Option<TokenCounts>,
    window: Option<u64>,
) -> AgentEventKind {
    AgentEventKind::Usage {
        total,
        last,
        context_window: window,
    }
}

fn tool_start(name: &str) -> AgentEventKind {
    AgentEventKind::ToolStart {
        tool: ToolKind::Command,
        name: name.to_owned(),
        title: String::new(),
        input: None,
    }
}

fn tool_end(status: ToolStatus, duration_ms: Option<u64>) -> AgentEventKind {
    AgentEventKind::ToolEnd {
        tool: ToolKind::Command,
        status,
        output: None,
        truncated: false,
        exit_code: None,
        duration_ms,
    }
}

fn message(role: MessageRole, text: &str) -> AgentEventKind {
    AgentEventKind::MessageEnd {
        role,
        phase: None,
        text: text.to_owned(),
    }
}

fn change(path: &str, kind: FileChangeKind, patch: Option<&str>) -> FileChange {
    FileChange {
        path: path.to_owned(),
        kind,
        move_path: None,
        patch: patch.map(str::to_owned),
        before: None,
        after: None,
    }
}

/// The recorded OMP 18.12.3 exchange (`docs/spikes/2026-10-28-insight/omp-acp-wire.jsonl`): a
/// turn of two model calls with a `read` tool, then a plain turn. `usage_update` says the
/// context is 11 200 of 200 000 after each, with the cumulative cost 0.02958 and 0.03636; the
/// prompt answers carry the turns' own counts.
#[test]
fn an_omp_chat_from_the_recorded_exchange_has_exact_numbers() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    let (t1, t2) = (TurnId::new(), TurnId::new());
    let read = ItemId::new();

    let e = s.event(t1, AgentEventKind::TurnStarted);
    r.apply(&e);
    let e = s.at(5, Some(t1), Some(read), tool_start("read"));
    r.apply(&e);
    let e = s.at(
        5,
        Some(t1),
        Some(read),
        tool_end(ToolStatus::Completed, Some(7)),
    );
    r.apply(&e);
    let e = s.event(t1, usage(Some(only_total(11_200)), None, Some(200_000)));
    r.apply(&e);
    r.cost_reported(0.029_58);
    let e = s.event(
        t1,
        usage(None, Some(counts(8_200, 52, 14_000, 22_252)), None),
    );
    r.apply(&e);
    let e = s.event(
        t1,
        AgentEventKind::TurnCompleted {
            duration_ms: Some(3_850),
        },
    );
    r.apply(&e);

    let e = s.event(t2, AgentEventKind::TurnStarted);
    r.apply(&e);
    let e = s.event(t2, usage(Some(only_total(11_200)), None, Some(200_000)));
    r.apply(&e);
    r.cost_reported(0.036_36);
    let e = s.event(
        t2,
        usage(None, Some(counts(1_200, 12, 10_000, 11_212)), None),
    );
    r.apply(&e);
    let e = s.event(
        t2,
        AgentEventKind::TurnCompleted {
            duration_ms: Some(1_000),
        },
    );
    r.apply(&e);

    let insight = r.snapshot(7, s.ts + 500, None);
    let context = insight.context.as_ref().expect("a context");
    assert_eq!(
        (context.used, context.window, context.exactness),
        (11_200, Some(200_000), Exactness::Reported)
    );
    let totals = insight.totals.as_ref().expect("totals");
    assert_eq!(
        (
            totals.turns,
            totals.input,
            totals.output,
            totals.cache_read,
            totals.total
        ),
        (2, Some(9_400), Some(64), Some(24_000), Some(33_464))
    );
    assert_eq!(
        (totals.cache_write, totals.reasoning),
        (None, None),
        "nothing reported is not zero"
    );
    assert_eq!(
        totals.cost_usd,
        Some(0.036_36),
        "the cumulative cost of the session"
    );
    assert_eq!(totals.cost_kind, Some(CostKind::Estimate));
    // The prompt is input + cache read: 24 000 / 33 400.
    let ratio = totals.cache_hit_ratio.expect("a ratio");
    assert!((ratio - 24_000.0 / 33_400.0).abs() < 1e-9, "{ratio}");

    assert_eq!(insight.turns.len(), 2);
    let first = &insight.turns[0];
    assert_eq!(
        (first.n, first.turn_id, first.outcome),
        (1, Some(t1), InsightOutcome::Completed)
    );
    assert_eq!(
        (
            first.input,
            first.output,
            first.cache_read,
            first.duration_ms,
            first.tool_calls,
            first.context_after
        ),
        (
            Some(8_200),
            Some(52),
            Some(14_000),
            Some(3_850),
            1,
            Some(11_200)
        )
    );
    assert_eq!(first.cost_usd, Some(0.029_58));
    let second = &insight.turns[1];
    assert_eq!(
        (second.n, second.input, second.tool_calls),
        (2, Some(1_200), 0)
    );
    assert!((second.cost_usd.expect("a cost") - 0.006_78).abs() < 1e-9);

    let tools = insight.tools.as_ref().expect("tools");
    assert_eq!((tools.calls, tools.failed), (1, 0));
    assert_eq!(
        tools.by_name,
        [ToolCount {
            name: "read".into(),
            calls: 1,
            failed: 0,
            ms: Some(7)
        }]
    );
    let time = insight.time.as_ref().expect("time");
    assert_eq!((time.active_ms, time.tool_ms), (Some(4_850), Some(7)));
    assert!(insight.unsupported.contains(&InsightField::Reasoning));
    assert!(insight.can_measure);
    let blocks: Vec<_> = insight.sources.iter().map(|s| s.block).collect();
    assert_eq!(
        blocks,
        [
            InsightBlock::Context,
            InsightBlock::Turns,
            InsightBlock::Tools,
            InsightBlock::Time
        ]
    );
    assert!(
        insight
            .sources
            .iter()
            .all(|s| s.source == InsightSourceKind::Chat)
    );
}

/// Codex 0.162.1 (`codex-0.162/approval-decline.jsonl`): the second request of the turn reports
/// the cumulative `total 41 290` and the request's own `last 20 672` on a 475 000 window.
#[test]
fn a_codex_context_is_the_latest_request_not_the_cumulative_total() {
    let mut s = Script::new();
    let mut r = s.reducer("codex");
    let t = TurnId::new();
    let e = s.event(t, AgentEventKind::TurnStarted);
    r.apply(&e);
    let first = TokenCounts {
        cache_write_tokens: Some(0),
        ..counts(20_000, 618, 0, 20_618)
    };
    let e = s.event(t, usage(Some(first), Some(first), Some(475_000)));
    r.apply(&e);
    let total = counts(40_500, 790, 18_000, 41_290);
    let last = counts(20_500, 172, 18_000, 20_672);
    let e = s.event(t, usage(Some(total), Some(last), Some(475_000)));
    r.apply(&e);
    let e = s.event(
        t,
        AgentEventKind::TurnCompleted {
            duration_ms: Some(3_850),
        },
    );
    r.apply(&e);

    let insight = r.snapshot(1, s.ts, None);
    let context = insight.context.expect("a context");
    assert_eq!((context.used, context.window), (20_672, Some(475_000)));
    let turn = &insight.turns[0];
    // The turn's tokens are the growth of the cumulative total.
    assert_eq!(
        (turn.input, turn.output, turn.cache_read),
        (Some(40_500), Some(790), Some(18_000))
    );
    assert_eq!(turn.cost_usd, None, "Codex reports no cost");
    assert!(insight.totals.expect("totals").cost_usd.is_none());
    assert_eq!(
        insight.unsupported,
        [InsightField::Breakdown, InsightField::Cost]
    );
    assert!(!insight.can_measure);
}

#[test]
fn a_codex_cache_ratio_reads_cached_over_input_and_cache_writes_are_summed() {
    let mut s = Script::new();
    let mut r = s.reducer("codex");
    let t = TurnId::new();
    let e = s.event(t, AgentEventKind::TurnStarted);
    r.apply(&e);
    let last = TokenCounts {
        cache_write_tokens: Some(300),
        ..counts(1_000, 10, 750, 1_010)
    };
    let e = s.event(t, usage(Some(last), Some(last), None));
    r.apply(&e);
    let e = s.event(t, AgentEventKind::TurnCompleted { duration_ms: None });
    r.apply(&e);
    let totals = r.snapshot(1, s.ts, None).totals.expect("totals");
    assert_eq!(totals.cache_write, Some(300));
    assert_eq!(
        totals.cache_hit_ratio,
        Some(0.75),
        "OpenAI's input already holds the cached tokens"
    );
}

#[test]
fn a_last_without_a_total_is_the_turns_spend_and_not_the_context() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    let t = TurnId::new();
    let e = s.event(t, AgentEventKind::TurnStarted);
    r.apply(&e);
    let e = s.event(t, usage(None, Some(counts(10, 1, 0, 11)), Some(200_000)));
    r.apply(&e);
    let insight = r.snapshot(1, s.ts, None);
    assert!(insight.context.is_none(), "no context was reported");
    assert_eq!(insight.turns[0].outcome, InsightOutcome::Running);
}

#[test]
fn a_context_turn_is_not_a_turn_and_its_answer_is_the_breakdown() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    let t = TurnId::new();
    let e = s.event(t, AgentEventKind::TurnStarted);
    r.apply(&e);
    let e = s.event(t, usage(Some(only_total(11_200)), None, Some(200_000)));
    r.apply(&e);
    let e = s.event(
        t,
        AgentEventKind::TurnCompleted {
            duration_ms: Some(10),
        },
    );
    r.apply(&e);

    let measure = TurnId::new();
    let e = s.event(measure, AgentEventKind::TurnStarted);
    r.apply(&e);
    let e = s.event(measure, message(MessageRole::User, "/context"));
    r.apply(&e);
    let answer = "Context window: 200000 tokens (6% used)\n  System prompt    [░░░░] 1%  2559 tokens\n  System tools     [░░░░] 2%  3976 tokens\n  System context   [░░░░] 0%  113 tokens\n  Messages         [█░░░] 2%  4552 tokens\n  Auto-compact buf [████] 15%  30000 tokens\n  Free             [████] 79%  158800 tokens";
    let e = s.event(measure, message(MessageRole::Assistant, answer));
    r.apply(&e);
    let e = s.event(
        measure,
        AgentEventKind::TurnCompleted {
            duration_ms: Some(5),
        },
    );
    r.apply(&e);

    let insight = r.snapshot(2, s.ts, None);
    assert_eq!(
        insight.totals.expect("totals").turns,
        1,
        "/context is not counted"
    );
    assert_eq!(insight.turns.len(), 1);
    let context = insight.context.expect("a context");
    assert_eq!(context.used, 11_200);
    assert_eq!(context.breakdown.len(), 6);
    assert_eq!(context.breakdown_exactness, Some(Exactness::Reported));
    assert_eq!(context.breakdown_as_of_ms, Some(s.ts));
    assert_eq!(
        context.compact_at,
        Some(170_000),
        "the window less the buffer"
    );
    let used: u64 = context
        .breakdown
        .iter()
        .filter(|row| row.kind == BreakdownKind::Used)
        .map(|row| row.tokens)
        .sum();
    assert_eq!(
        used, context.used,
        "the categories add up to the harness's own figure"
    );
    assert!(
        insight
            .sources
            .iter()
            .any(|s| s.block == InsightBlock::Breakdown)
    );
}

#[test]
fn a_context_answer_nobody_can_read_leaves_no_breakdown() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    let t = TurnId::new();
    for kind in [
        AgentEventKind::TurnStarted,
        message(MessageRole::User, "/context"),
        message(MessageRole::Assistant, "no model is selected"),
        AgentEventKind::TurnCompleted { duration_ms: None },
    ] {
        let e = s.event(t, kind);
        r.apply(&e);
    }
    let insight = r.snapshot(1, s.ts, None);
    assert!(insight.context.is_none() && insight.turns.is_empty() && insight.totals.is_none());
}

#[test]
fn a_report_fills_the_context_when_no_request_gave_one() {
    let mut s = Script::new();
    let mut r = s.reducer("claude");
    let t = TurnId::new();
    let markdown = "## Context Usage\n\n**Tokens:** 17k / 1m (2%)\n\n| Category | Tokens | Percentage |\n|---|---|---|\n| System prompt | 1.9k | 0.2% |\n| System tools | 13.1k | 1.3% |\n| Skills | 2k | 0.2% |\n| Free space | 950k | 95.0% |";
    for kind in [
        AgentEventKind::TurnStarted,
        message(MessageRole::User, "/context"),
        message(MessageRole::Assistant, markdown),
        AgentEventKind::TurnCompleted { duration_ms: None },
    ] {
        let e = s.event(t, kind);
        r.apply(&e);
    }
    let context = r.snapshot(1, s.ts, None).context.expect("a context");
    assert_eq!(
        (
            context.used,
            context.window,
            context.exactness,
            context.breakdown_exactness
        ),
        (
            17_000,
            Some(1_000_000),
            Exactness::Estimated,
            Some(Exactness::Estimated)
        )
    );
}

#[test]
fn tools_are_counted_by_name_with_failures_and_the_tail_is_other() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    let t = TurnId::new();
    let e = s.event(t, AgentEventKind::TurnStarted);
    r.apply(&e);
    // 18 names, `tool0` used twice.
    for n in 0..18 {
        let item = ItemId::new();
        let e = s.at(1, Some(t), Some(item), tool_start(&format!("tool{n}")));
        r.apply(&e);
        let status = if n == 3 || n == 17 {
            ToolStatus::Failed
        } else {
            ToolStatus::Completed
        };
        let e = s.at(1, Some(t), Some(item), tool_end(status, None));
        r.apply(&e);
    }
    let item = ItemId::new();
    for kind in [tool_start("tool0"), tool_end(ToolStatus::Declined, None)] {
        let e = s.at(1, Some(t), Some(item), kind);
        r.apply(&e);
    }
    let tools = r.snapshot(1, s.ts, None).tools.expect("tools");
    assert_eq!((tools.calls, tools.failed), (19, 2));
    assert_eq!(tools.by_name.len(), MAX_TOOL_NAMES + 1);
    assert_eq!(tools.by_name[0].name, "tool0");
    assert_eq!(tools.by_name[0].calls, 2);
    let last = tools.by_name.last().expect("a row");
    assert_eq!(
        (last.name.as_str(), last.calls, last.failed),
        (OTHER_TOOLS, 2, 1)
    );
    assert!(
        tools
            .by_name
            .iter()
            .any(|row| row.name == "tool3" && row.failed == 1)
    );
}

#[test]
fn a_tool_that_ends_without_a_start_in_the_window_is_counted_as_other() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    let t = TurnId::new();
    let e = s.event(t, AgentEventKind::TurnStarted);
    r.apply(&e);
    let e = s.at(
        1,
        Some(t),
        Some(ItemId::new()),
        tool_end(ToolStatus::Failed, Some(9)),
    );
    r.apply(&e);
    let tools = r.snapshot(1, s.ts, None).tools.expect("tools");
    assert_eq!(
        tools.by_name,
        [ToolCount {
            name: OTHER_TOOLS.into(),
            calls: 1,
            failed: 1,
            ms: Some(9)
        }]
    );
}

#[test]
fn files_are_relative_to_the_folder_and_counted_from_their_patches() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    let t = TurnId::new();
    let e = s.event(t, AgentEventKind::TurnStarted);
    r.apply(&e);
    let patch = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n fn a() {}\n-fn b() {}\n+fn b() -> u8 { 1 }\n+fn c() {}\n";
    let item = ItemId::new();
    // Codex says a patch when the item starts and again when it ends: counted once.
    for _ in 0..2 {
        let e = s.at(
            1,
            Some(t),
            Some(item),
            AgentEventKind::FileDiff {
                changes: vec![change(
                    "/work/app/src/lib.rs",
                    FileChangeKind::Update,
                    Some(patch),
                )],
                patch: None,
            },
        );
        r.apply(&e);
    }
    let e = s.at(
        1,
        Some(t),
        Some(ItemId::new()),
        AgentEventKind::FileDiff {
            changes: vec![FileChange {
                after: Some("one\ntwo\n".into()),
                ..change("/elsewhere/notes.md", FileChangeKind::Add, None)
            }],
            patch: None,
        },
    );
    r.apply(&e);
    let e = s.event(t, AgentEventKind::TurnCompleted { duration_ms: None });
    r.apply(&e);

    let insight = r.snapshot(1, s.ts, None);
    let files = insight.files.expect("files");
    assert_eq!(
        (files.count, files.added, files.removed, files.truncated),
        (2, 4, 1, false)
    );
    assert_eq!(
        files.items,
        [
            InsightFile {
                path: "src/lib.rs".into(),
                kind: FileChangeKind::Update,
                added: Some(2),
                removed: Some(1)
            },
            InsightFile {
                path: "/elsewhere/notes.md".into(),
                kind: FileChangeKind::Add,
                added: Some(2),
                removed: Some(0)
            },
        ]
    );
    assert_eq!(insight.turns[0].files, 2);
}

#[test]
fn files_past_the_cap_are_counted_in_the_lines_and_say_truncated() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    let t = TurnId::new();
    let e = s.event(t, AgentEventKind::TurnStarted);
    r.apply(&e);
    for n in 0..(MAX_FILES + 5) {
        let e = s.at(
            1,
            Some(t),
            Some(ItemId::new()),
            AgentEventKind::FileDiff {
                changes: vec![change(
                    &format!("/work/app/f{n}.rs"),
                    FileChangeKind::Update,
                    Some("+x\n"),
                )],
                patch: None,
            },
        );
        r.apply(&e);
    }
    let files = r.snapshot(1, s.ts, None).files.expect("files");
    assert_eq!(
        (files.items.len(), files.count, files.truncated),
        (MAX_FILES, MAX_FILES as u64, true)
    );
    assert_eq!(
        files.added,
        (MAX_FILES + 5) as u64,
        "the lines of the unlisted files count"
    );
}

#[test]
fn the_turn_ring_keeps_the_newest_and_numbers_keep_counting() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    let total = MAX_TURNS + 30;
    for _ in 0..total {
        let t = TurnId::new();
        for kind in [
            AgentEventKind::TurnStarted,
            AgentEventKind::TurnCompleted {
                duration_ms: Some(1),
            },
        ] {
            let e = s.event(t, kind);
            r.apply(&e);
        }
    }
    let insight = r.snapshot(1, s.ts, None);
    assert_eq!(insight.turns.len(), MAX_TURNS);
    assert!(insight.turns_truncated);
    assert_eq!(insight.turns.first().map(|t| t.n), Some(31));
    assert_eq!(insight.turns.last().map(|t| t.n), Some(total as u64));
    let totals = insight.totals.expect("totals");
    assert_eq!((totals.turns, totals.input), (total as u64, None));
}

#[test]
fn a_turn_in_progress_is_listed_last_and_a_turn_that_never_ended_is_cancelled() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    let (a, b) = (TurnId::new(), TurnId::new());
    let e = s.event(a, AgentEventKind::TurnStarted);
    r.apply(&e);
    let e = s.event(b, AgentEventKind::TurnStarted);
    r.apply(&e);
    let turns = r.snapshot(1, s.ts, None).turns;
    let shape: Vec<_> = turns.iter().map(|t| (t.n, t.turn_id, t.outcome)).collect();
    assert_eq!(
        shape,
        [
            (1, Some(a), InsightOutcome::Cancelled),
            (2, Some(b), InsightOutcome::Running)
        ]
    );
}

#[test]
fn events_already_applied_are_ignored() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    let t = TurnId::new();
    let events = [
        s.event(t, AgentEventKind::TurnStarted),
        s.event(t, usage(None, Some(counts(10, 1, 0, 11)), None)),
        s.event(
            t,
            AgentEventKind::TurnCompleted {
                duration_ms: Some(5),
            },
        ),
    ];
    for e in &events {
        assert!(r.apply(e));
    }
    let once = r.snapshot(1, s.ts, None);
    for e in &events {
        assert!(!r.apply(e), "a replay changes nothing");
    }
    assert_eq!(r.snapshot(1, s.ts, None), once);
    assert_eq!(r.last_seq(), 3);
}

#[test]
fn usage_records_fill_a_turns_cost_and_the_turns_before_the_journal_window() {
    let mut s = Script::new();
    s.ts = 5_000_000;
    let mut r = s.reducer("omp");
    let t = TurnId::new();
    for kind in [
        AgentEventKind::TurnStarted,
        usage(None, Some(counts(100, 10, 0, 110)), None),
        AgentEventKind::TurnCompleted {
            duration_ms: Some(1),
        },
    ] {
        let e = s.event(t, kind);
        r.apply(&e);
    }
    let older = TurnId::new();
    r.seed(
        &[
            SeedTurn {
                turn_id: older.to_string(),
                at_ms: 1_000,
                tokens: counts(7, 3, 0, 10),
                cost_usd: Some(0.25),
            },
            SeedTurn {
                turn_id: t.to_string(),
                at_ms: 5_000_100,
                tokens: counts(100, 10, 0, 110),
                cost_usd: Some(0.5),
            },
        ],
        true,
    );
    let insight = r.snapshot(1, s.ts, None);
    let ids: Vec<_> = insight
        .turns
        .iter()
        .map(|t| (t.n, t.turn_id, t.cost_usd))
        .collect();
    assert_eq!(ids, [(1, Some(older), Some(0.25)), (2, Some(t), Some(0.5))]);
    let totals = insight.totals.expect("totals");
    assert_eq!(
        (totals.turns, totals.input, totals.cost_usd),
        (2, Some(107), Some(0.75))
    );

    // Without a cut journal the older record is not invented into the list.
    let mut whole = s.reducer("omp");
    for kind in [
        AgentEventKind::TurnStarted,
        AgentEventKind::TurnCompleted {
            duration_ms: Some(1),
        },
    ] {
        let e = s.event(t, kind);
        whole.apply(&e);
    }
    whole.seed(
        &[SeedTurn {
            turn_id: older.to_string(),
            at_ms: 1_000,
            tokens: counts(7, 3, 0, 10),
            cost_usd: None,
        }],
        false,
    );
    assert_eq!(whole.snapshot(1, s.ts, None).turns.len(), 1);
}

#[test]
fn a_snapshot_never_exceeds_the_wire_budget() {
    let mut s = Script::new();
    let mut r = s.reducer("omp");
    for n in 0..MAX_TURNS {
        let t = TurnId::new();
        for kind in [
            AgentEventKind::TurnStarted,
            usage(
                None,
                Some(counts(123_456, 7_890, 99_000, 230_000)),
                Some(1_000_000),
            ),
            AgentEventKind::TurnCompleted {
                duration_ms: Some(123_456),
            },
        ] {
            let e = s.event(t, kind);
            r.apply(&e);
        }
        r.cost_reported(n as f64 * 0.123_456_789);
    }
    let t = TurnId::new();
    let e = s.event(t, AgentEventKind::TurnStarted);
    r.apply(&e);
    for n in 0..MAX_FILES {
        let long = format!("/work/app/{}{n}.rs", "d/".repeat(120));
        let e = s.at(
            1,
            Some(t),
            Some(ItemId::new()),
            AgentEventKind::FileDiff {
                changes: vec![change(&long, FileChangeKind::Update, Some("+x\n"))],
                patch: None,
            },
        );
        r.apply(&e);
    }
    let insight = r.snapshot(1, s.ts, None);
    let size = serde_json::to_vec(&insight).expect("json").len();
    assert!(size <= MAX_SNAPSHOT_BYTES, "{size} bytes");
    assert!(insight.turns_truncated, "turns were cut to fit");
    assert!(insight.files.expect("files").items.len() <= MAX_FILES);
    // The cut keeps the newest turns.
    assert_eq!(
        insight.turns.last().map(|t| t.outcome),
        Some(InsightOutcome::Running)
    );
}
