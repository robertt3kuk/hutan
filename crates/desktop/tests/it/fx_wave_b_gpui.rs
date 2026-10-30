#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! FX-WAVE-B: the tab, settings and layout findings of the 2026-10-31 user test. Each test
//! states what the user saw; every wait is on state.

use crate::common;

use std::time::Duration;

use common::assistant::{AssistantEnv, Options, open_assistant};
use common::{TestDaemon, click_at, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::workbench::closing::TOAST_TIME;
use hutan_desktop::workbench::model::{Edge, ItemRef, PaneId};
use hutan_desktop::workbench::{FileOpen, Region, Workbench, actions};
use hutan_settings::{SettingsPaths, SettingsStore};

fn write_settings(daemon: &TestDaemon, text: &str) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        format!("[app]\nsetup_done = true\n[history]\nenabled = false\n{text}"),
    )
    .unwrap();
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

fn tab_of(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    session: SessionId,
) -> Option<(PaneId, usize)> {
    view.read_with(cx, |wb, _| {
        wb.state().find_session_tab(&session.to_string())
    })
}

fn show(view: &Entity<Workbench>, cx: &mut VisualTestContext, session: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(session, window, cx));
    wait_until(cx, "the tab of the session", |cx| {
        tab_of(view, cx, session).is_some()
    });
}

fn known(view: &Entity<Workbench>, cx: &VisualTestContext, session: SessionId) -> bool {
    view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).session(session).is_some()
    })
}

fn click_region(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap()
        .center();
    click_at(cx, at);
}

// ---- F5: the Undo toast lives long enough and a closed tab can be reopened -----------------------

#[gpui::test]
fn the_undo_toast_stays_while_it_is_read_and_while_the_pointer_is_on_it_and_reopen_closed_tab_brings_the_session_back(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let session = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    show(&view, cx, session.id);
    let tab = tab_of(&view, cx, session.id).unwrap();

    click_region(&view, cx, Region::TabClose(tab.0, tab.1));
    wait_until(cx, "the session to be closed", |cx| {
        !known(&view, cx, session.id)
    });
    wait_until(cx, "the Undo toast", |cx| {
        view.read_with(cx, |wb, _| wb.close_toast().is_some())
    });
    assert!(
        TOAST_TIME >= Duration::from_secs(10),
        "long enough to read what was closed"
    );

    // The tester looked at the screen for a while: nine seconds later the toast is still there.
    cx.executor().advance_clock(Duration::from_secs(9));
    cx.run_until_parked();
    assert!(view.read_with(cx, |wb, _| wb.close_toast().is_some()));

    // With the pointer on it the time stands still.
    draw(cx);
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::CloseToast("undo")))
        .unwrap()
        .center();
    cx.simulate_mouse_move(at, None, Modifiers::none());
    draw(cx);
    cx.executor().advance_clock(TOAST_TIME * 3);
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |wb, _| wb.close_toast().is_some()),
        "a toast under the pointer is not taken away"
    );

    // The pointer leaves: the time starts again and the toast goes.
    cx.simulate_mouse_move(
        gpui::point(gpui::px(2.0), gpui::px(2.0)),
        None,
        Modifiers::none(),
    );
    draw(cx);
    cx.executor()
        .advance_clock(TOAST_TIME + Duration::from_secs(1));
    cx.run_until_parked();
    assert!(view.read_with(cx, |wb, _| wb.close_toast().is_none()));

    // The toast is gone, the session is still reopenable: ⌘K → Tab: Reopen Closed Tab.
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .recently_closed()
            .iter()
            .map(|c| c.session)
            .collect::<Vec<_>>()),
        [session.id]
    );
    assert!(
        view.read_with(cx, |wb, _| wb
            .commands()
            .iter()
            .any(|c| c.title == "Tab: Reopen Closed Tab")),
        "the palette lists it"
    );
    cx.dispatch_action(actions::ReopenClosedTab);
    wait_until(cx, "the session and its tab to come back", |cx| {
        known(&view, cx, session.id) && tab_of(&view, cx, session.id).is_some()
    });
    assert!(view.read_with(cx, |wb, _| wb.recently_closed().is_empty()));
    // Nothing is left to reopen: it says so instead of doing something else.
    cx.dispatch_action(actions::ReopenClosedTab);
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.notice().map(|n| n.to_string())),
        Some("Nothing closed to reopen".to_owned())
    );
}

// ---- F12: closing a tab never opens a session that was not open ----------------------------------

#[gpui::test]
fn closing_the_focused_session_does_not_open_another_agents_session(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let first = daemon.create_session(&["cat"], None);
    let second = daemon.create_session(&["cat"], None);
    let third = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    // Only the first has a tab; the other two are in the sidebar, waiting.
    show(&view, cx, first.id);
    assert!(tab_of(&view, cx, second.id).is_none() && tab_of(&view, cx, third.id).is_none());
    let tab = tab_of(&view, cx, first.id).unwrap();

    click_region(&view, cx, Region::TabClose(tab.0, tab.1));
    wait_until(cx, "the first session to be closed", |cx| {
        !known(&view, cx, first.id)
    });
    draw(cx);
    assert!(
        tab_of(&view, cx, second.id).is_none() && tab_of(&view, cx, third.id).is_none(),
        "closing a tab opened no other session"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        None,
        "nothing else took the keyboard"
    );
}

#[gpui::test]
fn closing_the_focused_session_hands_the_keyboard_to_a_neighbour_that_is_open(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let first = daemon.create_session(&["cat"], None);
    let second = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    show(&view, cx, first.id);
    show(&view, cx, second.id);
    // Both are open in one pane; the first has the keyboard.
    show(&view, cx, first.id);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(first.id)
    );

    let tab = tab_of(&view, cx, first.id).unwrap();
    click_region(&view, cx, Region::TabClose(tab.0, tab.1));
    wait_until(cx, "the first session to be closed", |cx| {
        !known(&view, cx, first.id)
    });
    wait_until(cx, "the second session to have the keyboard", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()) == Some(second.id)
    });
}

// ---- F8: a side preview next to an agent keeps a readable width ----------------------------------

#[gpui::test]
fn a_side_preview_beside_a_file_beside_an_agent_keeps_every_pane_readable_and_widths_stay_put(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let agent = daemon.create_session(&["cat"], None);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "# Notes\n\nSome text.\n").unwrap();
    let (view, cx) = open(cx, &daemon);
    cx.simulate_resize(gpui::size(gpui::px(2000.0), gpui::px(900.0)));
    show(&view, cx, agent.id);
    let agent_pane = tab_of(&view, cx, agent.id).unwrap().0;

    // The file beside the agent, then its page to the side: three columns.
    view.update_in(cx, |wb, window, cx| {
        wb.open_file_in_split(
            agent_pane,
            &file.display().to_string(),
            Edge::Right,
            window,
            cx,
        );
    });
    wait_until(cx, "the file's pane", |cx| {
        view.read_with(cx, |wb, _| wb.state().panes().len() == 2)
    });
    view.update_in(cx, |wb, window, cx| {
        wb.open_markdown_preview_side(window, cx)
    });
    wait_until(cx, "the preview's pane", |cx| {
        view.read_with(cx, |wb, _| wb.state().panes().len() == 3)
    });
    draw(cx);
    let widths = |view: &Entity<Workbench>, cx: &VisualTestContext| -> Vec<f32> {
        view.read_with(cx, |wb, _| {
            let mut panes: Vec<_> = wb
                .state()
                .panes()
                .iter()
                .filter_map(|p| wb.region_bounds(Region::Pane(p.id)))
                .collect();
            panes.sort_by(|a, b| a.left().partial_cmp(&b.left()).unwrap());
            panes.iter().map(|b| f32::from(b.size.width)).collect()
        })
    };
    let three = widths(&view, cx);
    assert_eq!(three.len(), 3, "{three:?}");
    let min = Workbench::MIN_SPLIT_PANE_WIDTH;
    assert!(
        three[1] >= min - 1.0 && three[2] >= min - 1.0,
        "the file and its page are readable: {three:?}"
    );

    // The page closed: the agent keeps its width (the file takes the page's).
    let preview = view.read_with(cx, |wb, _| {
        wb.preview_tabs().first().map(|(pane, _)| *pane).unwrap()
    });
    click_region(&view, cx, Region::TabClose(preview, 0));
    wait_until(cx, "the page to close", |cx| {
        view.read_with(cx, |wb, _| wb.state().panes().len() == 2)
    });
    draw(cx);
    let two = widths(&view, cx);
    assert!(
        (two[0] - three[0]).abs() < 2.0,
        "the agent's width did not change by itself: {three:?} then {two:?}"
    );

    // Opened again: the same widths as before.
    view.update_in(cx, |wb, window, cx| {
        wb.open_markdown_preview_side(window, cx)
    });
    wait_until(cx, "the page again", |cx| {
        view.read_with(cx, |wb, _| wb.state().panes().len() == 3)
    });
    draw(cx);
    let again = widths(&view, cx);
    for (before, now) in three.iter().zip(&again) {
        assert!((before - now).abs() < 2.0, "{three:?} then {again:?}");
    }
    let _ = ItemRef::file("");
    let _ = FileOpen::picked(&file);
}

// ---- F7: the Settings sheet owns the keyboard while it is open ------------------------------------

#[gpui::test]
fn escape_closes_settings_even_when_something_else_took_the_keyboard(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let terminal = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    show(&view, cx, terminal.id);
    wait_until(cx, "the terminal to have the keyboard", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()) == Some(terminal.id)
    });
    click_region(&view, cx, Region::Toolbar("settings"));
    draw(cx);
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_some()));

    // A session shown while the sheet is open (a notification's Review, `hutan ui open`, a
    // tab the daemon opens) takes the keyboard to its terminal.
    view.update_in(cx, |wb, window, cx| {
        wb.open_session(terminal.id, window, cx)
    });
    draw(cx);
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.settings_sheet().is_none()),
        "Escape closes Settings, it does not reach the terminal behind"
    );
}

// ---- F9: the first paint after the page gets wider is the right one ------------------------------

/// One frame, as the platform draws it; how many callbacks for the next frame it ran.
fn frame(cx: &mut VisualTestContext) -> usize {
    let ran = cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.simulate_next_frame(cx)
    });
    cx.run_until_parked();
    ran
}

#[gpui::test]
fn a_page_that_gets_wide_enough_shows_its_today_button_and_rail_within_two_frames(
    cx: &mut TestAppContext,
) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    let (view, cx) = common::assistant::open(cx, &env.daemon);
    cx.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(700.0)));
    let panel = open_assistant(&view, cx);
    wait_until(cx, "the folded page", |cx| {
        panel.read_with(cx, |p, _| p.folded())
    });
    let painted = |cx: &VisualTestContext, name: &'static str| {
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant(name)))
            .is_some()
    };
    assert!(!painted(cx, "today"), "no Today button on a folded page");

    // The right panel goes away (here: the window grows): the frame that measures the wider page
    // and the one after it are all the user gets before the next input, and the second is the
    // right one. (The real platform draws only when asked: the frame that crosses the line
    // asks for the next with `request_animation_frame`.)
    cx.simulate_resize(gpui::size(gpui::px(3000.0), gpui::px(1200.0)));
    frame(cx);
    frame(cx);
    assert!(!panel.read_with(cx, |p, _| p.folded()));
    assert!(painted(cx, "today"), "the Today button");
    assert!(painted(cx, "column"), "the rail's column");
}
