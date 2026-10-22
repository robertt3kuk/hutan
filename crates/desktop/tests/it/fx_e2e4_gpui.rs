#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! FX-E2E4: fixes from the real-window pass over the UI wave (docs/spikes/2026-10-23-e2e-usertest-4.md).

use crate::common;

use common::agents::{Node, Reporter};
use common::{ProcessDaemon, TestDaemon, draw, offline_connection, open_with, settle, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px};
use hutan_client::{Connection, ConnectionState};
use hutan_core::SessionId;
use hutan_desktop::theme::DensityExt as _;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::{DockPosition, PanelDest, PanelId, TermSplit};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{Request, SessionCreate, SessionResult, SessionSpec, TerminalSpec};
use hutan_settings::{SettingsPaths, SettingsStore};
use hutan_terminal_view::ViewStatus;

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    )
}

// ---- 1. F1: popovers above the splitters -------------------------------------------------------

/// A popover button that lies on the left dock's resize handle takes the click, not the handle
/// (the handle is painted before every overlay).
#[gpui::test]
fn a_popover_button_over_a_dock_edge_takes_the_click(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    cx.dispatch_action(actions::daemon::Status);
    cx.run_until_parked();
    draw(cx);
    let region =
        |cx: &VisualTestContext, region| view.read_with(cx, |wb, _| wb.region_bounds(region));
    let button = region(cx, Region::Popover("daemon-stop")).expect("the popover's Stop is drawn");
    // The dock's right edge is its handle; put it through the button, off its centre.
    let wanted = f32::from(button.origin.x) + 65.0;
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| s.resize_dock(DockPosition::Left, wanted + 3.0));
    });
    draw(cx);
    draw(cx);
    let button = region(cx, Region::Popover("daemon-stop")).expect("Stop is still drawn");
    let dock = region(cx, Region::Dock(DockPosition::Left)).expect("the left dock");
    let at = point(dock.right() - px(3.0), button.center().y);
    assert!(
        button.contains(&at),
        "the handle crosses the button: {at:?} in {button:?}"
    );
    let socket = daemon.paths.socket.clone();
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    wait_until(cx, "the stop to run", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_stopped()) || !socket.exists()
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.left.size),
        wanted + 3.0,
        "the dock was not dragged"
    );
}

// ---- 2. F2, F3: a stopped daemon, said once ----------------------------------------------------

/// `Stopped · Start` starts the daemon with the one click, no popover opens, and the sessions
/// that come back after the user's own stop and start raise no restart card or chip.
#[gpui::test]
fn the_stopped_chip_starts_the_daemon_at_once_without_a_restart_card(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    let session = process.create_session(&["sleep", "600"]);
    let settings = SettingsStore::new(SettingsPaths::in_dir(&process.dir.path().join("config")));
    let (view, cx) = open_with(cx, settings, Connection::start(process.options()), None);
    wait_until(cx, "the session", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == 1
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(session.id, window, cx));
    wait_until(cx, "the terminal", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(session.id)
                .is_some_and(|v| v.read(cx).status() == &ViewStatus::Live)
        })
    });

    // The popover's Stop; the daemon stays down.
    cx.dispatch_action(actions::daemon::Status);
    cx.run_until_parked();
    draw(cx);
    let stop = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Popover("daemon-stop")))
        .expect("Stop");
    cx.simulate_click(stop.center(), Modifiers::none());
    cx.run_until_parked();
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the stop to finish", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_busy().is_none() && wb.daemon_stopped()
        })
    });
    assert_eq!(view.read_with(cx, |wb, _| wb.daemon_popover_open()), true);
    cx.dispatch_action(hutan_desktop::workbench::actions::palette::Dismiss);
    draw(cx);
    assert!(!view.read_with(cx, |wb, _| wb.daemon_popover_open()));

    // The popover, opened while stopped, has one Start and no banner repeating it.
    cx.dispatch_action(actions::daemon::Status);
    cx.run_until_parked();
    draw(cx);
    let drawn = |cx: &VisualTestContext, name| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Popover(name)).is_some()
        })
    };
    assert!(drawn(cx, "daemon-start"));
    assert!(!drawn(cx, "daemon-banner-start"));
    cx.dispatch_action(hutan_desktop::workbench::actions::palette::Dismiss);
    draw(cx);

    // One click on `Stopped · Start`.
    let chip = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::StatusItem("connection"))
        })
        .expect("the connection item");
    cx.simulate_click(chip.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(
        !view.read_with(cx, |wb, _| wb.daemon_popover_open()),
        "no popover to click again"
    );
    wait_until(cx, "the daemon to start again", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.daemon_busy().is_none()
                && wb.sessions().read(cx).connection_state() == ConnectionState::Connected
                && !wb.daemon_stopped()
        })
    });
    // The respawned session is not news: no card, and no chip once the acknowledgement is back.
    wait_until(cx, "the respawned session", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.sessions().len() == 1 && store.sessions().iter().all(|s| s.state.is_live())
        })
    });
    wait_until(cx, "the recovery to be acknowledged", |cx| {
        assert!(
            !view.read_with(cx, |wb, _| wb.recovery_summary_open()),
            "no restart card after the user's own stop and start"
        );
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .sessions()
                .iter()
                .all(|s| s.recovery.is_none())
        })
    });
}

// ---- 3. F5: Undo of Close Split ----------------------------------------------------------------

fn terminal(daemon: &TestDaemon) -> SessionId {
    let created: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
        workspace_id: None,
        spec: SessionSpec::Terminal(TerminalSpec {
            command: vec!["sh".into(), "-c".into(), "sleep 120".into()],
            env: [("PS1".to_owned(), "$ ".to_owned())].into(),
            ..TerminalSpec::default()
        }),
    }));
    created.session.id
}

fn split_tab(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<TermSplit> {
    view.read_with(cx, |wb, _| {
        wb.state()
            .panes()
            .into_iter()
            .flat_map(|p| p.items.iter())
            .find_map(|i| i.as_term_split().cloned())
    })
}

fn tab_count(view: &Entity<Workbench>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |wb, _| {
        wb.state().panes().into_iter().map(|p| p.items.len()).sum()
    })
}

/// Undo after ⌘W in a split puts the session back into the split, in its place and with its
/// share, with no tab of its own and no restart card.
#[gpui::test]
fn undo_of_close_split_puts_the_session_back_in_its_split(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let a = terminal(&daemon);
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(a, window, cx));
    wait_until(cx, "the terminal", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(a).is_some())
    });
    cx.simulate_keystrokes("cmd-d");
    wait_until(cx, "two splits", |cx| {
        split_tab(&view, cx).is_some_and(|s| s.len() == 2)
    });
    // Make the shares uneven so that "the same size" means something.
    cx.simulate_keystrokes("ctrl-cmd-right");
    draw(cx);
    let before = split_tab(&view, cx).unwrap();
    let tabs_before = tab_count(&view, cx);

    cx.simulate_keystrokes("cmd-w");
    wait_until(cx, "the split to close", |cx| {
        split_tab(&view, cx).is_none()
    });
    wait_until(cx, "the Undo toast", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::CloseToast("undo")).is_some()
        })
    });
    let undo = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::CloseToast("undo")))
        .unwrap();
    cx.simulate_click(undo.center(), Modifiers::none());
    wait_until(cx, "the split to come back", |cx| {
        split_tab(&view, cx).is_some()
    });
    assert_eq!(
        split_tab(&view, cx).unwrap(),
        before,
        "same place, same shares"
    );
    assert_eq!(tab_count(&view, cx), tabs_before, "no tab of its own");

    // The reopen is not a restart: no card, and no chip once the acknowledgement is back.
    wait_until(cx, "the recovery to be acknowledged", |cx| {
        assert!(!view.read_with(cx, |wb, _| wb.recovery_summary_open()));
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .sessions()
                .iter()
                .all(|s| s.recovery.is_none())
        })
    });
}

// ---- 4. F6: toasts -----------------------------------------------------------------------------

/// An Undo toast is as high with its second line (the note) as without it, so the next one does
/// not jump and Undo stays under the pointer.
#[gpui::test]
fn an_undo_toast_keeps_its_height_with_or_without_a_note(cx: &mut TestAppContext) {
    let (connection, settings, _) = offline_connection();
    let (view, cx) = open_with(cx, settings, connection, None);
    let height = |view: &Entity<Workbench>, cx: &mut VisualTestContext| {
        settle(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(Region::LayoutToast("toast")))
            .expect("the toast is drawn")
            .size
            .height
    };
    // The first change on a built-in preset says why a Custom preset appeared: two lines.
    view.update(cx, |wb, cx| {
        wb.place_panel(
            &PanelId::new("changes"),
            PanelDest::Dock(DockPosition::Left),
            cx,
        );
    });
    assert!(view.read_with(cx, |wb, _| wb.layout_toast().unwrap().note().is_some()));
    let with_note = height(&view, cx);
    // The next one goes into Custom itself: one line.
    view.update(cx, |wb, cx| {
        wb.place_panel(
            &PanelId::new("history"),
            PanelDest::Dock(DockPosition::Left),
            cx,
        );
    });
    assert!(view.read_with(cx, |wb, _| wb.layout_toast().unwrap().note().is_none()));
    let without_note = height(&view, cx);
    assert_eq!(with_note, without_note);
}

/// A second call of a session that already has a toast does not change the toast's words.
#[gpui::test]
fn a_toast_keeps_its_text_when_the_same_session_calls_again(cx: &mut TestAppContext) {
    use hutan_desktop::sessions::RequestCalled;
    use hutan_protocol::notify::{IntentKind, NotificationIntent};

    let daemon = TestDaemon::start();
    let (shown, elsewhere) = (terminal(&daemon), terminal(&daemon));
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the sessions", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.session(shown).is_some() && store.session(elsewhere).is_some()
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(shown, window, cx));
    let call = |kind, body: &str, n: u32| RequestCalled {
        intent: Box::new(NotificationIntent {
            id: format!("intent-{n}"),
            kind,
            session_id: elsewhere,
            workspace_id: None,
            title: "Claude Code".into(),
            body: body.into(),
            actions: Vec::new(),
            created_at_ms: 0,
            expires_at_ms: None,
        }),
    };
    let sessions = view.read_with(cx, |wb, _| wb.sessions().clone());
    sessions.update(cx, |_, cx| {
        cx.emit(call(
            IntentKind::ApprovalRequired,
            "Edit src/lib.rs: adds a test",
            1,
        ));
    });
    cx.run_until_parked();
    let text = |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.attention_toasts());
    let first = text(cx);
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(
        first[0].text.contains("asks to edit src/lib.rs"),
        "{}",
        first[0].text
    );
    sessions.update(cx, |_, cx| {
        cx.emit(call(IntentKind::AgentBlocked, "", 2));
    });
    cx.run_until_parked();
    let second = text(cx);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].text, first[0].text, "the words never change");
}

// ---- 5. F7, F8: dots ---------------------------------------------------------------------------

/// A tab that gets an attention dot while it is scrolled out of the strip is scrolled into view,
/// whole, so its dot is never cut off at the strip's edge.
#[gpui::test]
fn a_tab_that_gets_a_dot_is_scrolled_into_view(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let ids: Vec<SessionId> = (0..14)
        .map(|n| {
            daemon
                .sh_titled(&format!("a session with a longish title {n}"), "sleep 300")
                .id
        })
        .collect();
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the sessions", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && ids.iter().all(|id| store.session(*id).is_some())
        })
    });
    for id in &ids {
        view.update_in(cx, |wb, window, cx| wb.open_session(*id, window, cx));
        draw(cx);
    }
    settle(cx);
    let pane = view.read_with(cx, |wb, _| wb.state().first_pane_id());
    let region =
        |cx: &VisualTestContext, region| view.read_with(cx, |wb, _| wb.region_bounds(region));
    let strip = region(cx, Region::Pane(pane)).expect("the pane");
    let first = region(cx, Region::Tab(pane, 0)).expect("the first tab");
    assert!(
        first.origin.x < strip.origin.x,
        "the first tab is scrolled out: {first:?}"
    );

    Reporter::new(ids[0], "omp-extension", "omp").report(&daemon, Node::main("m").blocked());
    wait_until(cx, "the first tab's dot", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::TabCall(pane, 0)).is_some()
        })
    });
    settle(cx);
    let tab = region(cx, Region::Tab(pane, 0)).unwrap();
    assert!(
        tab.origin.x >= strip.origin.x - px(1.0) && tab.right() <= strip.right() + px(1.0),
        "the tab with a dot is inside the strip: {tab:?} in {strip:?}"
    );
}

/// The unread dot of a session row is at least 6 px and centred on the row's text line.
#[gpui::test]
fn the_unread_dot_is_six_pixels_and_centred_on_the_row(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let id = daemon.sh_titled("quiet", "sleep 300").id;
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the session", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).session(id).is_some())
    });
    view.update(cx, |wb, cx| {
        wb.sessions()
            .update(cx, |store, cx| assert!(store.mark_unread(id, cx)));
    });
    wait_until(cx, "the dot", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::SessionUnread(id)).is_some()
        })
    });
    settle(cx);
    let (dot, row) = view.read_with(cx, |wb, _| {
        (
            wb.region_bounds(Region::SessionUnread(id)).unwrap(),
            wb.region_bounds(Region::SessionRow(id)).unwrap(),
        )
    });
    assert!(
        dot.size.width >= px(6.0) && dot.size.height >= px(6.0),
        "{dot:?}"
    );
    // On the row's first (text) line, not below it: the line's middle is the row's middle for a
    // one-line row.
    let first_line = view.read_with(cx, |wb, _| wb.density().ui().sidebar_row);
    let off = (dot.center().y - (row.origin.y + px(first_line) / 2.0)).abs();
    assert!(
        off <= px(1.0),
        "the dot is {off:?} off the line's middle: {dot:?} in {row:?}"
    );
}

// ---- 6. F9: a zoomed split says so ---------------------------------------------------------------

#[gpui::test]
fn a_zoomed_split_says_so_on_its_tab_and_keeps_the_count(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let a = terminal(&daemon);
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(a, window, cx));
    wait_until(cx, "the terminal", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(a).is_some())
    });
    cx.simulate_keystrokes("cmd-d");
    wait_until(cx, "two splits", |cx| {
        split_tab(&view, cx).is_some_and(|s| s.len() == 2)
    });
    let pane = view.read_with(cx, |wb, _| wb.state().first_pane_id());
    let label = |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.split_badge_text(pane, 0));
    assert_eq!(label(cx).as_deref(), Some("⊞ 2"));
    settle(cx);
    let width = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| wb.region_bounds(Region::TabSplits(pane, 0)))
            .map(|b| b.size.width)
    };
    let plain = width(cx).expect("the badge is drawn");

    cx.simulate_keystrokes("cmd-shift-enter");
    settle(cx);
    assert_eq!(
        label(cx).as_deref(),
        Some("⊞ 2 · zoomed"),
        "the count stays"
    );
    assert!(
        width(cx).unwrap() > plain,
        "the badge is drawn with the cue"
    );

    cx.simulate_keystrokes("cmd-shift-enter");
    settle(cx);
    assert_eq!(label(cx).as_deref(), Some("⊞ 2"));
}

// ---- 7. F10: rail tooltips follow a reorder ----------------------------------------------------

#[gpui::test]
fn rail_tooltips_follow_the_icons_when_the_panels_are_reordered(cx: &mut TestAppContext) {
    let (connection, settings, _) = offline_connection();
    let (view, cx) = open_with(cx, settings, connection, None);
    let tips = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.rail_tooltips(DockPosition::Right, cx)
                .into_iter()
                .map(|(panel, tip)| (panel.to_string(), tip.to_string()))
                .collect::<Vec<_>>()
        })
    };
    let before = tips(cx);
    assert!(before.len() >= 2, "{before:?}");
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.docks.get_mut(DockPosition::Right).panels.reverse()
        });
    });
    draw(cx);
    let after = tips(cx);
    let expected: Vec<_> = before.into_iter().rev().collect();
    assert_eq!(after, expected, "every icon says its own panel");
}
