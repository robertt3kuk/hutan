#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! User test round 4, part A: where the keyboard goes after a dialog closes (R4-1), the stopped
//! latch of the daemon (R4-2), the restart question following the restore policy (R4-3),
//! Escape in a dropdown (R4-4), the idle timeout's range (R4-5), the Problems footer and the
//! file the variables go to (R4-6). Daemons are in-process, or a real `hutand` where one is
//! stopped and started from outside.

use crate::common;

use std::path::Path;
use std::sync::Arc;

use common::{ProcessDaemon, TestDaemon, draw, open_options, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_client::{Connection, ConnectionState};
use hutan_desktop::integrations::{FakeInstaller, IntegrationStatus};
use hutan_desktop::workbench::actions::{self, daemon as daemon_actions};
use hutan_desktop::workbench::{Region, Workbench, WorkbenchOptions};
use hutan_protocol::messages::Request;
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::json;

fn store(dir: &Path) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(dir))
}

fn wait_synced(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, store(daemon.home()), daemon.connection(), None);
    wait_synced(&view, cx);
    (view, cx)
}

fn open_page(view: &Entity<Workbench>, cx: &mut VisualTestContext, page: &str) {
    cx.dispatch_action(actions::OpenSettingsFor(page.into()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some(page.to_owned())
    );
}

fn wait_listed(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the daemon settings", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_settings().settings().is_some())
    });
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn modal_title(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string()))
}

fn modal_message(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.modal().map(|m| m.message.to_string()))
}

fn query(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().query().to_owned())
}

// ---- R4-1: the keyboard after a dialog ---------------------------------------------------------

/// What the user did after a dialog closed: select the search text and type.
fn search_takes_the_keyboard(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    assert!(view.read_with(cx, |wb, _| wb.modal().is_none()));
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_some()));
    view.update(cx, |wb, cx| wb.set_settings_query("models", cx));
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("data");
    assert_eq!(query(view, cx), "data");
    // Escape works there again: it closes the sheet.
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_none()));
}

#[gpui::test]
fn after_clear_key_is_cancelled_or_confirmed_the_search_has_the_keyboard(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "models");

    // Cancel with the button's path.
    view.update_in(cx, |wb, window, cx| {
        wb.clear_provider_key("nobody", window, cx)
    });
    assert_eq!(
        modal_title(&view, cx).as_deref(),
        Some("Clear the API key?")
    );
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    search_takes_the_keyboard(&view, cx);

    // Cancel with Escape.
    open_page(&view, cx, "models");
    view.update_in(cx, |wb, window, cx| {
        wb.clear_provider_key("nobody", window, cx)
    });
    cx.simulate_keystrokes("escape");
    search_takes_the_keyboard(&view, cx);

    // Confirm.
    open_page(&view, cx, "models");
    view.update_in(cx, |wb, window, cx| {
        wb.clear_provider_key("nobody", window, cx)
    });
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    search_takes_the_keyboard(&view, cx);
}

#[gpui::test]
fn after_a_hooks_confirmation_the_search_has_the_keyboard(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let installer = FakeInstaller::new();
    installer.set_status("omp", IntegrationStatus::Installed);
    installer.set_status("claude", IntegrationStatus::Partial);
    installer.set_status("codex", IntegrationStatus::NotInstalled);
    let options = WorkbenchOptions::builtin(store(daemon.home()), daemon.connection(), None)
        .with_installer(Arc::new(installer.clone()));
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx);
    open_page(&view, cx, "integrations");
    wait_until(cx, "the three statuses", |cx| {
        view.read_with(cx, |wb, _| {
            ["omp", "claude", "codex"]
                .iter()
                .all(|h| wb.integration_status(h).is_some())
        })
    });

    // Uninstall, cancelled by the mouse on the dialog's Cancel path.
    click(&view, cx, Region::SettingsItem("hooks-uninstall", 0));
    assert!(modal_title(&view, cx).is_some());
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    search_takes_the_keyboard(&view, cx);

    // Install, confirmed.
    open_page(&view, cx, "integrations");
    // The sheet slides in; the click lands where it settles. The page is three sections long:
    // the last agent's row is below the fold, so the page is scrolled to it.
    common::settle(cx);
    view.update(cx, |wb, _| {
        let scroll = wb.settings_sheet().unwrap().scroll().clone();
        scroll.set_offset(gpui::point(gpui::px(0.0), gpui::px(-200.0)));
    });
    draw(cx);
    click(&view, cx, Region::SettingsItem("hooks-install", 2));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the install", |cx| {
        view.read_with(cx, |wb, _| !wb.integration_busy())
    });
    search_takes_the_keyboard(&view, cx);
}

#[gpui::test]
fn after_reset_page_and_revoke_all_the_search_has_the_keyboard(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[terminal]\nfont_size = 17\n",
    )
    .unwrap();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "terminal");

    view.update_in(cx, |wb, window, cx| wb.sheet_reset_page(window, cx));
    assert_eq!(modal_title(&view, cx).as_deref(), Some("Reset this page?"));
    cx.simulate_keystrokes("escape");
    search_takes_the_keyboard(&view, cx);

    open_page(&view, cx, "terminal");
    view.update_in(cx, |wb, window, cx| wb.sheet_reset_page(window, cx));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    search_takes_the_keyboard(&view, cx);
}

#[gpui::test]
fn after_a_daemon_dialog_the_search_has_the_keyboard(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the session", |cx| {
        view.read_with(cx, |wb, cx| wb.live_sessions(cx) == 1)
    });
    open_page(&view, cx, "data");
    wait_until(cx, "the daemon status", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_status().is_some())
    });

    // Stop asks (a session is live); Cancel.
    view.update_in(cx, |wb, window, cx| wb.stop_daemon(window, cx));
    assert_eq!(modal_title(&view, cx).as_deref(), Some("Stop the daemon?"));
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    search_takes_the_keyboard(&view, cx);

    // Restart asks too.
    open_page(&view, cx, "data");
    view.update_in(cx, |wb, window, cx| wb.restart_daemon(window, cx));
    assert_eq!(
        modal_title(&view, cx).as_deref(),
        Some("Restart the daemon?")
    );
    cx.simulate_keystrokes("escape");
    search_takes_the_keyboard(&view, cx);
}

#[gpui::test]
fn a_dialog_over_a_terminal_gives_the_terminal_its_keyboard_back(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the session", |cx| {
        view.read_with(cx, |wb, cx| wb.live_sessions(cx) == 1)
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    draw(cx);
    let before = cx.update(|window, cx| window.focused(cx)).expect("a focus");

    view.update_in(cx, |wb, window, cx| {
        wb.clear_provider_key("nobody", window, cx)
    });
    draw(cx);
    assert_ne!(
        cx.update(|window, cx| window.focused(cx)),
        Some(before.clone())
    );
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    draw(cx);
    assert_eq!(cx.update(|window, cx| window.focused(cx)), Some(before));
}

// ---- R4-2: the stopped latch --------------------------------------------------------------------

fn status_line(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, cx| wb.daemon_status_line(cx))
}

fn connected(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).connection_state() == ConnectionState::Connected
    })
}

fn screen_of(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    id: hutan_core::SessionId,
) -> String {
    view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(id)
            .map(|v| common::screen_text(&v.read(cx).model()))
            .unwrap_or_default()
    })
}

#[gpui::test]
fn a_daemon_started_from_outside_ends_the_stopped_state_and_terminals_take_input(
    cx: &mut TestAppContext,
) {
    let process = ProcessDaemon::new();
    process.create_session(&["true"]);
    let connection = Connection::start(process.options());
    let (view, cx) = open_with(
        cx,
        store(&process.dir.path().join("config")),
        connection,
        None,
    );
    wait_until(cx, "the connection", |cx| connected(&view, cx));

    // Stop from the desktop: it says so and stays down.
    cx.dispatch_action(daemon_actions::Stop);
    cx.run_until_parked();
    wait_until(cx, "the daemon to go", |_| !process.socket().exists());
    wait_until(cx, "the stopped state", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_busy().is_none() && wb.daemon_stopped()
        })
    });
    assert_eq!(status_line(&view, cx), "Stopped by you · Start");

    // Somebody else starts it (the CLI, autostart, another client): a request on a fresh
    // connection starts `hutand` the way `hutan daemon start` does.
    let _: serde_json::Value =
        process.call(Request::DaemonStatus(hutan_protocol::messages::Empty {}));
    wait_until(cx, "the window to leave the stopped state", |cx| {
        connected(&view, cx) && view.read_with(cx, |wb, _| !wb.daemon_stopped())
    });
    assert_eq!(status_line(&view, cx), "hutand connected");

    // A new terminal accepts typed input.
    let echo = process.create_session(&["cat"]);
    wait_until(cx, "the session", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(echo.id).is_some()
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(echo.id, window, cx));
    wait_until(cx, "the cat session to attach", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(echo.id)
                .is_some_and(|v| v.read(cx).model().is_ready())
        })
    });
    cx.simulate_input("r4-marker\n");
    wait_until(cx, "the marker on the screen", |cx| {
        screen_of(&view, cx, echo.id).contains("r4-marker")
    });
}

#[gpui::test]
fn start_while_the_daemon_runs_clears_the_stopped_state(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    wait_until(cx, "the cat session to attach", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .is_some_and(|v| v.read(cx).model().is_ready())
        })
    });

    // The latch is set while the daemon is up (what a stop that came back by another path
    // would have left behind): the terminal refuses input.
    let sessions = view.read_with(cx, |wb, _| wb.sessions().clone());
    sessions.update(cx, |store, cx| store.set_daemon_stopped(true, cx));
    wait_until(cx, "the terminal to say stopped", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id).is_some_and(|v| {
                matches!(v.read(cx).status(), hutan_terminal_view::ViewStatus::Ended(t) if t.contains("stopped"))
            })
        })
    });

    // Start finds the daemon running: it clears the state instead of only saying so.
    cx.dispatch_action(daemon_actions::Start);
    cx.run_until_parked();
    assert!(!sessions.read_with(cx, |store, _| store.daemon_stopped()));
    assert!(!view.read_with(cx, |wb, _| wb.daemon_stopped()));
    wait_until(cx, "the terminal to be live", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .is_some_and(|v| *v.read(cx).status() == hutan_terminal_view::ViewStatus::Live)
        })
    });
    cx.simulate_input("typed-after-start\n");
    wait_until(cx, "the text on the screen", |cx| {
        screen_of(&view, cx, info.id).contains("typed-after-start")
    });
}

// ---- R4-3: the restart question follows the restore policy ---------------------------------------

#[gpui::test]
fn the_restart_question_follows_the_restore_policy(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_listed(&view, cx);
    wait_until(cx, "the session", |cx| {
        view.read_with(cx, |wb, cx| wb.live_sessions(cx) == 1)
    });

    view.update_in(cx, |wb, window, cx| wb.restart_daemon(window, cx));
    let respawn = modal_message(&view, cx).unwrap();
    assert!(
        respawn.contains("The new daemon restores them"),
        "{respawn}"
    );
    assert!(respawn.contains("1 live session"), "{respawn}");
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));

    view.update(cx, |wb, cx| {
        wb.daemon_choose("daemon.restore_sessions", "stopped", cx)
    });
    wait_until(cx, "the policy", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_settings().value("daemon.restore_sessions") == Some(&json!("stopped"))
        })
    });
    view.update_in(cx, |wb, window, cx| wb.restart_daemon(window, cx));
    let stopped = modal_message(&view, cx).unwrap();
    assert!(!stopped.contains("restores them"), "{stopped}");
    assert!(
        stopped.contains("The new daemon lists them stopped")
            && stopped.contains("“Leave them stopped”")
            && stopped.contains("Restart action"),
        "{stopped}"
    );
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
}

// ---- R4-4: Escape in a dropdown -------------------------------------------------------------------

#[gpui::test]
fn escape_closes_an_open_dropdown_first_and_only_a_second_escape_closes_the_sheet(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    wait_listed(&view, cx);

    // The Keyboard page's dropdown, opened with the mouse, moved with Down and Up.
    open_page(&view, cx, "keymap");
    let before = view.read_with(cx, |wb, _| wb.sheet_value("base"));
    click(&view, cx, Region::Settings("dropdown"));
    assert!(view.read_with(cx, |wb, _| {
        wb.settings_sheet().unwrap().dropdown() == Some("base")
    }));
    cx.simulate_keystrokes("down up");
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| {
        wb.settings_sheet().unwrap().dropdown().is_none()
    }));
    assert_eq!(view.read_with(cx, |wb, _| wb.sheet_value("base")), before);
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_some()));
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_none()));

    // The same after a dialog was cancelled: the keyboard is in the sheet, so Escape reaches
    // the dropdown rather than nothing.
    open_page(&view, cx, "keymap");
    view.update_in(cx, |wb, window, cx| {
        wb.clear_provider_key("nobody", window, cx)
    });
    cx.simulate_keystrokes("escape");
    click(&view, cx, Region::Settings("dropdown"));
    assert!(view.read_with(cx, |wb, _| {
        wb.settings_sheet().unwrap().dropdown().is_some()
    }));
    cx.simulate_keystrokes("down escape");
    assert!(view.read_with(cx, |wb, _| {
        wb.settings_sheet().unwrap().dropdown().is_none()
    }));
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_some()));
    assert_eq!(view.read_with(cx, |wb, _| wb.sheet_value("base")), before);
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_none()));

    // A daemon page's dropdown.
    open_page(&view, cx, "control");
    wait_listed(&view, cx);
    click(&view, cx, Region::Settings("ddropdown"));
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .unwrap()
            .dropdown()
            .map(str::to_owned)),
        Some("daemon:control.agents".to_owned())
    );
    cx.simulate_keystrokes("down escape");
    assert!(view.read_with(cx, |wb, _| {
        wb.settings_sheet().unwrap().dropdown().is_none()
    }));
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .daemon_settings()
            .value("control.agents")
            .cloned()),
        Some(json!("ask"))
    );
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_none()));
}

// ---- R4-5: the idle timeout's rule -----------------------------------------------------------------

fn edit_text(view: &Entity<Workbench>, cx: &mut VisualTestContext, key: &str, text: &str) {
    view.update(cx, |wb, cx| wb.daemon_begin_edit(key, cx));
    view.update(cx, |wb, cx| wb.sheet_set_edit_text(text, cx));
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
}

fn error_of(view: &Entity<Workbench>, cx: &VisualTestContext, key: &str) -> Option<String> {
    view.read_with(cx, |wb, _| {
        wb.daemon_settings().error(key).map(str::to_owned)
    })
}

#[gpui::test]
fn the_idle_timeout_states_its_real_rule_and_a_refused_number_stays_in_its_box(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "assistant");
    wait_listed(&view, cx);
    let key = "assistant.idle_stop_secs";

    // The rule is 0 (never) or at least 60, not "at least 0".
    assert_eq!(
        view.read_with(cx, |wb, _| wb.daemon_number_hint(key))
            .as_deref(),
        Some("0 (never) or at least 60")
    );
    // A number whose minimum says it all keeps the range line it had.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.daemon_number_hint("notify.min_turn_secs"))
            .as_deref(),
        Some("at least 0")
    );

    // 1 is refused by the daemon, with the reason at the field; the box keeps what was typed.
    edit_text(&view, cx, key, "1");
    wait_until(cx, "the refusal", |cx| error_of(&view, cx, key).is_some());
    let error = error_of(&view, cx, key).unwrap();
    assert!(error.contains("0 (never) or at least 60"), "{error}");
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .daemon_settings()
            .refused_text(key)
            .map(str::to_owned)),
        Some("1".to_owned())
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .unwrap()
            .edit()
            .map(|e| e.text.clone())),
        Some("1".to_owned())
    );
    // The setting itself is untouched.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.daemon_settings().value(key).cloned()),
        Some(json!(600))
    );

    // Leaving the box does not snap it back: it still shows 1 and the reason, and editing it
    // again starts from what was typed.
    view.update(cx, |wb, cx| wb.sheet_tab(false, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .daemon_settings()
            .refused_text(key)
            .map(str::to_owned)),
        Some("1".to_owned())
    );
    view.update(cx, |wb, cx| wb.daemon_begin_edit(key, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .unwrap()
            .edit()
            .map(|e| e.text.clone())),
        Some("1".to_owned())
    );
    assert!(
        error_of(&view, cx, key).is_some(),
        "the reason stays while it is corrected"
    );

    // Correcting it clears both.
    edit_text(&view, cx, key, "60");
    wait_until(cx, "the value", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_settings().value(key) == Some(&json!(60))
        })
    });
    assert!(error_of(&view, cx, key).is_none());
    assert!(view.read_with(cx, |wb, _| wb.daemon_settings().refused_text(key).is_none()));

    // Escape gives a refused change up: the box shows the setting again.
    edit_text(&view, cx, key, "7");
    wait_until(cx, "the refusal", |cx| error_of(&view, cx, key).is_some());
    cx.simulate_keystrokes("escape");
    assert!(error_of(&view, cx, key).is_none());
    assert!(view.read_with(cx, |wb, _| wb.daemon_settings().refused_text(key).is_none()));
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_some()));
}

// ---- R4-6: the Problems footer ----------------------------------------------------------------------

#[gpui::test]
fn the_problems_footer_counts_what_the_daemons_settings_say(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "notifications");
    wait_listed(&view, cx);
    let problems = |view: &Entity<Workbench>, cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| wb.sheet_problems())
    };
    assert!(problems(&view, cx).is_empty());

    // A refusal at a field of the page.
    edit_text(&view, cx, "notify.min_turn_secs", "-5");
    wait_until(cx, "the refusal", |cx| {
        error_of(&view, cx, "notify.min_turn_secs").is_some()
    });
    let lines = problems(&view, cx);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("at least 0"), "{lines:?}");
    // Not on a page that does not show the field.
    open_page(&view, cx, "appearance");
    assert!(problems(&view, cx).is_empty());
    open_page(&view, cx, "notifications");
    assert_eq!(problems(&view, cx).len(), 1);
    cx.simulate_keystrokes("escape");

    // A daemon.toml that cannot be read, on a page that shows daemon settings.
    open_page(&view, cx, "control");
    std::fs::write(daemon.home().join("daemon.toml"), "[notify\nenabled = \n").unwrap();
    view.update(cx, |wb, cx| wb.refresh_daemon_settings(cx));
    wait_until(cx, "the problem", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_settings().problem().is_some())
    });
    let lines = problems(&view, cx);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("daemon.toml"), "{lines:?}");
    // …and says the same words as the panel above it.
    assert_eq!(
        Some(lines[0].as_str()),
        view.read_with(cx, |wb, _| wb
            .daemon_settings()
            .problem()
            .map(str::to_owned))
            .as_deref()
    );
    open_page(&view, cx, "appearance");
    assert!(problems(&view, cx).is_empty());
}

// ---- R4-7: provider errors ---------------------------------------------------------------------------

#[gpui::test]
fn an_unreachable_provider_is_explained_plainly_with_details_apart(cx: &mut TestAppContext) {
    use hutan_protocol::provider::{ProviderAdd, ProviderResult};
    let daemon = TestDaemon::start_with_gateway(hutan_daemon::GatewayConfig {
        credentials: Some(Arc::new(hutan_gateway::MemoryStore::new())),
        http: hutan_gateway::HttpConfig {
            no_proxy: true,
            ..Default::default()
        },
        discovery: Some(Vec::new()),
    });
    let added: ProviderResult = daemon.call(Request::ProviderAdd(ProviderAdd {
        kind: "openai-compatible".into(),
        name: "r4-local".into(),
        base_url: Some("http://127.0.0.1:9".into()),
        default_model: None,
    }));
    let id = added.provider.id.clone();
    let url = added.provider.base_url.clone();
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_models_settings(window, cx));
    wait_until(cx, "the provider list", |cx| {
        view.read_with(cx, |wb, _| wb.providers().iter().any(|p| p.id == id))
    });

    view.update(cx, |wb, cx| wb.provider_models(&id, cx));
    wait_until(cx, "the failure", |cx| {
        view.read_with(cx, |wb, _| wb.provider_failure().is_some())
    });
    let failure = view.read_with(cx, |wb, _| wb.provider_failure().cloned().unwrap());
    assert_eq!(
        failure.summary,
        format!(
            "Could not connect to r4-local at {url}. Check that the server is running and the address is right."
        )
    );
    assert!(!failure.summary.contains("attempt"), "{failure:?}");
    assert!(
        failure.details.contains("attempt") && failure.details.contains("ProviderError"),
        "{failure:?}"
    );
    // The footer counts it, with the plain first line.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_problems()),
        vec![failure.summary.clone()]
    );
}
