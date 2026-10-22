#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The second end-to-end pass (FX-E2E2-A) over a real daemon: file tabs and the docks survive a
//! daemon restart and an app restart, and the Finish message starts as what the commit changes.

use crate::common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_client::ConnectionState;
use hutan_desktop::workbench::model::{ItemRef, WorkbenchState};
use hutan_desktop::workbench::{ChangesView, FileOpen, Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

fn settings(daemon: &TestDaemon) -> SettingsStore {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

/// Opens `dir` as the project the window works in.
fn project(view: &Entity<Workbench>, cx: &mut VisualTestContext, dir: &Path) {
    let dir = dir.to_owned();
    view.update_in(cx, |wb, window, cx| {
        wb.open_project_path(dir.clone(), window, cx);
    });
    wait_until(cx, "the project to be the active one", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).files_root().as_deref() == Some(dir.as_path())
        })
    });
}

/// Opens `file` and waits until it is the focused tab; returns its key.
fn open_file(view: &Entity<Workbench>, cx: &mut VisualTestContext, file: &Path) -> String {
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(file), window, cx);
    });
    let key = file.canonicalize().unwrap().display().to_string();
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state().active_center_item() == Some(ItemRef::file(key.clone()))
        })
    });
    key
}

fn state_of(view: &Entity<Workbench>, cx: &VisualTestContext) -> WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

fn file_tabs(state: &WorkbenchState) -> Vec<String> {
    state
        .center
        .panes()
        .into_iter()
        .flat_map(|pane| pane.items.iter())
        .filter_map(|item| item.as_file().map(str::to_owned))
        .collect()
}

fn epoch(view: &Entity<Workbench>, cx: &VisualTestContext) -> hutan_core::Epoch {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch)
        .expect("a synchronised epoch")
}

/// A project with two files, both open in tabs, and a terminal of the project beside them.
struct Scene {
    _repo: tempfile::TempDir,
    root: PathBuf,
}

fn scene() -> Scene {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path().canonicalize().unwrap();
    std::fs::write(root.join("a.fake"), "alpha\n").unwrap();
    std::fs::write(root.join("b.fake"), "beta\n").unwrap();
    Scene { _repo: repo, root }
}

#[gpui::test]
fn file_tabs_and_docks_survive_a_daemon_restart(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let scene = scene();
    let (view, cx) = open_with(cx, settings(&daemon), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    project(&view, cx, &scene.root);
    let workspace = view.read_with(cx, |wb, _| wb.active_workspace()).unwrap();
    let terminal = daemon.create_session(&["cat"], Some(workspace));
    view.update_in(cx, |wb, window, cx| {
        wb.open_session(terminal.id, window, cx)
    });
    wait_until(cx, "the terminal to attach", |cx| {
        view.read_with(cx, |wb, _| wb.holds(terminal.id))
    });
    let a = open_file(&view, cx, &scene.root.join("a.fake"));
    let b = open_file(&view, cx, &scene.root.join("b.fake"));
    draw(cx);
    let before = state_of(&view, cx);
    assert_eq!(file_tabs(&before), [a.clone(), b.clone()]);

    let old = epoch(&view, cx);
    daemon.stop();
    daemon.restart();
    wait_until(cx, "the workbench to follow the new daemon", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.connection_state() == ConnectionState::Connected
                && store.state().epoch.is_some_and(|epoch| epoch != old)
        })
    });
    for _ in 0..5 {
        draw(cx);
        cx.run_until_parked();
    }
    let after = state_of(&view, cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.active_workspace()),
        Some(workspace),
        "the project stays the shown one"
    );
    assert_eq!(file_tabs(&after), [a, b], "{after:#?}");
    assert_eq!(after.docks, before.docks, "the docks did not move");
    assert_eq!(after.center, before.center, "the tabs did not move");
}

#[gpui::test]
fn file_tabs_and_docks_survive_an_app_restart(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let scene = scene();
    let state_path = daemon.home().join("desktop-state.json");
    let (before, files) = {
        let (view, cx) = open_with(
            cx,
            settings(&daemon),
            daemon.connection(),
            Some(state_path.clone()),
        );
        wait_until(cx, "the first synchronisation", |cx| {
            view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
        });
        project(&view, cx, &scene.root);
        let a = open_file(&view, cx, &scene.root.join("a.fake"));
        let b = open_file(&view, cx, &scene.root.join("b.fake"));
        draw(cx);
        view.update(cx, |wb, _| wb.flush_save());
        (state_of(&view, cx), vec![a, b])
    };
    // A new app instance restores the layout from `desktop-state.json`.
    let (view, cx) = open_with(cx, settings(&daemon), daemon.connection(), Some(state_path));
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    for _ in 0..5 {
        draw(cx);
        cx.run_until_parked();
    }
    let after = state_of(&view, cx);
    assert_eq!(file_tabs(&after), files, "{after:#?}");
    assert_eq!(after.docks, before.docks, "the docks did not move");
    assert_eq!(after.center, before.center, "the tabs did not move");
}

// The Finish message ---------------------------------------------------------------------------

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

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: Region) {
    wait_until(cx, &format!("{what:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(what)).is_some()
    });
    let bounds = view.read_with(cx, |wb, _| wb.region_bounds(what)).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

/// Lists the checkout again until it has `count` files, then opens Finish and returns the
/// message it starts with (and closes the form again).
fn prefill(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    changes: &Entity<ChangesView>,
    count: usize,
) -> String {
    changes.update(cx, |c, cx| c.reload(cx));
    wait_until(cx, &format!("{count} files listed"), |cx| {
        changes.read_with(cx, |c, _| c.files().len() == count && c.info().is_some())
    });
    click(view, cx, Region::Changes("finish"));
    let message = changes.read_with(cx, |c, _| c.finish_message()).unwrap();
    changes.update(cx, |c, cx| c.close_finish(cx));
    message
}

#[gpui::test]
fn the_finish_message_names_the_files_the_commit_changes(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path().canonicalize().unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    for (key, value) in [("user.name", "Test"), ("user.email", "test@example.com")] {
        git(&root, &["config", key, value]);
    }
    std::fs::create_dir(root.join("src")).unwrap();
    for file in ["app.py", "src/store.rs", "src/a.rs", "src/b.rs"] {
        std::fs::write(root.join(file), "one\n").unwrap();
    }
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);

    let (view, cx) = open_with(cx, settings(&daemon), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    project(&view, cx, &root);
    std::fs::write(root.join("app.py"), "two\n").unwrap();
    view.update_in(cx, |wb, window, cx| {
        wb.open_changes(root.clone(), window, cx);
    });
    draw(cx);
    let changes = view
        .read_with(cx, |wb, _| wb.changes(&root.display().to_string()))
        .expect("the Changes view was opened");

    // One file: its name.
    assert_eq!(prefill(&view, cx, &changes, 1), "Update app.py");
    // Two: both names, git's way.
    std::fs::write(root.join("src/store.rs"), "two\n").unwrap();
    assert_eq!(
        prefill(&view, cx, &changes, 2),
        "Update app.py and store.rs"
    );
    // Many in one folder: how many, and where.
    git(&root, &["checkout", "--", "app.py"]);
    for file in ["src/a.rs", "src/b.rs"] {
        std::fs::write(root.join(file), "two\n").unwrap();
    }
    std::fs::write(root.join("src/new.rs"), "new\n").unwrap();
    assert_eq!(prefill(&view, cx, &changes, 4), "Update 4 files in src/");
    // Only an untracked file: it is added.
    git(&root, &["checkout", "--", "."]);
    std::fs::remove_file(root.join("src/new.rs")).unwrap();
    std::fs::write(root.join("todo.md"), "- [ ] ship\n").unwrap();
    assert_eq!(prefill(&view, cx, &changes, 1), "Add todo.md");
}
