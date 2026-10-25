#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! SANDBOX-2: a terminal that runs inside Hutan's Seatbelt boundary wears a chip that says what
//! the boundary holds back and what it leaves open, read from what the daemon applied; a
//! session without one wears nothing.

use crate::common;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::Contain;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{Request, SessionCreate, SessionResult, SessionSpec, TerminalSpec};
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

fn terminal(
    daemon: &TestDaemon,
    cwd: &std::path::Path,
    contain: Option<Contain>,
) -> hutan_core::SessionId {
    let created: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
        workspace_id: None,
        spec: SessionSpec::Terminal(TerminalSpec {
            command: vec!["/bin/sh".into(), "-c".into(), "exec cat".into()],
            cwd: Some(cwd.to_owned()),
            contain,
            ..TerminalSpec::default()
        }),
    }));
    created.session.id
}

#[gpui::test]
fn a_contained_terminal_wears_the_chip_with_what_was_applied_and_a_plain_one_wears_none(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path().canonicalize().unwrap();

    let plain = terminal(&daemon, &cwd, None);
    wait_until(cx, "the plain session to be listed", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).session(plain).is_some())
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(plain, window, cx));
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Contain("chip")))
            .is_none(),
        "no boundary, no chip"
    );

    let contained = terminal(
        &daemon,
        &cwd,
        Some(Contain {
            network: Some(false),
            extra_write: Vec::new(),
        }),
    );
    wait_until(cx, "the contained session to be listed", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(contained)
                .is_some_and(|s| s.containment.is_some())
        })
    });
    let boundary = view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .session(contained)
            .and_then(|s| s.containment.clone())
            .unwrap()
    });
    assert!(boundary.applied && !boundary.network);
    assert_eq!(
        boundary.summary(),
        "writes limited to this worktree · can read your files · network off"
    );
    view.update_in(cx, |wb, window, cx| wb.open_session(contained, window, cx));
    wait_until(cx, "the chip to be drawn", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Contain("chip")).is_some()
        })
    });
}
