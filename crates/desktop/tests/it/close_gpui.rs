#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Closing sessions from the sidebar (protocol 0.30): the hover × closes an idle session without
//! a dialog and Undo brings it back; a busy session asks first (Cancel keeps it, Stop and close
//! closes it).

use crate::common;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::{SessionId, SessionState};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{AgentApplied, AgentReportRequest, AgentState, Request};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    open_with(cx, settings, daemon.connection(), None)
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, target: Region) {
    wait_until(cx, &format!("{target:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(target).is_some())
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(target))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
}

fn wait_sessions(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: &str, count: usize) {
    wait_until(cx, what, |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == count
        })
    });
}

fn state(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    id: SessionId,
) -> Option<SessionState> {
    view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).session(id).map(|s| s.state.clone())
    })
}

#[gpui::test]
fn the_hover_close_closes_an_idle_session_without_a_dialog_and_undo_brings_it_back(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let session = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    wait_sessions(&view, cx, "the session", 1);
    wait_until(cx, "running", |cx| {
        state(&view, cx, session.id) == Some(SessionState::Running)
    });

    click(&view, cx, Region::SessionClose(session.id));
    wait_sessions(&view, cx, "the session to be closed", 0);
    assert!(
        view.read_with(cx, |wb, _| !wb.modal_open()),
        "an idle session closes without a dialog"
    );
    wait_until(cx, "the toast", |cx| {
        view.read_with(cx, |wb, _| {
            wb.close_toast().is_some_and(|t| t.session == session.id)
        })
    });

    click(&view, cx, Region::CloseToast("undo"));
    wait_sessions(&view, cx, "the session to come back", 1);
    wait_until(cx, "running again", |cx| {
        state(&view, cx, session.id) == Some(SessionState::Running)
    });
    assert!(view.read_with(cx, |wb, _| wb.close_toast().is_none()));
}

#[gpui::test]
fn a_busy_session_asks_first_cancel_keeps_it_and_stop_and_close_closes_it(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let session = daemon.create_session(&["cat"], None);
    let _: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        pid: None,
        session_id: session.id,
        source: "fake-hooks".into(),
        agent: "Claude Code".into(),
        state: Some(AgentState::Working),
        message: None,
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
    let (view, cx) = open(cx, &daemon);
    wait_sessions(&view, cx, "the session", 1);
    wait_until(cx, "the agent working", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(session.id).is_some_and(|s| {
                s.agent
                    .as_ref()
                    .is_some_and(|a| a.state == AgentState::Working)
            })
        })
    });

    click(&view, cx, Region::SessionClose(session.id));
    wait_until(cx, "the confirmation", |cx| {
        view.read_with(cx, |wb, _| wb.modal_open())
    });
    let (title, message, ok) = view.read_with(cx, |wb, _| {
        let modal = wb.modal().unwrap();
        (
            modal.title.to_string(),
            modal.message.to_string(),
            modal.confirm_label.to_string(),
        )
    });
    assert!(title.starts_with("Close "), "{title}");
    assert!(message.contains("Claude Code is working"), "{message}");
    assert_eq!(ok, "Stop and close");

    // Cancel keeps it.
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    draw(cx);
    assert!(view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).session(session.id).is_some()
    }));
    assert_eq!(state(&view, cx, session.id), Some(SessionState::Running));

    // Stop and close.
    click(&view, cx, Region::SessionClose(session.id));
    wait_until(cx, "the confirmation again", |cx| {
        view.read_with(cx, |wb, _| wb.modal_open())
    });
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_sessions(&view, cx, "the session to be closed", 0);
}
