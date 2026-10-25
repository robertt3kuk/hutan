//! `hutan usage [--json] [--cached]`: the limit windows the harnesses report and the tokens and
//! cost of the turns Hutan ran (protocol 0.45, capability `usage`; `docs/agents.md` § Usage).
//! Nothing here is a credential: a limit is a label, a percentage and a reset time, and a cost is
//! always the harness's own estimate.

use hutan_protocol::usage::{UsageSummary, UsageTotals};

use super::Ctx;
use crate::args::UsageArgs;
use crate::error::CliError;

impl Ctx {
    pub(super) async fn usage(&self, args: UsageArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_usage() {
            return Err(CliError::Failed(
                "this hutand does not report usage (it is older than protocol 0.45); restart it"
                    .to_owned(),
            ));
        }
        let summary = client
            .usage_summary(!args.cached)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&summary, || usage_text(&summary, now_ms()))
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
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

/// `42m`, `3h 10m`, `2d 4h`.
fn reset_in(resets_at_ms: u64, now_ms: u64) -> String {
    let minutes = resets_at_ms.saturating_sub(now_ms).div_ceil(60_000);
    match minutes {
        0 => "now".to_owned(),
        1..=59 => format!("{minutes}m"),
        60..=1439 => match minutes % 60 {
            0 => format!("{}h", minutes / 60),
            rest => format!("{}h {rest}m", minutes / 60),
        },
        _ => match (minutes % 1440) / 60 {
            0 => format!("{}d", minutes / 1440),
            hours => format!("{}d {hours}h", minutes / 1440),
        },
    }
}

fn totals_text(totals: &UsageTotals) -> String {
    if totals.turns == 0 {
        return "no turns".to_owned();
    }
    let mut text = format!(
        "{} turn{} · {} tokens",
        totals.turns,
        if totals.turns == 1 { "" } else { "s" },
        totals.total_tokens
    );
    if let Some(cost) = totals.cost_usd {
        text.push_str(&format!(" · ≈ ${cost:.4}"));
        if totals.cost_is_estimate {
            text.push_str(" (estimate)");
        }
    }
    text
}

fn bar(used_percent: f64) -> String {
    let filled = ((used_percent / 10.0).round().clamp(0.0, 10.0)) as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}

/// The human rendering: per harness its running limit windows, then today and this week; then
/// the week by provider.
pub fn usage_text(summary: &UsageSummary, now_ms: u64) -> String {
    if summary.harnesses.is_empty() {
        return "no usage yet: no harness has reported a limit and no turn has run \
                (Codex reports its limits with its replies; `hutan usage` asks OMP)"
            .to_owned();
    }
    let mut lines = Vec::new();
    for harness in &summary.harnesses {
        lines.push(harness_name(&harness.harness).to_owned());
        let running: Vec<_> = harness
            .limits
            .iter()
            .filter(|l| l.resets_at_ms.is_none_or(|r| r > now_ms))
            .collect();
        for limit in &running {
            let mut line = format!(
                "  {:<8} {:<10} {} {:>4.0}%",
                limit.label,
                limit.provider,
                bar(limit.used_percent),
                limit.used_percent
            );
            if let Some(account) = limit.account_label.as_ref().or(limit.account.as_ref()) {
                line.push_str(&format!("  {account}"));
            }
            if let Some(resets) = limit.resets_at_ms {
                line.push_str(&format!("  resets in {}", reset_in(resets, now_ms)));
            }
            lines.push(line);
        }
        if running.is_empty() && harness.harness != "direct" {
            lines.push("  no limit reported".to_owned());
        }
        lines.push(format!("  today      {}", totals_text(&harness.today)));
        lines.push(format!("  this week  {}", totals_text(&harness.week)));
    }
    if !summary.providers.is_empty() {
        lines.push("By provider, this week".to_owned());
        for provider in &summary.providers {
            lines.push(format!(
                "  {:<14} {}",
                provider.provider,
                totals_text(&provider.week)
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::usage::{HarnessUsage, UsageLimit};

    #[test]
    fn the_human_rendering_shows_bars_resets_and_labels_a_cost_as_an_estimate() {
        let now = 1_000_000_000;
        let summary = UsageSummary {
            generated_at_ms: now,
            day_start_ms: 0,
            week_start_ms: 0,
            harnesses: vec![HarnessUsage {
                harness: "codex".into(),
                limits: vec![UsageLimit {
                    account: None,
                    account_label: None,
                    provider: "openai".into(),
                    id: "5h".into(),
                    label: "5-hour".into(),
                    used_percent: 72.0,
                    window_minutes: Some(300),
                    resets_at_ms: Some(now + 3 * 3_600_000),
                    plan: None,
                    observed_at_ms: now,
                }],
                today: UsageTotals {
                    turns: 2,
                    total_tokens: 100,
                    cost_usd: Some(0.5),
                    cost_is_estimate: true,
                    ..UsageTotals::default()
                },
                week: UsageTotals::default(),
            }],
            providers: vec![],
        };
        let text = usage_text(&summary, now);
        assert!(text.contains("Codex"), "{text}");
        assert!(
            text.contains("███████░░░") && text.contains("72%"),
            "{text}"
        );
        assert!(text.contains("resets in 3h"), "{text}");
        assert!(text.contains("≈ $0.5000 (estimate)"), "{text}");
        assert!(text.contains("this week  no turns"), "{text}");
    }
}
