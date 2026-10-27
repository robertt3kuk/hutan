#![allow(clippy::expect_used, clippy::unwrap_used)] // tests
//! FX-WORK-1, the workability fixes: the palette scrolls and fills the window and nothing under
//! an overlay hears the wheel, a scrolled list inside a sheet does not scroll the sheet, ⌘P takes
//! words, the shared-checkout warning counts only working agents, chat answers keep their line
//! breaks, ⌘↑/⌘↓ scroll a terminal, and the project replace clears its notice with the query.

use crate::common;

use std::path::{Path, PathBuf};

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, Pixels, TestAppContext, VisualTestContext, point, px};
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

/// A wheel notch of `dy` pixels (negative scrolls the content up, towards its end) at `at`.
fn wheel(cx: &mut VisualTestContext, at: gpui::Point<Pixels>, dy: f32) {
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(dy))),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    draw(cx);
}

fn region(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> gpui::Bounds<Pixels> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} is not drawn"))
}

// ---- 1. F-08: the palette ----------------------------------------------------------------------

#[gpui::test]
fn the_wheel_scrolls_the_palette_not_the_editor_under_it_and_the_palette_fills_the_window(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let text: String = (1..=400).map(|n| format!("line {n}\n")).collect();
    let file = write(&root, "long.txt", &text);
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&file), window, cx);
    });
    let wanted = file.display().to_string();
    wait_until(cx, "the file loaded", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view().is_some_and(|v| {
                v.read(cx).buffer().read(cx).path().display().to_string() == wanted
                    && *v.read(cx).buffer().read(cx).state() != hutan_code_view::LoadState::Loading
            })
        })
    });
    let editor_scroll = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view()
                .map(|v| v.read(cx).code().read(cx).scroll_y())
                .unwrap()
        })
    };
    assert_eq!(editor_scroll(cx), 0.0);

    view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
    draw(cx);
    let total = view.read_with(cx, |wb, _| wb.palette_titles().len());
    let list = region(&view, cx, Region::Palette("list"));

    // Fits the window: more than the old 12 rows, ending above the window's bottom edge.
    let window = cx.update(|window, _| window.viewport_size());
    let rows = (0..total)
        .take_while(|row| {
            view.read_with(cx, |wb, _| {
                wb.region_bounds(Region::PaletteRow(*row)).is_some()
            })
        })
        .count();
    assert!(rows > 12, "{rows} rows of {total} in a {window:?} window");
    assert!(list.bottom() <= window.height, "{list:?} in {window:?}");
    assert!(
        rows < total,
        "the list is longer than the window: it scrolls"
    );
    // A row spans the panel (the key hint sits at its right end), not just its text.
    let first = region(&view, cx, Region::PaletteRow(0));
    assert!(
        (first.size.width - list.size.width).abs() < px(1.0),
        "row {first:?} in list {list:?}"
    );

    // The wheel over the list scrolls the list and leaves the editor where it was.
    assert_eq!(view.read_with(cx, |wb, _| wb.palette_scrolled()), px(0.0));
    for _ in 0..5 {
        wheel(cx, list.center(), -60.0);
    }
    let scrolled = view.read_with(cx, |wb, _| wb.palette_scrolled());
    assert!(scrolled > px(100.0), "the list scrolled: {scrolled:?}");
    assert_eq!(
        editor_scroll(cx),
        0.0,
        "the editor under the palette heard the wheel"
    );

    // Over the backdrop (outside the panel) nothing moves either.
    wheel(cx, point(px(8.0), window.height / 2.0), -60.0);
    assert_eq!(editor_scroll(cx), 0.0, "the backdrop let the wheel through");
    assert_eq!(view.read_with(cx, |wb, _| wb.palette_scrolled()), scrolled);

    // The highlighted row follows the keyboard: ↑ from the first row wraps to the last one.
    view.update(cx, |wb, cx| wb.set_palette_query("", cx));
    draw(cx);
    cx.simulate_keystrokes("up");
    draw(cx);
    let last = view.read_with(cx, |wb, _| wb.palette_titles().len()) - 1;
    wait_until(cx, "the last row in view", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::PaletteRow(last)).is_some()
        })
    });
}

// ---- 3. F-13: ⌘P takes words -------------------------------------------------------------------

#[gpui::test]
fn quick_open_matches_words_separated_by_spaces(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    write(&root, "crates/searcher/src/searcher/glue.rs", "// glue\n");
    write(&root, "crates/searcher/src/lib.rs", "// lib\n");
    write(&root, "crates/printer/src/glue.rs", "// printer glue\n");
    view.update_in(cx, |wb, window, cx| {
        wb.open_project_path(root.clone(), window, cx);
    });
    wait_until(cx, "the project to be the active one", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).files_root().as_deref() == Some(root.as_path())
        })
    });

    cx.dispatch_action(actions::QuickOpen);
    cx.simulate_input("searcher glue");
    wait_until(cx, "the answer to the query", |cx| {
        view.read_with(cx, |wb, _| {
            wb.picker()
                .is_some_and(|p| !p.files.searching && p.files.answers > 0)
        })
    });
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.picker_titles().iter().map(ToString::to_string).collect()
    });
    assert_eq!(
        titles.first().map(String::as_str),
        Some("glue.rs"),
        "{titles:?}"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.picker().map(|p| p.files.rows)),
        Some(1),
        "only the file with both words: {titles:?}"
    );
    cx.simulate_keystrokes("enter");
    let wanted = root
        .join("crates/searcher/src/searcher/glue.rs")
        .display()
        .to_string();
    wait_until(cx, "the file opened", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view().is_some_and(|v| {
                v.read(cx).buffer().read(cx).path().display().to_string() == wanted
            })
        })
    });
}

// ---- 6. F-16: ⌘↑ / ⌘↓ in a terminal --------------------------------------------------------------

#[gpui::test]
fn cmd_up_and_cmd_down_scroll_a_focused_terminal_to_the_ends_of_its_scrollback(
    cx: &mut TestAppContext,
) {
    use gpui::Focusable as _;

    let daemon = TestDaemon::start();
    let session = daemon.sh("seq 1 300; echo END-MARK; exec cat");
    let id = session.id;
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
    let terminal = |view: &Entity<Workbench>, cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(id))
    };
    wait_until(cx, "the output", |cx| {
        terminal(&view, cx).is_some_and(|t| {
            t.read_with(cx, |t, _| {
                t.visible_text().iter().any(|l| l.contains("END-MARK"))
            })
        })
    });
    let terminal = terminal(&view, cx).expect("the session is open");
    let offset = |cx: &VisualTestContext| terminal.read_with(cx, |t, _| t.scroll_offset());
    let focused = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| terminal.focus_handle(cx).is_focused(window))
    };
    assert!(focused(cx), "the keyboard is in the terminal");
    let item = view.read_with(cx, |wb, _| wb.state().active_item());
    assert_eq!(offset(cx), 0);

    cx.simulate_keystrokes("cmd-up");
    wait_until(cx, "the top of the scrollback", |cx| offset(cx) >= 250);
    assert!(focused(cx), "⌘↑ stays in the terminal");
    assert_eq!(view.read_with(cx, |wb, _| wb.state().active_item()), item);

    cx.simulate_keystrokes("cmd-down");
    assert_eq!(offset(cx), 0, "⌘↓ is the live screen again");
    assert!(focused(cx));

    // The block jump is ⌘⇧↑ now: the palette still lists it, with its key.
    let titles = view.update_in(cx, |wb, window, cx| {
        wb.open_palette(window, cx);
        wb.palette_titles()
    });
    for title in [
        "Terminal: Scroll to Top",
        "Terminal: Scroll to Bottom",
        "Block: Previous Prompt",
    ] {
        assert!(titles.iter().any(|t| t.as_ref() == title), "{title}");
    }
}
