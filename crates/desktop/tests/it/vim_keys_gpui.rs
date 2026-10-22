#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Vim's own ⌃ keys (slice FX-VIM-2) with real key events: `⌃[` is Escape everywhere Vim is on
//! (a file tab, its `:` and `/` lines, the chat composer, the palette), also on a Russian
//! layout, and `⌃C` likewise; with Vim off they keep their meaning.

use crate::common;

use common::chat::ChatEnv;
use common::{TestDaemon, draw, offline_connection, open_with, wait_until};
use gpui::{Entity, Keystroke, Modifiers, TestAppContext, VisualTestContext};
use hutan_code_view::{CodeView, FileBuffer, LoadState, VimMode};
use hutan_core::SessionId;
use hutan_desktop::workbench::chat::vim::Mode;
use hutan_desktop::workbench::chat::{ChatModel, ChatView, LoadStatus};
use hutan_desktop::workbench::file_view::FileView;
use hutan_desktop::workbench::{FileOpen, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

// ---- helpers ---------------------------------------------------------------------------------

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
    buffer: Entity<FileBuffer>,
}

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

fn text(buffer: &Entity<FileBuffer>, cx: &mut VisualTestContext) -> String {
    buffer.read_with(cx, |b, _| b.text().to_string())
}

fn file_mode(tab: &Tab, cx: &mut VisualTestContext) -> Option<VimMode> {
    tab.code.read_with(cx, |c, _| c.vim_mode())
}

fn bar(tab: &Tab, cx: &mut VisualTestContext) -> Option<String> {
    draw(cx);
    tab.view
        .read_with(cx, |v, _| v.command_bar_text().map(str::to_owned))
}

/// `⌃[` as a Russian layout reports it when it names keys by its own letters.
fn russian_ctrl_bracket() -> Keystroke {
    Keystroke {
        modifiers: Modifiers::control(),
        key: "х".into(),
        key_char: None,
    }
}

fn press(cx: &mut VisualTestContext, keystroke: Keystroke) {
    cx.update(|window, cx| {
        window.dispatch_keystroke(keystroke, cx);
    });
    cx.run_until_parked();
}

fn open_chat<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    settings: &str,
    session: SessionId,
) -> (
    Entity<Workbench>,
    Entity<ChatView>,
    Entity<ChatModel>,
    &'a mut VisualTestContext,
) {
    std::fs::write(daemon.home().join("settings.toml"), settings).unwrap();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(session, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(session)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
                && wb.chat_view_of(session).is_some()
        })
    });
    let chat = view
        .read_with(cx, |wb, _| wb.chat_view_of(session))
        .unwrap();
    let model = view
        .read_with(cx, |wb, _| wb.chat_model_of(session))
        .unwrap();
    (view, chat, model, cx)
}

fn chat_mode(chat: &Entity<ChatView>, cx: &VisualTestContext) -> Option<Mode> {
    chat.read_with(cx, |c, cx| c.vim_mode(cx))
}

fn draft(model: &Entity<ChatModel>, cx: &VisualTestContext) -> String {
    model.read_with(cx, |m, _| m.input().text().to_owned())
}

// ---- ⌃[ and ⌃C ---------------------------------------------------------------------------------

#[gpui::test]
fn ctrl_bracket_and_ctrl_c_are_escape_in_a_file_tab(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let tab = open_text(&view, cx, dir.path(), "a.txt", "one two\nthree\n");
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Normal));

    // Insert: ⌃[ leaves it, the caret steps back like after Escape.
    cx.simulate_input("A!");
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Insert));
    cx.simulate_keystrokes("ctrl-[");
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Normal));
    assert_eq!(text(&tab.buffer, cx), "one two!\nthree\n");
    // The same key on a Russian layout, and ⌃C.
    cx.simulate_input("i");
    press(cx, russian_ctrl_bracket());
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Normal));
    cx.simulate_input("a");
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Normal));
    // `3ix` and ⌃C: the count repeats the text, as for a ⌃C Vim reads as a key.
    cx.simulate_input("03ix");
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(text(&tab.buffer, cx), "xxxone two!\nthree\n");

    // Visual and a pending operator.
    cx.simulate_input("v");
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Visual));
    cx.simulate_keystrokes("ctrl-[");
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Normal));
    cx.simulate_input("V");
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Normal));
    cx.simulate_input("d");
    cx.simulate_keystrokes("ctrl-[");
    cx.simulate_input("j");
    assert_eq!(
        text(&tab.buffer, cx),
        "xxxone two!\nthree\n",
        "the `d` was dropped, `j` only moved"
    );

    // The `/` line and the `:` line.
    cx.simulate_input("/thr");
    assert_eq!(bar(&tab, cx).as_deref(), Some("/thr"));
    cx.simulate_keystrokes("ctrl-[");
    assert_eq!(bar(&tab, cx), None);
    cx.simulate_input("?on");
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(bar(&tab, cx), None);
    cx.simulate_input(":");
    draw(cx);
    cx.simulate_input("s/x/y/");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":s/x/y/"));
    cx.simulate_keystrokes("ctrl-[");
    assert_eq!(bar(&tab, cx), None, "closed without running");
    cx.simulate_input(":");
    draw(cx);
    cx.simulate_input("s/x/y/");
    press(cx, russian_ctrl_bracket());
    assert_eq!(bar(&tab, cx), None);
    cx.simulate_input(":");
    draw(cx);
    cx.simulate_input("s/x/y/");
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(bar(&tab, cx), None);
    assert_eq!(text(&tab.buffer, cx), "xxxone two!\nthree\n");
    // The code has the keyboard back: `x` deletes.
    cx.simulate_input("x");
    assert_eq!(text(&tab.buffer, cx), "xxxone two!\nthee\n");
}

#[gpui::test]
fn ctrl_bracket_and_ctrl_c_leave_insert_in_the_composer(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (_view, chat, model, cx) = open_chat(cx, &daemon, "[editor]\nvim_mode = true\n", info.id);

    assert_eq!(chat_mode(&chat, cx), Some(Mode::Insert));
    cx.simulate_input("hello");
    cx.simulate_keystrokes("ctrl-[");
    assert_eq!(chat_mode(&chat, cx), Some(Mode::Normal));
    cx.simulate_input("a!");
    press(cx, russian_ctrl_bracket());
    assert_eq!(chat_mode(&chat, cx), Some(Mode::Normal));
    cx.simulate_input("a?");
    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(chat_mode(&chat, cx), Some(Mode::Normal));
    assert_eq!(draft(&model, cx), "hello!?");
    // Visual, then ⌃[.
    cx.simulate_input("v");
    assert_eq!(chat_mode(&chat, cx), Some(Mode::Visual));
    cx.simulate_keystrokes("ctrl-[");
    assert_eq!(chat_mode(&chat, cx), Some(Mode::Normal));
}

#[gpui::test]
fn ctrl_bracket_closes_the_palette_in_normal_mode_like_escape(cx: &mut TestAppContext) {
    let (connection, settings, _) = offline_connection();
    settings
        .write_user("editor.vim_mode", toml::Value::Boolean(true))
        .unwrap();
    let (view, cx) = open_with(cx, settings, connection, None);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("split");
    cx.simulate_keystrokes("ctrl-[");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.palette_vim_mode()),
        Some(Mode::Normal)
    );
    assert!(view.read_with(cx, |wb, _| wb.palette_open()));
    cx.simulate_keystrokes("ctrl-[");
    assert!(
        !view.read_with(cx, |wb, _| wb.palette_open()),
        "the second ⌃[ closes, as Escape does"
    );
}

#[gpui::test]
fn with_vim_off_ctrl_bracket_is_no_escape(cx: &mut TestAppContext) {
    let (connection, settings, _) = offline_connection();
    let (view, cx) = open_with(cx, settings, connection, None);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("split");
    cx.simulate_keystrokes("ctrl-[");
    assert!(
        view.read_with(cx, |wb, _| wb.palette_open()),
        "⌃[ is not Escape without Vim"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.palette().map(|p| p.query.clone())),
        Some("split".to_owned())
    );
}
