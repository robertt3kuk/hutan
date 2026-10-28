#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! AGENT-TERM-1: a terminal with a coding agent in it is that agent on every surface. The OSC
//! 7501 bytes OMP writes reach a real daemon through a real PTY (`cat` echoes them); the tab
//! and the sidebar row show the harness glyph, the agent's own title, the state, the question
//! or the error under the row, and a bell (until the tab is shown) when the agent waits or the
//! terminal rang while the tab was out of sight.

use crate::common;

use std::time::Duration;

use base64::Engine as _;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::icons::Icon;
use hutan_desktop::sessions::{TabState, agent_note, agent_subtitle, tab_marks};
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{SessionInfo, TerminalAgentState};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n[history]\nenabled = false\n",
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

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..6 {
        cx.executor().advance_clock(Duration::from_millis(60));
        draw(cx);
    }
}

fn show(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
    wait_until(cx, "the terminal", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(id).is_some())
    });
    settle(cx);
}

fn echo(daemon: &TestDaemon) -> SessionInfo {
    daemon.create_session(&["sh", "-c", "exec cat"], None)
}

/// What OMP writes (ST-terminated); the newline makes `cat` echo the line back as output.
fn osc(daemon: &TestDaemon, session: &SessionInfo, payload: &str) {
    daemon.send_input(
        session.terminal_id.unwrap(),
        format!("\x1b]{payload}\x1b\\\r").as_bytes(),
    );
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn info(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId) -> SessionInfo {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).session(id).cloned())
        .expect("the session is listed")
}

fn wait_agent(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    id: SessionId,
    what: &str,
    pred: impl Fn(&SessionInfo) -> bool,
) -> SessionInfo {
    wait_until(cx, what, |cx| pred(&info(view, cx, id)));
    info(view, cx, id)
}

fn in_state(state: TerminalAgentState) -> impl Fn(&SessionInfo) -> bool {
    move |s| s.terminal_agent.as_ref().is_some_and(|a| a.state == state)
}

fn tab_of(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId) -> (ItemRef, usize) {
    view.read_with(cx, |wb, _| {
        let pane = wb.state().first_pane_id();
        let items = &wb.state().pane(pane).unwrap().items;
        let index = items
            .iter()
            .position(|i| i.sessions().contains(&id.to_string().as_str()))
            .expect("the session has a tab");
        (items[index].clone(), index)
    })
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

fn pane(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
) -> hutan_desktop::workbench::model::PaneId {
    view.read_with(cx, |wb, _| wb.state().first_pane_id())
}

#[gpui::test]
fn an_agent_terminal_wears_the_harness_its_title_its_state_and_its_question(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let agent = echo(&daemon);
    let other = echo(&daemon);
    let (view, cx) = open(cx, &daemon);
    // The agent's tab first, another one in front of it: the agent is out of sight.
    show(&view, cx, agent.id);
    show(&view, cx, other.id);
    let (item, index) = tab_of(&view, cx, agent.id);
    let pane = pane(&view, cx);

    // A plain terminal: the terminal glyph, no marks.
    let (glyph, _) = view.read_with(cx, |wb, cx| wb.item_icon_and_title(&item, cx));
    assert_eq!(glyph, Icon::SessionTerminal);
    assert!(!drawn(&view, cx, Region::TabState(pane, index)));
    assert!(!drawn(&view, cx, Region::TabBell(pane, index)));

    // OMP starts: the tab is OMP's, called by the title the agent gave the session.
    osc(&daemon, &agent, "7501;state=idle:app=omp");
    osc(&daemon, &agent, "0;π > 👷 WORKER: worker");
    wait_agent(&view, cx, agent.id, "the agent's title", |s| {
        s.terminal_agent.as_ref().is_some_and(|a| a.title.is_some())
    });
    let (glyph, title) = view.read_with(cx, |wb, cx| wb.item_icon_and_title(&item, cx));
    assert_eq!(glyph, Icon::HarnessOmp);
    assert_eq!(title.as_ref(), "👷 WORKER: worker");
    // The sidebar row's kind glyph is the same one (`session_icon` draws both).
    let listed = info(&view, cx, agent.id);
    assert_eq!(
        hutan_desktop::workbench::panels::session_icon(&listed),
        Icon::HarnessOmp
    );
    settle(cx);
    // Idle: nothing but the glyph and the name.
    assert!(!drawn(&view, cx, Region::TabState(pane, index)));
    assert!(!drawn(&view, cx, Region::SessionNote(agent.id)));

    // A turn: the tab's spinner.
    osc(&daemon, &agent, "7501;state=working:app=omp");
    let working = wait_agent(
        &view,
        cx,
        agent.id,
        "working",
        in_state(TerminalAgentState::Working),
    );
    wait_until(cx, "the tab's spinner", |cx| {
        drawn(&view, cx, Region::TabState(pane, index))
    });
    let marks = view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        tab_marks(
            &working,
            &store.presentation(&working),
            store.unread(working.id),
        )
    });
    assert_eq!(marks.state, Some(TabState::Working));
    assert!(!marks.bell);

    // It asks: the bell on the tab, the question under the row.
    let question = "Run the tests?";
    osc(
        &daemon,
        &agent,
        &format!(
            "7501;state=blocked:kind=permission:app=omp:msg={}",
            b64(question)
        ),
    );
    let blocked = wait_agent(
        &view,
        cx,
        agent.id,
        "blocked",
        in_state(TerminalAgentState::Blocked),
    );
    assert_eq!(agent_note(&blocked).as_deref(), Some(question));
    wait_until(cx, "the tab's bell", |cx| {
        drawn(&view, cx, Region::TabBell(pane, index))
    });
    wait_until(cx, "the question under the row", |cx| {
        drawn(&view, cx, Region::SessionNote(agent.id))
    });

    // It fails: the failure replaces the bell; the message is the row's line.
    osc(
        &daemon,
        &agent,
        &format!("7501;state=error:app=omp:msg={}", b64("Rate limited")),
    );
    let failed = wait_agent(
        &view,
        cx,
        agent.id,
        "error",
        in_state(TerminalAgentState::Error),
    );
    assert_eq!(agent_note(&failed).as_deref(), Some("Rate limited"));
    wait_until(cx, "the bell to go", |cx| {
        !drawn(&view, cx, Region::TabBell(pane, index))
    });
    assert!(drawn(&view, cx, Region::TabState(pane, index)));
    assert!(drawn(&view, cx, Region::SessionNote(agent.id)));
    let marks = view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        tab_marks(
            &failed,
            &store.presentation(&failed),
            store.unread(failed.id),
        )
    });
    assert_eq!(marks.state, Some(TabState::Failed));

    // The next turn ends well: done, unseen, on the tab until the tab is shown.
    osc(&daemon, &agent, "7501;state=working:app=omp");
    wait_agent(
        &view,
        cx,
        agent.id,
        "working again",
        in_state(TerminalAgentState::Working),
    );
    osc(&daemon, &agent, "7501;state=done:app=omp");
    wait_agent(
        &view,
        cx,
        agent.id,
        "done",
        in_state(TerminalAgentState::Done),
    );
    wait_until(cx, "the done mark", |cx| {
        let done = info(&view, cx, agent.id);
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            tab_marks(&done, &store.presentation(&done), store.unread(done.id)).state
                == Some(TabState::Done)
        })
    });
    assert!(drawn(&view, cx, Region::TabState(pane, index)));
    assert!(!drawn(&view, cx, Region::SessionNote(agent.id)));
    show(&view, cx, agent.id);
    wait_until(cx, "the mark to clear when the tab is shown", |cx| {
        !drawn(&view, cx, Region::TabState(pane, index))
    });
}

#[gpui::test]
fn a_session_the_user_named_keeps_its_name_and_shows_the_agents_title_under_it(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let agent = echo(&daemon);
    let (view, cx) = open(cx, &daemon);
    show(&view, cx, agent.id);
    let _: hutan_protocol::messages::SessionResult = daemon.call(
        hutan_protocol::messages::Request::SessionRename(hutan_protocol::messages::SessionRename {
            session_id: agent.id,
            title: "my build".into(),
        }),
    );
    osc(&daemon, &agent, "7501;state=idle:app=omp");
    osc(&daemon, &agent, "0;π > Fix the build");
    let named = wait_agent(&view, cx, agent.id, "the agent's title", |s| {
        s.terminal_agent.as_ref().is_some_and(|a| a.title.is_some())
    });
    assert_eq!(named.title, "my build");
    assert_eq!(agent_subtitle(&named).as_deref(), Some("Fix the build"));
    let (item, _) = tab_of(&view, cx, agent.id);
    let (glyph, title) = view.read_with(cx, |wb, cx| wb.item_icon_and_title(&item, cx));
    assert_eq!((glyph, title.as_ref()), (Icon::HarnessOmp, "my build"));
    wait_until(cx, "the subtitle under the row", |cx| {
        drawn(&view, cx, Region::SessionNote(agent.id))
    });
}

#[gpui::test]
fn a_bell_or_a_notification_while_the_tab_is_out_of_sight_rings_the_tab_until_it_is_shown(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let rang = echo(&daemon);
    let notified = echo(&daemon);
    let front = echo(&daemon);
    let (view, cx) = open(cx, &daemon);
    show(&view, cx, rang.id);
    show(&view, cx, notified.id);
    show(&view, cx, front.id);
    let pane = pane(&view, cx);
    let (_, rang_tab) = tab_of(&view, cx, rang.id);
    let (_, notified_tab) = tab_of(&view, cx, notified.id);
    assert!(!drawn(&view, cx, Region::TabBell(pane, rang_tab)));

    // BEL (a plain terminal, no agent) and OSC 9 / 777 both ring the bell on their tab.
    daemon.send_input(rang.terminal_id.unwrap(), b"\x07\r");
    osc(&daemon, &notified, "9;Build finished");
    wait_until(cx, "the bell of the terminal that rang", |cx| {
        drawn(&view, cx, Region::TabBell(pane, rang_tab))
    });
    wait_until(cx, "the bell of the terminal that notified", |cx| {
        drawn(&view, cx, Region::TabBell(pane, notified_tab))
    });
    assert!(!drawn(&view, cx, Region::TabState(pane, rang_tab)));

    // The front tab stays quiet; showing a tab clears its bell and only its.
    daemon.send_input(front.terminal_id.unwrap(), b"\x07\r");
    settle(cx);
    let (_, front_tab) = tab_of(&view, cx, front.id);
    assert!(!drawn(&view, cx, Region::TabBell(pane, front_tab)));
    show(&view, cx, rang.id);
    wait_until(cx, "the bell to clear when the tab is shown", |cx| {
        !drawn(&view, cx, Region::TabBell(pane, rang_tab))
    });
    assert!(drawn(&view, cx, Region::TabBell(pane, notified_tab)));
}
