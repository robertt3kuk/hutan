#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! FX-E2E2-C: the tab strip keeps the active tab in view and caps long titles, the Files panel
//! decorates changed files, the Start sheet's default *What*, the settings row's help.

use crate::common;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::rc::Rc;

use common::{TestDaemon, draw, offline_connection, open_options, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::macos::{FakeOpenPanel, MacServices};
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::panels::{FILES_GIT_READ_EVERY, FileGitMark, FilesPanel};
use hutan_desktop::workbench::{Region, SheetFocus, Workbench, WorkbenchOptions, actions};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open_offline(cx: &mut TestAppContext) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    open_with(cx, settings, connection, None)
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

/// Whether tab `index` of `pane` lies inside the pane's width (the strip spans the pane).
fn tab_visible(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    pane: hutan_desktop::workbench::model::PaneId,
    index: usize,
) -> bool {
    let (Some(tab), Some(strip)) = (
        bounds(view, cx, Region::Tab(pane, index)),
        bounds(view, cx, Region::Pane(pane)),
    ) else {
        return false;
    };
    tab.left() >= strip.left() - gpui::px(1.0) && tab.right() <= strip.right() + gpui::px(1.0)
}

#[gpui::test]
fn the_active_tab_scrolls_into_view_when_the_strip_overflows_and_long_titles_are_capped(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open_offline(cx);
    draw(cx);
    let pane = view.read_with(cx, |wb, _| wb.state().first_pane_id());
    let count = 14;
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            for n in 0..count {
                s.add_item(
                    pane,
                    ItemRef::placeholder(format!("A tab with a long title {n}")),
                )
                .unwrap();
            }
            // A chat-like title far wider than any tab should be.
            s.add_item(pane, ItemRef::placeholder("x".repeat(300)))
                .unwrap();
        });
    });
    let items = view.read_with(cx, |wb, _| wb.state().pane(pane).unwrap().items.len());
    // The newest tab is active: the strip shows it though it overflows.
    wait_until(cx, "the newest tab in view", |cx| {
        tab_visible(&view, cx, pane, items - 1)
    });
    // The overflow is real: the first tab was scrolled out.
    assert!(!tab_visible(&view, cx, pane, 0), "the strip overflows");
    // The 300-character title is capped.
    let last = bounds(&view, cx, Region::Tab(pane, items - 1)).unwrap();
    assert!(
        last.size.width < gpui::px(300.0),
        "a long title is cut at a maximum width: {last:?}"
    );

    // Activating the first tab brings it back, on every activation.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.set_active_item(pane, 0).unwrap();
        });
    });
    wait_until(cx, "the first tab in view", |cx| {
        tab_visible(&view, cx, pane, 0)
    });
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.set_active_item(pane, 7).unwrap();
        });
    });
    wait_until(cx, "a middle tab in view", |cx| {
        tab_visible(&view, cx, pane, 7)
    });
}

// Files panel git status -------------------------------------------------------------------------

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

fn settings_of(daemon: &TestDaemon) -> SettingsStore {
    fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

#[gpui::test]
fn the_files_panel_marks_changed_files_and_their_folders_and_follows_fs_changes(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap().join("repo");
    fs::create_dir_all(root.join("src/deep")).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(&root, &["config", key, value]);
    }
    for file in [
        "app.py",
        "notes.txt",
        "clean.txt",
        "src/lib.rs",
        "src/deep/x.rs",
    ] {
        fs::write(root.join(file), "one\n").unwrap();
    }
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    // modified, staged-new, untracked (inside a folder), and a clean file.
    fs::write(root.join("app.py"), "one\ntwo\n").unwrap();
    fs::write(root.join("staged.txt"), "s\n").unwrap();
    git(&root, &["add", "staged.txt"]);
    fs::write(root.join("todo.md"), "t\n").unwrap();
    fs::write(root.join("src/deep/new.rs"), "n\n").unwrap();

    let (view, cx) = open_with(cx, settings_of(&daemon), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    let wanted = root.clone();
    view.update_in(cx, |wb, window, cx| {
        wb.open_project_path(wanted.clone(), window, cx);
    });
    wait_until(cx, "the project", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).files_root().as_deref() == Some(root.as_path())
        })
    });
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<FilesPanel>("files"))
        .expect("the Files panel is drawn");
    wait_until(cx, "the git marks", |cx| {
        panel.read_with(cx, |p, _| !p.git().is_empty())
    });
    panel.read_with(cx, |p, _| {
        let git = p.git();
        assert_eq!(git.mark(&root.join("app.py")), Some(FileGitMark::Modified));
        assert_eq!(git.mark(&root.join("staged.txt")), Some(FileGitMark::Added));
        assert_eq!(
            git.mark(&root.join("todo.md")),
            Some(FileGitMark::Untracked)
        );
        assert_eq!(
            git.mark(&root.join("src/deep/new.rs")),
            Some(FileGitMark::Untracked)
        );
        assert_eq!(git.mark(&root.join("clean.txt")), None);
        assert!(git.has_changes_in(&root.join("src")), "a dot on src");
        assert!(git.has_changes_in(&root.join("src/deep")));
    });

    // An edit of a clean file arrives as an fs.changed: the file gets its mark without a click.
    // The daemon's watcher (FSEvents) may not be live yet when the first edit happens, so the
    // edit is made again until the mark shows (the stimulus is retried, nothing is slept). The
    // panel reads the git marks at most once in `FILES_GIT_READ_EVERY`; the test clock runs that
    // out each time round.
    let mut edits = 0;
    wait_until(cx, "the mark of the edited file", |cx| {
        if edits % 25 == 0 {
            fs::write(root.join("notes.txt"), format!("one\nedited {edits}\n")).unwrap();
        }
        edits += 1;
        cx.executor().advance_clock(FILES_GIT_READ_EVERY);
        panel.read_with(cx, |p, _| {
            p.git().mark(&root.join("notes.txt")) == Some(FileGitMark::Modified)
        })
    });
}

// First-run Open a folder ---------------------------------------------------------------------------

#[gpui::test]
fn the_first_runs_open_folder_panel_starts_at_home_and_opens_the_choice(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fake = FakeOpenPanel::new();
    let mac = MacServices {
        open_panel: Rc::new(fake.clone()),
        ..MacServices::inert()
    };
    let options =
        WorkbenchOptions::builtin(settings_of(&daemon), daemon.connection(), None).with_mac(mac);
    let (view, cx) = open_options(cx, options);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    // No project yet: the first run.
    cx.dispatch_action(actions::OpenFolder);
    wait_until(cx, "the Open panel", |_| !fake.asked_folders().is_empty());
    let home = std::env::home_dir().expect("the test machine has a home folder");
    assert_eq!(fake.asked_folders(), [home]);

    // Choosing a folder opens it as a project.
    let dir = tempfile::tempdir().unwrap();
    let chosen = fs::canonicalize(dir.path()).unwrap();
    assert!(fake.answer(Some(chosen.clone())));
    wait_until(cx, "the project", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).files_root().as_deref() == Some(chosen.as_path())
        })
    });
}

// Settings row help ------------------------------------------------------------------------------

fn row_height(view: &Entity<Workbench>, cx: &mut VisualTestContext, index: usize) -> Option<f32> {
    draw(cx);
    bounds(view, cx, Region::SettingsItem("row", index)).map(|b| f32::from(b.size.height))
}

/// FX-E2E2-C item 7, replaced by CL-POLISH-4 item 14: a help line is never cut with "…", so no
/// row grows when the keyboard reaches it; the rows that need two lines have them at rest.
#[gpui::test]
fn a_help_line_is_whole_at_rest_and_the_keyboard_grows_no_row(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open_with(cx, settings_of(&daemon), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    cx.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(700.0)));
    cx.dispatch_action(actions::OpenSettingsFor("terminal".into()));
    draw(cx);
    let rows = (0..8)
        .take_while(|&i| row_height(&view, cx, i).is_some())
        .count();
    assert!(rows >= 4, "the Terminal page has rows: {rows}");
    let resting: Vec<f32> = (0..rows)
        .map(|i| row_height(&view, cx, i).unwrap())
        .collect();

    // The keyboard walks the rows (search, pages, controls): none changes its height.
    cx.simulate_keystrokes("tab tab");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_focus_zone()),
        Some(SheetFocus::Controls)
    );
    for (i, rest) in resting.iter().enumerate() {
        let focused = row_height(&view, cx, i).unwrap();
        assert!(
            (focused - rest).abs() <= 1.0,
            "row {i} grew from {rest} to {focused} under the keyboard: {resting:?}"
        );
        cx.simulate_keystrokes("down");
    }
    assert!(
        resting.iter().any(|h| *h > 56.0),
        "a long help takes a second line at rest: {resting:?}"
    );
}
