#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Settings → Automation (USER-HOOKS, protocol 0.48): the page lists the user's hooks from the
//! daemon, *Run test* runs one with a sample payload and shows how it went, the log lists the
//! runs, and a daemon that has no hooks says so. The daemon is in-process on a temporary home
//! whose `daemon.toml` holds the hooks.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::common;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_protocol::hooks::HookOutcome;
use hutan_settings::{SettingsPaths, SettingsStore};

const HOOKS: &str = r#"
[[hooks]]
name = "Phone"
on = "turn.finished"
harness = "claude"
run = ["/bin/sh", "-c", "cat > /dev/null; echo sent to the phone; echo token=abcdefghijklmnopqrstuvwxyz0123456789"]
when_focused = false

[[hooks]]
on = "session.exited"
run = ["/bin/sh", "-c", "echo failing >&2; exit 3"]
"#;

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let store = common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(daemon.home())));
    let (view, cx) = open_with(cx, store, daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn open_automation(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    cx.dispatch_action(actions::OpenSettingsFor("automation".into()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("automation".to_owned())
    );
}

#[gpui::test]
fn the_automation_page_lists_the_hooks_and_runs_a_test(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start_with_config(HOOKS);
    let (view, cx) = open(cx, &daemon);
    open_automation(&view, cx);

    // The hooks as the daemon has them, nothing run yet.
    wait_until(cx, "the hooks to be listed", |cx| {
        view.read_with(cx, |wb, _| wb.hooks_page().list.is_some())
    });
    let list = view.read_with(cx, |wb, _| wb.hooks_page().list.clone().unwrap());
    assert_eq!(list.hooks.len(), 2);
    assert_eq!(list.hooks[0].name.as_deref(), Some("Phone"));
    assert_eq!(list.hooks[0].harness.as_deref(), Some("claude"));
    assert!(!list.hooks[0].when_focused);
    assert_eq!(list.hooks[1].runs, 0);
    assert!(view.read_with(cx, |wb, _| wb.hooks_page().log.is_empty()));
    draw(cx);

    // Run test of the first hook, by its button.
    wait_until(cx, "the test button to be drawn", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Settings("hook-test-first"))
                .is_some()
        })
    });
    let bounds = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::Settings("hook-test-first"))
        })
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    wait_until(cx, "the test result", |cx| {
        view.read_with(cx, |wb, _| wb.hooks_page().tested.contains_key(&0))
    });
    let tested = view.read_with(cx, |wb, _| wb.hooks_page().tested[&0].clone());
    assert_eq!(tested.run.outcome, HookOutcome::Ok);
    assert!(tested.run.test);
    assert!(tested.run.stdout.starts_with("sent to the phone\n"));
    assert!(!tested.run.stdout.contains("abcdefghijkl"), "masked");
    assert_eq!(tested.payload.harness.as_deref(), Some("claude"));
    assert!(tested.payload.test);

    // The list and the log follow: one run for the first hook, and it is in the log.
    wait_until(cx, "the log to show the test", |cx| {
        view.read_with(cx, |wb, _| wb.hooks_page().log.len() == 1)
    });
    let page = view.read_with(cx, |wb, _| {
        let page = wb.hooks_page();
        (page.list.clone().unwrap(), page.log.clone(), page.testing)
    });
    assert_eq!(page.0.hooks[0].runs, 1);
    assert_eq!(page.0.hooks[0].last_run.as_ref().unwrap().id, page.1[0].id);
    assert_eq!(page.2, None, "the test is over");
    draw(cx);

    // A hook that fails says how.
    view.update(cx, |wb, cx| wb.test_hook(1, cx));
    wait_until(cx, "the second test", |cx| {
        view.read_with(cx, |wb, _| wb.hooks_page().tested.contains_key(&1))
    });
    let failed = view.read_with(cx, |wb, _| wb.hooks_page().tested[&1].clone());
    assert_eq!(failed.run.outcome, HookOutcome::Failed);
    assert_eq!(failed.run.exit_code, Some(3));
    assert_eq!(failed.run.stderr, "failing\n");
    wait_until(cx, "the log to hold both", |cx| {
        view.read_with(cx, |wb, _| wb.hooks_page().log.len() == 2)
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.hooks_page().log[0].hook),
        1,
        "newest first"
    );
    draw(cx);
}

#[gpui::test]
fn the_page_says_so_when_there_are_no_hooks(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_automation(&view, cx);
    wait_until(cx, "the empty list", |cx| {
        view.read_with(cx, |wb, _| {
            wb.hooks_page()
                .list
                .as_ref()
                .is_some_and(|l| l.hooks.is_empty())
        })
    });
    assert!(!view.read_with(cx, |wb, _| wb.hooks_page().unsupported));
    draw(cx);
}

#[gpui::test]
fn the_page_says_hooks_load_at_start_and_opens_daemon_toml(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start_with_config(HOOKS);
    let (view, cx) = open(cx, &daemon);
    open_automation(&view, cx);
    wait_until(cx, "the hooks to be listed", |cx| {
        view.read_with(cx, |wb, _| wb.hooks_page().list.is_some())
    });
    // The note about when hooks load, with the two actions beside it.
    wait_until(cx, "Open daemon.toml and Restart daemon…", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Settings("hooks-open-daemon-toml"))
                .is_some()
                && wb
                    .region_bounds(Region::Settings("hooks-restart-daemon"))
                    .is_some()
        })
    });
    let opened = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
    let sink = opened.clone();
    view.update(cx, |wb, _| {
        wb.set_open_handler(move |path, _| sink.borrow_mut().push(path.to_owned()));
    });
    let bounds = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::Settings("hooks-open-daemon-toml"))
        })
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(*opened.borrow(), vec![daemon.home().join("daemon.toml")]);
}

#[gpui::test]
fn a_daemon_without_hooks_is_said_to_be_older(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let connection = daemon.connection_without_hooks();
    let store = common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(daemon.home())));
    let (view, cx) = open_with(cx, store, connection, None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    open_automation(&view, cx);
    wait_until(cx, "the older daemon to be noticed", |cx| {
        view.read_with(cx, |wb, _| wb.hooks_page().unsupported)
    });
    assert!(view.read_with(cx, |wb, _| wb.hooks_page().list.is_none()));
    draw(cx);
}
