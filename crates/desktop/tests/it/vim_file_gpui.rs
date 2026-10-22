#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Vim in file tabs (slice FX-VIM-1) over a real daemon, with real key and mouse events: the
//! status items follow every key, a Normal-mode click stays on a character, the `/` line shows
//! while it is typed, a Cyrillic layout's keys are commands by their place, `:sp` keeps the
//! caret line and its lower pane runs its own `:` line, and turning Vim on in the Settings sheet
//! leaves the sheet's search plain and hands the keyboard back to the editor.

use crate::common;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{
    Entity, Focusable as _, Keystroke, Modifiers, TestAppContext, VisualTestContext, point, px,
};
use hutan_code_view::{CodeView, FileBuffer, LoadState, VimMode};
use hutan_desktop::workbench::file_view::FileView;
use hutan_desktop::workbench::{FileOpen, Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    settings_toml: &str,
) -> (Entity<Workbench>, &'a mut VisualTestContext, SettingsStore) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        format!("[app]\nsetup_done = true\n{settings_toml}"),
    )
    .unwrap();
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let (view, cx) = open_with(cx, settings.clone(), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    (view, cx, settings)
}

struct Tab {
    path: String,
    view: Entity<FileView>,
    code: Entity<CodeView>,
    buffer: Entity<FileBuffer>,
}

/// Opens `text` as `name` in a tab and waits until its text is in, the code focused.
fn open_text(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    dir: &std::path::Path,
    name: &str,
    text: &str,
) -> Tab {
    let file = dir.join(name);
    std::fs::write(&file, text).unwrap();
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&file), window, cx);
    });
    let path = file.canonicalize().unwrap().display().to_string();
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view().is_some_and(|v| {
                let buffer = v.read(cx).buffer().read(cx);
                buffer.path().display().to_string() == path && *buffer.state() == LoadState::Ready
            })
        })
    });
    let file_view = view.read_with(cx, |wb, _| wb.focused_file_view().unwrap());
    let (code, buffer) = file_view.read_with(cx, |v, _| (v.code().clone(), v.buffer().clone()));
    draw(cx);
    Tab {
        path,
        view: file_view,
        code,
        buffer,
    }
}

/// What the status bar's `editor.mode` and `editor.position` drew in the last frame.
fn status(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> (String, String) {
    draw(cx);
    view.read_with(cx, |wb, cx| {
        let items = wb.editor_items().expect("the status bar was drawn");
        (
            items.mode_text(cx).unwrap_or_default(),
            items.position_text(cx).unwrap_or_default(),
        )
    })
}

fn text(buffer: &Entity<FileBuffer>, cx: &mut VisualTestContext) -> String {
    buffer.read_with(cx, |b, _| b.text().to_string())
}

/// A key on a Russian layout: macOS names it by its US place (`key`) and it types `typed`.
fn russian(key: &str, typed: &str) -> Keystroke {
    Keystroke {
        modifiers: Modifiers::none(),
        key: key.into(),
        key_char: Some(typed.into()),
    }
}

fn press(cx: &mut VisualTestContext, keystroke: Keystroke) {
    cx.update(|window, cx| {
        window.dispatch_keystroke(keystroke, cx);
    });
    cx.run_until_parked();
}

#[gpui::test]
fn the_status_items_follow_every_vim_key_without_a_click(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let tab = open_text(
        &view,
        cx,
        dir.path(),
        "s.txt",
        "alpha beta gamma\nsecond line\nthird\nlast one",
    );
    assert_eq!(status(&view, cx), ("NORMAL".into(), "Ln 1, Col 1".into()));

    // The mode chip after each switch.
    cx.simulate_input("i");
    assert_eq!(status(&view, cx).0, "INSERT");
    cx.simulate_keystrokes("escape");
    assert_eq!(status(&view, cx).0, "NORMAL");
    cx.simulate_input("v");
    assert_eq!(status(&view, cx).0, "VISUAL");
    cx.simulate_keystrokes("escape");
    assert_eq!(status(&view, cx).0, "NORMAL");
    cx.simulate_input("2d");
    assert_eq!(status(&view, cx).0, "NORMAL 2d", "a command in progress");
    cx.simulate_keystrokes("escape");

    // `Ln/Col` follows the motions.
    cx.simulate_input("w");
    assert_eq!(status(&view, cx).1, "Ln 1, Col 7");
    cx.simulate_input("w");
    assert_eq!(status(&view, cx).1, "Ln 1, Col 12");
    cx.simulate_input("0");
    assert_eq!(status(&view, cx).1, "Ln 1, Col 1");
    cx.simulate_input("j");
    assert_eq!(status(&view, cx).1, "Ln 2, Col 1");
    cx.simulate_input("G");
    assert_eq!(status(&view, cx).1, "Ln 4, Col 1");
    // Typing in Insert moves it too.
    cx.simulate_input("Ax");
    assert_eq!(status(&view, cx), ("INSERT".into(), "Ln 4, Col 10".into()));
    cx.simulate_keystrokes("escape");
    assert_eq!(status(&view, cx), ("NORMAL".into(), "Ln 4, Col 9".into()));
    assert!(text(&tab.buffer, cx).ends_with("last onex"));

    // A click past the end of a line in Normal mode lands on its last character.
    let layout = tab.code.read_with(cx, |c, _| c.layout()).unwrap();
    let line_h = f32::from(layout.line_height);
    let scroll = tab.code.read_with(cx, |c, _| c.scroll_y());
    let y = layout.bounds.top() + px(2.5 * line_h - scroll);
    let x = layout.bounds.right() - px(8.0);
    cx.simulate_click(point(x, y), Modifiers::none());
    assert_eq!(
        status(&view, cx).1,
        "Ln 3, Col 5",
        "on the `d` of `third`, not after it"
    );
    cx.simulate_input("x");
    assert!(text(&tab.buffer, cx).contains("\nthir\n"), "x deletes it");
}

#[gpui::test]
fn the_last_line_of_a_file_ending_in_a_newline_is_its_last_real_line(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let tab = open_text(&view, cx, dir.path(), "n.txt", "one\ntwo\nthree\nfour\n");

    cx.simulate_input("G");
    assert_eq!(
        status(&view, cx).1,
        "Ln 4, Col 1",
        "on `four`, not on an empty line after it"
    );
    cx.simulate_input("j");
    assert_eq!(status(&view, cx).1, "Ln 4, Col 1", "j at the end stays");
    cx.simulate_input("gg:");
    draw(cx);
    cx.simulate_input("$");
    cx.simulate_keystrokes("enter");
    assert_eq!(status(&view, cx).1, "Ln 4, Col 1", ":$ too");
    cx.simulate_input("2GdG");
    assert_eq!(
        text(&tab.buffer, cx),
        "one\n",
        "dG deletes to the end and the file still ends in a newline"
    );
    assert_eq!(status(&view, cx).1, "Ln 1, Col 1");
}

#[gpui::test]
fn the_search_line_shows_while_it_is_typed(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let tab = open_text(&view, cx, dir.path(), "f.txt", "one\ntwo foo\nthree foo\n");
    let bar = |cx: &mut VisualTestContext| {
        draw(cx);
        tab.view
            .read_with(cx, |v, _| v.command_bar_text().map(str::to_owned))
    };

    cx.simulate_input("/");
    assert_eq!(bar(cx).as_deref(), Some("/"));
    cx.simulate_input("fo");
    assert_eq!(bar(cx).as_deref(), Some("/fo"));
    cx.simulate_input("o");
    assert_eq!(bar(cx).as_deref(), Some("/foo"));
    assert_eq!(
        status(&view, cx).0,
        "NORMAL",
        "the line is under the code, not in the status bar"
    );
    cx.simulate_keystrokes("escape");
    assert_eq!(bar(cx), None, "Escape clears it");
    assert_eq!(status(&view, cx).1, "Ln 1, Col 1");

    // Return searches; `?` shows its own line.
    cx.simulate_input("/foo");
    cx.simulate_keystrokes("enter");
    assert_eq!(bar(cx), None);
    assert_eq!(status(&view, cx).1, "Ln 2, Col 5");
    cx.simulate_input("?tw");
    assert_eq!(bar(cx).as_deref(), Some("?tw"));
    cx.simulate_keystrokes("backspace backspace backspace");
    assert_eq!(bar(cx), None, "Backspace on an empty line ends it");
}

#[gpui::test]
fn a_cyrillic_layout_types_commands_by_their_place_and_text_as_typed(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let tab = open_text(&view, cx, dir.path(), "r.txt", "first\nsecond\n");

    // The key of `d` types `в` on a Russian layout: `dd` deletes the line.
    press(cx, russian("d", "в"));
    press(cx, russian("d", "в"));
    assert_eq!(text(&tab.buffer, cx), "second\n");
    // ⇧ and the key of `a` (`ф`) is `A`.
    let mut append = russian("a", "Ф");
    append.modifiers.shift = true;
    press(cx, append);
    assert_eq!(
        tab.code.read_with(cx, |c, _| c.vim_mode()),
        Some(VimMode::Insert)
    );
    // Insert mode types what the layout types.
    press(cx, russian("d", "в"));
    press(cx, russian("f", "а"));
    assert_eq!(text(&tab.buffer, cx), "secondва\n");
    cx.simulate_keystrokes("escape");
    // After `f` the character itself is the target: `fв` finds the `в`.
    cx.simulate_input("0");
    press(cx, russian("f", "а"));
    press(cx, russian("d", "в"));
    assert_eq!(status(&view, cx).1, "Ln 1, Col 7");
    // `r` (the key that types `к`) waits for a character, which is the Cyrillic one.
    press(cx, russian("r", "к"));
    press(cx, russian("h", "р"));
    assert_eq!(text(&tab.buffer, cx), "secondра\n");
}

#[gpui::test]
fn split_keeps_the_caret_line_and_the_lower_pane_runs_its_own_colon_line(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let source: String = (1..=120).map(|n| format!("line {n}\n")).collect();
    let tab = open_text(&view, cx, dir.path(), "long.txt", &source);
    let upper = view.read_with(cx, |wb, _| wb.state().active_pane_id());
    cx.simulate_input("60G");
    assert_eq!(status(&view, cx).1, "Ln 60, Col 1");

    // `:sp` typed in the tab.
    cx.simulate_input(":");
    draw(cx);
    cx.simulate_input("sp");
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the split", |cx| {
        view.read_with(cx, |wb, _| wb.state().center.panes().len() == 2)
    });
    let lower = view.read_with(cx, |wb, _| wb.state().active_pane_id());
    assert_ne!(lower, upper, "the new pane has the focus");
    let lower_view = view
        .read_with(cx, |wb, _| wb.file_view_of(lower, &tab.path))
        .expect("the lower pane's view");
    let lower_code = lower_view.read_with(cx, |v, _| v.code().clone());
    draw(cx);
    let in_view = |code: &Entity<CodeView>, cx: &mut VisualTestContext| {
        code.read_with(cx, |c, cx| {
            let line = c.position(cx).0 - 1;
            let rows = c.layout().unwrap().rows;
            (
                c.position(cx),
                c.top_line() <= line && line + 1 < c.top_line() + rows,
            )
        })
    };
    assert_eq!(in_view(&lower_code, cx), ((60, 1), true), "the lower pane");
    assert_eq!(in_view(&tab.code, cx), ((60, 1), true), "the upper pane");
    assert!(
        cx.update(|window, cx| lower_code.focus_handle(cx).is_focused(window)),
        "the lower pane's code has the keyboard"
    );
    assert_eq!(status(&view, cx).1, "Ln 60, Col 1");

    // Keys go to the lower pane: its own `:` line shows, and `:q` closes it.
    cx.simulate_input("j");
    assert_eq!(status(&view, cx).1, "Ln 61, Col 1");
    assert_eq!(in_view(&tab.code, cx).0, (60, 1), "the upper caret stays");
    cx.simulate_input(":");
    draw(cx);
    cx.simulate_input("q");
    draw(cx);
    assert_eq!(
        lower_view.read_with(cx, |v, _| v.command_bar_text().map(str::to_owned)),
        Some(":q".to_owned())
    );
    assert_eq!(
        tab.view
            .read_with(cx, |v, _| v.command_bar_text().map(str::to_owned)),
        None
    );
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the lower pane to close", |cx| {
        view.read_with(cx, |wb, _| wb.state().center.panes().len() == 1)
    });
    let left = view.read_with(cx, |wb, _| wb.state().center.panes()[0].id);
    assert_eq!(left, upper, "`:q` closed the pane it was typed in");
}

#[gpui::test]
fn turning_vim_on_in_settings_leaves_the_search_plain_and_the_editor_focused(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "");
    let dir = tempfile::tempdir().unwrap();
    let tab = open_text(&view, cx, dir.path(), "t.txt", "text\n");
    let code_focused = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| tab.code.focus_handle(cx).is_focused(window))
    };
    assert!(code_focused(cx));

    // The sidebar's Settings button, clicked.
    wait_until(cx, "the Settings button", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Toolbar("settings")).is_some()
        })
    });
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Toolbar("settings")))
        .unwrap()
        .center();
    cx.simulate_click(at, Modifiers::none());
    draw(cx);
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_some()));
    view.update(cx, |wb, cx| wb.select_settings_namespace("editor", cx));
    view.update(cx, |wb, cx| wb.sheet_toggle("vim_mode", cx));
    wait_until(cx, "Vim on in the tab", |cx| {
        tab.code.read_with(cx, |c, _| c.vim_mode()) == Some(VimMode::Normal)
    });

    // The search box stays plain: the first Escape closes the sheet.
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.settings_sheet().is_none()),
        "one Escape closes the sheet"
    );
    assert!(code_focused(cx), "the editor has the keyboard again");
    cx.simulate_input("x");
    assert_eq!(text(&tab.buffer, cx), "ext\n", "a Normal-mode command");
}
