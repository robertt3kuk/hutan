#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Vim mode (`editor.vim_mode`, V-1) in GPUI: the chat composer against an in-process daemon
//! (`hutan-codex-fixture replay`), the command palette's filter without one, and the ⌘K toggle.
//! With the setting off nothing changes (`chat_gpui.rs` and `workbench_gpui.rs` run that).

mod common;

use std::time::Duration;

use common::chat::ChatEnv;
use common::{TestDaemon, draw, offline_connection, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::settings::EditorSettings;
use hutan_desktop::workbench::actions::{self, palette};
use hutan_desktop::workbench::chat::vim::Mode;
use hutan_desktop::workbench::chat::{ChatModel, ChatView, LoadStatus};
use hutan_desktop::workbench::vim_ui::CaretShape;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

// ---- the composer ----------------------------------------------------------------------------

fn open_chat_with<'a>(
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

fn draft(model: &Entity<ChatModel>, cx: &VisualTestContext) -> String {
    model.read_with(cx, |m, _| m.input().text().to_owned())
}

fn mode(chat: &Entity<ChatView>, cx: &VisualTestContext) -> Option<Mode> {
    chat.read_with(cx, |c, cx| c.vim_mode(cx))
}

fn pill_shown(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("vim-mode")))
        .is_some()
}

fn pump(
    cx: &mut VisualTestContext,
    what: &str,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    wait_until(cx, what, |cx| {
        cx.executor().advance_clock(Duration::from_millis(50));
        condition(cx)
    });
}

#[gpui::test]
fn vim_on_the_composer_starts_in_insert_and_escape_goes_to_normal(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (view, chat, model, cx) =
        open_chat_with(cx, &daemon, "[editor]\nvim_mode = true\n", info.id);

    // Typing is as ever; the composer starts in Insert with a bar caret and the pill says so.
    assert_eq!(mode(&chat, cx), Some(Mode::Insert));
    assert_eq!(
        chat.read_with(cx, |c, cx| c.vim_caret(cx)),
        Some(CaretShape::Bar)
    );
    assert!(pill_shown(&view, cx), "the INSERT pill is in the footer");
    cx.simulate_input("hello world");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("second line");
    assert_eq!(draft(&model, cx), "hello world\nsecond line");

    // Escape: Normal mode, a block caret, the caret steps back onto the last character.
    cx.simulate_keystrokes("escape");
    assert_eq!(mode(&chat, cx), Some(Mode::Normal));
    assert_eq!(
        chat.read_with(cx, |c, cx| c.vim_caret(cx)),
        Some(CaretShape::Block)
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.input().cursor()),
        "hello world\nsecond lin".len()
    );
    assert!(pill_shown(&view, cx), "the NORMAL pill is in the footer");
    assert_eq!(
        draft(&model, cx),
        "hello world\nsecond line",
        "Escape kept the text"
    );

    // Normal-mode commands: `dd` clears the line, `u` brings it back, `i` types again.
    cx.simulate_input("dd");
    assert_eq!(draft(&model, cx), "hello world");
    cx.simulate_input("u");
    assert_eq!(draft(&model, cx), "hello world\nsecond line");
    cx.simulate_input("gg");
    cx.simulate_input("0");
    cx.simulate_input("i");
    assert_eq!(mode(&chat, cx), Some(Mode::Insert));
    cx.simulate_input("X");
    assert_eq!(draft(&model, cx), "Xhello world\nsecond line");
    // Motions and operators with counts and words.
    cx.simulate_keystrokes("escape");
    cx.simulate_input("0dw");
    assert_eq!(draft(&model, cx), "world\nsecond line");
    cx.simulate_input("cwhi");
    cx.simulate_keystrokes("escape");
    assert_eq!(draft(&model, cx), "hi\nsecond line");
    cx.simulate_input("j0.");
    assert_eq!(
        draft(&model, cx),
        "hi\nhi line",
        "`.` repeats cw and its text"
    );
    // Visual mode.
    cx.simulate_input("0vwd");
    assert_eq!(draft(&model, cx), "hi\nine");
    cx.simulate_keystrokes("escape");
    assert_eq!(
        mode(&chat, cx),
        Some(Mode::Normal),
        "Escape in Normal changes nothing"
    );
}

#[gpui::test]
fn normal_mode_enter_sends_only_with_the_enter_send_key(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (_view, chat, model, cx) = open_chat_with(
        cx,
        &daemon,
        "[editor]\nvim_mode = true\n[chat]\nsend_key = \"cmd-enter\"\n",
        info.id,
    );
    cx.simulate_input("ping");
    cx.simulate_keystrokes("escape enter");
    assert_eq!(
        draft(&model, cx),
        "ping",
        "Enter is not a newline in Normal mode"
    );
    assert!(
        env.sent_texts().is_empty(),
        "and it does not send under the ⌘Return send key"
    );
    assert_eq!(mode(&chat, cx), Some(Mode::Normal));
    // ⌘↩ sends from any mode.
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the message to arrive", |_| env.sent_texts().len() == 1);
    assert_eq!(env.sent_texts(), ["ping"]);
    assert!(model.read_with(cx, |m, _| m.input().is_empty()));
    assert_eq!(
        mode(&chat, cx),
        Some(Mode::Insert),
        "a sent composer is back in Insert"
    );
}

#[gpui::test]
fn normal_mode_enter_sends_with_the_enter_send_key(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (_view, _chat, model, cx) = open_chat_with(
        cx,
        &daemon,
        "[editor]\nvim_mode = true\n[chat]\nsend_key = \"enter\"\n",
        info.id,
    );
    cx.simulate_input("pong");
    cx.simulate_keystrokes("escape enter");
    pump(cx, "the message to arrive", |_| env.sent_texts().len() == 1);
    assert_eq!(env.sent_texts(), ["pong"]);
    assert!(model.read_with(cx, |m, _| m.input().is_empty()));
}

#[gpui::test]
fn undo_and_redo_work_in_the_composer_without_vim(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (view, chat, model, cx) = open_chat_with(cx, &daemon, "", info.id);
    assert_eq!(mode(&chat, cx), None, "vim mode is off by default");
    assert!(!pill_shown(&view, cx), "no pill while it is off");
    cx.simulate_input("abc");
    cx.simulate_keystrokes("space");
    cx.simulate_input("def");
    cx.simulate_keystrokes("cmd-z");
    assert_eq!(draft(&model, cx), "", "typing is one undo step");
    cx.simulate_keystrokes("cmd-shift-z");
    assert_eq!(draft(&model, cx), "abc def");
    // Escape does nothing with vim off, and the keys type as they always did.
    cx.simulate_keystrokes("escape");
    cx.simulate_input("dd");
    assert_eq!(draft(&model, cx), "abc defdd");
}

#[gpui::test]
fn the_composer_follows_the_setting_live(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (view, chat, model, cx) = open_chat_with(cx, &daemon, "", info.id);
    cx.simulate_input("abc");
    view.update(cx, |wb, _| {
        wb.settings_store()
            .write_user("editor.vim_mode", toml::Value::Boolean(true))
            .unwrap();
    });
    assert_eq!(mode(&chat, cx), Some(Mode::Insert));
    cx.simulate_keystrokes("escape");
    cx.simulate_input("x");
    assert_eq!(draft(&model, cx), "ab");
}

// ---- the palette -----------------------------------------------------------------------------

fn open_offline(cx: &mut TestAppContext, vim: bool) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    if vim {
        settings
            .write_user("editor.vim_mode", toml::Value::Boolean(true))
            .unwrap();
    }
    open_with(cx, settings, connection, None)
}

fn palette_query(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.palette().map(|p| p.query.clone()))
}

#[gpui::test]
fn the_palette_filter_opens_in_insert_then_escape_goes_to_normal_then_closes(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open_offline(cx, true);
    cx.simulate_keystrokes("cmd-shift-p");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.palette_vim_mode()),
        Some(Mode::Insert)
    );
    let all = view.read_with(cx, |wb, _| wb.palette_titles().len());
    cx.simulate_input("split");
    assert_eq!(palette_query(&view, cx).as_deref(), Some("split"));
    let filtered = view.read_with(cx, |wb, _| wb.palette_titles());
    assert_eq!(filtered[0].as_ref(), "Layout: Split Right");

    // The first Escape leaves Insert and keeps the palette open.
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.palette_open()));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.palette_vim_mode()),
        Some(Mode::Normal)
    );
    // Normal-mode editing: `x` cuts the last letter (the caret stepped back onto it).
    cx.simulate_input("x");
    assert_eq!(palette_query(&view, cx).as_deref(), Some("spli"));
    // `dd` clears the line; the palette lists everything again; `i` types again.
    cx.simulate_input("dd");
    assert_eq!(palette_query(&view, cx).as_deref(), Some(""));
    assert_eq!(view.read_with(cx, |wb, _| wb.palette_titles().len()), all);
    cx.simulate_input("i");
    cx.simulate_input("dock");
    assert_eq!(palette_query(&view, cx).as_deref(), Some("dock"));
    // `u` undoes the typing session.
    cx.simulate_keystrokes("escape");
    cx.simulate_input("u");
    assert_eq!(palette_query(&view, cx).as_deref(), Some(""));
    // Enter in Normal mode is the palette's confirm.
    cx.simulate_keystrokes("i");
    cx.simulate_input("split right");
    cx.simulate_keystrokes("escape enter");
    assert!(
        !view.read_with(cx, |wb, _| wb.palette_open()),
        "Enter confirmed"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().panes().len()),
        2,
        "and ran Split Right"
    );

    // The second Escape closes, as it always did.
    cx.simulate_keystrokes("cmd-shift-p");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.palette_vim_mode()),
        Some(Mode::Insert),
        "each opening starts in Insert"
    );
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.palette_open()));
    cx.simulate_keystrokes("escape");
    assert!(!view.read_with(cx, |wb, _| wb.palette_open()));
    // A palette that was left in Normal mode opens in Insert again.
    cx.simulate_keystrokes("cmd-shift-p");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.palette_vim_mode()),
        Some(Mode::Insert)
    );
    cx.dispatch_action(palette::Dismiss);
    assert!(
        view.read_with(cx, |wb, _| wb.palette_open()),
        "Insert goes to Normal first"
    );
    cx.dispatch_action(palette::Dismiss);
    assert!(!view.read_with(cx, |wb, _| wb.palette_open()));
}

#[gpui::test]
fn with_vim_off_escape_closes_the_palette_at_once(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx, false);
    cx.simulate_keystrokes("cmd-shift-p");
    assert_eq!(view.read_with(cx, |wb, _| wb.palette_vim_mode()), None);
    cx.simulate_input("dd");
    assert_eq!(
        palette_query(&view, cx).as_deref(),
        Some("dd"),
        "typing is typing"
    );
    cx.simulate_keystrokes("escape");
    assert!(!view.read_with(cx, |wb, _| wb.palette_open()));
}

// ---- the toggle ------------------------------------------------------------------------------

#[gpui::test]
fn the_palette_command_toggles_vim_mode(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx, false);
    let on = |view: &Entity<Workbench>, cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| {
            wb.settings_store().get::<EditorSettings>().vim_mode
        })
    };
    assert!(!on(&view, cx));
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("toggle vim");
    let titles = view.read_with(cx, |wb, _| wb.palette_titles());
    assert_eq!(titles[0].as_ref(), "Editor: Toggle Vim Mode");
    cx.simulate_keystrokes("enter");
    assert!(on(&view, cx), "the command turned it on");
    cx.dispatch_action(actions::ToggleVimMode);
    assert!(!on(&view, cx), "and the action turns it off");
}
