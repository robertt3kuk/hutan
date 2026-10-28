//! `hutan chat insight SESSION [--measure] [--json]`: what a chat's context holds and what its
//! turns spent, from the daemon (protocol 0.59, capability `agent.insight`;
//! `docs/agents.md` § Insight). A number the harness did not report is not printed, and what a
//! harness can never report is said, not drawn as zero.

use super::Ctx;
use crate::args::ChatInsightArgs;
use crate::error::CliError;
use hutan_core::insight::{
    AgentInsight, BreakdownKind, CostKind, Exactness, InsightField, InsightOutcome,
};

impl Ctx {
    pub(super) async fn chat_insight(&self, args: ChatInsightArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_agent_insight() {
            return Err(CliError::Failed(
                "this daemon keeps no agent insight (it predates protocol 0.59); restart it with \
                 `hutan daemon stop` and run the command again"
                    .to_owned(),
            ));
        }
        let session = self.resolve_chat(&client, &args.target).await?;
        let insight = if args.measure {
            let measured = client
                .agent_measure_context(session.id)
                .await
                .map_err(|e| self.fail(e))?;
            if !measured.measured && !self.json {
                eprintln!(
                    "the agent did not print its context by category; the breakdown is what it was"
                );
            }
            measured.insight
        } else {
            client
                .agent_insight(session.id, None)
                .await
                .map_err(|e| self.fail(e))?
                .insight
        };
        self.print(&insight, || insight_text(&insight))
    }
}

/// `11.2k`, `1.5m`, `850`.
fn count(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0).replace(".0k", "k"),
        _ => format!("{:.1}m", n as f64 / 1_000_000.0).replace(".0m", "m"),
    }
}

/// `850 ms`, `3.9 s`, `3m 20s`, `2h 5m`.
fn duration(ms: u64) -> String {
    let seconds = ms / 1_000;
    match seconds {
        0 => format!("{ms} ms"),
        1..=9 => format!("{:.1} s", ms as f64 / 1_000.0),
        10..=59 => format!("{seconds} s"),
        60..=3_599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3_600, (seconds % 3_600) / 60),
    }
}

fn harness_name(harness: &str) -> &str {
    match harness {
        "omp" => "OMP",
        "claude" => "Claude Code",
        "codex" => "Codex",
        "direct" => "Direct chat",
        other => other,
    }
}

fn field_name(field: InsightField) -> &'static str {
    match field {
        InsightField::Breakdown => "the context by category",
        InsightField::Cost => "a cost",
        InsightField::Reasoning => "reasoning tokens",
    }
}

/// The human rendering of an insight.
pub fn insight_text(insight: &AgentInsight) -> String {
    let mut lines = Vec::new();
    let harness = insight.harness.as_deref().map_or("agent", harness_name);
    let model = insight.model.as_ref().map(|m| m.id.as_str()).unwrap_or("");
    lines.push(format!("{harness} {model}").trim().to_owned());

    match &insight.context {
        Some(context) => {
            let approx = if context.exactness == Exactness::Estimated {
                "≈ "
            } else {
                ""
            };
            let mut line = format!("Context    {approx}{} tokens", count(context.used));
            if let Some(window) = context.window {
                line.push_str(&format!(
                    " of {} ({}%)",
                    count(window),
                    (context.used.saturating_mul(100) + window / 2) / window.max(1)
                ));
            }
            lines.push(line);
            if let Some(compact_at) = context.compact_at {
                lines.push(format!(
                    "           compacts by itself at {}",
                    count(compact_at)
                ));
            }
            if context.breakdown.is_empty() {
                if insight.can_measure {
                    lines.push(
                        "           by category: not measured (`--measure` asks the agent: /context)"
                            .to_owned(),
                    );
                }
            } else {
                let rounded = context.breakdown_exactness == Some(Exactness::Estimated);
                lines.push(format!(
                    "           by category{}:",
                    if rounded {
                        " (rounded by the agent)"
                    } else {
                        ""
                    }
                ));
                for row in &context.breakdown {
                    let note = match row.kind {
                        BreakdownKind::Buffer => "  (reserved for compaction)",
                        BreakdownKind::Free => "  (free)",
                        BreakdownKind::Deferred => "  (loaded when needed)",
                        BreakdownKind::Used => "",
                    };
                    lines.push(format!(
                        "             {:<22} {:>7}{note}",
                        row.label,
                        count(row.tokens)
                    ));
                }
            }
        }
        None => lines.push("Context    not reported yet".to_owned()),
    }

    if let Some(totals) = &insight.totals {
        let mut parts = Vec::new();
        if let Some(n) = totals.input {
            parts.push(format!("{} in", count(n)));
        }
        if let Some(n) = totals.output {
            parts.push(format!("{} out", count(n)));
        }
        if let Some(n) = totals.cache_read {
            parts.push(format!("{} cache read", count(n)));
        }
        if let Some(n) = totals.cache_write {
            parts.push(format!("{} cache write", count(n)));
        }
        if let Some(n) = totals.reasoning {
            parts.push(format!("{} reasoning", count(n)));
        }
        let turns = format!(
            "{} turn{}",
            totals.turns,
            if totals.turns == 1 { "" } else { "s" }
        );
        if parts.is_empty() {
            lines.push(format!("Tokens     {turns}, none reported"));
        } else {
            let mut line = format!("Tokens     {turns}: {}", parts.join(" · "));
            if let Some(ratio) = totals.cache_hit_ratio {
                line.push_str(&format!("  (cache hit {:.0}%)", ratio * 100.0));
            }
            lines.push(line);
        }
        match (totals.cost_usd, totals.cost_kind) {
            (Some(cost), kind) => lines.push(format!(
                "Cost       ≈ ${cost:.4}{}",
                if kind == Some(CostKind::Reported) {
                    ""
                } else {
                    " (the agent's own estimate)"
                }
            )),
            (None, _) if insight.unsupported.contains(&InsightField::Cost) => {
                lines.push("Cost       not reported by this agent".to_owned());
            }
            (None, _) => lines.push("Cost       not reported yet".to_owned()),
        }
    } else {
        lines.push("Tokens     no turn yet".to_owned());
    }

    if !insight.turns.is_empty() {
        lines.push(format!(
            "Turns      newest {}{}",
            insight.turns.len(),
            if insight.turns_truncated {
                " (older ones left out)"
            } else {
                ""
            }
        ));
        for turn in insight.turns.iter().rev().take(5).rev() {
            let mut line = format!("  #{:<4}", turn.n);
            line.push_str(match turn.outcome {
                InsightOutcome::Running => " running  ",
                InsightOutcome::Completed => " completed",
                InsightOutcome::Failed => " failed   ",
                InsightOutcome::Cancelled => " cancelled",
            });
            if let Some(ms) = turn.duration_ms {
                line.push_str(&format!("  {}", duration(ms)));
            }
            let spent: Vec<String> = [("in", turn.input), ("out", turn.output)]
                .into_iter()
                .filter_map(|(label, n)| Some(format!("{} {label}", count(n?))))
                .collect();
            if !spent.is_empty() {
                line.push_str(&format!("  {}", spent.join(" · ")));
            }
            if let Some(cost) = turn.cost_usd {
                line.push_str(&format!("  ≈ ${cost:.4}"));
            }
            if let Some(after) = turn.context_after {
                line.push_str(&format!("  context {}", count(after)));
            }
            if turn.tool_calls > 0 {
                line.push_str(&format!(
                    "  {} tool{}",
                    turn.tool_calls,
                    if turn.tool_calls == 1 { "" } else { "s" }
                ));
            }
            lines.push(line);
        }
    }

    if let Some(tools) = &insight.tools {
        let names: Vec<String> = tools
            .by_name
            .iter()
            .map(|tool| {
                let failed = if tool.failed > 0 {
                    format!(" ({} failed)", tool.failed)
                } else {
                    String::new()
                };
                format!("{} ×{}{failed}", tool.name, tool.calls)
            })
            .collect();
        lines.push(format!(
            "Tools      {} call{}: {}",
            tools.calls,
            if tools.calls == 1 { "" } else { "s" },
            names.join(", ")
        ));
    }
    if let Some(files) = &insight.files {
        lines.push(format!(
            "Files      {} changed{}, +{} −{} lines",
            files.count,
            if files.truncated {
                " (more not listed)"
            } else {
                ""
            },
            files.added,
            files.removed
        ));
        for file in files.items.iter().take(8) {
            let changed = match (file.added, file.removed) {
                (Some(a), Some(r)) => format!("  +{a} −{r}"),
                _ => String::new(),
            };
            lines.push(format!("             {}{changed}", file.path));
        }
        if files.items.len() > 8 {
            lines.push(format!("             … {} more", files.items.len() - 8));
        }
    }
    if let Some(time) = &insight.time {
        let mut line = format!("Time       {} since the first turn", duration(time.wall_ms));
        if let Some(active) = time.active_ms {
            line.push_str(&format!(", {} in turns", duration(active)));
        }
        if let Some(tool) = time.tool_ms {
            line.push_str(&format!(", {} in tools", duration(tool)));
        }
        lines.push(line);
    }
    if !insight.unsupported.is_empty() {
        let names: Vec<&str> = insight.unsupported.iter().map(|f| field_name(*f)).collect();
        lines.push(format!("Not reported by {harness}: {}", names.join(", ")));
    }
    if insight.sources.is_empty() {
        lines.push("Nothing has reported on this chat yet.".to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::SessionId;

    #[test]
    fn numbers_are_short_and_durations_readable() {
        assert_eq!(count(850), "850");
        assert_eq!(count(11_200), "11.2k");
        assert_eq!(count(200_000), "200k");
        assert_eq!(count(1_500_000), "1.5m");
        assert_eq!(duration(850), "850 ms");
        assert_eq!(duration(3_850), "3.9 s");
        assert_eq!(duration(200_000), "3m 20s");
        assert_eq!(duration(7_500_000), "2h 05m");
    }

    #[test]
    fn an_insight_with_nothing_says_so_and_a_codex_says_what_it_never_reports() {
        let empty = AgentInsight::empty(SessionId::new(), 1, 0);
        let text = insight_text(&empty);
        assert!(text.contains("Context    not reported yet"), "{text}");
        assert!(text.contains("Nothing has reported on this chat yet."));
        assert!(
            !text.contains('0'),
            "no zero is drawn for what is missing: {text}"
        );

        let mut codex = AgentInsight::empty(SessionId::new(), 2, 0);
        codex.harness = Some("codex".into());
        codex.unsupported = vec![InsightField::Breakdown, InsightField::Cost];
        codex.totals = Some(hutan_core::insight::InsightTotals {
            turns: 1,
            input: Some(43_732),
            output: Some(55),
            cache_read: None,
            cache_write: None,
            reasoning: None,
            total: Some(43_787),
            cost_usd: None,
            cost_kind: None,
            cache_hit_ratio: None,
        });
        let text = insight_text(&codex);
        assert!(
            text.contains("Tokens     1 turn: 43.7k in · 55 out"),
            "{text}"
        );
        assert!(
            text.contains("Cost       not reported by this agent"),
            "{text}"
        );
        assert!(
            text.contains("Not reported by Codex: the context by category, a cost"),
            "{text}"
        );
    }
}
