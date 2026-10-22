#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! FX-E2E4: fixes from the real-window pass over the UI wave (docs/spikes/2026-10-23-e2e-usertest-4.md).

use crate::common;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::DockPosition;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

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
