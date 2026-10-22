#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests against an in-process daemon for projects and workspaces (M12-B): the sidebar's
//! sort modes, pins, manual drag, filters, the ⌘O project picker, the ⌘J jump palette, one
//! layout per workspace, and workspace settings that follow the active workspace.

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, draw, open_with, screen_text, wait_until};
use gpui::{
    Bounds, Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext,
    point, px,
};
use hutan_core::{SessionId, WorkspaceId};
use hutan_desktop::settings::{WorkspaceSort, WorkspacesSettings};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::{Edge, ItemRef, WorkbenchState};
use hutan_desktop::workbench::panels::SessionsPanel;
use hutan_desktop::workbench::{PickerKind, Region, Workbench};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, CreatedBy, Empty, RecentProjects, Request,
    SessionCreate, SessionInfo, SessionResult, SessionSpec, TerminalSpec, WorkspaceInfo,
    WorkspaceKind, WorkspaceList, WorkspaceMove, WorkspacePin, WorkspaceRecent, WorkspaceRef,
};
use hutan_settings::{SettingsPaths, SettingsStore};
use tempfile::TempDir;

// ---- harness ------------------------------------------------------------------------------

fn settings_for(daemon: &TestDaemon) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_with(cx, settings_for(daemon), daemon.connection(), state_path)
}

/// Waits until the desktop knows `workspaces` workspaces (Home included) and `sessions` sessions.
fn wait_synced(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    workspaces: usize,
    sessions: usize,
) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced()
                && store.workspaces().len() == workspaces
                && store.sessions().len() == sessions
        })
    });
    // The active workspace is resolved once the daemon has told what it has.
    wait_until(cx, "the active workspace", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace().is_some())
    });
}

fn panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<SessionsPanel> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<SessionsPanel>("sessions"))
        .expect("the sessions panel is drawn in the left dock")
}

fn listed(panel: &Entity<SessionsPanel>, cx: &mut VisualTestContext) -> Vec<String> {
    panel.read_with(cx, |p, cx| p.visible_workspaces(cx))
}

fn rows(panel: &Entity<SessionsPanel>, cx: &mut VisualTestContext) -> Vec<String> {
    panel.read_with(cx, |p, cx| p.visible_titles(cx))
}

fn state_of(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

fn active(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Option<WorkspaceId> {
    view.read_with(cx, |wb, _| wb.active_workspace())
}

fn open_session(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
}

fn wait_screen(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId, needle: &str) {
    wait_until(cx, &format!("`{needle}` on the screen"), |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(id)
                .is_some_and(|v| screen_text(&v.read(cx).model()).contains(needle))
        })
    });
}

fn pump(cx: &mut VisualTestContext, span: Duration) {
    let until = std::time::Instant::now() + span;
    while std::time::Instant::now() < until {
        draw(cx);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Temporary project folders with recognisable names, kept alive for the test.
#[derive(Default)]
struct Folders(Vec<TempDir>);

impl Folders {
    fn make(&mut self, name: &str) -> PathBuf {
        let dir = tempfile::Builder::new()
            .prefix(&format!("{name}-"))
            .tempdir()
            .unwrap();
        let path = std::fs::canonicalize(dir.path()).unwrap();
        self.0.push(dir);
        path
    }
}

fn project(daemon: &TestDaemon, folders: &mut Folders, name: &str) -> WorkspaceInfo {
    let root = folders.make(name);
    daemon.create_workspace(name, &[&root])
}

fn sleeper(daemon: &TestDaemon, workspace: &WorkspaceInfo) -> SessionInfo {
    // Session creation is the activity `recent` sorts by; keep the stamps apart.
    let session = daemon.create_session(&["sleep", "600"], Some(workspace.id));
    std::thread::sleep(Duration::from_millis(20));
    session
}

/// A session of `workspace` running `script`, with a title of its own.
fn session_in(
    daemon: &TestDaemon,
    workspace: &WorkspaceInfo,
    title: &str,
    script: &str,
) -> SessionInfo {
    let created: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
        workspace_id: Some(workspace.id),
        spec: SessionSpec::Terminal(TerminalSpec {
            command: vec!["sh".into(), "-c".into(), script.to_owned()],
            title: Some(title.to_owned()),
            ..TerminalSpec::default()
        }),
    }));
    created.session
}

fn pin(daemon: &TestDaemon, workspace: &WorkspaceInfo, pinned: bool) {
    let _: WorkspaceInfo = daemon.call(Request::WorkspacePin(WorkspacePin {
        workspace_id: workspace.id,
        pinned,
    }));
}

fn daemon_order(daemon: &TestDaemon) -> Vec<String> {
    let list: WorkspaceList = daemon.call(Request::WorkspaceList(Empty {}));
    list.workspaces.into_iter().map(|w| w.name).collect()
}

fn home_of(daemon: &TestDaemon) -> WorkspaceInfo {
    let list: WorkspaceList = daemon.call(Request::WorkspaceList(Empty {}));
    list.workspaces
        .into_iter()
        .find(|w| w.kind == WorkspaceKind::Home)
        .expect("a Home workspace")
}

fn read_settings(daemon: &TestDaemon) -> String {
    std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap_or_default()
}

fn set_sort(cx: &mut VisualTestContext, mode: WorkspaceSort) {
    cx.dispatch_action(actions::SetWorkspaceSort(mode));
}

fn center_of(b: Bounds<Pixels>) -> Point<Pixels> {
    point(
        b.origin.x + b.size.width / 2.0,
        b.origin.y + b.size.height / 2.0,
    )
}

/// Press at `from`, move in small steps with the button held, release at `to`.
fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    let steps = 8;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let p = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        cx.simulate_mouse_move(p, MouseButton::Left, Modifiers::none());
    }
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
}

fn row_center(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    workspace: WorkspaceId,
) -> Point<Pixels> {
    draw(cx);
    center_of(
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::WorkspaceRow(workspace))
        })
        .expect("the workspace row is drawn"),
    )
}

fn id_of(view: &Entity<Workbench>, cx: &mut VisualTestContext, name: &str) -> WorkspaceId {
    view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .workspaces()
            .iter()
            .find(|w| w.name == name)
            .map(|w| w.id)
    })
    .unwrap_or_else(|| panic!("no workspace named {name}"))
}

fn names_of(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .workspaces()
            .iter()
            .map(|w| w.name.clone())
            .collect()
    })
}

// ---- sort modes, pins ---------------------------------------------------------------------

#[gpui::test]
fn sort_modes_order_the_workspace_rows_and_pins_stay_on_top(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let alpha = project(&daemon, &mut folders, "alpha");
    let bravo = project(&daemon, &mut folders, "bravo");
    let charlie = project(&daemon, &mut folders, "charlie");
    let delta = project(&daemon, &mut folders, "delta");
    // Activity, oldest first: bravo, charlie, alpha, delta.
    sleeper(&daemon, &bravo);
    let blocked = sleeper(&daemon, &charlie);
    sleeper(&daemon, &alpha);
    sleeper(&daemon, &delta);
    pin(&daemon, &delta, true);
    let applied: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        pid: None,
        session_id: blocked.id,
        source: "test".into(),
        agent: "omp".into(),
        state: Some(AgentState::Blocked),
        message: None,
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
    assert!(applied.applied);

    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 5, 4);
    wait_until(cx, "the pin and the agent report", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.workspace(delta.id).is_some_and(|w| w.pinned)
                && store.session(blocked.id).is_some_and(|s| s.agent.is_some())
        })
    });
    let panel = panel(&view, cx);

    // Manual is the default: pins first, then Home, then the daemon's order.
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_store()
            .get::<WorkspacesSettings>()
            .sort),
        WorkspaceSort::Manual
    );
    assert_eq!(
        listed(&panel, cx),
        ["delta", "alpha", "bravo", "charlie", "Home"]
    );

    set_sort(cx, WorkspaceSort::Recent);
    assert_eq!(
        listed(&panel, cx),
        ["delta", "alpha", "charlie", "bravo", "Home"]
    );

    // charlie has a session waiting for the user.
    set_sort(cx, WorkspaceSort::Attention);
    assert_eq!(
        listed(&panel, cx),
        ["delta", "charlie", "alpha", "bravo", "Home"]
    );

    set_sort(cx, WorkspaceSort::Name);
    assert_eq!(
        listed(&panel, cx),
        ["delta", "alpha", "bravo", "charlie", "Home"]
    );
    assert!(
        read_settings(&daemon).contains("sort = \"name\""),
        "the mode is persisted: {}",
        read_settings(&daemon)
    );

    // Unpinning moves delta back into the order of the mode.
    pin(&daemon, &delta, false);
    wait_until(cx, "delta to be unpinned", |cx| {
        listed(&panel, cx) == ["alpha", "bravo", "charlie", "delta", "Home"]
    });
    // The pinned section has its own label.
    pin(&daemon, &bravo, true);
    wait_until(cx, "bravo to be pinned", |cx| {
        listed(&panel, cx).first().map(String::as_str) == Some("bravo")
    });
}

#[gpui::test]
fn the_sort_menu_lists_modes_and_filters_with_their_state(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1, 1);

    cx.dispatch_action(actions::ShowSortMenu(point(px(60.0), px(60.0))));
    let menu = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the sort menu is open");
    let labels: Vec<String> = menu.iter().map(|(l, _)| l.to_string()).collect();
    assert_eq!(
        labels,
        [
            "Manual",
            "Recent activity",
            "Attention first",
            "Name",
            "Hide Empty Projects",
            "Hide Sleeping Projects",
            "Hide CLI-Created Projects",
            "Always Show Where You Are",
            "Organize: Workspaces → Projects",
            "Organize: Projects",
            "Show All Sessions",
            "Projects Sidebar Settings…",
            "Move to",
            "Hide",
        ]
    );
    let checked: Vec<&str> = menu
        .iter()
        .filter(|(_, on)| *on)
        .map(|(l, _)| l.as_ref())
        .collect();
    assert_eq!(
        checked,
        [
            "Manual",
            "Hide Empty Projects",
            "Always Show Where You Are",
            "Organize: Workspaces → Projects"
        ]
    );

    // "Name" is entry 3; "Hide Sleeping Workspaces" entry 5.
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(3, window, cx));
    view.update_in(cx, |wb, window, cx| {
        wb.show_sort_menu(point(px(60.0), px(60.0)), window, cx);
        wb.run_menu_entry(5, window, cx);
    });
    let settings = view.read_with(cx, |wb, _| wb.settings_store().get::<WorkspacesSettings>());
    assert_eq!(settings.sort, WorkspaceSort::Name);
    assert!(settings.hide_sleeping);
    let text = read_settings(&daemon);
    assert!(text.contains("sort = \"name\"") && text.contains("hide_sleeping = true"));
}

// ---- manual order ---------------------------------------------------------------------------

#[gpui::test]
fn dragging_a_row_moves_the_workspace_and_the_order_survives_a_daemon_restart(
    cx: &mut TestAppContext,
) {
    let mut daemon = TestDaemon::start();
    let mut folders = Folders::default();
    for name in ["a", "b", "c"] {
        let ws = project(&daemon, &mut folders, name);
        sleeper(&daemon, &ws);
    }
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 4, 3);
    let panel = panel(&view, cx);
    assert_eq!(listed(&panel, cx), ["a", "b", "c", "Home"]);
    let (a, c) = (id_of(&view, cx, "a"), id_of(&view, cx, "c"));

    // c dropped onto a takes a's place; the daemon stores the new order.
    let (from, to) = (row_center(&view, cx, c), row_center(&view, cx, a));
    drag(cx, from, to);
    wait_until(cx, "the move to reach the sidebar", |cx| {
        listed(&panel, cx) == ["c", "a", "b", "Home"]
    });
    assert_eq!(daemon_order(&daemon), ["Home", "c", "a", "b"]);

    // b dropped onto c takes c's place (from below it lands before c).
    let b = id_of(&view, cx, "b");
    let (from, to) = (row_center(&view, cx, b), row_center(&view, cx, c));
    drag(cx, from, to);
    wait_until(cx, "b to lead", |cx| {
        listed(&panel, cx) == ["b", "c", "a", "Home"]
    });
    assert_eq!(daemon_order(&daemon), ["Home", "b", "c", "a"]);

    // Home closes the list, is no drop target and does not move.
    let home = home_of(&daemon).id;
    let (from, to) = (row_center(&view, cx, home), row_center(&view, cx, a));
    drag(cx, from, to);
    pump(cx, Duration::from_millis(300));
    let (from, to) = (row_center(&view, cx, b), row_center(&view, cx, home));
    drag(cx, from, to);
    pump(cx, Duration::from_millis(300));
    assert_eq!(daemon_order(&daemon), ["Home", "b", "c", "a"]);

    // Only manual order can be dragged.
    set_sort(cx, WorkspaceSort::Name);
    let (from, to) = (row_center(&view, cx, a), row_center(&view, cx, b));
    drag(cx, from, to);
    pump(cx, Duration::from_millis(300));
    assert_eq!(daemon_order(&daemon), ["Home", "b", "c", "a"]);
    set_sort(cx, WorkspaceSort::Manual);

    // The daemon restarts: the order is stored.
    let epoch = view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch);
    daemon.stop();
    daemon.restart();
    wait_until(cx, "the new daemon", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.state().epoch != epoch && store.synced()
        })
    });
    assert_eq!(daemon_order(&daemon), ["Home", "b", "c", "a"]);
    wait_until(cx, "the order after the restart", |cx| {
        listed(&panel, cx) == ["b", "c", "a", "Home"]
    });
    assert_eq!(names_of(&view, cx), ["Home", "b", "c", "a"]);
}

#[gpui::test]
fn a_pinned_workspace_reorders_only_among_the_pinned(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let one = project(&daemon, &mut folders, "one");
    let two = project(&daemon, &mut folders, "two");
    let three = project(&daemon, &mut folders, "three");
    for ws in [&one, &two, &three] {
        sleeper(&daemon, ws);
    }
    pin(&daemon, &one, true);
    pin(&daemon, &two, true);
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 4, 3);
    wait_until(cx, "the pins", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .workspaces()
                .iter()
                .filter(|w| w.pinned)
                .count()
                == 2
        })
    });
    let panel = panel(&view, cx);
    assert_eq!(listed(&panel, cx), ["one", "two", "three", "Home"]);

    // An unpinned row dropped on a pinned one does nothing.
    let (from, to) = (
        row_center(&view, cx, three.id),
        row_center(&view, cx, one.id),
    );
    drag(cx, from, to);
    pump(cx, Duration::from_millis(300));
    assert_eq!(listed(&panel, cx), ["one", "two", "three", "Home"]);

    // Within the pinned section it reorders.
    let (from, to) = (row_center(&view, cx, two.id), row_center(&view, cx, one.id));
    drag(cx, from, to);
    wait_until(cx, "two to lead the pinned section", |cx| {
        listed(&panel, cx) == ["two", "one", "three", "Home"]
    });
}

// ---- filters ------------------------------------------------------------------------------

#[gpui::test]
fn filters_hide_and_show_workspaces_and_the_box_narrows_by_name_and_path(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    project(&daemon, &mut folders, "empty");
    let sleepy = project(&daemon, &mut folders, "sleepy");
    let ended = session_in(&daemon, &sleepy, "ended-one", "true");
    let scripted_root = folders.make("quokka");
    let scripted = daemon.create_workspace_by("scripted", &[&scripted_root], Some(CreatedBy::Cli));
    sleeper(&daemon, &scripted);
    let busy = project(&daemon, &mut folders, "busy");
    let live = sleeper(&daemon, &busy);
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 5, 3);
    wait_until(cx, "the ended session", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(ended.id)
                .is_some_and(|s| !s.state.is_live())
        })
    });
    let panel = panel(&view, cx);

    // Defaults: empty projects are hidden, Home never is (and closes the list).
    assert_eq!(listed(&panel, cx), ["sleepy", "scripted", "busy", "Home"]);

    cx.dispatch_action(actions::ToggleHideSleepingWorkspaces);
    assert_eq!(listed(&panel, cx), ["scripted", "busy", "Home"]);
    cx.dispatch_action(actions::ToggleHideCliWorkspaces);
    assert_eq!(listed(&panel, cx), ["busy", "Home"]);

    // The active workspace stays listed whatever the filters say.
    open_session(&view, cx, ended.id);
    assert_eq!(active(&view, cx), Some(sleepy.id));
    assert_eq!(listed(&panel, cx), ["sleepy", "busy", "Home"]);
    open_session(&view, cx, live.id);
    assert_eq!(listed(&panel, cx), ["busy", "Home"]);

    // Everything off shows even the empty workspace; it is the daemon's order.
    cx.dispatch_action(actions::ToggleHideSleepingWorkspaces);
    cx.dispatch_action(actions::ToggleHideCliWorkspaces);
    cx.dispatch_action(actions::ToggleHideEmptyWorkspaces);
    assert_eq!(
        listed(&panel, cx),
        ["empty", "sleepy", "scripted", "busy", "Home"]
    );
    let text = read_settings(&daemon);
    assert!(text.contains("hide_empty = false"), "{text}");

    // Turning "always show the active workspace" off lets the filters hide it too.
    cx.dispatch_action(actions::ToggleHideEmptyWorkspaces);
    cx.dispatch_action(actions::ToggleHideSleepingWorkspaces);
    view.update_in(cx, |wb, _, cx| {
        wb.toggle_workspace_filter("always_show_active", cx)
    });
    open_session(&view, cx, ended.id);
    assert_eq!(listed(&panel, cx), ["scripted", "busy", "Home"]);
    view.update_in(cx, |wb, _, cx| {
        wb.toggle_workspace_filter("always_show_active", cx)
    });
    assert_eq!(listed(&panel, cx), ["sleepy", "scripted", "busy", "Home"]);

    // The filter box narrows by name and by path; a matching workspace shows its sessions.
    cx.dispatch_action(actions::ToggleHideSleepingWorkspaces);
    panel.update(cx, |p, cx| p.set_query("scrip", cx));
    assert_eq!(listed(&panel, cx), ["scripted"]);
    panel.update(cx, |p, cx| p.set_query("quokka", cx));
    assert_eq!(listed(&panel, cx), ["scripted"], "matches the folder name");
    assert_eq!(rows(&panel, cx).len(), 1, "with its sessions");
    panel.update(cx, |p, cx| p.set_query("busy", cx));
    assert_eq!(listed(&panel, cx), ["busy"]);
    panel.update(cx, |p, cx| p.set_query("nothing like it", cx));
    assert!(listed(&panel, cx).is_empty());
    panel.update(cx, |p, cx| p.set_query("", cx));
}

#[gpui::test]
fn sessions_that_need_you_stay_visible_in_filtered_workspaces(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let cli_root = folders.make("scripted");
    let scripted = daemon.create_workspace_by("scripted", &[&cli_root], Some(CreatedBy::Cli));
    let session = sleeper(&daemon, &scripted);
    let _: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        pid: None,
        session_id: session.id,
        source: "test".into(),
        agent: "omp".into(),
        state: Some(AgentState::Blocked),
        message: None,
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 2, 1);
    wait_until(cx, "the agent report", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(session.id)
                .is_some_and(|s| s.agent.is_some())
        })
    });
    let panel = panel(&view, cx);
    cx.dispatch_action(actions::ToggleHideCliWorkspaces);
    assert_eq!(
        listed(&panel, cx),
        ["Home"],
        "the workspace is filtered out"
    );
    assert_eq!(
        rows(&panel, cx),
        [session.title],
        "its blocked session is still in Requests"
    );
}

// ---- ⌘O and ⌘J ----------------------------------------------------------------------------

#[gpui::test]
fn the_project_picker_lists_recent_projects_and_opens_one(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    // One closed project (only in the recent list) and one that is open.
    let closed_root = folders.make("quokka");
    let open_root = folders.make("wombat");
    let opened: hutan_protocol::messages::WorkspaceOpened = daemon.call(Request::WorkspaceOpen(
        hutan_protocol::messages::WorkspaceOpen {
            path: closed_root.clone(),
            created_by: None,
        },
    ));
    assert!(opened.created);
    let _: serde_json::Value = daemon.call(Request::WorkspaceClose(WorkspaceRef {
        workspace_id: opened.workspace.id,
    }));
    let open_ws = daemon.create_workspace("wombat", &[&open_root]);
    sleeper(&daemon, &open_ws);
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 2, 1);

    cx.simulate_keystrokes("cmd-o");
    wait_until(cx, "the recent projects", |cx| {
        view.read_with(cx, |wb, _| {
            wb.picker()
                .is_some_and(|p| !p.loading && p.entries.len() == 3)
        })
    });
    let titles = view.read_with(cx, |wb, _| wb.picker_titles());
    assert_eq!(titles[0], "Open Folder…");
    assert!(
        titles.iter().any(|t| t.starts_with("quokka-")) && titles.iter().any(|t| t == "wombat"),
        "{titles:?}"
    );

    // Fuzzy filter; "Open Folder…" stays.
    view.update(cx, |wb, cx| wb.set_picker_query("qka", cx));
    let titles = view.read_with(cx, |wb, _| wb.picker_titles());
    assert_eq!(titles.len(), 2, "{titles:?}");
    assert!(titles[1].starts_with("quokka-"), "{titles:?}");
    view.update(cx, |wb, cx| wb.set_picker_query("zzzzzz", cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.picker_titles()),
        ["Open Folder…"]
    );
    view.update(cx, |wb, cx| wb.set_picker_query("quok", cx));

    // Down + Enter opens the closed project: the daemon recreates the workspace, the desktop
    // shows it.
    cx.simulate_keystrokes("down enter");
    assert!(view.read_with(cx, |wb, _| wb.picker().is_none()));
    wait_until(cx, "the project's workspace to be shown", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.active_workspace()
                .and_then(|id| wb.sessions().read(cx).workspace(id).cloned())
                .is_some_and(|w| w.name.starts_with("quokka-"))
        })
    });
    let shown = view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).active_workspace().cloned().unwrap()
    });
    assert_eq!(std::fs::canonicalize(&shown.roots[0]).unwrap(), closed_root);
    // Escape closes the picker without doing anything.
    cx.simulate_keystrokes("cmd-o");
    assert!(view.read_with(cx, |wb, _| wb.picker().is_some()));
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.picker().is_none()));
    assert_eq!(active(&view, cx), Some(shown.id));
}

#[gpui::test]
fn open_folder_uses_the_system_panel_and_opens_the_workspace(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let root = folders.make("fresh");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1, 0);

    cx.simulate_keystrokes("cmd-o");
    wait_until(cx, "the picker", |cx| {
        view.read_with(cx, |wb, _| wb.picker().is_some_and(|p| !p.loading))
    });
    // Row 0 is "Open Folder…".
    view.update_in(cx, |wb, window, cx| wb.picker_confirm(Some(0), window, cx));
    // The first run asks its panel outside of the update (it starts at `~`), so it comes a
    // moment later.
    wait_until(cx, "the folder panel", |cx| cx.did_prompt_for_paths());
    let chosen = root.clone();
    cx.simulate_path_prompt_response(move |options| {
        assert!(options.directories && !options.files && !options.multiple);
        Some(vec![chosen])
    });
    wait_until(cx, "the new workspace to be shown", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .active_workspace()
                .is_some_and(|w| w.name.starts_with("fresh-"))
        })
    });

    // Cancelling the panel changes nothing.
    cx.dispatch_action(actions::OpenFolder);
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| None);
    pump(cx, Duration::from_millis(100));
    let names = names_of(&view, cx);
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names[1].starts_with("fresh-"), "{names:?}");
}

#[gpui::test]
fn the_jump_palette_reaches_hidden_workspaces_and_their_sessions(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let ghost = project(&daemon, &mut folders, "ghost");
    let sleepy = project(&daemon, &mut folders, "sleepy");
    let ended = session_in(&daemon, &sleepy, "ended-two", "true");
    let busy = project(&daemon, &mut folders, "busy");
    sleeper(&daemon, &busy);
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 4, 2);
    wait_until(cx, "the session to end", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(ended.id)
                .is_some_and(|s| !s.state.is_live())
        })
    });
    cx.dispatch_action(actions::ToggleHideSleepingWorkspaces);
    let panel = panel(&view, cx);
    // `ghost` is empty and `sleepy` sleeping: both are out of the sidebar.
    assert_eq!(listed(&panel, cx), ["busy", "Home"]);

    // ⌘P (the Zed base's quick open) lists the jump rows as its Sessions section.
    cx.simulate_keystrokes("cmd-p");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.picker().map(|p| p.kind)),
        Some(PickerKind::QuickOpen)
    );
    let titles = view.read_with(cx, |wb, _| wb.picker_titles());
    for wanted in ["Home", "sleepy", "busy"] {
        assert!(titles.iter().any(|t| t == wanted), "{wanted} in {titles:?}");
    }
    // `ghost` never had a session: stale, not offered (E-13a); ⌘O still lists it.
    assert!(!titles.iter().any(|t| t == "ghost"), "{titles:?}");
    assert!(
        titles.iter().any(|t| *t == ended.title),
        "sessions are listed too: {titles:?}"
    );
    cx.simulate_keystrokes("escape");
    let _ = ghost;

    // A session of a hidden workspace: its workspace is shown and its tab focused.
    cx.simulate_keystrokes("cmd-p");
    view.update(cx, |wb, cx| wb.set_picker_query(&ended.title, cx));
    let rows = view.read_with(cx, |wb, _| wb.picker_titles());
    assert_eq!(
        rows.first().map(ToString::to_string),
        Some(ended.title.clone())
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(active(&view, cx), Some(sleepy.id));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(ended.id)
    );
    // ⌘P again closes the palette.
    cx.simulate_keystrokes("cmd-p");
    cx.simulate_keystrokes("cmd-p");
    assert!(view.read_with(cx, |wb, _| wb.picker().is_none()));
}

// ---- layouts per workspace --------------------------------------------------------------------

#[gpui::test]
fn switching_workspaces_restores_each_ones_pane_tree_and_detaches_the_others(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let one = project(&daemon, &mut folders, "one");
    let two = project(&daemon, &mut folders, "two");
    let a1 = daemon.create_session(&["sh", "-c", "echo marker-a1; sleep 600"], Some(one.id));
    let a2 = daemon.create_session(&["sh", "-c", "echo marker-a2; sleep 600"], Some(one.id));
    let b1 = daemon.create_session(&["sh", "-c", "echo marker-b1; sleep 600"], Some(two.id));
    let state_path = daemon.home().join("desktop-state.json");
    let (tree_one, tree_two) = {
        let (view, cx) = open(cx, &daemon, Some(state_path.clone()));
        wait_synced(&view, cx, 3, 3);

        // Workspace one: a split of two terminals.
        open_session(&view, cx, a1.id);
        assert_eq!(active(&view, cx), Some(one.id));
        cx.dispatch_action(actions::OpenSessionInSplit(actions::SessionSplit {
            session: a2.id.to_string(),
            edge: Edge::Right,
        }));
        wait_screen(&view, cx, a1.id, "marker-a1");
        wait_screen(&view, cx, a2.id, "marker-a2");
        let tree_one = state_of(&view, cx).center;
        assert_eq!(state_of(&view, cx).panes().len(), 2);
        assert_eq!(
            view.read_with(cx, |wb, cx| wb.sessions().read(cx).attached_count()),
            2
        );

        // Workspace two starts with its own single pane; one's terminals detach.
        open_session(&view, cx, b1.id);
        assert_eq!(active(&view, cx), Some(two.id));
        let in_two = state_of(&view, cx);
        assert_eq!(in_two.panes().len(), 1, "a fresh layout, not one's split");
        assert!(in_two.parked.contains_key(&one.id.to_string()));
        wait_screen(&view, cx, b1.id, "marker-b1");
        assert!(!view.read_with(cx, |wb, _| wb.holds(a1.id) || wb.holds(a2.id)));
        assert_eq!(
            view.read_with(cx, |wb, cx| wb.sessions().read(cx).attached_count()),
            1,
            "only what the window shows stays attached"
        );
        assert!(
            view.read_with(cx, |wb, _| wb.terminal_view_of(a1.id).is_none()),
            "no view of a parked workspace is kept to render"
        );
        // Their sessions keep running.
        for id in [a1.id, a2.id] {
            assert!(daemon.session(id).state.is_live());
        }
        // A split in two does not leak into one.
        cx.dispatch_action(actions::OpenSessionInSplit(actions::SessionSplit {
            session: b1.id.to_string(),
            edge: Edge::Bottom,
        }));

        // Back to one: the same tree, re-attached with a fresh snapshot of the screen.
        cx.dispatch_action(actions::OpenWorkspace(one.id.to_string()));
        assert_eq!(active(&view, cx), Some(one.id));
        assert_eq!(state_of(&view, cx).center, tree_one);
        wait_screen(&view, cx, a1.id, "marker-a1");
        wait_screen(&view, cx, a2.id, "marker-a2");
        assert!(!view.read_with(cx, |wb, _| wb.holds(b1.id)));

        // Both layouts are saved and come back with a new desktop.
        cx.dispatch_action(actions::OpenWorkspace(two.id.to_string()));
        let tree_two = state_of(&view, cx).center;
        view.update(cx, |wb, _| wb.flush_save());
        (tree_one, tree_two)
    };
    let (view, cx) = open(cx, &daemon, Some(state_path));
    wait_synced(&view, cx, 3, 3);
    assert_eq!(active(&view, cx), Some(two.id));
    assert_eq!(state_of(&view, cx).center, tree_two);
    wait_screen(&view, cx, b1.id, "marker-b1");
    cx.dispatch_action(actions::OpenWorkspace(one.id.to_string()));
    assert_eq!(state_of(&view, cx).center, tree_one);
    wait_screen(&view, cx, a1.id, "marker-a1");
}

#[gpui::test]
fn a_first_start_and_a_pre_workspace_layout_belong_to_the_right_workspace(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let proj = project(&daemon, &mut folders, "proj");
    let s = sleeper(&daemon, &proj);
    let state_path = daemon.home().join("desktop-state.json");

    // A saved layout without workspace information (written by an older desktop) that shows a
    // session of `proj` belongs to `proj`.
    {
        let mut state = WorkbenchState::default();
        state.replace_item(
            &ItemRef::placeholder("Welcome"),
            ItemRef::Terminal {
                session: s.id.to_string(),
            },
        );
        std::fs::write(&state_path, state.to_json().unwrap()).unwrap();
    }
    let (view, cx) = open(cx, &daemon, Some(state_path));
    wait_synced(&view, cx, 2, 1);
    assert_eq!(active(&view, cx), Some(proj.id));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(s.id),
        "the layout was kept"
    );
}

#[gpui::test]
fn a_vanished_workspace_hands_the_window_to_home_and_its_layout_is_forgotten(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let doomed = project(&daemon, &mut folders, "doomed");
    let keeper = project(&daemon, &mut folders, "keeper");
    let s = sleeper(&daemon, &keeper);
    let state_path = daemon.home().join("desktop-state.json");
    let (view, cx) = open(cx, &daemon, Some(state_path));
    wait_synced(&view, cx, 3, 1);
    // Show `doomed` (empty), park `keeper`'s layout.
    open_session(&view, cx, s.id);
    cx.dispatch_action(actions::OpenWorkspace(doomed.id.to_string()));
    assert_eq!(active(&view, cx), Some(doomed.id));
    assert!(
        state_of(&view, cx)
            .parked
            .contains_key(&keeper.id.to_string())
    );

    // Closing the shown workspace: Home takes over.
    let _: serde_json::Value = daemon.call(Request::WorkspaceClose(WorkspaceRef {
        workspace_id: doomed.id,
    }));
    let home = home_of(&daemon).id;
    wait_until(cx, "Home to take over", |cx| {
        active(&view, cx) == Some(home)
    });
    assert!(
        !state_of(&view, cx)
            .parked
            .contains_key(&doomed.id.to_string()),
        "the layout of the closed workspace is gone"
    );
    assert!(
        state_of(&view, cx)
            .parked
            .contains_key(&keeper.id.to_string())
    );
}

// ---- settings follow the active workspace -------------------------------------------------

#[gpui::test]
fn workspace_settings_layer_over_user_settings_while_their_workspace_is_active(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let root = folders.make("styled");
    std::fs::create_dir_all(root.join(".hutan")).unwrap();
    std::fs::write(
        root.join(".hutan/settings.toml"),
        "[terminal]\nfont_size = 20\n",
    )
    .unwrap();
    // No session at all: the workspace alone decides.
    let styled = daemon.create_workspace("styled", &[&root]);
    let plain = project(&daemon, &mut folders, "plain");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 3, 0);
    let font = |view: &Entity<Workbench>, cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size)
    };
    let home = home_of(&daemon);
    assert_eq!(active(&view, cx), Some(home.id));
    assert_eq!(font(&view, cx), 13.0);

    cx.dispatch_action(actions::OpenWorkspace(styled.id.to_string()));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.project_root().cloned()),
        Some(root.clone())
    );
    wait_until(cx, "the override", |cx| font(&view, cx) == 20.0);

    // The file hot-reloads while its workspace stays active.
    std::fs::write(
        root.join(".hutan/settings.toml"),
        "[terminal]\nfont_size = 24\n",
    )
    .unwrap();
    wait_until(cx, "the edit", |cx| font(&view, cx) == 24.0);

    // Another workspace without a file: back to the user's value.
    cx.dispatch_action(actions::OpenWorkspace(plain.id.to_string()));
    wait_until(cx, "the override to end", |cx| font(&view, cx) == 13.0);
    assert_ne!(
        view.read_with(cx, |wb, _| wb.project_root().cloned()),
        Some(root.clone())
    );
    cx.dispatch_action(actions::OpenWorkspace(styled.id.to_string()));
    wait_until(cx, "the override again", |cx| font(&view, cx) == 24.0);
}

// ---- new sessions, Home -------------------------------------------------------------------

#[gpui::test]
fn new_sessions_start_in_the_active_workspace_and_splits_fall_back_to_home_by_kind(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let first = project(&daemon, &mut folders, "first");
    // `first` takes the first place in the list: it is not Home.
    let _: serde_json::Value = daemon.call(Request::WorkspaceMove(WorkspaceMove {
        workspace_id: first.id,
        before: Some(home_of(&daemon).id),
    }));
    assert_eq!(daemon_order(&daemon), ["first", "Home"]);
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 2, 0);
    wait_until(cx, "the move", |cx| {
        names_of(&view, cx) == ["first", "Home"]
    });
    let home = home_of(&daemon);

    // A split of a pane that shows no session starts in Home's folder, not in the first
    // workspace of the list.
    let pane = state_of(&view, cx).active_pane_id();
    view.update_in(cx, |wb, window, cx| {
        wb.split_pane_with_terminal(pane, Edge::Right, window, cx)
    });
    wait_until(cx, "the split's terminal", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()).is_some()
    });
    let split = daemon.session(view.read_with(cx, |wb, _| wb.focused_session()).unwrap());
    assert_eq!(split.workspace_id, home.id);
    assert_eq!(
        std::fs::canonicalize(split.cwd.as_deref().unwrap()).unwrap(),
        std::fs::canonicalize(&home.roots[0]).unwrap()
    );

    // ⌘T follows the active workspace.
    cx.dispatch_action(actions::OpenWorkspace(first.id.to_string()));
    cx.simulate_keystrokes("cmd-t");
    wait_until(cx, "the new session", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()).is_some()
    });
    let created = daemon.session(view.read_with(cx, |wb, _| wb.focused_session()).unwrap());
    assert_eq!(created.workspace_id, first.id);
    assert_eq!(
        std::fs::canonicalize(created.cwd.as_deref().unwrap()).unwrap(),
        std::fs::canonicalize(&first.roots[0]).unwrap()
    );
    assert_eq!(active(&view, cx), Some(first.id));

    // Go to Home.
    cx.dispatch_action(actions::GoHome);
    assert_eq!(active(&view, cx), Some(home.id));
}

// ---- menus and verbs ------------------------------------------------------------------------

fn menu_labels(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("a menu is open")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect()
}

/// Opens a row's menu the way the sidebar does: through the window's dispatch.
fn show_menu(view: &Entity<Workbench>, cx: &mut VisualTestContext, workspace: WorkspaceId) {
    view.update_in(cx, |_, window, cx| {
        window.dispatch_action(
            Box::new(actions::ShowWorkspaceMenu(actions::WorkspaceMenuAt {
                workspace: workspace.to_string(),
                position: point(px(50.0), px(50.0)),
            })),
            cx,
        );
    });
    assert!(view.read_with(cx, |wb, _| wb.context_menu_entries().is_some()));
}

fn run_entry(view: &Entity<Workbench>, cx: &mut VisualTestContext, label_prefix: &str) {
    let index = menu_labels(view, cx)
        .iter()
        .position(|l| l.starts_with(label_prefix))
        .unwrap_or_else(|| panic!("no menu entry `{label_prefix}`"));
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(index, window, cx));
}

#[gpui::test]
fn the_project_and_workspace_menus_pin_rename_add_folders_and_close(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let proj = project(&daemon, &mut folders, "proj");
    // Folders are added to a workspace — the user's container of projects (protocol 0.19); a
    // project is one folder.
    let shop = daemon.create_workspace("shop", &[]);
    let extra = folders.make("extra");
    let extra_too = folders.make("extra-too");
    let busy = project(&daemon, &mut folders, "busy");
    let live = sleeper(&daemon, &busy);
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 4, 1);
    let panel = panel(&view, cx);
    let revealed: std::rc::Rc<std::cell::RefCell<Vec<PathBuf>>> = Default::default();
    view.update(cx, |wb, _| {
        let revealed = std::rc::Rc::clone(&revealed);
        wb.set_reveal_handler(move |path: &Path, _| revealed.borrow_mut().push(path.to_owned()));
    });

    // A project with a live session cannot be closed; the entry says why. In no workspace, it
    // cannot leave one either.
    show_menu(&view, cx, busy.id);
    assert_eq!(
        menu_labels(&view, cx),
        [
            "Rename…",
            "Pin",
            "Move to Workspace",
            "Remove from Workspace (in no workspace)",
            "Open File…",
            "Reveal in Finder",
            "Close (1 live session)",
        ]
    );
    let disabled = view.read_with(cx, |wb, _| wb.context_menu_disabled());
    assert_eq!(
        disabled.iter().map(ToString::to_string).collect::<Vec<_>>(),
        [
            "Remove from Workspace (in no workspace)",
            "Close (1 live session)"
        ]
    );
    // Running a greyed-out entry only closes the menu.
    run_entry(&view, cx, "Close (");
    assert!(view.read_with(cx, |wb, _| wb.context_menu_entries().is_none()));
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
    assert_eq!(names_of(&view, cx).len(), 4);
    // The command palette's verb gives the same answer in words.
    cx.dispatch_action(actions::OpenWorkspace(busy.id.to_string()));
    cx.dispatch_action(actions::CloseActiveWorkspace);
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .is_some_and(|n| n.contains("1 live session")),
    );

    // Home has one folder: it can be pinned, renamed and revealed, nothing else.
    let home = home_of(&daemon);
    show_menu(&view, cx, home.id);
    assert_eq!(
        menu_labels(&view, cx),
        [
            "Pin",
            "Rename…",
            "Add Folder… (Home has one folder)",
            "Reveal in Finder"
        ]
    );
    // Out of range: the menu only closes.
    view.update_in(cx, |wb, window, cx| {
        wb.run_menu_entry(usize::MAX, window, cx)
    });
    assert!(view.read_with(cx, |wb, _| wb.context_menu_entries().is_none()));

    // Pin / Unpin.
    show_menu(&view, cx, proj.id);
    run_entry(&view, cx, "Pin");
    wait_until(cx, "the pin", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .workspace(proj.id)
                .is_some_and(|w| w.pinned)
        })
    });
    show_menu(&view, cx, proj.id);
    assert_eq!(menu_labels(&view, cx)[1], "Unpin");
    run_entry(&view, cx, "Unpin");
    wait_until(cx, "the unpin", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .workspace(proj.id)
                .is_some_and(|w| !w.pinned)
        })
    });

    // Rename edits the row in place (it is on screen: an empty project stays listed while it is
    // the one shown); Escape leaves the name alone.
    let renaming = |cx: &mut VisualTestContext| {
        panel.read_with(cx, |p, _| {
            p.renaming().map(|(id, text)| (id, text.to_owned()))
        })
    };
    cx.dispatch_action(actions::OpenWorkspace(proj.id.to_string()));
    draw(cx);
    show_menu(&view, cx, proj.id);
    run_entry(&view, cx, "Rename");
    draw(cx);
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()), "no dialog");
    assert_eq!(renaming(cx), Some((proj.id, "proj".to_owned())));
    cx.simulate_keystrokes("escape");
    assert_eq!(renaming(cx), None);
    pump(cx, Duration::from_millis(200));
    assert!(names_of(&view, cx).contains(&"proj".to_owned()));
    // Enter sends the typed name.
    show_menu(&view, cx, proj.id);
    run_entry(&view, cx, "Rename");
    draw(cx);
    cx.simulate_keystrokes("backspace backspace backspace backspace");
    cx.simulate_input("project");
    assert_eq!(renaming(cx), Some((proj.id, "project".to_owned())));
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the new name", |cx| {
        names_of(&view, cx).contains(&"project".to_owned())
    });
    assert_eq!(renaming(cx), None);

    // A workspace's menu; Add Folder… goes through the system panel, twice.
    show_menu(&view, cx, shop.id);
    assert_eq!(
        menu_labels(&view, cx),
        [
            "Pin",
            "Rename…",
            "Add Folder…",
            "Delete Workspace…",
            "Reveal in Finder (no folders yet)"
        ]
    );
    for folder in [extra.clone(), extra_too.clone()] {
        show_menu(&view, cx, shop.id);
        run_entry(&view, cx, "Add Folder");
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(move |_| Some(vec![folder]));
    }
    wait_until(cx, "the second folder", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .workspace(shop.id)
                .is_some_and(|w| w.roots.len() == 2)
        })
    });
    // A project leaves its workspace again through its own menu.
    let too = view
        .read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .workspaces()
                .iter()
                .find(|w| w.name.starts_with("extra-too"))
                .map(|w| w.id)
        })
        .expect("the project of the second folder");
    show_menu(&view, cx, too);
    assert!(menu_labels(&view, cx).contains(&"Remove from Workspace".to_owned()));
    run_entry(&view, cx, "Remove from Workspace");
    wait_until(cx, "the folder to go", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .workspace(shop.id)
                .is_some_and(|w| w.roots.len() == 1)
        })
    });

    // Reveal in Finder goes through the reveal handler.
    show_menu(&view, cx, proj.id);
    run_entry(&view, cx, "Reveal");
    let first_root = view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).workspace(proj.id).unwrap().roots[0].clone()
    });
    assert!(
        revealed.borrow().contains(&first_root),
        "{:?}",
        revealed.borrow()
    );

    // Close asks first; the project stays in the recent list.
    show_menu(&view, cx, proj.id);
    run_entry(&view, cx, "Close");
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the project to close", |cx| {
        !names_of(&view, cx).contains(&"project".to_owned())
    });
    let recent: RecentProjects = daemon.call(Request::WorkspaceRecent(WorkspaceRecent {
        limit: Some(10),
    }));
    assert!(
        recent
            .projects
            .iter()
            .any(|p| p.name == "project" && p.workspace_id.is_none()),
        "{recent:?}"
    );
    let _ = live;
}

#[gpui::test]
fn every_workspace_command_is_in_the_palette(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1, 0);
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.commands().iter().map(|c| c.title.to_string()).collect()
    });
    for wanted in [
        "Workspace: Open Project…",
        "Workspace: Open Folder…",
        "Workspace: Jump to Workspace or Session…",
        "Workspace: Go to Home",
        "Workspace: Pin / Unpin Active Workspace",
        "Workspace: Rename Active Workspace…",
        "Workspace: Add Folder to Active Workspace…",
        "Workspace: New Workspace…",
        "Workspace: Delete or Close Active Workspace",
        "Workspace: Reveal Active Workspace in Finder",
        "Workspace: Sort — Manual",
        "Workspace: Sort — Recent activity",
        "Workspace: Sort — Attention first",
        "Workspace: Sort — Name",
        "Sidebar: Toggle Hide Empty Projects",
        "Sidebar: Toggle Hide Sleeping Projects",
        "Sidebar: Toggle Hide CLI-Created Projects",
        "Sidebar: Organize by Workspaces",
        "Sidebar: Organize by Projects",
    ] {
        assert!(titles.iter().any(|t| t == wanted), "missing `{wanted}`");
    }
    // The commands run through the palette like the keys do.
    view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
    view.update(cx, |wb, cx| wb.set_palette_query("Sort — Name", cx));
    cx.simulate_keystrokes("enter");
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_store()
            .get::<WorkspacesSettings>()
            .sort),
        WorkspaceSort::Name
    );
}

#[gpui::test]
fn the_active_workspace_follows_open_workspace_and_is_in_the_status_bar(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let mut folders = Folders::default();
    let proj = project(&daemon, &mut folders, "proj");
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 2, 0);
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::StatusItem("workspace")))
            .is_some(),
        "the workspace item is in the built-in status bar (FX-STATUSBAR)"
    );
    assert_eq!(
        view.read_with(cx, |wb, cx| wb
            .sessions()
            .read(cx)
            .active_workspace()
            .map(|w| w.name.clone())),
        Some("Home".to_owned())
    );
    cx.dispatch_action(actions::OpenWorkspace(proj.id.to_string()));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb
            .sessions()
            .read(cx)
            .active_workspace()
            .map(|w| w.name.clone())),
        Some("proj".to_owned())
    );
}
