//! Usage and limits (protocol 0.45, capability `usage`): DTOs of `usage.summary` and the
//! `usage.changed` event (`docs/protocol.md` § Usage, `docs/agents.md` § Usage).
//!
//! What the daemon says here is what harnesses themselves report (their rate-limit windows) and
//! what the turns Hutan ran used (tokens, and a cost where the harness reports one). Hutan never
//! reads, stores or forwards a harness's credentials (ADR 0015): a limit is a label, a
//! percentage and a reset time, nothing that identifies an account.

use serde::{Deserialize, Serialize};

/// The `harness` of turns of a direct chat (`chat.direct`): no harness, a provider the user
/// configured in Hutan.
pub const HARNESS_DIRECT: &str = "direct";

/// `usage.summary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UsageSummaryRequest {
    /// Ask the harnesses what is left before answering (`omp usage`, and Codex's own
    /// `account/rateLimits/read` for a Codex that has been used through Hutan): they answer
    /// over the network and take a few seconds. Absent or false answers from what the daemon
    /// already knows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh: bool,
}

/// How much has been used and what is left.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSummary {
    /// When the daemon made it.
    pub generated_at_ms: u64,
    /// Start of today in the daemon's local time zone (midnight): `today` counts from here.
    pub day_start_ms: u64,
    /// Start of this week (Monday midnight, local): `week` counts from here.
    pub week_start_ms: u64,
    /// Per harness (`omp`, `claude`, `codex`, `direct`) that has reported a limit or run a turn
    /// in the last week, in that order.
    pub harnesses: Vec<HarnessUsage>,
    /// The same turns by model provider (`anthropic`, `openai`, `ollama`, ...), most used first.
    pub providers: Vec<ProviderUsage>,
}

/// One harness: the limits it reports and the turns Hutan ran with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessUsage {
    pub harness: String,
    /// The harness's own limit windows; empty while it has reported none (or only windows that
    /// have reset since).
    #[serde(default)]
    pub limits: Vec<UsageLimit>,
    pub today: UsageTotals,
    pub week: UsageTotals,
}

/// The turns Hutan ran against one model provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: String,
    pub today: UsageTotals,
    pub week: UsageTotals,
}

/// One limit window a harness reported (the 5-hour or the weekly window of a plan).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageLimit {
    /// Protocol 0.50 (capability `accounts.multi`): the managed account the window belongs to
    /// (its name); absent: the harness's own login. Two accounts of one harness have a window
    /// each.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The account's label, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
    /// The provider the limit is for (`anthropic`, `openai`).
    pub provider: String,
    /// `5h`, `7d`, `30m`, or `primary`/`secondary` when the harness gave no duration.
    pub id: String,
    /// What to call it: `5-hour`, `weekly`.
    pub label: String,
    /// 0 to 100 (or more while over the limit): how much of the window is used.
    pub used_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_minutes: Option<u64>,
    /// When the window resets, when the harness says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at_ms: Option<u64>,
    /// The subscription plan, when the harness reports one (`plus`, `max`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// When the harness last said so.
    pub observed_at_ms: u64,
}

/// What a set of turns used. Token counts are what the harness or provider counted (a counter
/// it never reported adds nothing); a cost is only there when the harness reports one, and then
/// it is the harness's own estimate at list prices.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UsageTotals {
    pub turns: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
    /// The harness-reported cost in US dollars; absent when no turn reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// `cost_usd` is an estimate (a harness's price table, not a bill). Always true while a
    /// cost is present; named so that clients label it without knowing which harness it is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cost_is_estimate: bool,
}
