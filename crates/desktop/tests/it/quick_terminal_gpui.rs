#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The quick terminal (QUICK-TERMINAL): the panel's model (show and hide toggle it, its sessions
//! are kept, a tab moves to the main window), the shortcut with its failures, the keys inside
//! the panel and what the system side was asked, against an in-process daemon and the fakes of
//! `common::mac`. No test registers a real hot key or touches another app.

use crate::common;

use std::time::Duration;

use common::mac::{Fakes, open, open_with_services};
use common::{TestDaemon, draw, wait_until};
use gpui::{AnyWindowHandle, Entity, Keystroke, TestAppContext, VisualTestContext};
use hutan_core::{SessionId, SessionState};
use hutan_desktop::macos::{MacEvent, QuickPanelCall, event_channel};
use hutan_desktop::workbench::actions::ToggleQuickTerminal;
use hutan_desktop::workbench::quick_terminal::{HideOnEscape, MoveToMain};
use hutan_desktop::workbench::{QuickEvent, QuickPanelView, QuickTerminal, Workbench};

const SHELL: &str = "[terminal]\nshell = \"/bin/sh\"\n";

fn write_settings(daemon: &TestDaemon, text: &str) {
    std::fs::write(daemon.home().join("settings.toml"), text).unwrap();
}

fn wait_synced(view: &Entity<Workbench>, cx: &mut VisualTestContext, sessions: usize) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == sessions
        })
    });
}

fn set(view: &Entity<Workbench>, cx: &mut VisualTestContext, key: &str, value: toml::Value) {
    view.read_with(cx, |wb, _| wb.settings_store().write_user(key, value))
        .unwrap();
    cx.run_until_parked();
}

fn notice(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
}

fn model(view: &Entity<Workbench>, cx: &VisualTestContext) -> Entity<QuickTerminal> {
    view.read_with(cx, |wb, _| wb.quick_terminal().clone())
}

fn toggle(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    view.update(cx, |wb, cx| wb.toggle_quick_terminal(cx));
    cx.run_until_parked();
}

fn visible(model: &Entity<QuickTerminal>, cx: &VisualTestContext) -> bool {
    model.read_with(cx, |quick, _| quick.visible())
}

fn sessions(model: &Entity<QuickTerminal>, cx: &VisualTestContext) -> Vec<SessionId> {
    model.read_with(cx, |quick, _| quick.sessions())
}

fn window(model: &Entity<QuickTerminal>, cx: &VisualTestContext) -> Option<AnyWindowHandle> {
    model.read_with(cx, |quick, _| quick.window())
}

/// Shows the panel and waits for its first session.
fn show_with_session(
    view: &Entity<Workbench>,
    model: &Entity<QuickTerminal>,
    cx: &mut VisualTestContext,
) -> SessionId {
    toggle(view, cx);
    wait_until(cx, "the panel's first session", |cx| {
        sessions(model, cx).len() == 1
    });
    draw_panel(model, cx);
    sessions(model, cx)[0]
}

/// Two frames of the panel's own window (its terminal views are built by the first, and given
/// the keyboard by the second).
fn draw_panel(model: &Entity<QuickTerminal>, cx: &mut VisualTestContext) {
    let handle = window(model, cx).expect("the panel has a window");
    for _ in 0..3 {
        handle
            .update(cx, |_, window, cx| {
                window.draw(cx).clear(cx);
                window.simulate_next_frame(cx);
            })
            .unwrap();
        cx.run_until_parked();
    }
}

fn press(model: &Entity<QuickTerminal>, cx: &mut VisualTestContext, keys: &str) {
    let handle = window(model, cx).expect("the panel has a window");
    for key in keys.split(' ') {
        handle
            .update(cx, |_, window, cx| {
                window.dispatch_keystroke(Keystroke::parse(key).unwrap(), cx);
            })
            .unwrap();
        cx.run_until_parked();
    }
}

/// Lets the exit animation run out, after which the window is gone.
fn settle_exit(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
}

fn state_of(daemon: &TestDaemon, id: SessionId) -> SessionState {
    daemon.session(id).state
}

fn is_alive(state: &SessionState) -> bool {
    !matches!(
        state,
        SessionState::Exited { .. } | SessionState::Failed { .. } | SessionState::Stopped
    )
}

#[gpui::test]
fn showing_and_hiding_toggle_the_panel_and_the_session_is_kept(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, SHELL);
    let fakes = Fakes::new();
    fakes.quick_panel.point_at(100.0, 100.0, 25.0);
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    assert!(!visible(&model, cx));
    assert!(window(&model, cx).is_none(), "no window until it is shown");

    let session = show_with_session(&view, &model, cx);
    assert!(visible(&model, cx));
    assert!(window(&model, cx).is_some());
    assert_eq!(fakes.quick_panel.calls(), [QuickPanelCall::Remember]);
    assert_eq!(
        model.read_with(cx, |quick, _| quick.active_session()),
        Some(session)
    );
    assert!(is_alive(&state_of(&daemon, session)));

    // The hotkey again hides it, gives the keyboard back, and keeps the shell running.
    toggle(&view, cx);
    assert!(!visible(&model, cx));
    assert_eq!(
        fakes.quick_panel.calls(),
        [QuickPanelCall::Remember, QuickPanelCall::Restore]
    );
    settle_exit(cx);
    assert!(
        window(&model, cx).is_none(),
        "the window goes after the exit"
    );
    assert_eq!(sessions(&model, cx), [session], "the tab is kept");
    assert!(is_alive(&state_of(&daemon, session)), "hiding never kills");
    assert!(
        view.read_with(cx, |wb, cx| wb
            .sessions()
            .read(cx)
            .attached(session)
            .is_some()),
        "the panel's attachment keeps the terminal live while hidden"
    );

    // Shown again: the same session, no second one.
    toggle(&view, cx);
    assert!(visible(&model, cx));
    assert!(window(&model, cx).is_some());
    assert_eq!(sessions(&model, cx), [session]);
    wait_synced(&view, cx, 1);
    assert_eq!(
        fakes.quick_panel.calls().len(),
        3,
        "the front app is remembered again on the second show"
    );
}

#[gpui::test]
fn showing_again_while_it_slides_away_cancels_the_close(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, SHELL);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    show_with_session(&view, &model, cx);
    let first = window(&model, cx).unwrap();

    toggle(&view, cx);
    assert!(!visible(&model, cx));
    toggle(&view, cx);
    assert!(visible(&model, cx));
    settle_exit(cx);
    assert_eq!(
        window(&model, cx),
        Some(first),
        "the window that was leaving stays for the new show"
    );
    assert!(visible(&model, cx));
}

#[gpui::test]
fn the_panel_covers_the_configured_share_below_the_menu_bar(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(
        &daemon,
        "[terminal]\nshell = \"/bin/sh\"\nquick_height = 50\n",
    );
    let fakes = Fakes::new();
    fakes.quick_panel.point_at(10.0, 10.0, 30.0);
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    toggle(&view, cx);
    let handle = window(&model, cx).unwrap();
    let (bounds, display) = handle
        .update(cx, |_, window, cx| {
            (window.bounds(), window.display(cx).map(|d| d.bounds()))
        })
        .unwrap();
    let display = display.expect("a display");
    let usable = f32::from(display.size.height) - 30.0;
    assert!(
        (f32::from(bounds.size.height) - (usable * 0.5).round()).abs() < 1.5,
        "height {:?} of {:?}",
        bounds.size.height,
        display.size
    );
    assert_eq!(bounds.size.width, display.size.width, "the full width");
    assert!(
        (f32::from(bounds.origin.y) - (f32::from(display.origin.y) + 30.0)).abs() < 1.5,
        "starts under the menu bar: {:?}",
        bounds.origin
    );
}

#[gpui::test]
fn the_shortcut_toggles_the_panel_and_follows_the_settings(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, SHELL);
    let (tx, rx) = event_channel();
    let fakes = Fakes::new();
    let mut mac = fakes.services();
    mac.events = Some(rx);
    let (view, cx) = open_with_services(cx, &daemon, mac);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    assert_eq!(
        fakes.quick_hotkey.active().as_deref(),
        Some("⌃⇧`"),
        "on by default, on a key neither macOS nor Hutan binds"
    );
    assert_eq!(notice(&view, cx), None);
    assert!(fakes.hotkey.attempts().is_empty(), "Mawas's is its own");

    tx.try_send(MacEvent::QuickHotkey).unwrap();
    wait_until(cx, "the panel to show", |cx| visible(&model, cx));
    tx.try_send(MacEvent::QuickHotkey).unwrap();
    wait_until(cx, "the panel to hide", |cx| !visible(&model, cx));

    // Another combination replaces it; off releases it; on again takes it back.
    set(&view, cx, "terminal.quick_hotkey", "cmd-alt-k".into());
    assert_eq!(fakes.quick_hotkey.active().as_deref(), Some("⌥⌘K"));
    set(&view, cx, "terminal.quick_terminal", false.into());
    assert_eq!(fakes.quick_hotkey.active(), None);
    set(&view, cx, "terminal.quick_terminal", true.into());
    assert_eq!(fakes.quick_hotkey.active().as_deref(), Some("⌥⌘K"));
    set(&view, cx, "terminal.quick_hotkey", "".into());
    assert_eq!(
        fakes.quick_hotkey.active(),
        None,
        "an empty shortcut is off"
    );
    assert_eq!(notice(&view, cx), None);
}

#[gpui::test]
fn a_shortcut_that_cannot_be_used_is_reported_once_and_left_as_set(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(
        &daemon,
        "[terminal]\nshell = \"/bin/sh\"\nquick_hotkey = \"alt-k\"\n",
    );
    let fakes = Fakes::new();
    fakes.quick_hotkey.take("⌥K");
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let text = notice(&view, cx).expect("a notice");
    assert!(text.contains("another app"), "{text}");
    assert!(text.contains("terminal.quick_hotkey"), "{text}");
    assert_eq!(fakes.quick_hotkey.active(), None);
    assert_eq!(fakes.quick_hotkey.attempts(), ["⌥K"]);

    // Another setting reloading does not try (or tell) again.
    view.update(cx, |wb, cx| wb.clear_notice(cx));
    set(&view, cx, "terminal.font_size", 14.0.into());
    assert_eq!(fakes.quick_hotkey.attempts(), ["⌥K"]);
    assert_eq!(notice(&view, cx), None);

    // The setting is what the user wrote: letting go of the key and writing it again works.
    fakes.quick_hotkey.release("⌥K");
    set(&view, cx, "terminal.quick_hotkey", "alt-j".into());
    assert_eq!(fakes.quick_hotkey.active().as_deref(), Some("⌥J"));

    // Not a shortcut at all: its own notice; a system key: a warning, still registered.
    set(&view, cx, "terminal.quick_hotkey", "space".into());
    let text = notice(&view, cx).expect("a notice");
    assert!(text.contains("not valid"), "{text}");
    assert_eq!(fakes.quick_hotkey.active(), None);
    set(&view, cx, "terminal.quick_hotkey", "cmd-space".into());
    let text = notice(&view, cx).expect("a notice");
    assert!(text.contains("Spotlight"), "{text}");
    assert_eq!(fakes.quick_hotkey.active().as_deref(), Some("⌘Space"));
}

#[gpui::test]
fn the_quick_shortcut_never_takes_the_one_mawas_uses(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(
        &daemon,
        "[assistant]\nenabled = true\nglobal_shortcut = \"ctrl-shift-grave\"\n",
    );
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    assert_eq!(
        fakes.hotkey.active().as_deref(),
        Some("⌃⇧`"),
        "Mawas keeps it"
    );
    assert_eq!(fakes.quick_hotkey.active(), None);
    let text = notice(&view, cx).expect("a notice");
    assert!(text.contains("Mawas"), "{text}");
    // Moving Mawas's shortcut away frees it for the quick terminal.
    set(&view, cx, "assistant.global_shortcut", "alt-space".into());
    assert_eq!(fakes.quick_hotkey.active().as_deref(), Some("⌃⇧`"));
}

#[gpui::test]
fn another_app_in_secure_input_keeps_the_panel_away(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, SHELL);
    let fakes = Fakes::new();
    fakes.quick_panel.set_secure_input(true);
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    toggle(&view, cx);
    assert!(!visible(&model, cx));
    assert!(window(&model, cx).is_none(), "no window took the keyboard");
    assert!(
        fakes.quick_panel.calls().is_empty(),
        "no app was remembered: nothing was taken from it"
    );
    wait_synced(&view, cx, 0);

    fakes.quick_panel.set_secure_input(false);
    toggle(&view, cx);
    assert!(visible(&model, cx));
}

#[gpui::test]
fn the_first_session_starts_in_home_or_the_active_project(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().canonicalize().unwrap();
    write_settings(
        &daemon,
        "[terminal]\nshell = \"/bin/sh\"\nquick_start_in = \"project\"\n",
    );
    let workspace = daemon.create_workspace("proj", &[root.as_path()]);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    wait_until(cx, "the project to be listed", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).workspace(workspace.id).is_some()
        })
    });
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    cx.run_until_parked();
    let model = model(&view, cx);
    let session = show_with_session(&view, &model, cx);
    let info = daemon.session(session);
    assert_eq!(
        info.cwd.as_deref().map(|p| p.canonicalize().unwrap()),
        Some(root)
    );
}

#[gpui::test]
fn a_tab_added_with_the_new_terminal_key_sits_in_the_panel(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(
        &daemon,
        "[terminal]\nshell = \"/bin/sh\"\n[tabs]\nclose_session = \"never\"\n",
    );
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    let first = show_with_session(&view, &model, cx);

    // ⌘T with the keyboard in the panel adds a tab to the panel, not to the main window.
    press(&model, cx, "cmd-t");
    wait_until(cx, "the second tab", |cx| sessions(&model, cx).len() == 2);
    let second = sessions(&model, cx)[1];
    assert_ne!(first, second);
    assert_eq!(
        model.read_with(cx, |quick, _| quick.active_session()),
        Some(second),
        "the new tab has the keyboard"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        None,
        "the main window got no tab"
    );
    draw_panel(&model, cx);

    // ⌘} and ⌘{ walk the tabs.
    press(&model, cx, "cmd-{");
    assert_eq!(
        model.read_with(cx, |quick, _| quick.active_session()),
        Some(first)
    );
    press(&model, cx, "cmd-}");
    assert_eq!(
        model.read_with(cx, |quick, _| quick.active_session()),
        Some(second)
    );

    // ⌘W closes the active tab: the session is kept (tabs.close_session = never), the panel
    // stays for the other tab; closing the last one hides the panel.
    press(&model, cx, "cmd-w");
    assert_eq!(sessions(&model, cx), [first]);
    assert!(visible(&model, cx));
    assert!(is_alive(&state_of(&daemon, second)));
    press(&model, cx, "cmd-w");
    assert!(sessions(&model, cx).is_empty());
    assert!(!visible(&model, cx));
    assert!(is_alive(&state_of(&daemon, first)));
}

#[gpui::test]
fn the_move_key_puts_the_tab_in_the_main_window(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, SHELL);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    let session = show_with_session(&view, &model, cx);
    press(&model, cx, "cmd-t");
    wait_until(cx, "the second tab", |cx| sessions(&model, cx).len() == 2);
    let other = sessions(&model, cx)[1];
    draw_panel(&model, cx);

    // ⌘⇧↩ in a terminal is the split zoom of the main window; in the panel it moves the tab.
    model.update(cx, |quick, cx| quick.select(0, cx));
    draw_panel(&model, cx);
    press(&model, cx, "cmd-shift-enter");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(session),
        "the main window shows it"
    );
    assert_eq!(
        sessions(&model, cx),
        [other],
        "the panel lost that tab only"
    );
    assert!(!visible(&model, cx), "the panel is hidden");
    assert!(is_alive(&state_of(&daemon, session)), "moved, not killed");
    assert!(view.read_with(cx, |wb, _| wb.holds(session)));
    assert_eq!(
        fakes.quick_panel.calls(),
        [QuickPanelCall::Remember],
        "Hutan comes forward: the keyboard is not given back to the other app"
    );
}

#[gpui::test]
fn the_button_and_the_event_move_the_only_tab_and_the_next_show_starts_a_new_one(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, SHELL);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    let session = show_with_session(&view, &model, cx);
    model.update(cx, |quick, cx| {
        quick.request(QuickEvent::MoveToMain(session), cx)
    });
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(session)
    );
    assert!(sessions(&model, cx).is_empty());
    assert!(!visible(&model, cx));
    settle_exit(cx);

    toggle(&view, cx);
    wait_until(cx, "a new session for the panel", |cx| {
        sessions(&model, cx).len() == 1
    });
    assert_ne!(sessions(&model, cx)[0], session);
    wait_synced(&view, cx, 2);
}

#[gpui::test]
fn escape_hides_the_panel_but_not_from_a_program_on_the_alternate_screen(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, SHELL);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    let session = show_with_session(&view, &model, cx);
    let terminal = daemon.session(session).terminal_id.expect("a terminal");
    let alt = |cx: &mut VisualTestContext| {
        let handle = window(&model, cx).unwrap();
        handle
            .update(cx, |root, _, cx| {
                root.downcast::<QuickPanelView>()
                    .ok()
                    .and_then(|root| root.read(cx).active_view(cx))
                    .map(|view| view.read(cx).model().modes().alt_screen)
            })
            .unwrap()
            .unwrap_or(false)
    };

    // A full-screen program keeps its Escape.
    daemon.send_input(terminal, b"printf '\\033[?1049h'\n");
    wait_until(cx, "the alternate screen", |cx| alt(cx));
    press(&model, cx, "escape");
    assert!(visible(&model, cx), "vim's Escape is vim's");

    // At the prompt Escape hides it.
    daemon.send_input(terminal, b"printf '\\033[?1049l'\n");
    wait_until(cx, "the main screen", |cx| !alt(cx));
    press(&model, cx, "escape");
    assert!(!visible(&model, cx));
    assert!(is_alive(&state_of(&daemon, session)));
}

#[gpui::test]
fn escape_stays_with_the_terminal_when_the_setting_is_off(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(
        &daemon,
        "[terminal]\nshell = \"/bin/sh\"\nquick_escape_hides = false\n",
    );
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    show_with_session(&view, &model, cx);
    press(&model, cx, "escape");
    assert!(visible(&model, cx));
    // The shortcut still hides it.
    toggle(&view, cx);
    assert!(!visible(&model, cx));
}

#[gpui::test]
fn the_action_is_dispatchable_and_the_command_is_in_the_palette(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, SHELL);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    cx.dispatch_action(ToggleQuickTerminal);
    cx.run_until_parked();
    assert!(visible(&model, cx), "the palette command shows the panel");
    assert!(view.read_with(cx, |wb, _| {
        wb.commands()
            .iter()
            .any(|c| c.title.as_ref() == "Terminal: Toggle Quick Terminal")
    }));
    // The panel's own actions are named for the keymap.
    assert_eq!(
        <HideOnEscape as gpui::Action>::name_for_type(),
        "quick_terminal::HideOnEscape"
    );
    assert_eq!(
        <MoveToMain as gpui::Action>::name_for_type(),
        "quick_terminal::MoveToMain"
    );
}

#[gpui::test]
fn a_shell_that_exits_takes_its_tab_and_the_panel_with_it(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, SHELL);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    let session = show_with_session(&view, &model, cx);
    let terminal = daemon.session(session).terminal_id.expect("a terminal");
    daemon.send_input(terminal, b"exit\n");
    wait_until(cx, "the panel to give up the tab", |cx| {
        sessions(&model, cx).is_empty()
    });
    assert!(!visible(&model, cx), "nothing left to show");
    settle_exit(cx);
    assert!(window(&model, cx).is_none());

    toggle(&view, cx);
    wait_until(cx, "a fresh session", |cx| sessions(&model, cx).len() == 1);
    assert_ne!(sessions(&model, cx)[0], session);
}

#[gpui::test]
fn without_a_daemon_the_panel_says_why_it_has_no_terminal(cx: &mut TestAppContext) {
    let (connection, settings, _) = common::offline_connection();
    let fakes = Fakes::new();
    let options = hutan_desktop::workbench::WorkbenchOptions::builtin(settings, connection, None)
        .with_mac(fakes.services());
    let (view, cx) = common::open_options(cx, options);
    let model = model(&view, cx);
    toggle(&view, cx);
    wait_until(cx, "the failure", |cx| {
        model.read_with(cx, |quick, _| quick.error().is_some())
    });
    let text = model.read_with(cx, |quick, _| quick.error().unwrap().to_owned());
    assert!(text.contains("Cannot start a terminal"), "{text}");
    assert!(
        visible(&model, cx),
        "the panel stays: the shortcut hides it"
    );
    toggle(&view, cx);
    assert!(!visible(&model, cx));
}

#[gpui::test]
fn closing_a_tab_ends_its_session_by_default(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, SHELL);
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    let session = show_with_session(&view, &model, cx);
    press(&model, cx, "cmd-w");
    assert!(sessions(&model, cx).is_empty());
    assert!(!visible(&model, cx));
    wait_until(cx, "the session to be closed", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(session).is_none()
        })
    });
}

#[gpui::test]
fn the_older_terminal_tab_close_still_ends_the_session_of_a_closed_tab(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(
        &daemon,
        "[terminal]\nshell = \"/bin/sh\"\ntab_close = \"close_session\"\n",
    );
    let fakes = Fakes::new();
    let (view, cx) = open(cx, &daemon, &fakes);
    wait_synced(&view, cx, 0);
    let model = model(&view, cx);
    let session = show_with_session(&view, &model, cx);
    press(&model, cx, "cmd-w");
    assert!(sessions(&model, cx).is_empty());
    assert!(!visible(&model, cx));
    wait_until(cx, "the session to be closed", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(session).is_none()
        })
    });
}
