#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! ⌘-click on a link in a terminal reaches the one opener (UX-LINKS): the view's
//! `OpenLink` event, the `terminal.open_files_with` rule, and URLs to the browser. What the view
//! detects is tested in `hutan-terminal-view`.

use crate::common;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use common::{TestDaemon, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::settings::OpenFilesWith;
use hutan_desktop::workbench::Workbench;
use hutan_desktop::workbench::open_link::FileOpener;
use hutan_settings::{SettingsPaths, SettingsStore};
use hutan_terminal_view::{LinkTarget, TerminalView, TerminalViewEvent};

type Opened = Rc<RefCell<Vec<(PathBuf, OpenFilesWith)>>>;

/// A workbench with one terminal session open, and a recording file opener.
fn open_terminal<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    settings_toml: &str,
) -> (
    Entity<Workbench>,
    &'a mut VisualTestContext,
    Entity<TerminalView>,
    Opened,
) {
    std::fs::write(daemon.home().join("settings.toml"), settings_toml).unwrap();
    let info = daemon.sh("sleep 600");
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    open_session(&view, cx, info.id);
    wait_until(cx, "the terminal view", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(info.id).is_some())
    });
    let terminal = view.read_with(cx, |wb, _| wb.terminal_view_of(info.id).unwrap());
    let opened: Opened = Rc::default();
    let sink = opened.clone();
    cx.update(|_, cx| {
        cx.set_global(FileOpener(Rc::new(move |path, with| {
            sink.borrow_mut().push((path.to_path_buf(), with));
        })));
    });
    (view, cx, terminal, opened)
}

fn open_session(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
}

fn click(terminal: &Entity<TerminalView>, cx: &mut VisualTestContext, target: LinkTarget) {
    terminal.update(cx, |_, cx| cx.emit(TerminalViewEvent::OpenLink(target)));
    cx.run_until_parked();
}

#[gpui::test]
fn a_clicked_file_opens_with_the_system_by_default(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (_view, cx, terminal, opened) = open_terminal(cx, &daemon, "");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();
    click(
        &terminal,
        cx,
        LinkTarget::File {
            path: file.clone(),
            line: Some(42),
            column: Some(7),
        },
    );
    assert_eq!(*opened.borrow(), [(file, OpenFilesWith::System)]);
}

#[gpui::test]
fn terminal_open_files_with_editor_is_honoured(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (_view, cx, terminal, opened) =
        open_terminal(cx, &daemon, "[terminal]\nopen_files_with = \"editor\"\n");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "").unwrap();
    click(
        &terminal,
        cx,
        LinkTarget::File {
            path: file.clone(),
            line: None,
            column: None,
        },
    );
    assert_eq!(*opened.borrow(), [(file, OpenFilesWith::Editor)]);
}

#[gpui::test]
fn a_url_opens_in_the_browser_and_a_vanished_file_says_so(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, terminal, opened) = open_terminal(cx, &daemon, "");
    click(
        &terminal,
        cx,
        LinkTarget::Url("https://example.com/a".into()),
    );
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/a"));
    assert!(opened.borrow().is_empty(), "a URL is not a file");

    let gone = PathBuf::from("/nonexistent/hutan/lib.rs");
    click(
        &terminal,
        cx,
        LinkTarget::File {
            path: gone.clone(),
            line: Some(1),
            column: None,
        },
    );
    assert!(opened.borrow().is_empty());
    let notice = view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string));
    assert!(
        notice.is_some_and(
            |n| n.contains("/nonexistent/hutan/lib.rs") && n.contains("no longer exists")
        )
    );
}

#[gpui::test]
fn a_notice_from_the_terminal_view_is_shown(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, terminal, _opened) = open_terminal(cx, &daemon, "");
    terminal.update(cx, |_, cx| {
        cx.emit(TerminalViewEvent::Notice(
            hutan_terminal_view::IMAGE_PASTE_NOTICE.to_owned(),
        ));
    });
    cx.run_until_parked();
    let notice = view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string));
    assert_eq!(
        notice.as_deref(),
        Some("Images can't be pasted into a terminal — attach them in a chat")
    );
}
