#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Scrolling past the end of a file tab (EDITOR-POLISH, F-19) in the desktop: the setting
//! `editor.scroll_beyond_last_line` is live, and a tab that is shown again after its file got
//! shorter on disk starts inside the new extent, not on a stray offset.

use crate::common;

use std::fs;
use std::path::Path;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext};
use hutan_code_view::LoadState;
use hutan_desktop::workbench::file_view::FileView;
use hutan_desktop::workbench::{FileOpen, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

fn numbered(lines: usize) -> String {
    (1..=lines).map(|n| format!("line {n}\n")).collect()
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext, SettingsStore) {
    fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let (view, cx) = open_with(cx, settings.clone(), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx, settings)
}

fn open_file(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    file: &Path,
) -> Entity<FileView> {
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(file), window, cx);
    });
    let canonical = file.canonicalize().unwrap().display().to_string();
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view().is_some_and(|v| {
                let buffer = v.read(cx).buffer().read(cx);
                buffer.path().display().to_string() == canonical
                    && *buffer.state() == LoadState::Ready
            })
        })
    });
    draw(cx);
    let file_view = view
        .read_with(cx, |wb, _| wb.focused_file_view())
        .expect("a file tab");
    let code = file_view.read_with(cx, |v, _| v.code().clone());
    cx.update(|window, cx| {
        let focus = code.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    });
    file_view
}

#[gpui::test]
fn the_setting_is_live_and_a_tab_shown_again_is_inside_the_extent(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, settings) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let long = dir.path().join("long.txt");
    let other = dir.path().join("other.txt");
    fs::write(&long, numbered(300)).unwrap();
    fs::write(&other, "x\n").unwrap();

    let tab = open_file(&view, cx, &long);
    let code = tab.read_with(cx, |v, _| v.code().clone());
    assert_eq!(code.read_with(cx, |c, _| c.scroll_beyond_last_line()), 0.5);
    code.update(cx, |c, cx| c.scroll_to(1.0e9, cx));
    draw(cx);
    let end = code.read_with(cx, |c, _| c.scroll_y());
    let layout = code.read_with(cx, |c, _| c.layout().unwrap());
    let view_h = f32::from(layout.bounds.size.height);
    let line_h = f32::from(layout.line_height);
    assert_eq!(end, 301.0 * line_h - view_h * 0.5);

    // Live: the setting takes effect without reopening the file.
    settings
        .write_user("editor.scroll_beyond_last_line", toml::Value::Float(0.0))
        .unwrap();
    wait_until(cx, "the view to follow the setting", |cx| {
        code.read_with(cx, |c, _| c.scroll_beyond_last_line()) == 0.0
    });
    draw(cx);
    assert_eq!(
        code.read_with(cx, |c, _| c.scroll_y()),
        301.0 * line_h - view_h
    );
    settings
        .write_user("editor.scroll_beyond_last_line", toml::Value::Float(0.5))
        .unwrap();
    wait_until(cx, "the view to follow the setting again", |cx| {
        code.read_with(cx, |c, _| c.scroll_beyond_last_line()) == 0.5
    });
    code.update(cx, |c, cx| c.scroll_to(1.0e9, cx));
    draw(cx);

    // Another tab comes forward; the first file gets shorter on disk meanwhile.
    open_file(&view, cx, &other);
    fs::write(&long, numbered(40)).unwrap();
    wait_until(cx, "the buffer to take the shorter file", |cx| {
        tab.read_with(cx, |v, cx| v.buffer().read(cx).text().line_count() <= 41)
    });
    // Back to the first tab: its first frame is inside the new extent.
    let tab_again = open_file(&view, cx, &long);
    assert_eq!(tab_again.entity_id(), tab.entity_id());
    draw(cx);
    let layout = code.read_with(cx, |c, _| c.layout().unwrap());
    let view_h = f32::from(layout.bounds.size.height);
    let now = code.read_with(cx, |c, _| c.scroll_y());
    assert!(
        now <= 41.0 * line_h - view_h * 0.5 + 0.01,
        "{now} is inside the shorter file's extent"
    );
}
