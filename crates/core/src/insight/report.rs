//! Reading what a harness prints for `/context`: the breakdown of its context by category.
//!
//! Two shapes are known, both recorded (`docs/spikes/2026-10-28-insight/`):
//!
//! - **OMP** prints one line per category with a bar: `System prompt    [░░░░] 1%  2559 tokens`,
//!   under `Context window: 200000 tokens (6% used)`. The numbers are exact.
//! - **Claude Code** prints markdown: `**Tokens:** 17k / 1m (2%)` and a table
//!   `| System prompt | 1.9k | 0.2% |`. The numbers are rounded by the harness (`1.9k`), so they are
//!   estimates.
//!
//! Anything else is not a report: [`parse`] returns `None` and the caller says so.

use super::{BreakdownKind, BreakdownRow, Exactness, MAX_BREAKDOWN};

/// What a `/context` answer says.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextReport {
    /// The model's window, when the report names it.
    pub window: Option<u64>,
    /// The tokens in the context: the report's own figure, else the sum of the `used` rows.
    pub used: Option<u64>,
    pub rows: Vec<BreakdownRow>,
    pub exactness: Exactness,
}

/// The vocabulary of [`BreakdownRow::id`] for the categories the known harnesses print. A name
/// that is not listed gets its own slug (and the desktop shows its label).
pub fn category_id(label: &str) -> String {
    let lower = label.trim().to_ascii_lowercase();
    let known = match lower.as_str() {
        "system prompt" => "system_prompt",
        "system tools" | "tools" => "tools",
        "system tools (deferred)" => "tools_deferred",
        "system context" | "memory files" | "project context" => "project_context",
        "skills" => "skills",
        "messages" => "messages",
        "mcp tools" | "mcp" => "mcp",
        "mcp tools (deferred)" => "mcp_deferred",
        "custom agents" | "agents" => "agents",
        "autocompact buffer" | "auto-compact buf" | "auto-compact buffer" => "autocompact_buffer",
        "free space" | "free" => "free",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_owned();
    }
    let mut slug = String::new();
    for c in lower.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('_') && !slug.is_empty() {
            slug.push('_');
        }
    }
    let slug = slug.trim_end_matches('_');
    if slug.is_empty() {
        "other".to_owned()
    } else {
        slug.chars().take(48).collect()
    }
}

fn kind_of(id: &str, label: &str) -> BreakdownKind {
    if id == "autocompact_buffer" {
        BreakdownKind::Buffer
    } else if id == "free" {
        BreakdownKind::Free
    } else if label.to_ascii_lowercase().contains("deferred") {
        BreakdownKind::Deferred
    } else {
        BreakdownKind::Used
    }
}

/// Reads `text`, the harness's answer to `/context`, or `None` when it is not one.
pub fn parse(text: &str) -> Option<ContextReport> {
    let clean = strip_ansi(text);
    let report = if clean.contains("| Category |") || clean.contains("|Category|") {
        parse_markdown(&clean)
    } else {
        parse_lines(&clean)
    }?;
    (!report.rows.is_empty()).then_some(report)
}

fn row(label: &str, tokens: u64) -> BreakdownRow {
    let label = label.trim();
    let id = category_id(label);
    BreakdownRow {
        kind: kind_of(&id, label),
        id,
        label: label.chars().take(64).collect(),
        tokens,
    }
}

/// The sum of what fills the context now: `used` rows (deferred tools are not loaded).
fn used_sum(rows: &[BreakdownRow]) -> u64 {
    rows.iter()
        .filter(|row| row.kind == BreakdownKind::Used)
        .map(|row| row.tokens)
        .sum()
}

fn parse_lines(text: &str) -> Option<ContextReport> {
    let mut window = None;
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Context window:") {
            window = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.replace(',', "").parse::<u64>().ok());
            continue;
        }
        let (Some(open), Some(close)) = (line.find('['), line.rfind(']')) else {
            continue;
        };
        if close < open {
            continue;
        }
        let label = &line[..open];
        let mut after = line[close + 1..].split_whitespace();
        let (Some(percent), Some(tokens), Some("tokens")) =
            (after.next(), after.next(), after.next())
        else {
            continue;
        };
        if !percent.ends_with('%') || label.trim().is_empty() {
            continue;
        }
        let Ok(tokens) = tokens.replace(',', "").parse::<u64>() else {
            continue;
        };
        if rows.len() < MAX_BREAKDOWN {
            rows.push(row(label, tokens));
        }
    }
    let used = (!rows.is_empty()).then_some(used_sum(&rows));
    Some(ContextReport {
        window,
        used,
        rows,
        exactness: Exactness::Reported,
    })
}

fn parse_markdown(text: &str) -> Option<ContextReport> {
    let mut window = None;
    let mut used = None;
    let mut rows = Vec::new();
    let mut in_table = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("**Tokens:**") {
            // `17k / 1m (2%)`
            let mut parts = rest.split('/');
            used = parts.next().and_then(tokens_of);
            window = parts
                .next()
                .and_then(|w| w.split('(').next())
                .and_then(tokens_of);
            continue;
        }
        if !line.starts_with('|') {
            in_table = false;
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 2 {
            continue;
        }
        if cells[0].eq_ignore_ascii_case("category") {
            in_table = true;
            continue;
        }
        if !in_table || cells[0].chars().all(|c| matches!(c, '-' | ':' | ' ')) {
            continue;
        }
        let Some(tokens) = tokens_of(cells[1]) else {
            continue;
        };
        if rows.len() < MAX_BREAKDOWN {
            rows.push(row(cells[0], tokens));
        }
    }
    let used = used.or_else(|| (!rows.is_empty()).then_some(used_sum(&rows)));
    Some(ContextReport {
        window,
        used,
        rows,
        exactness: Exactness::Estimated,
    })
}

/// `1.9k`, `2k`, `1m`, `950`, `1,234` as a number of tokens.
fn tokens_of(text: &str) -> Option<u64> {
    let text = text.trim().replace(',', "");
    let (number, scale) = match text.chars().last()?.to_ascii_lowercase() {
        'k' => (&text[..text.len() - 1], 1_000.0),
        'm' => (&text[..text.len() - 1], 1_000_000.0),
        _ => (text.as_str(), 1.0),
    };
    let value: f64 = number.trim().parse().ok()?;
    (value.is_finite() && value >= 0.0).then(|| (value * scale).round() as u64)
}

/// `text` without ANSI escape sequences (colours the harness puts in a bar).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answer of OMP 18.12.3 to `/context` in the recorded exchange
    /// (`docs/spikes/2026-10-28-insight/omp-acp-wire.jsonl`).
    const OMP: &str = "Context window: 200000 tokens (6% used)\n  System prompt    [░░░░░░░░░░░░░░░░░░░░░░░░] 1%  2559 tokens\n  System tools     [░░░░░░░░░░░░░░░░░░░░░░░░] 2%  3976 tokens\n  System context   [░░░░░░░░░░░░░░░░░░░░░░░░] 0%  113 tokens\n  Messages         [█░░░░░░░░░░░░░░░░░░░░░░░] 2%  4552 tokens\n  Auto-compact buf [████░░░░░░░░░░░░░░░░░░░░] 15%  30000 tokens\n  Free             [███████████████████░░░░░] 79%  158800 tokens";

    /// Claude Code 2.1.312's `/context` (`claude-context-usage.json`, `markdown`).
    const CLAUDE: &str = "## Context Usage\n\n**Model:** claude-opus-5-5  \n**Tokens:** 17k / 1m (2%)\n\n### Estimated usage by category\n\n| Category | Tokens | Percentage |\n|----------|--------|------------|\n| System prompt | 1.9k | 0.2% |\n| System tools | 13.1k | 1.3% |\n| System tools (deferred) | 19.3k | 1.9% |\n| Skills | 2k | 0.2% |\n| Free space | 950k | 95.0% |\n| Autocompact buffer | 33k | 3.3% |";

    fn rows(report: &ContextReport) -> Vec<(&str, u64, BreakdownKind)> {
        report
            .rows
            .iter()
            .map(|r| (r.id.as_str(), r.tokens, r.kind))
            .collect()
    }

    #[test]
    fn omp_reports_exact_categories_that_add_up_to_the_context() {
        let report = parse(OMP).unwrap_or_else(|| unreachable!("a report"));
        assert_eq!(report.window, Some(200_000));
        assert_eq!(report.exactness, Exactness::Reported);
        assert_eq!(
            rows(&report),
            [
                ("system_prompt", 2_559, BreakdownKind::Used),
                ("tools", 3_976, BreakdownKind::Used),
                ("project_context", 113, BreakdownKind::Used),
                ("messages", 4_552, BreakdownKind::Used),
                ("autocompact_buffer", 30_000, BreakdownKind::Buffer),
                ("free", 158_800, BreakdownKind::Free),
            ]
        );
        // 2 559 + 3 976 + 113 + 4 552, the `used` of the harness's own `usage_update`.
        assert_eq!(report.used, Some(11_200));
    }

    #[test]
    fn omp_colours_in_the_bars_do_not_matter() {
        let coloured = OMP.replace(
            "System prompt    [",
            "\u{1b}[32mSystem prompt\u{1b}[0m    [",
        );
        assert_eq!(parse(&coloured), parse(OMP));
    }

    #[test]
    fn claude_reports_rounded_categories_as_estimates() {
        let report = parse(CLAUDE).unwrap_or_else(|| unreachable!("a report"));
        assert_eq!(
            (report.used, report.window, report.exactness),
            (Some(17_000), Some(1_000_000), Exactness::Estimated)
        );
        assert_eq!(
            rows(&report),
            [
                ("system_prompt", 1_900, BreakdownKind::Used),
                ("tools", 13_100, BreakdownKind::Used),
                ("tools_deferred", 19_300, BreakdownKind::Deferred),
                ("skills", 2_000, BreakdownKind::Used),
                ("free", 950_000, BreakdownKind::Free),
                ("autocompact_buffer", 33_000, BreakdownKind::Buffer),
            ]
        );
    }

    #[test]
    fn an_unknown_category_keeps_its_label_and_a_slug_for_an_id() {
        let text = "| Category | Tokens | Percentage |\n|---|---|---|\n| MCP tools | 1.2k | 0.1% |\n| Plugin: Foo Bar | 300 | 0% |";
        let report = parse(text).unwrap_or_else(|| unreachable!("a report"));
        assert_eq!(report.rows[0].id, "mcp");
        assert_eq!(report.rows[1].id, "plugin_foo_bar");
        assert_eq!(report.rows[1].label, "Plugin: Foo Bar");
    }

    #[test]
    fn text_that_is_not_a_report_is_not_read() {
        assert_eq!(parse("no model is selected"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("| a | b |\n| c | d |"), None);
    }

    #[test]
    fn token_figures_with_a_suffix_are_read() {
        assert_eq!(tokens_of("1.9k"), Some(1_900));
        assert_eq!(tokens_of("1m"), Some(1_000_000));
        assert_eq!(tokens_of("1,234"), Some(1_234));
        assert_eq!(tokens_of("nope"), None);
    }
}
