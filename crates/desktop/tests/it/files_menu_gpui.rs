#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! EX-4: the Files tree's multi-select (⇧ / ⌘ click, ⇧↑↓) with the operations and the read-only
//! actions taking every selected row, the row menu (groups, key hints, what each entry does),
//! compare tabs (two files, a file and HEAD) and rows dragged onto the chat composer and a
//! terminal. Real daemon, real sockets, real clicks and keys.

use crate::common;

use std::path::{Path, PathBuf};

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::panels::FilesPanel;
use hutan_desktop::workbench::{Region, Workbench, actions};
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

fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    (dir, root)
}

fn files_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<FilesPanel> {
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<FilesPanel>("files"))
        .expect("the Files panel is drawn")
}

/// The rows as the tree draws them: two spaces per level, then the name.
fn names(panel: &Entity<FilesPanel>, cx: &VisualTestContext) -> Vec<String> {
    panel.read_with(cx, |p, _| {
        p.rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    })
}

fn wait_names(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext, wanted: &[&str]) {
    let wanted: Vec<String> = wanted.iter().map(ToString::to_string).collect();
    wait_until(cx, &format!("the rows {wanted:?}"), |cx| {
        names(panel, cx) == wanted
    });
}

fn idle(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext) {
    wait_until(cx, "the operations answered and the tree listed", |cx| {
        panel.read_with(cx, |p, _| p.ops_running() == 0 && p.settled())
    });
}

/// Where the rows of the tree are drawn: the middle of the row at an index, in window
/// coordinates.
struct Rows {
    origin: gpui::Point<gpui::Pixels>,
    height: f32,
}

impl Rows {
    fn of(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Self {
        use hutan_desktop::theme::DensityExt as _;
        use hutan_desktop::workbench::style::UiStyle;
        let tree = view
            .read_with(cx, |wb, _| wb.region_bounds(Region::Files("tree")))
            .expect("the tree was drawn");
        let height = cx.update(|_, app| UiStyle::get(app).density.ui().sidebar_row - 4.0);
        Self {
            origin: tree.origin,
            height,
        }
    }

    fn at(&self, index: usize) -> gpui::Point<gpui::Pixels> {
        gpui::point(
            self.origin.x + gpui::px(80.0),
            self.origin.y + gpui::px(self.height * (index as f32 + 0.5)),
        )
    }
}

/// The rows selected, as names.
fn selected_names(panel: &Entity<FilesPanel>, cx: &VisualTestContext) -> Vec<String> {
    panel.read_with(cx, |p, _| {
        p.selection()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    })
}

fn sheet(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<(String, String, bool)> {
    view.read_with(cx, |wb, _| {
        wb.modal()
            .map(|m| (m.title.to_string(), m.message.to_string(), m.destructive))
    })
}

fn click(cx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>, modifiers: Modifiers) {
    cx.simulate_mouse_move(at, None, modifiers);
    cx.simulate_click(at, modifiers);
    draw(cx);
}

#[gpui::test]
fn shift_and_command_clicks_and_shift_arrows_select_several_rows_and_the_operations_take_them_all(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    write(&root, "docs/readme.md", "# docs\n");
    for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        write(&root, name, name);
    }
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["docs", "a.txt", "b.txt", "c.txt", "d.txt"]);
    draw(cx);
    let rows = Rows::of(&view, cx);

    // A plain click selects one row; ⇧-click takes the rows from it to the one clicked.
    let none = Modifiers::none();
    click(cx, rows.at(1), none);
    assert_eq!(selected_names(&panel, cx), ["a.txt"]);
    click(cx, rows.at(3), Modifiers::shift());
    assert_eq!(selected_names(&panel, cx), ["a.txt", "b.txt", "c.txt"]);
    // A ⇧-click moves only the end of the range: the anchor stays where the plain click was.
    click(cx, rows.at(2), Modifiers::shift());
    assert_eq!(selected_names(&panel, cx), ["a.txt", "b.txt"]);

    // ⌘-click takes a row in, and out again; it does not open anything.
    click(cx, rows.at(4), Modifiers::command());
    assert_eq!(selected_names(&panel, cx), ["a.txt", "b.txt", "d.txt"]);
    click(cx, rows.at(2), Modifiers::command());
    assert_eq!(selected_names(&panel, cx), ["a.txt", "d.txt"]);
    assert!(
        view.read_with(cx, |wb, _| wb
            .buffers()
            .get(&root.join("a.txt").display().to_string())
            .is_none()),
        "a modifier click only selects"
    );
    // ⌘-click on a folder selects it too and does not open it.
    click(cx, rows.at(0), Modifiers::command());
    assert_eq!(selected_names(&panel, cx), ["docs", "a.txt", "d.txt"]);
    assert!(!panel.read_with(cx, |p, _| p.tree().unwrap().is_expanded(&root.join("docs"))));

    // A plain click starts over; ⇧↓ and ⇧↑ extend and shrink the range from the anchor.
    click(cx, rows.at(2), none);
    assert_eq!(selected_names(&panel, cx), ["b.txt"]);
    cx.simulate_keystrokes("shift-down");
    cx.simulate_keystrokes("shift-down");
    assert_eq!(selected_names(&panel, cx), ["b.txt", "c.txt", "d.txt"]);
    cx.simulate_keystrokes("shift-up");
    assert_eq!(selected_names(&panel, cx), ["b.txt", "c.txt"]);
    cx.simulate_keystrokes("shift-up");
    cx.simulate_keystrokes("shift-up");
    assert_eq!(
        selected_names(&panel, cx),
        ["a.txt", "b.txt"],
        "past the anchor the range turns upward"
    );
    // A plain arrow collapses the selection to the row it lands on.
    cx.simulate_keystrokes("down");
    assert_eq!(selected_names(&panel, cx), ["b.txt"]);

    // The operations take every selected row: Duplicate copies both.
    cx.simulate_keystrokes("up");
    cx.simulate_keystrokes("shift-down");
    assert_eq!(selected_names(&panel, cx), ["a.txt", "b.txt"]);
    cx.simulate_keystrokes("cmd-d");
    wait_until(cx, "both copies", |_| {
        root.join("a copy.txt").exists() && root.join("b copy.txt").exists()
    });
    idle(&panel, cx);
    wait_names(
        &panel,
        cx,
        &[
            "docs",
            "a copy.txt",
            "a.txt",
            "b copy.txt",
            "b.txt",
            "c.txt",
            "d.txt",
        ],
    );

    // ⌘⌫ asks about all of them in one sheet; one answer sends both to the Trash.
    draw(cx);
    click(cx, rows.at(2), none);
    click(cx, rows.at(3), Modifiers::shift());
    let picked = selected_names(&panel, cx);
    assert_eq!(picked, ["a.txt", "b copy.txt"]);
    cx.simulate_keystrokes("cmd-backspace");
    draw(cx);
    let (title, message, destructive) = sheet(&view, cx).expect("the Trash sheet");
    assert_eq!(title, "Move 2 items to the Trash?");
    assert!(destructive);
    for name in &picked {
        assert!(message.contains(name.as_str()), "{message}");
    }
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "both rows gone", |_| {
        picked.iter().all(|name| !root.join(name).exists())
    });
    idle(&panel, cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.journal_depth()),
        (2, 0),
        "a duplicate and a trash, each one step"
    );

    // Rename is of one name: with two rows selected F2 does nothing.
    wait_names(&panel, cx, &["docs", "a copy.txt", "b.txt", "c.txt", "d.txt"]);
    draw(cx);
    click(cx, rows.at(1), none);
    click(cx, rows.at(2), Modifiers::shift());
    assert_eq!(selected_names(&panel, cx), ["a copy.txt", "b.txt"]);
    cx.simulate_keystrokes("f2");
    assert!(panel.read_with(cx, |p, _| p.editing().is_none()));
    // One row selected: F2 opens its name field.
    click(cx, rows.at(3), none);
    cx.simulate_keystrokes("f2");
    assert!(panel.read_with(cx, |p, _| p.editing().is_some()));
}

/// Presses on `from` and drags to `to` (the button stays down).
#[allow(dead_code)]
fn drag_to(
    cx: &mut VisualTestContext,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
) {
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        from + gpui::point(gpui::px(4.0), gpui::px(4.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    draw(cx);
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    draw(cx);
}
