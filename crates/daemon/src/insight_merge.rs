//! Merging what a terminal agent's integrations reported with what its transcript says
//! (INSIGHT-C): one record, each block from one source, never added together.
//!
//! The integrations (the OMP extension, Claude Code's and Codex's hooks, the status-line tee) and
//! the transcript each fill their own [`InsightReducer`]; this puts the two snapshots together
//! block by block with the spike's rule (`docs/spikes/2026-10-28-insight.md` § 5): **the best
//! source that reported a block wins it** — extension, status line, hooks, transcript — and a
//! lower source only fills a block a better one never reported. Two refinements, both about what
//! a source *has*, not who it is:
//!
//! - **Turns** (and with them the totals): a source that reports turns without tokens (hooks) does
//!   not hide the tokens another source has. The transcript's turns win when they carry tokens and
//!   the better source's do not.
//! - **Tools**: likewise for times (Codex's hooks name the tool, its transcript says how long it
//!   took).
//!
//! The cost is a number apart: whichever source has one fills it. Because every block comes from one
//! side whole, a tool is counted once however many sources saw it: no double counting.

use hutan_core::insight::{
    AgentInsight, BreakdownKind, BreakdownRow, Exactness, InsightBlock, InsightField, InsightModel,
    InsightSource, InsightSourceKind, InsightTime, MAX_SNAPSHOT_BYTES,
};

/// What the transcript knows that its reducer cannot say.
#[derive(Debug, Clone, Copy)]
pub struct Extras<'a> {
    /// The model the transcript names.
    pub model: Option<&'a str>,
    /// The context by category of the newest `/context` record, and when it was written.
    pub breakdown: Option<(&'a [BreakdownRow], u64)>,
    /// The transcript was followed from its middle: older turns are not counted.
    pub cut: bool,
}

fn rank(insight: &AgentInsight, block: InsightBlock) -> Option<u8> {
    insight
        .sources
        .iter()
        .find(|source| source.block == block)
        .map(|source| source.source.rank())
}

fn entry(insight: &AgentInsight, block: InsightBlock) -> Option<InsightSource> {
    insight
        .sources
        .iter()
        .find(|source| source.block == block)
        .cloned()
}

/// Whether `other` takes a block from `base` on the sources' ranks alone: the better source
/// wins, `base` on a tie.
fn better(base: Option<u8>, other: Option<u8>) -> Option<bool> {
    match (base, other) {
        (None, None) => None,
        (Some(_), None) => Some(false),
        (None, Some(_)) => Some(true),
        (Some(base), Some(other)) => Some(other < base),
    }
}

fn has_tokens(insight: &AgentInsight) -> bool {
    insight.totals.as_ref().is_some_and(|totals| {
        totals.input.is_some()
            || totals.output.is_some()
            || totals.cache_read.is_some()
            || totals.total.is_some()
    })
}

fn has_tool_times(insight: &AgentInsight) -> bool {
    insight
        .tools
        .as_ref()
        .is_some_and(|tools| tools.by_name.iter().any(|tool| tool.ms.is_some()))
}

/// `insight` with the transcript's blocks where they win; see the module's rules.
pub fn merge(base: &AgentInsight, transcript: &AgentInsight, extras: Extras<'_>) -> AgentInsight {
    let mut out = base.clone();
    let mut sources: Vec<InsightSource> = Vec::new();

    // Context ------------------------------------------------------------------------------
    let context_from = if base.context.is_none() {
        transcript.context.is_some()
    } else {
        transcript.context.is_some()
            && better(
                rank(base, InsightBlock::Context),
                rank(transcript, InsightBlock::Context),
            ) == Some(true)
    };
    let (winner, loser) = if context_from {
        (transcript, base)
    } else {
        (base, transcript)
    };
    out.context = winner.context.clone();
    if let (Some(context), Some(other)) = (out.context.as_mut(), loser.context.as_ref())
        && context.window.is_none()
    {
        // The window is the model's, whoever read it.
        context.window = other.window;
    }
    if out.context.is_some()
        && let Some(source) = entry(winner, InsightBlock::Context)
    {
        sources.push(source);
    }

    // Breakdown ----------------------------------------------------------------------------
    let base_breakdown = base
        .context
        .as_ref()
        .filter(|context| !context.breakdown.is_empty());
    let transcript_rank = InsightSourceKind::Transcript.rank();
    let from_transcript = match (base_breakdown, extras.breakdown) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(_), Some(_)) => {
            transcript_rank < rank(base, InsightBlock::Breakdown).unwrap_or(u8::MAX)
        }
    };
    if let Some(context) = out.context.as_mut() {
        if from_transcript && let Some((rows, at)) = extras.breakdown {
            context.breakdown = rows.to_vec();
            context.breakdown_as_of_ms = Some(at);
            context.breakdown_exactness = Some(Exactness::Reported);
            sources.push(InsightSource {
                block: InsightBlock::Breakdown,
                source: InsightSourceKind::Transcript,
                as_of_ms: at,
            });
        } else if let Some(held) = base_breakdown {
            // The context may have come from the transcript, the categories from the base.
            context.breakdown = held.breakdown.clone();
            context.breakdown_as_of_ms = held.breakdown_as_of_ms;
            context.breakdown_exactness = held.breakdown_exactness;
            if let Some(source) = entry(base, InsightBlock::Breakdown) {
                sources.push(source);
            }
        }
        context.compact_at = context.window.and_then(|window| {
            context
                .breakdown
                .iter()
                .find(|row| row.kind == BreakdownKind::Buffer)
                .map(|buffer| window.saturating_sub(buffer.tokens))
        });
        if !context.breakdown.is_empty() {
            out.unsupported
                .retain(|field| *field != InsightField::Breakdown);
        }
    }

    // Turns and the totals -----------------------------------------------------------------
    let turns_rank = (
        rank(base, InsightBlock::Turns),
        rank(transcript, InsightBlock::Turns),
    );
    let turns_from_transcript = match turns_rank {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(a), Some(b)) => match (has_tokens(base), has_tokens(transcript)) {
            (false, true) => true,
            (true, false) => false,
            _ => b < a,
        },
    };
    let (turns_winner, turns_loser) = if turns_from_transcript {
        (transcript, base)
    } else {
        (base, transcript)
    };
    if turns_rank.0.is_some() || turns_rank.1.is_some() {
        out.totals = turns_winner.totals.clone();
        out.turns = turns_winner.turns.clone();
        out.turns_truncated = turns_winner.turns_truncated || (turns_from_transcript && extras.cut);
        if let Some(source) = entry(turns_winner, InsightBlock::Turns) {
            sources.push(source);
        }
    }
    // The cost is a number of its own, whatever block the turns came from: the integrations' (a
    // status line's, the extension's) when they have one, else the transcript's.
    let cost_of = |insight: &AgentInsight| {
        insight
            .totals
            .as_ref()
            .and_then(|totals| totals.cost_usd.map(|cost| (cost, totals.cost_kind)))
    };
    if let Some((cost, kind)) = cost_of(base).or_else(|| cost_of(transcript)) {
        match out.totals.as_mut() {
            Some(totals) => {
                totals.cost_usd = Some(cost);
                totals.cost_kind = kind;
            }
            None => {
                out.totals = if cost_of(base).is_some() {
                    base.totals.clone()
                } else {
                    transcript.totals.clone()
                };
            }
        }
    }
    if out
        .totals
        .as_ref()
        .is_some_and(|totals| totals.cost_usd.is_some())
    {
        out.unsupported.retain(|field| *field != InsightField::Cost);
    }

    // Tools --------------------------------------------------------------------------------
    let tools_rank = (
        rank(base, InsightBlock::Tools),
        rank(transcript, InsightBlock::Tools),
    );
    let tools_from_transcript = match tools_rank {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(a), Some(b)) => match (has_tool_times(base), has_tool_times(transcript)) {
            (false, true) => true,
            (true, false) => false,
            _ => b < a,
        },
    };
    let tools_winner = if tools_from_transcript {
        transcript
    } else {
        base
    };
    if tools_rank.0.is_some() || tools_rank.1.is_some() {
        out.tools = tools_winner.tools.clone();
        if let Some(source) = entry(tools_winner, InsightBlock::Tools) {
            sources.push(source);
        }
    }

    // Files --------------------------------------------------------------------------------
    let files_rank = (
        rank(base, InsightBlock::Files),
        rank(transcript, InsightBlock::Files),
    );
    if better(files_rank.0, files_rank.1) == Some(true) {
        out.files = transcript.files.clone();
    }
    if files_rank.0.is_some() || files_rank.1.is_some() {
        let winner = if better(files_rank.0, files_rank.1) == Some(true) {
            transcript
        } else {
            base
        };
        if let Some(source) = entry(winner, InsightBlock::Files) {
            sources.push(source);
        }
    }

    // Time ---------------------------------------------------------------------------------
    out.time = match (base.time.as_ref(), transcript.time.as_ref()) {
        (None, None) => None,
        (one, other) => {
            let started_ms = [one, other]
                .into_iter()
                .flatten()
                .map(|time| time.started_ms)
                .min()
                .unwrap_or(0);
            let of = |insight: &AgentInsight| insight.time.clone();
            let api_from_transcript = match (
                base.time.as_ref().and_then(|time| time.api_ms),
                transcript.time.as_ref().and_then(|time| time.api_ms),
            ) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some(_), Some(_)) => {
                    better(
                        rank(base, InsightBlock::Time),
                        rank(transcript, InsightBlock::Time),
                    ) == Some(true)
                }
            };
            let api_ms = if api_from_transcript {
                of(transcript).and_then(|time| time.api_ms)
            } else {
                of(base)
                    .and_then(|time| time.api_ms)
                    .or_else(|| of(transcript).and_then(|time| time.api_ms))
            };
            let active_ms = of(turns_winner)
                .and_then(|time| time.active_ms)
                .or_else(|| of(turns_loser).and_then(|time| time.active_ms));
            let tool_ms = of(tools_winner).and_then(|time| time.tool_ms).or_else(|| {
                of(if tools_from_transcript {
                    base
                } else {
                    transcript
                })
                .and_then(|time| time.tool_ms)
            });
            Some(InsightTime {
                started_ms,
                wall_ms: base.as_of_ms.saturating_sub(started_ms),
                active_ms,
                api_ms,
                tool_ms,
            })
        }
    };
    let time_winner = if api_source_is_transcript(base, transcript) {
        transcript
    } else {
        base
    };
    if let Some(source) = entry(time_winner, InsightBlock::Time)
        .or_else(|| entry(base, InsightBlock::Time))
        .or_else(|| entry(transcript, InsightBlock::Time))
    {
        sources.push(source);
    }

    // The rest -----------------------------------------------------------------------------
    if out.model.is_none() {
        out.model = extras.model.map(|id| InsightModel {
            id: id.to_owned(),
            provider: None,
            thinking: None,
        });
    }
    sources.sort_by_key(|source| match source.block {
        InsightBlock::Context => 0,
        InsightBlock::Breakdown => 1,
        InsightBlock::Turns => 2,
        InsightBlock::Tools => 3,
        InsightBlock::Files => 4,
        InsightBlock::Time => 5,
    });
    out.sources = sources;
    fit(&mut out);
    out
}

/// Whether the transcript is who the time block is named after: the one that has the API time,
/// when it is the better source for it.
fn api_source_is_transcript(base: &AgentInsight, transcript: &AgentInsight) -> bool {
    let api = |insight: &AgentInsight| insight.time.as_ref().and_then(|time| time.api_ms);
    match (api(base), api(transcript)) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(_), Some(_)) => {
            better(
                rank(base, InsightBlock::Time),
                rank(transcript, InsightBlock::Time),
            ) == Some(true)
        }
    }
}

/// Cuts `insight` until its JSON is at most [`MAX_SNAPSHOT_BYTES`], as the reducer does for one
/// source: the oldest turns and the last files go first.
fn fit(insight: &mut AgentInsight) {
    loop {
        let size = serde_json::to_vec(insight).map_or(0, |json| json.len());
        if size <= MAX_SNAPSHOT_BYTES {
            return;
        }
        let files = insight.files.as_ref().map_or(0, |files| files.items.len());
        if insight.turns.len() > 50 {
            let cut = (insight.turns.len() / 4).max(1);
            insight.turns.drain(..cut);
            insight.turns_truncated = true;
        } else if files > 25 {
            if let Some(list) = insight.files.as_mut() {
                list.items.truncate(files / 2);
                list.count = list.items.len() as u64;
                list.truncated = true;
            }
        } else if !insight.turns.is_empty() {
            insight.turns.remove(0);
            insight.turns_truncated = true;
        } else if files > 0 {
            if let Some(list) = insight.files.as_mut() {
                list.items.pop();
                list.count = list.items.len() as u64;
                list.truncated = true;
            }
        } else {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use hutan_core::SessionId;
    use hutan_core::agent::{FileChangeKind, TokenCounts};
    use hutan_core::insight::{
        InsightOutcome, InsightReducer, InsightReport, ReportedContext, ReportedFile, ReportedTool,
        ReportedTurn, TurnPhase,
    };

    use super::*;

    const NOW: u64 = 2_000_000;

    fn reducer(agent: &str) -> InsightReducer {
        InsightReducer::new_terminal(SessionId::new(), agent, Some("/w/app"))
    }

    fn tool(name: &str, ms: Option<u64>) -> ReportedTool {
        ReportedTool {
            name: name.to_owned(),
            calls: 1,
            failed: 0,
            ms,
        }
    }

    fn turn(phase: TurnPhase) -> Option<ReportedTurn> {
        Some(ReportedTurn {
            phase,
            outcome: (phase == TurnPhase::End).then_some(InsightOutcome::Completed),
            duration_ms: None,
        })
    }

    fn tokens(input: u64, output: u64) -> TokenCounts {
        TokenCounts {
            input_tokens: Some(input),
            output_tokens: Some(output),
            ..TokenCounts::default()
        }
    }

    fn file(path: &str) -> ReportedFile {
        ReportedFile {
            path: path.to_owned(),
            kind: FileChangeKind::Update,
        }
    }

    fn snapshot(reducer: &InsightReducer) -> AgentInsight {
        reducer.snapshot(1, NOW, None)
    }

    fn none() -> Extras<'static> {
        Extras {
            model: None,
            breakdown: None,
            cut: false,
        }
    }

    fn source_of(insight: &AgentInsight, block: InsightBlock) -> Option<InsightSourceKind> {
        insight
            .sources
            .iter()
            .find(|source| source.block == block)
            .map(|source| source.source)
    }

    /// Hooks of Claude Code: turn edges, a tool with its time, a file; no tokens, no context.
    fn hooks() -> InsightReducer {
        let mut r = reducer("claude");
        let at = 1_000_000;
        for (report, t) in [
            (
                InsightReport {
                    source: InsightSourceKind::Hooks,
                    turn: turn(TurnPhase::Start),
                    ..InsightReport::default()
                },
                at,
            ),
            (
                InsightReport {
                    source: InsightSourceKind::Hooks,
                    tools: vec![tool("Bash", Some(2_000))],
                    files: vec![file("src/lib.rs")],
                    turn: turn(TurnPhase::End),
                    ..InsightReport::default()
                },
                at + 10_000,
            ),
        ] {
            r.apply_report(&report, t);
        }
        r
    }

    /// The transcript of the same turn: tokens, context, a tool with another time, the file.
    fn transcript() -> InsightReducer {
        let mut r = reducer("claude");
        let at = 999_000;
        for (report, t) in [
            (
                InsightReport {
                    source: InsightSourceKind::Transcript,
                    turn: turn(TurnPhase::Start),
                    ..InsightReport::default()
                },
                at,
            ),
            (
                InsightReport {
                    source: InsightSourceKind::Transcript,
                    tokens: Some(tokens(5_000, 300)),
                    context: Some(ReportedContext {
                        used: 5_300,
                        window: None,
                        fixed: None,
                        estimated: false,
                    }),
                    cost_usd: Some(0.5),
                    tools: vec![tool("Bash", Some(2_300))],
                    files: vec![file("src/lib.rs")],
                    turn: turn(TurnPhase::End),
                    ..InsightReport::default()
                },
                at + 12_000,
            ),
        ] {
            r.apply_report(&report, t);
        }
        r
    }

    #[test]
    fn a_session_with_no_integration_has_the_transcripts_blocks() {
        let empty = snapshot(&reducer("codex"));
        let merged = merge(&empty, &snapshot(&transcript()), none());
        assert_eq!(merged.totals.as_ref().map(|t| t.turns), Some(1));
        assert_eq!(merged.context.as_ref().map(|c| c.used), Some(5_300));
        assert_eq!(merged.tools.as_ref().map(|t| t.calls), Some(1));
        for block in [
            InsightBlock::Context,
            InsightBlock::Turns,
            InsightBlock::Tools,
            InsightBlock::Files,
            InsightBlock::Time,
        ] {
            assert_eq!(
                source_of(&merged, block),
                Some(InsightSourceKind::Transcript),
                "{block:?}"
            );
        }
    }

    #[test]
    fn the_hooks_and_the_transcript_of_one_turn_are_not_added_together() {
        let base = snapshot(&hooks());
        let merged = merge(&base, &snapshot(&transcript()), none());
        // One tool call, one file, one turn: not two.
        assert_eq!(merged.tools.as_ref().map(|t| t.calls), Some(1));
        assert_eq!(merged.files.as_ref().map(|f| f.count), Some(1));
        assert_eq!(merged.totals.as_ref().map(|t| t.turns), Some(1));
        assert_eq!(merged.turns.len(), 1);
        // The hooks win what both report and both have the numbers of (their rank is higher):
        // tools with a time, files.
        assert_eq!(
            source_of(&merged, InsightBlock::Tools),
            Some(InsightSourceKind::Hooks)
        );
        assert_eq!(
            merged.tools.as_ref().map(|t| t.by_name[0].ms),
            Some(Some(2_000))
        );
        assert_eq!(
            source_of(&merged, InsightBlock::Files),
            Some(InsightSourceKind::Hooks)
        );
        // Hooks carry no tokens: the transcript's turns, with theirs, are shown.
        assert_eq!(
            source_of(&merged, InsightBlock::Turns),
            Some(InsightSourceKind::Transcript)
        );
        let totals = merged.totals.as_ref().expect("totals");
        assert_eq!((totals.input, totals.output), (Some(5_000), Some(300)));
        assert_eq!(totals.cost_usd, Some(0.5));
        // The context only the transcript has.
        assert_eq!(merged.context.as_ref().map(|c| c.used), Some(5_300));
        assert_eq!(
            source_of(&merged, InsightBlock::Context),
            Some(InsightSourceKind::Transcript)
        );
    }

    #[test]
    fn a_better_source_keeps_the_context_and_the_transcript_fills_the_window() {
        let mut statusline = hooks();
        statusline.apply_report(
            &InsightReport {
                source: InsightSourceKind::Statusline,
                context: Some(ReportedContext {
                    used: 6_000,
                    window: Some(200_000),
                    fixed: None,
                    estimated: false,
                }),
                cost_usd: Some(0.9),
                api_ms: Some(4_000),
                ..InsightReport::default()
            },
            1_020_000,
        );
        let mut t = transcript();
        t.apply_report(
            &InsightReport {
                source: InsightSourceKind::Transcript,
                api_ms: Some(9_100),
                ..InsightReport::default()
            },
            1_030_000,
        );
        let merged = merge(&snapshot(&statusline), &snapshot(&t), none());
        let context = merged.context.as_ref().expect("a context");
        assert_eq!((context.used, context.window), (6_000, Some(200_000)));
        assert_eq!(
            source_of(&merged, InsightBlock::Context),
            Some(InsightSourceKind::Statusline)
        );
        // The status line's API time beats the transcript's, and so does its cost; the totals'
        // tokens are the transcript's (hooks carry none).
        assert_eq!(merged.time.as_ref().and_then(|t| t.api_ms), Some(4_000));
        assert_eq!(merged.totals.as_ref().and_then(|t| t.cost_usd), Some(0.9));
        assert_eq!(merged.totals.as_ref().and_then(|t| t.input), Some(5_000));
    }

    #[test]
    fn the_cost_of_a_source_without_turns_fills_the_totals_of_the_other() {
        let mut costed = reducer("claude");
        costed.apply_report(
            &InsightReport {
                source: InsightSourceKind::Statusline,
                cost_usd: Some(1.25),
                ..InsightReport::default()
            },
            1_000,
        );
        let mut t = reducer("claude");
        for (turn_report, at) in [(turn(TurnPhase::Start), 10), (turn(TurnPhase::End), 20)] {
            t.apply_report(
                &InsightReport {
                    source: InsightSourceKind::Transcript,
                    turn: turn_report,
                    tokens: Some(tokens(10, 1)),
                    ..InsightReport::default()
                },
                at,
            );
        }
        let merged = merge(&snapshot(&costed), &snapshot(&t), none());
        let totals = merged.totals.as_ref().expect("totals");
        assert_eq!(totals.input, Some(20));
        assert_eq!(totals.cost_usd, Some(1.25));
    }

    #[test]
    fn a_transcript_with_tool_times_beats_hooks_without_them() {
        let mut codex_hooks = reducer("codex");
        codex_hooks.apply_report(
            &InsightReport {
                source: InsightSourceKind::Hooks,
                tools: vec![tool("Bash", None)],
                ..InsightReport::default()
            },
            1_000,
        );
        let mut t = reducer("codex");
        t.apply_report(
            &InsightReport {
                source: InsightSourceKind::Transcript,
                tools: vec![tool("Bash", Some(1_500))],
                ..InsightReport::default()
            },
            1_000,
        );
        let merged = merge(&snapshot(&codex_hooks), &snapshot(&t), none());
        assert_eq!(
            source_of(&merged, InsightBlock::Tools),
            Some(InsightSourceKind::Transcript)
        );
        assert_eq!(
            merged.tools.as_ref().map(|t| t.by_name[0].ms),
            Some(Some(1_500))
        );
    }

    #[test]
    fn the_context_by_category_comes_from_the_transcript_and_the_model_when_nobody_else_has_them() {
        let rows = vec![
            BreakdownRow {
                id: "system_prompt".into(),
                label: "System prompt".into(),
                tokens: 1_877,
                kind: BreakdownKind::Used,
            },
            BreakdownRow {
                id: "autocompact_buffer".into(),
                label: "Autocompact buffer".into(),
                tokens: 33_000,
                kind: BreakdownKind::Buffer,
            },
        ];
        let mut t = transcript();
        t.apply_report(
            &InsightReport {
                source: InsightSourceKind::Transcript,
                context: Some(ReportedContext {
                    used: 16_964,
                    window: Some(1_000_000),
                    fixed: None,
                    estimated: false,
                }),
                ..InsightReport::default()
            },
            1_010_000,
        );
        let merged = merge(
            &snapshot(&hooks()),
            &snapshot(&t),
            Extras {
                model: Some("claude-opus-5-5"),
                breakdown: Some((&rows, 1_010_000)),
                cut: true,
            },
        );
        let context = merged.context.as_ref().expect("a context");
        assert_eq!(context.breakdown.len(), 2);
        assert_eq!(context.breakdown_exactness, Some(Exactness::Reported));
        assert_eq!(context.compact_at, Some(1_000_000 - 33_000));
        assert_eq!(
            source_of(&merged, InsightBlock::Breakdown),
            Some(InsightSourceKind::Transcript)
        );
        assert!(!merged.unsupported.contains(&InsightField::Breakdown));
        assert_eq!(
            merged.model.as_ref().map(|m| m.id.as_str()),
            Some("claude-opus-5-5")
        );
        assert!(
            merged.turns_truncated,
            "a transcript followed from its middle says so"
        );
        // Without the breakdown the claim stands: a terminal Claude Code has none.
        let plain = merge(&snapshot(&hooks()), &snapshot(&transcript()), none());
        assert!(plain.unsupported.contains(&InsightField::Breakdown));
    }

    #[test]
    fn nothing_reported_by_either_stays_nothing() {
        let merged = merge(
            &snapshot(&reducer("claude")),
            &snapshot(&reducer("claude")),
            none(),
        );
        assert!(merged.sources.is_empty());
        assert!(merged.totals.is_none() && merged.context.is_none() && merged.tools.is_none());
    }

    #[test]
    fn the_merged_record_fits_the_snapshot_budget() {
        let mut big = reducer("claude");
        for n in 0..180u64 {
            for (turn_report, at) in [
                (turn(TurnPhase::Start), n * 100),
                (turn(TurnPhase::End), n * 100 + 50),
            ] {
                big.apply_report(
                    &InsightReport {
                        source: InsightSourceKind::Transcript,
                        turn: turn_report,
                        tokens: Some(tokens(100 + n, 10)),
                        files: vec![file(&format!("{}{n}", "dir/".repeat(50)))],
                        ..InsightReport::default()
                    },
                    at,
                );
            }
        }
        let merged = merge(&snapshot(&reducer("claude")), &snapshot(&big), none());
        let size = serde_json::to_vec(&merged).unwrap().len();
        assert!(size <= MAX_SNAPSHOT_BYTES, "{size}");
    }
}
