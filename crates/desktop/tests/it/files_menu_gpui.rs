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

/// The tree does not follow the focused tab here: a file previewed by a click would otherwise
/// select itself in the tree when its tab opens, whenever the daemon answers. Open Editors stays
/// off too: it would push the tree down as tabs open, and these tests click rows at fixed places.
fn settings(daemon: &TestDaemon) -> SettingsStore {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n[panels.files]\nauto_reveal = false\nopen_editors = \"never\"\n",
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
    // (The plain click on a.txt previewed it; b.txt and d.txt were only ever ⇧- or ⌘-clicked.)
    let opened = |name: &str, cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| {
            wb.buffers()
                .get(&root.join(name).display().to_string())
                .is_some()
        })
    };
    assert!(
        !opened("b.txt", cx) && !opened("d.txt", cx),
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
    wait_names(
        &panel,
        cx,
        &["docs", "a copy.txt", "b.txt", "c.txt", "d.txt"],
    );
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

fn right_click(cx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>) {
    let none = Modifiers::none();
    cx.simulate_mouse_move(at, None, none);
    cx.simulate_mouse_down(at, MouseButton::Right, none);
    cx.simulate_mouse_up(at, MouseButton::Right, none);
    draw(cx);
}

fn menu_labels(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("a menu is open")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect()
}

fn menu_hint(view: &Entity<Workbench>, cx: &VisualTestContext, label: &str) -> Option<String> {
    view.read_with(cx, |wb, _| wb.context_menu_hints())
        .into_iter()
        .find(|(l, _)| l == label)
        .unwrap_or_else(|| panic!("no entry {label}"))
        .1
        .map(|h| h.to_string())
}

fn menu_groups(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| wb.context_menu_groups())
        .into_iter()
        .map(|l| l.to_string())
        .collect()
}

/// Runs the entry named `label` of the open menu.
fn run_entry(view: &Entity<Workbench>, cx: &mut VisualTestContext, label: &str) {
    let at = menu_labels(view, cx)
        .iter()
        .position(|l| l == label)
        .unwrap_or_else(|| panic!("no entry {label} in {:?}", menu_labels(view, cx)));
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(at, window, cx));
    draw(cx);
}

fn clipboard_text(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
}

fn notice(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.notice().map(|n| n.to_string()))
}

#[gpui::test]
fn the_row_menu_is_grouped_shows_the_keys_and_its_read_only_entries_take_every_selected_row(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    write(&root, "src/lib.rs", "// lib\n");
    write(&root, "notes.md", "# notes\n");
    write(&root, "todo.md", "- [ ] todo\n");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["src", "notes.md", "todo.md"]);
    draw(cx);
    let rows = Rows::of(&view, cx);
    let none = Modifiers::none();

    // A file: the groups are open, compare, the hooks, the paths and the operations, each under
    // a line, and the entries that have a key show it.
    right_click(cx, rows.at(1));
    assert_eq!(
        menu_labels(&view, cx),
        [
            "Open",
            "Open to the Side",
            "Reveal in Finder",
            "Select for Compare",
            "Compare with HEAD",
            "Open in Terminal",
            "Attach to Chat",
            "Filter in this Folder",
            "Find in Folder",
            "Show File History",
            "Copy Path",
            "Copy Relative Path",
            "New File",
            "New Folder",
            "Rename",
            "Duplicate",
            "Cut",
            "Copy",
            "Paste",
            "Move to Trash"
        ]
    );
    assert_eq!(
        menu_groups(&view, cx),
        [
            "Select for Compare",
            "Open in Terminal",
            "Copy Path",
            "New File"
        ]
    );
    let hint = |label: &str, cx: &VisualTestContext| menu_hint(&view, cx, label);
    assert_eq!(hint("Open", cx).as_deref(), Some("↩"));
    assert_eq!(hint("Open to the Side", cx).as_deref(), Some("⌥↩"));
    assert_eq!(hint("Copy Path", cx).as_deref(), Some("⌥⌘C"));
    assert_eq!(hint("Copy Relative Path", cx).as_deref(), Some("⌥⌘⇧C"));
    assert_eq!(hint("New File", cx).as_deref(), Some("⌘N"));
    assert_eq!(hint("New Folder", cx).as_deref(), Some("⌥⌘N"));
    assert_eq!(hint("Rename", cx).as_deref(), Some("F2"));
    assert_eq!(hint("Duplicate", cx).as_deref(), Some("⌘D"));
    assert_eq!(hint("Cut", cx).as_deref(), Some("⌘X"));
    assert_eq!(hint("Copy", cx).as_deref(), Some("⌘C"));
    assert_eq!(hint("Paste", cx).as_deref(), Some("⌘V"));
    assert_eq!(hint("Move to Trash", cx).as_deref(), Some("⌘⌫"));
    assert_eq!(hint("Reveal in Finder", cx), None);
    cx.simulate_keystrokes("escape");
    assert!(
        view.read_with(cx, |wb, _| wb.context_menu_entries())
            .is_none()
    );

    // A folder: nothing to open or compare, Collapse Folder only while it is open.
    right_click(cx, rows.at(0));
    assert_eq!(
        menu_labels(&view, cx),
        [
            "Reveal in Finder",
            "Open in Terminal",
            "Filter in this Folder",
            "Find in Folder",
            "Copy Path",
            "Copy Relative Path",
            "New File",
            "New Folder",
            "Rename",
            "Duplicate",
            "Cut",
            "Copy",
            "Paste",
            "Move to Trash"
        ]
    );
    cx.simulate_keystrokes("escape");
    click(cx, rows.at(0), none);
    wait_names(&panel, cx, &["src", "  lib.rs", "notes.md", "todo.md"]);
    draw(cx);
    right_click(cx, rows.at(0));
    let labels = menu_labels(&view, cx);
    assert_eq!(
        labels[..5],
        [
            "Reveal in Finder",
            "Open in Terminal",
            "Filter in this Folder",
            "Find in Folder",
            "Collapse Folder"
        ]
    );
    assert_eq!(
        menu_hint(&view, cx, "Collapse Folder").as_deref(),
        Some("←")
    );
    run_entry(&view, cx, "Collapse Folder");
    wait_names(&panel, cx, &["src", "notes.md", "todo.md"]);

    // Two rows selected: a right click inside the selection keeps it, and the menu is about both.
    // What has one subject (a terminal, a search, a compare, a name) is not offered.
    click(cx, rows.at(1), none);
    click(cx, rows.at(2), Modifiers::shift());
    right_click(cx, rows.at(2));
    assert_eq!(selected_names(&panel, cx), ["notes.md", "todo.md"]);
    assert_eq!(
        menu_labels(&view, cx),
        [
            "Open",
            "Reveal in Finder",
            "Compare Selected",
            "Attach to Chat",
            "Copy Path",
            "Copy Relative Path",
            "Duplicate",
            "Cut",
            "Copy",
            "Paste",
            "Move to Trash"
        ]
    );
    run_entry(&view, cx, "Copy Path");
    assert_eq!(
        clipboard_text(cx),
        Some(format!(
            "{}\n{}",
            root.join("notes.md").display(),
            root.join("todo.md").display()
        ))
    );
    assert_eq!(notice(&view, cx).as_deref(), Some("Copied 2 paths"));
    right_click(cx, rows.at(2));
    run_entry(&view, cx, "Copy Relative Path");
    assert_eq!(clipboard_text(cx).as_deref(), Some("notes.md\ntodo.md"));

    // Reveal in Finder shows each (the window's handler stands in for Finder).
    let revealed = std::rc::Rc::new(std::cell::RefCell::new(Vec::<PathBuf>::new()));
    let sink = std::rc::Rc::clone(&revealed);
    view.update(cx, |wb, _| {
        wb.set_reveal_handler(move |path, _| sink.borrow_mut().push(path.to_owned()));
    });
    right_click(cx, rows.at(2));
    run_entry(&view, cx, "Reveal in Finder");
    assert_eq!(
        *revealed.borrow(),
        [root.join("notes.md"), root.join("todo.md")]
    );

    // Open opens every file among them in a tab.
    right_click(cx, rows.at(2));
    run_entry(&view, cx, "Open");
    wait_until(cx, "both tabs", |cx| {
        view.read_with(cx, |wb, _| {
            ["notes.md", "todo.md"].iter().all(|n| {
                wb.buffers()
                    .get(&root.join(n).display().to_string())
                    .is_some()
            })
        })
    });

    // (The tabs that opened revealed their file in the tree, which selected it: select both
    // again.)
    click(cx, rows.at(1), none);
    click(cx, rows.at(2), Modifiers::shift());
    // Attach to Chat with no chat open copies `@path` text for the next one.
    right_click(cx, rows.at(2));
    run_entry(&view, cx, "Attach to Chat");
    assert_eq!(clipboard_text(cx).as_deref(), Some("@notes.md @todo.md"));
    assert_eq!(
        notice(&view, cx).as_deref(),
        Some("No chat is open: copied @notes.md @todo.md")
    );

    // The keys: ⌥⌘C and ⌥⇧⌘C with the keyboard in the tree copy the selection's paths too.
    click(cx, rows.at(1), none);
    click(cx, rows.at(2), Modifiers::shift());
    cx.simulate_keystrokes("cmd-alt-c");
    assert_eq!(
        clipboard_text(cx),
        Some(format!(
            "{}\n{}",
            root.join("notes.md").display(),
            root.join("todo.md").display()
        ))
    );
    cx.simulate_keystrokes("cmd-alt-shift-c");
    assert_eq!(clipboard_text(cx).as_deref(), Some("notes.md\ntodo.md"));
}

#[gpui::test]
fn open_to_the_side_open_in_terminal_and_find_in_folder_start_from_the_row(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::workbench::panels::SearchPanel;
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    write(&root, "src/lib.rs", "// lib\n");
    write(&root, "notes.md", "# notes\n");
    write(&root, "todo.md", "- [ ] todo\n");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["src", "notes.md", "todo.md"]);
    draw(cx);
    let rows = Rows::of(&view, cx);
    let none = Modifiers::none();
    let panes =
        |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.state().center.panes().len());
    assert_eq!(panes(cx), 1);

    // Open to the Side puts the file in a split to the right.
    right_click(cx, rows.at(1));
    run_entry(&view, cx, "Open to the Side");
    wait_until(cx, "the split with the file", |cx| {
        panes(cx) == 2
            && view.read_with(cx, |wb, _| {
                wb.buffers()
                    .get(&root.join("notes.md").display().to_string())
                    .is_some()
            })
    });
    // ⌥↩ with the keyboard in the tree does the same for the row under the cursor: the split
    // on the right is reused (TABS-FILES), so no third pane — the file is a tab of it.
    let right_pane_has = |cx: &VisualTestContext, name: &str| {
        let path = root.join(name).display().to_string();
        view.read_with(cx, |wb, _| {
            wb.state().center.panes().last().is_some_and(|p| {
                p.items
                    .contains(&hutan_desktop::workbench::model::ItemRef::file(
                        path.clone(),
                    ))
            })
        })
    };
    click(cx, rows.at(2), none);
    cx.simulate_keystrokes("alt-enter");
    wait_until(cx, "todo.md beside", |cx| right_pane_has(cx, "todo.md"));
    assert_eq!(panes(cx), 2);
    click(cx, rows.at(1), none);
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "notes.md beside", |cx| right_pane_has(cx, "notes.md"));
    // (From the file's own pane, the side of it is a new split: it is the side of where you are.)
    assert!(panes(cx) >= 2);

    // Open in Terminal: a session whose directory is the folder (a file's folder).
    draw(cx);
    right_click(cx, rows.at(0));
    run_entry(&view, cx, "Open in Terminal");
    wait_until(cx, "a terminal in src", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .sessions()
                .iter()
                .any(|s| s.cwd.as_deref() == Some(root.join("src").as_path()))
        })
    });
    right_click(cx, rows.at(1));
    run_entry(&view, cx, "Open in Terminal");
    wait_until(cx, "a terminal in the root, where notes.md is", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .sessions()
                .iter()
                .any(|s| s.cwd.as_deref() == Some(root.as_path()))
        })
    });

    // Filter in this Folder: the filter field looks in that folder only, and has the keyboard.
    draw(cx);
    right_click(cx, rows.at(0));
    run_entry(&view, cx, "Filter in this Folder");
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.filter_scope().map(Path::to_owned)),
        Some(root.join("src"))
    );
    assert!(cx.update(|window, app| panel.read(app).filter_focused(window)));
    cx.simulate_keystrokes("escape");
    draw(cx);

    // Find in Folder: the Search panel's include glob names the folder.
    draw(cx);
    right_click(cx, rows.at(0));
    run_entry(&view, cx, "Find in Folder");
    draw(cx);
    let search = view
        .read_with(cx, |wb, _| wb.panel::<SearchPanel>("search"))
        .expect("the Search panel opened");
    wait_until(cx, "the include glob", |cx| {
        search.read_with(cx, |s, _| s.include() == "src/**")
    });
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The active tab of the focused pane.
fn active_item(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
) -> Option<hutan_desktop::workbench::model::ItemRef> {
    view.read_with(cx, |wb, _| {
        wb.state()
            .active_pane()
            .and_then(|pane| pane.active_item().cloned())
    })
}

#[gpui::test]
fn select_for_compare_then_compare_with_selected_opens_a_two_file_diff_tab(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::workbench::file_compare::CompareState;
    use hutan_desktop::workbench::model::ItemRef;
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    let a = write(&root, "a.txt", "one\ntwo\nthree\n");
    let b = write(&root, "b.txt", "one\n2\nthree\nfour\n");
    let same = write(&root, "same.txt", "one\ntwo\nthree\n");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["a.txt", "b.txt", "same.txt"]);
    draw(cx);
    let rows = Rows::of(&view, cx);

    // Nothing selected yet: no Compare with Selected. Select for Compare says what it holds.
    right_click(cx, rows.at(0));
    assert!(!menu_labels(&view, cx).contains(&"Compare with Selected".to_owned()));
    run_entry(&view, cx, "Select for Compare");
    assert_eq!(
        notice(&view, cx).as_deref(),
        Some("Selected “a.txt” for compare")
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.compare_selection().map(Path::to_owned)),
        Some(a.clone())
    );
    // On the selected file itself there is nothing to compare it with.
    right_click(cx, rows.at(0));
    assert!(!menu_labels(&view, cx).contains(&"Compare with Selected".to_owned()));
    cx.simulate_keystrokes("escape");

    // On another file: Compare with Selected (a left, b right).
    right_click(cx, rows.at(1));
    let labels = menu_labels(&view, cx);
    let at = |label: &str| labels.iter().position(|l| l == label).unwrap();
    assert!(at("Select for Compare") < at("Compare with Selected"));
    assert!(at("Compare with Selected") < at("Compare with HEAD"));
    run_entry(&view, cx, "Compare with Selected");
    let item = ItemRef::compare(a.display().to_string(), b.display().to_string());
    assert_eq!(
        active_item(&view, cx),
        Some(item.clone()),
        "its tab is the active one"
    );
    let compare = view
        .read_with(cx, |wb, _| wb.compare(&item))
        .expect("its view");
    wait_until(cx, "the diff", |cx| {
        compare.read_with(cx, |c, _| c.diff().is_some())
    });
    compare.read_with(cx, |c, _| {
        let diff = c.diff().unwrap();
        let lines: Vec<(hutan_chat_view::diff::LineKind, &str)> = diff
            .lines
            .iter()
            .map(|l| (l.kind, l.text.as_str()))
            .collect();
        use hutan_chat_view::diff::LineKind::{Add, Remove};
        assert!(lines.contains(&(Remove, "two")), "{lines:?}");
        assert!(lines.contains(&(Add, "2")), "{lines:?}");
        assert!(lines.contains(&(Add, "four")), "{lines:?}");
        assert_eq!((diff.added, diff.removed), (2, 1));
        assert_eq!(c.title(), "Compare · a.txt ↔ b.txt");
    });
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.item_title(&item, cx).to_string()),
        "Compare · a.txt ↔ b.txt"
    );
    draw(cx);

    // The same text on both sides says so; two files selected in the tree are compared too.
    click(cx, rows.at(0), Modifiers::none());
    click(cx, rows.at(2), Modifiers::shift());
    assert_eq!(selected_names(&panel, cx), ["a.txt", "b.txt", "same.txt"]);
    click(cx, rows.at(0), Modifiers::none());
    click(cx, rows.at(2), Modifiers::command());
    right_click(cx, rows.at(2));
    assert_eq!(
        menu_labels(&view, cx)
            .iter()
            .filter(|l| l.starts_with("Compare"))
            .collect::<Vec<_>>(),
        ["Compare Selected"]
    );
    run_entry(&view, cx, "Compare Selected");
    let item = ItemRef::compare(a.display().to_string(), same.display().to_string());
    assert_eq!(active_item(&view, cx), Some(item.clone()));
    let compare = view.read_with(cx, |wb, _| wb.compare(&item)).unwrap();
    wait_until(cx, "the verdict", |cx| {
        compare.read_with(cx, |c, _| !matches!(c.state(), CompareState::Loading))
    });
    assert!(compare.read_with(cx, |c, _| matches!(c.state(), CompareState::Identical)));

    // A file that is gone is told, not drawn as empty.
    let gone = ItemRef::compare(
        a.display().to_string(),
        root.join("nope.txt").display().to_string(),
    );
    view.update_in(cx, |wb, window, cx| {
        wb.open_compare(&a, &root.join("nope.txt"), window, cx);
    });
    let missing = view.read_with(cx, |wb, _| wb.compare(&gone)).unwrap();
    wait_until(cx, "the refusal", |cx| {
        missing.read_with(cx, |c, _| matches!(c.state(), CompareState::Failed(_)))
    });
}

#[gpui::test]
fn compare_with_head_diffs_the_file_against_its_committed_text(cx: &mut TestAppContext) {
    use hutan_desktop::workbench::file_compare::CompareState;
    use hutan_desktop::workbench::model::ItemRef;
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    git(&root, &["init", "-q", "-b", "main"]);
    let a = write(&root, "a.txt", "one\ntwo\nthree\n");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    std::fs::write(&a, "one\nTWO\nthree\n").unwrap();
    let fresh = write(&root, "fresh.txt", "never committed\n");
    daemon.create_workspace("proj", &[&root]);
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["a.txt", "fresh.txt"]);
    draw(cx);
    let rows = Rows::of(&view, cx);

    right_click(cx, rows.at(0));
    run_entry(&view, cx, "Compare with HEAD");
    let item = ItemRef::compare_head(a.display().to_string());
    assert_eq!(active_item(&view, cx), Some(item.clone()));
    let compare = view.read_with(cx, |wb, _| wb.compare(&item)).unwrap();
    wait_until(cx, "the diff against HEAD", |cx| {
        compare.read_with(cx, |c, _| c.diff().is_some())
    });
    compare.read_with(cx, |c, _| {
        use hutan_chat_view::diff::LineKind::{Add, Remove};
        let lines: Vec<_> = c
            .diff()
            .unwrap()
            .lines
            .iter()
            .map(|l| (l.kind, l.text.as_str()))
            .collect();
        assert!(
            lines.contains(&(Remove, "two")) && lines.contains(&(Add, "TWO")),
            "{lines:?}"
        );
        assert_eq!(c.title(), "Compare · a.txt ↔ HEAD");
    });

    // A file git does not have has no HEAD to compare with: the tab says why.
    right_click(cx, rows.at(1));
    run_entry(&view, cx, "Compare with HEAD");
    let item = ItemRef::compare_head(fresh.display().to_string());
    let compare = view.read_with(cx, |wb, _| wb.compare(&item)).unwrap();
    wait_until(cx, "the refusal", |cx| {
        compare.read_with(cx, |c, _| matches!(c.state(), CompareState::Failed(_)))
    });
    let CompareState::Failed(why) = compare.read_with(cx, |c, _| c.state().clone()) else {
        unreachable!()
    };
    assert!(why.contains("fresh.txt"), "{why}");
}

/// A chat of the project `workspace`, working in `cwd`.
fn chat_in(
    daemon: &TestDaemon,
    workspace: hutan_core::WorkspaceId,
    cwd: &Path,
) -> hutan_protocol::messages::SessionInfo {
    use hutan_protocol::chat::ChatCreate;
    use hutan_protocol::messages::{Request, SessionResult};
    let created: SessionResult = daemon.call(Request::ChatCreate(ChatCreate {
        extra_dirs: Vec::new(),
        contain: None,
        workspace_id: Some(workspace),
        cwd: cwd.to_path_buf(),
        agent: "codex".to_owned(),
        model: None,
        approval_policy: None,
        sandbox: None,
        title: None,
        mode: None,
        provider_id: None,
        system_prompt: None,
        account: None,
        safety: None,
    }));
    created.session
}

/// A project with `src/lib.rs`, `notes.md` and `todo.md` and a daemon whose codex replays a
/// recorded turn.
struct ChatProject {
    _env: common::chat::ChatEnv,
    daemon: TestDaemon,
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl ChatProject {
    fn new() -> Self {
        let env = common::chat::ChatEnv::new();
        let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
        let (dir, root) = scratch();
        write(&root, "src/lib.rs", "// lib\n");
        write(&root, "notes.md", "# notes\n");
        write(&root, "todo.md", "- [ ] todo\n");
        Self {
            _env: env,
            daemon,
            _dir: dir,
            root,
        }
    }

    /// The window with the project open, a chat of it as the active tab and the Files tree with
    /// `src` open: the rows are `src`, `lib.rs`, `notes.md`, `todo.md`.
    fn window<'a>(
        &self,
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<Workbench>,
        &'a mut VisualTestContext,
        hutan_core::SessionId,
        Entity<FilesPanel>,
    ) {
        use hutan_desktop::workbench::chat::LoadStatus;
        let workspace = self.daemon.create_workspace("proj", &[&self.root]);
        let chat = chat_in(&self.daemon, workspace.id, &self.root).id;
        let (view, cx) = open(cx, &self.daemon);
        project(&view, cx, &self.root);
        view.update_in(cx, |wb, window, cx| wb.open_session(chat, window, cx));
        wait_until(cx, "the chat to load", |cx| {
            view.read_with(cx, |wb, cx| {
                wb.chat_model_of(chat)
                    .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
            })
        });
        let panel = files_panel(&view, cx);
        wait_names(&panel, cx, &["src", "notes.md", "todo.md"]);
        draw(cx);
        let rows = Rows::of(&view, cx);
        click(cx, rows.at(0), Modifiers::none());
        wait_names(&panel, cx, &["src", "  lib.rs", "notes.md", "todo.md"]);
        draw(cx);
        (view, cx, chat, panel)
    }
}

#[gpui::test]
fn attach_to_chat_puts_a_file_chip_of_each_selected_file_in_the_open_chat(cx: &mut TestAppContext) {
    use hutan_protocol::assistant::Attachment;
    let fx = ChatProject::new();
    let (view, cx, chat, panel) = fx.window(cx);
    let model = view.read_with(cx, |wb, _| wb.chat_model_of(chat).unwrap());
    let rows = Rows::of(&view, cx);

    // One file: a chip that names it relative to the tree's root and reads the absolute path.
    right_click(cx, rows.at(1));
    run_entry(&view, cx, "Attach to Chat");
    let chips = model.read_with(cx, |m, _| m.context().to_vec());
    assert_eq!(chips.len(), 1, "{chips:?}");
    assert_eq!(chips[0].source, "src/lib.rs");
    assert_eq!(
        chips[0].attachment,
        Attachment::File {
            path: fx.root.join("src/lib.rs").display().to_string(),
            range: None,
            picked: true
        }
    );
    assert_eq!(
        notice(&view, cx).as_deref(),
        Some("Attached 1 file to the chat")
    );

    // Two rows: a chip each. (The click on notes.md previews it over the chat in its pane: the
    // chat is behind that tab and still the chat Attach to Chat means.)
    click(cx, rows.at(2), Modifiers::none());
    click(cx, rows.at(3), Modifiers::shift());
    assert_eq!(selected_names(&panel, cx), ["notes.md", "todo.md"]);
    right_click(cx, rows.at(3));
    run_entry(&view, cx, "Attach to Chat");
    let sources: Vec<String> = model.read_with(cx, |m, _| {
        m.context().iter().map(|c| c.source.clone()).collect()
    });
    assert_eq!(sources, ["src/lib.rs", "notes.md", "todo.md"]);
    assert_eq!(
        notice(&view, cx).as_deref(),
        Some("Attached 2 files to the chat")
    );
}

#[gpui::test]
fn a_row_dragged_onto_the_chat_composer_types_its_relative_path(cx: &mut TestAppContext) {
    let fx = ChatProject::new();
    let (view, cx, chat, panel) = fx.window(cx);
    let model = view.read_with(cx, |wb, _| wb.chat_model_of(chat).unwrap());
    let rows = Rows::of(&view, cx);
    let composer = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Chat("composer")))
        .expect("the composer is drawn")
        .center();
    let text = |cx: &VisualTestContext| model.read_with(cx, |m, _| m.input().text().to_owned());

    // A nested row: its path below the tree's root, and a space after it.
    drag_to(cx, rows.at(1), composer);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("path-overlay")))
            .is_some(),
        "the chat shows it takes the drop"
    );
    cx.simulate_mouse_up(composer, MouseButton::Left, Modifiers::none());
    draw(cx);
    assert_eq!(text(cx), "src/lib.rs ");
    assert_eq!(
        panel.read_with(cx, |p, _| p.journal_depth()),
        (0, 0),
        "nothing was moved on disk"
    );

    // Several selected rows travel together. (The keys select without previewing a file over
    // the chat, which would take the composer away.)
    draw(cx);
    panel.update_in(cx, |p, window, cx| p.focus_tree(window, cx));
    cx.simulate_keystrokes("down down shift-down");
    assert_eq!(selected_names(&panel, cx), ["notes.md", "todo.md"]);
    drag_to(cx, rows.at(3), composer);
    cx.simulate_mouse_up(composer, MouseButton::Left, Modifiers::none());
    draw(cx);
    assert_eq!(text(cx), "src/lib.rs notes.md todo.md ");
    assert!(fx.root.join("notes.md").exists() && fx.root.join("todo.md").exists());
}

#[gpui::test]
fn a_row_dragged_onto_a_terminal_types_its_relative_path_quoted_for_the_shell(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (_dir, root) = scratch();
    write(&root, "src/lib.rs", "// lib\n");
    write(&root, "my notes.md", "# notes\n");
    let workspace = daemon.create_workspace("proj", &[&root]);
    // `cat` echoes what is typed, so the screen shows what the terminal received.
    let session = daemon.create_session(&["cat"], Some(workspace.id));
    let (view, cx) = open(cx, &daemon);
    project(&view, cx, &root);
    view.update_in(cx, |wb, window, cx| wb.open_session(session.id, window, cx));
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["src", "my notes.md"]);
    draw(cx);
    let rows = Rows::of(&view, cx);
    click(cx, rows.at(0), Modifiers::none());
    wait_names(&panel, cx, &["src", "  lib.rs", "my notes.md"]);
    draw(cx);
    let terminal = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Center))
        .expect("the centre is drawn")
        .center();
    let screen = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(session.id)
                .map(|v| common::screen_text(&v.read(cx).model()))
                .unwrap_or_default()
        })
    };

    drag_to(cx, rows.at(1), terminal);
    cx.simulate_mouse_up(terminal, MouseButton::Left, Modifiers::none());
    draw(cx);
    wait_until(cx, "the path on the terminal's screen", |cx| {
        screen(cx).contains("src/lib.rs")
    });
    // A name with a space is quoted, as a dropped Finder file is.
    draw(cx);
    drag_to(cx, rows.at(2), terminal);
    cx.simulate_mouse_up(terminal, MouseButton::Left, Modifiers::none());
    draw(cx);
    wait_until(cx, "the quoted name on the screen", |cx| {
        screen(cx).contains("'my notes.md'")
    });
}

/// Presses on `from` and drags to `to` (the button stays down).
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
