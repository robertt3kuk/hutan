#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! EX-3: filter as you type in the Files panel — the field (a character typed in the tree, ⌘F),
//! the instant matches over what the tree has listed while the daemon's answer is held back, the
//! daemon's answer replacing them (`fs.find`, debounced, an older answer dropped), the projection
//! with folders made up from the paths, highlight mode, ↓ into the results, Return opening a
//! file, Escape bringing the tree back as it was, a filter scoped to a folder (the row after a
//! truncated listing, *Filter in Folder*), and a 100 000-file project. Real daemon, real sockets;
//! the timers are the test clock's.

use crate::common;

use std::path::{Path, PathBuf};

use common::{Hold, TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_client::Connection;
use hutan_desktop::workbench::panels::{
    FILES_FILTER_DEBOUNCE, FileRowKind, FilesFilterStatus, FilesPanel,
};
use hutan_desktop::workbench::{Workbench, actions};
use hutan_settings::{SettingsPaths, SettingsStore};

fn settings(daemon: &TestDaemon) -> SettingsStore {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

fn open_over<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    connection: Connection,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, settings(daemon), connection, None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    (view, cx)
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_over(cx, daemon, daemon.connection())
}

fn open_held<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext, Hold) {
    let (connection, hold) = daemon.held_connection();
    let (view, cx) = open_over(cx, daemon, connection);
    (view, cx, hold)
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

/// The rows as the panel draws them: two spaces per level, then the name.
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

fn in_field(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, app| panel.read(app).filter_focused(window))
}

fn in_tree(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, app| panel.read(app).has_focus(window, app))
}

/// Types `text` with the keyboard in the tree: the first character starts the filter and puts the
/// keyboard in the field, the rest is typed there.
fn type_text(cx: &mut VisualTestContext, text: &str) {
    let mut chars = text.chars();
    let first = chars.next().expect("something to type");
    cx.simulate_keystrokes(&first.to_string());
    draw(cx);
    let rest: String = chars.collect();
    if !rest.is_empty() {
        cx.simulate_input(&rest);
        draw(cx);
    }
}

fn status(panel: &Entity<FilesPanel>, cx: &VisualTestContext) -> FilesFilterStatus {
    panel.read_with(cx, |p, _| p.filter_status().clone())
}

/// Ends the debounce (the timers are the test clock's) and waits for the daemon's answer to the
/// query typed.
fn wait_found(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext) {
    cx.executor().advance_clock(FILES_FILTER_DEBOUNCE);
    wait_until(cx, "the daemon's answer", |cx| {
        matches!(status(panel, cx), FilesFilterStatus::Found { .. })
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

/// A small project: listed at the root and in `src`, with files below folders nobody opened.
fn small_project(root: &Path) {
    write(root, "main.rs", "");
    write(root, "notes.md", "");
    write(root, "src/lib.rs", "");
    write(root, "src/main_util.rs", "");
    write(root, "deep/a/b/main_deep.rs", "");
    write(root, "docs/guide.md", "");
}

#[gpui::test]
fn typing_shows_what_the_tree_has_listed_while_the_daemons_answer_is_held(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, hold) = open_held(cx, &daemon);
    let (_dir, root) = scratch();
    small_project(&root);
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["deep", "docs", "src", "main.rs", "notes.md"]);
    panel.update(cx, |p, cx| p.toggle(&root.join("src"), cx));
    wait_names(
        &panel,
        cx,
        &[
            "deep",
            "docs",
            "src",
            "  lib.rs",
            "  main_util.rs",
            "main.rs",
            "notes.md",
        ],
    );
    let tree_before = names(&panel, cx);
    focus_tree(&panel, cx);

    // Nothing the daemon says reaches the desktop from here on.
    hold.answers(true);
    cx.simulate_keystrokes("m");
    draw(cx);
    assert!(
        in_field(&panel, cx),
        "the first character puts the keyboard in the field"
    );
    let started = std::time::Instant::now();
    cx.simulate_input("ain");
    // One frame later the rows are what the tree has listed that matches: the folder `src`
    // is open (listed) and `deep` is not.
    draw(cx);
    let frame = started.elapsed();
    assert!(
        frame < std::time::Duration::from_secs(1),
        "keys to a frame with rows took {frame:?} while the daemon's answer was held"
    );
    assert_eq!(
        panel.read_with(cx, |p, _| p.filter_text().to_owned()),
        "main"
    );
    assert_eq!(
        names(&panel, cx),
        ["src", "  main_util.rs", "main.rs"],
        "matches among the listed files, with their folders"
    );
    assert_eq!(status(&panel, cx), FilesFilterStatus::Searching);
    assert_eq!(
        panel.read_with(cx, |p, _| p.filter_requests()),
        0,
        "the debounce is not over"
    );

    // The debounce ends and the daemon is asked; its answer is on its way and held.
    cx.executor().advance_clock(FILES_FILTER_DEBOUNCE);
    wait_until(cx, "the daemon's answer to be held", |cx| {
        hold.holds_answer_with("main_deep.rs")
            && panel.read_with(cx, |p, _| p.filter_requests() == 1)
    });
    draw(cx);
    assert_eq!(
        names(&panel, cx),
        ["src", "  main_util.rs", "main.rs"],
        "frames do not wait for the daemon"
    );
    assert_eq!(status(&panel, cx), FilesFilterStatus::Searching);

    // Let go: the answer looks in the folders nobody opened and replaces the instant rows.
    hold.answers(false);
    wait_found(&panel, cx);
    wait_names(
        &panel,
        cx,
        &[
            "deep/a/b",
            "  main_deep.rs",
            "src",
            "  main_util.rs",
            "main.rs",
        ],
    );
    assert_eq!(
        status(&panel, cx),
        FilesFilterStatus::Found {
            shown: 3,
            capped: false,
            index_truncated: false
        }
    );
    // The folders of the projection are made up from the paths: the tree behind is untouched.
    assert_eq!(
        panel.read_with(cx, |p, _| p.tree().unwrap().rows().len()),
        tree_before.len()
    );

    // The matched letters are marked.
    panel.read_with(cx, |p, _| {
        let index = p.rows().iter().position(|r| &*r.name == "main.rs").unwrap();
        let marks: Vec<(usize, usize)> = p
            .filter_marks(index)
            .iter()
            .map(|m| (m.start, m.end))
            .collect();
        assert_eq!(marks, [(0, 4)]);
    });
}

#[gpui::test]
fn the_daemon_is_asked_once_per_burst_and_an_older_answer_is_dropped(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, hold) = open_held(cx, &daemon);
    let (_dir, root) = scratch();
    small_project(&root);
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["deep", "docs", "src", "main.rs", "notes.md"]);
    focus_tree(&panel, cx);

    // Four keys, one request.
    type_text(cx, "main");
    assert_eq!(panel.read_with(cx, |p, _| p.filter_requests()), 0);
    cx.executor().advance_clock(FILES_FILTER_DEBOUNCE);
    wait_found(&panel, cx);
    assert_eq!(panel.read_with(cx, |p, _| p.filter_requests()), 1);
    assert_eq!(panel.read_with(cx, |p, _| p.filter_answers()), 1);

    // A request whose answer is held, then another key: the first answer is dropped, the rows
    // follow the last query.
    hold.answers(true);
    cx.simulate_input("_");
    draw(cx);
    cx.executor().advance_clock(FILES_FILTER_DEBOUNCE);
    wait_until(cx, "the second request", |cx| {
        panel.read_with(cx, |p, _| p.filter_requests() == 2)
    });
    cx.simulate_input("d");
    draw(cx);
    cx.executor().advance_clock(FILES_FILTER_DEBOUNCE);
    wait_until(cx, "the third request", |cx| {
        panel.read_with(cx, |p, _| p.filter_requests() == 3)
    });
    hold.answers(false);
    wait_found(&panel, cx);
    wait_names(&panel, cx, &["deep/a/b", "  main_deep.rs"]);
    assert_eq!(
        panel.read_with(cx, |p, _| p.filter_answers()),
        2,
        "only the first and the last query's answers were taken"
    );
    assert_eq!(
        panel.read_with(cx, |p, _| p.filter_text().to_owned()),
        "main_d"
    );
}

#[gpui::test]
fn escape_brings_the_tree_back_exactly_as_it_was(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    small_project(&root);
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["deep", "docs", "src", "main.rs", "notes.md"]);
    panel.update(cx, |p, cx| p.toggle(&root.join("src"), cx));
    wait_names(
        &panel,
        cx,
        &[
            "deep",
            "docs",
            "src",
            "  lib.rs",
            "  main_util.rs",
            "main.rs",
            "notes.md",
        ],
    );
    let tree_before = names(&panel, cx);
    panel.update(cx, |p, cx| p.select_path(&root.join("src/lib.rs"), cx));
    focus_tree(&panel, cx);

    type_text(cx, "guide");
    wait_found(&panel, cx);
    wait_names(&panel, cx, &["docs", "  guide.md"]);
    assert_eq!(
        panel.read_with(cx, |p, _| p.selected().map(Path::to_owned)),
        None,
        "a row that is not shown is nobody's selection"
    );

    // ↓ moves into the results: the keyboard leaves the field, the first row is selected.
    cx.simulate_keystrokes("down");
    draw(cx);
    assert!(in_tree(&panel, cx) && !in_field(&panel, cx));
    assert_eq!(
        panel.read_with(cx, |p, _| p.selected().map(Path::to_owned)),
        Some(root.join("docs"))
    );
    cx.simulate_keystrokes("down");
    assert_eq!(
        panel.read_with(cx, |p, _| p.selected().map(Path::to_owned)),
        Some(root.join("docs/guide.md"))
    );
    // ↑ from the first row goes back to the field.
    cx.simulate_keystrokes("up up");
    draw(cx);
    assert!(in_field(&panel, cx));

    // Escape: the filter is gone, the tree has the keyboard, and its rows, open folders and
    // selection are what they were.
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(panel.read_with(cx, |p, _| p.filter_text().to_owned()), "");
    assert!(!panel.read_with(cx, |p, _| p.filtering()));
    assert_eq!(names(&panel, cx), tree_before);
    assert!(in_tree(&panel, cx) && !in_field(&panel, cx));
    assert_eq!(
        panel.read_with(cx, |p, _| p.selected().map(Path::to_owned)),
        Some(root.join("src/lib.rs"))
    );
}

#[gpui::test]
fn return_opens_the_first_file_and_a_folder_row_shows_that_folder_in_the_tree(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    small_project(&root);
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["deep", "docs", "src", "main.rs", "notes.md"]);
    focus_tree(&panel, cx);

    type_text(cx, "lib");
    wait_found(&panel, cx);
    wait_names(&panel, cx, &["src", "  lib.rs"]);
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the file to open", |cx| {
        tab_paths(&view, cx).contains(&root.join("src/lib.rs").display().to_string())
    });

    // A folder row of the results ends the filter and opens that folder in the tree.
    focus_tree(&panel, cx);
    panel.update_in(cx, |p, window, cx| p.start_filter(None, window, cx));
    draw(cx);
    cx.simulate_input("guide");
    draw(cx);
    wait_found(&panel, cx);
    wait_names(&panel, cx, &["docs", "  guide.md"]);
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert!(
        !panel.read_with(cx, |p, _| p.filtering()),
        "the filter ended"
    );
    assert!(
        panel.read_with(cx, |p, _| p.tree().unwrap().is_expanded(&root.join("docs"))),
        "the folder is open in the tree"
    );
    assert_eq!(
        panel.read_with(cx, |p, _| p.selected().map(Path::to_owned)),
        Some(root.join("docs"))
    );
}

#[gpui::test]
fn command_f_and_the_command_put_the_keyboard_in_the_field(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    small_project(&root);
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["deep", "docs", "src", "main.rs", "notes.md"]);
    focus_tree(&panel, cx);
    assert!(!in_field(&panel, cx));
    cx.simulate_keystrokes("cmd-f");
    draw(cx);
    assert!(in_field(&panel, cx), "⌘F with the tree focused");
    // Typing narrows; Backspace in the field takes one back; Escape in the field leaves it with
    // the filter cleared and the keyboard in the tree.
    cx.simulate_input("not");
    draw(cx);
    wait_found(&panel, cx);
    wait_names(&panel, cx, &["notes.md"]);
    cx.simulate_keystrokes("backspace");
    draw(cx);
    assert_eq!(panel.read_with(cx, |p, _| p.filter_text().to_owned()), "no");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(in_tree(&panel, cx));
    assert_eq!(panel.read_with(cx, |p, _| p.filter_text().to_owned()), "");
    // Backspace in the tree with a filter on edits it.
    type_text(cx, "no");
    cx.simulate_keystrokes("down");
    draw(cx);
    assert!(in_tree(&panel, cx));
    cx.simulate_keystrokes("backspace");
    draw(cx);
    assert!(in_field(&panel, cx));
    assert_eq!(panel.read_with(cx, |p, _| p.filter_text().to_owned()), "n");

    // The command does the same.
    cx.simulate_keystrokes("escape");
    draw(cx);
    cx.dispatch_action(actions::FilesFilter);
    draw(cx);
    assert!(in_field(&panel, cx));
}

#[gpui::test]
fn highlight_mode_keeps_the_tree_and_marks_the_matches(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    small_project(&root);
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["deep", "docs", "src", "main.rs", "notes.md"]);
    panel.update(cx, |p, cx| p.toggle(&root.join("src"), cx));
    wait_names(
        &panel,
        cx,
        &[
            "deep",
            "docs",
            "src",
            "  lib.rs",
            "  main_util.rs",
            "main.rs",
            "notes.md",
        ],
    );
    panel.update(cx, |p, cx| {
        p.set_option("filter_mode", toml::Value::String("highlight".into()), cx);
    });
    let tree_before = names(&panel, cx);
    focus_tree(&panel, cx);
    type_text(cx, "ma");
    // The tree is as it was: nothing is hidden, nothing is asked.
    assert_eq!(names(&panel, cx), tree_before);
    assert!(panel.read_with(cx, |p, _| p.filtering() && !p.projecting()));
    assert_eq!(panel.read_with(cx, |p, _| p.filter_requests()), 0);
    assert_eq!(status(&panel, cx), FilesFilterStatus::Marked(2));
    panel.read_with(cx, |p, _| {
        let marked: Vec<(String, Vec<(usize, usize)>)> = p
            .rows()
            .iter()
            .enumerate()
            .filter(|(i, _)| !p.filter_marks(*i).is_empty())
            .map(|(i, r)| {
                let spans = p.filter_marks(i).iter().map(|m| (m.start, m.end)).collect();
                (r.name.to_string(), spans)
            })
            .collect();
        assert_eq!(
            marked,
            [
                ("main_util.rs".to_owned(), vec![(0, 2)]),
                ("main.rs".to_owned(), vec![(0, 2)])
            ]
        );
    });
    // ↓ moves to the first match, Escape clears.
    cx.simulate_keystrokes("down");
    draw(cx);
    assert!(in_tree(&panel, cx));
    assert_eq!(
        panel.read_with(cx, |p, _| p.selected().map(Path::to_owned)),
        Some(root.join("src/main_util.rs"))
    );
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!panel.read_with(cx, |p, _| p.filtering()));
    assert_eq!(names(&panel, cx), tree_before);
}

#[gpui::test]
fn the_row_after_a_truncated_listing_filters_that_folder_and_finds_what_the_listing_left_out(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    write(&root, "other/f05050-elsewhere.txt", "");
    // The folder with too many entries is below the root, so its filter is a scoped one.
    let big = root.join("big");
    std::fs::create_dir(&big).unwrap();
    for n in 0..5_100 {
        std::fs::write(big.join(format!("f{n:05}")), "").unwrap_or_default();
    }
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["big", "other"]);
    panel.update(cx, |p, cx| p.toggle(&big, cx));
    wait_until(
        cx,
        "the folder's first 5 000 and the row after them",
        |cx| {
            panel.read_with(cx, |p, _| {
                p.tree().unwrap().is_truncated(&big)
                    && p.rows().iter().any(|r| {
                        r.kind == FileRowKind::More && r.path.parent() == Some(big.as_path())
                    })
                    && p.settled()
            })
        },
    );
    // Return on that row filters the folder.
    panel.update(cx, |p, cx| p.select_path(&big.join("\u{0}more"), cx));
    focus_tree(&panel, cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.filter_scope().map(Path::to_owned)),
        Some(big.clone())
    );
    assert!(in_field(&panel, cx));

    // What the listing left out is found, and only inside that folder.
    cx.simulate_input("f05050");
    draw(cx);
    wait_found(&panel, cx);
    wait_names(&panel, cx, &["f05050"]);
    panel.read_with(cx, |p, _| {
        assert_eq!(p.rows()[0].path, big.join("f05050"));
    });

    // Escape drops the folder as well.
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.filter_scope().map(Path::to_owned)),
        None
    );
    assert!(!panel.read_with(cx, |p, _| p.filtering()));
}

#[gpui::test]
fn filter_in_folder_looks_in_the_selected_folder_only(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    small_project(&root);
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["deep", "docs", "src", "main.rs", "notes.md"]);
    panel.update(cx, |p, cx| p.select_path(&root.join("src"), cx));
    focus_tree(&panel, cx);
    cx.dispatch_action(actions::FilesFilterInFolder);
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.filter_scope().map(Path::to_owned)),
        Some(root.join("src"))
    );
    assert!(in_field(&panel, cx));
    cx.simulate_input("main");
    draw(cx);
    wait_found(&panel, cx);
    // `main.rs` of the root and `main_deep.rs` are not below `src`.
    wait_names(&panel, cx, &["main_util.rs"]);
    panel.read_with(cx, |p, _| {
        assert_eq!(p.rows()[0].path, root.join("src/main_util.rs"));
    });
    // A selected file means its folder.
    cx.simulate_keystrokes("escape");
    draw(cx);
    panel.update(cx, |p, cx| p.select_path(&root.join("docs"), cx));
    panel.update(cx, |p, cx| p.toggle(&root.join("docs"), cx));
    wait_until(cx, "docs listed", |cx| {
        panel.read_with(cx, |p, _| p.tree().unwrap().is_listed(&root.join("docs")))
    });
    panel.update(cx, |p, cx| p.select_path(&root.join("docs/guide.md"), cx));
    cx.dispatch_action(actions::FilesFilterInFolder);
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.filter_scope().map(Path::to_owned)),
        Some(root.join("docs"))
    );
}

#[gpui::test]
fn hidden_names_stay_out_of_the_results_while_the_tree_hides_them(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    write(&root, ".github/ci.yml", "");
    write(&root, "src/ci.rs", "");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &[".github", "src"]);
    focus_tree(&panel, cx);
    type_text(cx, "ci");
    wait_found(&panel, cx);
    wait_names(&panel, cx, &[".github", "  ci.yml", "src", "  ci.rs"]);
    cx.simulate_keystrokes("escape");
    panel.update(cx, |p, cx| {
        p.set_option("show_hidden", toml::Value::Boolean(false), cx);
    });
    wait_names(&panel, cx, &["src"]);
    focus_tree(&panel, cx);
    type_text(cx, "ci");
    wait_found(&panel, cx);
    wait_names(&panel, cx, &["src", "  ci.rs"]);
}

#[gpui::test]
fn a_project_of_a_hundred_thousand_files_answers_with_at_most_two_hundred(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let (_dir, root) = scratch();
    for d in 0..200 {
        let dir = root.join(format!("pkg{d:03}"));
        std::fs::create_dir(&dir).unwrap();
        for f in 0..500 {
            std::fs::write(dir.join(format!("file{f:03}.rs")), "").unwrap();
        }
    }
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_until(cx, "the root listed", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 200 && p.settled())
    });
    focus_tree(&panel, cx);

    // Almost everything matches: the daemon gives 200 and the panel says there are more.
    type_text(cx, "rs");
    cx.executor().advance_clock(FILES_FILTER_DEBOUNCE);
    wait_found(&panel, cx);
    panel.read_with(cx, |p, _| {
        let files = p
            .rows()
            .iter()
            .filter(|r| r.kind == FileRowKind::File)
            .count();
        assert_eq!(files, 200, "at most 200 files are shown");
        assert_eq!(
            p.filter_status(),
            &FilesFilterStatus::Found {
                shown: 200,
                capped: true,
                index_truncated: false
            }
        );
    });

    // Typing a narrower query one key at a time, the index warm: each key is timed from the
    // key to the frame that shows the daemon's answer to it (the debounce is the test clock's).
    let mut took = Vec::new();
    for key in "  pkg137 file042".chars() {
        let started = std::time::Instant::now();
        cx.simulate_input(&key.to_string());
        draw(cx);
        wait_found(&panel, cx);
        took.push(started.elapsed());
    }
    wait_until(cx, "the narrow answer", |cx| {
        names(&panel, cx) == ["pkg137", "  file042.rs"]
            && matches!(
                status(&panel, cx),
                FilesFilterStatus::Found { shown: 1, .. }
            )
    });
    took.sort();
    eprintln!(
        "filter at 100 000 files, key to answer drawn (debug build): median {:?}, worst {:?}",
        took[took.len() / 2],
        took[took.len() - 1]
    );
    // The folders of the projection were never listed: no `fs.list` per folder.
    assert!(!panel.read_with(cx, |p, _| p.tree().unwrap().knows(&root.join("pkg137"))));
}
