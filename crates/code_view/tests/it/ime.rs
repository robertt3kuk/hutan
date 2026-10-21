//! The platform's text input in a code view (slice E-6, the TI-1 scenarios for the editor): an
//! input method composing Japanese in place (marked text, its candidate window under the
//! composition, the commit as one undo step), at several carets at once; a Cyrillic layout
//! whose key press carries the layout's letter while the text arrives through the input
//! handler; and macOS press-and-hold, which replaces the typed letter with an accented one.

use gpui::{EntityInputHandler as _, KeyDownEvent, Keystroke, Modifiers, TestAppContext};

use crate::editing::{draw, open, place, ranges, run, text_of};
use hutan_code_view::actions as a;

#[gpui::test]
fn japanese_composition_is_marked_in_place_and_commits_as_one_step(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "notes.md", "# 日記\nab\n", false);
    // After `ab`: UTF-16 offset 7 (`# 日記\n` is 5 units, `ab` 2).
    place(&view, cx, &[11]);
    let mark = |cx: &mut gpui::VisualTestContext, text: &str, selected: std::ops::Range<usize>| {
        view.update_in(cx, |v, window, cx| {
            v.replace_and_mark_text_in_range(None, text, Some(selected), window, cx);
        });
        draw(cx);
    };
    mark(cx, "k", 1..1);
    mark(cx, "か", 1..1);
    mark(cx, "かな", 2..2);
    assert_eq!(text_of(&buffer, cx), "# 日記\nabかな\n");
    let marked = view.update_in(cx, |v, window, cx| v.marked_text_range(window, cx));
    assert_eq!(marked, Some(7..9), "the composition is marked, in UTF-16");
    assert_eq!(
        ranges(&view, cx),
        vec![17..17],
        "the caret is where the IME put it"
    );

    // The candidate window goes under the composition, on its line.
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    let bounds = view
        .update_in(cx, |v, window, cx| {
            v.bounds_for_range(7..9, layout.bounds, window, cx)
        })
        .expect("bounds of the marked text");
    let near = |a: gpui::Pixels, b: gpui::Pixels| (f32::from(a) - f32::from(b)).abs() < 0.01;
    assert!(near(
        bounds.origin.y,
        layout.bounds.top() + layout.line_height
    ));
    assert!(near(
        bounds.origin.x,
        layout.text_left + layout.cell_width * 2.0
    ));
    assert!(
        near(bounds.size.width, layout.cell_width * 4.0),
        "two wide characters"
    );

    // Converting selects the whole composition; Return-like commit replaces it.
    mark(cx, "仮名", 0..2);
    assert_eq!(ranges(&view, cx), vec![11..17]);
    view.update_in(cx, |v, window, cx| {
        v.replace_text_in_range(None, "仮名", window, cx);
    });
    draw(cx);
    assert_eq!(text_of(&buffer, cx), "# 日記\nab仮名\n");
    assert_eq!(
        view.update_in(cx, |v, window, cx| v.marked_text_range(window, cx)),
        None
    );
    assert_eq!(ranges(&view, cx), vec![17..17]);
    run(cx, a::Undo);
    assert_eq!(
        text_of(&buffer, cx),
        "# 日記\nab\n",
        "the whole composition is one undo step"
    );
}

#[gpui::test]
fn return_and_escape_end_a_composition_without_doing_anything_else(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", "x\n", false);
    place(&view, cx, &[1]);
    view.update_in(cx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "に", None, window, cx);
    });
    run(cx, a::Newline);
    assert_eq!(text_of(&buffer, cx), "xに\n", "no line break");
    assert!(view.read_with(cx, |v, _| v.marked_ranges().is_empty()));
    view.update_in(cx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "ほ", None, window, cx);
    });
    run(cx, a::Cancel);
    assert_eq!(text_of(&buffer, cx), "xにほ\n");
    assert!(view.read_with(cx, |v, _| v.marked_ranges().is_empty()));
}

#[gpui::test]
fn a_composition_goes_in_at_every_caret(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", "a\nb\n", false);
    place(&view, cx, &[1, 3]);
    view.update_in(cx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "ㅎ", None, window, cx);
    });
    view.update_in(cx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "한", None, window, cx);
    });
    assert_eq!(text_of(&buffer, cx), "a한\nb한\n");
    assert_eq!(view.read_with(cx, |v, _| v.marked_ranges().len()), 2);
    view.update_in(cx, |v, window, cx| {
        v.replace_text_in_range(None, "한", window, cx)
    });
    assert_eq!(text_of(&buffer, cx), "a한\nb한\n");
    assert_eq!(ranges(&view, cx), vec![4..4, 9..9]);
}

#[gpui::test]
fn a_cyrillic_layout_types_what_it_shows(cx: &mut TestAppContext) {
    let (_view, buffer, cx) = open(cx, "a.txt", "", false);
    // The key press carries the key's layout letter; the text arrives through the handler.
    cx.simulate_event(KeyDownEvent {
        keystroke: Keystroke {
            modifiers: Modifiers::default(),
            key: "a".into(),
            key_char: Some("ф".into()),
        },
        is_held: false,
        prefer_character_input: false,
    });
    assert_eq!(
        text_of(&buffer, cx),
        "",
        "a key press alone inserts nothing"
    );
    cx.simulate_input("фыв");
    cx.simulate_input("ё");
    assert_eq!(text_of(&buffer, cx), "фывё");
    run(cx, a::DeleteBackward);
    assert_eq!(text_of(&buffer, cx), "фыв", "one character, not one byte");
}

#[gpui::test]
fn press_and_hold_replaces_the_letter_with_its_accent(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", "caf\n", false);
    place(&view, cx, &[3]);
    cx.simulate_input("e");
    assert_eq!(text_of(&buffer, cx), "cafe\n");
    // The accent menu picks é: the platform replaces the `e` (UTF-16 3..4).
    view.update_in(cx, |v, window, cx| {
        v.replace_text_in_range(Some(3..4), "é", window, cx);
    });
    assert_eq!(text_of(&buffer, cx), "café\n");
    assert_eq!(ranges(&view, cx), vec![5..5], "after the two-byte é");
    run(cx, a::Undo);
    assert_eq!(
        text_of(&buffer, cx),
        "caf\n",
        "the letter and its accent are one step"
    );
}
