#![allow(clippy::expect_used, clippy::unwrap_used)] // tests
//! FX-WORK-2, the second workability evaluation's fixes: preview tabs and the *Open Editors*
//! section (N-03, N-13), the filter's relevance (N-02, N-12), the tree's root row, blame, and the
//! polish list (N-04 … N-11). Real daemon, real sockets; timers are the test clock's.

use crate::common;

use std::path::{Path, PathBuf};

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::panels::{FILES_MAX_OPEN_ROWS, FilesPanel};
use hutan_desktop::workbench::{FileOpen, Region, Workbench, actions};
use hutan_settings::{SettingsPaths, SettingsStore};

fn settings(daemon: &TestDaemon) -> SettingsStore {
    std::fs::write(
        daemon.home().join("settings.toml"),
        // Preview tabs are off by default (TABS-FILES); this file is about them.
        "[app]\nsetup_done = true\n[editor]\npreview_tabs = true\n",
    )
    .unwrap();
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, settings(daemon), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    (view, cx)
}

fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    (dir, root)
}

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

fn files_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<FilesPanel> {
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<FilesPanel>("files"))
        .expect("the Files panel is drawn")
}

fn wait_rows(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext, count: usize) {
    wait_until(cx, &format!("{count} rows"), |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == count)
    });
}

/// The point of the tree's row `index` (the sections above shift the tree, its region moves).
fn row_at(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    index: usize,
) -> gpui::Point<gpui::Pixels> {
    use hutan_desktop::theme::DensityExt as _;
    use hutan_desktop::workbench::style::UiStyle;
    let tree = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Files("tree")))
        .expect("the tree was drawn");
    let height = cx.update(|_, app| UiStyle::get(app).density.ui().sidebar_row - 4.0);
    gpui::point(
        tree.origin.x + gpui::px(80.0),
        tree.origin.y + gpui::px(height * (index as f32 + 0.5)),
    )
}

fn click_row(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    index: usize,
    modifiers: Modifiers,
) {
    let at = row_at(view, cx, index);
    cx.simulate_mouse_move(at, None, modifiers);
    cx.simulate_click(at, modifiers);
    draw(cx);
}

fn double_click_row(view: &Entity<Workbench>, cx: &mut VisualTestContext, index: usize) {
    let at = row_at(view, cx, index);
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers: Modifiers::none(),
        button: gpui::MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: at,
        modifiers: Modifiers::none(),
        button: gpui::MouseButton::Left,
        click_count: 1,
    });
    cx.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers: Modifiers::none(),
        button: gpui::MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: at,
        modifiers: Modifiers::none(),
        button: gpui::MouseButton::Left,
        click_count: 2,
    });
    draw(cx);
}

/// The files of the center's tabs, in tab order.
fn tabs(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| {
        wb.state()
            .center
            .panes()
            .into_iter()
            .flat_map(|pane| pane.items.iter())
            .filter_map(|item| item.as_file())
            .map(|p| {
                Path::new(p)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    })
}

fn preview(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| {
        wb.preview_file().map(|p| {
            Path::new(p)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
    })
}

fn shown(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| {
        wb.active_center_file().map(|p| {
            Path::new(&p)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
    })
}

fn wait_shown(view: &Entity<Workbench>, cx: &mut VisualTestContext, name: &str) {
    wait_until(cx, &format!("{name} to be shown"), |cx| {
        shown(view, cx).as_deref() == Some(name)
            && view.read_with(cx, |wb, cx| {
                wb.active_center_file()
                    .and_then(|p| wb.buffers().get(&p).map(|b| b.read(cx).is_ready()))
                    .unwrap_or(false)
            })
    });
}

#[gpui::test]
fn a_single_click_previews_a_file_and_the_next_click_takes_its_place(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        write(&root, name, name);
    }
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_rows(&panel, cx, 4);

    // One click: a preview tab.
    click_row(&view, cx, 0, Modifiers::none());
    wait_shown(&view, cx, "a.txt");
    assert_eq!(tabs(&view, cx), ["a.txt"]);
    assert_eq!(preview(&view, cx).as_deref(), Some("a.txt"));

    // The next click replaces it: still one tab, and a.txt's buffer is gone.
    click_row(&view, cx, 1, Modifiers::none());
    wait_shown(&view, cx, "b.txt");
    assert_eq!(tabs(&view, cx), ["b.txt"]);
    assert_eq!(preview(&view, cx).as_deref(), Some("b.txt"));
    view.read_with(cx, |wb, _| {
        assert_eq!(wb.buffers().len(), 1, "the replaced tab freed its buffer");
    });

    // A double click keeps the tab: it is no longer the preview and the next click adds one.
    double_click_row(&view, cx, 2);
    wait_shown(&view, cx, "c.txt");
    assert_eq!(preview(&view, cx), None);
    assert_eq!(tabs(&view, cx), ["c.txt"]);
    click_row(&view, cx, 0, Modifiers::none());
    wait_shown(&view, cx, "a.txt");
    assert_eq!(tabs(&view, cx), ["c.txt", "a.txt"]);
    assert_eq!(preview(&view, cx).as_deref(), Some("a.txt"));

    // An edit makes the preview a regular tab (the keyboard is in the tree after a click: the
    // editor takes it first).
    cx.dispatch_action(actions::FocusEditor);
    draw(cx);
    cx.simulate_input("typed ");
    wait_until(cx, "the edit to keep the tab", |cx| {
        preview(&view, cx).is_none()
    });
    click_row(&view, cx, 3, Modifiers::none());
    wait_shown(&view, cx, "d.txt");
    assert_eq!(tabs(&view, cx), ["c.txt", "a.txt", "d.txt"]);
    assert_eq!(preview(&view, cx).as_deref(), Some("d.txt"));
}

#[gpui::test]
fn opening_a_file_any_other_way_keeps_it_and_the_setting_turns_previews_off(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let store = settings(&daemon);
    let (view, cx) = open_with(cx, store.clone(), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    let (_dir, root) = scratch();
    let files: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt"]
        .iter()
        .map(|name| write(&root, name, name))
        .collect();
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_rows(&panel, cx, 3);

    click_row(&view, cx, 0, Modifiers::none());
    wait_shown(&view, cx, "a.txt");
    assert_eq!(preview(&view, cx).as_deref(), Some("a.txt"));
    // ⌘P / `open_file` shows a.txt on purpose: it stays.
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&files[0]), window, cx);
    });
    wait_until(cx, "a.txt to be kept", |cx| preview(&view, cx).is_none());
    // Another file by ⌘P is a regular tab beside it and the next click does not replace it.
    click_row(&view, cx, 1, Modifiers::none());
    wait_shown(&view, cx, "b.txt");
    assert_eq!(tabs(&view, cx), ["a.txt", "b.txt"]);
    assert_eq!(preview(&view, cx).as_deref(), Some("b.txt"));

    // Shift- and command-clicks select and never open.
    click_row(&view, cx, 2, Modifiers::shift());
    click_row(&view, cx, 0, Modifiers::command());
    assert_eq!(tabs(&view, cx), ["a.txt", "b.txt"], "no tab was added");

    // `editor.preview_tabs = false`: a click opens a regular tab.
    store
        .write_user("editor.preview_tabs", toml::Value::Boolean(false))
        .unwrap();
    click_row(&view, cx, 2, Modifiers::none());
    wait_shown(&view, cx, "c.txt");
    assert_eq!(tabs(&view, cx), ["a.txt", "b.txt", "c.txt"]);
    assert_eq!(
        preview(&view, cx).as_deref(),
        Some("b.txt"),
        "the old preview stays until it is replaced or kept"
    );
}

#[gpui::test]
fn open_editors_lists_the_preview_once_and_scrolls_past_six_rows_to_the_active_file(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    let files: Vec<PathBuf> = (0..12)
        .map(|n| write(&root, &format!("f{n:02}.txt"), "x"))
        .collect();
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_rows(&panel, cx, 12);
    for file in &files {
        view.update_in(cx, |wb, window, cx| {
            wb.open_file(FileOpen::picked(file), window, cx);
        });
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        wait_shown(&view, cx, &name);
    }
    wait_until(cx, "twelve editors", |cx| {
        panel.read_with(cx, |p, cx| p.open_editors(cx).len() == 12)
    });
    // A click on a tree row adds a preview: still one row per file.
    click_row(&view, cx, 0, Modifiers::none());
    wait_until(
        cx,
        "the preview to replace nothing and to be flagged",
        |cx| panel.read_with(cx, |p, cx| p.open_editors(cx).len() == 12),
    );

    // The section takes six rows, not twelve, and the active file's row is scrolled into them.
    let bounds = |cx: &mut VisualTestContext, region: Region| {
        view.read_with(cx, |wb, _| wb.region_bounds(region))
    };
    wait_until(cx, "the active row to be in view", |cx| {
        let (Some(rows), Some(row)) = (
            bounds(cx, Region::FilesSection("open-rows", 0)),
            bounds(cx, Region::FilesSection("open-editors", 0)),
        ) else {
            return false;
        };
        let _ = row;
        let name = shown(&view, cx);
        let active = panel.read_with(cx, |p, cx| {
            p.open_editors(cx)
                .iter()
                .position(|e| name.as_deref() == e.path.file_name().and_then(|n| n.to_str()))
        });
        let Some(active) = active else { return false };
        bounds(cx, Region::FilesSection("open-editors", active)).is_some_and(|r| {
            r.top() >= rows.top() - gpui::px(1.0) && r.bottom() <= rows.bottom() + gpui::px(1.0)
        })
    });
    let rows = bounds(cx, Region::FilesSection("open-rows", 0)).unwrap();
    let one = f32::from(
        bounds(cx, Region::FilesSection("open-editors", 11))
            .unwrap()
            .size
            .height,
    );
    assert!(
        f32::from(rows.size.height) <= one * FILES_MAX_OPEN_ROWS as f32 + 4.0,
        "{:?} is more than {FILES_MAX_OPEN_ROWS} rows of {one}",
        rows.size.height
    );
    assert!(
        f32::from(rows.size.height) >= one * (FILES_MAX_OPEN_ROWS as f32 - 1.0),
        "the section is about six rows tall"
    );
}

fn wheel(cx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>, dy: f32) {
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(dy))),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    draw(cx);
}

#[gpui::test]
fn collapse_all_and_refresh_scroll_the_tree_to_its_top(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    for n in 0..120 {
        write(&root, &format!("dir{n:03}/file.txt"), "x");
    }
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_rows(&panel, cx, 120);
    let at = row_at(&view, cx, 3);
    wheel(cx, at, -900.0);
    wait_until(cx, "the tree to be scrolled", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.scrolled() > 100.0)
    });
    // Collapse all lands on the first row.
    panel.update(cx, |p, cx| p.collapse_all(cx));
    wait_until(cx, "the tree at its top after Collapse all", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.scrolled() < 1.0)
    });
    // So does Refresh.
    let at = row_at(&view, cx, 3);
    wheel(cx, at, -900.0);
    wait_until(cx, "the tree to be scrolled again", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.scrolled() > 100.0)
    });
    panel.update(cx, |p, cx| p.refresh(cx));
    wait_until(cx, "the tree at its top after Refresh", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.scrolled() < 1.0)
    });
}

#[gpui::test]
fn a_rename_that_collides_says_so_under_the_edited_row(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    for name in ["a.txt", "b.txt", "c.txt"] {
        write(&root, name, name);
    }
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_rows(&panel, cx, 3);
    panel.update_in(cx, |p, window, cx| {
        p.select_path(&root.join("b.txt"), cx);
        p.focus_tree(window, cx);
    });
    draw(cx);
    cx.simulate_keystrokes("f2");
    wait_until(cx, "the name field", |cx| {
        panel.read_with(cx, |p, _| p.editing().is_some())
    });
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("c.txt");
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the collision to be said", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.editing().is_some_and(|e| e.error.is_some()))
            && view
                .read_with(cx, |wb, _| wb.region_bounds(Region::Files("name-error")))
                .is_some()
    });
    let error = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Files("name-error")))
        .unwrap();
    // The edited row is the second of three; the message hangs right under it, not at the bottom
    // of the panel.
    let second = row_at(&view, cx, 1);
    let third = row_at(&view, cx, 2);
    assert!(
        error.top() >= second.y && error.top() < third.y + gpui::px(6.0),
        "the message starts at {:?}, the edited row is at {:?}",
        error.top(),
        second.y
    );
    let tree = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Files("tree")))
        .unwrap();
    assert!(
        error.bottom() < tree.bottom(),
        "the message is inside the tree's area, not in a strip under it"
    );
}

#[gpui::test]
fn a_tooltip_goes_stale_when_the_wheel_moves_or_a_sheet_opens(cx: &mut TestAppContext) {
    use common::{offline_connection, open_with as open_offline};
    use hutan_desktop::workbench::style::{TextTooltip, TooltipGate};

    let (connection, settings, _) = offline_connection();
    let (view, cx) = open_offline(cx, settings, connection, None);
    draw(cx);
    let epoch = |cx: &mut VisualTestContext| cx.update(|_, app| TooltipGate::epoch(app));
    let fresh = |cx: &mut VisualTestContext| {
        cx.update(|_, app| gpui::AppContext::new(app, |_| TextTooltip::new("tip".into())))
    };

    // A tooltip is good until the next wheel notch.
    let before = fresh(cx);
    assert!(!cx.update(|_, app| before.read(app).stale(app)));
    let at = gpui::point(gpui::px(300.0), gpui::px(300.0));
    wheel(cx, at, -20.0);
    assert!(epoch(cx) > 0);
    assert!(
        cx.update(|_, app| before.read(app).stale(app)),
        "a tooltip of before the wheel moved draws nothing"
    );
    let after = fresh(cx);
    assert!(
        !cx.update(|_, app| after.read(app).stale(app)),
        "one made after it is fine"
    );

    // A sheet opening over the window makes what was shown stale, once.
    let epoch_before = epoch(cx);
    cx.dispatch_action(actions::OpenSettingsFor("appearance".into()));
    wait_until(cx, "the settings sheet", |cx| epoch(cx) > epoch_before);
    assert!(cx.update(|_, app| after.read(app).stale(app)));
    let inside = fresh(cx);
    draw(cx);
    draw(cx);
    assert!(
        !cx.update(|_, app| inside.read(app).stale(app)),
        "a tooltip of the sheet's own buttons shows"
    );
    let _ = view;
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
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

#[gpui::test]
fn the_counts_follow_a_burst_of_file_changes_without_waiting_for_the_next_notice(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (_dir, base) = scratch();
    let root = base.join("repo");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(&root, &["config", key, value]);
    }
    write(&root, "a.txt", "one\n");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    for n in 0..30 {
        write(&root, &format!("junk/f{n:02}.txt"), "x\n");
    }
    write(&root, "a.txt", "one\ntwo\n");

    let (view, cx) = open(cx, &daemon);
    project(&view, cx, &root);
    let badge = |cx: &VisualTestContext| view.read_with(cx, |wb, cx| wb.rail_badge("changes", cx));
    wait_until(cx, "31 uncommitted files", |cx| badge(cx) == Some(31));
    // The Files panel watches the folders it shows: open `junk`.
    let panel = files_panel(&view, cx);
    wait_until(cx, "the root's rows", |cx| {
        panel.read_with(cx, |p, _| !p.rows().is_empty())
    });
    panel.update(cx, |p, cx| p.toggle(&root.join("junk"), cx));
    wait_until(cx, "junk listed and watched", |cx| {
        panel.read_with(cx, |p, _| p.settled() && p.watched_dirs() > 0)
    });

    // Thirty files go at once (outside the window, as an agent or a script would); the badge
    // follows within a recount, with no window activation and no daemon worktree notice.
    std::fs::remove_dir_all(root.join("junk")).unwrap();
    wait_until(cx, "the badge to count again", |cx| {
        cx.executor()
            .advance_clock(hutan_desktop::sessions::FS_RECOUNT_AFTER);
        badge(cx) == Some(1)
    });
}

#[gpui::test]
fn a_wheel_notch_moves_the_tree_three_rows(cx: &mut TestAppContext) {
    use hutan_desktop::theme::DensityExt as _;
    use hutan_desktop::workbench::style::UiStyle;
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    for n in 0..120 {
        write(&root, &format!("f{n:03}.txt"), "x");
    }
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_rows(&panel, cx, 120);
    let row = cx.update(|_, app| UiStyle::get(app).density.ui().sidebar_row - 4.0);
    let at = row_at(&view, cx, 3);
    // A notch of a wheel mouse is one line: three rows of the list, not three lines of text.
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Lines(gpui::point(0.0, -1.0)),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    draw(cx);
    let moved = panel.read_with(cx, |p, _| p.scrolled());
    assert!(
        (moved - 3.0 * row).abs() < 1.0,
        "one notch moved {moved} px, three rows are {}",
        3.0 * row
    );
    // A trackpad's pixel deltas are exact as ever.
    wheel(cx, at, -10.0);
    let more = panel.read_with(cx, |p, _| p.scrolled());
    assert!((more - moved - 10.0).abs() < 1.0, "{moved} → {more}");
}

#[gpui::test]
fn the_tree_starts_with_a_root_row_that_names_the_folder_and_has_its_menu(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, base) = scratch();
    let root = base.join("my-project");
    for name in ["a.txt", "b.txt"] {
        write(&root, name, name);
    }
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_rows(&panel, cx, 2);
    wait_until(cx, "the root row", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Files("root-row")).is_some()
        })
    });
    let row = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Files("root-row")))
        .unwrap();
    let tree = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Files("tree")))
        .unwrap();
    // The first row of the tree area: right above the entries, not part of them.
    assert!(
        row.bottom() <= tree.top() + gpui::px(1.0),
        "{row:?} / {tree:?}"
    );
    assert_eq!(panel.read_with(cx, |p, _| p.rows().len()), 2);

    // Its menu is the folder's: right click, or the ⋯ at its end.
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    cx.simulate_mouse_down(row.center(), gpui::MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(row.center(), gpui::MouseButton::Right, Modifiers::none());
    draw(cx);
    let labels = view.read_with(cx, |wb, _| {
        wb.context_menu_entries()
            .expect("the folder menu is open")
            .into_iter()
            .map(|(label, _)| label.to_string())
            .collect::<Vec<_>>()
    });
    for wanted in ["New File", "New Folder", "Reveal in Finder", "Copy Path"] {
        assert!(
            labels.iter().any(|l| l.starts_with(wanted)),
            "{wanted} in {labels:?}"
        );
    }
}
