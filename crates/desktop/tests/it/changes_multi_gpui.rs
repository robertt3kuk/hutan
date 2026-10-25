#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! `CHANGES-MULTI`: one Changes panel for every repository of a workspace. *All in workspace*
//! groups the changed files by checkout (two repositories and a worktree here), a file row opens
//! the same Changes view on that file, *Finish…* of a group header opens the Finish form of that
//! checkout, the sidebar's project rows carry a changed-file badge while the window works in
//! another project, and nothing asks the daemon (so git) while frames are drawn.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::WorkspaceId;
use hutan_desktop::settings::ChangesScope;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::panels::ChangesPanel;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{
    Request, WorkspaceCreate, WorkspaceInfo, WorkspaceKind, WorkspaceOpen, WorkspaceOpened,
};
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

/// A repository with one commit holding `<name>.txt`.
fn repo(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
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
    fs::write(root.join(format!("{name}.txt")), "one\ntwo\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    root
}

/// Two repositories in one workspace: `alpha` with a worktree `feature/x` that has a commit of
/// its own, and `beta` with an uncommitted file. `alpha`'s main checkout is clean.
struct Fx {
    daemon: TestDaemon,
    _keep: tempfile::TempDir,
    alpha: PathBuf,
    beta: PathBuf,
    wt: PathBuf,
    workspace: WorkspaceId,
}

impl Fx {
    fn new() -> Self {
        let daemon = TestDaemon::start();
        let keep = tempfile::tempdir().unwrap();
        let parent = fs::canonicalize(keep.path()).unwrap();
        let alpha = repo(&parent, "alpha");
        let beta = repo(&parent, "beta");
        let open = |path: &Path| -> WorkspaceInfo {
            let opened: WorkspaceOpened = daemon.call(Request::WorkspaceOpen(WorkspaceOpen {
                path: path.to_owned(),
                created_by: None,
            }));
            opened.workspace
        };
        let (alpha_project, beta_project) = (open(&alpha), open(&beta));
        let workspace: WorkspaceInfo = daemon.call(Request::WorkspaceCreate(WorkspaceCreate {
            name: "ws".into(),
            paths: Vec::new(),
            project_ids: vec![alpha_project.id, beta_project.id],
            created_by: None,
        }));
        let workspace = workspace.id;
        let created: WorktreeCreated = daemon.call(Request::WorktreeCreate(WorktreeCreate {
            workspace_id: alpha_project.id,
            repo: None,
            branch: "feature/x".into(),
            base: None,
            path: None,
            no_setup: false,
        }));
        let wt = created.worktree.path;
        fs::write(wt.join("alpha.txt"), "one\ntwo\nthree\n").unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "-m", "feature work"]);
        fs::write(beta.join("new.txt"), "hello\n").unwrap();
        Self {
            daemon,
            _keep: keep,
            alpha,
            beta,
            wt,
            workspace,
        }
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    fx: &Fx,
    connection: hutan_client::Connection,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(fx.daemon.home())),
        connection,
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

fn panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<ChangesPanel> {
    cx.dispatch_action(actions::OpenPanel("changes".into()));
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<ChangesPanel>("changes"))
        .expect("the Changes panel is drawn")
}

/// The panel with its three checkouts listed and counted.
fn ready_panel(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    fx: &Fx,
) -> Entity<ChangesPanel> {
    let panel = panel(view, cx);
    wait_until(cx, "three checkouts", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 3)
    });
    wait_until(cx, "their files", |cx| {
        panel.read_with(cx, |p, _| {
            p.counts(&fx.wt).is_some() && p.counts(&fx.beta).is_some()
        })
    });
    panel
}

/// The index of the row of `path` in the panel's listing.
fn row_of(panel: &Entity<ChangesPanel>, cx: &VisualTestContext, path: &Path) -> usize {
    panel.read_with(cx, |p, _| {
        p.rows()
            .iter()
            .position(|w| w.path == path)
            .expect("the checkout is listed")
    })
}

fn region(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> gpui::Bounds<gpui::Pixels> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} is drawn"))
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, at: Region) {
    let bounds = region(view, cx, at);
    cx.simulate_click(bounds.center(), Modifiers::none());
}

#[gpui::test]
fn all_in_workspace_groups_the_files_by_checkout_and_a_file_opens_the_changes_view(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let connection = fx.daemon.connection();
    let (view, cx) = open(cx, &fx, connection);
    let panel = ready_panel(&view, cx, &fx);

    // Two repositories: both scopes are on offer, and the window works in one checkout.
    assert!(panel.read_with(cx, |p, _| p.all_available()));
    panel.update(cx, |p, cx| p.set_changes_scope(ChangesScope::Checkout, cx));
    assert_eq!(panel.read_with(cx, |p, cx| p.shown(cx).len()), 1);

    panel.update(cx, |p, cx| p.set_changes_scope(ChangesScope::Workspace, cx));
    assert_eq!(
        panel.read_with(cx, |p, cx| p.shown(cx).len()),
        3,
        "every checkout of both repositories is a group"
    );
    draw(cx);

    // Counts per checkout, from the files git reports.
    let counts = |path: &Path, cx: &VisualTestContext| {
        panel.read_with(cx, |p, _| {
            p.counts(path).map(|c| (c.files, c.added, c.removed))
        })
    };
    assert_eq!(counts(&fx.wt, cx), Some((1, 1, 0)), "the worktree's commit");
    assert_eq!(
        counts(&fx.beta, cx),
        Some((1, 1, 0)),
        "beta's untracked file"
    );
    assert_eq!(
        panel.read_with(cx, |p, _| p.files_of(&fx.beta)[0].path.clone()),
        "new.txt"
    );
    assert!(
        panel.read_with(cx, |p, _| p.files_of(&fx.alpha).is_empty()),
        "alpha's main checkout is clean"
    );

    // A header per checkout and a row per changed file, in the worktree's and beta's groups.
    let wt = row_of(&panel, cx, &fx.wt);
    let beta = row_of(&panel, cx, &fx.beta);
    let alpha = row_of(&panel, cx, &fx.alpha);
    for group in [wt, beta, alpha] {
        region(&view, cx, Region::ChangesCheckout(group));
    }
    region(&view, cx, Region::ChangesPanelFile(wt, 0));
    region(&view, cx, Region::ChangesPanelFile(beta, 0));
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::ChangesPanelFile(alpha, 0))
            .is_none()),
        "a clean checkout has no file rows"
    );

    // A group folds to its header and unfolds again.
    click(&view, cx, Region::ChangesFold(wt));
    draw(cx);
    assert!(panel.read_with(cx, |p, _| p.is_folded(&fx.wt)));
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::ChangesPanelFile(wt, 0))
            .is_none()),
        "a folded group lists no files"
    );
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::ChangesPanelFile(beta, 0))
            .is_some()),
        "the other group is as it was"
    );
    click(&view, cx, Region::ChangesFold(wt));
    draw(cx);
    assert!(!panel.read_with(cx, |p, _| p.is_folded(&fx.wt)));

    // A file row opens the Changes view of its checkout on that file.
    click(&view, cx, Region::ChangesPanelFile(beta, 0));
    let key = fx.beta.display().to_string();
    let changes = view
        .read_with(cx, |wb, _| wb.changes(&key))
        .expect("the Changes view of beta is a tab");
    wait_until(cx, "new.txt to be selected", |cx| {
        changes.read_with(cx, |c, _| {
            c.selected() == Some("new.txt") && c.selected_diff().is_some()
        })
    });
    assert!(!changes.read_with(cx, |c, _| c.finish_open()));
}

#[gpui::test]
fn finish_on_a_group_header_opens_the_finish_form_of_that_checkout(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let connection = fx.daemon.connection();
    let (view, cx) = open(cx, &fx, connection);
    let panel = ready_panel(&view, cx, &fx);
    panel.update(cx, |p, cx| p.set_changes_scope(ChangesScope::Workspace, cx));
    draw(cx);

    let wt = row_of(&panel, cx, &fx.wt);
    let alpha = row_of(&panel, cx, &fx.alpha);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::ChangesFinish(alpha))
            .is_none()),
        "a clean checkout has nothing to finish"
    );
    click(&view, cx, Region::ChangesFinish(wt));
    let key = fx.wt.display().to_string();
    let changes = view
        .read_with(cx, |wb, _| wb.changes(&key))
        .expect("the Changes view of the worktree is a tab");
    wait_until(cx, "the Finish form", |cx| {
        changes.read_with(cx, |c, _| c.finish_open())
    });
    assert!(
        view.read_with(cx, |wb, _| wb
            .changes(&fx.beta.display().to_string())
            .is_none()),
        "only the checkout of the header was opened"
    );
}

/// The ids of the projects `alpha` and `beta`.
fn projects(view: &Entity<Workbench>, cx: &VisualTestContext) -> (WorkspaceId, WorkspaceId) {
    view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        let find = |name: &str| {
            store
                .workspaces()
                .iter()
                .find(|w| w.kind == WorkspaceKind::Project && w.name == name)
                .map(|w| w.id)
                .unwrap_or_else(|| panic!("project {name}"))
        };
        (find("alpha"), find("beta"))
    })
}

/// The window works in `project` of the workspace (what a click on its sidebar row does).
fn work_in(cx: &mut VisualTestContext, project: WorkspaceId) {
    cx.dispatch_action(actions::OpenWorkspace(project.to_string()));
    draw(cx);
}

fn changed(view: &Entity<Workbench>, cx: &VisualTestContext, project: WorkspaceId) -> usize {
    view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        store.changed_files_of(store.workspace(project).unwrap())
    })
}

#[gpui::test]
fn every_project_row_carries_its_changed_files_also_the_one_the_window_works_in(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let connection = fx.daemon.connection();
    let (view, cx) = open(cx, &fx, connection);
    let (alpha, beta) = projects(&view, cx);
    work_in(cx, alpha);

    // Only beta has uncommitted files (alpha's worktree has a commit, which is not one).
    wait_until(cx, "beta's files to be counted", |cx| {
        changed(&view, cx, beta) == 1
    });
    assert_eq!(changed(&view, cx, alpha), 0);

    // Beta's row shows the badge while the window works in alpha; alpha's row has none (nothing
    // is uncommitted there).
    wait_until(cx, "the badge on beta's row", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::ProjectChanged(beta)).is_some()
        })
    });
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::ProjectChanged(alpha))
            .is_none()),
        "no badge where there is nothing uncommitted"
    );

    // The window goes to beta: its own row keeps the badge.
    work_in(cx, beta);
    draw(cx);
    wait_until(cx, "beta's row to keep its badge", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::ProjectChanged(beta)).is_some()
        })
    });
    work_in(cx, alpha);
    wait_until(cx, "the badge on beta's row", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::ProjectChanged(beta)).is_some()
        })
    });

    // Another file changes behind the desktop's back; the window coming back counts again.
    fs::write(fx.beta.join("second.txt"), "more\n").unwrap();
    cx.deactivate_window();
    draw(cx);
    cx.update(|window, _| window.activate_window());
    wait_until(cx, "two files", |cx| changed(&view, cx, beta) == 2);
}

#[gpui::test]
fn drawing_frames_asks_the_daemon_nothing(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (connection, hold) = fx.daemon.held_connection();
    let (view, cx) = open(cx, &fx, connection);
    let panel = ready_panel(&view, cx, &fx);
    panel.update(cx, |p, cx| p.set_changes_scope(ChangesScope::Workspace, cx));
    let (_, beta) = projects(&view, cx);
    wait_until(cx, "the badges to be counted", |cx| {
        changed(&view, cx, beta) == 1
    });
    // Everything the desktop asked at start has been sent and answered once the list is up.
    draw(cx);
    let before = hold.sent_bytes();
    assert!(before > 0, "the desktop did ask the daemon at start");
    // Many frames, a store notification and a redraw of the panels: every git process the
    // daemon starts is the answer to a request, and none is sent.
    for _ in 0..40 {
        draw(cx);
    }
    assert_eq!(
        hold.sent_bytes(),
        before,
        "drawing frames sent requests to the daemon"
    );
}

#[gpui::test]
fn changes_default_scope_picks_what_the_panel_opens_on(cx: &mut TestAppContext) {
    let fx = Fx::new();
    fs::write(
        fx.daemon.home().join("settings.toml"),
        "[changes]\ndefault_scope = \"workspace\"\n",
    )
    .unwrap();
    let connection = fx.daemon.connection();
    let (view, cx) = open(cx, &fx, connection);
    let panel = ready_panel(&view, cx, &fx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.changes_scope()),
        ChangesScope::Workspace
    );
    assert_eq!(panel.read_with(cx, |p, cx| p.shown(cx).len()), 3);
    // The switch shows it, and *This checkout* narrows the list to one group for this window.
    click(&view, cx, Region::Changes("panel-scope-checkout"));
    assert_eq!(panel.read_with(cx, |p, cx| p.shown(cx).len()), 1);
    assert_eq!(
        panel.read_with(cx, |p, _| p.changes_scope()),
        ChangesScope::Checkout
    );
}

#[gpui::test]
fn the_default_scope_is_this_checkout_and_a_lone_repository_has_no_switch(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    fs::write(
        daemon.home().join("settings.toml"),
        "[changes]\ndefault_scope = \"workspace\"\n",
    )
    .unwrap();
    let keep = tempfile::tempdir().unwrap();
    let solo = repo(&fs::canonicalize(keep.path()).unwrap(), "solo");
    fs::write(solo.join("edit.txt"), "x\n").unwrap();
    let workspace = daemon.create_workspace("solo", &[&solo]).id;
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
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace, window, cx)
    });
    let panel = panel(&view, cx);
    wait_until(cx, "the checkout and its file", |cx| {
        panel.read_with(cx, |p, _| {
            p.rows().len() == 1 && p.counts(&solo).is_some_and(|c| c.files == 1)
        })
    });
    // One repository, no worktree with changes: the setting has nothing to choose between.
    assert!(!panel.read_with(cx, |p, _| p.all_available()));
    assert_eq!(
        panel.read_with(cx, |p, _| p.shown_scope()),
        ChangesScope::Checkout
    );
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::Changes("panel-scope-workspace"))
            .is_none()),
        "no switch while there is one place that changes"
    );
    region(&view, cx, Region::ChangesPanelFile(0, 0));
}

#[gpui::test]
fn two_checkouts_on_one_branch_name_say_their_repository_in_the_tab_title(cx: &mut TestAppContext) {
    use hutan_desktop::workbench::model::ItemRef;

    let fx = Fx::new();
    let connection = fx.daemon.connection();
    let (view, cx) = open(cx, &fx, connection);
    let _panel = ready_panel(&view, cx, &fx);
    // alpha and beta are both on `main`: the repository comes first; the worktree's branch is
    // its own.
    for (path, expected) in [
        (&fx.alpha, "Changes · alpha · main"),
        (&fx.beta, "Changes · beta · main"),
        (&fx.wt, "Changes · feature/x"),
    ] {
        view.update_in(cx, |wb, window, cx| {
            wb.open_changes(path.clone(), window, cx);
        });
        let item = ItemRef::changes(path.display().to_string());
        wait_until(cx, &format!("the title {expected}"), |cx| {
            view.read_with(cx, |wb, cx| {
                wb.item_title(&item, cx).to_string() == expected
            })
        });
    }
}
