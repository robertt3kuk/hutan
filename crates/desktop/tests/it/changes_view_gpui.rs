#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Changes view with a large changeset (FX-CHANGES-VIEW): the file list and the diff scroll
//! on their own to the very last row and line at any window size, with the wheel, the trackpad,
//! the keys and the scrollbar; the code scrolls sideways under a gutter that stays; the tint
//! covers the whole width; a file switch starts at the top left; lines wrap for prose; rows say
//! the name first; the list can be a tree; the divider between list and diff drags. The repository
//! has the shape of the report: nested folders of new Markdown files with long Cyrillic lines and
//! tables, uncommitted.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{
    Entity, Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
    VisualTestContext, point, px, size,
};
use hutan_core::WorkspaceId;
use hutan_desktop::settings::ChangesList;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::panels::ChangesPanel;
use hutan_desktop::workbench::{ChangesView, ListRow, Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

const FILES: usize = 90;

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

/// A sentence of `words` Cyrillic words.
fn sentence(seed: usize, words: usize) -> String {
    const WORDS: [&str; 8] = [
        "транспортные",
        "сбои",
        "не",
        "превращаются",
        "в",
        "бизнес-коды",
        "хелпер",
        "упаковывает",
    ];
    (0..words)
        .map(|i| WORDS[(seed + i * 3) % WORDS.len()])
        .collect::<Vec<_>>()
        .join(" ")
}

/// A Markdown file of 40 lines: sentences of ~230 characters and a table, one blank line between.
fn markdown(seed: usize) -> String {
    let mut text = format!("# Заметка {seed}\n\n");
    for line in 0..20 {
        if line % 7 == 3 {
            text.push_str("| поле | значение | комментарий |\n|---|---|---|\n");
            text.push_str(&format!(
                "| `a_{line}` | `b` | {} |\n\n",
                sentence(seed + line, 30)
            ));
        } else {
            text.push_str(&sentence(seed + line, 30));
            text.push_str("\n\n");
        }
    }
    text
}

struct Fx {
    daemon: TestDaemon,
    _keep: tempfile::TempDir,
    root: PathBuf,
    workspace: WorkspaceId,
}

impl Fx {
    /// A repository with a README committed and [`FILES`] untracked files: Markdown in nested
    /// folders and `src/main.rs` with a 400-character line.
    fn new() -> Self {
        let daemon = TestDaemon::start();
        let keep = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(keep.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git(&root, &["config", key, value]);
        }
        fs::write(root.join("README.md"), "# scratch\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "first"]);
        for n in 0..FILES - 1 {
            let dir = match n % 3 {
                0 => root.join("PRJ-1"),
                1 => root.join("PRJ-1").join("notes"),
                _ => root.join("PRJ-2").join("notes").join("deep"),
            };
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("note-{n:02}.md")), markdown(n)).unwrap();
        }
        fs::create_dir_all(root.join("src")).unwrap();
        let long = "x".repeat(400);
        fs::write(
            root.join("src/main.rs"),
            format!("fn main() {{\n    let s = \"{long}\";\n}}\n"),
        )
        .unwrap();
        let workspace = daemon.create_workspace("proj", &[&root]).id;
        Self {
            daemon,
            _keep: keep,
            root,
            workspace,
        }
    }
}

fn open<'a>(cx: &'a mut TestAppContext, fx: &Fx) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(fx.daemon.home())),
        fx.daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    let workspace = fx.workspace;
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace, window, cx)
    });
    (view, cx)
}

fn open_changes(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    root: &Path,
) -> Entity<ChangesView> {
    view.update_in(cx, |wb, window, cx| {
        wb.open_changes(root.to_owned(), window, cx)
    });
    draw(cx);
    let changes = view
        .read_with(cx, |wb, _| wb.changes(&root.display().to_string()))
        .expect("the Changes view was opened");
    wait_until(cx, "the listing and the first patch", |cx| {
        changes.read_with(cx, |c, _| {
            c.files().len() == FILES && c.selected().is_some() && c.selected_diff().is_some()
        })
    });
    changes
}

fn region(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn at(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    what: Region,
) -> gpui::Bounds<gpui::Pixels> {
    region(view, cx, what).unwrap_or_else(|| panic!("{what:?} is drawn"))
}

fn wheel(cx: &mut VisualTestContext, position: gpui::Point<gpui::Pixels>, dx: f32, dy: f32) {
    cx.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    draw(cx);
}

/// Scrolls `at` to its end the way a user would: more wheel than there is content, in steps.
fn wheel_to_end(cx: &mut VisualTestContext, position: gpui::Point<gpui::Pixels>) {
    for _ in 0..30 {
        wheel(cx, position, 0.0, -2000.0);
    }
}

/// Let the window be `w × h` and the view settle: the list and the diff are drawn.
fn resize(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    changes: &Entity<ChangesView>,
    w: f32,
    h: f32,
) {
    cx.simulate_resize(size(px(w), px(h)));
    wait_until(cx, "the list and the diff at the new size", |cx| {
        region(view, cx, Region::Changes("list")).is_some()
            && region(view, cx, Region::Changes("diff")).is_some()
            && changes.read_with(cx, |c, _| c.selected_diff().is_some())
    });
    draw(cx);
}

#[gpui::test]
fn the_list_and_the_diff_scroll_independently_to_their_last_row_at_every_window_size(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.root);
    // 1280 × 800 with both docks (the center is narrow: the list is a strip over the diff),
    // 1440 × 900, and a full screen.
    for (w, h) in [(1280.0, 800.0), (1440.0, 900.0), (2056.0, 1329.0)] {
        resize(&view, cx, &changes, w, h);
        changes.update(cx, |c, cx| {
            c.select(&c.files()[0].path.clone(), cx);
        });
        draw(cx);
        let label = format!("at {w} × {h}");

        // The list: the wheel over it scrolls it to its last row, and only it.
        let list = at(&view, cx, Region::Changes("list"));
        wheel_to_end(cx, list.center());
        let (down, most) = changes.read_with(cx, |c, _| c.list_scroll_y());
        assert!(
            most > 0.0 && (most - down).abs() < 1.0,
            "the list is at its end {label}: {down}/{most}"
        );
        let last = at(&view, cx, Region::ChangesFile(FILES - 1));
        let list = at(&view, cx, Region::Changes("list"));
        assert!(
            last.bottom() <= list.bottom() + px(1.0) && last.top() >= list.top(),
            "the last row is inside the list {label}: {last:?} in {list:?}"
        );
        assert_eq!(
            changes.read_with(cx, |c, _| (c.diff_scroll_y().0, c.diff_offset())),
            (0.0, 0.0),
            "the diff did not move with the list {label}"
        );

        // The diff: the wheel over it scrolls it down to its last line, and not the list; it
        // does not drift sideways.
        let list_at = changes.read_with(cx, |c, _| c.list_scroll_y().0);
        let diff = at(&view, cx, Region::Changes("diff-view"));
        wheel_to_end(cx, diff.center());
        let (down, most) = changes.read_with(cx, |c, _| c.diff_scroll_y());
        assert!(
            most > 0.0 && (most - down).abs() < 1.0,
            "the diff is at its end {label}: {down}/{most}"
        );
        assert_eq!(
            changes.read_with(cx, |c, _| c.diff_offset()),
            0.0,
            "no drift {label}"
        );
        assert_eq!(
            changes.read_with(cx, |c, _| c.list_scroll_y().0),
            list_at,
            "the list stayed {label}"
        );
        let lines = changes.read_with(cx, |c, _| c.selected_diff().unwrap().lines.len());
        let last = at(&view, cx, Region::ChangesRow(lines - 1));
        let pane = at(&view, cx, Region::Changes("diff-view"));
        assert!(
            last.bottom() <= pane.bottom() + px(1.0),
            "the last diff line is inside the pane {label}: {last:?} in {pane:?}"
        );

        // And back to the top.
        wheel(cx, diff.center(), 0.0, 1.0e6);
        assert_eq!(changes.read_with(cx, |c, _| c.diff_scroll_y().0), 0.0);
    }
}

#[gpui::test]
fn code_scrolls_sideways_under_a_gutter_that_stays_and_the_tint_spans_the_scrolled_width(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.root);
    resize(&view, cx, &changes, 2056.0, 1329.0);
    // Code does not wrap by default (the Rust file); its 400-character line scrolls.
    changes.update(cx, |c, cx| c.select("src/main.rs", cx));
    wait_until(cx, "the patch of main.rs", |cx| {
        changes.read_with(cx, |c, _| {
            c.selected() == Some("src/main.rs") && c.selected_diff().is_some()
        })
    });
    wait_until(cx, "its rows", |cx| {
        region(&view, cx, Region::ChangesRow(2)).is_some()
    });
    assert!(
        !changes.read_with(cx, |c, _| c.wraps("src/main.rs")),
        "code scrolls"
    );
    let max = changes.read_with(cx, |c, _| c.diff_scroll_width());
    assert!(max > 100.0, "the 400-column line can be scrolled to: {max}");

    let diff = at(&view, cx, Region::Changes("diff-view"));
    let gutter_before = at(&view, cx, Region::ChangesLine(2));
    let row_before = at(&view, cx, Region::ChangesRow(2));
    // A trackpad swipe sideways moves the code, and the pane stays where it is vertically.
    wheel(
        cx,
        point(diff.left() + px(300.0), row_before.center().y),
        -300.0,
        0.0,
    );
    let moved = changes.read_with(cx, |c, _| c.diff_offset());
    assert!(moved > 100.0, "the code moved: {moved}");
    assert_eq!(changes.read_with(cx, |c, _| c.diff_scroll_y().0), 0.0);
    let gutter = at(&view, cx, Region::ChangesLine(2));
    assert_eq!(
        gutter.left(),
        gutter_before.left(),
        "the line-number gutter stayed"
    );
    // The row's tint (the row is the widest line's width) moved with the code, and every row has
    // the same width: a short line's tint reaches as far as the long one's.
    let row = at(&view, cx, Region::ChangesRow(2));
    assert!(
        row.left() < row_before.left() - px(100.0),
        "the row scrolled: {row:?}"
    );
    let short = at(&view, cx, Region::ChangesRow(1));
    assert_eq!(short.size.width, row.size.width, "rows have one width");
    assert!(row.right() > diff.right() - px(20.0) || row.size.width > diff.size.width);

    // A file switch starts at the left.
    let other = changes.read_with(cx, |c, _| {
        c.files()
            .iter()
            .find(|f| f.path != "src/main.rs")
            .unwrap()
            .path
            .clone()
    });
    changes.update(cx, |c, cx| c.select(&other, cx));
    draw(cx);
    assert_eq!(
        changes.read_with(cx, |c, _| c.diff_offset()),
        0.0,
        "the offset was reset"
    );

    // The scrollbar along the lines shows while the content is wider, and its thumb drags.
    changes.update(cx, |c, cx| c.select("src/main.rs", cx));
    wait_until(cx, "the horizontal scrollbar", |cx| {
        region(&view, cx, Region::Changes("diff-hbar")).is_some()
    });
    let thumb = at(&view, cx, Region::Changes("diff-hbar"));
    cx.simulate_mouse_down(thumb.center(), MouseButton::Left, Modifiers::none());
    draw(cx);
    cx.simulate_mouse_move(
        point(thumb.center().x + px(60.0), thumb.center().y),
        MouseButton::Left,
        Modifiers::none(),
    );
    draw(cx);
    assert!(
        changes.read_with(cx, |c, _| c.diff_offset()) > 0.0,
        "the thumb scrolled the code"
    );
    cx.simulate_mouse_move(
        point(thumb.center().x + px(5000.0), thumb.center().y),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_up(
        point(thumb.center().x + px(5000.0), thumb.center().y),
        MouseButton::Left,
        Modifiers::none(),
    );
    draw(cx);
    let (offset, max) = changes.read_with(cx, |c, _| (c.diff_offset(), c.diff_scroll_width()));
    assert!(
        (max - offset).abs() < 1.0,
        "dragged to the end: {offset}/{max}"
    );
}

#[gpui::test]
fn lines_wrap_for_prose_by_default_and_the_header_toggle_changes_it(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.root);
    resize(&view, cx, &changes, 2056.0, 1329.0);
    let markdown = changes.read_with(cx, |c, _| c.selected().unwrap().to_owned());
    assert!(
        markdown.ends_with(".md"),
        "the first file is Markdown: {markdown}"
    );
    // Prose wraps: nothing to scroll sideways, whatever the 230-character sentences.
    assert!(changes.read_with(cx, |c, _| c.wraps(&markdown)));
    assert_eq!(changes.read_with(cx, |c, _| c.diff_scroll_width()), 0.0);
    // Code does not.
    assert!(!changes.read_with(cx, |c, _| c.wraps("src/main.rs")));

    // The header's toggle (Wrap lines) turns it off for the view: the sentences scroll.
    wait_until(cx, "the toggle", |cx| {
        region(&view, cx, Region::Changes("wrap")).is_some()
    });
    let toggle = at(&view, cx, Region::Changes("wrap"));
    cx.simulate_click(toggle.center(), Modifiers::none());
    draw(cx);
    assert!(!changes.read_with(cx, |c, _| c.wraps(&markdown)));
    wait_until(cx, "the unwrapped diff to be laid out", |cx| {
        changes.read_with(cx, |c, _| c.diff_scroll_width()) > 100.0
    });
    // And back on.
    let toggle = at(&view, cx, Region::Changes("wrap"));
    cx.simulate_click(toggle.center(), Modifiers::none());
    draw(cx);
    wait_until(cx, "the wrapped diff", |cx| {
        changes.read_with(cx, |c, _| {
            c.wraps(&markdown) && c.diff_scroll_width() == 0.0
        })
    });
}

#[gpui::test]
fn keys_scroll_the_list_and_the_diff_and_arrows_move_the_selection(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.root);
    resize(&view, cx, &changes, 2056.0, 1329.0);

    // A click in the diff gives it the keys: page down, End, Home.
    let diff = at(&view, cx, Region::Changes("diff-view"));
    cx.simulate_click(
        point(diff.center().x, diff.top() + px(40.0)),
        Modifiers::none(),
    );
    draw(cx);
    cx.simulate_keystrokes("pagedown");
    draw(cx);
    let paged = changes.read_with(cx, |c, _| c.diff_scroll_y().0);
    assert!(paged > 100.0, "page down scrolled the diff: {paged}");
    cx.simulate_keystrokes("end");
    draw(cx);
    let (down, most) = changes.read_with(cx, |c, _| c.diff_scroll_y());
    assert!(
        (most - down).abs() < 1.0 && most > 0.0,
        "End: {down}/{most}"
    );
    cx.simulate_keystrokes("home");
    draw(cx);
    assert_eq!(changes.read_with(cx, |c, _| c.diff_scroll_y().0), 0.0);
    assert_eq!(
        changes.read_with(cx, |c, _| c.list_scroll_y().0),
        0.0,
        "the list did not move"
    );

    // A click on a row gives the list the keys: End scrolls it to the last row, the arrows move
    // the selection (and the diff starts at the top again).
    let first = at(&view, cx, Region::ChangesFile(1));
    cx.simulate_click(first.center(), Modifiers::none());
    draw(cx);
    let selected = changes.read_with(cx, |c, _| c.selected().unwrap().to_owned());
    cx.simulate_keystrokes("down");
    draw(cx);
    assert_ne!(
        changes.read_with(cx, |c, _| c.selected().unwrap().to_owned()),
        selected
    );
    cx.simulate_keystrokes("end");
    draw(cx);
    let (down, most) = changes.read_with(cx, |c, _| c.list_scroll_y());
    assert!(
        most > 0.0 && (most - down).abs() < 1.0,
        "End scrolled the list: {down}/{most}"
    );
    assert_eq!(
        changes.read_with(cx, |c, _| c.diff_scroll_y().0),
        0.0,
        "not the diff"
    );
}

#[gpui::test]
fn the_list_scrollbar_drags_to_the_last_row(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.root);
    resize(&view, cx, &changes, 1440.0, 900.0);
    wait_until(cx, "the list scrollbar", |cx| {
        region(&view, cx, Region::Changes("files-vbar")).is_some()
    });
    let thumb = at(&view, cx, Region::Changes("files-vbar"));
    cx.simulate_mouse_down(thumb.center(), MouseButton::Left, Modifiers::none());
    draw(cx);
    let far = point(thumb.center().x, thumb.center().y + px(5000.0));
    cx.simulate_mouse_move(far, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(far, MouseButton::Left, Modifiers::none());
    draw(cx);
    let (down, most) = changes.read_with(cx, |c, _| c.list_scroll_y());
    assert!(
        most > 0.0 && (most - down).abs() < 1.0,
        "the thumb reached the end: {down}/{most}"
    );
    let last = at(&view, cx, Region::ChangesFile(FILES - 1));
    let list = at(&view, cx, Region::Changes("list"));
    assert!(last.bottom() <= list.bottom() + px(1.0));
}

#[gpui::test]
fn the_divider_drags_and_the_columns_refit_when_the_window_shrinks(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.root);
    cx.dispatch_action(actions::ToggleRightDock);
    resize(&view, cx, &changes, 1800.0, 900.0);
    wait_until(cx, "the divider", |cx| {
        region(&view, cx, Region::Changes("split")).is_some()
    });
    let before = at(&view, cx, Region::Changes("list")).size.width;
    let handle = at(&view, cx, Region::Changes("split"));
    cx.simulate_mouse_down(handle.center(), MouseButton::Left, Modifiers::none());
    draw(cx);
    let to = point(handle.center().x - px(90.0), handle.center().y);
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    draw(cx);
    let after = at(&view, cx, Region::Changes("list")).size.width;
    assert!(
        (before - after - px(90.0)).abs() < px(2.0),
        "the list is 90 px narrower: {before:?} → {after:?}"
    );
    // The diff takes what the list gave back and nothing sticks out of the view.
    let root = at(&view, cx, Region::Changes("root"));
    let diff = at(&view, cx, Region::Changes("diff-view"));
    assert!(
        diff.right() <= root.right() + px(1.0),
        "{diff:?} in {root:?}"
    );

    // A double click on the divider gives the default width back.
    let handle = at(&view, cx, Region::Changes("split"));
    cx.simulate_click(handle.center(), Modifiers::none());
    cx.simulate_click(handle.center(), Modifiers::none());
    draw(cx);

    // Wider than the window can hold: the list takes what leaves the diff its minimum.
    let handle = at(&view, cx, Region::Changes("split"));
    cx.simulate_mouse_down(handle.center(), MouseButton::Left, Modifiers::none());
    let far = point(handle.center().x + px(5000.0), handle.center().y);
    cx.simulate_mouse_move(far, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(far, MouseButton::Left, Modifiers::none());
    draw(cx);
    let diff = at(&view, cx, Region::Changes("diff-view"));
    assert!(
        diff.size.width >= px(250.0),
        "the diff keeps room: {diff:?}"
    );
    resize(&view, cx, &changes, 1100.0, 700.0);
    let root = at(&view, cx, Region::Changes("root"));
    let list = at(&view, cx, Region::Changes("list"));
    let diff = at(&view, cx, Region::Changes("diff-view"));
    assert!(
        list.size.width + diff.size.width <= root.size.width
            && diff.right() <= root.right() + px(1.0),
        "everything fits after the window shrank: {list:?} {diff:?} in {root:?}"
    );
}

#[gpui::test]
fn the_list_can_be_a_tree_with_folders_that_fold_and_count(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.root);
    resize(&view, cx, &changes, 2056.0, 1329.0);
    // Flat by default: one file row per file.
    assert_eq!(
        changes.read_with(cx, |c, _| c.list_mode()),
        ChangesList::Flat
    );
    assert_eq!(changes.read_with(cx, |c, _| c.list_rows().len()), FILES);

    wait_until(cx, "the toggle", |cx| {
        region(&view, cx, Region::Changes("list-mode")).is_some()
    });
    let toggle = at(&view, cx, Region::Changes("list-mode"));
    cx.simulate_click(toggle.center(), Modifiers::none());
    draw(cx);
    assert_eq!(
        changes.read_with(cx, |c, _| c.list_mode()),
        ChangesList::Tree
    );
    let rows = changes.read_with(cx, |c, _| c.list_rows());
    let dirs: Vec<(String, usize)> = rows
        .iter()
        .filter_map(|r| match r {
            ListRow::Dir {
                name,
                files,
                depth: 0,
                ..
            } => Some((name.clone(), *files)),
            _ => None,
        })
        .collect();
    assert_eq!(
        dirs.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["PRJ-1", "PRJ-2/notes/deep", "src"],
        "top-level folders; a chain of single folders is one row"
    );
    let total: usize = dirs.iter().map(|(_, n)| n).sum();
    assert_eq!(total, FILES);

    // A folded folder hides its files: their rows are gone and the list is shorter.
    let before = rows.len();
    changes.update(cx, |c, cx| c.toggle_folder("PRJ-1", cx));
    draw(cx);
    let after = changes.read_with(cx, |c, _| c.list_rows().len());
    assert!(after < before - 10, "{before} → {after}");
    assert_eq!(changes.read_with(cx, |c, _| c.folded_folders()), ["PRJ-1"]);
    // And the listing is flat again with the toggle.
    let toggle = at(&view, cx, Region::Changes("list-mode"));
    cx.simulate_click(toggle.center(), Modifiers::none());
    draw(cx);
    assert_eq!(changes.read_with(cx, |c, _| c.list_rows().len()), FILES);
}

#[gpui::test]
fn the_changes_panel_in_the_right_dock_scrolls_to_its_last_row_at_every_size(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    cx.dispatch_action(actions::OpenPanel("changes".into()));
    draw(cx);
    let panel: Entity<ChangesPanel> = view
        .read_with(cx, |wb, _| wb.panel::<ChangesPanel>("changes"))
        .expect("the Changes panel is drawn");
    wait_until(cx, "the checkout and its files", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 1)
            && region(&view, cx, Region::Changes("panel-more")).is_some()
    });
    for (w, h) in [(1280.0, 800.0), (1440.0, 900.0), (2056.0, 1329.0)] {
        cx.simulate_resize(size(px(w), px(h)));
        draw(cx);
        let list = at(&view, cx, Region::Changes("panel-list"));
        if h < 1000.0 {
            // Forty rows do not fit: the scrollbar says so.
            assert!(
                region(&view, cx, Region::Changes("panel-vbar")).is_some(),
                "a scrollbar at {w} × {h}"
            );
        }
        wheel_to_end(cx, list.center());
        // The "N more files…" row, the last of the list, is inside the list at the end.
        let more = at(&view, cx, Region::Changes("panel-more"));
        let list = at(&view, cx, Region::Changes("panel-list"));
        assert!(
            more.bottom() <= list.bottom() + px(1.0) && more.top() >= list.top(),
            "the last row is reachable at {w} × {h}: {more:?} in {list:?}"
        );
    }
}

// Show in Files -------------------------------------------------------------------------------

/// A repository with a worktree `feature/x` that changed `notes/a.md`; the main checkout changed
/// `notes/b.md` (uncommitted).
struct Pair {
    daemon: TestDaemon,
    _keep: tempfile::TempDir,
    main: PathBuf,
    wt: PathBuf,
    workspace: WorkspaceId,
}

impl Pair {
    fn new() -> Self {
        use hutan_protocol::messages::Request;
        use hutan_protocol::worktree::{WorktreeCreate, WorktreeCreated};
        let daemon = TestDaemon::start();
        let keep = tempfile::tempdir().unwrap();
        let main = fs::canonicalize(keep.path()).unwrap().join("repo");
        fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git(&main, &["config", key, value]);
        }
        fs::create_dir(main.join("notes")).unwrap();
        fs::write(main.join("notes/a.md"), "one\n").unwrap();
        fs::write(main.join("notes/b.md"), "one\n").unwrap();
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-q", "-m", "first"]);
        let workspace = daemon.create_workspace("proj", &[&main]).id;
        let created: WorktreeCreated = daemon.call(Request::WorktreeCreate(WorktreeCreate {
            workspace_id: workspace,
            repo: None,
            branch: "feature/x".into(),
            base: None,
            path: None,
            no_setup: false,
        }));
        let wt = created.worktree.path;
        // The daemon lists the folders of the checkouts its sessions work in (and the
        // workspace's roots): a terminal in the worktree makes it one.
        let _: hutan_protocol::messages::SessionResult = daemon.call(Request::SessionCreate(
            hutan_protocol::messages::SessionCreate {
                workspace_id: Some(workspace),
                spec: hutan_protocol::messages::SessionSpec::Terminal(
                    hutan_protocol::messages::TerminalSpec {
                        command: vec!["cat".into()],
                        cwd: Some(wt.clone()),
                        ..Default::default()
                    },
                ),
            },
        ));
        fs::write(wt.join("notes/a.md"), "one\ntwo\n").unwrap();
        fs::write(main.join("notes/b.md"), "one\nthree\n").unwrap();
        Self {
            daemon,
            _keep: keep,
            main,
            wt,
            workspace,
        }
    }
}

#[gpui::test]
fn show_in_files_scopes_the_files_panel_to_the_checkout_of_the_view_and_selects_the_file(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::workbench::panels::FilesPanel;
    let fx = Pair::new();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(fx.daemon.home())),
        fx.daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    let workspace = fx.workspace;
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace, window, cx)
    });
    let files_root = |view: &Entity<Workbench>, cx: &VisualTestContext| -> Option<PathBuf> {
        view.read_with(cx, |wb, cx| {
            wb.panel::<FilesPanel>("files")
                .and_then(|p| p.read(cx).root().map(Path::to_owned))
        })
    };
    let open_view = |view: &Entity<Workbench>, cx: &mut VisualTestContext, path: &Path| {
        view.update_in(cx, |wb, window, cx| {
            wb.open_changes(path.to_owned(), window, cx)
        });
        draw(cx);
        let changes = view
            .read_with(cx, |wb, _| wb.changes(&path.display().to_string()))
            .expect("the view was opened");
        wait_until(cx, "its patch", |cx| {
            changes.read_with(cx, |c, _| {
                c.selected().is_some() && c.selected_diff().is_some()
            })
        });
        changes
    };

    // The worktree's view: its header's Show in Files puts the Files panel in the worktree with
    // the file selected, and the sidebar's scope picks the worktree.
    let changes = open_view(&view, cx, &fx.wt);
    wait_until(cx, "the button", |cx| {
        region(&view, cx, Region::Changes("show-in-files")).is_some()
    });
    let button = at(&view, cx, Region::Changes("show-in-files"));
    cx.simulate_click(button.center(), Modifiers::none());
    wait_until(cx, "Files in the worktree", |cx| {
        files_root(&view, cx).as_deref() == Some(fx.wt.as_path())
    });
    let selected = changes.read_with(cx, |c, _| c.selected().unwrap().to_owned());
    assert_eq!(selected, "notes/a.md");
    wait_until(cx, "the file selected in the tree", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.panel::<FilesPanel>("files")
                .and_then(|p| p.read(cx).selected().map(Path::to_owned))
        }) == Some(fx.wt.join("notes/a.md"))
    });
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).active_checkout()),
        Some(fx.wt.clone()),
        "the sidebar's scope picked the worktree"
    );

    // A file row's own button does the same for another file, here in the main checkout's view,
    // which brings Files back to the main checkout.
    let main_changes = open_view(&view, cx, &fx.main);
    wait_until(cx, "Files follows the tab to the main checkout", |cx| {
        files_root(&view, cx).as_deref() == Some(fx.main.as_path())
    });
    wait_until(cx, "the row button", |cx| {
        region(&view, cx, Region::ChangesShowInFiles(0)).is_some()
    });
    let row = at(&view, cx, Region::ChangesShowInFiles(0));
    cx.simulate_click(row.center(), Modifiers::none());
    wait_until(cx, "b.md selected in the main checkout's tree", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.panel::<FilesPanel>("files")
                .and_then(|p| p.read(cx).selected().map(Path::to_owned))
        }) == Some(fx.main.join("notes/b.md"))
    });
    assert_eq!(
        main_changes.read_with(cx, |c, _| c.selected().map(str::to_owned)),
        Some("notes/b.md".to_owned())
    );

    // Back to the worktree's tab: Files follows the checkout the view shows.
    view.update_in(cx, |wb, window, cx| {
        wb.open_changes(fx.wt.clone(), window, cx)
    });
    wait_until(cx, "Files follows the tab back to the worktree", |cx| {
        files_root(&view, cx).as_deref() == Some(fx.wt.as_path())
    });
}
