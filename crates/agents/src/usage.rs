//! What a harness says about its account limits (`docs/agents.md` § Usage): the rate-limit
//! windows of a plan, read from the messages the harness sends anyway.
//!
//! Only numbers and a few short identifiers are kept: a percentage, a window length, a reset
//! time, a plan name. Nothing here reads, keeps or forwards a credential, an account id or an
//! e-mail address (ADR 0015): the payloads carry none of those in the fields read, and every
//! string that is kept passes [`identifier`].

use serde_json::Value;

/// The limit windows of one provider's plan, as a harness reported them.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimits {
    /// The model provider the plan is with (`openai`, `anthropic`).
    pub provider: String,
    /// The subscription plan (`plus`, `pro`, `max`) when the harness names it.
    pub plan: Option<String>,
    /// The windows this report carries. A report may carry only some of a plan's windows; the
    /// ones it leaves out keep what was last known.
    pub windows: Vec<RateWindow>,
}

/// One window of a plan.
#[derive(Debug, Clone, PartialEq)]
pub struct RateWindow {
    /// `5h`, `7d`, `<n>m`/`<n>h`/`<n>d`, or `primary`/`secondary` when the harness gave no
    /// length.
    pub id: String,
    /// `5-hour`, `weekly`, ...
    pub label: String,
    /// How much of the window is used: 0 to 100, more while over the limit.
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    /// When the window resets, in milliseconds since the epoch.
    pub resets_at_ms: Option<u64>,
}

impl RateWindow {
    /// A window of `minutes` (or of the harness's own name `fallback` when it gave no length).
    /// A percentage that is not a finite number is refused.
    pub fn new(
        used_percent: f64,
        minutes: Option<u64>,
        resets_at_ms: Option<u64>,
        fallback: &str,
    ) -> Option<Self> {
        if !used_percent.is_finite() {
            return None;
        }
        let (id, label) = match minutes.filter(|m| *m > 0) {
            Some(minutes) => window_names(minutes),
            None => (fallback.to_owned(), fallback.to_owned()),
        };
        Some(Self {
            id,
            label,
            used_percent: used_percent.max(0.0),
            window_minutes: minutes.filter(|m| *m > 0),
            resets_at_ms,
        })
    }
}

/// The id and the label of a window `minutes` long.
pub fn window_names(minutes: u64) -> (String, String) {
    const DAY: u64 = 24 * 60;
    match minutes {
        300 => ("5h".to_owned(), "5-hour".to_owned()),
        DAY => ("1d".to_owned(), "daily".to_owned()),
        10_080 => ("7d".to_owned(), "weekly".to_owned()),
        m if m % DAY == 0 => (format!("{}d", m / DAY), format!("{}-day", m / DAY)),
        m if m % 60 == 0 => (format!("{}h", m / 60), format!("{}-hour", m / 60)),
        m => (format!("{m}m"), format!("{m}-minute")),
    }
}

/// `value` when it is a short identifier (letters, digits, `.`, `_`, `-`, at most 40 of them):
/// the only kind of string the usage records keep, so a payload that carried something else
/// there (an address, a token) is dropped instead of stored.
pub fn identifier(value: &str) -> Option<String> {
    let value = value.trim();
    let fine = !value.is_empty()
        && value.len() <= 40
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    fine.then(|| value.to_ascii_lowercase())
}

fn seconds_to_ms(seconds: &Value) -> Option<u64> {
    let seconds = seconds.as_f64()?;
    (seconds.is_finite() && seconds >= 0.0).then_some((seconds * 1000.0) as u64)
}

/// The rate limits in an `account/rateLimits/updated` notification or an
/// `account/rateLimits/read` answer of Codex's app-server (`params` / `result`).
///
/// `rateLimits` is the single-bucket view; `rateLimitsByLimitId.codex`, when there is one, is
/// the same bucket from the multi-bucket view and wins. `primary` and `secondary` are windows
/// whose length says which they are (a Pro plan has been seen with only the weekly one, as
/// `primary`). `usedPercent` is a whole percent, `resetsAt` epoch seconds.
pub fn codex_rate_limits(payload: &Value) -> Option<RateLimits> {
    let snapshot = payload
        .pointer("/rateLimitsByLimitId/codex")
        .filter(|v| v.is_object())
        .or_else(|| payload.get("rateLimits").filter(|v| v.is_object()))?;
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let Some(window) = snapshot.get(key).filter(|w| w.is_object()) else {
            continue;
        };
        let Some(used) = window.get("usedPercent").and_then(Value::as_f64) else {
            continue;
        };
        let minutes = window.get("windowDurationMins").and_then(Value::as_u64);
        let resets = window.get("resetsAt").and_then(seconds_to_ms);
        if let Some(window) = RateWindow::new(used, minutes, resets, key) {
            windows.push(window);
        }
    }
    if windows.is_empty() {
        return None;
    }
    Some(RateLimits {
        provider: "openai".to_owned(),
        plan: snapshot
            .get("planType")
            .and_then(Value::as_str)
            .and_then(identifier)
            .filter(|plan| plan != "unknown"),
        windows,
    })
}

/// The rate limits Claude Code's ACP adapter forwards in a `usage_update`
/// (`_meta["_claude/rateLimit"]`, a `rate_limit_event` of the Agent SDK): `unifiedWindows`
/// `{five_hour, seven_day}` each with a `utilization` (a fraction) and `resetsAt` (epoch
/// seconds). Observed in the adapter 0.73 and not part of the SDK's declared types, so
/// everything is optional. An event that says `rejected` for a window without a utilization is
/// a full window.
pub fn claude_rate_limits(update: &Value) -> Option<RateLimits> {
    let info = update.pointer("/_meta/_claude~1rateLimit")?;
    let mut windows = Vec::new();
    if let Some(unified) = info.get("unifiedWindows").and_then(Value::as_object) {
        for (key, minutes) in [("five_hour", 300), ("seven_day", 10_080)] {
            let Some(window) = unified.get(key) else {
                continue;
            };
            let Some(utilization) = window.get("utilization").and_then(Value::as_f64) else {
                continue;
            };
            let resets = window.get("resetsAt").and_then(seconds_to_ms);
            if let Some(window) = RateWindow::new(utilization * 100.0, Some(minutes), resets, key) {
                windows.push(window);
            }
        }
    }
    if windows.is_empty()
        && info.get("status").and_then(Value::as_str) == Some("rejected")
        && let Some(minutes) = match info.get("rateLimitType").and_then(Value::as_str) {
            Some("five_hour") => Some(300),
            Some("seven_day") => Some(10_080),
            _ => None,
        }
    {
        let resets = info.get("resetsAt").and_then(seconds_to_ms);
        windows.extend(RateWindow::new(100.0, Some(minutes), resets, "limit"));
    }
    if windows.is_empty() {
        return None;
    }
    Some(RateLimits {
        provider: "anthropic".to_owned(),
        plan: None,
        windows,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn codex_windows_are_named_by_their_length() {
        let limits = codex_rate_limits(&json!({
            "rateLimits": {
                "limitId": "codex",
                "primary": {"usedPercent": 72, "windowDurationMins": 300, "resetsAt": 1_791_904_800},
                "secondary": {"usedPercent": 31, "windowDurationMins": 10_080},
                "planType": "plus"
            }
        }))
        .unwrap();
        assert_eq!(limits.provider, "openai");
        assert_eq!(limits.plan.as_deref(), Some("plus"));
        assert_eq!(limits.windows[0].id, "5h");
        assert_eq!(limits.windows[0].label, "5-hour");
        assert_eq!(limits.windows[0].resets_at_ms, Some(1_791_904_800_000));
        assert_eq!(limits.windows[1].id, "7d");
        assert_eq!(limits.windows[1].resets_at_ms, None);
    }

    #[test]
    fn a_pro_plan_with_only_the_weekly_window_is_weekly_not_primary() {
        let limits = codex_rate_limits(&json!({
            "rateLimits": {"primary": {"usedPercent": 10, "windowDurationMins": 10_080, "resetsAt": 5}, "secondary": null}
        }))
        .unwrap();
        assert_eq!(limits.windows.len(), 1);
        assert_eq!(limits.windows[0].id, "7d");
    }

    #[test]
    fn the_codex_bucket_of_the_multi_bucket_view_wins_and_odd_payloads_are_nothing() {
        let limits = codex_rate_limits(&json!({
            "rateLimits": {"primary": {"usedPercent": 1, "windowDurationMins": 300}},
            "rateLimitsByLimitId": {"codex": {"primary": {"usedPercent": 9, "windowDurationMins": 300}}}
        }))
        .unwrap();
        assert!((limits.windows[0].used_percent - 9.0).abs() < f64::EPSILON);
        assert!(codex_rate_limits(&json!({"rateLimits": {}})).is_none());
        assert!(
            codex_rate_limits(&json!({"rateLimits": {"primary": {"usedPercent": "x"}}})).is_none()
        );
        assert!(codex_rate_limits(&json!({})).is_none());
    }

    #[test]
    fn a_window_without_a_length_keeps_the_harness_name() {
        let limits = codex_rate_limits(&json!({
            "rateLimits": {"primary": {"usedPercent": 40}}
        }))
        .unwrap();
        assert_eq!(limits.windows[0].id, "primary");
        assert_eq!(limits.windows[0].window_minutes, None);
    }

    #[test]
    fn claude_utilization_is_a_fraction_and_a_rejection_is_a_full_window() {
        let update = json!({"sessionUpdate": "usage_update", "used": 1, "size": 2, "_meta": {
            "_claude/rateLimit": {
                "status": "allowed", "resetsAt": 1_788_465_600, "rateLimitType": "five_hour",
                "unifiedWindows": {
                    "five_hour": {"utilization": 0.24, "resetsAt": 1_788_465_600},
                    "seven_day": {"utilization": 0.13, "resetsAt": 1_789_005_600}
                }
            }
        }});
        let limits = claude_rate_limits(&update).unwrap();
        assert_eq!(limits.provider, "anthropic");
        assert!((limits.windows[0].used_percent - 24.0).abs() < 1e-9);
        assert_eq!(limits.windows[1].id, "7d");
        let rejected = json!({"_meta": {"_claude/rateLimit": {
            "status": "rejected", "resetsAt": 1_788_465_600, "rateLimitType": "five_hour"
        }}});
        let limits = claude_rate_limits(&rejected).unwrap();
        assert!((limits.windows[0].used_percent - 100.0).abs() < f64::EPSILON);
        assert!(claude_rate_limits(&json!({"used": 1})).is_none());
    }

    #[test]
    fn identifiers_refuse_what_is_not_one() {
        assert_eq!(identifier("Plus").as_deref(), Some("plus"));
        assert_eq!(identifier("me@example.com"), None);
        assert_eq!(identifier("sk-abc def"), None);
        assert_eq!(identifier(&"a".repeat(41)), None);
    }
}
