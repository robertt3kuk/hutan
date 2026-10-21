#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! ⌘P quick open and the Files panel (E-13a) over a real daemon: typing a file's name opens it,
//! `path:line:col` lands on the line and column, an `fs.generation` brings a new file in, stale
//! projects and recent files of another project are not offered, the tree builds only the rows
//! in view with 20 000 entries open, the panel reveals the focused tab's file, *Open File…*
//! starts in the project, and the finder's first results on a 100 000-file tree are timed
//! (key → the frame that shows them).

use crate::common;

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use common::{TestDaemon, draw, open_options, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_code_view::LoadState;
use hutan_desktop::macos::{FakeOpenPanel, MacServices};
use hutan_desktop::workbench::panels::{FileRowKind, FilesPanel};
use hutan_desktop::workbench::{PickerKind, Workbench, WorkbenchOptions, actions};
use hutan_settings::{SettingsPaths, SettingsStore};

fn settings(daemon: &TestDaemon) -> SettingsStore {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

fn synced(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, settings(daemon), daemon.connection(), None);
    synced(&view, cx);
    (view, cx)
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

fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

fn source(lines: usize) -> String {
    (1..=lines)
        .map(|n| format!("fn line_{n}() {{}}\n"))
        .collect()
}

fn titles(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| {
        wb.picker_titles().iter().map(ToString::to_string).collect()
    })
}

/// The file rows of ⌘P and whether an answer is still on its way.
fn file_rows(view: &Entity<Workbench>, cx: &VisualTestContext) -> (usize, bool, u64) {
    view.read_with(cx, |wb, _| {
        wb.picker().map_or((0, false, 0), |p| {
            (p.files.rows, p.files.searching, p.files.answers)
        })
    })
}

fn focused_file(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<(String, bool)> {
    view.read_with(cx, |wb, cx| {
        wb.focused_file_view().map(|v| {
            let buffer = v.read(cx).buffer().read(cx);
            (
                buffer.path().display().to_string(),
                *buffer.state() != LoadState::Loading,
            )
        })
    })
}

fn wait_answer(view: &Entity<Workbench>, cx: &mut VisualTestContext, after: u64) {
    wait_until(cx, "the answer of fs.find", |cx| {
        let (_, searching, answers) = file_rows(view, cx);
        !searching && answers > after
    });
}

#[gpui::test]
fn typing_a_name_opens_the_file_and_a_line_and_column_land_where_asked(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let main = write(&root, "src/main.rs", &source(40));
    write(&root, "src/lib.rs", &source(5));
    write(&root, "docs/maintenance.md", "notes\n");
    project(&view, cx, &root);

    cx.dispatch_action(actions::QuickOpen);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.picker().map(|p| p.kind)),
        Some(PickerKind::QuickOpen)
    );
    cx.simulate_input("main.rs");
    wait_answer(&view, cx, 0);
    assert_eq!(
        titles(&view, cx).first().map(String::as_str),
        Some("main.rs"),
        "{:?}",
        titles(&view, cx)
    );
    cx.simulate_keystrokes("enter");
    let wanted = main.display().to_string();
    wait_until(cx, "main.rs in the focused tab", |cx| {
        focused_file(&view, cx).is_some_and(|(path, loaded)| path == wanted && loaded)
    });
    assert!(view.read_with(cx, |wb, _| wb.picker().is_none()));

    // `path:line:col`: the caret lands at line 16, column 5.
    cx.dispatch_action(actions::QuickOpen);
    // The file just opened is the recent one: listed with nothing typed.
    assert_eq!(
        titles(&view, cx).first().map(String::as_str),
        Some("main.rs")
    );
    let before = file_rows(&view, cx).2;
    cx.simulate_input("src/main.rs:16:5");
    wait_answer(&view, cx, before);
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the caret at 16:5", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view()
                .is_some_and(|v| v.read(cx).code().read(cx).position(cx) == (16, 5))
        })
    });

    // ⌘Return opens in a split to the right.
    let panes = view.read_with(cx, |wb, _| wb.state().panes().len());
    cx.dispatch_action(actions::QuickOpen);
    let before = file_rows(&view, cx).2;
    cx.simulate_input("lib.rs");
    wait_answer(&view, cx, before);
    cx.simulate_keystrokes("cmd-enter");
    let lib = root.join("src/lib.rs").display().to_string();
    wait_until(cx, "lib.rs in a new split", |cx| {
        focused_file(&view, cx).is_some_and(|(path, _)| path == lib)
            && view.read_with(cx, |wb, _| wb.state().panes().len()) == panes + 1
    });
}

#[gpui::test]
fn a_file_made_while_the_palette_is_open_comes_in_with_the_roots_generation(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    write(&root, "src/old.rs", "\n");
    project(&view, cx, &root);
    cx.dispatch_action(actions::QuickOpen);
    cx.simulate_input("zebra");
    wait_answer(&view, cx, 0);
    assert_eq!(file_rows(&view, cx).0, 0, "nothing matches yet");

    write(&root, "src/zebra_crossing.rs", "\n");
    wait_until(cx, "the new file's row", |cx| {
        titles(&view, cx).iter().any(|t| t == "zebra_crossing.rs")
    });
    assert!(
        view.read_with(cx, |wb, _| wb.picker().unwrap().files.refreshes) >= 1,
        "the root's fs.generation asked again"
    );
    // Closing stops watching the root.
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.picker().is_none()));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).watched_dirs()),
        0
    );
}

#[gpui::test]
fn stale_projects_and_another_projects_recent_files_are_not_offered(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let (scratch, acme) = (base.join("scratch"), base.join("acme"));
    let mistake = write(&scratch, "mistake.txt", "oops\n");
    write(&acme, "src/main.rs", &source(3));

    // A folder opened by mistake (a file looked at), then the real project.
    project(&view, cx, &scratch);
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(
            hutan_desktop::workbench::FileOpen::picked(&mistake),
            window,
            cx,
        );
    });
    let wanted = mistake.display().to_string();
    wait_until(cx, "the file", |cx| {
        focused_file(&view, cx).is_some_and(|(path, _)| path == wanted)
    });
    project(&view, cx, &acme);

    cx.dispatch_action(actions::QuickOpen);
    let shown = titles(&view, cx);
    assert!(!shown.iter().any(|t| t == "scratch"), "{shown:?}");
    assert!(!shown.iter().any(|t| t == "mistake.txt"), "{shown:?}");
    assert!(
        shown.iter().any(|t| t == "acme"),
        "the active project stays: {shown:?}"
    );
    cx.simulate_input("mistake");
    wait_answer(&view, cx, 0);
    assert!(!titles(&view, cx).iter().any(|t| t == "mistake.txt"));
}

fn files_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<FilesPanel> {
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<FilesPanel>("files"))
        .expect("the Files panel is drawn")
}

#[gpui::test]
fn the_tree_builds_only_the_rows_in_view_with_twenty_thousand_entries_open(
    cx: &mut TestAppContext,
) {
    const DIRS: usize = 20;
    const FILES: usize = 1_000;
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for d in 0..DIRS {
        let folder = root.join(format!("dir{d:02}"));
        std::fs::create_dir(&folder).unwrap();
        for f in 0..FILES {
            std::fs::write(folder.join(format!("file{f:04}.txt")), "").unwrap();
        }
    }
    // Not a git repository: `.ignore` rules apply (`.gitignore` ones need a repository).
    write(&root, ".ignore", "ignored/\n");
    write(&root, "ignored/hidden.txt", "");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_until(cx, "the root's listing", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == DIRS + 1 && p.settled())
    });
    assert!(
        panel.read_with(cx, |p, _| !p.rows().iter().any(|r| r.name == "ignored")),
        "ignored entries are hidden"
    );
    for d in 0..DIRS {
        let folder = root.join(format!("dir{d:02}"));
        panel.update(cx, |p, cx| p.toggle(&folder, cx));
    }
    wait_until(cx, "every folder listed", |cx| {
        panel.read_with(cx, |p, _| {
            p.settled() && p.rows().len() == DIRS * (FILES + 1) + 1
        })
    });
    assert!(panel.read_with(cx, |p, _| p.rows().len()) >= 20_000);
    for _ in 0..3 {
        draw(cx);
    }
    let before = panel.read_with(cx, |p, _| p.rows_rendered());
    panel.update(cx, |_, cx| cx.notify());
    draw(cx);
    let one_frame = panel.read_with(cx, |p, _| p.rows_rendered()) - before;
    assert!(
        (1..=60).contains(&one_frame),
        "a frame built {one_frame} rows of {}",
        DIRS * (FILES + 1) + 1
    );
    assert!(
        panel.read_with(cx, |p, _| p.watched_dirs())
            <= hutan_desktop::workbench::panels::FILES_MAX_WATCHED_DIRS
    );
}

#[gpui::test]
fn the_panel_reveals_the_focused_tabs_file_and_follows_changes_on_disk(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let deep = write(&root, "src/a/b/deep.rs", &source(3));
    write(&root, "README.md", "hi\n");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_until(cx, "the root's listing", |cx| {
        panel.read_with(cx, |p, _| !p.rows().is_empty() && p.settled())
    });
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(
            hutan_desktop::workbench::FileOpen::picked(&deep),
            window,
            cx,
        );
    });
    wait_until(cx, "the file selected in the tree", |cx| {
        panel.read_with(cx, |p, _| {
            p.selected() == Some(deep.as_path()) && p.rows().iter().any(|r| r.path == deep)
        })
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p.active_file().map(Path::to_owned)),
        Some(deep.clone())
    );

    // A file created in an open folder appears without asking.
    let fresh = write(&root, "src/a/b/fresh.rs", "\n");
    wait_until(cx, "the new file's row", |cx| {
        panel.read_with(cx, |p, _| p.rows().iter().any(|r| r.path == fresh))
    });
    // Revealing opened exactly the folders the file is in: src, a, b.
    assert!(panel.read_with(cx, |p, _| {
        p.rows()
            .iter()
            .filter(|r| r.kind == FileRowKind::Dir && r.expanded)
            .count()
            == 3
    }));
}

#[gpui::test]
fn open_file_starts_in_the_active_project(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let fake = FakeOpenPanel::new();
    let mac = MacServices {
        open_panel: Rc::new(fake.clone()),
        ..MacServices::inert()
    };
    let options =
        WorkbenchOptions::builtin(settings(&daemon), daemon.connection(), None).with_mac(mac);
    let (view, cx) = open_options(cx, options);
    synced(&view, cx);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let file = write(&root, "notes.md", "# notes\n");
    project(&view, cx, &root);
    cx.dispatch_action(actions::OpenFileDialog);
    wait_until(cx, "the Open panel", |_| !fake.asked().is_empty());
    assert_eq!(fake.asked(), [root.clone()]);
    assert!(fake.answer(Some(file.clone())));
    let wanted = file.display().to_string();
    wait_until(cx, "the chosen file", |cx| {
        focused_file(&view, cx).is_some_and(|(path, _)| path == wanted)
    });
}

/// Pumps GPUI until `condition` holds; panics after `limit`.
fn wait_long(
    cx: &mut VisualTestContext,
    what: &str,
    limit: Duration,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + limit;
    while !condition(cx) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        draw(cx);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[gpui::test]
fn the_finders_first_results_on_a_hundred_thousand_files(cx: &mut TestAppContext) {
    const DIRS: usize = 100;
    const FILES: usize = 1_000;
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for d in 0..DIRS {
        let folder = root.join(format!("pkg{d:03}/src"));
        std::fs::create_dir_all(&folder).unwrap();
        for f in 0..FILES {
            std::fs::write(folder.join(format!("module_{f:04}.rs")), "").unwrap();
        }
    }
    write(&root, "pkg042/src/needle_handler.rs", "");
    project(&view, cx, &root);
    cx.dispatch_action(actions::QuickOpen);
    // Opening ⌘P builds the daemon's index; the timing starts once it is warm.
    wait_long(cx, "the index", Duration::from_secs(300), |cx| {
        view.read_with(cx, |wb, _| wb.picker().unwrap().files.indexed) as usize > DIRS * FILES
    });
    let mut times = Vec::new();
    for (n, key) in ["n", "e", "e", "d"].into_iter().enumerate() {
        let before = file_rows(&view, cx).2;
        let start = Instant::now();
        cx.simulate_input(key);
        // Key → the first frame drawn with its answer.
        wait_long(cx, "the answer", Duration::from_secs(60), |cx| {
            let (rows, searching, answers) = file_rows(&view, cx);
            !searching && answers > before && rows > 0
        });
        draw(cx);
        times.push(start.elapsed());
        let _ = n;
    }
    assert_eq!(
        titles(&view, cx).first().map(String::as_str),
        Some("needle_handler.rs")
    );
    let worst = times.iter().max().copied().unwrap_or_default();
    eprintln!("E-13a finder, 100 001 files: key → frame {times:?}, worst {worst:?}");
    let budget = if cfg!(debug_assertions) {
        // Unoptimized daemon and desktop: the number is recorded, the budget is release's.
        Duration::from_secs(5)
    } else {
        Duration::from_millis(50)
    };
    assert!(worst <= budget, "worst {worst:?} over {budget:?}");
}
