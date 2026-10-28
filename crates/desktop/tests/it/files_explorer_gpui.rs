#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! EX-1: the Files panel as an explorer — natural order, the dotfiles toggle and the header's ⋯,
//! folded single-folder chains, collapse all and refresh, the keys (Home / End / PgUp / PgDn,
//! ⌘← / ⌘→, Esc and ⌘⇧E hand the keyboard back, ⌘W closes the tab), the scrollbar, indent
//! guides, the row after a truncated listing, a burst of `fs.changed` listing a folder at most
//! four times a second, the git read throttle, the tree kept per root and the open folders
//! remembered across restarts. Real daemon, real sockets; the timers are the test clock's.

use crate::common;

use std::path::{Path, PathBuf};

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px};
use hutan_client::Notice;
use hutan_desktop::workbench::panels::{
    FILES_GIT_READ_EVERY, FILES_MORE_ROW, FILES_RELIST_EVERY, FileRowKind, FilesPanel,
};
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_protocol::fs::{ChangeKind, FsChange};
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
    state: Option<PathBuf>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, settings(daemon), daemon.connection(), state);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
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

fn set(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext, key: &str, value: toml::Value) {
    panel.update(cx, |p, cx| p.set_option(key, value, cx));
    draw(cx);
}

fn focused(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, app| panel.read(app).has_focus(window, app))
}

fn focus_tree(panel: &Entity<FilesPanel>, cx: &mut VisualTestContext) {
    panel.update_in(cx, |p, window, cx| p.focus_tree(window, cx));
    draw(cx);
}

#[gpui::test]
fn names_sort_naturally_and_dotfiles_hide_with_the_header_menu(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    let (_dir, root) = scratch();
    for file in [
        "file10.txt",
        "file2.txt",
        "file1.txt",
        ".hidden",
        "Zeta.txt",
    ] {
        write(&root, file, "");
    }
    std::fs::create_dir(root.join("b10")).unwrap();
    std::fs::create_dir(root.join("b9")).unwrap();
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    // Folders first, each group in natural order; the dotfile sorts before the letters.
    wait_names(
        &panel,
        cx,
        &[
            "b9",
            "b10",
            ".hidden",
            "file1.txt",
            "file2.txt",
            "file10.txt",
            "Zeta.txt",
        ],
    );

    // The header's ⋯ lists the toggles, the shown ones checked; choosing one writes the setting.
    cx.dispatch_action(actions::ShowFilesMoreMenu(point(px(10.0), px(10.0))));
    let entries = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the menu is open");
    let hidden = entries
        .iter()
        .position(|(label, _)| label.as_ref() == "Show Hidden Files")
        .expect("Show Hidden Files");
    assert!(entries[hidden].1, "dotfiles are shown by default");
    let ignored = entries
        .iter()
        .find(|(label, _)| label.as_ref() == "Show Ignored Files")
        .expect("Show Ignored Files");
    assert!(!ignored.1);
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(hidden, window, cx));
    draw(cx);
    wait_names(
        &panel,
        cx,
        &[
            "b9",
            "b10",
            "file1.txt",
            "file2.txt",
            "file10.txt",
            "Zeta.txt",
        ],
    );
    set(&panel, cx, "show_hidden", toml::Value::Boolean(true));
    assert!(names(&panel, cx).contains(&".hidden".to_owned()));
}

#[gpui::test]
fn a_folder_that_holds_one_folder_shares_its_row_and_opens_as_one(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    let (_dir, root) = scratch();
    write(&root, "src/a/b/deep.rs", "");
    write(&root, "src/a/b/other.rs", "");
    write(&root, "README.md", "");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["src", "README.md"]);

    panel.update(cx, |p, cx| p.toggle(&root.join("src"), cx));
    // `a` and `b` are opened by folding and listed, then the chain shows as one row.
    wait_names(
        &panel,
        cx,
        &["src/a/b", "  deep.rs", "  other.rs", "README.md"],
    );
    panel.read_with(cx, |p, _| {
        let row = &p.rows()[0];
        assert_eq!(row.path, root.join("src"));
        assert_eq!(row.folder(), root.join("src/a/b"));
        assert!(row.expanded && row.kind == FileRowKind::Dir);
        assert_eq!(
            p.tree().unwrap().index_of(&root.join("src/a")),
            Some(0),
            "a folder inside the chain is on the chain's row"
        );
    });

    // One click closes the whole chain, one opens it again, folded.
    panel.update(cx, |p, cx| p.toggle(&root.join("src"), cx));
    assert_eq!(names(&panel, cx), ["src", "README.md"]);
    panel.update(cx, |p, cx| p.toggle(&root.join("src"), cx));
    wait_names(
        &panel,
        cx,
        &["src/a/b", "  deep.rs", "  other.rs", "README.md"],
    );

    // Folding off shows every level.
    set(&panel, cx, "compact_folders", toml::Value::Boolean(false));
    wait_names(
        &panel,
        cx,
        &[
            "src",
            "  a",
            "    b",
            "      deep.rs",
            "      other.rs",
            "README.md",
        ],
    );
}

#[gpui::test]
fn collapse_all_closes_every_folder_and_refresh_lists_again(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    let (_dir, root) = scratch();
    write(&root, "a/one.txt", "");
    write(&root, "a/two.txt", "");
    write(&root, "b/three.txt", "");
    write(&root, "b/four.txt", "");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["a", "b"]);
    for dir in ["a", "b"] {
        panel.update(cx, |p, cx| p.toggle(&root.join(dir), cx));
    }
    wait_until(cx, "both folders listed", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 6 && p.settled())
    });
    let watched = panel.read_with(cx, |p, _| p.watched_dirs());
    assert_eq!(watched, 3, "the root and the two open folders");

    cx.dispatch_action(actions::FilesCollapseAll);
    assert_eq!(names(&panel, cx), ["a", "b"]);
    assert_eq!(
        panel.read_with(cx, |p, _| p.watched_dirs()),
        1,
        "closed folders are not watched"
    );

    // Refresh lists the open folders again and reads the git marks.
    panel.update(cx, |p, cx| p.toggle(&root.join("a"), cx));
    wait_until(cx, "a listed", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 4 && p.settled())
    });
    let (issued, reads) = panel.read_with(cx, |p, _| (p.lists_issued(), p.git_reads()));
    cx.dispatch_action(actions::FilesRefresh);
    assert_eq!(
        panel.read_with(cx, |p, _| p.lists_issued()),
        issued + 2,
        "the root and the open folder"
    );
    wait_until(cx, "the refresh answered", |cx| {
        panel.read_with(cx, |p, _| p.settled() && p.git_reads() > reads)
    });
}

#[gpui::test]
fn the_keys_jump_open_and_close_everything_and_hand_the_keyboard_back(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    let (_dir, root) = scratch();
    for n in 0..60 {
        write(&root, &format!("f{n:02}.txt"), "");
    }
    write(&root, "d1/x/leaf1.txt", "");
    write(&root, "d1/y.txt", "");
    write(&root, "d2/z.txt", "");
    let open_tab = write(&root, "notes.md", "# notes\n");
    let second_tab = write(&root, "second.md", "# second\n");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_until(cx, "the root listed", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 64 && p.settled())
    });
    focus_tree(&panel, cx);
    let selected_index = |cx: &VisualTestContext| {
        panel.read_with(cx, |p, _| {
            let selected = p.selected().expect("a selection").to_owned();
            p.rows().iter().position(|r| r.path == selected)
        })
    };

    cx.simulate_keystrokes("end");
    assert_eq!(selected_index(cx), Some(63));
    cx.simulate_keystrokes("home");
    assert_eq!(selected_index(cx), Some(0));
    draw(cx);
    cx.simulate_keystrokes("pagedown");
    let page = selected_index(cx).expect("selected");
    assert!(
        (2..63).contains(&page),
        "PgDn moves by a page of the rows in view, not to the end: {page}"
    );
    cx.simulate_keystrokes("pageup");
    assert_eq!(selected_index(cx), Some(0));

    // ⌘→ opens every folder in view, ⌘← closes every folder.
    cx.simulate_keystrokes("cmd-right");
    wait_until(cx, "the folders open and listed", |cx| {
        panel.read_with(cx, |p, _| {
            // d1 (x, y.txt) and d2 (z.txt) are open and listed; x is one folder deeper.
            p.settled() && p.rows().len() == 67
        })
    });
    cx.simulate_keystrokes("cmd-left");
    assert_eq!(panel.read_with(cx, |p, _| p.rows().len()), 64);
    assert!(
        panel.read_with(cx, |p, _| p.rows()[0].kind == FileRowKind::Dir
            && !p.rows()[0].expanded)
    );

    // Esc hands the keyboard back to the editor; the panel stays where it is.
    assert!(focused(&panel, cx));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!focused(&panel, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.right.active.clone()),
        Some("files".into())
    );

    // ⌘⇧E: into the tree, and from the tree back out.
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    wait_until(cx, "the keyboard in the tree", |cx| focused(&panel, cx));
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    assert!(!focused(&panel, cx));

    // ⌘W with the keyboard in the tree closes the focused tab, never the panel.
    for file in [&open_tab, &second_tab] {
        view.update_in(cx, |wb, window, cx| {
            wb.open_file(hutan_desktop::workbench::FileOpen::picked(file), window, cx);
        });
    }
    let tabs = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| {
            wb.state()
                .panes()
                .iter()
                .map(|pane| pane.items.len())
                .sum::<usize>()
        })
    };
    wait_until(cx, "both tabs", |cx| tabs(cx) >= 2);
    let before = tabs(cx);
    focus_tree(&panel, cx);
    cx.simulate_keystrokes("cmd-w");
    wait_until(cx, "a tab closed", |cx| tabs(cx) == before - 1);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.right.active.clone()),
        Some("files".into()),
        "the panel was not closed"
    );
    assert!(focused(&panel, cx), "the keyboard stays in the tree");
}

#[gpui::test]
fn a_truncated_listing_says_how_many_it_shows(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    let (_dir, root) = scratch();
    for n in 0..5_001 {
        std::fs::write(root.join(format!("f{n:05}")), "").unwrap_or_default();
    }
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_until(cx, "the first 5 000 entries and the row after them", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 5_001 && p.settled())
    });
    panel.read_with(cx, |p, _| {
        let last = p.rows().last().unwrap();
        assert_eq!(last.kind, FileRowKind::More);
        assert_eq!(&*last.name, FILES_MORE_ROW);
        assert_eq!(
            &*last.name,
            "Showing the first 5 000 — filter in this folder"
        );
    });
}

#[gpui::test]
fn the_tree_has_a_scrollbar_while_the_rows_do_not_fit_and_indent_guides_follow_the_setting(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    let (_dir, root) = scratch();
    for n in 0..120 {
        write(&root, &format!("f{n:03}.txt"), "");
    }
    write(&root, "dir/one.txt", "");
    write(&root, "dir/two.txt", "");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_until(cx, "the root listed", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 121 && p.settled())
    });
    draw(cx);
    draw(cx);
    assert!(
        panel.read_with(cx, |p, _| p.scrollbar_thumb()).is_some(),
        "121 rows do not fit the body"
    );

    // Indent guides: on hover by default, so none until the pointer is over the tree.
    panel.update(cx, |p, cx| p.toggle(&root.join("dir"), cx));
    wait_until(cx, "dir listed", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 123 && p.settled())
    });
    panel.update(cx, |p, cx| p.reveal(&root.join("dir/two.txt"), cx));
    draw(cx);
    let drawn = |cx: &mut VisualTestContext| {
        let before = panel.read_with(cx, |p, _| p.guides_drawn());
        panel.update(cx, |_, cx| cx.notify());
        draw(cx);
        panel.read_with(cx, |p, _| p.guides_drawn()) - before
    };
    assert!(!panel.read_with(cx, |p, _| p.guides_shown()));
    assert_eq!(drawn(cx), 0, "no guides while the pointer is elsewhere");
    let tree = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Files("tree")))
        .expect("the tree was drawn");
    cx.simulate_mouse_move(
        point(tree.origin.x + px(40.0), tree.origin.y + px(40.0)),
        None,
        Modifiers::none(),
    );
    draw(cx);
    assert!(panel.read_with(cx, |p, _| p.guides_shown()), "hovered");
    assert!(drawn(cx) > 0, "a line for each level above the rows of dir");
    cx.simulate_mouse_move(point(px(2.0), px(2.0)), None, Modifiers::none());
    draw(cx);
    assert!(!panel.read_with(cx, |p, _| p.guides_shown()));

    set(
        &panel,
        cx,
        "indent_guides",
        toml::Value::String("always".into()),
    );
    assert!(panel.read_with(cx, |p, _| p.guides_shown()));
    assert!(drawn(cx) > 0);
    set(
        &panel,
        cx,
        "indent_guides",
        toml::Value::String("off".into()),
    );
    assert!(!panel.read_with(cx, |p, _| p.guides_shown()));
    assert_eq!(drawn(cx), 0);
}

fn changed(path: PathBuf) -> Notice {
    Notice::FsChanged {
        changes: vec![FsChange {
            path,
            kind: ChangeKind::Modified,
            from: None,
            version: None,
            origin: None,
        }],
        overflow: false,
    }
}

#[gpui::test]
fn a_burst_of_changes_lists_a_folder_at_most_four_times_a_second(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    let (_dir, root) = scratch();
    write(&root, "busy/x.txt", "");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["busy"]);
    panel.update(cx, |p, cx| p.toggle(&root.join("busy"), cx));
    wait_until(cx, "busy listed", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 2 && p.settled())
    });
    let busy = root.join("busy");

    // An agent writes a thousand times: the first change lists the folder at once, the other 999
    // wait for the quarter second to be over.
    let before = panel.read_with(cx, |p, _| p.lists_issued());
    panel.update(cx, |p, cx| {
        for n in 0..1_000 {
            p.on_notice(changed(busy.join(format!("w{n}"))), cx);
        }
        assert_eq!(p.lists_issued(), before + 1, "one listing for the burst");
    });
    cx.executor().advance_clock(FILES_RELIST_EVERY);
    cx.run_until_parked();
    assert!(
        panel.read_with(cx, |p, _| p.lists_issued()) >= before + 2,
        "the changes that came meanwhile are listed once the time is up"
    );
    // A change after a quiet quarter second is listed at once again.
    wait_until(cx, "the listings answered", |cx| {
        panel.read_with(cx, |p, _| p.settled())
    });
    cx.executor().advance_clock(FILES_RELIST_EVERY);
    cx.run_until_parked();
    let quiet = panel.read_with(cx, |p, _| p.lists_issued());
    panel.update(cx, |p, cx| {
        p.on_notice(changed(busy.join("later")), cx);
        assert_eq!(p.lists_issued(), quiet + 1);
    });
}

#[gpui::test]
fn the_git_marks_are_read_at_most_once_in_two_seconds_and_never_overlapping(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    let (_dir, root) = scratch();
    write(&root, "a.txt", "");
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_until(cx, "the first read", |cx| {
        panel.read_with(cx, |p, _| p.git_reads() >= 1 && p.git_settled())
    });
    let reads = panel.read_with(cx, |p, _| p.git_reads());
    // A thousand changes inside the two seconds: no read starts.
    panel.update(cx, |p, cx| {
        for n in 0..1_000 {
            p.on_notice(changed(root.join(format!("w{n}"))), cx);
        }
    });
    draw(cx);
    assert_eq!(panel.read_with(cx, |p, _| p.git_reads()), reads);
    assert!(
        !panel.read_with(cx, |p, _| p.git_settled()),
        "one read waits"
    );
    // The two seconds are up: exactly one read.
    cx.executor().advance_clock(FILES_GIT_READ_EVERY);
    wait_until(cx, "the read the burst asked for", |cx| {
        panel.read_with(cx, |p, _| p.git_reads() == reads + 1 && p.git_settled())
    });
    cx.executor().advance_clock(FILES_GIT_READ_EVERY);
    cx.run_until_parked();
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.git_reads()),
        reads + 1,
        "nothing more was asked for"
    );
}

#[gpui::test]
fn a_root_the_window_comes_back_to_has_its_tree_and_the_open_folders_are_saved(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let state = daemon.home().join("desktop-state.json");
    let (view, cx) = open(cx, &daemon, Some(state.clone()));
    let (_dir_a, a) = scratch();
    let (_dir_b, b) = scratch();
    write(&a, "pkg/one.txt", "");
    write(&a, "pkg/two.txt", "");
    write(&a, "top.txt", "");
    write(&b, "other.txt", "");
    project(&view, cx, &a);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["pkg", "top.txt"]);
    panel.update(cx, |p, cx| p.toggle(&a.join("pkg"), cx));
    wait_names(&panel, cx, &["pkg", "  one.txt", "  two.txt", "top.txt"]);
    // The open folders are saved with the layout.
    cx.executor()
        .advance_clock(hutan_desktop::workbench::SAVE_DEBOUNCE);
    view.update(cx, |wb, _| wb.flush_save());
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    assert_eq!(
        saved["files_expanded"],
        serde_json::json!([{ "root": a.display().to_string(), "dirs": [a.join("pkg").display().to_string()] }])
    );

    // Another root, and back: the first one's tree is there again at once.
    project(&view, cx, &b);
    wait_names(&panel, cx, &["other.txt"]);
    // (The panel is on screen from the first frame, so the home's default folder may be cached
    // too.)
    assert!(panel.read_with(cx, |p, _| p.cached_roots()).contains(&a));
    project(&view, cx, &a);
    panel.read_with(cx, |p, _| {
        let tree = p.tree().unwrap();
        assert_eq!(tree.root(), a);
        assert!(tree.is_expanded(&a.join("pkg")), "pkg is still open");
        assert!(
            !p.rows().is_empty(),
            "drawn before anything is listed again"
        );
    });
    wait_names(&panel, cx, &["pkg", "  one.txt", "  two.txt", "top.txt"]);
    assert!(panel.read_with(cx, |p, _| p.cached_roots()).contains(&b));
}

#[gpui::test]
fn the_open_folders_of_the_last_run_come_back_and_one_that_is_gone_closes_quietly(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (_dir, root) = scratch();
    write(&root, "src/a.rs", "");
    write(&root, "src/deep/b.rs", "");
    write(&root, "docs/c.md", "");
    let state = daemon.home().join("desktop-state.json");
    std::fs::write(
        &state,
        serde_json::json!({
            "files_expanded": [{
                "root": root.display().to_string(),
                "dirs": [
                    root.join("src").display().to_string(),
                    root.join("src/deep").display().to_string(),
                    root.join("vanished").display().to_string(),
                ],
            }],
        })
        .to_string(),
    )
    .unwrap();
    let (view, cx) = open(cx, &daemon, Some(state));
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    wait_names(&panel, cx, &["docs", "src", "  deep", "    b.rs", "  a.rs"]);
    wait_until(cx, "every listing answered", |cx| {
        panel.read_with(cx, |p, _| p.settled())
    });
    panel.read_with(cx, |p, _| {
        let tree = p.tree().unwrap();
        assert!(!tree.is_expanded(&root.join("vanished")));
        assert!(
            !p.rows().iter().any(|r| r.kind == FileRowKind::Failed),
            "no error row for a folder that is gone"
        );
    });
}
