#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests of usage and limits in the window (USAGE-LIMITS): the `usage` status item and the
//! Usage section of the Agents page drawn from a fixture summary, and the toast at 80 % and 95 %
//! of a limit window — once per window, never for what was already there when the window
//! opened, and only while `notifications.usage_warnings` is on. The summaries are what the
//! daemon's `usage.summary` and `usage.changed` carry (the daemon side is tested in
//! `crates/daemon/tests/it/{chat,harness,usage}.rs`).

use crate::common;

use common::{TestDaemon, draw, open_with, settle, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::panels::AgentsRequest;
use hutan_desktop::workbench::usage::USAGE_TOAST_TIME;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::usage::{HarnessUsage, ProviderUsage, UsageLimit, UsageSummary, UsageTotals};
use hutan_settings::{SettingsPaths, SettingsStore};

const HOUR: u64 = 3_600_000;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn limit(provider: &str, id: &str, used: f64, resets: Option<u64>) -> UsageLimit {
    UsageLimit {
        provider: provider.into(),
        id: id.into(),
        label: if id == "5h" { "5-hour" } else { "weekly" }.into(),
        used_percent: used,
        window_minutes: Some(if id == "5h" { 300 } else { 10_080 }),
        resets_at_ms: resets,
        plan: None,
        observed_at_ms: now(),
    }
}

fn totals(turns: u64, tokens: u64, cost: Option<f64>) -> UsageTotals {
    UsageTotals {
        turns,
        total_tokens: tokens,
        input_tokens: tokens,
        cost_usd: cost,
        cost_is_estimate: cost.is_some(),
        ..UsageTotals::default()
    }
}

/// Codex at `used` % of its 5-hour window (resetting in three hours) and 31 % of its weekly one;
/// OMP with a 7 % window and a cost; a direct chat with turns and no limits.
fn summary(used: f64, resets: u64) -> UsageSummary {
    UsageSummary {
        generated_at_ms: now(),
        day_start_ms: now() - HOUR,
        week_start_ms: now() - 24 * HOUR,
        harnesses: vec![
            HarnessUsage {
                harness: "omp".into(),
                limits: vec![limit("anthropic", "5h", 7.0, Some(now() + 2 * HOUR))],
                today: totals(2, 1_234, Some(0.42)),
                week: totals(9, 45_000, Some(1.5)),
            },
            HarnessUsage {
                harness: "codex".into(),
                limits: vec![
                    limit("openai", "5h", used, Some(resets)),
                    limit("openai", "7d", 31.0, None),
                ],
                today: totals(1, 22_178, None),
                week: totals(4, 90_000, None),
            },
            HarnessUsage {
                harness: "direct".into(),
                limits: vec![],
                today: totals(0, 0, None),
                week: totals(1, 7, None),
            },
        ],
        providers: vec![ProviderUsage {
            provider: "anthropic".into(),
            today: totals(2, 1_234, Some(0.42)),
            week: totals(9, 45_000, Some(1.5)),
        }],
    }
}

fn settings_in(daemon: &TestDaemon, toml: &str) -> SettingsStore {
    let paths = SettingsPaths::in_dir(daemon.home());
    std::fs::create_dir_all(daemon.home()).unwrap();
    if !toml.is_empty() {
        std::fs::write(&paths.user, toml).unwrap();
    }
    SettingsStore::new(paths)
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    toml: &str,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, settings_in(daemon, toml), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    (view, cx)
}

fn give(view: &Entity<Workbench>, cx: &mut VisualTestContext, summary: UsageSummary) {
    view.update(cx, |wb, cx| {
        wb.usage_directory()
            .update(cx, |usage, cx| usage.apply(summary, cx));
    });
    cx.run_until_parked();
    draw(cx);
}

fn item_drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::StatusItem("usage"))
            .is_some_and(|b| b.size.width > gpui::px(8.0))
    })
}

fn toast(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.usage_toast().map(|t| t.text.to_string()))
}

// ---- the status item -------------------------------------------------------------------------

#[gpui::test]
fn the_usage_item_is_not_in_the_built_in_bar_and_agents_settings_turn_it_on(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    give(&view, cx, summary(72.0, now() + 3 * HOUR));
    let bar = view.read_with(cx, |wb, _| wb.status_bar());
    assert!(
        !bar.left
            .iter()
            .chain(&bar.right)
            .any(|item| item == "usage"),
        "off by default in the built-in bar: {bar:?}"
    );
    assert!(!item_drawn(&view, cx), "so it is not drawn");
    // What the item would say is known all the same: the tightest limit of any harness.
    let text = view.read_with(cx, |wb, cx| wb.status_data(cx).usage.unwrap());
    assert_eq!(text.text, "Codex 72% · resets 3h");

    drop(view);
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "[agents]\nusage_in_status_bar = true\n");
    assert!(!item_drawn(&view, cx), "nothing reported, nothing drawn");
    give(&view, cx, summary(72.0, now() + 3 * HOUR));
    assert!(
        item_drawn(&view, cx),
        "Settings → Agents puts it in the bar"
    );
    let data = view.read_with(cx, |wb, cx| wb.status_data(cx).usage.unwrap());
    assert_eq!(data.text, "Codex 72% · resets 3h");
    assert!(
        data.tooltip.contains("OMP (anthropic) 5-hour 7%"),
        "{}",
        data.tooltip
    );
    assert!(
        data.tooltip.contains("Codex weekly 31%"),
        "{}",
        data.tooltip
    );
    assert!(
        data.tooltip
            .contains("OMP today: 1.2k tokens · ≈ $0.42 (estimate)"),
        "{}",
        data.tooltip
    );

    // The tightest limit changes with the numbers; a window that has reset is not one.
    give(&view, cx, summary(30.0, now() + 3 * HOUR));
    let data = view.read_with(cx, |wb, cx| wb.status_data(cx).usage.unwrap());
    assert_eq!(
        data.text, "Codex 31%",
        "the weekly window, which says no reset time"
    );
    give(&view, cx, summary(99.0, now() - 1));
    let data = view.read_with(cx, |wb, cx| wb.status_data(cx).usage.unwrap());
    assert_eq!(data.text, "Codex 31%", "the 5-hour window reset");
}

// ---- the Agents page -------------------------------------------------------------------------

#[gpui::test]
fn the_agents_page_shows_each_harness_with_bars_resets_and_estimate_labels(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    view.update_in(cx, |wb, window, cx| {
        wb.open_agents_page(AgentsRequest::default(), window, cx);
    });
    settle(cx);
    let drawn = |view: &Entity<Workbench>, cx: &mut VisualTestContext, region| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(region))
    };
    assert!(
        drawn(&view, cx, Region::AgentsPage("usage")).is_none(),
        "no section while the daemon has said nothing"
    );

    give(&view, cx, summary(72.0, now() + 3 * HOUR));
    settle(cx);
    assert!(drawn(&view, cx, Region::AgentsPage("usage")).is_some());
    assert!(drawn(&view, cx, Region::AgentsPage("usage-refresh")).is_some());
    for index in 0..3 {
        assert!(
            drawn(&view, cx, Region::UsageRow(index)).is_some(),
            "row {index}: OMP, Codex, a direct chat"
        );
    }
    assert!(drawn(&view, cx, Region::UsageRow(3)).is_none());
    // The bars' fill is the share used of a 96 px track.
    let width = |view: &Entity<Workbench>, cx: &mut VisualTestContext, harness, limit| {
        let bounds = drawn(view, cx, Region::UsageBar(harness, limit)).expect("a bar");
        f32::from(bounds.size.width)
    };
    assert!(
        (width(&view, cx, 0, 0) - 96.0 * 0.07).abs() < 1.0,
        "OMP 7 %"
    );
    assert!(
        (width(&view, cx, 1, 0) - 96.0 * 0.72).abs() < 1.0,
        "Codex 5-hour 72 %"
    );
    assert!(
        (width(&view, cx, 1, 1) - 96.0 * 0.31).abs() < 1.0,
        "Codex weekly 31 %"
    );
    assert!(
        drawn(&view, cx, Region::UsageBar(2, 0)).is_none(),
        "a direct chat has no limit"
    );
    // The rows are as tall as their words: a row for each harness, one above the other.
    let top = |view: &Entity<Workbench>, cx: &mut VisualTestContext, i| {
        f32::from(drawn(view, cx, Region::UsageRow(i)).unwrap().origin.y)
    };
    assert!(top(&view, cx, 0) < top(&view, cx, 1) && top(&view, cx, 1) < top(&view, cx, 2));

    // Hiding the section is a setting.
    view.update(cx, |wb, cx| {
        wb.settings_store()
            .write_user("agents.page_usage", toml::Value::Boolean(false))
            .unwrap();
        cx.notify();
    });
    settle(cx);
    assert!(drawn(&view, cx, Region::AgentsPage("usage")).is_none());
}

// ---- the warnings ----------------------------------------------------------------------------

#[gpui::test]
fn a_limit_warns_at_80_and_at_95_once_per_window_and_not_for_what_was_there_at_the_start(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    let window = now() + 3 * HOUR;
    // The first summary seeds: the window is at 85 % when the app opens, and that is no news.
    give(&view, cx, summary(85.0, window));
    assert_eq!(toast(&view, cx), None);
    give(&view, cx, summary(90.0, window));
    assert_eq!(
        toast(&view, cx),
        None,
        "80 was passed before the window opened"
    );
    give(&view, cx, summary(96.0, window));
    assert_eq!(
        toast(&view, cx).as_deref(),
        Some("Codex is at 96% of its 5-hour limit · resets in 3h")
    );
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::UsageToast("toast"))
            .is_some()),
        "the toast is drawn"
    );
    // Dismissed, it does not come back for the same level.
    let dismiss = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::UsageToast("dismiss")))
        .expect("a dismiss button");
    cx.simulate_click(dismiss.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(toast(&view, cx), None);
    give(&view, cx, summary(97.0, window));
    assert_eq!(toast(&view, cx), None, "once per level and window");

    // The window resets; the next one is a window of its own.
    let next = now() + 8 * HOUR;
    give(&view, cx, summary(10.0, next));
    assert_eq!(toast(&view, cx), None);
    give(&view, cx, summary(81.0, next));
    assert_eq!(
        toast(&view, cx).as_deref(),
        Some("Codex is at 81% of its 5-hour limit · resets in 8h")
    );
    // A toast goes by itself after a while.
    cx.executor()
        .advance_clock(USAGE_TOAST_TIME + std::time::Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(toast(&view, cx), None);
}

#[gpui::test]
fn warnings_follow_the_notification_settings(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "[notifications]\nusage_warnings = false\n");
    let window = now() + 3 * HOUR;
    give(&view, cx, summary(10.0, window));
    give(&view, cx, summary(96.0, window));
    assert_eq!(
        toast(&view, cx),
        None,
        "switched off in Settings → Notifications"
    );
    // The numbers are there all the same.
    let data = view.read_with(cx, |wb, cx| wb.status_data(cx).usage.unwrap());
    assert_eq!(data.text, "Codex 96% · resets 3h");
}
