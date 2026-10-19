#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! FX-1: regressions of cached views — hover on rows of a cached panel, the resize handles of
//! the docks and the splits in a steady (replayed) frame, and the desktop's single-instance
//! socket the daemon looks at before it puts its own menu bar item up.

mod common;

use std::time::Duration;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{
    Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, point, px,
};
use hutan_desktop::workbench::model::DockPosition;
use hutan_desktop::workbench::panels::SessionsPanel;
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
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

/// Frames until nothing changes: the cached panels are replayed from here on.
fn settle(cx: &mut VisualTestContext) {
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(50));
        draw(cx);
    }
}

fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for i in 1..=8 {
        let t = i as f32 / 8.0;
        let p = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        cx.simulate_mouse_move(p, MouseButton::Left, Modifiers::none());
    }
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> gpui::Bounds<Pixels> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} was not drawn"))
}

fn size(view: &Entity<Workbench>, cx: &VisualTestContext, pos: DockPosition) -> f32 {
    view.read_with(cx, |wb, _| wb.state().docks.get(pos).size)
}

/// The sidebar is a cached view: moving the pointer over another row must render it (the hover
/// × and the row's hover background are painted by that render), after a click on a row too.
#[gpui::test]
fn hovering_a_row_of_the_cached_sidebar_renders_the_panel_again(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let a = daemon.sh("sleep 30");
    let b = daemon.sh("sleep 30");
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "both rows", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::SessionClose(a.id)).is_some()
                && wb.region_bounds(Region::SessionClose(b.id)).is_some()
        })
    });
    settle(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<SessionsPanel>("sessions"))
        .expect("the sessions panel");
    let renders = |cx: &mut VisualTestContext| panel.read_with(cx, |p, _| p.renders());
    let beside_close = |cx: &VisualTestContext, id| {
        let close = bounds(&view, cx, Region::SessionClose(id));
        point(close.origin.x - px(60.0), close.center().y)
    };

    // A click selects `a`, then the pointer goes to `b`, back to `a`, and to `b` again.
    let at = beside_close(cx, a.id);
    cx.simulate_click(at, Modifiers::none());
    settle(cx);
    for id in [b.id, a.id, b.id] {
        let before = renders(cx);
        let at = beside_close(cx, id);
        cx.simulate_mouse_move(at, None, Modifiers::none());
        assert!(
            renders(cx) > before,
            "moving onto the row of {id} did not render the sidebar: its hover × and background stay stale"
        );
    }
}

/// Every dock edge and the split dividers resize in a steady frame, with sessions listed.
#[gpui::test]
fn dock_edges_and_split_dividers_resize_in_a_steady_frame(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 30");
    let (view, cx) = open(cx, &daemon);
    settle(cx);

    for (pos, grow) in [(DockPosition::Left, 1.0), (DockPosition::Right, -1.0)] {
        for y in [106.0, 300.0, 450.0, 600.0, 759.0] {
            let dock = bounds(&view, cx, Region::Dock(pos));
            let before = size(&view, cx, pos);
            let edge_x = match pos {
                DockPosition::Left => dock.right() - px(2.0),
                _ => dock.left() + px(2.0),
            };
            let edge = point(edge_x, px(y));
            drag(cx, edge, edge + point(px(40.0 * grow), px(0.0)));
            settle(cx);
            let after = size(&view, cx, pos);
            assert!(
                (after - before - 40.0).abs() < 1.0,
                "{pos:?} dock edge dragged at y={y}: {before} -> {after}"
            );
        }
    }

    cx.dispatch_action(actions::ToggleBottomDock);
    settle(cx);
    let bottom = bounds(&view, cx, Region::Dock(DockPosition::Bottom));
    let before = size(&view, cx, DockPosition::Bottom);
    let edge = point(px(700.0), bottom.top() + px(2.0));
    drag(cx, edge, edge + point(px(0.0), px(-30.0)));
    assert!(size(&view, cx, DockPosition::Bottom) > before + 25.0);

    cx.dispatch_action(actions::SplitRight);
    settle(cx);
    let panes: Vec<_> = view.read_with(cx, |wb, _| {
        wb.state().panes().iter().map(|p| p.id).collect()
    });
    let first = bounds(&view, cx, Region::Pane(panes[0]));
    let divider = point(first.right(), first.origin.y + first.size.height / 2.0);
    drag(cx, divider, divider + point(px(80.0), px(0.0)));
    let after = bounds(&view, cx, Region::Pane(panes[0]));
    assert!(
        after.size.width > first.size.width + px(60.0),
        "the split divider did not move"
    );
}

/// The daemon decides whether a desktop runs by this socket; the name is the daemon's constant.
#[test]
fn the_desktop_listens_where_the_daemon_looks_for_it() {
    assert_eq!(
        hutan_desktop::instance::SOCKET_FILE,
        hutan_daemon::menu_bar::DESKTOP_SOCKET_FILE
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_sees_a_desktop_that_holds_the_instance_claim() {
    struct Nothing;
    impl hutan_desktop::instance::ActivationSink for Nothing {
        fn activate(&self) {}
    }
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join(hutan_desktop::instance::SOCKET_FILE);
    assert!(!hutan_daemon::menu_bar::desktop_running(&socket).await);
    let claim = match hutan_desktop::instance::claim(dir.path(), Nothing).unwrap() {
        hutan_desktop::instance::Claim::Primary(primary) => primary,
        hutan_desktop::instance::Claim::Running => panic!("nobody holds the claim"),
    };
    assert!(hutan_daemon::menu_bar::desktop_running(&socket).await);
    drop(claim);
    assert!(!hutan_daemon::menu_bar::desktop_running(&socket).await);
}

/// Pressing on the dock edge at the height of a nav row resizes; the row under it gets no click.
#[gpui::test]
fn a_press_on_the_left_dock_edge_at_a_nav_row_does_not_click_the_row(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    settle(cx);
    let dock = bounds(&view, cx, Region::Dock(DockPosition::Left));
    let search = bounds(&view, cx, Region::Nav("search"));
    let edge = point(dock.right() - px(1.5), search.center().y);
    cx.simulate_click(edge, Modifiers::none());
    settle(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.picker().is_none()),
        "the Search row was clicked through the dock edge"
    );
    let before = size(&view, cx, DockPosition::Left);
    drag(cx, edge, edge + point(px(-35.0), px(0.0)));
    settle(cx);
    assert!((size(&view, cx, DockPosition::Left) - before + 35.0).abs() < 1.0);
    assert!(view.read_with(cx, |wb, _| wb.picker().is_none()));
}

/// The sidebar's list is a cached view: widening the dock must lay its rows out at the new width
/// (the hover × sits at the row's right end).
#[gpui::test]
fn resizing_the_left_dock_resizes_the_session_rows(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let a = daemon.sh("sleep 30");
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the row", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::SessionClose(a.id)).is_some()
        })
    });
    settle(cx);
    let close_right =
        |cx: &VisualTestContext| bounds(&view, cx, Region::SessionClose(a.id)).right();
    let before = close_right(cx);
    let dock = bounds(&view, cx, Region::Dock(DockPosition::Left));
    assert!(
        before > dock.right() - px(40.0),
        "the row card ends at {before:?}, far short of the dock's edge {:?}: it is as wide as its title",
        dock.right()
    );
    let edge = point(dock.right() - px(2.0), px(500.0));
    drag(cx, edge, edge + point(px(80.0), px(0.0)));
    settle(cx);
    let after = close_right(cx);
    assert!(
        (f32::from(after) - f32::from(before) - 80.0).abs() < 2.0,
        "the row's right end stayed at {before:?} (now {after:?}) after the dock grew by 80"
    );
}
