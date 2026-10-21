#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! `PR-FIRST-RUN` against an in-process daemon: the first launch is a first screen in the centre
//! (agents, *Open a folder*, *New terminal*), not a modal sheet; opening a folder turns the centre
//! into the start composer of that project (prompt and agent, Options folded) and starts a
//! running agent; the old asks happen where they matter — notifications when something first
//! needs the user while Hutan is behind other windows, hooks on the *Track as* strip, the send key
//! above the first composer — each once, and nothing is written unless the user chooses.
//! Every system seam is a fake: the notification permission and the integration installer.

use crate::common;
use crate::setup_gpui::{daemon_with_harnesses, wait_listing};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::agents::{Node, Reporter};
use common::mac::Fakes;
use common::{TestDaemon, draw, open_options, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::integrations::{FakeInstaller, IntegrationStatus};
use hutan_desktop::macos::NotificationAnswer;
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::{
    LauncherStop, NotifyOffer, Readiness, Region, Workbench, WorkbenchOptions,
};
use hutan_settings::{SettingsPaths, SettingsStore};

// ---- fixtures ----------------------------------------------------------------------------------

fn store(daemon: &TestDaemon) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

fn saved(daemon: &TestDaemon) -> String {
    std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap_or_default()
}

/// A workbench on its first launch: no `app.setup_done`, the first screen asked for.
fn open_first_launch<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let options =
        WorkbenchOptions::builtin(store(daemon), daemon.connection(), None).with_first_run_setup();
    let (view, cx) = open_options(cx, options);
    synced(&view, cx);
    (view, cx)
}

fn synced(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

fn wait_drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        drawn(view, cx, region)
    });
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_drawn(view, cx, region);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn close_window(cx: &mut VisualTestContext) {
    cx.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

fn sessions(view: &Entity<Workbench>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().len())
}

/// A folder the daemon opens as a project.
fn folder(dir: &Path, name: &str) -> PathBuf {
    let path = std::fs::canonicalize(dir).unwrap().join(name);
    std::fs::create_dir(&path).unwrap();
    path
}

fn open_folder(view: &Entity<Workbench>, cx: &mut VisualTestContext, path: PathBuf) {
    view.update_in(cx, |wb, window, cx| wb.open_project_path(path, window, cx));
}

fn composer_open(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.launcher().is_some_and(|l| l.inline()))
}

// ---- the first screen --------------------------------------------------------------------------

#[gpui::test]
fn the_first_launch_is_a_first_screen_not_a_modal_sheet(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    let (view, cx) = open_first_launch(cx, &daemon);

    // Nothing modal: no sheet, no walk-through; the centre says what to do.
    assert!(!view.read_with(cx, |wb, _| wb.setup_open()));
    wait_drawn(&view, cx, Region::FirstRun("screen"));
    assert!(drawn(&view, cx, Region::FirstRun("open-folder")));
    assert!(drawn(&view, cx, Region::FirstRun("new-terminal")));
    assert!(!drawn(&view, cx, Region::Setup("sheet")));
    assert_eq!(saved(&daemon), "", "showing it writes nothing");

    // The detected agents: OMP is logged in; the others say what gets them ready.
    wait_listing(&view, cx);
    let choices = view.read_with(cx, |wb, cx| wb.launcher_harness_choices(cx));
    let omp = choices.iter().find(|c| c.id == "omp").unwrap();
    assert_eq!(omp.readiness, Readiness::Ready);
    for choice in choices.iter().filter(|c| c.readiness != Readiness::Ready) {
        let action = match choice.readiness {
            Readiness::NeedsLogin => "login",
            Readiness::NeedsInstall(_) => "guide",
            ref other => panic!("{}: {other:?}", choice.id),
        };
        wait_drawn(&view, cx, Region::FirstRunAgent(choice.id, action));
    }
    // Mawas is on without being asked for, and nothing about it was written.
    assert!(view.read_with(cx, |wb, _| wb.assistant_enabled()));
    assert_eq!(saved(&daemon), "");
}

#[gpui::test]
fn new_terminal_on_the_first_screen_ends_the_first_run(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open_first_launch(cx, &daemon);
    click(&view, cx, Region::FirstRun("new-terminal"));
    wait_until(cx, "the terminal", |cx| sessions(&view, cx) == 1);
    assert_eq!(saved(&daemon).trim(), "[app]\nsetup_done = true");
    assert!(!drawn(&view, cx, Region::FirstRun("screen")));
}

#[gpui::test]
fn after_the_first_run_the_welcome_pane_is_the_plain_empty_state(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let (view, cx) = open_first_launch(cx, &daemon);
    wait_drawn(&view, cx, Region::Launcher("empty-start"));
    assert!(!drawn(&view, cx, Region::FirstRun("screen")));
}

// ---- opening a folder: the start composer -----------------------------------------------------

#[gpui::test]
fn opening_a_folder_makes_the_centre_the_start_composer_and_a_prompt_starts_an_agent(
    cx: &mut TestAppContext,
) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    let folders = tempfile::tempdir().unwrap();
    let api = folder(folders.path(), "api");
    let (view, cx) = open_first_launch(cx, &daemon);
    wait_listing(&view, cx);

    open_folder(&view, cx, api);
    wait_until(cx, "the start composer", |cx| composer_open(&view, cx));
    // The first run is over, and that is all that was written.
    assert!(!drawn(&view, cx, Region::FirstRun("screen")));
    assert_eq!(saved(&daemon).trim(), "[app]\nsetup_done = true");

    // The centre is the composer: a prompt and an agent; the rest folded under Options.
    wait_drawn(&view, cx, Region::Launcher("composer"));
    assert!(drawn(&view, cx, Region::Launcher("prompt")));
    assert!(drawn(&view, cx, Region::LauncherChoice("agent", "omp")));
    assert!(drawn(&view, cx, Region::Launcher("options")));
    assert!(!drawn(&view, cx, Region::LauncherChoice("what", "chat")));
    assert!(!drawn(&view, cx, Region::Launcher("start-new")));
    assert!(!drawn(&view, cx, Region::Launcher("cancel")));
    view.read_with(cx, |wb, cx| {
        let launcher = wb.launcher().unwrap();
        assert!(launcher.project().is_some());
        assert!(!launcher.options_open());
        assert_eq!(
            wb.launcher_stops(cx),
            vec![
                LauncherStop::Prompt,
                LauncherStop::Agent,
                LauncherStop::Options
            ],
            "Tab walks the prompt, the agent and Options"
        );
    });

    // Options unfold the rest of the sheet's rows.
    click(&view, cx, Region::Launcher("options"));
    assert!(drawn(&view, cx, Region::LauncherChoice("what", "chat")));
    assert!(drawn(&view, cx, Region::Launcher("project")));
    assert!(view.read_with(cx, |wb, cx| {
        wb.launcher_stops(cx).contains(&LauncherStop::Where)
    }));
    click(&view, cx, Region::Launcher("options"));
    assert!(!drawn(&view, cx, Region::LauncherChoice("what", "chat")));

    // Type the task and press ⌘↩: the first chat on an agent asks once, ↩ says yes.
    click(&view, cx, Region::Launcher("prompt"));
    cx.simulate_input("Fix the flaky test");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().prompt().to_owned()),
        "Fix the flaky test"
    );
    cx.simulate_keystrokes("cmd-enter");
    if view.read_with(cx, |wb, _| wb.launcher().unwrap().confirm().is_some()) {
        cx.simulate_keystrokes("enter");
    }
    wait_until(cx, "the agent chat", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .sessions()
                .iter()
                .any(|s| s.chat.as_ref().is_some_and(|c| c.agent == "omp"))
        })
    });
    wait_until(cx, "the composer to give way", |cx| {
        view.read_with(cx, |wb, _| wb.launcher().is_none())
    });
    let chat_open = view.read_with(cx, |wb, _| {
        wb.state()
            .panes()
            .iter()
            .any(|p| p.items.iter().any(|i| matches!(i, ItemRef::Chat { .. })))
    });
    assert!(chat_open, "the centre shows the chat");

    // The same folder again: its centre is not empty, so there is no composer on top of it.
    open_folder(&view, cx, folders.path().join("api"));
    cx.run_until_parked();
    draw(cx);
    assert!(!composer_open(&view, cx));
}

#[gpui::test]
fn the_inline_composer_is_not_a_sheet_and_escape_keeps_it(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let folders = tempfile::tempdir().unwrap();
    let (view, cx) = open_first_launch(cx, &daemon);
    open_folder(&view, cx, folder(folders.path(), "web"));
    wait_until(cx, "the start composer", |cx| composer_open(&view, cx));
    wait_drawn(&view, cx, Region::Launcher("composer"));
    // No backdrop: the sheet's own region never appears for it, and Escape does not close it.
    click(&view, cx, Region::Launcher("prompt"));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(composer_open(&view, cx), "Escape leaves the composer be");
    // Opening the sheet over it replaces it: the sheet is the Start… sheet again.
    view.update_in(cx, |wb, window, cx| {
        wb.open_launcher(Default::default(), window, cx)
    });
    assert!(view.read_with(cx, |wb, _| !wb.launcher().unwrap().inline()));
}

#[gpui::test]
fn switching_to_a_plain_terminal_keeps_the_primary_button_and_drops_the_task_fields(
    cx: &mut TestAppContext,
) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon_with_harnesses(scripts.path());
    let folders = tempfile::tempdir().unwrap();
    let api = folder(folders.path(), "api");
    let (view, cx) = open_first_launch(cx, &daemon);
    wait_listing(&view, cx);
    open_folder(&view, cx, api);
    wait_until(cx, "the start composer", |cx| composer_open(&view, cx));
    click(&view, cx, Region::Launcher("options"));
    wait_drawn(&view, cx, Region::LauncherChoice("what", "chat"));

    // An agent chat has a title and Start now / Queue as task; the Start button is in view.
    assert!(drawn(&view, cx, Region::Launcher("title")));
    assert!(drawn(&view, cx, Region::LauncherChoice("mode", "now")));
    let start = |view: &Entity<Workbench>, cx: &mut VisualTestContext| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Launcher("start")))
            .expect("the primary button is drawn")
    };
    let before = start(&view, cx);

    // A plain terminal: no Prompt and Agent rows, no task-only fields, and the primary button
    // has not moved.
    click(&view, cx, Region::LauncherChoice("what", "terminal"));
    draw(cx);
    assert!(!drawn(&view, cx, Region::Launcher("prompt")));
    assert!(!drawn(&view, cx, Region::Launcher("title")));
    assert!(!drawn(&view, cx, Region::LauncherChoice("mode", "now")));
    assert!(!drawn(&view, cx, Region::LauncherChoice("mode", "queue")));
    let after = start(&view, cx);
    assert_eq!(
        (after.top(), after.right()),
        (before.top(), before.right()),
        "the button stays where it was (its label is another one)"
    );
    view.read_with(cx, |wb, cx| {
        let stops = wb.launcher_stops(cx);
        assert!(!stops.contains(&LauncherStop::Title), "{stops:?}");
        assert!(!stops.contains(&LauncherStop::Mode), "{stops:?}");
        assert!(!wb.launcher().unwrap().queue());
    });

    // Back to a chat: they are back.
    click(&view, cx, Region::LauncherChoice("what", "chat"));
    assert!(drawn(&view, cx, Region::Launcher("title")));
    assert!(drawn(&view, cx, Region::LauncherChoice("mode", "queue")));
    assert_eq!(start(&view, cx).top(), before.top());
}

#[gpui::test]
fn the_composer_asks_the_send_key_once_above_itself(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let folders = tempfile::tempdir().unwrap();
    let (view, cx) = open_first_launch(cx, &daemon);
    open_folder(&view, cx, folder(folders.path(), "web"));
    wait_until(cx, "the start composer", |cx| composer_open(&view, cx));
    wait_drawn(&view, cx, Region::Launcher("ask-enter"));
    assert!(drawn(&view, cx, Region::Launcher("ask-cmd-enter")));
    assert!(
        !saved(&daemon).contains("send_key"),
        "asking writes nothing"
    );

    click(&view, cx, Region::Launcher("ask-enter"));
    let text = saved(&daemon);
    assert!(text.contains("send_key = \"enter\""), "{text}");
    assert!(text.contains("ask_send_key = false"), "{text}");
    assert!(!drawn(&view, cx, Region::Launcher("ask-enter")), "once");
}

// ---- notifications, when something first needs the user ---------------------------------------

/// A workbench after its first run, with the fakes, then the window goes to the background.
fn open_after_first_run<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    fakes: &Fakes,
    extra: &str,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        format!("[app]\nsetup_done = true\n{extra}"),
    )
    .unwrap();
    let options = WorkbenchOptions::builtin(store(daemon), daemon.connection(), None)
        .with_mac(fakes.services());
    let (view, cx) = open_options(cx, options);
    synced(&view, cx);
    (view, cx)
}

/// An agent that blocks: it needs the user.
fn something_needs_the_user(daemon: &TestDaemon, title: &str) {
    let blocked = daemon.sh_titled(title, "sleep 300").id;
    Reporter::new(blocked, "omp-extension", "omp").report(daemon, Node::main("m").blocked());
}

fn offer(view: &Entity<Workbench>, cx: &VisualTestContext) -> NotifyOffer {
    view.read_with(cx, |wb, _| wb.notify_offer().clone())
}

#[gpui::test]
fn the_first_thing_that_needs_you_while_hutan_is_behind_offers_notifications_once(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open_after_first_run(cx, &daemon, &fakes, "");
    assert_eq!(offer(&view, cx), NotifyOffer::Hidden);

    // Hutan is in front: something needs the user, and nothing is offered.
    something_needs_the_user(&daemon, "front");
    wait_until(cx, "the session to need the user", |cx| {
        view.read_with(cx, |wb, cx| !wb.sessions().read(cx).needs_you().is_empty())
    });
    assert_eq!(offer(&view, cx), NotifyOffer::Hidden);

    // Hutan goes behind other windows; the next one is the first it could not show.
    cx.deactivate_window();
    something_needs_the_user(&daemon, "behind");
    wait_until(cx, "the offer", |cx| {
        offer(&view, cx) == NotifyOffer::Offered
    });
    wait_drawn(&view, cx, Region::Offer("notify-yes"));
    assert!(drawn(&view, cx, Region::Offer("notify-no")));
    assert_eq!(
        fakes.notifications.requests(),
        0,
        "the system is not asked until the user says yes"
    );
    assert!(!saved(&daemon).contains("ask_notifications"));

    // Yes: now the system's prompt; the answer is shown; the offer is not made again.
    click(&view, cx, Region::Offer("notify-yes"));
    wait_until(cx, "the answer", |cx| {
        offer(&view, cx) == NotifyOffer::Answered(NotificationAnswer::Allowed)
    });
    assert_eq!(fakes.notifications.requests(), 1);
    assert!(saved(&daemon).contains("ask_notifications = false"));
    click(&view, cx, Region::Offer("notify-done"));
    assert_eq!(offer(&view, cx), NotifyOffer::Hidden);
    something_needs_the_user(&daemon, "again");
    wait_until(cx, "three to need the user", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).needs_you().len() >= 3)
    });
    assert_eq!(offer(&view, cx), NotifyOffer::Hidden, "asked once");
    assert_eq!(fakes.notifications.requests(), 1);
}

#[gpui::test]
fn no_thanks_is_final_and_asks_the_system_nothing(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open_after_first_run(cx, &daemon, &fakes, "");
    cx.deactivate_window();
    something_needs_the_user(&daemon, "behind");
    wait_until(cx, "the offer", |cx| {
        offer(&view, cx) == NotifyOffer::Offered
    });
    click(&view, cx, Region::Offer("notify-no"));
    assert_eq!(offer(&view, cx), NotifyOffer::Hidden);
    assert_eq!(fakes.notifications.requests(), 0);
    assert!(saved(&daemon).contains("ask_notifications = false"));
}

#[gpui::test]
fn a_denied_permission_is_said_and_never_asked_again(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    fakes.notifications.set_answer(NotificationAnswer::Denied);
    let (view, cx) = open_after_first_run(cx, &daemon, &fakes, "");
    cx.deactivate_window();
    something_needs_the_user(&daemon, "behind");
    wait_until(cx, "the offer", |cx| {
        offer(&view, cx) == NotifyOffer::Offered
    });
    click(&view, cx, Region::Offer("notify-yes"));
    wait_until(cx, "the answer", |cx| {
        offer(&view, cx) == NotifyOffer::Answered(NotificationAnswer::Denied)
    });
    assert!(drawn(&view, cx, Region::Offer("notify-done")));
    assert!(saved(&daemon).contains("ask_notifications = false"));
}

#[gpui::test]
fn nothing_is_offered_where_the_system_cannot_ask_or_after_a_choice(cx: &mut TestAppContext) {
    // Outside Hutan.app (inert services): there is no permission to ask for.
    let daemon = TestDaemon::start();
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let (view, cx) = open_options(
        cx,
        WorkbenchOptions::builtin(store(&daemon), daemon.connection(), None),
    );
    synced(&view, cx);
    cx.deactivate_window();
    something_needs_the_user(&daemon, "behind");
    wait_until(cx, "the session to need the user", |cx| {
        view.read_with(cx, |wb, cx| !wb.sessions().read(cx).needs_you().is_empty())
    });
    assert_eq!(offer(&view, cx), NotifyOffer::Hidden);
    close_window(cx);
    drop(view);

    // `app.ask_notifications = false`: the user already chose.
    let daemon = TestDaemon::start();
    let fakes = Fakes::new();
    let (view, cx) = open_after_first_run(cx, &daemon, &fakes, "ask_notifications = false\n");
    cx.deactivate_window();
    something_needs_the_user(&daemon, "behind");
    wait_until(cx, "the session to need the user", |cx| {
        view.read_with(cx, |wb, cx| !wb.sessions().read(cx).needs_you().is_empty())
    });
    assert_eq!(offer(&view, cx), NotifyOffer::Hidden);
}

// ---- hooks, on the Track as strip -------------------------------------------------------------

/// A terminal in which `claude` runs without ever having reported.
fn untracked_claude(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    daemon: &TestDaemon,
) -> hutan_core::SessionId {
    let session = daemon.create_session(&["bash", "-c", "exec -a claude sleep 300"], None);
    let id = session.id;
    wait_until(cx, "the session to be listed", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).session(id).is_some())
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
    wait_until(cx, "the agent to be found in the process tree", |cx| {
        view.update(cx, |wb, cx| {
            wb.sessions().update(cx, |s, cx| s.scan_agents(cx));
        });
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).untracked_agent(id).is_some()
        })
    });
    id
}

fn open_with_installer<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    installer: &FakeInstaller,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let options = WorkbenchOptions::builtin(store(daemon), daemon.connection(), None)
        .with_installer(Arc::new(installer.clone()));
    let (view, cx) = open_options(cx, options);
    synced(&view, cx);
    (view, cx)
}

#[gpui::test]
fn the_track_as_strip_offers_hooks_and_writes_them_only_when_accepted(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let installer = FakeInstaller::new();
    let (view, cx) = open_with_installer(cx, &daemon, &installer);
    untracked_claude(&view, cx, &daemon);
    wait_drawn(&view, cx, Region::Untracked("track"));

    // The offer waits for the hooks' status, then says what it would write before it writes.
    wait_drawn(&view, cx, Region::Offer("hooks-offer"));
    assert!(installer.installs().is_empty());
    click(&view, cx, Region::Offer("hooks-offer"));
    wait_drawn(&view, cx, Region::Offer("hooks-install"));
    assert!(drawn(&view, cx, Region::Offer("hooks-not-now")));
    assert!(
        installer.installs().is_empty(),
        "unfolding installs nothing"
    );
    assert!(!saved(&daemon).contains("ask_hooks"));

    click(&view, cx, Region::Offer("hooks-install"));
    wait_until(cx, "the install", |_| installer.installs() == ["claude"]);
    assert!(saved(&daemon).contains("ask_hooks = false"));
}

#[gpui::test]
fn no_thanks_to_the_hooks_is_final_and_installed_hooks_are_not_offered(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let installer = FakeInstaller::new();
    let (view, cx) = open_with_installer(cx, &daemon, &installer);
    untracked_claude(&view, cx, &daemon);
    click(&view, cx, Region::Offer("hooks-offer"));
    click(&view, cx, Region::Offer("hooks-not-now"));
    assert!(installer.installs().is_empty());
    assert!(saved(&daemon).contains("ask_hooks = false"));
    assert!(!drawn(&view, cx, Region::Offer("hooks-offer")));
    assert!(
        drawn(&view, cx, Region::Untracked("track")),
        "tracking stays"
    );
    close_window(cx);
    drop(view);

    // Hooks that are installed are not offered.
    let daemon = TestDaemon::start();
    let installer = FakeInstaller::new();
    installer.set_status("claude", IntegrationStatus::Installed);
    let (view, cx) = open_with_installer(cx, &daemon, &installer);
    untracked_claude(&view, cx, &daemon);
    wait_until(cx, "the status", |cx| {
        view.read_with(cx, |wb, _| {
            wb.integration_status("claude") == Some(IntegrationStatus::Installed)
        })
    });
    assert!(drawn(&view, cx, Region::Untracked("track")));
    assert!(!drawn(&view, cx, Region::Offer("hooks-offer")));
}
