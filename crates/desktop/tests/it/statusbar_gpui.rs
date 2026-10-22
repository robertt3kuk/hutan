#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests of the built-in status bar (FX-STATUSBAR, design.md § Workbench model): the
//! `connection` item is there in every daemon state and opens the daemon popover, the
//! `workspace` item follows the active project and its branch, the `working` count comes and
//! goes with the agents at work, a customised preset keeps its own bar, and the bar gives way
//! in a narrow window. Real daemons (in-process, or a `hutand` process for a start that fails),
//! real git repositories in temporary directories.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::agents::{Node, Reporter};
use common::{ProcessDaemon, TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_client::{Connection, ConnectionState};
use hutan_core::SessionId;
use hutan_desktop::workbench::actions::daemon as daemon_actions;
use hutan_desktop::workbench::model::{AGENTS_PAGE, ItemRef};
use hutan_desktop::workbench::panels::{ChipKind, ConnectionChip};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{
    Request, SessionCreate, SessionResult, SessionSpec, TerminalSpec, WorkspaceOpen,
    WorkspaceOpened,
};
use hutan_settings::{SettingsPaths, SettingsStore};

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn settings_in(dir: &Path) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(dir))
}

fn open_daemon<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    connection: Connection,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, settings_in(daemon.home()), connection, None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn chip(view: &Entity<Workbench>, cx: &VisualTestContext) -> ConnectionChip {
    view.read_with(cx, |wb, cx| wb.status_data(cx).connection)
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: &'static str) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::StatusItem(id))
            .is_some_and(|b| b.size.width > gpui::px(8.0))
    })
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    draw(cx);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} was not drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn popover_open(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.daemon_popover_open())
}

fn close_popover(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    view.update_in(cx, |wb, window, cx| wb.close_daemon_popover(window, cx));
    cx.run_until_parked();
}

// ---- the connection item ---------------------------------------------------------------------

#[gpui::test]
fn connected_the_bar_shows_a_dot_and_hutand_with_the_daemons_facts_in_the_tooltip(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open_daemon(cx, &daemon, daemon.connection());

    assert!(
        drawn(&view, cx, "connection"),
        "the item is in the bar while the daemon is up"
    );
    let up = chip(&view, cx);
    assert_eq!((up.kind, up.word.as_str()), (ChipKind::Up, "hutand"));

    // Drawing the bar asked the daemon for its status: version, pid, uptime, sessions, clients.
    wait_until(cx, "the daemon's status in the tooltip", |cx| {
        chip(&view, cx).tooltip.contains("PID")
    });
    let tip = chip(&view, cx).tooltip;
    for part in [
        "hutand ",
        "protocol",
        "PID ",
        "Uptime ",
        "Sessions 1 live / 1 total",
        "Clients ",
        "Click for the daemon controls",
    ] {
        assert!(tip.contains(part), "{part:?} in {tip:?}");
    }

    // A click opens the popover.
    assert!(!popover_open(&view, cx));
    click(&view, cx, Region::StatusItem("connection"));
    assert!(popover_open(&view, cx));
}

#[gpui::test]
fn reconnecting_the_bar_says_so_in_the_warning_colour_and_the_click_opens_the_popover(
    cx: &mut TestAppContext,
) {
    let mut daemon = TestDaemon::start();
    let connection = daemon.proxied_connection();
    let (view, cx) = open_daemon(cx, &daemon, connection);
    assert_eq!(chip(&view, cx).kind, ChipKind::Up);

    daemon.proxy().block(true);
    daemon.proxy().sever();
    wait_until(cx, "the connection to drop", |cx| {
        matches!(
            view.read_with(cx, |wb, cx| wb.sessions().read(cx).connection_state()),
            ConnectionState::Reconnecting { .. }
        )
    });
    let lost = chip(&view, cx);
    assert_eq!(
        (lost.kind, lost.word.as_str()),
        (ChipKind::Reconnecting, "Reconnecting…")
    );
    assert!(drawn(&view, cx, "connection"));

    click(&view, cx, Region::StatusItem("connection"));
    assert!(popover_open(&view, cx));
    close_popover(&view, cx);

    daemon.proxy().block(false);
    wait_until(cx, "the connection to come back", |cx| {
        chip(&view, cx).kind == ChipKind::Up
    });
    daemon.stop();
}

#[gpui::test]
fn stopped_by_you_the_bar_says_stopped_start_and_the_click_opens_the_popover(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open_daemon(cx, &daemon, daemon.connection());

    // Nothing runs, so Stop needs no confirmation.
    cx.dispatch_action(daemon_actions::Stop);
    cx.run_until_parked();
    wait_until(cx, "the daemon to stop", |cx| {
        view.read_with(cx, |wb, _| {
            wb.daemon_stopped() && wb.daemon_busy().is_none()
        })
    });
    let stopped = chip(&view, cx);
    assert_eq!(
        (stopped.kind, stopped.word.as_str()),
        (ChipKind::Stopped, "Stopped · Start")
    );
    assert!(drawn(&view, cx, "connection"));

    click(&view, cx, Region::StatusItem("connection"));
    assert!(popover_open(&view, cx));
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::Popover("daemon-start"))
            .is_some()),
        "the popover offers Start daemon"
    );
}

#[gpui::test]
fn a_start_that_fails_says_cant_start_details_and_the_click_opens_the_popover(
    cx: &mut TestAppContext,
) {
    let process = ProcessDaemon::new();
    let home = process.home();
    fs::create_dir_all(&home).unwrap();
    let conn = rusqlite::Connection::open(home.join("hutan.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO app_meta (key, value) VALUES ('schema_version', '999');",
    )
    .unwrap();
    drop(conn);
    let connection = Connection::start(process.options());
    let (view, cx) = open_with(
        cx,
        settings_in(&process.dir.path().join("config")),
        connection,
        None,
    );

    wait_until(cx, "the start failure", |cx| {
        view.read_with(cx, |wb, cx| wb.daemon_start_failure(cx).is_some())
    });
    let failed = chip(&view, cx);
    assert_eq!(
        (failed.kind, failed.word.as_str()),
        (ChipKind::Failed, "Can't start · Details")
    );
    assert!(
        failed.tooltip.contains("data from a newer Hutan"),
        "{}",
        failed.tooltip
    );
    assert!(drawn(&view, cx, "connection"));

    click(&view, cx, Region::StatusItem("connection"));
    assert!(popover_open(&view, cx));
}

// ---- the workspace item ----------------------------------------------------------------------

struct Repos {
    _dir: tempfile::TempDir,
    api: PathBuf,
    api_wt: PathBuf,
    web: PathBuf,
}

fn repos() -> Repos {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let repo = |name: &str, branch: &str| {
        let root = base.join(name);
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", branch]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "first"]);
        root
    };
    let api = repo("api", "main");
    let web = repo("web", "trunk");
    let api_wt = base.join("api-wt").join("topic");
    git(
        &api,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "topic",
            api_wt.to_str().unwrap(),
        ],
    );
    Repos {
        _dir: dir,
        api,
        api_wt,
        web,
    }
}

fn session_at(daemon: &TestDaemon, title: &str, dir: &Path) -> SessionId {
    let created: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
        workspace_id: None,
        spec: SessionSpec::Terminal(TerminalSpec {
            command: vec!["sh".into(), "-c".into(), "sleep 600".into()],
            cwd: Some(dir.to_owned()),
            title: Some(title.to_owned()),
            ..TerminalSpec::default()
        }),
    }));
    created.session.id
}

fn open_project(daemon: &TestDaemon, path: &Path) {
    let _: WorkspaceOpened = daemon.call(Request::WorkspaceOpen(WorkspaceOpen {
        path: path.to_owned(),
        created_by: None,
    }));
}

fn workspace_item(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
) -> (Option<String>, Option<String>) {
    view.read_with(cx, |wb, cx| {
        let data = wb.status_data(cx);
        (data.workspace, data.branch)
    })
}

#[gpui::test]
fn the_workspace_item_follows_the_active_project_and_its_branch(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let repos = repos();
    open_project(&daemon, &repos.api);
    open_project(&daemon, &repos.web);
    let in_api = session_at(&daemon, "in-api", &repos.api);
    let in_worktree = session_at(&daemon, "in-worktree", &repos.api_wt);
    let in_web = session_at(&daemon, "in-web", &repos.web);
    let (view, cx) = open_daemon(cx, &daemon, daemon.connection());

    let open = |cx: &mut VisualTestContext, id: SessionId| {
        view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
    };
    let shows = |cx: &mut VisualTestContext, name: &str, branch: &str| {
        wait_until(cx, &format!("{name} · {branch}"), |cx| {
            workspace_item(&view, cx) == (Some(name.to_owned()), Some(branch.to_owned()))
        });
    };

    open(cx, in_api);
    shows(cx, "api", "main");
    assert!(drawn(&view, cx, "workspace"), "the item is drawn");

    // A session in a worktree of the project: the branch of that checkout.
    open(cx, in_worktree);
    shows(cx, "api", "topic");

    // Another project: its name and its own branch.
    open(cx, in_web);
    shows(cx, "web", "trunk");

    // A click opens the jump palette.
    click(&view, cx, Region::StatusItem("workspace"));
    assert!(
        view.read_with(cx, |wb, _| wb.picker().is_some()),
        "the workspace item opens the jump palette"
    );
}

// ---- the working count -----------------------------------------------------------------------

#[gpui::test]
fn the_working_count_appears_with_agents_at_work_and_goes_when_they_stop(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let first = daemon.sh("sleep 600").id;
    let second = daemon.sh("sleep 600").id;
    let (view, cx) = open_daemon(cx, &daemon, daemon.connection());
    let working = |cx: &VisualTestContext| view.read_with(cx, |wb, cx| wb.status_data(cx).working);

    assert_eq!(working(cx), 0);
    assert!(!drawn(&view, cx, "working"), "hidden at 0");

    let omp = Reporter::new(first, "omp-extension", "omp");
    let claude = Reporter::new(second, "claude-hooks", "claude");
    omp.report(&daemon, Node::main("root").working().name("omp"));
    wait_until(cx, "one agent working", |cx| working(cx) == 1);
    assert!(drawn(&view, cx, "working"));
    claude.report(&daemon, Node::main("root").working().name("claude"));
    wait_until(cx, "two agents working", |cx| working(cx) == 2);
    assert!(drawn(&view, cx, "working"));

    // The click goes to the Agents page.
    click(&view, cx, Region::StatusItem("working"));
    assert!(
        view.read_with(cx, |wb, _| wb
            .state()
            .panes()
            .into_iter()
            .filter_map(|pane| pane.active_item())
            .any(|item| item == &ItemRef::panel(AGENTS_PAGE))),
        "the working count opens the Agents page"
    );

    omp.release(&daemon, None, None);
    claude.release(&daemon, None, None);
    wait_until(cx, "nobody working", |cx| working(cx) == 0);
    assert!(!drawn(&view, cx, "working"), "hidden again at 0");
}
