#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The History panel's states and settings (M17-B) that never start a session: the disabled
//! card, the `history` settings namespace as the filters' defaults, ⌘J `?` while indexing is
//! off, the panel in another dock, and Clear's confirmation. Resume lives in `history_gpui.rs`
//! (it needs stand-in agents on `PATH`).

use crate::common;

use std::time::Duration;

use common::history::HistoryEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::settings::{HistoryRange, HistoryScope};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::{DockPosition, PanelDest, PanelId};
use hutan_desktop::workbench::panels::{HistoryConfirm, HistoryPanel};
use hutan_desktop::workbench::{HistoryState, Region, Workbench};
use hutan_protocol::history::{HistoryRole, HistorySource};
use hutan_settings::{SettingsPaths, SettingsStore};

fn pump_until(
    cx: &mut VisualTestContext,
    what: &str,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    wait_until(cx, what, |cx| {
        cx.executor().advance_clock(Duration::from_millis(100));
        condition(cx)
    });
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn show_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<HistoryPanel> {
    cx.dispatch_action(actions::OpenPanel("history".into()));
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<HistoryPanel>("history"))
        .expect("the History panel is drawn");
    pump_until(cx, "the index status", |cx| {
        panel.read_with(cx, |p, _| p.status().is_some())
    });
    panel
}

#[gpui::test]
fn indexing_off_shows_the_card_and_reads_nothing(cx: &mut TestAppContext) {
    let env = HistoryEnv::new();
    env.write_fixtures();
    let daemon = TestDaemon::start_with_config(&env.daemon_toml(false));
    let (view, cx) = open(cx, &daemon);
    let panel = show_panel(&view, cx);
    assert!(!panel.read_with(cx, |p, _| p.status().unwrap().enabled));
    // The card is short: one quiet button, no form.
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::History("setup")))
            .is_some()
    );
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::History("enable")))
            .is_none()
    );
    let setup = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::History("setup")))
        .unwrap();
    cx.simulate_click(setup.center(), Modifiers::none());
    // Set up history… offers the button and the link to the settings.
    for region in ["enable", "settings"] {
        draw(cx);
        assert!(
            view.read_with(cx, |wb, _| wb.region_bounds(Region::History(region)))
                .is_some(),
            "{region} is on the card"
        );
    }
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::History("search")))
            .is_none(),
        "no search field while indexing is off"
    );
    assert!(!daemon.paths.index_database.exists());

    // The link opens the `history` settings.
    let link = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::History("settings")))
        .unwrap();
    cx.simulate_click(link.center(), Modifiers::none());
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("history".to_owned())
    );
    cx.simulate_keystrokes("escape");

    // ⌘J `?` says why there is nothing, and Enter does nothing.
    cx.simulate_keystrokes("cmd-p");
    cx.simulate_input("?hutantest");
    pump_until(cx, "the answer", |cx| {
        view.read_with(cx, |wb, _| {
            wb.picker()
                .is_some_and(|p| p.history_state == HistoryState::Disabled)
        })
    });
    assert!(view.read_with(cx, |wb, _| wb.picker_titles().is_empty()));
    cx.simulate_keystrokes("enter");
    assert!(!daemon.paths.index_database.exists());
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.picker().is_none()));
}

#[gpui::test]
fn the_history_namespace_sets_the_default_filters(cx: &mut TestAppContext) {
    let env = HistoryEnv::new();
    let daemon = TestDaemon::start_with_config(&env.daemon_toml(false));
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[history]\nsources = [\"omp\", \"codex\", \"nonsense\"]\nroles = [\"user\"]\nrange = \"7d\"\nproject = \"workspace\"\nshow_snippets = false\n",
    )
    .unwrap();
    let (view, cx) = open(cx, &daemon);
    let panel = show_panel(&view, cx);
    let filters = panel.read_with(cx, |p, _| p.filters().clone());
    assert_eq!(filters.sources, [HistorySource::Codex, HistorySource::Omp]);
    assert_eq!(filters.roles, [HistoryRole::User]);
    assert_eq!(filters.range, HistoryRange::Week);
    assert_eq!(filters.scope, HistoryScope::Workspace);
    assert!(filters.narrows());
    // The segmented control starts where the settings say.
    assert_eq!(
        panel.read_with(cx, |p, _| p.scope()),
        hutan_protocol::history::HistoryScope::Workspace
    );

    // Defaults when the file says nothing.
    let quiet = TestDaemon::start_with_config(&env.daemon_toml(false));
    let (view, cx) = open(cx, &quiet);
    let panel = show_panel(&view, cx);
    assert!(!panel.read_with(cx, |p, _| p.filters().narrows()));
}

#[gpui::test]
fn the_panel_moves_like_any_other_and_clear_asks_first(cx: &mut TestAppContext) {
    let env = HistoryEnv::new();
    env.write_fixtures();
    let daemon = TestDaemon::start_with_config(&env.daemon_toml(true));
    let (view, cx) = open(cx, &daemon);

    // Right dock instead of the left.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.move_panel(
                &PanelId::new("history"),
                PanelDest::Dock(DockPosition::Right),
            )
            .unwrap();
            s.open_panel(&PanelId::new("history"), DockPosition::Right);
        });
    });
    let panel = show_panel(&view, cx);
    assert!(view.read_with(cx, |wb, _| {
        wb.state()
            .docks
            .right
            .panels
            .contains(&PanelId::new("history"))
    }));
    pump_until(cx, "the transcripts to be indexed", |cx| {
        panel.read_with(cx, |p, _| {
            p.status()
                .is_some_and(|s| s.enabled && s.sources.iter().map(|s| s.rows).sum::<u64>() >= 10)
        })
    });

    // Clear asks; Escape-style cancelling leaves the index alone; the confirmation deletes it.
    // The controls are behind the scan strip.
    draw(cx);
    let scan = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::History("scan")))
        .expect("the scan strip is drawn");
    cx.simulate_click(scan.center(), Modifiers::none());
    assert!(panel.read_with(cx, |p, _| p.scan_open()));
    draw(cx);
    let clear = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::History("clear")))
        .expect("Clear is in the footer");
    cx.simulate_click(clear.center(), Modifiers::none());
    assert_eq!(
        panel.read_with(cx, |p, _| p.pending_confirmation()),
        Some(HistoryConfirm::Clear)
    );
    panel.update(cx, |p, cx| p.cancel_confirmation(cx));
    assert_eq!(panel.read_with(cx, |p, _| p.pending_confirmation()), None);
    let rows = |cx: &mut VisualTestContext| {
        panel.read_with(cx, |p, _| {
            p.status()
                .map_or(0, |s| s.sources.iter().map(|s| s.rows).sum::<u64>())
        })
    };
    assert!(rows(cx) >= 10);
    panel.update(cx, |p, cx| p.ask(HistoryConfirm::Clear, cx));
    panel.update(cx, |p, cx| p.confirmed(cx));
    pump_until(cx, "the index to be empty", |cx| rows(cx) == 0);
}

/// *Search history…* on a fresh window makes the History panel, which has yet to read the index
/// state from the daemon: the keys typed right after it go to its search field, not to the
/// terminal that had the keyboard.
#[gpui::test]
fn typing_right_after_search_history_lands_in_the_history_field(cx: &mut TestAppContext) {
    let env = HistoryEnv::new();
    let daemon = TestDaemon::start_with_config(&env.daemon_toml(true));
    let cat = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_session(cat.id, window, cx));
    let terminal = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(cat.id)
                .map(|v| gpui::Focusable::focus_handle(v.read(cx), cx))
        })
    };
    wait_until(cx, "the terminal to have the keyboard", |cx| {
        terminal(cx).is_some_and(|f| cx.update(|window, _| f.is_focused(window)))
    });
    assert!(
        view.read_with(cx, |wb, _| wb.panel::<HistoryPanel>("history"))
            .is_none(),
        "the History panel is made by Search history…"
    );

    cx.dispatch_action(actions::SearchHistory);
    // One frame, as a window draws before the next key arrives.
    draw(cx);
    cx.simulate_input("axolotl");

    let panel = view
        .read_with(cx, |wb, _| wb.panel::<HistoryPanel>("history"))
        .expect("the History panel is drawn");
    assert_eq!(panel.read_with(cx, |p, _| p.query().to_owned()), "axolotl");
    let field = panel.read_with(cx, |p, cx| gpui::Focusable::focus_handle(p, cx));
    let terminal = terminal(cx).expect("the terminal view is still there");
    cx.update(|window, _| {
        assert!(
            field.is_focused(window),
            "the History field has the keyboard"
        );
        assert!(!terminal.is_focused(window));
    });
}

/// Refreshes asked while `history.status` is in flight do not cancel it: the answer already on
/// its way lands, so the index state becomes known however often the panel is asked to refresh.
#[gpui::test]
fn refreshes_asked_while_the_status_is_read_do_not_cancel_it(cx: &mut TestAppContext) {
    let env = HistoryEnv::new();
    let daemon = TestDaemon::start_with_config(&env.daemon_toml(true));
    let (connection, hold) = daemon.held_connection();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        connection,
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    // The new panel reads the status; the daemon answers, and the answer is held on its way.
    hold.answers(true);
    cx.dispatch_action(actions::OpenPanel("history".into()));
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<HistoryPanel>("history"))
        .expect("the History panel is drawn");
    wait_until(cx, "the status answer on its way", |_| {
        hold.holds_answer_with("\"db_bytes\"")
    });
    assert!(panel.read_with(cx, |p, _| p.status().is_none()));

    // Three rapid refreshes while it is in flight; whatever they would send is held as well.
    hold.requests(true);
    for _ in 0..3 {
        panel.update(cx, |p, cx| p.refresh_status(cx));
        draw(cx);
    }
    hold.answers(false);
    pump_until(cx, "the index state", |cx| {
        panel.read_with(cx, |p, _| p.status().is_some())
    });
    assert!(panel.read_with(cx, |p, _| p.status().unwrap().enabled));
}
