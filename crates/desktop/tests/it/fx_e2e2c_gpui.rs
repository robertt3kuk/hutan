#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! FX-E2E2-C: the tab strip keeps the active tab in view and caps long titles, the Files panel
//! decorates changed files, the Start sheet's default *What*, the settings row's help.

use crate::common;

use common::{draw, offline_connection, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::{Region, Workbench};

fn open_offline(cx: &mut TestAppContext) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    open_with(cx, settings, connection, None)
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

/// Whether tab `index` of `pane` lies inside the pane's width (the strip spans the pane).
fn tab_visible(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    pane: hutan_desktop::workbench::model::PaneId,
    index: usize,
) -> bool {
    let (Some(tab), Some(strip)) = (
        bounds(view, cx, Region::Tab(pane, index)),
        bounds(view, cx, Region::Pane(pane)),
    ) else {
        return false;
    };
    tab.left() >= strip.left() - gpui::px(1.0) && tab.right() <= strip.right() + gpui::px(1.0)
}

#[gpui::test]
fn the_active_tab_scrolls_into_view_when_the_strip_overflows_and_long_titles_are_capped(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open_offline(cx);
    draw(cx);
    let pane = view.read_with(cx, |wb, _| wb.state().first_pane_id());
    let count = 14;
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            for n in 0..count {
                s.add_item(
                    pane,
                    ItemRef::placeholder(format!("A tab with a long title {n}")),
                )
                .unwrap();
            }
            // A chat-like title far wider than any tab should be.
            s.add_item(pane, ItemRef::placeholder("x".repeat(300)))
                .unwrap();
        });
    });
    let items = view.read_with(cx, |wb, _| wb.state().pane(pane).unwrap().items.len());
    // The newest tab is active: the strip shows it though it overflows.
    wait_until(cx, "the newest tab in view", |cx| {
        tab_visible(&view, cx, pane, items - 1)
    });
    // The overflow is real: the first tab was scrolled out.
    assert!(!tab_visible(&view, cx, pane, 0), "the strip overflows");
    // The 300-character title is capped.
    let last = bounds(&view, cx, Region::Tab(pane, items - 1)).unwrap();
    assert!(
        last.size.width < gpui::px(300.0),
        "a long title is cut at a maximum width: {last:?}"
    );

    // Activating the first tab brings it back, on every activation.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.set_active_item(pane, 0).unwrap();
        });
    });
    wait_until(cx, "the first tab in view", |cx| {
        tab_visible(&view, cx, pane, 0)
    });
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.set_active_item(pane, 7).unwrap();
        });
    });
    wait_until(cx, "a middle tab in view", |cx| {
        tab_visible(&view, cx, pane, 7)
    });
}
