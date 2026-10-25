//! What the window says about usage and limits (USAGE-LIMITS, protocol 0.45): the words of the
//! `usage` status item, of the Agents page's Usage section and of the 80 % / 95 % warnings, and
//! the rule that gives each warning once per limit window. Pure data, no GPUI: every function
//! takes the time it is asked at.
//!
//! The daemon reports what harnesses say about their plans and what the turns Hutan ran used
//! (`docs/agents.md` § Usage); this only words it. A cost is always an estimate and says so.

use std::collections::HashMap;

use hutan_protocol::usage::{HARNESS_DIRECT, HarnessUsage, UsageLimit, UsageSummary, UsageTotals};

/// The levels a warning is given at, in percent of a limit window.
pub const WARN_LEVELS: [u8; 2] = [80, 95];

/// How a harness is called.
pub fn harness_label(harness: &str) -> String {
    match harness {
        "omp" => "OMP".to_owned(),
        "claude" => "Claude Code".to_owned(),
        "codex" => "Codex".to_owned(),
        HARNESS_DIRECT => "Direct chat".to_owned(),
        other => other.to_owned(),
    }
}

/// `42m`, `3h`, `3h 10m`, `13h`, `2d`, `2d 4h`: how long until `resets_at_ms`, as the status item
/// and the page say it. A time that has come is `now`.
pub fn reset_in(resets_at_ms: u64, now_ms: u64) -> String {
    let minutes = resets_at_ms.saturating_sub(now_ms).div_ceil(60_000);
    match minutes {
        0 => "now".to_owned(),
        1..=59 => format!("{minutes}m"),
        60..=599 => match minutes % 60 {
            0 => format!("{}h", minutes / 60),
            rest => format!("{}h {rest}m", minutes / 60),
        },
        600..=1439 => format!("{}h", minutes / 60),
        _ => match (minutes % 1440) / 60 {
            0 => format!("{}d", minutes / 1440),
            hours => format!("{}d {hours}h", minutes / 1440),
        },
    }
}

/// `72%`: whole percent, the provider's own figure.
pub fn percent(used: f64) -> String {
    format!("{}%", used.round() as i64)
}

/// How worried the colour should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Calm,
    /// 80 % or more.
    Warning,
    /// 95 % or more.
    Critical,
}

pub fn severity(used_percent: f64) -> Severity {
    if used_percent >= f64::from(WARN_LEVELS[1]) {
        Severity::Critical
    } else if used_percent >= f64::from(WARN_LEVELS[0]) {
        Severity::Warning
    } else {
        Severity::Calm
    }
}

/// A limit window that is still running at `now_ms`.
fn current(limit: &UsageLimit, now_ms: u64) -> bool {
    limit.resets_at_ms.is_none_or(|resets| resets > now_ms)
}

/// The tightest limit: the most used window of any harness that is still running.
pub fn tightest(summary: &UsageSummary, now_ms: u64) -> Option<(&HarnessUsage, &UsageLimit)> {
    summary
        .harnesses
        .iter()
        .flat_map(|h| h.limits.iter().map(move |l| (h, l)))
        .filter(|(_, l)| current(l, now_ms))
        .max_by(|(_, a), (_, b)| a.used_percent.total_cmp(&b.used_percent))
}

/// What the `usage` status item shows.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusText {
    /// `Codex 72% · resets 3h`.
    pub text: String,
    /// Every limit the daemon knows, one per line, and what was used today.
    pub tooltip: String,
    pub severity: Severity,
}

/// The status item: the tightest limit, or nothing while no harness has reported one.
pub fn status_text(summary: &UsageSummary, now_ms: u64) -> Option<StatusText> {
    let (harness, limit) = tightest(summary, now_ms)?;
    let mut text = format!(
        "{} {}",
        harness_label(&harness.harness),
        percent(limit.used_percent)
    );
    if let Some(resets) = limit.resets_at_ms {
        text.push_str(&format!(" · resets {}", reset_in(resets, now_ms)));
    }
    let mut lines = Vec::new();
    for h in &summary.harnesses {
        for l in h.limits.iter().filter(|l| current(l, now_ms)) {
            lines.push(limit_line(h, l, now_ms));
        }
    }
    for h in &summary.harnesses {
        if h.today.turns > 0 {
            lines.push(format!(
                "{} today: {}",
                harness_label(&h.harness),
                totals_text(&h.today)
            ));
        }
    }
    Some(StatusText {
        text,
        tooltip: lines.join("\n"),
        severity: severity(limit.used_percent),
    })
}

/// `Codex 5-hour 72% · resets in 3h` (a provider other than the harness's own is named, since
/// OMP has one login per provider).
pub fn limit_line(harness: &HarnessUsage, limit: &UsageLimit, now_ms: u64) -> String {
    let mut line = format!("{} {}", harness_label(&harness.harness), limit.label);
    if harness.harness == "omp" {
        line = format!(
            "{} ({}) {}",
            harness_label(&harness.harness),
            limit.provider,
            limit.label
        );
    }
    line.push_str(&format!(" {}", percent(limit.used_percent)));
    if let Some(account) = account_name(limit) {
        line.push_str(&format!(" · {account}"));
    }
    if let Some(resets) = limit.resets_at_ms {
        line.push_str(&format!(" · resets in {}", reset_in(resets, now_ms)));
    }
    line
}

/// The account a window belongs to, as it is named next to it (`Work`); none for the
/// harness's own login (protocol 0.50).
pub fn account_name(limit: &UsageLimit) -> Option<String> {
    limit
        .account_label
        .clone()
        .or_else(|| limit.account.clone())
}

/// `22.2k tokens · ≈ $0.42 (estimate)`; `no turns` for none. A cost is the harness's own price
/// table, never a bill, and is labelled so wherever it shows.
pub fn totals_text(totals: &UsageTotals) -> String {
    if totals.turns == 0 {
        return "no turns".to_owned();
    }
    let mut text = format!(
        "{} tokens",
        crate::workbench::chat::widgets::compact_tokens(totals.total_tokens)
    );
    if let Some(cost) = totals.cost_usd {
        text.push_str(&format!(" · ≈ {}", money(cost)));
        if totals.cost_is_estimate {
            text.push_str(" (estimate)");
        }
    }
    text
}

fn money(cost: f64) -> String {
    if cost < 0.01 && cost > 0.0 {
        "<$0.01".to_owned()
    } else if cost < 100.0 {
        format!("${cost:.2}")
    } else {
        format!("${cost:.0}")
    }
}

// The Agents page's section ------------------------------------------------------------------

/// One limit window as the Usage section draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitView {
    /// `5-hour`, `weekly`.
    pub label: String,
    /// The provider the window is with.
    pub provider: String,
    /// The managed account the window is of (`Work`); none for the harness's own login.
    pub account: Option<String>,
    /// `72%`.
    pub percent: String,
    /// How full the bar is, 0 to 1.
    pub fraction: f32,
    /// `resets in 3h`, when the harness says when.
    pub resets: Option<String>,
    pub severity: Severity,
}

/// One harness of the Usage section.
#[derive(Debug, Clone, PartialEq)]
pub struct HarnessView {
    pub harness: String,
    pub name: String,
    pub limits: Vec<LimitView>,
    /// `Today: 22k tokens · ≈ $0.42 (estimate)`.
    pub today: String,
    /// `This week: 120k tokens`.
    pub week: String,
    /// Why there is no limit to show (`reported after its first reply`).
    pub note: Option<String>,
}

/// The rows of the Usage section: per harness its running limit windows and what its turns used
/// today and this week. A harness the daemon has not heard of is not a row.
pub fn section(summary: &UsageSummary, now_ms: u64) -> Vec<HarnessView> {
    summary
        .harnesses
        .iter()
        .map(|harness| {
            let limits: Vec<LimitView> = harness
                .limits
                .iter()
                .filter(|l| current(l, now_ms))
                .map(|l| LimitView {
                    label: l.label.clone(),
                    provider: l.provider.clone(),
                    account: account_name(l),
                    percent: percent(l.used_percent),
                    fraction: (l.used_percent / 100.0).clamp(0.0, 1.0) as f32,
                    resets: l
                        .resets_at_ms
                        .map(|resets| format!("resets in {}", reset_in(resets, now_ms))),
                    severity: severity(l.used_percent),
                })
                .collect();
            let note = limits.is_empty().then_some(match harness.harness.as_str() {
                HARNESS_DIRECT => None,
                "codex" => Some("no limit reported yet: Codex reports it with its replies"),
                "claude" => {
                    Some("no limit reported yet: Claude Code reports it with its first reply")
                }
                _ => Some("no limit reported yet"),
            });
            HarnessView {
                harness: harness.harness.clone(),
                name: harness_label(&harness.harness),
                limits,
                today: format!("Today: {}", totals_text(&harness.today)),
                week: format!("This week: {}", totals_text(&harness.week)),
                note: note.flatten().map(str::to_owned),
            }
        })
        .collect()
}

// Warnings ------------------------------------------------------------------------------------

/// A limit window that crossed 80 % or 95 %.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageWarning {
    pub harness: String,
    /// The managed account (its name), none for the harness's own login.
    pub account: Option<String>,
    /// How the account reads next to the harness (`Work`).
    pub account_name: Option<String>,
    pub provider: String,
    pub window_id: String,
    pub label: String,
    /// 80 or 95.
    pub level: u8,
    pub used_percent: f64,
    pub resets_at_ms: Option<u64>,
}

impl UsageWarning {
    /// `Codex is at 80% of its 5-hour limit · resets in 3h`.
    pub fn text(&self, now_ms: u64) -> String {
        let who = match &self.account_name {
            Some(account) => format!("{} ({account})", harness_label(&self.harness)),
            None => harness_label(&self.harness),
        };
        let mut text = format!(
            "{who} is at {} of its {} limit",
            percent(self.used_percent),
            self.label
        );
        if let Some(resets) = self.resets_at_ms {
            text.push_str(&format!(" · resets in {}", reset_in(resets, now_ms)));
        }
        text
    }
}

type WindowKey = (String, String, String, String, u64);

/// Gives each warning once per limit window.
///
/// A window is a harness, an account, a provider, a window id and the reset time it runs to: when the reset
/// comes the daemon reports the next one, which is a window of its own. The first summary a
/// window sees only seeds what is known (a limit that is at 85 % when the app opens is not news:
/// the status item shows it). After that a window warns when it reaches 80 %, and again when it
/// reaches 95 %, never twice for one level; usage that falls back below a level (a window
/// that reports no reset time and starts over) is forgotten so that the level can warn again.
#[derive(Debug, Default)]
pub struct UsageWarnings {
    levels: HashMap<WindowKey, u8>,
    seeded: bool,
}

fn level_of(used: f64) -> u8 {
    WARN_LEVELS
        .iter()
        .rev()
        .copied()
        .find(|level| used >= f64::from(*level))
        .unwrap_or(0)
}

impl UsageWarnings {
    /// Folds in a summary; the warnings it earns, most used first.
    pub fn observe(&mut self, summary: &UsageSummary, now_ms: u64) -> Vec<UsageWarning> {
        let seeding = !self.seeded;
        self.seeded = true;
        let mut warnings = Vec::new();
        let mut live = Vec::new();
        for harness in &summary.harnesses {
            for limit in harness.limits.iter().filter(|l| current(l, now_ms)) {
                let key: WindowKey = (
                    harness.harness.clone(),
                    limit.account.clone().unwrap_or_default(),
                    limit.provider.clone(),
                    limit.id.clone(),
                    limit.resets_at_ms.unwrap_or(0),
                );
                let level = level_of(limit.used_percent);
                let known = self.levels.get(&key).copied().unwrap_or(0);
                if level > known && !seeding {
                    warnings.push(UsageWarning {
                        harness: harness.harness.clone(),
                        account: limit.account.clone(),
                        account_name: account_name(limit),
                        provider: limit.provider.clone(),
                        window_id: limit.id.clone(),
                        label: limit.label.clone(),
                        level,
                        used_percent: limit.used_percent,
                        resets_at_ms: limit.resets_at_ms,
                    });
                }
                if level != known || seeding {
                    self.levels.insert(key.clone(), level);
                }
                live.push(key);
            }
        }
        // A window that is gone (it reset) is not remembered.
        self.levels.retain(|key, _| live.contains(key));
        warnings.sort_by(|a, b| b.used_percent.total_cmp(&a.used_percent));
        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000_000_000;
    const HOUR: u64 = 3_600_000;

    fn limit(provider: &str, id: &str, used: f64, resets: Option<u64>) -> UsageLimit {
        UsageLimit {
            account: None,
            account_label: None,
            provider: provider.into(),
            id: id.into(),
            label: if id == "5h" { "5-hour" } else { "weekly" }.into(),
            used_percent: used,
            window_minutes: Some(if id == "5h" { 300 } else { 10_080 }),
            resets_at_ms: resets,
            plan: None,
            observed_at_ms: NOW,
        }
    }

    fn harness(name: &str, limits: Vec<UsageLimit>) -> HarnessUsage {
        HarnessUsage {
            harness: name.into(),
            limits,
            today: UsageTotals::default(),
            week: UsageTotals::default(),
        }
    }

    fn summary(harnesses: Vec<HarnessUsage>) -> UsageSummary {
        UsageSummary {
            generated_at_ms: NOW,
            day_start_ms: NOW - HOUR,
            week_start_ms: NOW - 24 * HOUR,
            harnesses,
            providers: vec![],
        }
    }

    fn of_account(mut limit: UsageLimit, name: &str, label: &str) -> UsageLimit {
        limit.account = Some(name.into());
        limit.account_label = Some(label.into());
        limit
    }

    #[test]
    fn two_accounts_of_one_harness_each_have_their_window_and_warn_on_their_own() {
        let own = limit("anthropic", "5h", 10.0, Some(NOW + HOUR));
        let work = of_account(
            limit("anthropic", "5h", 82.0, Some(NOW + HOUR)),
            "work",
            "Work",
        );
        let usage = summary(vec![harness("claude", vec![own.clone(), work.clone()])]);
        // The Usage section says whose each bar is; the harness's own login says nothing.
        let rows = section(&usage, NOW);
        let accounts: Vec<_> = rows[0].limits.iter().map(|l| l.account.clone()).collect();
        assert_eq!(accounts, [None, Some("Work".to_owned())]);
        // The tooltip and the status item name the tightest window's account.
        let text = status_text(&usage, NOW).unwrap();
        assert!(
            text.tooltip.contains("Claude Code 5-hour 82% · Work"),
            "{}",
            text.tooltip
        );
        // Seeded at 10 % and 20 %, the second account crossing 80 % warns by itself.
        let mut warnings = UsageWarnings::default();
        let calm = summary(vec![harness(
            "claude",
            vec![
                own.clone(),
                of_account(
                    limit("anthropic", "5h", 20.0, Some(NOW + HOUR)),
                    "work",
                    "Work",
                ),
            ],
        )]);
        assert!(warnings.observe(&calm, NOW).is_empty());
        let news = warnings.observe(&usage, NOW);
        assert_eq!(news.len(), 1);
        assert_eq!(news[0].account.as_deref(), Some("work"));
        assert_eq!(
            news[0].text(NOW),
            "Claude Code (Work) is at 82% of its 5-hour limit · resets in 1h"
        );
        assert!(warnings.observe(&usage, NOW).is_empty(), "once per window");
    }

    #[test]
    fn reset_times_are_said_in_the_two_units_that_matter() {
        assert_eq!(reset_in(NOW, NOW), "now");
        assert_eq!(reset_in(NOW - 5, NOW), "now");
        assert_eq!(reset_in(NOW + 1, NOW), "1m");
        assert_eq!(reset_in(NOW + 42 * 60_000, NOW), "42m");
        assert_eq!(reset_in(NOW + 3 * HOUR, NOW), "3h");
        assert_eq!(reset_in(NOW + 3 * HOUR + 10 * 60_000, NOW), "3h 10m");
        assert_eq!(reset_in(NOW + 13 * HOUR + 59 * 60_000, NOW), "13h");
        assert_eq!(reset_in(NOW + 48 * HOUR, NOW), "2d");
        assert_eq!(reset_in(NOW + 52 * HOUR, NOW), "2d 4h");
    }

    #[test]
    fn the_status_item_shows_the_tightest_limit_that_is_still_running() {
        let usage = summary(vec![
            harness(
                "codex",
                vec![
                    limit("openai", "5h", 72.0, Some(NOW + 3 * HOUR)),
                    limit("openai", "7d", 31.0, None),
                ],
            ),
            harness("omp", vec![limit("anthropic", "5h", 95.0, Some(NOW - 1))]),
        ]);
        let text = status_text(&usage, NOW).unwrap();
        assert_eq!(
            text.text, "Codex 72% · resets 3h",
            "the over window does not count"
        );
        assert_eq!(text.severity, Severity::Calm);
        assert!(text.tooltip.contains("Codex 5-hour 72% · resets in 3h"));
        assert!(text.tooltip.contains("Codex weekly 31%"));
        assert!(!text.tooltip.contains("OMP"), "{}", text.tooltip);
        // Nothing reported: nothing shown.
        assert_eq!(
            status_text(&summary(vec![harness("codex", vec![])]), NOW),
            None
        );
        // 85 % and 96 % are words of their own.
        let hot = summary(vec![harness(
            "omp",
            vec![limit("anthropic", "7d", 96.4, None)],
        )]);
        let text = status_text(&hot, NOW).unwrap();
        assert_eq!(
            (text.text.as_str(), text.severity),
            ("OMP 96%", Severity::Critical)
        );
        assert_eq!(severity(80.0), Severity::Warning);
        assert_eq!(severity(79.9), Severity::Calm);
    }

    #[test]
    fn the_section_has_a_row_per_harness_with_bars_resets_and_estimate_labels() {
        let mut omp = harness(
            "omp",
            vec![limit("anthropic", "5h", 7.0, Some(NOW + 3 * HOUR))],
        );
        omp.today = UsageTotals {
            turns: 1,
            total_tokens: 1_234,
            cost_usd: Some(0.0042),
            cost_is_estimate: true,
            ..UsageTotals::default()
        };
        let usage = summary(vec![
            omp,
            harness("codex", vec![]),
            harness("direct", vec![]),
        ]);
        let rows = section(&usage, NOW);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "OMP");
        assert_eq!(
            rows[0].limits,
            [LimitView {
                label: "5-hour".into(),
                provider: "anthropic".into(),
                account: None,
                percent: "7%".into(),
                fraction: 0.07,
                resets: Some("resets in 3h".into()),
                severity: Severity::Calm,
            }]
        );
        assert_eq!(rows[0].today, "Today: 1.2k tokens · ≈ <$0.01 (estimate)");
        assert_eq!(rows[0].week, "This week: no turns");
        assert_eq!(rows[0].note, None);
        assert!(rows[1].note.as_deref().unwrap().contains("Codex reports"));
        assert_eq!(rows[2].note, None, "a direct chat has no plan to report");
    }

    #[test]
    fn a_cost_is_labelled_as_an_estimate() {
        let totals = UsageTotals {
            turns: 2,
            total_tokens: 22_178,
            cost_usd: Some(0.4213),
            cost_is_estimate: true,
            ..UsageTotals::default()
        };
        assert_eq!(totals_text(&totals), "22k tokens · ≈ $0.42 (estimate)");
        let no_cost = UsageTotals {
            cost_usd: None,
            cost_is_estimate: false,
            ..totals.clone()
        };
        assert_eq!(totals_text(&no_cost), "22k tokens");
        assert_eq!(totals_text(&UsageTotals::default()), "no turns");
        let tiny = UsageTotals {
            cost_usd: Some(0.001),
            ..totals
        };
        assert!(totals_text(&tiny).contains("≈ <$0.01 (estimate)"));
    }

    #[test]
    fn a_window_warns_at_80_and_at_95_once_each_and_never_for_what_was_there_when_the_app_opened() {
        let reset = Some(NOW + 3 * HOUR);
        let at = |used: f64, resets: Option<u64>| {
            summary(vec![harness(
                "codex",
                vec![limit("openai", "5h", used, resets)],
            )])
        };
        let mut warnings = UsageWarnings::default();
        // The first summary seeds: already at 85 %, no toast.
        assert!(warnings.observe(&at(85.0, reset), NOW).is_empty());
        assert!(
            warnings.observe(&at(90.0, reset), NOW).is_empty(),
            "80 was seen"
        );
        let crossed = warnings.observe(&at(96.0, reset), NOW);
        assert_eq!(crossed.len(), 1);
        assert_eq!(crossed[0].level, 95);
        assert_eq!(
            crossed[0].text(NOW),
            "Codex is at 96% of its 5-hour limit · resets in 3h"
        );
        assert!(
            warnings.observe(&at(97.0, reset), NOW).is_empty(),
            "once per level"
        );
        // The window resets: the next one is a window of its own and warns again.
        let next = Some(NOW + 9 * HOUR);
        assert!(warnings.observe(&at(10.0, next), NOW + 4 * HOUR).is_empty());
        let again = warnings.observe(&at(81.0, next), NOW + 5 * HOUR);
        assert_eq!(again.iter().map(|w| w.level).collect::<Vec<_>>(), [80]);
    }

    #[test]
    fn jumping_over_80_warns_at_the_level_reached_and_a_window_without_a_reset_time_starts_over() {
        let none = None;
        let at = |used: f64| {
            summary(vec![harness(
                "omp",
                vec![limit("anthropic", "7d", used, none)],
            )])
        };
        let mut warnings = UsageWarnings::default();
        assert!(warnings.observe(&at(5.0), NOW).is_empty());
        let jump = warnings.observe(&at(96.0), NOW);
        assert_eq!(
            jump.iter().map(|w| w.level).collect::<Vec<_>>(),
            [95],
            "one toast, not two"
        );
        // It falls back (the window began again) and rises again.
        assert!(warnings.observe(&at(3.0), NOW).is_empty());
        assert_eq!(warnings.observe(&at(82.0), NOW).len(), 1);
    }

    #[test]
    fn warnings_come_most_used_first_and_name_each_harness() {
        let reset = Some(NOW + HOUR);
        let mut warnings = UsageWarnings::default();
        let calm = summary(vec![
            harness("codex", vec![limit("openai", "5h", 10.0, reset)]),
            harness("omp", vec![limit("anthropic", "5h", 10.0, reset)]),
        ]);
        assert!(warnings.observe(&calm, NOW).is_empty());
        let hot = summary(vec![
            harness("codex", vec![limit("openai", "5h", 81.0, reset)]),
            harness("omp", vec![limit("anthropic", "5h", 97.0, reset)]),
        ]);
        let crossed = warnings.observe(&hot, NOW);
        let who: Vec<_> = crossed
            .iter()
            .map(|w| (w.harness.as_str(), w.level))
            .collect();
        assert_eq!(who, [("omp", 95), ("codex", 80)]);
    }
}
