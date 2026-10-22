#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! `UX-CHANGES-FRESH`: the Changes view and the Changes panel read the checkout again when the
//! window comes back, when an agent's turn ends in it and when a command finishes in a terminal
//! there; a refresh keeps what the user was doing, and a failed one says so.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::agents::{Node, Reporter};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::{SessionId, WorkspaceId};
use hutan_desktop::workbench::{ChangesView, Region, Workbench};
use hutan_protocol::messages::{Request, SessionCreate, SessionResult, SessionSpec, TerminalSpec};
use hutan_protocol::worktree::{WorktreeCreate, WorktreeCreated};
use hutan_settings::{SettingsPaths, SettingsStore};

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
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

/// `a.txt` with twenty lines on `main`.
fn base() -> String {
    (1..=20).map(|n| format!("line {n}\n")).collect()
}

/// What the worktree made of it: lines 2 and 19 changed, so the diff has two hunks:
/// `0 @@`, `1 line 1`, `2 -line 2`, `3 +LINE 2`, `4..6` context, `7 @@`, `8..10` context,
/// `11 -line 19`, `12 +LINE 19`, `13 line 20`.
fn feature() -> String {
    base()
        .replace("line 2\n", "LINE 2\n")
        .replace("line 19\n", "LINE 19\n")
}

/// [`feature`] with a line in front of it: every line of the first hunk moves down by one.
fn feature_with_zero() -> String {
    format!("zero\n{}", feature())
}

struct Fx {
    daemon: TestDaemon,
    keep: tempfile::TempDir,
    wt: PathBuf,
    workspace: WorkspaceId,
}

impl Fx {
    fn new() -> Self {
        let daemon = TestDaemon::start();
        let keep = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(keep.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git(&root, &["config", key, value]);
        }
        fs::write(root.join("a.txt"), base()).unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "first"]);
        let workspace = daemon.create_workspace("proj", &[&root]).id;
        let created: WorktreeCreated = daemon.call(Request::WorktreeCreate(WorktreeCreate {
            workspace_id: workspace,
            repo: None,
            branch: "feature/x".into(),
            base: None,
            path: None,
        }));
        let wt = created.worktree.path;
        fs::write(wt.join("a.txt"), feature()).unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "-m", "feature work"]);
        Self {
            daemon,
            keep,
            wt,
            workspace,
        }
    }

    /// Rewrites `a.txt` of the worktree behind the desktop's back.
    fn edit(&self, text: &str) {
        fs::write(self.wt.join("a.txt"), text).unwrap();
    }

    /// A terminal in the worktree running `script`, the placement the daemon found for it.
    fn terminal(&self, script: &str) -> hutan_protocol::messages::SessionInfo {
        let created: SessionResult = self.daemon.call(Request::SessionCreate(SessionCreate {
            workspace_id: Some(self.workspace),
            spec: SessionSpec::Terminal(TerminalSpec {
                command: vec!["sh".into(), "-c".into(), script.into()],
                cwd: Some(self.wt.clone()),
                title: Some("work".into()),
                ..TerminalSpec::default()
            }),
        }));
        created.session
    }
}

fn open<'a>(cx: &'a mut TestAppContext, fx: &Fx) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(fx.daemon.home())),
        fx.daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    let workspace = fx.workspace;
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace, window, cx)
    });
    (view, cx)
}

fn open_changes(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    wt: &Path,
) -> Entity<ChangesView> {
    view.update_in(cx, |wb, window, cx| {
        wb.open_changes(wt.to_owned(), window, cx)
    });
    draw(cx);
    let changes = view
        .read_with(cx, |wb, _| wb.changes(&wt.display().to_string()))
        .expect("the Changes view was opened");
    wait_until(cx, "the patch of a.txt", |cx| {
        changes.read_with(cx, |c, _| {
            c.selected() == Some("a.txt") && c.selected_diff().is_some() && c.info().is_some()
        })
    });
    changes
}

/// The window goes to the background and comes back.
fn reactivate(cx: &mut VisualTestContext) {
    cx.deactivate_window();
    draw(cx);
    cx.update(|window, _| window.activate_window());
    draw(cx);
}

/// Whether the shown diff of `changes` has a line with `needle`.
fn shows(changes: &Entity<ChangesView>, cx: &VisualTestContext, needle: &str) -> bool {
    changes.read_with(cx, |c, _| {
        c.selected_diff()
            .is_some_and(|d| d.lines.iter().any(|l| l.text.contains(needle)))
    })
}

#[gpui::test]
fn an_edit_made_elsewhere_shows_when_the_window_comes_back_and_the_review_stays(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.wt);
    assert!(!shows(&changes, cx, "zero"));
    assert!(changes.read_with(cx, |c, _| c.refreshed_ms()).is_some());

    // The user folded the second hunk, selected `+LINE 2` and began a comment on it.
    changes.update(cx, |c, cx| c.toggle_hunk(1, cx));
    changes.update_in(cx, |c, window, cx| {
        c.select_line(3, false, cx);
        assert!(c.open_comment_box(window, cx));
        c.set_comment_text("half-written", cx);
    });
    draw(cx);

    // A line is added in front of the file (a terminal, an editor); the window comes back.
    fx.edit(&feature_with_zero());
    reactivate(cx);
    wait_until(cx, "the new line in the diff", |cx| {
        assert!(
            changes.read_with(cx, |c, _| c.selected_diff().is_some()),
            "the diff stays on screen while it is read again"
        );
        shows(&changes, cx, "zero")
    });

    changes.read_with(cx, |c, _| {
        assert_eq!(c.selected(), Some("a.txt"));
        assert_eq!(c.collapsed_hunks(), vec![1], "the fold stays");
        assert!(c.comment_box_open());
        assert_eq!(c.comment_text().as_deref(), Some("half-written"));
        // Every line of the first hunk moved down by one, and the selection and the box went
        // with `+LINE 2`.
        assert_eq!(c.comment_box_at(), Some((4, false)));
        assert_eq!(c.selection(), Some((4, 4)));
        assert!(c.refresh_error().is_none());
        let now = hutan_client::time::now_ms();
        let chip = c.freshness_text(now).expect("a chip");
        assert!(chip.starts_with("updated "), "{chip}");
    });

    // The comment is added where its lines are now.
    changes.update(cx, |c, cx| assert!(c.add_comment(cx)));
    let comments = changes.read_with(cx, |c, _| c.comments());
    assert_eq!(comments.len(), 1);
    assert!(!comments[0].outdated);
    assert_eq!(comments[0].lines[0].text, "LINE 2");
}

#[gpui::test]
fn a_refresh_that_fails_keeps_the_list_and_says_it_is_old(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.wt);

    // The checkout goes away from under the view.
    let away = fx.keep.path().join("moved-away");
    fs::rename(&fx.wt, &away).unwrap();
    reactivate(cx);
    wait_until(cx, "the failed refresh", |cx| {
        changes.read_with(cx, |c, _| c.refresh_error().is_some())
    });
    changes.read_with(cx, |c, _| {
        assert_eq!(c.files().len(), 1, "the list stays");
        assert!(c.selected_diff().is_some(), "and so does the diff");
        assert!(c.error().is_none());
        let at = c.refreshed_ms().expect("it was read once");
        let chip = c.freshness_text(at + 180_000).expect("a chip");
        assert_eq!(chip, "couldn't refresh · showing changes from 3 min ago");
    });
    wait_until(cx, "the chip to be drawn", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Changes("freshness")).is_some()
        })
    });

    // It comes back: the next refresh succeeds and the chip is plain again.
    fs::rename(&away, &fx.wt).unwrap();
    reactivate(cx);
    wait_until(cx, "the refresh to succeed", |cx| {
        changes.read_with(cx, |c, _| c.refresh_error().is_none() && !c.loading())
    });
    let chip = changes.read_with(cx, |c, _| {
        c.freshness_text(hutan_client::time::now_ms()).unwrap()
    });
    assert!(chip.starts_with("updated "), "{chip}");
}

#[gpui::test]
fn an_agent_turn_that_ends_in_the_checkout_reads_it_again(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.wt);

    let session = fx.terminal("sleep 600");
    let reporter = Reporter::new(session.id, "omp-extension", "omp");
    reporter.report(&fx.daemon, Node::main("m").working());
    wait_busy(&view, cx, session.id, true);

    // The agent writes a file, then its turn ends.
    fx.edit(&feature_with_zero());
    draw(cx);
    assert!(!shows(&changes, cx, "zero"), "nothing read the file yet");
    reporter.report(&fx.daemon, Node::main("m").idle());
    wait_until(cx, "the diff to show the agent's line", |cx| {
        shows(&changes, cx, "zero")
    });
}

/// Waits until the store says the agent of `session` works (or does not).
fn wait_busy(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    session: SessionId,
    working: bool,
) {
    wait_until(cx, "the agent state", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(session)
                .and_then(|s| s.agent.as_ref())
                .is_some_and(|a| {
                    (a.state == hutan_protocol::messages::AgentState::Working) == working
                })
        })
    });
}

#[gpui::test]
fn a_command_that_finishes_in_a_terminal_there_reads_the_checkout_again(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.wt);

    // The terminal's command starts (`OSC 133;C`) and ends (`D`) when it is told to.
    let session = fx.terminal(
        "printf '\\033]133;A\\007\\033]133;B\\007\\033]133;C\\007'; read x; \
         printf '\\033]133;D;0\\007'; sleep 600",
    );
    let terminal = session.terminal_id.expect("a terminal");
    view.update_in(cx, |wb, window, cx| wb.open_session(session.id, window, cx));
    wait_until(cx, "the command to run", |cx| {
        fx.daemon
            .blocks(terminal)
            .iter()
            .any(|b| b.state == hutan_protocol::blocks::BlockState::Running)
            && view.read_with(cx, |wb, cx| {
                wb.sessions()
                    .read(cx)
                    .session(session.id)
                    .is_some_and(|s| s.placement.is_some())
            })
    });

    fx.edit(&feature_with_zero());
    draw(cx);
    assert!(!shows(&changes, cx, "zero"), "nothing read the file yet");
    fx.daemon.send_input(terminal, b"\n");
    wait_until(cx, "the diff to show the command's change", |cx| {
        shows(&changes, cx, "zero")
    });
}

#[gpui::test]
fn the_changes_panel_lists_the_checkouts_again_when_the_window_comes_back(cx: &mut TestAppContext) {
    use hutan_desktop::workbench::panels::ChangesPanel;

    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenPanel(
        "changes".into(),
    ));
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<ChangesPanel>("changes"))
        .expect("the Changes panel is drawn");
    wait_until(cx, "both checkouts", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 2)
    });
    assert!(panel.read_with(cx, |p, _| p.rows().iter().all(|w| !w.dirty)));

    fs::write(fx.wt.join("new.txt"), "hello\n").unwrap();
    reactivate(cx);
    wait_until(cx, "the worktree to be dirty", |cx| {
        panel.read_with(cx, |p, _| p.rows().iter().any(|w| w.dirty))
    });
    assert!(panel.read_with(cx, |p, _| p.refresh_error().is_none()));
}

/// `UX-CHANGES-INDEX` (protocol 0.34, read-only): each uncommitted file says whether it is
/// staged, unstaged or both, and the toolbar's one segmented control (*Committed · All · Staged ·
/// Unstaged*) filters the list.
#[gpui::test]
fn the_changes_list_marks_staged_and_unstaged_files_and_filters_them(cx: &mut TestAppContext) {
    use hutan_desktop::workbench::{IndexFilter, Scope, index_mark};
    let fx = Fx::new();
    fx.edit(&feature_with_zero());
    fs::write(fx.wt.join("b.txt"), "staged\n").unwrap();
    git(&fx.wt, &["add", "b.txt"]);
    fs::write(fx.wt.join("c.txt"), "first\n").unwrap();
    git(&fx.wt, &["add", "c.txt"]);
    fs::write(fx.wt.join("c.txt"), "first\nthen more\n").unwrap();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.wt);
    let marks = changes.read_with(cx, |c, _| {
        c.files()
            .iter()
            .map(|f| (f.path.clone(), index_mark(f)))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        marks,
        [
            ("a.txt".to_owned(), Some("unstaged")),
            ("b.txt".to_owned(), Some("staged")),
            ("c.txt".to_owned(), Some("both")),
        ]
    );
    assert!(changes.read_with(cx, |c, _| c.has_index_marks()));
    let visible = |cx: &VisualTestContext| changes.read_with(cx, |c, _| c.visible_files());
    changes.update(cx, |c, cx| c.set_index_filter(IndexFilter::Staged, cx));
    assert_eq!(visible(cx), ["b.txt", "c.txt"]);
    changes.update(cx, |c, cx| c.set_index_filter(IndexFilter::Unstaged, cx));
    assert_eq!(visible(cx), ["a.txt", "c.txt"]);
    changes.update(cx, |c, cx| c.set_index_filter(IndexFilter::All, cx));
    assert_eq!(visible(cx), ["a.txt", "b.txt", "c.txt"]);
    draw(cx);
    // The toolbar is one strip of four segments, an icon Refresh, and no age chip while the list
    // is current; the old chips are gone.
    let drawn = |cx: &VisualTestContext, id: &'static str| {
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Changes(id)).is_some())
    };
    for id in [
        "changes-scope-committed",
        "changes-index-all",
        "changes-index-staged",
        "changes-index-unstaged",
        "refresh",
    ] {
        assert!(drawn(cx, id), "{id} is drawn");
    }
    assert!(!drawn(cx, "toggle"), "no With uncommitted chip");
    assert!(
        !drawn(cx, "freshness"),
        "no `updated just now` while current"
    );
    // The segments drive the list.
    let click = |cx: &mut VisualTestContext, id: &'static str| {
        let at = view
            .read_with(cx, |wb, _| wb.region_bounds(Region::Changes(id)))
            .unwrap()
            .center();
        cx.simulate_click(at, gpui::Modifiers::none());
    };
    click(cx, "changes-index-staged");
    assert_eq!(visible(cx), ["b.txt", "c.txt"]);
    assert_eq!(
        changes.read_with(cx, |c, _| c.scope()),
        Scope::Index(IndexFilter::Staged)
    );
    click(cx, "changes-scope-committed");
    wait_until(cx, "the committed-only list", |cx| {
        changes.read_with(cx, |c, _| {
            c.scope() == Scope::Committed && c.visible_files() == ["a.txt"]
        })
    });
    click(cx, "changes-index-all");
    wait_until(cx, "the full list", |cx| {
        changes.read_with(cx, |c, _| {
            c.scope() == Scope::Index(IndexFilter::All) && c.visible_files().len() == 3
        })
    });
    // Nothing was staged or unstaged by looking.
    let status = Command::new("git")
        .arg("-C")
        .arg(&fx.wt)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&status.stdout),
        " M a.txt\nA  b.txt\nAM c.txt\n"
    );
}
