//! What a read of a transcript says, as reports for the insight reducer (INSIGHT-C).
//!
//! [`fold`] turns the [`UsageEvent`]s the history adapters read into [`InsightReport`]s labelled
//! `transcript`, in order, each with the time of its newest event: the reducer is the one that
//! also folds the hooks' and the extension's reports, so a transcript gets the same turns, the
//! same tool rows and the same cost as every other source, and the merge by source
//! (`crate::insight_merge`) decides whose numbers a block shows.
//!
//! A turn is two reports: its start at the time of the prompt, and what it did with its end, so
//! that the turn's time is the transcript's, not the time Hutan read it. Pure.

use hutan_core::agent::{FileChangeKind, TokenCounts};
use hutan_core::insight::report::category_id;
use hutan_core::insight::{
    BreakdownKind, BreakdownRow, InsightOutcome, InsightReport, InsightSourceKind, MAX_BREAKDOWN,
    MAX_REPORT_FILES, MAX_REPORT_TOOLS, ReportedContext, ReportedFile, ReportedTool, ReportedTurn,
    TurnPhase,
};
use hutan_index::adapters::usage::{BreakdownLine, Counts, FileKind, Outcome, UsageEvent};

/// What folding events said besides reports.
#[derive(Debug, Default)]
pub struct Folded {
    /// The reports, oldest first, each with the time (Unix ms) its newest event happened.
    pub reports: Vec<(InsightReport, u64)>,
    /// The newest model the transcript names.
    pub model: Option<String>,
    /// The conversation's id as the transcript says it (a Codex rollout's `session_meta.id`,
    /// Claude Code's `sessionId`).
    pub session_key: Option<String>,
    /// The context by category of the newest `/context` record, and when it was written.
    pub breakdown: Option<(Vec<BreakdownRow>, u64)>,
}

fn tokens_of(counts: Counts) -> TokenCounts {
    TokenCounts {
        input_tokens: counts.input,
        cached_input_tokens: counts.cache_read,
        cache_write_tokens: counts.cache_write,
        output_tokens: counts.output,
        reasoning_output_tokens: counts.reasoning,
        total_tokens: counts.total,
    }
}

fn plus(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (a, b) => a.or(b),
    }
}

fn add_tokens(sum: &mut Option<TokenCounts>, more: Counts) {
    let more = tokens_of(more);
    *sum = Some(match sum.take() {
        None => more,
        Some(before) => TokenCounts {
            input_tokens: plus(before.input_tokens, more.input_tokens),
            cached_input_tokens: plus(before.cached_input_tokens, more.cached_input_tokens),
            cache_write_tokens: plus(before.cache_write_tokens, more.cache_write_tokens),
            output_tokens: plus(before.output_tokens, more.output_tokens),
            reasoning_output_tokens: plus(
                before.reasoning_output_tokens,
                more.reasoning_output_tokens,
            ),
            total_tokens: plus(before.total_tokens, more.total_tokens),
        },
    });
}

fn outcome_of(outcome: Outcome) -> InsightOutcome {
    match outcome {
        Outcome::Completed => InsightOutcome::Completed,
        Outcome::Cancelled => InsightOutcome::Cancelled,
        Outcome::Failed => InsightOutcome::Failed,
    }
}

fn kind_of(kind: FileKind) -> FileChangeKind {
    match kind {
        FileKind::Add => FileChangeKind::Add,
        FileKind::Update => FileChangeKind::Update,
        FileKind::Delete => FileChangeKind::Delete,
    }
}

/// The category rows of a `/context` record, in the insight's vocabulary.
fn breakdown_rows(lines: &[BreakdownLine]) -> Vec<BreakdownRow> {
    lines
        .iter()
        .take(MAX_BREAKDOWN)
        .map(|line| {
            let id = category_id(&line.name);
            let kind = match line.kind.as_str() {
                "buffer" => BreakdownKind::Buffer,
                "free" => BreakdownKind::Free,
                "deferred" => BreakdownKind::Deferred,
                "used" => BreakdownKind::Used,
                _ if id == "autocompact_buffer" => BreakdownKind::Buffer,
                _ if id == "free" => BreakdownKind::Free,
                _ if line.name.to_ascii_lowercase().contains("deferred") => BreakdownKind::Deferred,
                _ => BreakdownKind::Used,
            };
            BreakdownRow {
                id,
                label: line.name.clone(),
                tokens: line.tokens,
                kind,
            }
        })
        .collect()
}

/// The report being filled and the time of its newest event.
struct Cursor {
    report: InsightReport,
    at: Option<u64>,
}

impl Cursor {
    fn new() -> Self {
        Self {
            report: InsightReport {
                source: InsightSourceKind::Transcript,
                ..InsightReport::default()
            },
            at: None,
        }
    }
}

fn flush(cursor: &mut Cursor, last: u64, out: &mut Folded) {
    let done = std::mem::replace(cursor, Cursor::new());
    if !done.report.is_empty() {
        out.reports.push((done.report, done.at.unwrap_or(last)));
    }
}

/// Folds `events` into `out`. `fallback_ms` stands in for the time of an event whose line
/// carried none and that no earlier event gave one to.
pub fn fold(events: Vec<UsageEvent>, fallback_ms: u64, out: &mut Folded) {
    let mut cursor = Cursor::new();
    let mut last = fallback_ms;
    // A turn's start is a report of its own, at the time of the prompt.
    for event in events {
        let ts = match &event {
            UsageEvent::Identity { .. } => None,
            UsageEvent::TurnStart { ts_ms }
            | UsageEvent::TurnEnd { ts_ms, .. }
            | UsageEvent::Tokens { ts_ms, .. }
            | UsageEvent::Context { ts_ms, .. }
            | UsageEvent::Breakdown { ts_ms, .. }
            | UsageEvent::Cost { ts_ms, .. }
            | UsageEvent::ApiTime { ts_ms, .. }
            | UsageEvent::Tool { ts_ms, .. }
            | UsageEvent::File { ts_ms, .. } => *ts_ms,
        };
        let at = ts.unwrap_or(last);
        if !matches!(
            event,
            UsageEvent::Identity { .. } | UsageEvent::TurnStart { .. }
        ) {
            last = last.max(at);
            cursor.at = Some(cursor.at.map_or(at, |known| known.max(at)));
        }
        match event {
            UsageEvent::Identity {
                session_id, model, ..
            } => {
                if session_id.is_some() && out.session_key.is_none() {
                    out.session_key = session_id;
                }
                if model.is_some() {
                    out.model = model;
                }
            }
            UsageEvent::TurnStart { .. } => {
                // What came before (after the last turn's end) is reported at its own time.
                flush(&mut cursor, last, out);
                last = last.max(at);
                out.reports.push((
                    InsightReport {
                        source: InsightSourceKind::Transcript,
                        turn: Some(ReportedTurn {
                            phase: TurnPhase::Start,
                            outcome: None,
                            duration_ms: None,
                        }),
                        ..InsightReport::default()
                    },
                    at,
                ));
            }
            UsageEvent::TurnEnd {
                outcome,
                duration_ms,
                ..
            } => {
                cursor.report.turn = Some(ReportedTurn {
                    phase: TurnPhase::End,
                    outcome: Some(outcome_of(outcome)),
                    duration_ms,
                });
                flush(&mut cursor, last, out);
            }
            UsageEvent::Tokens { counts, .. } => add_tokens(&mut cursor.report.tokens, counts),
            UsageEvent::Context {
                used,
                window,
                estimated,
                ..
            } => {
                cursor.report.context = Some(ReportedContext {
                    used,
                    window,
                    fixed: None,
                    estimated,
                });
            }
            UsageEvent::Breakdown { rows, .. } => {
                let rows = breakdown_rows(&rows);
                if !rows.is_empty() {
                    out.breakdown = Some((rows, at));
                }
            }
            UsageEvent::Cost { usd, .. } => cursor.report.cost_usd = Some(usd),
            UsageEvent::ApiTime { ms, .. } => cursor.report.api_ms = Some(ms),
            UsageEvent::Tool {
                name, failed, ms, ..
            } => {
                let known = cursor
                    .report
                    .tools
                    .iter()
                    .position(|tool| tool.name == name);
                match known {
                    Some(at) => {
                        let tool = &mut cursor.report.tools[at];
                        tool.calls += 1;
                        tool.failed += u32::from(failed);
                        tool.ms = plus(tool.ms, ms);
                    }
                    None => {
                        if cursor.report.tools.len() >= MAX_REPORT_TOOLS {
                            let keep = cursor.at;
                            flush(&mut cursor, last, out);
                            cursor.at = keep;
                        }
                        cursor.report.tools.push(ReportedTool {
                            name,
                            calls: 1,
                            failed: u32::from(failed),
                            ms,
                        });
                    }
                }
            }
            UsageEvent::File { path, kind, .. } => {
                if cursor.report.files.len() >= MAX_REPORT_FILES {
                    let keep = cursor.at;
                    flush(&mut cursor, last, out);
                    cursor.at = keep;
                }
                cursor.report.files.push(ReportedFile {
                    path,
                    kind: kind_of(kind),
                });
            }
        }
    }
    flush(&mut cursor, last, out);
}

#[cfg(test)]
mod tests {
    use hutan_core::insight::InsightReport;
    use hutan_index::Source;
    use hutan_index::adapters::adapter_for;
    use hutan_index::adapters::usage::UsageState;

    use super::*;

    const CLAUDE: &str = include_str!("../../index/tests/fixtures/usage/claude-session.jsonl");
    const CODEX: &str = include_str!("../../index/tests/fixtures/usage/codex-rollout.jsonl");
    const T0: u64 = 1_793_700_000_000;

    fn fold_text(source: Source, text: &str) -> Folded {
        let adapter = adapter_for(source).unwrap();
        let mut state = UsageState::default();
        let mut events = Vec::new();
        for line in text.lines() {
            assert!(adapter.usage_line(line, &mut state, &mut events));
        }
        let mut folded = Folded::default();
        fold(events, 1, &mut folded);
        folded
    }

    fn turn(report: &InsightReport) -> Option<TurnPhase> {
        report.turn.map(|turn| turn.phase)
    }

    #[test]
    fn a_turn_is_its_start_at_the_prompt_and_its_content_with_its_end_at_the_last_event() {
        let folded = fold_text(Source::Claude, CLAUDE);
        let shape: Vec<(Option<TurnPhase>, u64)> = folded
            .reports
            .iter()
            .map(|(report, at)| (turn(report), at - T0))
            .collect();
        assert_eq!(
            shape,
            [
                (Some(TurnPhase::Start), 0),
                (Some(TurnPhase::End), 12_000),
                (Some(TurnPhase::Start), 60_000),
                (Some(TurnPhase::End), 66_000),
                // The `/context` record (at 120 s) and the cost (at 125 s) after the last turn.
                (None, 125_000),
            ],
            "{:#?}",
            folded.reports
        );
        assert!(
            folded
                .reports
                .iter()
                .all(|(report, _)| report.source == InsightSourceKind::Transcript)
        );
        let (first_turn, _) = &folded.reports[1];
        assert_eq!(
            first_turn.tokens.and_then(|t| t.output_tokens),
            Some(12 + 28 + 80 + 150 + 200)
        );
        assert_eq!(first_turn.tools.len(), 3);
        assert_eq!(
            first_turn
                .tools
                .iter()
                .map(|t| (t.name.as_str(), t.calls, t.ms))
                .collect::<Vec<_>>(),
            [
                ("Bash", 1, Some(2_300)),
                ("Edit", 1, Some(400)),
                ("Write", 1, Some(100))
            ]
        );
        assert_eq!(first_turn.files.len(), 2);
        assert_eq!(first_turn.context.map(|c| c.used), Some(12_765));
        let second = &folded.reports[3].0;
        assert_eq!(second.tools[0].failed, 1);
        assert_eq!(
            second.turn.and_then(|t| t.outcome),
            Some(InsightOutcome::Cancelled)
        );
        // After the turns: the window from `/context`, the cost, the API time.
        let last = &folded.reports[4].0;
        assert_eq!(
            last.context.map(|c| (c.used, c.window)),
            Some((16_964, Some(1_000_000)))
        );
        assert_eq!(last.cost_usd, Some(0.42));
        assert_eq!(last.api_ms, Some(9_100));
        assert_eq!(folded.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(
            folded.session_key.as_deref(),
            Some("5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60")
        );
    }

    #[test]
    fn the_context_by_category_is_kept_apart_in_the_insights_vocabulary() {
        let folded = fold_text(Source::Claude, CLAUDE);
        let (rows, at) = folded.breakdown.expect("a /context record");
        assert_eq!(at - T0, 120_000);
        let words: Vec<(&str, u64, BreakdownKind)> = rows
            .iter()
            .map(|row| (row.id.as_str(), row.tokens, row.kind))
            .collect();
        assert_eq!(
            words,
            [
                ("system_prompt", 1_877, BreakdownKind::Used),
                ("tools", 13_055, BreakdownKind::Used),
                ("tools_deferred", 19_286, BreakdownKind::Deferred),
                ("skills", 2_032, BreakdownKind::Used),
                ("autocompact_buffer", 33_000, BreakdownKind::Buffer),
                ("free", 950_036, BreakdownKind::Free),
            ]
        );
    }

    #[test]
    fn a_codex_rollout_folds_the_same_way() {
        let folded = fold_text(Source::Codex, CODEX);
        let whole: Vec<(Option<TurnPhase>, u64)> = folded
            .reports
            .iter()
            .map(|(report, at)| (turn(report), at - T0))
            .collect();
        assert_eq!(
            whole,
            [
                (Some(TurnPhase::Start), 1_000),
                (Some(TurnPhase::End), 14_000),
                (Some(TurnPhase::Start), 60_000),
                (Some(TurnPhase::End), 64_000),
            ]
        );
        let first = &folded.reports[1].0;
        assert_eq!(first.tokens.and_then(|t| t.total_tokens), Some(4_100));
        assert_eq!(
            first.context.map(|c| (c.used, c.window)),
            Some((1_580, Some(272_000)))
        );
        assert_eq!(
            first
                .tools
                .iter()
                .map(|t| (t.name.as_str(), t.calls, t.failed, t.ms))
                .collect::<Vec<_>>(),
            [("Bash", 2, 1, Some(4_500)), ("apply_patch", 1, 0, None)]
        );
        assert_eq!(first.files.len(), 2);
        assert_eq!(folded.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(
            folded.session_key.as_deref(),
            Some("0198aaaa-bbbb-cccc-dddd-eeeeffff0001")
        );
    }

    #[test]
    fn more_than_sixteen_tools_or_thirty_two_files_split_into_further_reports() {
        let mut events = vec![UsageEvent::TurnStart { ts_ms: Some(10) }];
        for n in 0..20 {
            events.push(UsageEvent::Tool {
                ts_ms: Some(11),
                name: format!("t{n}"),
                failed: false,
                ms: None,
            });
        }
        for n in 0..40 {
            events.push(UsageEvent::File {
                ts_ms: Some(12),
                path: format!("/w/f{n}"),
                kind: FileKind::Update,
            });
        }
        events.push(UsageEvent::TurnEnd {
            ts_ms: Some(13),
            outcome: Outcome::Completed,
            duration_ms: None,
        });
        let mut folded = Folded::default();
        fold(events, 1, &mut folded);
        assert!(folded.reports.iter().all(|(report, _)| {
            report.tools.len() <= MAX_REPORT_TOOLS && report.files.len() <= MAX_REPORT_FILES
        }));
        let tools: usize = folded.reports.iter().map(|(r, _)| r.tools.len()).sum();
        let files: usize = folded.reports.iter().map(|(r, _)| r.files.len()).sum();
        assert_eq!((tools, files), (20, 40));
        assert_eq!(
            turn(&folded.reports.last().unwrap().0),
            Some(TurnPhase::End),
            "the end comes last"
        );
    }

    #[test]
    fn events_without_a_time_take_the_newest_one_before_them_or_the_fallback() {
        let mut folded = Folded::default();
        fold(
            vec![
                UsageEvent::Tool {
                    ts_ms: None,
                    name: "a".into(),
                    failed: false,
                    ms: None,
                },
                UsageEvent::TurnStart { ts_ms: Some(50) },
            ],
            7,
            &mut folded,
        );
        let times: Vec<u64> = folded.reports.iter().map(|(_, at)| *at).collect();
        assert_eq!(times, [7, 50]);
    }
}
