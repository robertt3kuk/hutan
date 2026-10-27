#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! EX-8: the Files panel's roots and worktrees — the scope chip's root switcher (the checkouts of
//! the project, the other projects of the workspace, *Follow active container*, each root's kept
//! tree), `panels.files.roots = workspace` (every project a collapsible root row of one tree,
//! the filter and the sections across them) and files dropped from Finder. A real daemon with
//! real git repositories.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::WorkspaceId;
use hutan_desktop::workbench::panels::FilesPanel;
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_protocol::messages::{
    Request, SessionCreate, SessionResult, SessionSpec, TerminalSpec, WorkspaceCreate,
    WorkspaceInfo, WorkspaceOpen, WorkspaceOpened,
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
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join(format!("{name}_lib.rs")), "// lib\n").unwrap();
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
        // The daemon lets a client read the checkouts its sessions work in: one works in the
        // worktree.
        let _: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
            workspace_id: Some(alpha_project.id),
            spec: SessionSpec::Terminal(TerminalSpec {
                command: vec!["sh".into(), "-c".into(), "sleep 300".into()],
                cwd: Some(wt.clone()),
                title: Some("in the worktree".to_owned()),
                ..TerminalSpec::default()
            }),
        }));
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
    extra: &str,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    fs::write(
        fx.daemon.home().join("settings.toml"),
        format!("[app]\nsetup_done = true\n{extra}"),
    )
    .unwrap();
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

fn files_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<FilesPanel> {
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<FilesPanel>("files"))
        .expect("the Files panel is drawn")
}

fn names(panel: &Entity<FilesPanel>, cx: &VisualTestContext) -> Vec<String> {
    panel.read_with(cx, |p, _| {
        p.rows().iter().map(|r| r.name.to_string()).collect()
    })
}

fn wait_root(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext, root: &Path) {
    wait_until(cx, &format!("Files to list {}", root.display()), |cx| {
        panel.read_with(cx, |p, _| {
            p.root() == Some(root) && p.settled() && !p.rows().is_empty()
        })
    });
}

fn region(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> gpui::Bounds<gpui::Pixels> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} is drawn"))
}

/// Opens the root switcher with a click on the scope chip; its entries, in order.
fn open_switcher(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<(String, bool)> {
    let chip = region(view, cx, Region::Files("root-chip"));
    cx.simulate_click(chip.center(), Modifiers::none());
    draw(cx);
    view.read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the switcher opens")
        .into_iter()
        .map(|(label, checked)| (label.to_string(), checked))
        .collect()
}

fn close_menu(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.context_menu_entries())
            .is_none()
    );
}

fn run_entry(view: &Entity<Workbench>, cx: &mut VisualTestContext, label: &str) {
    let at = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("a menu is open")
        .iter()
        .position(|(l, _)| l == label)
        .unwrap_or_else(|| panic!("no entry {label}"));
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(at, window, cx));
    draw(cx);
}

fn files_root(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<PathBuf> {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).files_root())
}

#[gpui::test]
fn the_root_switcher_shows_a_worktree_and_another_project_and_goes_back_with_the_caches_kept(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx, "");
    let panel = files_panel(&view, cx);
    wait_root(&panel, cx, &fx.alpha);
    wait_until(cx, "alpha's worktree to be listed", |cx| {
        panel.read_with(cx, |p, cx| p.root_choices(cx).checkouts.len() == 2)
    });
    fs::write(fx.wt.join("wt_only.txt"), "only here\n").unwrap();

    // The chip's menu: this project's checkouts (the main one first), the workspace's other
    // projects, and Follow active container, which is the one checked.
    let entries = open_switcher(&view, cx);
    assert_eq!(
        entries,
        vec![
            ("Main checkout · main".to_owned(), false),
            ("Worktree · feature/x".to_owned(), false),
            ("beta".to_owned(), false),
            ("Follow active container".to_owned(), true),
        ]
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.context_menu_groups())
            .iter()
            .map(|g| g.to_string())
            .collect::<Vec<_>>(),
        vec!["beta".to_owned(), "Follow active container".to_owned()],
        "the checkouts, the other projects and the follow entry are three groups"
    );

    // The worktree: the panel shows it without the window changing what it works in.
    run_entry(&view, cx, "Worktree · feature/x");
    wait_root(&panel, cx, &fx.wt);
    wait_until(cx, "the worktree's own file", |cx| {
        names(&panel, cx).iter().any(|n| n == "wt_only.txt")
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p.pinned_root().map(Path::to_owned)),
        Some(fx.wt.clone())
    );
    assert_eq!(
        files_root(&view, cx),
        Some(fx.alpha.clone()),
        "the window still works in alpha"
    );
    assert!(
        panel
            .read_with(cx, |p, _| p.cached_roots())
            .contains(&fx.alpha)
    );
    let scope = panel.read_with(cx, |p, cx| p.scope(cx)).expect("a chip");
    assert_eq!(scope.name, "alpha");
    assert_eq!(
        scope.branch.as_deref(),
        Some("feature/x"),
        "the chip says the worktree"
    );
    let entries = open_switcher(&view, cx);
    assert!(entries.contains(&("Worktree · feature/x".to_owned(), true)));
    assert!(entries.contains(&("Follow active container".to_owned(), false)));
    close_menu(&view, cx);

    // Open a folder in the worktree's tree so the cache has something to keep.
    panel.update(cx, |p, cx| p.toggle(&fx.wt.join("src"), cx));
    wait_until(cx, "src listed in the worktree", |cx| {
        panel.read_with(cx, |p, _| {
            p.settled() && p.rows().iter().any(|r| r.name == "alpha_lib.rs")
        })
    });

    // Back to the window's own folder: its tree is drawn at once from the kept one (before any
    // listing answers), the worktree's tree is kept in turn.
    let entries = open_switcher(&view, cx);
    assert!(entries.contains(&("Worktree · feature/x".to_owned(), true)));
    close_menu(&view, cx);
    panel.update(cx, |p, cx| p.pin_root(None, cx));
    assert_eq!(
        panel.read_with(cx, |p, _| p.root().map(Path::to_owned)),
        Some(fx.alpha.clone())
    );
    assert!(
        names(&panel, cx).iter().any(|n| n == "alpha.txt"),
        "alpha's rows are there before any listing came back: {:?}",
        names(&panel, cx)
    );
    assert!(panel.read_with(cx, |p, _| p.pinned_root().is_none()));
    assert!(
        panel
            .read_with(cx, |p, _| p.cached_roots())
            .contains(&fx.wt)
    );
    wait_root(&panel, cx, &fx.alpha);

    // And to the worktree again: the folder opened there is still open.
    panel.update(cx, |p, cx| p.pin_root(Some(fx.wt.clone()), cx));
    assert!(
        names(&panel, cx).iter().any(|n| n == "alpha_lib.rs"),
        "the worktree's open folder came back with its tree: {:?}",
        names(&panel, cx)
    );
    wait_root(&panel, cx, &fx.wt);

    // Another project of the workspace.
    panel.update(cx, |p, cx| p.pin_root(Some(fx.beta.clone()), cx));
    wait_root(&panel, cx, &fx.beta);
    wait_until(cx, "beta's files", |cx| {
        names(&panel, cx).iter().any(|n| n == "beta.txt")
    });
    assert_eq!(files_root(&view, cx), Some(fx.alpha.clone()));
    assert_eq!(
        panel.read_with(cx, |p, cx| p.scope(cx)).map(|s| s.name),
        Some("beta".to_owned())
    );

    // The window going elsewhere ends the pick: the panel follows it again.
    panel.update(cx, |p, cx| p.pin_root(Some(fx.wt.clone()), cx));
    wait_root(&panel, cx, &fx.wt);
    let beta_project: WorkspaceId = view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .workspaces()
            .iter()
            .find(|w| w.name == "beta")
            .expect("beta is a project")
            .id
    });
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(beta_project, window, cx)
    });
    wait_root(&panel, cx, &fx.beta);
    assert!(
        panel.read_with(cx, |p, _| p.pinned_root().is_none()),
        "the pick ended"
    );
}
