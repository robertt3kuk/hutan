#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Soft wrap in file tabs (EDITOR-WRAP) over a real daemon: Markdown and plain text wrap and code
//! does not, `editor.soft_wrap` / `editor.wrap_column` / `languages.<lang>.soft_wrap` decide and
//! are live, View → Toggle Soft Wrap (⌥Z) flips one tab, the status bar says `Wrap`, and the
//! rows follow the caret.

use crate::common;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_code_view::{CodeView, FileBuffer, LoadState, ToggleSoftWrap, WrapMode};
use hutan_desktop::workbench::file_view::FileView;
use hutan_desktop::workbench::{FileOpen, Workbench};
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
    view: Entity<FileView>,
    code: Entity<CodeView>,
    #[allow(dead_code)]
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
        view: file_view,
        code,
        buffer,
    }
}

/// A paragraph that is much wider than any test window.
fn long() -> String {
    let words: Vec<String> = (0..80).map(|i| format!("word{i:02}")).collect();
    format!("{}\nshort\n", words.join(" "))
}

fn mode(tab: &Tab, cx: &VisualTestContext) -> WrapMode {
    tab.code.read_with(cx, |c, _| c.soft_wrap())
}

fn drawn_rows(tab: &Tab, cx: &VisualTestContext) -> usize {
    tab.code.read_with(cx, |c, _| c.stats().last_rows.len())
}

fn status_wrap(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<bool> {
    view.read_with(cx, |wb, cx| wb.editor_status(cx).map(|s| s.wrap))
}

#[gpui::test]
fn markdown_and_plain_text_wrap_by_default_and_code_does_not(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "");
    let dir = tempfile::tempdir().unwrap();
    let md = open_text(&view, cx, dir.path(), "a.md", &long());
    assert_eq!(mode(&md, cx), WrapMode::Viewport);
    assert!(md.code.read_with(cx, |c, _| c.is_wrapped()));
    assert!(
        md.code.read_with(cx, |c, cx| c.display_rows(cx)) > 3,
        "the long line is several rows"
    );
    assert_eq!(
        status_wrap(&view, cx),
        Some(true),
        "the status bar says Wrap"
    );

    let txt = open_text(&view, cx, dir.path(), "b.txt", &long());
    assert_eq!(mode(&txt, cx), WrapMode::Viewport);
    // A file without an extension is plain text too.
    let plain = open_text(&view, cx, dir.path(), "NOTES", &long());
    assert_eq!(mode(&plain, cx), WrapMode::Viewport);

    let rs = open_text(&view, cx, dir.path(), "c.rs", &long());
    assert_eq!(mode(&rs, cx), WrapMode::Off);
    assert!(!rs.code.read_with(cx, |c, _| c.is_wrapped()));
    assert_eq!(rs.code.read_with(cx, |c, cx| c.display_rows(cx)), 3);
    assert_eq!(
        status_wrap(&view, cx),
        Some(false),
        "no Wrap for code that scrolls sideways"
    );
}

#[gpui::test]
fn the_setting_and_a_language_override_decide(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(
        cx,
        &daemon,
        "[editor]\nsoft_wrap = \"viewport\"\nwrap_column = 60\n[languages.markdown]\nsoft_wrap = \"column\"\n[languages.plaintext]\nsoft_wrap = \"off\"\n",
    );
    let dir = tempfile::tempdir().unwrap();
    let rs = open_text(&view, cx, dir.path(), "c.rs", &long());
    assert_eq!(mode(&rs, cx), WrapMode::Viewport, "editor.soft_wrap");
    let md = open_text(&view, cx, dir.path(), "a.md", &long());
    assert_eq!(
        mode(&md, cx),
        WrapMode::Column(60),
        "languages.markdown.soft_wrap with editor.wrap_column"
    );
    let width = md
        .code
        .read_with(cx, |c, _| c.wrap_params().expect("wrapping").cols);
    assert_eq!(
        width, 60,
        "rows are 60 cells wide in a window wider than that"
    );
    let txt = open_text(&view, cx, dir.path(), "b.txt", &long());
    assert_eq!(
        mode(&txt, cx),
        WrapMode::Off,
        "languages.plaintext.soft_wrap"
    );
}

#[gpui::test]
fn settings_are_live_and_follow_the_file(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, settings) = open(cx, &daemon, "");
    let dir = tempfile::tempdir().unwrap();
    let md = open_text(&view, cx, dir.path(), "a.md", &long());
    let rs_dir = dir.path();
    let rs = open_text(&view, cx, rs_dir, "c.rs", &long());
    assert_eq!(mode(&md, cx), WrapMode::Viewport);
    assert_eq!(mode(&rs, cx), WrapMode::Off);

    settings
        .write_user("editor.soft_wrap", toml::Value::String("off".into()))
        .unwrap();
    wait_until(cx, "Markdown stops wrapping", |cx| {
        md.code.read_with(cx, |c, _| c.soft_wrap()) == WrapMode::Off
    });
    settings
        .write_user(
            "languages.rust.soft_wrap",
            toml::Value::String("viewport".into()),
        )
        .unwrap();
    wait_until(cx, "Rust wraps", |cx| {
        rs.code.read_with(cx, |c, _| c.soft_wrap()) == WrapMode::Viewport
    });
    assert_eq!(mode(&md, cx), WrapMode::Off);
    draw(cx);
    assert!(rs.code.read_with(cx, |c, _| c.is_wrapped()));
}

#[gpui::test]
fn toggle_soft_wrap_flips_one_tab_and_settings_come_back_when_it_matches(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, settings) = open(cx, &daemon, "");
    let dir = tempfile::tempdir().unwrap();
    let md = open_text(&view, cx, dir.path(), "a.md", &long());
    let rs = open_text(&view, cx, dir.path(), "c.rs", &long());
    // The focused tab is the Rust file: ⌥Z (the action) wraps it, and only it.
    cx.dispatch_action(ToggleSoftWrap);
    draw(cx);
    assert_eq!(mode(&rs, cx), WrapMode::Viewport);
    assert_eq!(
        mode(&md, cx),
        WrapMode::Viewport,
        "the other tab is as it was"
    );
    assert!(rs.code.read_with(cx, |c, _| c.is_wrapped()));
    assert_eq!(status_wrap(&view, cx), Some(true));
    let rows = drawn_rows(&rs, cx);
    assert!(rows > 3);
    cx.dispatch_action(ToggleSoftWrap);
    draw(cx);
    assert_eq!(mode(&rs, cx), WrapMode::Off);
    assert_eq!(status_wrap(&view, cx), Some(false));
    assert_eq!(drawn_rows(&rs, cx), 3);
    assert!(
        md.view.read_with(cx, |v, cx| v.wraps(cx)),
        "Markdown wraps still"
    );

    // A tab that was toggled keeps its choice when settings change; one that was toggled back
    // to what settings say follows them again.
    cx.dispatch_action(ToggleSoftWrap);
    draw(cx);
    assert_eq!(mode(&rs, cx), WrapMode::Viewport);
    settings
        .write_user("editor.soft_wrap", toml::Value::String("off".into()))
        .unwrap();
    wait_until(cx, "Markdown follows the setting", |cx| {
        md.code.read_with(cx, |c, _| c.soft_wrap()) == WrapMode::Off
    });
    assert_eq!(mode(&rs, cx), WrapMode::Viewport, "the toggled tab stays");
    cx.dispatch_action(ToggleSoftWrap);
    draw(cx);
    assert_eq!(mode(&rs, cx), WrapMode::Off);
    settings
        .write_user("editor.soft_wrap", toml::Value::String("viewport".into()))
        .unwrap();
    wait_until(cx, "the tab follows settings again", |cx| {
        rs.code.read_with(cx, |c, _| c.soft_wrap()) == WrapMode::Viewport
    });
}
