#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Placement in the desktop (protocol 0.14): session rows and tabs show where a session works
//! (`main` / `wt <branch>`, a warning while another live agent shares the checkout), and the two
//! worktree commands are in the palette. Real daemon, real git repositories in temporary
//! directories.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::agents::{Node, Reporter};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::{SessionId, WorkspaceId};
use hutan_desktop::sessions::placement_badge;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::{BadgeSite, Region, Workbench};
use hutan_protocol::messages::{Request, SessionCreate, SessionResult, SessionSpec, TerminalSpec};
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

/// `<tmp>/repo` on `main` with one commit and a worktree `<tmp>/repo-wt/topic` on `topic`.
fn repo_with_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap().join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let wt = root.parent().unwrap().join("repo-wt").join("topic");
    git(
        &root,
        &["worktree", "add", "-q", "-b", "topic", wt.to_str().unwrap()],
    );
    (dir, root, wt)
}

fn session_in(daemon: &TestDaemon, workspace: WorkspaceId, dir: &Path, title: &str) -> SessionId {
    let created: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
        workspace_id: Some(workspace),
        spec: SessionSpec::Terminal(TerminalSpec {
            command: vec!["sh".into(), "-c".into(), "sleep 300".into()],
            cwd: Some(dir.to_owned()),
            title: Some(title.to_owned()),
            ..TerminalSpec::default()
        }),
    }));
    created.session.id
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
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

fn badge_text(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    session: SessionId,
) -> Option<(String, bool)> {
    view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .session(session)
            .and_then(placement_badge)
            .map(|badge| (badge.text, badge.shared))
    })
}

fn drawn(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId, site: BadgeSite) -> bool {
    view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::PlacementBadge(site, id)).is_some()
    })
}

#[gpui::test]
fn rows_and_tabs_show_main_and_worktree_badges(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (_keep, root, wt) = repo_with_worktree();
    let workspace = daemon.create_workspace("proj", &[&root]).id;
    let in_main = session_in(&daemon, workspace, &root, "in-main");
    let in_wt = session_in(&daemon, workspace, &wt, "in-worktree");
    let (view, cx) = open(cx, &daemon);

    wait_until(cx, "both sessions to be placed", |cx| {
        badge_text(&view, cx, in_main).is_some() && badge_text(&view, cx, in_wt).is_some()
    });
    assert_eq!(badge_text(&view, cx, in_main), Some(("main".into(), false)));
    assert_eq!(
        badge_text(&view, cx, in_wt),
        Some(("wt topic".into(), false))
    );

    // The rows draw them.
    for id in [in_main, in_wt] {
        wait_until(cx, "the row badge", |cx| {
            drawn(&view, cx, id, BadgeSite::Row)
        });
    }

    // The tab tooltip says it too.
    view.update_in(cx, |wb, window, cx| wb.open_session(in_wt, window, cx));
    draw(cx);
    let tip = view.read_with(cx, |wb, cx| {
        wb.item_tooltip(
            &ItemRef::Terminal {
                session: in_wt.to_string(),
            },
            cx,
        )
    });
    let tip = tip.expect("a session in a worktree has a tab tooltip");
    assert!(tip.starts_with("in-worktree · wt topic\n"), "{tip}");
    assert!(
        tip.contains(wt.to_str().unwrap()) && tip.contains("Branch topic"),
        "{tip}"
    );
    let none = view.read_with(cx, |wb, cx| {
        wb.item_tooltip(
            &ItemRef::Panel {
                panel: "sessions".into(),
            },
            cx,
        )
    });
    assert_eq!(none, None, "panels have no placement");

    // The two commands are in the palette.
    cx.dispatch_action(actions::TogglePalette);
    let titles = view.read_with(cx, |wb, _| wb.palette_titles());
    for title in ["Worktree: New Terminal in Worktree…", "Worktree: Remove…"] {
        assert!(titles.iter().any(|t| t.as_ref() == title), "{title}");
    }
}

#[gpui::test]
fn two_agents_in_one_checkout_turn_the_badge_into_a_warning(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (_keep, root, _wt) = repo_with_worktree();
    let workspace = daemon.create_workspace("proj", &[&root]).id;
    let first = session_in(&daemon, workspace, &root, "first");
    let second = session_in(&daemon, workspace, &root, "second");
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the rows", |cx| {
        drawn(&view, cx, first, BadgeSite::Row) && drawn(&view, cx, second, BadgeSite::Row)
    });
    assert_eq!(badge_text(&view, cx, first), Some(("main".into(), false)));

    let omp = Reporter::new(first, "omp-extension", "omp");
    let claude = Reporter::new(second, "claude-hooks", "claude");
    omp.report(&daemon, Node::main("root").working().name("omp"));
    claude.report(&daemon, Node::main("root").working().name("claude"));
    wait_until(cx, "the warning on both", |cx| {
        badge_text(&view, cx, first) == Some(("main".into(), true))
            && badge_text(&view, cx, second) == Some(("main".into(), true))
    });
    let tip = view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .session(first)
            .and_then(placement_badge)
            .unwrap()
            .tooltip
    });
    assert!(
        tip.ends_with("⚠ Another live agent works in this checkout"),
        "{tip}"
    );
    assert!(drawn(&view, cx, first, BadgeSite::Row));

    // One agent leaves: the warning goes with it.
    claude.release(&daemon, None, None);
    wait_until(cx, "the warning to clear", |cx| {
        badge_text(&view, cx, first) == Some(("main".into(), false))
    });
}

#[gpui::test]
fn the_changes_tab_lists_the_checkouts_and_a_click_opens_the_changes_view(cx: &mut TestAppContext) {
    use gpui::Modifiers;
    use hutan_desktop::workbench::model::DockPosition;
    use hutan_desktop::workbench::panels::ChangesPanel;

    let daemon = TestDaemon::start();
    let (_keep, root, wt) = repo_with_worktree();
    // The worktree has an uncommitted file, the main checkout is clean.
    fs::write(wt.join("new.txt"), "hello\n").unwrap();
    let workspace = daemon.create_workspace("proj", &[&root]).id;
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace, window, cx)
    });

    // Changes is the rail's first icon and the panel a new layout remembers; open its body.
    cx.dispatch_action(actions::OpenPanel("changes".into()));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.right.active.clone())
            .map(|p| p.to_string()),
        Some("changes".to_owned())
    );
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<ChangesPanel>("changes"))
        .expect("the Changes panel is drawn");
    wait_until(cx, "both checkouts", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 2)
    });
    let rows = panel.read_with(cx, |p, _| p.rows().to_vec());
    // The checkout with uncommitted changes first, the clean main checkout after it.
    assert_eq!(rows[0].branch.as_deref(), Some("topic"));
    assert!(rows[0].dirty && !rows[0].main);
    assert!(rows[1].main && !rows[1].dirty);
    wait_until(cx, "the rows to be drawn", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::ChangesCheckout(1)).is_some()
        })
    });

    // A click on the worktree opens its full Changes view as a tab of the center.
    let row = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::ChangesCheckout(0)))
        .unwrap();
    cx.simulate_click(row.center(), Modifiers::none());
    let key = rows[0].path.display().to_string();
    assert!(
        view.read_with(cx, |wb, _| wb
            .state()
            .find_item(&ItemRef::changes(key.clone())))
            .is_some(),
        "the Changes view of {key} is a tab"
    );
    assert!(view.read_with(cx, |wb, _| wb.changes(&key)).is_some());
}
