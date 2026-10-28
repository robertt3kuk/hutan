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
use gpui::{Entity, ExternalPaths, FileDropEvent, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::WorkspaceId;
use hutan_desktop::workbench::panels::{
    FILES_FILTER_DEBOUNCE, FileGitMark, FilesFilterStatus, FilesPanel,
};
use hutan_desktop::workbench::{FileOpen, Region, Workbench, actions};
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
        Self::build(true)
    }

    /// `session`: a terminal works in the worktree (else nothing does).
    fn build(session: bool) -> Self {
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
        // A session in the worktree used to be what made the daemon list its folder.
        if session {
            let _: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
                workspace_id: Some(alpha_project.id),
                spec: SessionSpec::Terminal(TerminalSpec {
                    command: vec!["sh".into(), "-c".into(), "sleep 300".into()],
                    cwd: Some(wt.clone()),
                    title: Some("in the worktree".to_owned()),
                    ..TerminalSpec::default()
                }),
            }));
        }
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

/// The rows as the panel draws them: two spaces per level, then the name.
fn rows(panel: &Entity<FilesPanel>, cx: &VisualTestContext) -> Vec<String> {
    panel.read_with(cx, |p, _| {
        p.rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    })
}

fn wait_rows(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext, wanted: &[&str]) {
    let wanted: Vec<String> = wanted.iter().map(ToString::to_string).collect();
    wait_until(cx, &format!("the rows {wanted:?}"), |cx| {
        rows(panel, cx) == wanted
    });
}

const WORKSPACE: &str = "[panels.files]\nroots = \"workspace\"\n";

#[gpui::test]
fn the_workspace_stacks_every_project_as_a_collapsible_root_with_git_marks_and_comes_back_from_the_cache(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx, WORKSPACE);
    let panel = files_panel(&view, cx);

    // One tree: a row per project, its folder's entries one level in.
    wait_rows(
        &panel,
        cx,
        &[
            "alpha",
            "  src",
            "  alpha.txt",
            "beta",
            "  src",
            "  beta.txt",
            "  new.txt",
        ],
    );
    assert_eq!(
        panel.read_with(cx, |p, _| p.roots().to_vec()),
        vec![fx.alpha.clone(), fx.beta.clone()]
    );
    let scope = panel.read_with(cx, |p, cx| p.scope(cx)).expect("a chip");
    assert_eq!(scope.name, "ws", "the chip names the workspace");

    // Git marks come from each root: beta's untracked file, a dot on its root's row.
    wait_until(cx, "the git marks of both roots", |cx| {
        panel.read_with(cx, |p, _| p.git().mark(&fx.beta.join("new.txt")).is_some())
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p.git().mark(&fx.beta.join("new.txt"))),
        Some(FileGitMark::Untracked)
    );
    assert!(panel.read_with(cx, |p, _| p.git().has_changes_in(&fx.beta)));
    assert!(!panel.read_with(cx, |p, _| p.git().has_changes_in(&fx.alpha)));

    // A folder opens inside its own root; a root folds into its row and opens again.
    panel.update(cx, |p, cx| p.toggle(&fx.beta.join("src"), cx));
    wait_rows(
        &panel,
        cx,
        &[
            "alpha",
            "  src",
            "  alpha.txt",
            "beta",
            "  src",
            "    beta_lib.rs",
            "  beta.txt",
            "  new.txt",
        ],
    );
    panel.update(cx, |p, cx| p.toggle(&fx.alpha, cx));
    assert_eq!(
        rows(&panel, cx)[..2],
        ["alpha".to_owned(), "beta".to_owned()],
        "alpha folded into its row"
    );
    assert!(
        panel.read_with(cx, |p, _| p.rows()[0].root && !p.rows()[0].expanded),
        "the row says it is folded"
    );
    panel.update(cx, |p, cx| p.toggle(&fx.alpha, cx));
    wait_until(cx, "alpha's rows again", |cx| {
        rows(&panel, cx).get(1).map(String::as_str) == Some("  src")
    });

    // Switching to one project and back keeps the workspace's tree (its open folder too).
    panel.update(cx, |p, cx| p.set_option("roots", "active".into(), cx));
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.roots().to_vec()),
        vec![fx.alpha.clone()],
        "one project, no root rows"
    );
    panel.update(cx, |p, cx| p.set_option("roots", "workspace".into(), cx));
    draw(cx);
    assert!(
        rows(&panel, cx).contains(&"    beta_lib.rs".to_owned()),
        "the workspace's tree came back with its open folder: {:?}",
        rows(&panel, cx)
    );

    // A pick of the root switcher shows that one folder alone; following brings the stack back.
    panel.update(cx, |p, cx| p.pin_root(Some(fx.beta.clone()), cx));
    wait_root(&panel, cx, &fx.beta);
    assert_eq!(panel.read_with(cx, |p, _| p.roots().len()), 1);
    panel.update(cx, |p, cx| p.pin_root(None, cx));
    draw(cx);
    assert_eq!(panel.read_with(cx, |p, _| p.roots().len()), 2);
}

#[gpui::test]
fn the_filter_and_open_editors_look_across_every_root(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx, WORKSPACE);
    let panel = files_panel(&view, cx);
    wait_until(cx, "both roots listed", |cx| {
        rows(&panel, cx).contains(&"  new.txt".to_owned())
    });

    // The filter asks every root: the matches are grouped under their root's row, a root with
    // none has no row.
    panel.update(cx, |p, cx| p.set_filter_text("lib", cx));
    cx.executor().advance_clock(FILES_FILTER_DEBOUNCE);
    wait_until(cx, "the daemon's answers", |cx| {
        panel.read_with(cx, |p, _| {
            matches!(p.filter_status(), FilesFilterStatus::Found { .. })
        })
    });
    assert_eq!(
        rows(&panel, cx),
        [
            "alpha",
            "  src",
            "    alpha_lib.rs",
            "beta",
            "  src",
            "    beta_lib.rs"
        ]
    );
    assert!(panel.read_with(cx, |p, _| p.rows()[0].root && p.rows()[3].root));
    assert_eq!(
        panel.read_with(cx, |p, _| p.filter_status().clone()),
        FilesFilterStatus::Found {
            shown: 2,
            capped: false,
            index_truncated: false
        }
    );
    panel.update(cx, |p, cx| p.set_filter_text("new", cx));
    cx.executor().advance_clock(FILES_FILTER_DEBOUNCE);
    wait_until(cx, "the answers to the next query", |cx| {
        rows(&panel, cx) == ["beta", "  new.txt"]
    });
    panel.update_in(cx, |p, window, cx| p.clear_filter(window, cx));
    wait_until(cx, "the tree back", |cx| rows(&panel, cx).len() == 7);

    // Open Editors lists the tabs below any root.
    for file in [fx.alpha.join("alpha.txt"), fx.beta.join("beta.txt")] {
        let key = file.display().to_string();
        view.update_in(cx, |wb, window, cx| {
            wb.open_file(FileOpen::picked(&file), window, cx);
        });
        wait_until(cx, "the file's tab", |cx| {
            view.read_with(cx, |wb, cx| {
                wb.buffers()
                    .get(&key)
                    .is_some_and(|b| b.read(cx).is_ready())
            })
        });
    }
    wait_until(cx, "Open Editors to list both", |cx| {
        panel.read_with(cx, |p, cx| p.open_editors(cx).len() == 2)
    });
    let open: Vec<PathBuf> = panel.read_with(cx, |p, cx| {
        p.open_editors(cx).into_iter().map(|e| e.path).collect()
    });
    assert_eq!(
        open,
        vec![fx.alpha.join("alpha.txt"), fx.beta.join("beta.txt")]
    );
}

#[gpui::test]
fn files_dropped_from_finder_on_a_root_row_are_copied_into_that_project(cx: &mut TestAppContext) {
    use hutan_desktop::theme::DensityExt as _;
    use hutan_desktop::workbench::style::UiStyle;
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx, WORKSPACE);
    let panel = files_panel(&view, cx);
    wait_until(cx, "both roots listed", |cx| {
        rows(&panel, cx).contains(&"  new.txt".to_owned())
    });
    draw(cx);
    let outside = tempfile::tempdir().unwrap();
    let logo = outside.path().join("logo.png");
    fs::write(&logo, "png\n").unwrap();

    // Rows: alpha 0, src 1, alpha.txt 2, beta 3 — the second root's own row.
    let tree = region(&view, cx, Region::Files("tree"));
    let row = cx.update(|_, app| UiStyle::get(app).density.ui().sidebar_row - 4.0);
    let at = gpui::point(
        tree.origin.x + gpui::px(80.0),
        tree.origin.y + gpui::px(row * 3.5),
    );
    cx.simulate_event(FileDropEvent::Entered {
        position: at,
        paths: ExternalPaths([logo.clone()].into_iter().collect()),
    });
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.drop_target().map(Path::to_owned)),
        Some(fx.beta.clone()),
        "the root's row is a drop target"
    );
    cx.simulate_event(FileDropEvent::Submit { position: at });
    cx.simulate_event(FileDropEvent::Exited);
    cx.simulate_event(FileDropEvent::Ended);
    draw(cx);
    wait_until(cx, "the copy in beta", |_| {
        fx.beta.join("logo.png").exists()
    });
    assert!(!fx.alpha.join("logo.png").exists());
    assert!(logo.exists(), "copied, not moved");
    wait_until(cx, "beta's new row", |cx| {
        rows(&panel, cx).contains(&"  logo.png".to_owned())
    });
}

#[gpui::test]
fn a_worktree_nobody_works_in_is_in_the_switcher_and_its_tree_lists(cx: &mut TestAppContext) {
    let fx = Fx::build(false);
    fs::write(fx.wt.join("wt_only.txt"), "only here\n").unwrap();
    let (view, cx) = open(cx, &fx, "");
    let panel = files_panel(&view, cx);
    wait_root(&panel, cx, &fx.alpha);
    wait_until(cx, "alpha's worktree to be listed", |cx| {
        panel.read_with(cx, |p, cx| p.root_choices(cx).checkouts.len() == 2)
    });
    let entries = open_switcher(&view, cx);
    assert!(
        entries.contains(&("Worktree · feature/x".to_owned(), false)),
        "{entries:?}"
    );
    run_entry(&view, cx, "Worktree · feature/x");
    wait_root(&panel, cx, &fx.wt);
    wait_until(cx, "the worktree's files", |cx| {
        names(&panel, cx).iter().any(|n| n == "wt_only.txt")
    });
    assert!(
        !names(&panel, cx)
            .iter()
            .any(|n| n.contains("outside the workspace roots")),
        "{:?}",
        names(&panel, cx)
    );
}
