#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests of usage and limits in the window (USAGE-LIMITS): the `usage` status item and the
//! Usage section of the Agents page drawn from a fixture summary, and the toast at 80 % and 95 %
//! of a limit window — once per window, never for what was already there when the window
//! opened, and only while `notifications.usage_warnings` is on. The summaries are what the
//! daemon's `usage.summary` and `usage.changed` carry (the daemon side is tested in
//! `crates/daemon/tests/it/{chat,harness,usage}.rs`).

use crate::common;

use common::{TestDaemon, draw, open_with, settle, wait_until};
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use hutan_desktop::sessions::usage::{MeterView, Severity};
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::panels::AgentsRequest;
use hutan_desktop::workbench::style::TooltipGate;
use hutan_desktop::workbench::usage::USAGE_TOAST_TIME;
use hutan_desktop::workbench::usage_card::UsageCard;
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
        account: None,
        account_label: None,
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

fn item_drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: &'static str) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::StatusItem(id))
            .is_some_and(|b| b.size.width > gpui::px(8.0))
    })
}

fn toast(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.usage_toast().map(|t| t.text.to_string()))
}

// ---- the status item -------------------------------------------------------------------------

#[gpui::test]
fn the_usage_words_are_not_in_the_built_in_bar_and_the_text_style_swaps_them_for_the_rings(
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
        "the words are off in the built-in bar: {bar:?}"
    );
    assert!(
        bar.right.iter().any(|item| item == "usage.meters"),
        "{bar:?}"
    );
    assert!(!item_drawn(&view, cx, "usage"), "so they are not drawn");
    assert!(item_drawn(&view, cx, "usage.meters"), "the rings are");
    // What the item would say is known all the same: the tightest limit of any harness.
    let text = view.read_with(cx, |wb, cx| wb.status_data(cx).usage.unwrap());
    assert_eq!(text.text, "Codex 72% · resets 3h");

    drop(view);
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "[status.usage]\nstyle = \"text\"\n");
    assert!(
        !item_drawn(&view, cx, "usage"),
        "nothing reported, nothing drawn"
    );
    give(&view, cx, summary(72.0, now() + 3 * HOUR));
    assert!(
        item_drawn(&view, cx, "usage"),
        "Settings → Status bar puts the words where the rings were"
    );
    assert!(!item_drawn(&view, cx, "usage.meters"));
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

// ---- the rings (STATUS-METERS) ----------------------------------------------------------------

fn meters(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<MeterView> {
    view.read_with(cx, |wb, cx| wb.status_data(cx).meters)
}

fn ring_bounds(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    index: usize,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(Region::UsageMeter(index)))
}

#[gpui::test]
fn the_rings_are_hidden_until_a_harness_reports_a_limit_and_follow_the_daemon_live(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    // On by default in the built-in bar, and silent while nothing is known.
    assert!(view.read_with(cx, |wb, _| {
        wb.status_bar().right.iter().any(|i| i == "usage.meters")
    }));
    assert!(meters(&view, cx).is_empty());
    assert!(!item_drawn(&view, cx, "usage.meters"), "no limit, no rings");

    // A harness reports: OMP at 7 % and Codex at 72 % are two rings, a direct chat none.
    give(&view, cx, summary(72.0, now() + 3 * HOUR));
    let rings = meters(&view, cx);
    let who: Vec<_> = rings.iter().map(MeterView::summary).collect();
    assert_eq!(who, ["OMP 5-hour (anthropic) 7%", "Codex 5-hour 72%"]);
    assert!(item_drawn(&view, cx, "usage.meters"));
    let (omp, codex) = (
        ring_bounds(&view, cx, 0).expect("OMP's ring"),
        ring_bounds(&view, cx, 1).expect("Codex's ring"),
    );
    assert!(
        ring_bounds(&view, cx, 2).is_none(),
        "a direct chat has none"
    );
    assert!(omp.origin.x < codex.origin.x, "in the daemon's order");

    // `usage.changed` redraws it: the next number is the ring's.
    give(&view, cx, summary(88.0, now() + 3 * HOUR));
    assert_eq!(meters(&view, cx)[1].percent, "88%");
    // Every window over: nothing is drawn again.
    let over = |mut s: UsageSummary| {
        for h in &mut s.harnesses {
            for l in &mut h.limits {
                l.resets_at_ms = Some(now() - 1);
            }
        }
        s
    };
    give(&view, cx, over(summary(88.0, now() + 3 * HOUR)));
    assert!(!item_drawn(&view, cx, "usage.meters"));

    // Not shown is a setting too: the Agents page keeps the numbers.
    give(&view, cx, summary(72.0, now() + 3 * HOUR));
    assert!(item_drawn(&view, cx, "usage.meters"));
    view.update(cx, |wb, cx| {
        wb.settings_store()
            .write_user("status.usage.style", toml::Value::String("off".into()))
            .unwrap();
        cx.notify();
    });
    cx.run_until_parked();
    assert!(!item_drawn(&view, cx, "usage.meters"));
    assert!(!item_drawn(&view, cx, "usage"));
}

#[gpui::test]
fn a_ring_turns_amber_at_80_and_red_at_95_and_says_its_number_from_80(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    let ring_width = |view: &Entity<Workbench>, cx: &mut VisualTestContext| {
        ring_bounds(view, cx, 1).map(|b| f32::from(b.size.width))
    };
    give(&view, cx, summary(79.0, now() + 3 * HOUR));
    let calm = meters(&view, cx)[1].clone();
    assert_eq!(
        (calm.severity, calm.says_percent()),
        (Severity::Calm, false)
    );
    let calm_width = ring_width(&view, cx).unwrap();
    give(&view, cx, summary(80.0, now() + 3 * HOUR));
    let amber = meters(&view, cx)[1].clone();
    assert_eq!(
        (amber.severity, amber.says_percent()),
        (Severity::Warning, true)
    );
    let amber_width = ring_width(&view, cx).unwrap();
    assert!(
        amber_width > calm_width + 10.0,
        "from 80 % the number is drawn beside the ring: {calm_width} → {amber_width}"
    );
    give(&view, cx, summary(95.0, now() + 3 * HOUR));
    let red = meters(&view, cx)[1].clone();
    assert_eq!(
        (red.severity, red.percent.as_str()),
        (Severity::Critical, "95%")
    );
    assert_eq!(
        meters(&view, cx)[0].severity,
        Severity::Calm,
        "each ring has its own level: OMP is at 7 %"
    );
}

#[gpui::test]
fn the_hover_card_lists_every_window_of_the_harness_with_its_account_and_reset(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    give(&view, cx, summary(72.0, now() + 3 * HOUR));
    let rings = meters(&view, cx);
    let codex = UsageCard::lines(&rings[1].card);
    assert_eq!(
        codex,
        [
            "Codex",
            "5-hour  72%  resets in 3h",
            "weekly  31%",
            "Today: 22k tokens",
            "This week: 90k tokens",
            "Click for the Agents page",
        ]
    );
    // OMP names the provider of its window and says what its turns cost, as an estimate.
    let omp = UsageCard::lines(&rings[0].card);
    assert_eq!(omp[1], "5-hour (anthropic)  7%  resets in 2h");
    assert_eq!(omp[2], "Today: 1.2k tokens · ≈ $0.42 (estimate)");

    // The card is a tooltip view: it draws nothing once an overlay moved the gate on.
    let card = cx.update(|_, app| app.new(|_| UsageCard::new(rings[1].card.clone())));
    assert!(!cx.update(|_, app| card.read(app).stale(app)));
    cx.update(|_, app| TooltipGate::bump(app));
    assert!(cx.update(|_, app| card.read(app).stale(app)));
}

#[gpui::test]
fn two_accounts_of_one_harness_are_one_ring_for_the_tightest_window_and_the_card_names_whose(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    let mut work = limit("anthropic", "5h", 82.0, Some(now() + HOUR));
    work.account = Some("work".into());
    work.account_label = Some("Work".into());
    let own = limit("anthropic", "5h", 10.0, Some(now() + HOUR));
    let usage = UsageSummary {
        generated_at_ms: now(),
        day_start_ms: now() - HOUR,
        week_start_ms: now() - 24 * HOUR,
        harnesses: vec![HarnessUsage {
            harness: "claude".into(),
            limits: vec![own, work],
            today: totals(0, 0, None),
            week: totals(0, 0, None),
        }],
        providers: vec![],
    };
    give(&view, cx, usage);
    let rings = meters(&view, cx);
    assert_eq!(rings.len(), 1, "one ring, not one per account");
    assert_eq!(rings[0].summary(), "Claude Code 5-hour · Work 82%");
    assert_eq!(rings[0].severity, Severity::Warning);
    assert_eq!(
        UsageCard::lines(&rings[0].card),
        [
            "Claude Code",
            "5-hour  10%  resets in 1h",
            "5-hour · Work  82%  resets in 1h",
            "Click for the Agents page",
        ]
    );
}

#[gpui::test]
fn a_click_on_a_ring_opens_the_agents_page_where_the_usage_section_is(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    give(&view, cx, summary(72.0, now() + 3 * HOUR));
    let page = ItemRef::panel("agents-dashboard");
    assert_ne!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(page.clone()),
        "the page is not showing yet"
    );
    let ring = ring_bounds(&view, cx, 1).expect("Codex's ring");
    common::click_at(cx, ring.center());
    wait_until(cx, "the Agents page", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state().active_center_item() == Some(page.clone())
        })
    });
    // Its Usage section is the first thing under the filter band.
    settle(cx);
    wait_until(cx, "the Usage section", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::AgentsPage("usage")).is_some()
        })
    });
}

#[gpui::test]
fn the_rings_give_way_in_a_narrow_window(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, "");
    give(&view, cx, summary(72.0, now() + 3 * HOUR));
    assert!(item_drawn(&view, cx, "usage.meters"));
    cx.simulate_resize(gpui::size(gpui::px(640.0), gpui::px(700.0)));
    draw(cx);
    assert!(
        !item_drawn(&view, cx, "usage.meters"),
        "below 700 px the bar keeps the daemon's dot and the requests"
    );
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
