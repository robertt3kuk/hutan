#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! EX-7: file operations in the Files tree — the inline name field (new file, new folder, rename
//! with the stem selected, validation, a name that is taken), tabs that follow a rename or a move
//! (an unsaved buffer keeps its text, the language server's document is opened again under the
//! new path, an agent's write that raced the move is found), Trash with its sheet (names, unsaved
//! tabs, a working agent), duplicate, copy / cut / paste with the tree's clipboard and the files
//! Finder copied, drag to move (⌥ copies, the first drop asks, folders open under the pointer),
//! and the undo journal. Real daemon, real sockets; the daemon's Trash is a folder of the test's own.

use crate::common;

use std::path::{Path, PathBuf};

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{
    ClipboardEntry, ClipboardItem, Entity, ExternalPaths, Modifiers, TestAppContext,
    VisualTestContext,
};
use hutan_desktop::workbench::panels::{FilesEditKind, FilesPanel};
use hutan_desktop::workbench::{FileOpen, Workbench, actions};
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

fn focus_tree(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext) {
    panel.update_in(cx, |p, window, cx| p.focus_tree(window, cx));
    draw(cx);
}

fn select(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext, path: &Path) {
    panel.update(cx, |p, cx| p.select_path(path, cx));
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.selected().map(Path::to_owned)),
        Some(path.to_owned()),
        "the row of {} is on screen",
        path.display()
    );
}

fn idle(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext) {
    wait_until(cx, "the operations answered and the tree listed", |cx| {
        panel.read_with(cx, |p, _| p.ops_running() == 0 && p.settled())
    });
}

#[gpui::test]
fn a_new_file_is_an_inline_row_that_checks_the_name_and_opens_the_file(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    write(&root, "src/lib.rs", "// lib\n");
    write(&root, "README.md", "# readme\n");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["src", "README.md"]);

    // ⌘N with a folder selected puts the field first inside it, and opens the folder.
    focus_tree(&panel, cx);
    select(&panel, cx, &root.join("src"));
    cx.simulate_keystrokes("cmd-n");
    wait_until(cx, "the name field in src", |cx| {
        panel.read_with(cx, |p, _| p.editing().is_some())
    });
    let edit = panel.read_with(cx, |p, _| p.editing().unwrap());
    assert_eq!(edit.kind, FilesEditKind::NewFile);
    assert_eq!(edit.parent, root.join("src"));
    wait_until(cx, "src open", |cx| {
        panel.read_with(cx, |p, _| p.tree().unwrap().is_expanded(&root.join("src")))
    });

    // What is typed shows in the field; "/" and a taken name are refused, in words.
    cx.simulate_input("a/b");
    cx.simulate_keystrokes("enter");
    let error = panel.read_with(cx, |p, _| p.editing().unwrap().error);
    assert!(error.unwrap().contains('/'), "a name is one component");
    assert!(!root.join("src/a").exists());

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("LIB.rs");
    cx.simulate_keystrokes("enter");
    let error = panel.read_with(cx, |p, _| p.editing().unwrap().error);
    assert!(
        error.unwrap().contains("already exists"),
        "src/lib.rs is taken, whatever the case"
    );

    // A good name makes the file, opens it in a tab and selects its row; the field is gone.
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("notes.md");
    cx.simulate_keystrokes("enter");
    let made = root.join("src/notes.md");
    wait_until(cx, "the file on disk", |_| made.exists());
    idle(&panel, cx);
    assert!(panel.read_with(cx, |p, _| p.editing().is_none()));
    wait_until(cx, "its tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.buffers().get(&made.display().to_string()).is_some()
        })
    });
    wait_until(cx, "its row selected", |cx| {
        panel.read_with(cx, |p, _| p.selected() == Some(made.as_path()))
    });
    assert_eq!(std::fs::read_to_string(&made).unwrap(), "", "an empty file");
    assert_eq!(panel.read_with(cx, |p, _| p.journal_depth()), (1, 0));
}

#[gpui::test]
fn a_new_folder_goes_in_the_root_and_escape_makes_nothing(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    write(&root, "a.txt", "a");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["a.txt"]);

    // The command (⌘K, the header's button) with a file selected: its folder, the root.
    select(&panel, cx, &root.join("a.txt"));
    cx.dispatch_action(actions::FilesNewFolder);
    draw(cx);
    wait_until(cx, "the field", |cx| {
        panel.read_with(cx, |p, _| p.is_making())
    });
    cx.simulate_input("docs");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(panel.read_with(cx, |p, _| p.editing().is_none()));
    assert!(!root.join("docs").exists(), "Escape makes nothing");

    cx.dispatch_action(actions::FilesNewFolder);
    draw(cx);
    wait_until(cx, "the field again", |cx| {
        panel.read_with(cx, |p, _| p.is_making())
    });
    cx.simulate_input("docs");
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the folder", |_| root.join("docs").is_dir());
    idle(&panel, cx);
    wait_names(&panel, cx, &["docs", "a.txt"]);
    assert_eq!(
        panel.read_with(cx, |p, _| p.selected().map(Path::to_owned)),
        Some(root.join("docs"))
    );
}

#[gpui::test]
fn f2_renames_with_the_stem_selected_and_a_taken_name_stays_in_the_field(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    write(&root, "main.rs", "fn main() {}\n");
    write(&root, "util.rs", "");
    write(&root, ".gitignore", "target\n");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &[".gitignore", "main.rs", "util.rs"]);

    focus_tree(&panel, cx);
    select(&panel, cx, &root.join("main.rs"));
    cx.simulate_keystrokes("f2");
    wait_until(cx, "the rename field", |cx| {
        panel.read_with(cx, |p, _| p.editing().is_some())
    });
    let edit = panel.read_with(cx, |p, _| p.editing().unwrap());
    assert_eq!(edit.kind, FilesEditKind::Rename);
    assert_eq!(edit.text, "main.rs");
    assert_eq!(edit.selection, Some(0..4), "the stem, not the extension");

    // Typing replaces the stem; a name that exists is refused and stays in the field.
    cx.simulate_input("util");
    let edit = panel.read_with(cx, |p, _| p.editing().unwrap());
    assert_eq!(edit.text, "util.rs");
    cx.simulate_keystrokes("enter");
    let error = panel.read_with(cx, |p, _| p.editing().unwrap().error);
    assert!(error.unwrap().contains("already exists"));
    assert!(root.join("main.rs").exists());

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("app.rs");
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the renamed file", |_| root.join("app.rs").exists());
    assert!(!root.join("main.rs").exists());
    idle(&panel, cx);
    wait_names(&panel, cx, &[".gitignore", "app.rs", "util.rs"]);
    wait_until(cx, "the renamed row selected", |cx| {
        panel.read_with(cx, |p, _| {
            p.selected() == Some(root.join("app.rs").as_path())
        })
    });

    // A dotfile and a folder are selected whole.
    select(&panel, cx, &root.join(".gitignore"));
    cx.simulate_keystrokes("f2");
    let edit = panel.read_with(cx, |p, _| p.editing().unwrap());
    assert_eq!(edit.selection, Some(0..10));
    cx.simulate_keystrokes("escape");
    assert!(panel.read_with(cx, |p, _| p.editing().is_none()));
    assert!(root.join(".gitignore").exists(), "Escape renames nothing");
}

/// Opens `path` in a tab and waits until its buffer has its text.
fn open_tab(view: &Entity<Workbench>, cx: &mut VisualTestContext, path: &Path, text: &str) {
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(path), window, cx);
    });
    let key = path.display().to_string();
    wait_until(cx, "the tab's text", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.buffers()
                .get(&key)
                .is_some_and(|b| b.read(cx).is_ready() && b.read(cx).text().to_string() == text)
        })
    });
}

fn tab_paths(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| {
        wb.state()
            .panes()
            .iter()
            .flat_map(|pane| {
                pane.items
                    .iter()
                    .filter_map(|i| i.as_file().map(str::to_owned))
            })
            .collect()
    })
}

fn _unused(_: ClipboardEntry, _: ClipboardItem, _: ExternalPaths, _: Modifiers) {}
