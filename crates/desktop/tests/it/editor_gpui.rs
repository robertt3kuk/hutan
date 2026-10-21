#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! File tabs (slice E-5) over a real daemon: a file opens through `fs.stat`/`fs.read`, the same
//! file in two panes shares one buffer, closing the last tab frees the buffer and its syntax
//! tree, the layout round-trips with a file tab, a theme change restyles without a reparse,
//! large files ask first and binary files are refused.

use crate::common;

use std::time::{Duration, Instant};

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext};
use hutan_code_view::{FailReason, LoadState};
use hutan_desktop::workbench::model::{Edge, ItemRef, WorkbenchState};
use hutan_desktop::workbench::{FileOpen, Workbench, actions};
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

fn rust_source(lines: usize) -> String {
    (0..lines)
        .map(|n| format!("pub fn item_{n}() -> u32 {{ {n} }}\n"))
        .collect()
}

/// Opens `file` (picked) and waits until the focused tab shows its text.
fn open_file(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    file: &std::path::Path,
) -> String {
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(file), window, cx);
    });
    let canonical = file.canonicalize().unwrap().display().to_string();
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view().is_some_and(|v| {
                let buffer = v.read(cx).buffer().read(cx);
                buffer.path().display().to_string() == canonical
                    && *buffer.state() != LoadState::Loading
            })
        })
    });
    canonical
}

#[gpui::test]
fn two_panes_share_one_buffer_and_the_last_tab_frees_it(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, rust_source(500)).unwrap();
    let path = open_file(&view, cx, &file);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(ItemRef::file(path.clone()))
    );

    // The same file in a split: a second view, one buffer.
    let first_pane = view.read_with(cx, |wb, _| wb.state().active_pane_id());
    view.update_in(cx, |wb, window, cx| {
        wb.open_file_in_split(first_pane, &path, Edge::Right, window, cx);
    });
    draw(cx);
    let second_pane = view.read_with(cx, |wb, _| wb.state().active_pane_id());
    assert_ne!(first_pane, second_pane);
    let (a, b) = view.read_with(cx, |wb, _| {
        (
            wb.file_view_of(first_pane, &path).expect("first view"),
            wb.file_view_of(second_pane, &path).expect("second view"),
        )
    });
    assert_ne!(a.entity_id(), b.entity_id(), "two views");
    let (buffer_a, buffer_b) = (
        a.read_with(cx, |v, _| v.buffer().clone()),
        b.read_with(cx, |v, _| v.buffer().clone()),
    );
    assert_eq!(buffer_a.entity_id(), buffer_b.entity_id(), "one buffer");
    assert_eq!(view.read_with(cx, |wb, _| wb.buffers().len()), 1);

    // The visible file has a tree; keep a handle on it to see it freed.
    assert!(buffer_a.read_with(cx, |b, _| b.wait_for_syntax(Duration::from_secs(30))));
    let tree = std::sync::Arc::downgrade(
        &buffer_a
            .read_with(cx, |b, _| b.syntax_snapshot())
            .expect("a tree"),
    );
    let weak_buffer = buffer_a.downgrade();
    drop((a, b, buffer_a, buffer_b));

    // Closing one tab keeps the buffer for the other.
    cx.dispatch_action(actions::ClosePaneItem);
    draw(cx);
    assert_eq!(view.read_with(cx, |wb, _| wb.buffers().len()), 1);
    // Closing the last one frees it, and its tree.
    cx.dispatch_action(actions::ClosePaneItem);
    draw(cx);
    assert_eq!(view.read_with(cx, |wb, _| wb.buffers().len()), 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while weak_buffer.upgrade().is_some() || tree.upgrade().is_some() {
        assert!(
            Instant::now() < deadline,
            "the buffer and its tree are freed"
        );
        draw(cx);
        std::thread::yield_now();
    }
}

#[gpui::test]
fn the_layout_round_trips_with_a_file_tab(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "# Notes\n\nSome *text*.\n").unwrap();
    let path = open_file(&view, cx, &file);
    let json = view.read_with(cx, |wb, _| wb.state().to_json().unwrap());
    let restored = WorkbenchState::from_json(&json).unwrap();
    assert!(
        restored.find_item(&ItemRef::file(path.clone())).is_some(),
        "the file tab comes back: {json}"
    );
    // A binary from before file tabs reads it as a placeholder named after the file.
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let text = value.to_string();
    assert!(
        text.contains(r#""placeholder":{"file":"#) || text.contains(r#""title":"notes.md""#),
        "{text}"
    );
}

#[gpui::test]
fn a_theme_change_restyles_open_files_without_a_reparse(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, settings) = open(cx, &daemon, "[appearance]\nappearance = \"dark\"\n");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.rs");
    std::fs::write(&file, rust_source(300)).unwrap();
    open_file(&view, cx, &file);
    let file_view = view.read_with(cx, |wb, _| wb.focused_file_view().unwrap());
    let buffer = file_view.read_with(cx, |v, _| v.buffer().clone());
    assert!(buffer.read_with(cx, |b, _| b.wait_for_syntax(Duration::from_secs(30))));
    draw(cx);
    let parses = buffer.read_with(cx, |b, _| b.syntax_stats().unwrap().started);
    let before = file_view.read_with(cx, |v, cx| v.code().read(cx).style().theme.name.clone());

    settings
        .write_user("appearance.appearance", toml::Value::String("light".into()))
        .unwrap();
    wait_until(cx, "the light editor colors", |cx| {
        file_view.read_with(cx, |v, cx| v.code().read(cx).style().theme.name != before)
    });
    assert_eq!(
        buffer.read_with(cx, |b, _| b.syntax_stats().unwrap().started),
        parses,
        "restyling parses nothing"
    );
}

#[gpui::test]
fn large_files_ask_first_and_binary_files_are_refused(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nlarge_file_bytes = 1000\n");
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.rs");
    std::fs::write(&big, rust_source(100)).unwrap();
    open_file(&view, cx, &big);
    let file_view = view.read_with(cx, |wb, _| wb.focused_file_view().unwrap());
    let buffer = file_view.read_with(cx, |v, _| v.buffer().clone());
    assert!(matches!(
        buffer.read_with(cx, |b, _| b.state().clone()),
        LoadState::NeedsConfirm { .. }
    ));
    file_view.update(cx, |_, cx| {
        cx.emit(
            hutan_desktop::workbench::file_view::FileViewEvent::OpenLarge(
                buffer.read(cx).path().to_owned(),
            ),
        );
    });
    wait_until(cx, "the large file to load", |cx| {
        buffer.read_with(cx, |b, _| b.is_ready())
    });
    assert!(buffer.read_with(cx, |b, _| b.is_large() && !b.has_syntax()));

    let binary = dir.path().join("blob.bin");
    std::fs::write(&binary, b"\x00\x01\x02").unwrap();
    open_file(&view, cx, &binary);
    let state = view.read_with(cx, |wb, cx| {
        wb.focused_file_view()
            .unwrap()
            .read(cx)
            .buffer()
            .read(cx)
            .state()
            .clone()
    });
    assert!(
        matches!(
            state,
            LoadState::Failed {
                reason: FailReason::Binary,
                ..
            }
        ),
        "{state:?}"
    );
}

#[gpui::test]
fn workbench_actions_reach_the_window_from_a_focused_file(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, rust_source(50)).unwrap();
    let path = open_file(&view, cx, &file);
    draw(cx);
    let focused = cx.update(|window, cx| {
        let code = view
            .read(cx)
            .focused_file_view()
            .unwrap()
            .read(cx)
            .code()
            .clone();
        code.focus_handle(cx).is_focused(window)
    });
    assert!(focused, "the file's code has the keyboard");
    cx.dispatch_action(actions::OpenFileInSplitRight);
    draw(cx);
    let panes = view.read_with(cx, |wb, _| wb.state().center.panes().len());
    assert_eq!(panes, 2, "the palette command reached the workbench");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(ItemRef::file(path))
    );
}

/// E-6 over a real daemon: typing then ⌘S writes through `fs.write` and the file keeps its
/// CRLF line endings; our own save comes back as `fs.changed` (origin `self`) and reloads
/// nothing; a change on disk reloads a clean buffer; a change on disk under unsaved changes shows
/// the conflict bar and ⌘S never writes over it; Keep Mine does, on purpose.
#[gpui::test]
fn saving_keeps_crlf_and_never_overwrites_a_change_on_disk(cx: &mut TestAppContext) {
    use hutan_code_view::Conflict;
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "one\r\ntwo\r\n").unwrap();
    open_file(&view, cx, &file);
    let file_view = view.read_with(cx, |wb, _| wb.focused_file_view().unwrap());
    let buffer = file_view.read_with(cx, |v, _| v.buffer().clone());
    draw(cx);

    cx.simulate_input("zero ");
    assert!(buffer.read_with(cx, |b, _| b.is_dirty()));
    cx.simulate_keystrokes("cmd-s");
    wait_until(cx, "the save", |cx| {
        buffer.read_with(cx, |b, _| !b.is_dirty() && !b.is_saving())
    });
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"zero one\r\ntwo\r\n",
        "CRLF stays"
    );

    // The daemon tells this window about its own write; that is no change.
    let version = buffer.read_with(cx, |b, _| b.version());
    wait_until(cx, "the echo of our own save", |cx| {
        view.read_with(cx, |wb, _| wb.buffers().changes_seen() >= 1)
    });
    assert_eq!(
        buffer.read_with(cx, |b, _| b.version()),
        version,
        "no reload"
    );
    assert!(buffer.read_with(cx, |b, _| b.conflict().is_none() && b.can_undo()));

    // Somebody else changes the file: the clean buffer takes it.
    std::fs::write(&file, "external\r\ntwo\r\n").unwrap();
    wait_until(cx, "the reload", |cx| {
        buffer.read_with(cx, |b, _| b.text() == "external\ntwo\n")
    });
    assert!(!buffer.read_with(cx, |b, _| b.is_dirty()));

    // Under unsaved changes a change on disk gets the bar, and ⌘S does not write.
    cx.simulate_input("mine ");
    std::fs::write(&file, "theirs\r\n").unwrap();
    wait_until(cx, "the conflict bar", |cx| {
        buffer.read_with(cx, |b, _| {
            matches!(b.conflict(), Some(Conflict::Changed { disk: Some(_) }))
        })
    });
    cx.simulate_keystrokes("cmd-s");
    draw(cx);
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"theirs\r\n",
        "never overwritten"
    );
    assert!(buffer.read_with(cx, |b, _| b.is_dirty()));
    file_view.update(cx, |v, cx| v.toggle_diff(cx));
    assert!(file_view.read_with(cx, |v, _| v.diff().is_some_and(|d| !d.is_empty())));

    // Keep Mine writes the buffer over the file, on purpose.
    file_view.update(cx, |_, cx| {
        cx.emit(
            hutan_desktop::workbench::file_view::FileViewEvent::Overwrite(
                buffer.read(cx).path().to_owned(),
            ),
        );
    });
    wait_until(cx, "the overwrite", |cx| {
        buffer.read_with(cx, |b, _| !b.is_dirty() && !b.is_saving())
    });
    let mine = buffer.read_with(cx, |b, _| b.text().to_string());
    assert!(mine.contains("mine "), "{mine:?}");
    assert_eq!(
        String::from_utf8(std::fs::read(&file).unwrap()).unwrap(),
        mine.replace('\n', "\r\n")
    );
}
