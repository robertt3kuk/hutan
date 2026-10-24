//! Usage and limits (protocol 0.45, capability `usage`; `docs/agents.md` § Usage).
//!
//! Two things are recorded and summed:
//!
//! - **what the turns Hutan ran used**: when a chat turn ends, [`TurnUsage`] has folded the
//!   `usage` events of the turn (and a cost the harness reported) into the turn's tokens and
//!   cost, and one row goes to `usage_turns`. Tokens are exact; a cost is the harness's own
//!   estimate.
//! - **the limit windows harnesses report** (Codex, Claude Code, OMP): the newest report of each
//!   window is kept in `usage_limits`. They arrive with a harness's own messages
//!   ([`UsageService::limits_reported`]) or when a client asks to refresh
//!   ([`UsageService::summary`]): `omp usage --json`, and for a Codex that has run through
//!   Hutan a short-lived `codex app-server` asked `account/rateLimits/read`.
//!
//! Nothing here reads, stores or forwards a credential (ADR 0015): the rows hold a harness id,
//! a provider id, a model id, a window id, a plan name and numbers, every string passed
//! through [`hutan_agents::usage::identifier`].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use hutan_agents::usage::{RateLimits, identifier, window_names};
use hutan_agents::{AgentError, CodexConfig};
use hutan_core::SessionId;
use hutan_core::agent::TokenCounts;
use hutan_protocol::account::{HARNESS_CLAUDE, HARNESS_CODEX, HARNESS_OMP};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::Event;
use hutan_protocol::usage::{
    HARNESS_DIRECT, HarnessUsage, ProviderUsage, UsageLimit, UsageSummary, UsageTotals,
};
use hutan_storage::{Storage, UsageLimitRow, UsageSum, UsageTurnRow};
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::bus::EventBus;
use crate::harness::Harnesses;
use crate::registry::now_ms;

/// Bursts of changes (a limit notification per request) become one `usage.changed`.
const COALESCE: Duration = Duration::from_millis(250);
/// How long Codex may take to answer `account/rateLimits/read`.
const CODEX_READ_TIMEOUT: Duration = Duration::from_secs(20);

const DAY_MS: u64 = 86_400_000;

// Turn accounting ---------------------------------------------------------------------------

/// Folds the `usage` events and the reported cost of a session's turns into what each turn used.
///
/// The harnesses report differently: Codex and the gateway give a running `total` for the
/// conversation and the `last` request; an ACP harness gives the turn's own counts in the
/// prompt's answer (`last` only), and its `total` is the context size, which is not spend. So a
/// turn's tokens are the growth of `total` over the turn, plus every `last` that came without
/// a `total`. Cost is the growth of the cumulative session cost.
#[derive(Debug, Default)]
pub struct TurnUsage {
    /// The latest cumulative total, kept across turns as the next turn's baseline.
    total: Option<TokenCounts>,
    /// What `total` was before the turn: known from an earlier turn, or derived from the first
    /// cumulative event of the turn (`total` minus `last`).
    baseline: Option<TokenCounts>,
    /// Counts a harness gave for the turn as such (`last` without `total`).
    direct: Option<TokenCounts>,
    /// The cumulative session cost last reported; `None` before one.
    cost: Option<f64>,
    /// The cost when the turn began; `None` when it is not known (a resumed session that
    /// reports its cost only at the end of the turn).
    cost_before: Option<f64>,
    /// The session began in this daemon's turns: its cost started at zero.
    fresh: bool,
}

impl TurnUsage {
    /// A session whose cost (when it reports one) starts at zero if it is `fresh`, and at an
    /// unknown amount if it continues an earlier conversation.
    pub fn new(fresh: bool) -> Self {
        Self {
            fresh,
            cost_before: fresh.then_some(0.0),
            ..Self::default()
        }
    }

    /// A turn began.
    pub fn turn_started(&mut self) {
        self.baseline = self.total;
        self.direct = None;
        self.cost_before = self.cost.or(self.fresh.then_some(0.0));
    }

    /// A `usage` event of the turn. A `total` that comes without a `last` is not spend (an ACP
    /// harness's `usage_update` says how full the context is), so it is not read.
    pub fn observe(&mut self, total: Option<TokenCounts>, last: Option<TokenCounts>) {
        match (total, last) {
            (Some(total), Some(last)) => {
                if self.baseline.is_none() {
                    self.baseline = Some(minus(&total, &last));
                }
                self.total = Some(total);
            }
            (None, Some(last)) => {
                self.direct = Some(match self.direct {
                    Some(sum) => plus(&sum, &last),
                    None => last,
                });
            }
            (Some(_), None) | (None, None) => {}
        }
    }

    /// The agent reported the cumulative cost of the session so far, in US dollars.
    pub fn cost_reported(&mut self, cost_usd: f64) {
        if cost_usd.is_finite() && cost_usd >= 0.0 {
            self.cost = Some(cost_usd);
        }
    }

    /// The turn ended: its tokens (a counter nothing reported is `None`) and cost. The cost
    /// baseline moves to now, so a cost reported between turns is not charged to the next one.
    pub fn turn_ended(&mut self) -> (TokenCounts, Option<f64>) {
        let grown = match (self.total, self.baseline) {
            (Some(total), Some(baseline)) => Some(minus(&total, &baseline)),
            _ => None,
        };
        let tokens = match (grown, self.direct) {
            (Some(a), Some(b)) => merge(&a, &b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => TokenCounts::default(),
        };
        let cost = match (self.cost, self.cost_before) {
            (Some(now), Some(before)) => Some((now - before).max(0.0)),
            _ => None,
        };
        self.cost_before = self.cost;
        self.direct = None;
        self.baseline = self.total;
        (tokens, cost)
    }
}

fn each(
    a: &TokenCounts,
    b: &TokenCounts,
    f: impl Fn(Option<u64>, Option<u64>) -> Option<u64>,
) -> TokenCounts {
    TokenCounts {
        input_tokens: f(a.input_tokens, b.input_tokens),
        cached_input_tokens: f(a.cached_input_tokens, b.cached_input_tokens),
        output_tokens: f(a.output_tokens, b.output_tokens),
        reasoning_output_tokens: f(a.reasoning_output_tokens, b.reasoning_output_tokens),
        total_tokens: f(a.total_tokens, b.total_tokens),
    }
}

/// `a - b` where both reported a counter; a counter either lacked stays unknown.
fn minus(a: &TokenCounts, b: &TokenCounts) -> TokenCounts {
    each(a, b, |a, b| Some(a?.saturating_sub(b?)))
}

/// `a + b`, counting a counter only one of them reported as it is.
fn plus(a: &TokenCounts, b: &TokenCounts) -> TokenCounts {
    each(a, b, |a, b| match (a, b) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (one, other) => one.or(other),
    })
}

/// `a`, with what it did not report from `b`.
fn merge(a: &TokenCounts, b: &TokenCounts) -> TokenCounts {
    each(a, b, |a, b| a.or(b))
}

/// Who a session's turns are charged to.
#[derive(Debug, Clone)]
pub struct Attribution {
    /// `codex`, `omp`, `claude` or `direct`.
    pub harness: String,
    pub provider: String,
    pub model: Option<String>,
}

impl Attribution {
    /// A harness chat on `model` (OMP's ids are `provider/model`).
    pub fn harness(harness: &str, model: Option<&str>) -> Self {
        let (provider, model) = match (harness, model) {
            (HARNESS_OMP, Some(model)) => match model.split_once('/') {
                Some((provider, rest)) => (provider.to_owned(), Some(rest.to_owned())),
                None => (HARNESS_OMP.to_owned(), Some(model.to_owned())),
            },
            (HARNESS_CLAUDE, model) => ("anthropic".to_owned(), model.map(str::to_owned)),
            (HARNESS_CODEX, model) => ("openai".to_owned(), model.map(str::to_owned)),
            (other, model) => (other.to_owned(), model.map(str::to_owned)),
        };
        Self {
            harness: harness.to_owned(),
            provider,
            model,
        }
    }

    /// A direct chat with `provider_id`.
    pub fn direct(provider_id: &str, model: &str) -> Self {
        Self {
            harness: HARNESS_DIRECT.to_owned(),
            provider: provider_id.to_owned(),
            model: Some(model.to_owned()),
        }
    }
}

/// A model id the records keep: short, printable, no spaces.
fn model_id(model: &str) -> Option<String> {
    let fine = !model.is_empty()
        && model.len() <= 80
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':'));
    fine.then(|| model.to_owned())
}

// Periods -----------------------------------------------------------------------------------

/// The offset of the local time zone from UTC at `ms`, in seconds east.
#[allow(unsafe_code)] // `localtime_r` on a stack value; it reads the time zone, nothing else.
pub fn local_offset_secs(ms: u64) -> i64 {
    let Ok(seconds) = libc::time_t::try_from(ms / 1000) else {
        return 0;
    };
    // SAFETY: `tm` is plain integers (and a pointer `localtime_r` fills in or leaves null); a
    // zeroed one is valid. `localtime_r` writes only into the `tm` it is given and reads
    // `seconds` by pointer for the length of the call.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&raw const seconds, &raw mut tm).is_null() {
            return 0;
        }
        tm
    };
    tm.tm_gmtoff
}

/// Local midnight of today and of this week's Monday, as UTC milliseconds. `offset` says how
/// far the local zone is from UTC at an instant, so the start of a day in which the clocks
/// changed is still the real midnight.
pub fn period_starts(now_ms: u64, offset: impl Fn(u64) -> i64) -> (u64, u64) {
    let local_midnight = |ms: u64| -> u64 {
        let at = |offset_secs: i64| -> i64 {
            let local = i64::try_from(ms / 1000).unwrap_or(0) + offset_secs;
            (local - local.rem_euclid(86_400)) - offset_secs
        };
        // The offset of the midnight itself, not of now (a change of clocks since).
        let guess = at(offset(ms));
        let guess_ms = u64::try_from(guess.max(0)).unwrap_or(0) * 1000;
        u64::try_from(at(offset(guess_ms)).max(0)).unwrap_or(0) * 1000
    };
    let day = local_midnight(now_ms);
    let local_day_index =
        i64::try_from((day / 1000).saturating_add(12 * 3600)).unwrap_or(0) + offset(day);
    // 1970-01-01 was a Thursday: days since then, shifted so that Monday is 0.
    let weekday = (local_day_index.div_euclid(86_400) + 3).rem_euclid(7);
    let monday = local_midnight(
        day.saturating_sub(u64::try_from(weekday).unwrap_or(0) * DAY_MS)
            .saturating_add(12 * 3_600_000),
    );
    (day, monday.min(day))
}

// The summary -------------------------------------------------------------------------------

fn totals(sums: &[&UsageSum]) -> UsageTotals {
    let mut out = UsageTotals::default();
    for sum in sums {
        out.turns += sum.turns;
        out.input_tokens += sum.input_tokens;
        out.cached_input_tokens += sum.cached_input_tokens;
        out.output_tokens += sum.output_tokens;
        out.reasoning_output_tokens += sum.reasoning_output_tokens;
        out.total_tokens += sum.total_tokens;
        if let Some(cost) = sum.cost_usd {
            out.cost_usd = Some(out.cost_usd.unwrap_or(0.0) + cost);
        }
    }
    out.cost_is_estimate = out.cost_usd.is_some();
    out
}

fn harness_rank(harness: &str) -> (u8, &str) {
    let rank = match harness {
        HARNESS_OMP => 0,
        HARNESS_CLAUDE => 1,
        HARNESS_CODEX => 2,
        HARNESS_DIRECT => 3,
        _ => 4,
    };
    (rank, harness)
}

/// The summary of the stored records at `now_ms`. A limit window whose reset time has passed
/// is not in it: what it said is over.
pub fn build_summary(
    now_ms: u64,
    day_start_ms: u64,
    week_start_ms: u64,
    limits: &[UsageLimitRow],
    today: &[UsageSum],
    week: &[UsageSum],
) -> UsageSummary {
    let mut names: Vec<&str> = limits
        .iter()
        .filter(|row| !is_over(row, now_ms))
        .map(|row| row.harness.as_str())
        .chain(week.iter().map(|sum| sum.harness.as_str()))
        .collect();
    names.sort_by_key(|name| harness_rank(name));
    names.dedup();
    let harnesses = names
        .into_iter()
        .map(|harness| {
            let of = |sums: &[UsageSum]| -> UsageTotals {
                totals(
                    &sums
                        .iter()
                        .filter(|s| s.harness == harness)
                        .collect::<Vec<_>>(),
                )
            };
            let mut windows: Vec<&UsageLimitRow> = limits
                .iter()
                .filter(|row| row.harness == harness && !is_over(row, now_ms))
                .collect();
            windows.sort_by(|a, b| {
                (&a.provider, a.window_minutes, &a.window_id).cmp(&(
                    &b.provider,
                    b.window_minutes,
                    &b.window_id,
                ))
            });
            HarnessUsage {
                harness: harness.to_owned(),
                limits: windows
                    .into_iter()
                    .map(|row| UsageLimit {
                        provider: row.provider.clone(),
                        id: row.window_id.clone(),
                        label: row.label.clone(),
                        used_percent: row.used_percent,
                        window_minutes: row.window_minutes,
                        resets_at_ms: row.resets_at_ms,
                        plan: row.plan.clone(),
                        observed_at_ms: row.observed_at_ms,
                    })
                    .collect(),
                today: of(today),
                week: of(week),
            }
        })
        .collect();
    let mut by_provider: BTreeMap<&str, ()> = BTreeMap::new();
    for sum in week {
        by_provider.insert(sum.provider.as_str(), ());
    }
    let mut providers: Vec<ProviderUsage> = by_provider
        .into_keys()
        .map(|provider| {
            let of = |sums: &[UsageSum]| -> UsageTotals {
                totals(
                    &sums
                        .iter()
                        .filter(|s| s.provider == provider)
                        .collect::<Vec<_>>(),
                )
            };
            ProviderUsage {
                provider: provider.to_owned(),
                today: of(today),
                week: of(week),
            }
        })
        .collect();
    providers.sort_by(|a, b| {
        b.week
            .total_tokens
            .cmp(&a.week.total_tokens)
            .then_with(|| a.provider.cmp(&b.provider))
    });
    UsageSummary {
        generated_at_ms: now_ms,
        day_start_ms,
        week_start_ms,
        harnesses,
        providers,
    }
}

fn is_over(row: &UsageLimitRow, now_ms: u64) -> bool {
    row.resets_at_ms.is_some_and(|resets| resets <= now_ms)
}

// OMP's own report ----------------------------------------------------------------------------

/// The limit windows in `omp usage --json` (OMP 18.12): per account report, `limits[]` with a
/// `window.durationMs`, `window.resetsAt` (milliseconds) and an `amount` (`usedFraction`, or
/// `used` of `limit`). Account e-mails, ids and organisations in `metadata` are never read.
/// Reports of several accounts of one provider keep the tightest window. `None` when the text
/// is not a usage report at all.
pub fn parse_omp_usage(text: &str, now_ms: u64) -> Option<Vec<UsageLimitRow>> {
    let value: Value = serde_json::from_str(text).ok()?;
    let reports = value.get("reports")?.as_array()?;
    let mut rows: BTreeMap<(String, String), UsageLimitRow> = BTreeMap::new();
    for report in reports {
        let Some(provider) = report
            .get("provider")
            .and_then(Value::as_str)
            .and_then(identifier)
        else {
            continue;
        };
        let plan = report
            .pointer("/metadata/planType")
            .and_then(Value::as_str)
            .and_then(identifier)
            .filter(|plan| plan != "unknown");
        let observed = report
            .get("fetchedAt")
            .and_then(Value::as_u64)
            .unwrap_or(now_ms);
        for limit in report
            .get("limits")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let amount = limit.get("amount");
            let used = amount
                .and_then(|a| a.get("usedFraction"))
                .and_then(Value::as_f64)
                .map(|fraction| fraction * 100.0)
                .or_else(|| {
                    let used = amount?.get("used")?.as_f64()?;
                    let max = amount?.get("limit")?.as_f64()?;
                    (max > 0.0).then(|| used / max * 100.0)
                });
            let Some(used) = used.filter(|u| u.is_finite()) else {
                continue;
            };
            let minutes = limit
                .pointer("/window/durationMs")
                .and_then(Value::as_u64)
                .map(|ms| ms / 60_000)
                .filter(|m| *m > 0);
            let window_label = limit
                .pointer("/window/label")
                .and_then(Value::as_str)
                .and_then(identifier);
            let (mut id, mut label) = match (minutes, &window_label) {
                (Some(minutes), _) => window_names(minutes),
                (None, Some(label)) => (label.clone(), label.clone()),
                (None, None) => continue,
            };
            // A tier (a model family with a meter of its own) is a window of its own.
            if let Some(tier) = limit
                .pointer("/scope/tier")
                .and_then(Value::as_str)
                .and_then(identifier)
            {
                label = format!("{label} ({tier})");
                id = format!("{id}:{tier}");
            }
            let row = UsageLimitRow {
                harness: HARNESS_OMP.to_owned(),
                provider: provider.clone(),
                window_id: id.clone(),
                label,
                used_percent: used.max(0.0),
                window_minutes: minutes,
                resets_at_ms: limit.pointer("/window/resetsAt").and_then(Value::as_u64),
                plan: plan.clone(),
                observed_at_ms: observed,
            };
            match rows.get(&(provider.clone(), id.clone())) {
                Some(old) if old.used_percent >= row.used_percent => {}
                _ => {
                    rows.insert((provider.clone(), id), row);
                }
            }
        }
    }
    Some(rows.into_values().collect())
}

// The service ---------------------------------------------------------------------------------

/// Records usage and answers `usage.summary`.
#[derive(Debug)]
pub struct UsageService {
    storage: Storage,
    bus: Arc<EventBus>,
    harnesses: Arc<Harnesses>,
    codex: CodexConfig,
    /// Whether a refresh may ask the harnesses (`DaemonConfig::usage_probes`).
    probes: bool,
    /// A `usage.changed` is waiting for its coalescing delay.
    publish_pending: AtomicBool,
    /// Held while the harnesses are asked; holds when the last refresh finished.
    refreshing: AsyncMutex<u64>,
    /// How many refreshes have finished: a caller that waited for the lock while one did has
    /// what it wanted.
    refreshed: AtomicU64,
}

impl UsageService {
    pub fn new(
        storage: Storage,
        bus: Arc<EventBus>,
        harnesses: Arc<Harnesses>,
        codex: CodexConfig,
        probes: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            storage,
            bus,
            harnesses,
            codex,
            probes,
            publish_pending: AtomicBool::new(false),
            refreshing: AsyncMutex::new(0),
            refreshed: AtomicU64::new(0),
        })
    }

    /// A turn ended: records what it used (nothing when it reported nothing).
    pub fn turn_ended(
        self: &Arc<Self>,
        session: SessionId,
        turn_id: String,
        attribution: &Attribution,
        tokens: TokenCounts,
        cost_usd: Option<f64>,
        at_ms: u64,
    ) {
        let reported = tokens.input_tokens.is_some()
            || tokens.output_tokens.is_some()
            || tokens.total_tokens.is_some()
            || cost_usd.is_some();
        if !reported {
            return;
        }
        let total =
            tokens
                .total_tokens
                .or_else(|| match (tokens.input_tokens, tokens.output_tokens) {
                    (None, None) => None,
                    (input, output) => Some(input.unwrap_or(0).saturating_add(output.unwrap_or(0))),
                });
        let row = UsageTurnRow {
            session_id: session,
            turn_id,
            at_ms,
            harness: identifier(&attribution.harness).unwrap_or_else(|| "other".to_owned()),
            provider: identifier(&attribution.provider).unwrap_or_else(|| "other".to_owned()),
            model: attribution.model.as_deref().and_then(model_id),
            input_tokens: tokens.input_tokens,
            cached_input_tokens: tokens.cached_input_tokens,
            output_tokens: tokens.output_tokens,
            reasoning_output_tokens: tokens.reasoning_output_tokens,
            total_tokens: total,
            cost_usd,
        };
        let this = self.clone();
        tokio::task::spawn_blocking(move || match this.storage.usage().record_turn(row) {
            Ok(()) => this.changed(),
            Err(error) => warn!(%error, "could not record a turn's usage"),
        });
    }

    /// A harness reported the limit windows of its account.
    pub fn limits_reported(self: &Arc<Self>, harness: &str, limits: &RateLimits) {
        let now = now_ms();
        let Some(provider) = identifier(&limits.provider) else {
            return;
        };
        let rows: Vec<UsageLimitRow> = limits
            .windows
            .iter()
            .map(|window| UsageLimitRow {
                harness: harness.to_owned(),
                provider: provider.clone(),
                window_id: window.id.clone(),
                label: window.label.clone(),
                used_percent: window.used_percent,
                window_minutes: window.window_minutes,
                resets_at_ms: window.resets_at_ms,
                plan: limits.plan.clone(),
                observed_at_ms: now,
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || match this.storage.usage().upsert_limits(rows) {
            Ok(()) => this.changed(),
            Err(error) => warn!(%error, "could not record limits"),
        });
    }

    /// Tells every client, once the burst is over.
    fn changed(self: &Arc<Self>) {
        if self.publish_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(COALESCE).await;
            this.publish_pending.store(false, Ordering::Release);
            match this.snapshot().await {
                Ok(summary) => {
                    if let Err(error) = this.bus.publish(&Event::UsageChanged {
                        summary: Box::new(summary),
                    }) {
                        warn!(%error, "failed to publish usage.changed");
                    }
                }
                Err(error) => warn!(message = %error.message, "could not read usage"),
            }
        });
    }

    /// The summary; with `refresh` the harnesses are asked what is left first.
    pub async fn summary(self: &Arc<Self>, refresh: bool) -> Result<UsageSummary, ErrorBody> {
        if refresh {
            self.refresh().await;
        }
        self.snapshot().await
    }

    /// The summary of what is stored, now.
    pub async fn snapshot(&self) -> Result<UsageSummary, ErrorBody> {
        let storage = self.storage.clone();
        let now = now_ms();
        tokio::task::spawn_blocking(move || {
            let (day, week) = period_starts(now, local_offset_secs);
            let usage = storage.usage();
            let limits = usage.limits()?;
            let week_sums = usage.sums_since(week)?;
            let day_sums = usage.sums_since(day)?;
            Ok::<_, hutan_storage::StorageError>(build_summary(
                now, day, week, &limits, &day_sums, &week_sums,
            ))
        })
        .await
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading usage: {e}")))?
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading usage: {e}")))
    }

    /// Asks the harnesses that can be asked: OMP (`omp usage --json`), and Codex when it has
    /// been used through Hutan. Failures are logged and leave what was known.
    pub async fn refresh(self: &Arc<Self>) {
        if !self.probes {
            return;
        }
        let seen = self.refreshed.load(Ordering::Acquire);
        let mut last = self.refreshing.lock().await;
        if self.refreshed.load(Ordering::Acquire) != seen {
            // Another caller's refresh ended while this one waited: its answer is fresh.
            return;
        }
        tokio::join!(self.refresh_omp(), self.refresh_codex());
        *last = now_ms();
        self.refreshed.fetch_add(1, Ordering::AcqRel);
        drop(last);
        self.changed();
    }

    /// Whether a harness has reported limits or run a turn through Hutan.
    fn known(&self, harness: &str) -> bool {
        let usage = self.storage.usage();
        usage
            .limits()
            .is_ok_and(|rows| rows.iter().any(|row| row.harness == harness))
            || usage
                .sums_since(0)
                .is_ok_and(|sums| sums.iter().any(|sum| sum.harness == harness))
    }

    async fn refresh_omp(&self) {
        let harnesses = self.harnesses.clone();
        let codex = self.codex.program.clone();
        let cli = tokio::task::spawn_blocking(move || harnesses.cli(HARNESS_OMP, &codex)).await;
        let Ok(Ok((program, env))) = cli else {
            debug!("omp is not installed: no limits from `omp usage`");
            return;
        };
        let text = match crate::accounts::run(&program, &["usage", "--json"], &env).await {
            Ok(text) => text,
            Err(error) => {
                debug!(message = %error.message, "no usage report from omp");
                return;
            }
        };
        let Some(rows) = parse_omp_usage(&text, now_ms()) else {
            debug!("`omp usage --json` printed something that is not a usage report");
            return;
        };
        let storage = self.storage.clone();
        let stored = tokio::task::spawn_blocking(move || {
            storage.usage().replace_limits(HARNESS_OMP.to_owned(), rows)
        })
        .await;
        if !matches!(stored, Ok(Ok(()))) {
            warn!("could not record the limits OMP reported");
        }
    }

    async fn refresh_codex(self: &Arc<Self>) {
        let this = self.clone();
        let known = tokio::task::spawn_blocking(move || this.known(HARNESS_CODEX))
            .await
            .unwrap_or(false);
        if !known {
            return;
        }
        match timeout(
            CODEX_READ_TIMEOUT,
            hutan_agents::codex::read_rate_limits(&self.codex),
        )
        .await
        {
            Ok(Ok(Some(limits))) => self.limits_reported(HARNESS_CODEX, &limits),
            Ok(Ok(None)) => {}
            Ok(Err(AgentError::Spawn { .. })) => debug!("codex is not installed"),
            Ok(Err(error)) => debug!(%error, "codex did not report its limits"),
            Err(_) => debug!("codex did not report its limits in time"),
        }
    }

    /// An OMP turn ended: its limits moved, and OMP is the harness that can say so; asked at
    /// most once a minute.
    pub fn harness_turn_ended(self: &Arc<Self>, harness: &str) {
        if harness != HARNESS_OMP || !self.probes {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let stale = {
                let last = this.refreshing.lock().await;
                now_ms().saturating_sub(*last) >= 60_000
            };
            if stale {
                this.refresh().await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(input: u64, output: u64) -> TokenCounts {
        TokenCounts {
            input_tokens: Some(input),
            output_tokens: Some(output),
            total_tokens: Some(input + output),
            ..TokenCounts::default()
        }
    }

    #[test]
    fn a_cumulative_total_gives_each_turn_its_own_growth() {
        // Codex and the gateway: `total` runs on, `last` is the request just made.
        let mut turn = TurnUsage::new(true);
        turn.turn_started();
        turn.observe(Some(counts(100, 10)), Some(counts(100, 10)));
        let (tokens, _) = turn.turn_ended();
        assert_eq!(tokens.total_tokens, Some(110));
        turn.turn_started();
        // Two requests in one turn.
        turn.observe(Some(counts(250, 30)), Some(counts(150, 20)));
        turn.observe(Some(counts(400, 50)), Some(counts(150, 20)));
        let (tokens, _) = turn.turn_ended();
        assert_eq!(tokens.input_tokens, Some(300));
        assert_eq!(tokens.output_tokens, Some(40));
        assert_eq!(tokens.total_tokens, Some(340));
    }

    #[test]
    fn the_first_turn_after_a_resume_starts_from_total_minus_last() {
        // The thread already held 5 000 tokens when Hutan resumed it.
        let mut turn = TurnUsage::new(false);
        turn.turn_started();
        turn.observe(Some(counts(5_100, 60)), Some(counts(100, 10)));
        let (tokens, _) = turn.turn_ended();
        assert_eq!(tokens.input_tokens, Some(100));
        assert_eq!(tokens.total_tokens, Some(110));
    }

    #[test]
    fn an_acp_turn_counts_its_own_answer_and_not_the_context_size() {
        // `usage_update` gives the context (`total` only); the prompt's answer gives the turn.
        let mut turn = TurnUsage::new(true);
        turn.turn_started();
        turn.observe(
            Some(TokenCounts {
                total_tokens: Some(1_234),
                ..TokenCounts::default()
            }),
            None,
        );
        turn.observe(None, Some(counts(1_200, 34)));
        let (tokens, _) = turn.turn_ended();
        assert_eq!(tokens.input_tokens, Some(1_200));
        assert_eq!(tokens.total_tokens, Some(1_234));
        // The next turn does not inherit the context size as spend.
        turn.turn_started();
        turn.observe(None, Some(counts(10, 1)));
        assert_eq!(turn.turn_ended().0.total_tokens, Some(11));
    }

    #[test]
    fn a_turn_that_reported_nothing_has_nothing() {
        let mut turn = TurnUsage::new(true);
        turn.turn_started();
        assert_eq!(turn.turn_ended(), (TokenCounts::default(), None));
    }

    #[test]
    fn cost_is_the_growth_of_the_session_cost() {
        let mut turn = TurnUsage::new(true);
        turn.turn_started();
        turn.cost_reported(0.25);
        assert_eq!(turn.turn_ended().1, Some(0.25));
        turn.turn_started();
        turn.cost_reported(0.75);
        let (_, cost) = turn.turn_ended();
        assert!((cost.unwrap() - 0.5).abs() < 1e-9);
        // A resumed session's first cost has no known start: nothing is claimed for that turn.
        let mut resumed = TurnUsage::new(false);
        resumed.turn_started();
        resumed.cost_reported(9.0);
        assert_eq!(resumed.turn_ended().1, None);
        resumed.turn_started();
        resumed.cost_reported(9.5);
        assert!((resumed.turn_ended().1.unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn omp_models_name_their_provider_and_others_have_fixed_ones() {
        let omp = Attribution::harness("omp", Some("anthropic/claude-haiku-5-5"));
        assert_eq!(
            (omp.provider.as_str(), omp.model.as_deref()),
            ("anthropic", Some("claude-haiku-5-5"))
        );
        assert_eq!(
            Attribution::harness("codex", Some("gpt-6.1-sol")).provider,
            "openai"
        );
        assert_eq!(Attribution::harness("claude", None).provider, "anthropic");
        assert_eq!(Attribution::direct("ollama", "llama").harness, "direct");
    }

    #[test]
    fn periods_start_at_local_midnight_and_on_monday() {
        // 2026-10-25 (a Sunday) 02:30 UTC.
        let sunday = 1_792_895_400_000u64;
        let (day, week) = period_starts(sunday, |_| 0);
        assert_eq!(day, 1_792_886_400_000, "2026-10-25 00:00 UTC");
        assert_eq!(week, day - 6 * DAY_MS, "the Monday before");
        // In UTC-5 it is still Saturday evening (21:30 on the 24th).
        let (day, week) = period_starts(sunday, |_| -5 * 3600);
        assert_eq!(
            day,
            1_792_886_400_000 - DAY_MS + 5 * 3_600_000,
            "the 24th, 00:00 at -05:00"
        );
        assert_eq!(week, day - 5 * DAY_MS, "Monday the 19th");
        // On a Monday the week starts today.
        let monday = 1_792_886_400_000 + DAY_MS + 3_600_000;
        let (day, week) = period_starts(monday, |_| 0);
        assert_eq!(day, week);
    }

    fn limit(harness: &str, id: &str, used: f64, resets: Option<u64>) -> UsageLimitRow {
        UsageLimitRow {
            harness: harness.to_owned(),
            provider: "openai".to_owned(),
            window_id: id.to_owned(),
            label: id.to_owned(),
            used_percent: used,
            window_minutes: Some(if id == "5h" { 300 } else { 10_080 }),
            resets_at_ms: resets,
            plan: None,
            observed_at_ms: 1,
        }
    }

    fn sum(harness: &str, provider: &str, turns: u64, tokens: u64, cost: Option<f64>) -> UsageSum {
        UsageSum {
            harness: harness.to_owned(),
            provider: provider.to_owned(),
            turns,
            total_tokens: tokens,
            input_tokens: tokens,
            cost_usd: cost,
            ..UsageSum::default()
        }
    }

    #[test]
    fn the_summary_orders_harnesses_drops_windows_that_reset_and_labels_estimates() {
        let now = 1_000_000;
        let summary = build_summary(
            now,
            900_000,
            100_000,
            &[
                limit("codex", "7d", 31.0, None),
                limit("codex", "5h", 72.0, Some(now + 1)),
                limit("codex", "5h-old", 99.0, Some(now - 1)),
                limit("omp", "5h", 5.0, Some(now + 5)),
            ],
            &[sum("omp", "anthropic", 1, 10, Some(0.5))],
            &[
                sum("direct", "ollama", 1, 7, None),
                sum("omp", "anthropic", 3, 40, Some(1.5)),
                sum("omp", "openai", 1, 5, None),
            ],
        );
        let order: Vec<_> = summary
            .harnesses
            .iter()
            .map(|h| h.harness.as_str())
            .collect();
        assert_eq!(order, ["omp", "codex", "direct"]);
        let codex = &summary.harnesses[1];
        let ids: Vec<_> = codex.limits.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(
            ids,
            ["5h", "7d"],
            "the window that reset is gone, shortest first"
        );
        let omp = &summary.harnesses[0];
        assert_eq!(
            (omp.today.turns, omp.week.turns, omp.week.total_tokens),
            (1, 4, 45)
        );
        assert_eq!(omp.week.cost_usd, Some(1.5));
        assert!(omp.week.cost_is_estimate);
        assert!(!summary.harnesses[2].week.cost_is_estimate);
        let providers: Vec<_> = summary
            .providers
            .iter()
            .map(|p| p.provider.as_str())
            .collect();
        assert_eq!(
            providers,
            ["anthropic", "ollama", "openai"],
            "most used first"
        );
    }

    #[test]
    fn omp_usage_json_gives_windows_and_never_reads_the_account() {
        let text = include_str!("../tests/fixtures/omp-usage.json");
        let rows = parse_omp_usage(text, 5).unwrap();
        let shown: Vec<_> = rows
            .iter()
            .map(|r| {
                (
                    r.provider.as_str(),
                    r.window_id.as_str(),
                    r.used_percent.round() as u64,
                    r.resets_at_ms,
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("anthropic", "5h", 7, Some(1_792_887_600_256)),
                ("anthropic", "7d", 23, Some(1_793_012_400_256)),
                ("anthropic", "7d:fable", 0, Some(1_793_012_400_000)),
            ]
        );
        assert_eq!(rows[2].label, "weekly (fable)");
        let stored = format!("{rows:?}");
        for private in ["@", "orgName", "accountId"] {
            assert!(!stored.contains(private), "{private} in {stored}");
        }
        assert!(parse_omp_usage("not json", 5).is_none());
        assert!(parse_omp_usage("{}", 5).is_none());
        assert_eq!(parse_omp_usage(r#"{"reports":[]}"#, 5).unwrap(), []);
    }
}
