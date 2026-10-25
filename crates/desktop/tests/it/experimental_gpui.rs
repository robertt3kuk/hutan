#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Experimental page of Settings (R-2): "Remote hosts over SSH (experimental)" is off by
//! default, sits in its own group of the page list, toggles in the user's `settings.toml` (the
//! file the CLI reads), and carries the disclaimer and the link to the docs.

use crate::common;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::settings_labels::{field_title, namespace_title};
use hutan_desktop::workbench::settings_schema::NavGroup;
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_settings::experimental::REMOTE_HOSTS_DISCLAIMER;
use hutan_settings::{ExperimentalSettings, SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let store = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let (view, cx) = open_with(cx, store, daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn user_file(daemon: &TestDaemon) -> String {
    std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap_or_default()
}

fn remote_hosts_on(daemon: &TestDaemon) -> bool {
    ExperimentalSettings::load(&SettingsPaths::in_dir(daemon.home()))
        .unwrap()
        .remote_hosts
}

#[gpui::test]
fn remote_hosts_are_an_experimental_toggle_with_a_disclaimer(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);

    // Its own group of the page list, shown (not folded like Advanced).
    cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
    draw(cx);
    let nav = view.read_with(cx, |wb, _| wb.sheet_nav());
    let group = nav
        .iter()
        .find(|(g, _, _)| *g == NavGroup::Experimental)
        .expect("an Experimental group");
    assert!(group.1, "the group is open");
    assert_eq!(group.2, ["experimental"]);
    assert_eq!(namespace_title("experimental"), "Experimental features");

    // Search finds it by what a user would type.
    view.update(cx, |wb, cx| wb.set_settings_query("--host", cx));
    assert!(
        view.read_with(cx, |wb, _| wb.sheet_pages().contains(&"experimental")),
        "searching for --host finds the page"
    );
    view.update(cx, |wb, cx| wb.set_settings_query("", cx));

    cx.dispatch_action(actions::OpenSettingsFor("experimental".into()));
    draw(cx);
    assert_eq!(
        field_title("experimental", "remote_hosts"),
        "Remote hosts over SSH (experimental)"
    );

    // Off by default; the disclaimer is under the row, with the link to the docs.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_value("remote_hosts")),
        Some(serde_json::json!(false))
    );
    assert!(!remote_hosts_on(&daemon));
    for needle in [
        "experimental",
        "may change or break between releases",
        "same control",
        "Use at your own risk",
    ] {
        assert!(REMOTE_HOSTS_DISCLAIMER.contains(needle), "{needle}");
    }
    for region in ["experimental-disclaimer", "experimental-docs"] {
        wait_until(cx, region, |cx| {
            draw(cx);
            view.read_with(cx, |wb, _| {
                wb.region_bounds(Region::Settings(region)).is_some()
            })
        });
    }

    // The toggle writes the user's settings.toml, which is where the CLI reads it.
    view.update(cx, |wb, cx| wb.sheet_toggle("remote_hosts", cx));
    assert!(
        user_file(&daemon).contains("[experimental]")
            && user_file(&daemon).contains("remote_hosts = true"),
        "{}",
        user_file(&daemon)
    );
    assert!(remote_hosts_on(&daemon));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_value("remote_hosts")),
        Some(serde_json::json!(true))
    );
    view.update(cx, |wb, cx| wb.sheet_toggle("remote_hosts", cx));
    assert!(!remote_hosts_on(&daemon));
}
