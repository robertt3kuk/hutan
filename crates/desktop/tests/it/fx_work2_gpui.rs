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
        "[app]\nsetup_done = true\n",
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
