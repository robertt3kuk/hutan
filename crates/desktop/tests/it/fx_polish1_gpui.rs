#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! FX-POLISH-1: the e2e pass's polish list, as far as it is behaviour of the workbench shell:
//! the empty centre after the last tab closed, the nav that does not shift.

use crate::common;

use common::{draw, offline_connection, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::{Region, Workbench};

fn open_offline(cx: &mut TestAppContext) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    open_with(cx, settings, connection, None)
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

#[gpui::test]
fn closing_the_last_tab_shows_the_quiet_empty_centre(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx);
    draw(cx);
    let pane = view.read_with(cx, |wb, _| wb.state().first_pane_id());
    wait_until(cx, "the welcome tab", |cx| {
        drawn(&view, cx, Region::Tab(pane, 0))
    });
    assert!(!drawn(&view, cx, Region::Launcher("empty-centre")));

    // × on the only tab.
    let tab = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Tab(pane, 0)))
        .unwrap();
    cx.simulate_click(
        gpui::point(tab.right() - gpui::px(10.0), tab.center().y),
        Modifiers::none(),
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().pane(pane).unwrap().items.len()),
        0
    );
    wait_until(cx, "the empty centre", |cx| {
        drawn(&view, cx, Region::Launcher("empty-centre"))
    });
    assert!(
        drawn(&view, cx, Region::Launcher("empty-start")),
        "one action: Start…"
    );
    assert_eq!(
        cx.window_title().as_deref(),
        Some("Hutan"),
        "the app's name, not 'Hutan — Hutan'"
    );
}
