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
    wait_until(cx, "src open and listed", |cx| {
        panel.read_with(cx, |p, _| {
            p.tree().unwrap().is_expanded(&root.join("src"))
                && p.tree().unwrap().is_listed(&root.join("src"))
        })
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

/// Renames `path` through the Files tree's own field, as the user does.
fn rename(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext, path: &Path, to: &str) {
    let path = path.to_owned();
    panel.update_in(cx, |p, window, cx| {
        assert!(p.begin_rename_of(path, window, cx), "the row exists");
    });
    draw(cx);
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input(to);
    cx.simulate_keystrokes("enter");
}

fn buffer_of(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    path: &Path,
) -> Option<Entity<hutan_code_view::FileBuffer>> {
    let key = path.display().to_string();
    view.read_with(cx, |wb, _| wb.buffers().get(&key).cloned())
}

fn text_of(buffer: &Entity<hutan_code_view::FileBuffer>, cx: &VisualTestContext) -> String {
    buffer.read_with(cx, |b, _| b.text().to_string())
}

fn has_tab(view: &Entity<Workbench>, cx: &VisualTestContext, path: &Path) -> bool {
    tab_paths(view, cx).contains(&path.display().to_string())
}

#[gpui::test]
fn a_rename_moves_the_tabs_with_their_unsaved_text_and_a_folder_takes_its_files_along(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    let a = write(&root, "src/a.txt", "alpha\n");
    let b = write(&root, "src/b.txt", "beta\n");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["src"]);
    panel.update(cx, |p, cx| p.toggle(&root.join("src"), cx));
    wait_names(&panel, cx, &["src", "  a.txt", "  b.txt"]);

    // Two tabs; the one in front has text the disk does not.
    open_tab(&view, cx, &b, "beta\n");
    open_tab(&view, cx, &a, "alpha\n");
    cx.simulate_input("zero ");
    let buffer = buffer_of(&view, cx, &a).expect("a.txt's buffer");
    assert!(buffer.read_with(cx, |b, _| b.is_dirty()));
    assert_eq!(text_of(&buffer, cx), "zero alpha\n");

    rename(&panel, cx, &a, "first.txt");
    let first = root.join("src/first.txt");
    wait_until(cx, "the file renamed on disk", |_| {
        first.exists() && !a.exists()
    });
    wait_until(cx, "the tab following", |cx| {
        has_tab(&view, cx, &first) && !has_tab(&view, cx, &a)
    });
    // The same buffer, under the new path: text and unsaved state kept, nothing under the old.
    let moved = buffer_of(&view, cx, &first).expect("a buffer under the new path");
    assert_eq!(moved.entity_id(), buffer.entity_id());
    assert!(buffer_of(&view, cx, &a).is_none());
    assert_eq!(text_of(&moved, cx), "zero alpha\n");
    assert!(moved.read_with(cx, |b, _| b.is_dirty() && b.path() == first));
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "alpha\n");

    // Saving writes the new file, not a file at the old path.
    cx.dispatch_action(actions::FocusEditor);
    draw(cx);
    cx.simulate_keystrokes("cmd-s");
    wait_until(cx, "the save", |cx| {
        moved.read_with(cx, |b, _| !b.is_dirty() && !b.is_saving())
    });
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "zero alpha\n");
    assert!(!a.exists(), "the old path is not written again");

    // A folder: every tab below it follows, and the open folder stays open under its new name.
    rename(&panel, cx, &root.join("src"), "lib");
    let (lib_first, lib_b) = (root.join("lib/first.txt"), root.join("lib/b.txt"));
    wait_until(cx, "the folder renamed", |_| {
        lib_first.exists() && lib_b.exists() && !root.join("src").exists()
    });
    wait_until(cx, "both tabs under lib", |cx| {
        has_tab(&view, cx, &lib_first) && has_tab(&view, cx, &lib_b)
    });
    assert_eq!(
        buffer_of(&view, cx, &lib_first).map(|b| b.entity_id()),
        Some(buffer.entity_id())
    );
    idle(&panel, cx);
    wait_names(&panel, cx, &["lib", "  b.txt", "  first.txt"]);
}

#[gpui::test]
fn a_write_that_races_a_rename_is_found_at_the_new_path(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    let clean = write(&root, "clean.txt", "one\n");
    let dirty = write(&root, "dirty.txt", "two\n");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["clean.txt", "dirty.txt"]);
    open_tab(&view, cx, &clean, "one\n");
    open_tab(&view, cx, &dirty, "two\n");
    cx.simulate_input("mine ");
    let dirty_buffer = buffer_of(&view, cx, &dirty).unwrap();
    let clean_buffer = buffer_of(&view, cx, &clean).unwrap();

    // Somebody (an agent) writes both files, and the rename follows at once: whichever of the
    // daemon's notice and the rename comes first, the new path ends up with what is on disk.
    std::fs::write(&clean, "agent wrote this\n").unwrap();
    std::fs::write(&dirty, "agent wrote that\n").unwrap();
    rename(&panel, cx, &clean, "clean2.txt");
    wait_until(cx, "the first rename", |_| root.join("clean2.txt").exists());
    idle(&panel, cx);
    rename(&panel, cx, &dirty, "dirty2.txt");
    wait_until(cx, "the second rename", |_| {
        root.join("dirty2.txt").exists()
    });

    wait_until(
        cx,
        "the clean buffer to reload what the agent wrote",
        |cx| text_of(&clean_buffer, cx) == "agent wrote this\n",
    );
    assert!(!clean_buffer.read_with(cx, |b, _| b.is_dirty()));
    wait_until(cx, "the bar on the buffer with unsaved text", |cx| {
        dirty_buffer.read_with(cx, |b, _| {
            matches!(
                b.conflict(),
                Some(hutan_code_view::Conflict::Changed { .. })
            )
        })
    });
    assert_eq!(
        text_of(&dirty_buffer, cx),
        "mine two\n",
        "the user's text is never replaced"
    );
    assert!(dirty_buffer.read_with(cx, |b, _| b.path() == root.join("dirty2.txt")));
}

#[gpui::test]
fn the_language_servers_document_is_opened_again_under_the_new_path(cx: &mut TestAppContext) {
    use crate::lsp_gpui::{Env, open_file};
    let env = Env::new(
        "always",
        |_| serde_json::json!({"diagnostics": true, "warnings": true}),
    );
    let (view, cx) = env.open(cx, "");
    let tab = open_file(&view, cx, &env.file("a.fake"), "let x = error;\n");
    wait_until(cx, "the server's diagnostic", |cx| {
        tab.buffer.read_with(cx, |b, _| b.diagnostics().len() == 1)
    });
    // Unsaved text: the server must get this, not what is on disk.
    cx.simulate_input("// top ");
    let opens = view.read_with(cx, |wb, cx| wb.lsp().read(cx).opens_sent());

    let panel = files_panel(&view, cx);
    wait_until(cx, "the tree", |cx| {
        panel.read_with(cx, |p, _| p.rows().iter().any(|r| r.name == "a.fake"))
    });
    rename(&panel, cx, &env.file("a.fake"), "b.fake");
    let new_path = env.file("b.fake").display().to_string();
    let old_path = env.file("a.fake").display().to_string();
    wait_until(cx, "the document under the new path", |cx| {
        view.read_with(cx, |wb, cx| {
            let lsp = wb.lsp().read(cx);
            lsp.is_open(&new_path) && !lsp.has_doc(&old_path)
        })
    });
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.lsp().read(cx).opens_sent()),
        opens + 1,
        "one document opened again"
    );
    // The server answers for the new document (the old diagnostics went with the old path).
    wait_until(cx, "the diagnostic again", |cx| {
        tab.buffer.read_with(cx, |b, _| {
            b.path() == env.file("b.fake") && b.diagnostics().len() == 1
        })
    });
    assert!(
        tab.buffer.read_with(cx, |b, _| b.is_dirty()
            && b.text().to_string() == "// top let x = error;\n"),
        "the unsaved text is what the server has"
    );
}

/// Whether `name` is anywhere in the folder the daemon's Trash is.
fn in_trash(daemon: &TestDaemon, name: &str) -> bool {
    fn walk(dir: &Path, name: &str) -> bool {
        std::fs::read_dir(dir).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name() == name || (e.path().is_dir() && walk(&e.path(), name)))
        })
    }
    walk(&daemon.dir.path().join("trash"), name)
}

fn sheet(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<(String, String, bool)> {
    view.read_with(cx, |wb, _| {
        wb.modal()
            .map(|m| (m.title.to_string(), m.message.to_string(), m.destructive))
    })
}

#[gpui::test]
fn the_trash_sheet_warns_of_unsaved_text_and_a_working_agent_and_undo_brings_it_back(
    cx: &mut TestAppContext,
) {
    use hutan_protocol::messages::{AgentApplied, AgentReportRequest, AgentState, Request};
    let daemon = TestDaemon::start();
    let (_dir, root) = scratch();
    let a = write(&root, "a.txt", "alpha\n");
    write(&root, "b.txt", "beta\n");
    // Codex works in this checkout.
    let workspace = daemon.create_workspace("proj", &[&root]);
    let session = daemon.create_session(&["sleep", "600"], Some(workspace.id));
    let applied: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        pid: None,
        session_id: session.id,
        source: "test".into(),
        agent: "codex".into(),
        state: Some(AgentState::Working),
        message: None,
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
    assert!(applied.applied);
    let (view, cx) = open(cx, &daemon);
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["a.txt", "b.txt"]);
    open_tab(&view, cx, &a, "alpha\n");
    cx.simulate_input("zero ");
    let buffer = buffer_of(&view, cx, &a).unwrap();
    wait_until(cx, "the agent to be working", |cx| {
        view.read_with(cx, |wb, cx| {
            !wb.sessions()
                .read(cx)
                .agents_working_near(&[root.clone()])
                .is_empty()
        })
    });

    // ⌘⌫ always asks: the name, the unsaved tab, the agent. Cancelling deletes nothing.
    focus_tree(&panel, cx);
    select(&panel, cx, &a);
    cx.simulate_keystrokes("cmd-backspace");
    let (title, message, destructive) = sheet(&view, cx).expect("the sheet");
    assert_eq!(title, "Move “a.txt” to the Trash?");
    assert!(destructive, "Return cancels, never confirms");
    assert!(
        message.contains("Unsaved changes in 1 tab (a.txt)"),
        "{message}"
    );
    assert!(
        message.contains("Codex is working in this checkout"),
        "{message}"
    );
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    draw(cx);
    assert!(a.exists() && !in_trash(&daemon, "a.txt"));

    // Confirmed: the Trash takes it; the tab says so and keeps the text.
    focus_tree(&panel, cx);
    cx.simulate_keystrokes("cmd-backspace");
    assert!(sheet(&view, cx).is_some());
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the file in the Trash", |_| {
        !a.exists() && in_trash(&daemon, "a.txt")
    });
    wait_until(cx, "the tab's deleted bar", |cx| {
        buffer.read_with(cx, |b, _| {
            matches!(b.conflict(), Some(hutan_code_view::Conflict::Deleted))
        })
    });
    assert_eq!(
        text_of(&buffer, cx),
        "zero alpha\n",
        "the text stays in the tab"
    );
    idle(&panel, cx);
    wait_names(&panel, cx, &["b.txt"]);
    assert_eq!(
        panel.read_with(cx, |p, _| p.selected().map(Path::to_owned)),
        Some(root.join("b.txt")),
        "the selection takes the place of the row that went"
    );

    // ⌘Z puts it back (the Trash's ticket) and the tab finds its file; ⇧⌘Z sends it away again.
    focus_tree(&panel, cx);
    cx.simulate_keystrokes("cmd-z");
    wait_until(cx, "the file back", |_| a.exists());
    wait_until(cx, "the bar gone", |cx| {
        buffer.read_with(cx, |b, _| b.conflict().is_none())
    });
    assert_eq!(text_of(&buffer, cx), "zero alpha\n");
    assert!(buffer.read_with(cx, |b, _| b.is_dirty()));
    idle(&panel, cx);
    wait_names(&panel, cx, &["a.txt", "b.txt"]);
    focus_tree(&panel, cx);
    cx.simulate_keystrokes("cmd-shift-z");
    wait_until(cx, "the file in the Trash again", |_| !a.exists());
    wait_until(cx, "the bar again", |cx| {
        buffer.read_with(cx, |b, _| {
            matches!(b.conflict(), Some(hutan_code_view::Conflict::Deleted))
        })
    });
}

fn _unused(_: ClipboardEntry, _: ClipboardItem, _: ExternalPaths, _: Modifiers) {}
