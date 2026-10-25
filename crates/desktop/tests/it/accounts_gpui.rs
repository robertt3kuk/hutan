#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers
//! Several accounts per harness in the desktop (protocol 0.50, ADR 0015 § Amendment): the
//! accounts of a harness on the Accounts page (default, remove with its confirmation, add with
//! the harness's own login in a terminal), the Account row of the Start sheet, and the account
//! label of a chat's header. The daemon runs in this process on a temporary home; the
//! harnesses' CLIs are shell scripts and OMP's ACP agent is `hutan-acp-fixture replay`. No real
//! harness, login or token is touched.

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::chat::who_runs;
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::{LaunchAgent, Models, Region, Workbench};
use hutan_protocol::account::{AccountAdd, AccountDefaultSet, ManagedAccount};
use hutan_protocol::messages::Request;
use hutan_settings::{SettingsPaths, SettingsStore};

fn acp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(bin.is_file(), "run `cargo build -p hutan-agents --bins`");
    bin
}

fn transcript(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"))
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// OMP replays the plain turn and records `PI_CODING_AGENT_DIR` per start in `omp-starts`;
/// Claude Code's CLI says logged in whenever an account's home is set, and logs what it is
/// asked to log out of in `claude-logout`.
fn daemon(scripts: &Path) -> TestDaemon {
    let starts = scripts.join("omp-starts");
    let omp = script(
        scripts,
        "omp",
        r#"case "$1" in
  models) echo '{"models":[{"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","selector":"anthropic/claude-opus-4-5","name":"Claude Opus 4.5"}]}' ;;
  usage) echo '{"reports":[]}' ;;
  *) echo "fake omp $*" ;;
esac"#,
    );
    let acp = script(
        scripts,
        "omp-acp",
        &format!(
            "echo \"$PI_CODING_AGENT_DIR\" >> {starts:?}\nexec {:?} \"$@\"",
            acp_bin()
        ),
    );
    let logout = scripts.join("claude-logout");
    let claude = script(
        scripts,
        "claude",
        &format!(
            r#"case "$1 $2" in
  "auth status")
    if [ -n "$CLAUDE_CONFIG_DIR" ]; then echo '{{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max"}}'; else echo '{{"loggedIn":false,"authMethod":"none"}}'; fi ;;
  "auth logout") echo "$CLAUDE_CONFIG_DIR" >> {logout:?} ;;
  *) echo "fake claude $*" ;;
esac"#
        ),
    );
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {acp:?}\nargs = [\"replay\", {t:?}, \"acp\"]\ncli = {omp:?}\n\n[harness.claude]\nbin = {missing:?}\ncli = {claude:?}\n",
        t = transcript("plain-turn"),
        missing = scripts.join("no-such-claude-code-acp"),
    ))
}

fn add(daemon: &TestDaemon, harness: &str, name: &str, label: &str) -> ManagedAccount {
    daemon.call(Request::AccountAdd(AccountAdd {
        harness: harness.into(),
        name: name.into(),
        label: Some(label.into()),
    }))
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(daemon.home()))),
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

fn open_accounts(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    view.update_in(cx, |wb, window, cx| wb.open_accounts(window, cx));
    wait_until(cx, "the accounts", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.harness_directory().read(cx).listing() == &hutan_desktop::workbench::Listing::Ready
        })
    });
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

/// Scrolls the page of the sheet until `region` is well inside it (see `mcp_gpui.rs`).
fn reveal(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    let size = cx.update(|window, _| window.viewport_size());
    let at = gpui::point(size.width / 2.0, gpui::px(300.0));
    let (band_top, band_bottom) = (gpui::px(150.0), gpui::px(470.0));
    for _ in 0..200 {
        let bounds = view
            .read_with(cx, |wb, _| wb.region_bounds(region))
            .unwrap_or_else(|| panic!("{region:?} is drawn"));
        let delta = if bounds.top() < band_top {
            40.0
        } else if bounds.bottom() > band_bottom {
            -40.0
        } else {
            return;
        };
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: at,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(delta))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        draw(cx);
    }
    panic!("{region:?} never came into view");
}

fn click_in_sheet(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    reveal(view, cx, region);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
}

#[gpui::test]
fn the_accounts_page_lists_each_harnesss_accounts_sets_a_default_and_removes_after_asking(
    cx: &mut TestAppContext,
) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path());
    let work = add(&daemon, "claude", "work", "Work");
    let (view, cx) = open(cx, &daemon);
    open_accounts(&view, cx);
    view.read_with(cx, |wb, cx| {
        let directory = wb.harness_directory().read(cx);
        let managed: Vec<_> = directory.managed_of("claude").collect();
        assert_eq!(managed.len(), 1);
        assert_eq!(managed[0].label, "Work");
        // What the harness says with the account's home set, not what the own login says.
        assert_eq!(
            managed[0].logins[0].status,
            hutan_protocol::account::AccountStatus::LoggedIn
        );
        assert_eq!(directory.default_account("claude"), None);
    });
    assert!(drawn(&view, cx, Region::SettingsItem("account-login", 0)));
    assert!(drawn(&view, cx, Region::SettingsItem("account-default", 0)));
    assert!(drawn(&view, cx, Region::SettingsItem("account-remove", 0)));
    assert!(drawn(&view, cx, Region::Account("claude", "add")));

    // Set as default: the daemon keeps it and the page lists it again.
    click_in_sheet(&view, cx, Region::SettingsItem("account-default", 0));
    wait_until(cx, "the default", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.harness_directory().read(cx).default_account("claude") == Some("work")
        })
    });
    // Clear default (the same button) takes it back.
    click_in_sheet(&view, cx, Region::SettingsItem("account-default", 0));
    wait_until(cx, "the default cleared", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.harness_directory()
                .read(cx)
                .default_account("claude")
                .is_none()
        })
    });

    // Remove asks first; Cancel keeps the account and its folder.
    click_in_sheet(&view, cx, Region::SettingsItem("account-remove", 0));
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));
    let title = view.read_with(cx, |wb, _| wb.modal().unwrap().title.to_string());
    assert!(title.contains("Work"), "{title}");
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    assert!(Path::new(&work.home).is_dir());
    // Confirm: the harness logs out with the account's home, then the folder is deleted.
    click_in_sheet(&view, cx, Region::SettingsItem("account-remove", 0));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the account to be removed", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.harness_directory()
                .read(cx)
                .managed_of("claude")
                .next()
                .is_none()
        })
    });
    assert!(!Path::new(&work.home).exists());
    let logged_out = std::fs::read_to_string(scripts.path().join("claude-logout")).unwrap();
    assert_eq!(logged_out.trim(), work.home);
}

#[gpui::test]
fn add_account_creates_the_home_and_opens_the_harnesss_login_in_a_terminal(
    cx: &mut TestAppContext,
) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path());
    let (view, cx) = open(cx, &daemon);
    open_accounts(&view, cx);
    click_in_sheet(&view, cx, Region::Account("claude", "add"));
    assert!(view.read_with(cx, |wb, _| wb.account_form().is_some()));
    // A name is needed; the form says so and stays.
    click_in_sheet(&view, cx, Region::Settings("account-save"));
    let error = view.read_with(cx, |wb, _| wb.account_form().unwrap().error.clone());
    assert!(error.unwrap().contains("name"));
    // Name, Return, label, Return: the account is added and its login starts in a terminal.
    click_in_sheet(&view, cx, Region::SettingsItem("account-field", 0));
    view.update(cx, |wb, cx| wb.sheet_set_edit_text("personal", cx));
    cx.simulate_keystrokes("enter");
    view.update(cx, |wb, cx| wb.sheet_set_edit_text("Personal", cx));
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the login terminal of the new account", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.settings_sheet().is_none()
                && wb.state().active_item().is_some_and(|item| match item {
                    ItemRef::Terminal { session } => session
                        .parse()
                        .ok()
                        .and_then(|id| wb.sessions().read(cx).session(id).cloned())
                        .is_some_and(|s| s.title == "Log in to Claude Code (personal)"),
                    _ => false,
                })
        })
    });
    let home = daemon.home().join("accounts/claude/personal");
    assert!(home.is_dir(), "the daemon created the account's home");
    // The daemon refuses a bad name inside the form.
    open_accounts(&view, cx);
    click_in_sheet(&view, cx, Region::Account("claude", "add"));
    click_in_sheet(&view, cx, Region::SettingsItem("account-field", 0));
    view.update(cx, |wb, cx| wb.sheet_set_edit_text("Bad Name", cx));
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the daemon's refusal", |cx| {
        view.read_with(cx, |wb, _| {
            wb.account_form()
                .is_some_and(|f| f.error.is_some() && !f.saving)
        })
    });
    click_in_sheet(&view, cx, Region::Settings("account-cancel"));
    assert!(view.read_with(cx, |wb, _| wb.account_form().is_none()));
}

#[gpui::test]
fn the_start_sheet_offers_the_account_only_where_a_harness_has_one_and_the_chat_runs_as_it(
    cx: &mut TestAppContext,
) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path());
    let (view, cx) = open(cx, &daemon);
    // Nothing to choose from: no Account row.
    cx.simulate_keystrokes("cmd-shift-t");
    view.update(cx, |wb, cx| {
        wb.launcher_set_kind(hutan_desktop::sessions::launch::LaunchKind::Chat, cx);
    });
    wait_until(cx, "OMP and its models", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher()
                .is_some_and(|l| l.agent() == &LaunchAgent::Harness("omp".into()))
                && matches!(wb.launcher_models(cx), Models::Ready(_))
        })
    });
    assert!(!drawn(&view, cx, Region::LauncherItem("account", 0)));
    view.update_in(cx, |wb, window, cx| wb.close_launcher(window, cx));

    let second = add(&daemon, "omp", "second", "Second");
    wait_until(cx, "the account to be known", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.harness_directory()
                .read(cx)
                .managed_of("omp")
                .next()
                .is_some()
        })
    });
    cx.simulate_keystrokes("cmd-shift-t");
    view.update(cx, |wb, cx| {
        wb.launcher_set_kind(hutan_desktop::sessions::launch::LaunchKind::Chat, cx);
    });
    // Default · the harness's own login, the harness's own login, then each account.
    assert!(drawn(&view, cx, Region::LauncherItem("account", 0)));
    assert!(drawn(&view, cx, Region::LauncherItem("account", 1)));
    assert!(drawn(&view, cx, Region::LauncherItem("account", 2)));
    assert!(!drawn(&view, cx, Region::LauncherItem("account", 3)));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher_account().map(str::to_owned)),
        None
    );
    click(&view, cx, Region::LauncherItem("account", 2));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher_account().map(str::to_owned)),
        Some("second".to_owned())
    );
    click(&view, cx, Region::Launcher("prompt"));
    cx.simulate_input("Fix the build");
    view.update_in(cx, |wb, window, cx| wb.launcher_start(false, window, cx));
    view.update_in(cx, |wb, window, cx| wb.launcher_confirm_start(window, cx));
    wait_until(cx, "the chat tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher().is_none()
                && wb
                    .state()
                    .active_item()
                    .is_some_and(|i| matches!(i, ItemRef::Chat { .. }))
        })
    });
    let chat = view
        .read_with(cx, |wb, cx| {
            wb.sessions().read(cx).sessions().first().cloned()
        })
        .unwrap()
        .chat
        .unwrap();
    let account = chat.account.clone().expect("the chat knows its account");
    assert_eq!(
        (account.name.as_str(), account.label.as_str()),
        ("second", "Second")
    );
    // The header names it; OMP was started with the account's agent directory.
    assert_eq!(who_runs("OMP", &chat), "OMP (Second)");
    let starts = std::fs::read_to_string(scripts.path().join("omp-starts")).unwrap();
    assert!(starts.lines().any(|l| l == second.home), "{starts}");

    // A chat of the harness's own login says nothing extra.
    let mut own = chat.clone();
    own.account = None;
    assert_eq!(who_runs("OMP", &own), "OMP");
}

#[gpui::test]
fn the_start_sheet_names_the_default_in_its_first_choice(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path());
    add(&daemon, "omp", "second", "Second");
    let _: hutan_protocol::messages::Empty =
        daemon.call(Request::AccountDefault(AccountDefaultSet {
            harness: "omp".into(),
            account: Some("second".into()),
            project: None,
        }));
    let (view, cx) = open(cx, &daemon);
    view.update(cx, |wb, cx| {
        wb.harness_directory()
            .update(cx, |directory, cx| directory.refresh(cx));
    });
    wait_until(cx, "the accounts", |cx| {
        view.read_with(cx, |wb, cx| {
            let directory = wb.harness_directory().read(cx);
            directory.default_account("omp") == Some("second")
        })
    });
    let choices = view.read_with(cx, |wb, cx| {
        wb.harness_directory().read(cx).account_choices("omp", None)
    });
    let labels: Vec<_> = choices.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["Default · Second", "Harness login", "Second"]);
    assert_eq!(
        choices[0].account, None,
        "the first choice follows the defaults"
    );
}
