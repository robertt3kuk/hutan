#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! `UX-ATTN-3` against an in-process daemon: while the window is in front, a request for a
//! session that is not on screen shows a toast (two at most, five seconds on the test clock,
//! hover pauses, Escape dismisses) and a dot on the session's tab until it is seen; a window
//! behind others and a muted session say nothing in the window.

use crate::common;

use std::time::Duration;

use common::agents::{Node, Reporter};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point};
use hutan_core::SessionId;
use hutan_desktop::sessions::calls::MAX_TOASTS;
use hutan_desktop::workbench::actions::ReviewRequest;
use hutan_desktop::workbench::model::{ItemRef, PaneId};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{Request, SessionResult};
use hutan_protocol::notify::NotifyMute;
use hutan_settings::{SettingsPaths, SettingsStore};

/// A window on `daemon` with one live session per title, every one of them open as a tab (in
/// the order given) and the first one shown and reported to the daemon as looked at.
fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    titles: &[&str],
) -> (Entity<Workbench>, &'a mut VisualTestContext, Vec<SessionId>) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let ids: Vec<SessionId> = titles
        .iter()
        .map(|title| daemon.sh_titled(title, "sleep 300").id)
        .collect();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the sessions to be listed", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && ids.iter().all(|id| store.session(*id).is_some())
        })
    });
    for id in ids.iter().rev() {
        view.update_in(cx, |wb, window, cx| wb.open_session(*id, window, cx));
    }
    let first = ids[0];
    wait_until(cx, "the first session to be reported as looked at", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).focus_reported() == Some(Some(first))
        })
    });
    (view, cx, ids)
}

fn blocked(daemon: &TestDaemon, session: SessionId) {
    Reporter::new(session, "omp-extension", "omp").report(daemon, Node::main("m").blocked());
}

fn toasts(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<(SessionId, String)> {
    view.read_with(cx, |wb, _| {
        wb.attention_toasts()
            .into_iter()
            .map(|t| (t.session, t.text))
            .collect()
    })
}

fn pane(view: &Entity<Workbench>, cx: &VisualTestContext) -> PaneId {
    view.read_with(cx, |wb, _| wb.state().first_pane_id())
}

/// The index of `session`'s tab in the first pane.
fn tab_of(view: &Entity<Workbench>, cx: &VisualTestContext, session: SessionId) -> usize {
    view.read_with(cx, |wb, _| {
        let pane = wb.state().first_pane_id();
        wb.state()
            .pane(pane)
            .unwrap()
            .items
            .iter()
            .position(|item| {
                matches!(
                    item,
                    ItemRef::Terminal { session: s }
                        if *s == session.to_string()
                )
            })
            .expect("the session has a tab")
    })
}

fn has_dot(view: &Entity<Workbench>, cx: &mut VisualTestContext, session: SessionId) -> bool {
    draw(cx);
    let (pane, index) = (pane(view, cx), tab_of(view, cx, session));
    view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::TabCall(pane, index)).is_some()
    })
}

#[gpui::test]
fn a_request_for_a_session_off_screen_shows_a_toast_and_a_tab_dot_until_it_is_seen(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx, ids) = open(cx, &daemon, &["shown", "elsewhere"]);
    let (shown, elsewhere) = (ids[0], ids[1]);
    assert!(cx.update(|window, _| window.is_window_active()));
    assert!(toasts(&view, cx).is_empty());
    assert!(!has_dot(&view, cx, elsewhere));

    blocked(&daemon, elsewhere);
    wait_until(cx, "the toast", |cx| !toasts(&view, cx).is_empty());
    let shown_toasts = toasts(&view, cx);
    assert_eq!(shown_toasts.len(), 1);
    assert_eq!(shown_toasts[0].0, elsewhere);
    assert!(
        shown_toasts[0]
            .1
            .starts_with("elsewhere is waiting for you"),
        "{}",
        shown_toasts[0].1
    );
    // The tab of the session that asked has the dot; the one on screen has none.
    wait_until(cx, "the dot", |cx| has_dot(&view, cx, elsewhere));
    assert!(!has_dot(&view, cx, shown));
    // The toast is drawn with its Review button.
    draw(cx);
    view.read_with(cx, |wb, _| {
        assert!(
            wb.region_bounds(Region::AttentionToast(0, "toast"))
                .is_some()
        );
        assert!(
            wb.region_bounds(Region::AttentionToast(0, "review"))
                .is_some()
        );
    });

    // Looking at the session ends both: its tab is shown, the dot and the toast go. The request
    // still waits, in the Requests group.
    view.update_in(cx, |wb, window, cx| wb.open_session(elsewhere, window, cx));
    wait_until(cx, "the toast to go", |cx| toasts(&view, cx).is_empty());
    assert!(!has_dot(&view, cx, elsewhere));
    view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        assert!(!store.has_call(elsewhere));
        assert_eq!(store.requests(), [elsewhere]);
    });
    drop(daemon);
}

#[gpui::test]
fn at_most_two_toasts_stay_five_seconds_and_a_hovered_one_waits(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, ids) = open(cx, &daemon, &["shown", "b", "c", "d"]);
    let (b, c, d) = (ids[1], ids[2], ids[3]);

    // Three requests, one after the other: the oldest toast is replaced.
    for session in [b, c, d] {
        blocked(&daemon, session);
        wait_until(cx, "the toast of the session", |cx| {
            toasts(&view, cx).iter().any(|(s, _)| *s == session)
        });
    }
    let on_screen: Vec<SessionId> = toasts(&view, cx).iter().map(|(s, _)| *s).collect();
    assert_eq!(on_screen.len(), MAX_TOASTS);
    assert_eq!(on_screen, [c, d], "the oldest was replaced");
    // Every session that asked has its dot, the replaced one too: the toast is gone, the call
    // is not.
    for session in [b, c, d] {
        assert!(has_dot(&view, cx, session));
    }

    // Four seconds in: still there. Hovering the lower (newer) toast stops its clock; the
    // clock of the test does not move between the pushes, so both have a second left.
    cx.executor().advance_clock(Duration::from_secs(4));
    draw(cx);
    assert_eq!(toasts(&view, cx).len(), 2);
    let lower = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::AttentionToast(1, "toast"))
        })
        .unwrap();
    cx.simulate_mouse_move(lower.center(), None, Modifiers::none());
    draw(cx);

    // The other one's five seconds end; the hovered one stays however long the pointer rests.
    cx.executor().advance_clock(Duration::from_secs(2));
    draw(cx);
    assert_eq!(
        toasts(&view, cx)
            .iter()
            .map(|(s, _)| *s)
            .collect::<Vec<_>>(),
        [d]
    );
    cx.executor().advance_clock(Duration::from_secs(120));
    draw(cx);
    assert_eq!(toasts(&view, cx).len(), 1, "a hovered toast does not go");

    // The pointer leaves: the second it had left starts again, and then it goes.
    cx.simulate_mouse_move(point(gpui::px(1.0), gpui::px(1.0)), None, Modifiers::none());
    draw(cx);
    cx.executor().advance_clock(Duration::from_millis(900));
    draw(cx);
    assert_eq!(toasts(&view, cx).len(), 1);
    cx.executor().advance_clock(Duration::from_millis(100));
    draw(cx);
    assert!(toasts(&view, cx).is_empty());
    // The dots stay until the sessions are looked at.
    assert!(has_dot(&view, cx, d));
    drop(daemon);
}

#[gpui::test]
fn escape_dismisses_the_toasts_and_review_opens_the_session(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, ids) = open(cx, &daemon, &["shown", "b", "c"]);
    let (shown, b, c) = (ids[0], ids[1], ids[2]);
    blocked(&daemon, b);
    wait_until(cx, "the toast", |cx| toasts(&view, cx).len() == 1);

    // Escape with the keyboard where it was (the terminal or window) is never taken: the
    // toast stays.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        toasts(&view, cx).len(),
        1,
        "Escape elsewhere leaves the toast"
    );

    // With focus on the toast (Tab or a click puts it there), Escape dismisses.
    draw(cx);
    let focus = view
        .read_with(cx, |wb, _| wb.attention_toast_focus())
        .expect("a toast was drawn");
    cx.update(|window, cx| focus.focus(window, cx));
    draw(cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        toasts(&view, cx).is_empty(),
        "Escape on the toast dismisses"
    );
    assert!(has_dot(&view, cx, b), "the dot stays");

    // Review (⌘⇧R) shows the session of the newest toast.
    blocked(&daemon, c);
    wait_until(cx, "the second toast", |cx| toasts(&view, cx).len() == 1);
    assert_eq!(toasts(&view, cx)[0].0, c);
    cx.dispatch_action(ReviewRequest);
    cx.run_until_parked();
    wait_until(cx, "the session to be shown", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session() == Some(c))
    });
    assert!(toasts(&view, cx).is_empty());
    assert!(!has_dot(&view, cx, c));

    // With no toast it shows the request that has waited longest (b's: still waiting).
    view.update_in(cx, |wb, window, cx| wb.open_session(shown, window, cx));
    cx.dispatch_action(ReviewRequest);
    cx.run_until_parked();
    wait_until(cx, "the oldest request to be shown", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session() == Some(b))
    });
    drop(daemon);
}

#[gpui::test]
fn a_window_behind_others_keeps_the_dot_but_shows_no_toast(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, ids) = open(cx, &daemon, &["shown", "b"]);
    let b = ids[1];
    cx.deactivate_window();
    wait_until(cx, "the deactivation to be reported", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).focus_reported() == Some(None)
        })
    });
    blocked(&daemon, b);
    wait_until(cx, "the call to reach the window", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).has_call(b))
    });
    assert!(
        toasts(&view, cx).is_empty(),
        "the system's notification is for that"
    );
    assert!(has_dot(&view, cx, b));
    drop(daemon);
}

#[gpui::test]
fn a_muted_session_gets_no_toast_and_no_dot(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, ids) = open(cx, &daemon, &["shown", "muted", "loud"]);
    let (muted, loud) = (ids[1], ids[2]);
    let _: SessionResult = daemon.call(Request::NotifyMute(NotifyMute {
        session_id: muted,
        muted: true,
    }));
    blocked(&daemon, muted);
    blocked(&daemon, loud);
    wait_until(cx, "the loud session's toast", |cx| {
        toasts(&view, cx).iter().any(|(s, _)| *s == loud)
    });
    // Both asked in order: the daemon would have raised the muted one first.
    assert_eq!(toasts(&view, cx).len(), 1);
    assert!(!has_dot(&view, cx, muted));
    assert!(has_dot(&view, cx, loud));
    // It still waits: muting silences, it does not hide the request.
    view.read_with(cx, |wb, cx| {
        assert!(wb.sessions().read(cx).requests().contains(&muted));
    });
    drop(daemon);
}
