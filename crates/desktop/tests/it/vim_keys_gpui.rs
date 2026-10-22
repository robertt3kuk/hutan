#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Vim's own ⌃ keys (slice FX-VIM-2) with real key events: `⌃[` is Escape everywhere Vim is on
//! (a file tab, its `:` and `/` lines, the chat composer, the palette), also on a Russian
//! layout, and `⌃C` likewise; with Vim off they keep their meaning.

use crate::common;

use common::chat::ChatEnv;
use common::{TestDaemon, draw, offline_connection, open_with, wait_until};
use gpui::{Entity, Focusable as _, Keystroke, Modifiers, TestAppContext, VisualTestContext};
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
fn the_ex_line_of_a_file_tab_takes_ctrl_w_h_u_r_and_ctrl_p_n_history(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let tab = open_text(&view, cx, dir.path(), "a.txt", "one two\nthree\n");
    // `"ayiw`: register a holds `one`.
    cx.simulate_input("\"ayiw");

    cx.simulate_input(":");
    draw(cx);
    cx.simulate_input("echo foo bar");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":echo foo bar"));
    cx.simulate_keystrokes("ctrl-w");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":echo foo "));
    cx.simulate_keystrokes("ctrl-h");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":echo foo"));
    cx.simulate_keystrokes("ctrl-u");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":"));
    cx.simulate_keystrokes("ctrl-r a");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":one"));
    cx.simulate_keystrokes("ctrl-h ctrl-h ctrl-h");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":"));
    // ⌃H on the empty line closes it, as Backspace does.
    cx.simulate_keystrokes("ctrl-h");
    assert_eq!(bar(&tab, cx), None);
    assert_eq!(text(&tab.buffer, cx), "one two\nthree\n");

    // ⌃P / ⌃N walk the history like ↑ / ↓.
    for line in ["2", "1"] {
        cx.simulate_input(":");
        draw(cx);
        cx.simulate_input(line);
        cx.simulate_keystrokes("enter");
        draw(cx);
    }
    cx.simulate_input(":");
    draw(cx);
    cx.simulate_keystrokes("ctrl-p");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":1"));
    cx.simulate_keystrokes("ctrl-p");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":2"));
    cx.simulate_keystrokes("ctrl-n");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":1"));
    cx.simulate_keystrokes("ctrl-n");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":"));
    cx.simulate_keystrokes("up");
    assert_eq!(bar(&tab, cx).as_deref(), Some(":1"));
    cx.simulate_keystrokes("escape");
    assert_eq!(bar(&tab, cx), None);
}

#[gpui::test]
fn the_ex_line_takes_its_own_row_and_the_caret_stays_in_view(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let long: String = (1..=300).map(|n| format!("line {n}\n")).collect();
    let tab = open_text(&view, cx, dir.path(), "long.txt", &long);
    // `G`: the caret on the last line, at the bottom of the view.
    cx.simulate_input("G");
    draw(cx);
    draw(cx);
    let before = tab.code.read_with(cx, |c, _| c.scroll_y());
    assert!(before > 0.0);
    cx.simulate_input(":");
    draw(cx);
    draw(cx);
    assert_eq!(bar(&tab, cx).as_deref(), Some(":"));
    let after = tab.code.read_with(cx, |c, _| c.scroll_y());
    assert!(
        after > before,
        "the code scrolled up by the line's row ({before} → {after}), the caret is not under it"
    );
}

#[gpui::test]
fn toggling_vim_in_settings_keeps_the_caret_and_closing_gives_the_code_the_keyboard(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "");
    let dir = tempfile::tempdir().unwrap();
    let tab = open_text(&view, cx, dir.path(), "a.txt", "one two\nthree\n");
    let head =
        |cx: &mut VisualTestContext| tab.code.read_with(cx, |c, _| c.selections().primary().head);
    let focused = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| tab.code.read(cx).focus_handle(cx).is_focused(window))
    };
    cx.simulate_keystrokes("right right");
    assert_eq!(head(cx), 2);
    assert!(focused(cx));

    // Vim on from the sheet: Normal mode, the caret where it was (Col 3).
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenSettingsFor(
        "editor".into(),
    ));
    draw(cx);
    view.update(cx, |wb, cx| wb.sheet_toggle("vim_mode", cx));
    draw(cx);
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Normal));
    assert_eq!(head(cx), 2, "toggling Vim moved the caret");
    // Escape closes the sheet; the code has the keyboard (its caret is drawn) and takes keys.
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_none()));
    assert!(focused(cx), "the code has the keyboard back");
    cx.simulate_input("x");
    assert_eq!(text(&tab.buffer, cx), "on two\nthree\n");

    // And off again, opened the way the sidebar's Settings button does it (its click moved the
    // keyboard to the sidebar): the caret stays, and the code gets the keyboard back.
    view.update_in(cx, |wb, window, cx| window.focus(&wb.focus_handle(cx), cx));
    assert!(!focused(cx));
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenSettingsFor(
        "editor".into(),
    ));
    draw(cx);
    view.update(cx, |wb, cx| wb.sheet_toggle("vim_mode", cx));
    draw(cx);
    assert_eq!(file_mode(&tab, cx), None);
    assert_eq!(head(cx), 2);
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(focused(cx));
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

// ---- precedence: the `vim_mode` contexts -------------------------------------------------------

#[gpui::test]
fn the_overlays_carry_vim_mode_in_their_key_context(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    // ⌃Y closes the palette only in Normal mode (a binding the user scoped to it).
    std::fs::write(
        daemon.home().join("keymap.toml"),
        "[[binding]]\nkeys = \"ctrl-y\"\naction = \"palette::Dismiss\"\ncontext = \"Palette && vim_mode == normal\"\n",
    )
    .unwrap();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("split");
    cx.simulate_keystrokes("ctrl-y");
    assert!(
        view.read_with(cx, |wb, _| wb.palette_open()),
        "Insert mode: the binding does not apply"
    );
    cx.simulate_keystrokes("escape");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.palette_vim_mode()),
        Some(Mode::Normal)
    );
    cx.simulate_keystrokes("ctrl-y");
    assert!(!view.read_with(cx, |wb, _| wb.palette_open()));
}

#[gpui::test]
fn a_keymap_binding_scoped_to_insert_mode_gets_an_editor_key_back(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    // The documented way back to the editor's ⌃A (line start) in Insert mode.
    std::fs::write(
        daemon.home().join("keymap.toml"),
        "[[binding]]\nkeys = \"ctrl-a\"\naction = \"code_view::MoveLineStart\"\ncontext = \"CodeView && vim_mode == insert\"\n",
    )
    .unwrap();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let tab = open_text(&view, cx, dir.path(), "a.txt", "one two\n");
    cx.simulate_input("A");
    cx.simulate_keystrokes("ctrl-a");
    cx.simulate_input("X");
    assert_eq!(text(&tab.buffer, cx), "Xone two\n");
    // In Normal mode ⌃A is still Vim's (increment; no number here, nothing changes).
    cx.simulate_keystrokes("escape ctrl-a");
    assert_eq!(text(&tab.buffer, cx), "Xone two\n");
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Normal));
}

// ---- Insert mode -------------------------------------------------------------------------------

#[gpui::test]
fn insert_mode_control_keys_edit_a_file_tab(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let tab = open_text(&view, cx, dir.path(), "i.txt", "foo bar\nxyz\n");

    // ⌃W stops once where the insert started, ⌃U deletes the line before the caret.
    cx.simulate_input("A baz");
    cx.simulate_keystrokes("ctrl-w");
    assert_eq!(text(&tab.buffer, cx), "foo bar \nxyz\n");
    cx.simulate_keystrokes("ctrl-w");
    assert_eq!(
        text(&tab.buffer, cx),
        "foo bar\nxyz\n",
        "stopped where `A` began"
    );
    cx.simulate_keystrokes("ctrl-w");
    assert_eq!(text(&tab.buffer, cx), "foo \nxyz\n");
    cx.simulate_keystrokes("ctrl-u");
    assert_eq!(text(&tab.buffer, cx), "\nxyz\n");
    // ⌃Y copies the character above, ⌃E the one below: back on line 2.
    cx.simulate_input("abc");
    cx.simulate_keystrokes("escape");
    cx.simulate_input("jI");
    cx.simulate_keystrokes("ctrl-y ctrl-y");
    assert_eq!(text(&tab.buffer, cx), "abc\nabxyz\n");
    // ⌃H is Backspace, ⌃J breaks the line.
    cx.simulate_keystrokes("ctrl-h ctrl-j");
    assert_eq!(text(&tab.buffer, cx), "abc\na\nxyz\n");
    // ⌃T / ⌃D indent and dedent the line (the file's indent: here a tab).
    cx.simulate_keystrokes("ctrl-t");
    assert!(
        text(&tab.buffer, cx).contains("\n\txyz\n")
            || text(&tab.buffer, cx).contains("\n    xyz\n"),
        "{:?}",
        text(&tab.buffer, cx)
    );
    cx.simulate_keystrokes("ctrl-d");
    assert_eq!(text(&tab.buffer, cx), "abc\na\nxyz\n");
    // ⌃O runs one Normal command and comes back to Insert: `$`, then typing at the end.
    cx.simulate_keystrokes("ctrl-o");
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Normal));
    cx.simulate_input("$");
    assert_eq!(file_mode(&tab, cx), Some(VimMode::Insert));
    cx.simulate_input("!");
    assert_eq!(text(&tab.buffer, cx), "abc\na\nxyz!\n");
    // ⌃R {reg} inserts a register, ⌃A the last inserted text.
    cx.simulate_keystrokes("escape");
    cx.simulate_input("ggyiwGA ");
    cx.simulate_keystrokes("ctrl-r");
    cx.simulate_input("\"");
    assert_eq!(text(&tab.buffer, cx), "abc\na\nxyz! abc\n");
    cx.simulate_keystrokes("escape");
    cx.simulate_input("ggA");
    cx.simulate_keystrokes("ctrl-a");
    assert_eq!(text(&tab.buffer, cx), "abc abc\na\nxyz! abc\n");
}

#[gpui::test]
fn insert_mode_control_keys_edit_the_composer(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (_view, chat, model, cx) = open_chat(cx, &daemon, "[editor]\nvim_mode = true\n", info.id);

    cx.simulate_input("hello big world");
    cx.simulate_keystrokes("ctrl-w");
    assert_eq!(draft(&model, cx), "hello big ");
    cx.simulate_keystrokes("ctrl-h ctrl-u");
    assert_eq!(draft(&model, cx), "");
    cx.simulate_input("one two");
    cx.simulate_keystrokes("ctrl-o");
    cx.simulate_input("0");
    assert_eq!(chat_mode(&chat, cx), Some(Mode::Insert));
    cx.simulate_input(">");
    assert_eq!(draft(&model, cx), ">one two");
}

// ---- Normal mode -------------------------------------------------------------------------------

fn panes(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> usize {
    draw(cx);
    view.read_with(cx, |wb, _| wb.state().panes().len())
}

fn active_pane(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, _| format!("{:?}", wb.state().active_pane_id()))
}

#[gpui::test]
fn ctrl_w_drives_the_panes(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let _tab = open_text(&view, cx, dir.path(), "w.txt", "one\ntwo\n");
    let first = panes(&view, cx);

    // ⌃W v splits beside, ⌃W s below; the new pane has the keyboard.
    let left = active_pane(&view, cx);
    cx.simulate_keystrokes("ctrl-w");
    cx.simulate_input("v");
    assert_eq!(panes(&view, cx), first + 1);
    let right = active_pane(&view, cx);
    assert_ne!(left, right);
    // ⌃W h / ⌃W l walk between them.
    cx.simulate_keystrokes("ctrl-w");
    cx.simulate_input("h");
    draw(cx);
    assert_eq!(active_pane(&view, cx), left);
    cx.simulate_keystrokes("ctrl-w ctrl-l");
    draw(cx);
    assert_eq!(active_pane(&view, cx), right);
    cx.simulate_keystrokes("ctrl-w");
    cx.simulate_input("s");
    assert_eq!(panes(&view, cx), first + 2);
    // ⌃W w cycles, ⌃W = evens the splits out (nothing to see but no harm).
    cx.simulate_keystrokes("ctrl-w");
    cx.simulate_input("w");
    draw(cx);
    cx.simulate_keystrokes("ctrl-w");
    cx.simulate_input("=");
    // ⌃W q closes the file's tab in the pane that has the keyboard, as `:q` does.
    cx.simulate_keystrokes("ctrl-w");
    cx.simulate_input("q");
    assert_eq!(panes(&view, cx), first + 1);
    // The file's code still takes Vim keys afterwards.
    let tab = view
        .read_with(cx, |wb, _| wb.focused_file_view())
        .expect("a file has the keyboard");
    let code = tab.read_with(cx, |v, _| v.code().clone());
    assert_eq!(
        code.read_with(cx, |c, _| c.vim_mode()),
        Some(VimMode::Normal)
    );
}

#[gpui::test]
fn normal_mode_control_keys_in_a_file_tab(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _settings) = open(cx, &daemon, "[editor]\nvim_mode = true\n");
    let dir = tempfile::tempdir().unwrap();
    let mut long = String::from("x 7 y\n");
    for i in 2..=200 {
        long.push_str(&format!("line {i}\n"));
    }
    let tab = open_text(&view, cx, dir.path(), "n.txt", &long);
    let line = |cx: &mut VisualTestContext| {
        tab.code.read_with(cx, |c, cx| {
            let head = c.selections().primary().head;
            c.buffer().read(cx).text().line_of_byte(head).unwrap()
        })
    };

    // ⌃A / ⌃X on the number after the caret, with a count.
    cx.simulate_keystrokes("ctrl-a");
    assert!(text(&tab.buffer, cx).starts_with("x 8 y\n"));
    cx.simulate_input("5");
    cx.simulate_keystrokes("ctrl-x");
    assert!(text(&tab.buffer, cx).starts_with("x 3 y\n"));
    // ⌃D / ⌃U move by half the window and scroll it; ⌃E scrolls a line.
    let scroll = tab.code.read_with(cx, |c, _| c.scroll_y());
    cx.simulate_keystrokes("ctrl-d");
    draw(cx);
    let down = line(cx);
    assert!(down > 3, "half a window down: line {down}");
    assert!(
        tab.code.read_with(cx, |c, _| c.scroll_y()) > scroll,
        "the view scrolled"
    );
    cx.simulate_keystrokes("ctrl-u");
    draw(cx);
    assert_eq!(line(cx), 0);
    cx.simulate_keystrokes("ctrl-f");
    draw(cx);
    assert!(line(cx) > down, "a page is more than half a window");
    cx.simulate_keystrokes("ctrl-b");
    draw(cx);
    // ⌃L redraws: nothing changes.
    let before = (line(cx), text(&tab.buffer, cx));
    cx.simulate_keystrokes("ctrl-l");
    assert_eq!((line(cx), text(&tab.buffer, cx)), before);
    // ⌃G shows the file and where the caret is.
    cx.simulate_input("gg");
    cx.simulate_keystrokes("ctrl-g");
    draw(cx);
    let shown = tab
        .view
        .read_with(cx, |v, _| v.command_bar_text().map(str::to_owned))
        .unwrap_or_default();
    assert!(
        shown.contains("n.txt") && shown.contains("[Modified]") && shown.contains("200 lines"),
        "{shown}"
    );

    // ⌃^ goes to the file shown before, and back.
    let other = open_text(&view, cx, dir.path(), "o.txt", "other\n");
    let focused = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view()
                .map(|v| v.read(cx).buffer().read(cx).path().display().to_string())
        })
    };
    let o_path = focused(cx).unwrap();
    cx.simulate_keystrokes("ctrl-^");
    wait_until(cx, "the alternate file", |cx| {
        focused(cx).is_some_and(|p| p.ends_with("n.txt"))
    });
    cx.simulate_keystrokes("ctrl-6");
    wait_until(cx, "back to o.txt", |cx| {
        focused(cx).as_deref() == Some(o_path.as_str())
    });
    drop(other);
}
