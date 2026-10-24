//! Usage records over the wire: what `usage.summary` adds up from the database (`docs/agents.md`
//! § Usage). The turns come from the chat tests (`chat.rs`, `harness.rs`); here the database is
//! filled first, so that days and weeks can be told apart without waiting for them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::TestDaemon;

use hutan_core::SessionId;
use hutan_daemon::usage::{local_offset_secs, period_starts};
use hutan_storage::{Storage, UsageTurnRow};

const DAY: u64 = 86_400_000;

fn turn(at_ms: u64, harness: &str, provider: &str, tokens: u64, cost: Option<f64>) -> UsageTurnRow {
    UsageTurnRow {
        session_id: SessionId::new(),
        turn_id: format!("turn-{at_ms}-{tokens}"),
        at_ms,
        harness: harness.to_owned(),
        provider: provider.to_owned(),
        model: None,
        input_tokens: Some(tokens),
        cached_input_tokens: None,
        output_tokens: Some(0),
        reasoning_output_tokens: None,
        total_tokens: Some(tokens),
        cost_usd: cost,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn turns_are_summed_for_today_and_for_this_week_and_older_ones_do_not_count() {
    let dir = tempfile::tempdir().unwrap();
    let paths = hutan_daemon::Paths::in_home(dir.path());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let (day, week) = period_starts(now, local_offset_secs);
    assert!(week <= day && day <= now);
    {
        let storage = Storage::open(&paths.database).unwrap();
        let usage = storage.usage();
        // Today, earlier this week, last week.
        usage
            .record_turn(turn(day, "omp", "anthropic", 100, Some(0.5)))
            .unwrap();
        usage
            .record_turn(turn(day + 1, "codex", "openai", 10, None))
            .unwrap();
        usage
            .record_turn(turn(day - 1, "omp", "anthropic", 1_000, Some(1.5)))
            .unwrap();
        if week < day {
            // (On a Monday this turn is last week's: the day before the week began.)
            usage
                .record_turn(turn(week, "omp", "anthropic", 10_000, None))
                .unwrap();
        }
        usage
            .record_turn(turn(week - 1, "omp", "anthropic", 100_000, Some(9.0)))
            .unwrap();
        usage
            .record_turn(turn(week - 2 * DAY, "direct", "ollama", 7, None))
            .unwrap();
        storage.close();
    }
    let daemon = TestDaemon::start_in(dir, "", |_| {}).await;
    let (client, _events) = daemon.connect_subscribed().await;
    let summary = client.usage_summary(false).await.unwrap();
    assert_eq!((summary.day_start_ms, summary.week_start_ms), (day, week));

    let names: Vec<_> = summary
        .harnesses
        .iter()
        .map(|h| h.harness.as_str())
        .collect();
    assert_eq!(
        names,
        ["omp", "codex"],
        "last week's direct turn is not in it"
    );
    let omp = &summary.harnesses[0];
    assert_eq!(
        (omp.today.turns, omp.today.total_tokens, omp.today.cost_usd),
        (1, 100, Some(0.5))
    );
    let this_week = if week < day {
        1_000 + 10_000 + 100
    } else {
        100
    };
    assert_eq!(
        omp.week.total_tokens, this_week,
        "last week's 100 000 is not in it"
    );
    let codex = &summary.harnesses[1];
    assert_eq!(
        (codex.today.turns, codex.week.turns, codex.week.cost_usd),
        (1, 1, None)
    );
    assert!(!codex.week.cost_is_estimate, "no cost, no estimate label");
    assert!(omp.week.cost_is_estimate);
    let providers: Vec<_> = summary
        .providers
        .iter()
        .map(|p| p.provider.as_str())
        .collect();
    assert_eq!(providers, ["anthropic", "openai"], "most used first");
    daemon.stop().await;
}
