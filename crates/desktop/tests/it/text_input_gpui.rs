#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! TI-1: text reaches Hutan's fields through the platform's text input handler, so synthesized
//! Unicode (Cyrillic, CJK, emoji) lands in the field as typed, a plain key press inserts once,
//! and the editing shortcuts (⌘A, ⌘C, ⌘V) work in the Jump / command palette query.

use crate::common;

use common::{offline_connection, open_with};
use gpui::{
    ClipboardItem, Entity, KeyDownEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext,
};
use hutan_desktop::workbench::Workbench;

fn open(cx: &mut TestAppContext) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    open_with(cx, settings, connection, None)
}

fn query(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, _| {
        wb.palette().map(|p| p.query.clone()).unwrap_or_default()
    })
}

#[gpui::test]
fn unicode_text_lands_in_the_palette_as_typed(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    assert!(view.read_with(cx, |wb, _| wb.palette_open()));

    // The characters of the text, not the layout's letter for the key code ("ффф").
    cx.simulate_input("ффф");
    assert_eq!(query(&view, cx), "ффф");
    cx.simulate_input("日本語");
    cx.simulate_input("😀");
    assert_eq!(query(&view, cx), "ффф日本語😀");

    // Backspace removes one character (an emoji is one), ⌘A then typing replaces everything.
    cx.simulate_keystrokes("backspace");
    assert_eq!(query(&view, cx), "ффф日本語");
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("é");
    assert_eq!(query(&view, cx), "é");
}

#[gpui::test]
fn a_plain_key_press_inserts_once(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_keystrokes("s p l");
    assert_eq!(query(&view, cx), "spl");
    cx.simulate_keystrokes("left left");
    cx.simulate_input("X");
    assert_eq!(
        query(&view, cx),
        "sXpl",
        "the caret moves and the text goes in at it"
    );
}

#[gpui::test]
fn select_all_copy_and_paste_work_in_the_jump_query(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("split");
    cx.simulate_keystrokes("cmd-a cmd-c");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("split".to_owned())
    );
    cx.write_to_clipboard(ClipboardItem::new_string("a\nb".to_owned()));
    cx.simulate_keystrokes("cmd-a cmd-v");
    assert_eq!(
        query(&view, cx),
        "a",
        "a one-line field takes the first line of a paste"
    );
}

/// The platform's key-down carries the key's layout letter as `key_char` ("ф" on the Russian
/// layout, whatever string a driver synthesized); the text arrives afterwards through the input
/// handler (`insertText:`). Only the handler's text may land.
#[gpui::test]
fn only_the_input_handlers_text_lands_not_the_layouts_key_char(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_event(KeyDownEvent {
        keystroke: Keystroke {
            modifiers: Modifiers::default(),
            key: "a".into(),
            key_char: Some("ф".into()),
        },
        is_held: false,
        prefer_character_input: false,
    });
    assert_eq!(query(&view, cx), "", "a key press alone inserts nothing");
    // What the platform then commits for that key press.
    cx.simulate_input("п");
    assert_eq!(query(&view, cx), "п");
}
