//! Editing in a code view (slice E-6), in a GPUI test window: typing through the platform's text
//! input with auto-pairs and auto-indent, multi-cursor edits as one undo step, ⌥-click and
//! column selection, replace, the clipboard, line commands, what a keystroke reshapes, and that
//! the parser follows every edit incrementally.

use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    Action, AppContext as _, ClipboardItem, Entity, Focusable as _, Modifiers, MouseButton,
    TestAppContext, VisualTestContext, point, px, size,
};
use hutan_buffer::{Selection, Selections};
use hutan_code_view::{CodeStyle, CodeView, FileBuffer, FindQuery, actions as a};
use hutan_settings::themes::{Builtin, builtin_theme};

pub(crate) fn style() -> CodeStyle {
    CodeStyle::new(
        builtin_theme(Builtin::HutanDark).editor(),
        "Menlo",
        13.0,
        1.4,
    )
}

pub(crate) fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.run_until_parked();
}

/// A focused view of a `name` file holding `text` in a 900 × 600 window.
pub(crate) fn open<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    text: &str,
    syntax: bool,
) -> (
    Entity<CodeView>,
    Entity<FileBuffer>,
    &'a mut VisualTestContext,
) {
    cx.executor().allow_parking();
    let text = text.to_owned();
    let buffer = cx.new(|cx| FileBuffer::new(PathBuf::from(format!("/virtual/{name}")), cx));
    let shared = buffer.clone();
    let (view, cx) = cx.add_window_view(move |_, cx| CodeView::new(shared, style(), cx));
    cx.simulate_resize(size(px(900.0), px(600.0)));
    buffer.update(cx, |buffer, cx| {
        buffer.set_loaded(&text, false, cx);
        buffer.set_syntax_active(syntax, cx);
    });
    cx.update(|window, cx| {
        let focus = view.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    });
    draw(cx);
    (view, buffer, cx)
}

pub(crate) fn text_of(buffer: &Entity<FileBuffer>, cx: &VisualTestContext) -> String {
    buffer.read_with(cx, |b, _| b.text().to_string())
}

pub(crate) fn ranges(view: &Entity<CodeView>, cx: &VisualTestContext) -> Vec<Range<usize>> {
    view.read_with(cx, |v, _| {
        v.selections().iter().map(|s| s.range()).collect()
    })
}

pub(crate) fn place(view: &Entity<CodeView>, cx: &mut VisualTestContext, carets: &[usize]) {
    let selections = Selections::new(carets.iter().map(|c| Selection::cursor(*c)), 0).unwrap();
    view.update(cx, |v, cx| v.set_selections(selections, cx));
    draw(cx);
}

pub(crate) fn run(cx: &mut VisualTestContext, action: impl Action) {
    cx.dispatch_action(action);
    draw(cx);
}

#[gpui::test]
fn typing_pairs_brackets_steps_over_closers_and_indents_a_new_block(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.rs", "", false);
    cx.simulate_input("fn f(");
    assert_eq!(text_of(&buffer, cx), "fn f()");
    assert_eq!(
        ranges(&view, cx),
        vec![5..5],
        "the caret sits between the pair"
    );
    cx.simulate_input("x)");
    assert_eq!(
        text_of(&buffer, cx),
        "fn f(x)",
        "the typed closer stepped over"
    );
    cx.simulate_input(" {");
    assert_eq!(text_of(&buffer, cx), "fn f(x) {}");
    run(cx, a::Newline);
    assert_eq!(
        text_of(&buffer, cx),
        "fn f(x) {\n    \n}",
        "the closer goes to a line of its own"
    );
    assert_eq!(ranges(&view, cx), vec![14..14]);
    cx.simulate_input("let s = \"");
    assert_eq!(text_of(&buffer, cx), "fn f(x) {\n    let s = \"\"\n}");
    // Backspace inside an empty pair takes both halves.
    run(cx, a::DeleteBackward);
    assert_eq!(text_of(&buffer, cx), "fn f(x) {\n    let s = \n}");
    // Backspace in leading spaces goes back to the previous stop.
    place(&view, cx, &[14]);
    run(cx, a::DeleteBackward);
    assert_eq!(text_of(&buffer, cx), "fn f(x) {\nlet s = \n}");
    // A bracket typed around a selection surrounds it.
    view.update(cx, |v, cx| v.select(14..15, cx));
    cx.simulate_input("[");
    assert_eq!(text_of(&buffer, cx), "fn f(x) {\nlet [s] = \n}");
    assert_eq!(
        ranges(&view, cx),
        vec![15..16],
        "the selection stays on the text"
    );
}

#[gpui::test]
fn a_closing_bracket_on_a_blank_line_outdents_it(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.rs", "fn f() {\n    x\n    \n", false);
    place(&view, cx, &[19]);
    cx.simulate_input("}");
    assert_eq!(text_of(&buffer, cx), "fn f() {\n    x\n}\n");
}

#[gpui::test]
fn return_indents_as_the_syntax_tree_says(cx: &mut TestAppContext) {
    let source = "fn main() {\n    if x {\n        y();\n    }\n}\n";
    let (view, buffer, cx) = open(cx, "main.rs", source, true);
    assert!(buffer.read_with(cx, |b, _| b.wait_for_syntax(Duration::from_secs(30))));
    // After `y();`: two levels.
    place(&view, cx, &[35]);
    run(cx, a::Newline);
    assert_eq!(
        text_of(&buffer, cx),
        "fn main() {\n    if x {\n        y();\n        \n    }\n}\n"
    );
    // After the inner `}`: one level, whatever the line before had.
    assert!(buffer.read_with(cx, |b, _| b.wait_for_syntax(Duration::from_secs(30))));
    let after_brace = text_of(&buffer, cx).find("    }\n}").unwrap() + 5;
    place(&view, cx, &[after_brace]);
    run(cx, a::Newline);
    assert!(
        text_of(&buffer, cx).contains("    }\n    \n}"),
        "{:?}",
        text_of(&buffer, cx)
    );
}

#[gpui::test]
fn a_multi_cursor_edit_is_one_undo_step(cx: &mut TestAppContext) {
    let source = "let a = foo;\nlet b = foo;\nlet c = foo;\nlet food = 1;\n";
    let (view, buffer, cx) = open(cx, "a.rs", source, false);
    place(&view, cx, &[9]);
    // ⌘D selects the word, then each next whole-word occurrence (never `food`).
    run(cx, a::SelectNextOccurrence);
    run(cx, a::SelectNextOccurrence);
    run(cx, a::SelectNextOccurrence);
    run(cx, a::SelectNextOccurrence);
    assert_eq!(ranges(&view, cx), vec![8..11, 21..24, 34..37]);
    cx.simulate_input("bar");
    let edited = "let a = bar;\nlet b = bar;\nlet c = bar;\nlet food = 1;\n";
    assert_eq!(text_of(&buffer, cx), edited);
    assert_eq!(ranges(&view, cx), vec![11..11, 24..24, 37..37]);
    assert!(buffer.read_with(cx, |b, _| b.is_dirty()));

    run(cx, a::Undo);
    assert_eq!(
        text_of(&buffer, cx),
        source,
        "one step undoes every caret's typing"
    );
    assert_eq!(
        ranges(&view, cx),
        vec![8..11, 21..24, 34..37],
        "the selections come back"
    );
    assert!(
        !buffer.read_with(cx, |b, _| b.is_dirty()),
        "back at the saved text"
    );
    run(cx, a::Redo);
    assert_eq!(text_of(&buffer, cx), edited);

    // Escape goes back to one caret.
    run(cx, a::Cancel);
    assert_eq!(view.read_with(cx, |v, _| v.selections().len()), 1);
}

#[gpui::test]
fn select_all_occurrences_takes_every_one(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", "x y x z x\n", false);
    place(&view, cx, &[4]);
    run(cx, a::SelectAllOccurrences);
    assert_eq!(ranges(&view, cx), vec![0..1, 4..5, 8..9]);
    cx.simulate_input("ab");
    assert_eq!(text_of(&buffer, cx), "ab y ab z ab\n");
}

#[gpui::test]
fn alt_click_adds_carets_and_alt_shift_drag_selects_columns(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", "aaaa\nbbbb\ncccc\n", false);
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    let at = |col: f32, line: f32| {
        point(
            layout.text_left + layout.cell_width * col,
            layout.bounds.top() + layout.line_height * (line + 0.5),
        )
    };
    cx.simulate_click(at(1.0, 0.0), Modifiers::none());
    cx.simulate_click(at(1.0, 2.0), Modifiers::alt());
    assert_eq!(ranges(&view, cx), vec![1..1, 11..11]);
    // ⌥-clicking a caret takes it away again.
    cx.simulate_click(at(1.0, 2.0), Modifiers::alt());
    assert_eq!(ranges(&view, cx), vec![1..1]);
    cx.simulate_click(at(1.0, 2.0), Modifiers::alt());
    cx.simulate_input("X");
    assert_eq!(text_of(&buffer, cx), "aXaaa\nbbbb\ncXccc\n");

    // A block of two columns on three lines.
    let block = Modifiers {
        alt: true,
        shift: true,
        ..Modifiers::default()
    };
    cx.simulate_mouse_down(at(0.0, 0.0), MouseButton::Left, block);
    cx.simulate_mouse_move(at(2.0, 2.0), MouseButton::Left, block);
    cx.simulate_mouse_up(at(2.0, 2.0), MouseButton::Left, block);
    assert_eq!(ranges(&view, cx), vec![0..2, 6..8, 11..13]);
    cx.simulate_input("-");
    assert_eq!(text_of(&buffer, cx), "-aaa\n-bb\n-ccc\n");
    run(cx, a::Undo);
    assert_eq!(text_of(&buffer, cx), "aXaaa\nbbbb\ncXccc\n");
}

#[gpui::test]
fn carets_move_by_characters_words_lines_and_keep_their_column(cx: &mut TestAppContext) {
    let (view, _buffer, cx) = open(cx, "a.txt", "alpha beta\nx\ngamma delta\n", false);
    place(&view, cx, &[8]);
    run(cx, a::MoveDown);
    assert_eq!(ranges(&view, cx), vec![12..12], "the end of the short line");
    run(cx, a::MoveDown);
    assert_eq!(
        ranges(&view, cx),
        vec![21..21],
        "the column it wanted, back"
    );
    run(cx, a::MoveWordLeft);
    assert_eq!(ranges(&view, cx), vec![19..19]);
    run(cx, a::SelectWordRight);
    assert_eq!(ranges(&view, cx), vec![19..24]);
    run(cx, a::MoveLeft);
    assert_eq!(ranges(&view, cx), vec![19..19], "← collapses to the start");
    run(cx, a::MoveLineStart);
    assert_eq!(ranges(&view, cx), vec![13..13]);
    run(cx, a::MoveLineEnd);
    assert_eq!(ranges(&view, cx), vec![24..24]);
    run(cx, a::MoveToStart);
    assert_eq!(ranges(&view, cx), vec![0..0]);
    run(cx, a::SelectToEnd);
    assert_eq!(ranges(&view, cx), vec![0..25]);
}

#[gpui::test]
fn replace_all_is_one_undo_step_and_replace_moves_on(cx: &mut TestAppContext) {
    let source = "foo(1); foo(2);\nfoo(3);\n";
    let (view, buffer, cx) = open(cx, "a.rs", source, false);
    view.update(cx, |v, cx| v.set_find_query(FindQuery::literal("foo"), cx));
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |v, _| v.find_state().summary()),
        "1 of 3"
    );
    assert!(view.update(cx, |v, cx| v.replace_current("bar", cx)));
    cx.run_until_parked();
    assert_eq!(text_of(&buffer, cx), "bar(1); foo(2);\nfoo(3);\n");
    assert_eq!(
        view.read_with(cx, |v, _| v.find_state().summary()),
        "1 of 2",
        "the next match is current"
    );
    assert_eq!(ranges(&view, cx), vec![8..11], "and selected");

    let replaced = view.update(cx, |v, cx| v.replace_all("baz", cx));
    assert_eq!(replaced, 2);
    assert_eq!(text_of(&buffer, cx), "bar(1); baz(2);\nbaz(3);\n");
    run(cx, a::Undo);
    assert_eq!(
        text_of(&buffer, cx),
        "bar(1); foo(2);\nfoo(3);\n",
        "one step undoes every replacement"
    );
    run(cx, a::Undo);
    assert_eq!(text_of(&buffer, cx), source);
}

#[gpui::test]
fn a_keystroke_shapes_only_the_line_it_changed(cx: &mut TestAppContext) {
    let source: String = (0..50_000)
        .map(|n| format!("line {n} of the file\n"))
        .collect();
    let (view, buffer, cx) = open(cx, "big.txt", &source, false);
    let at = buffer.read_with(cx, |b, _| b.text().line_start(10).unwrap());
    place(&view, cx, &[at]);
    let before = view.read_with(cx, |v, _| v.stats().lines_shaped);
    draw(cx);
    assert_eq!(
        view.read_with(cx, |v, _| v.stats().lines_shaped),
        before,
        "idle"
    );
    cx.simulate_input("x");
    draw(cx);
    let (shaped, lines) = view.read_with(cx, |v, _| {
        (v.stats().lines_shaped - before, v.stats().last_lines.len())
    });
    assert!(lines > 10);
    assert_eq!(shaped, 1, "only the edited line is shaped again");
}

#[gpui::test]
fn edits_reach_the_parser_incrementally(cx: &mut TestAppContext) {
    let source: String = (0..2_000)
        .map(|n| format!("pub fn item_{n}(x: u32) -> u32 {{\n    x + {n}\n}}\n"))
        .collect();
    let (view, buffer, cx) = open(cx, "lib.rs", &source, true);
    assert!(buffer.read_with(cx, |b, _| b.wait_for_syntax(Duration::from_secs(30))));
    place(&view, cx, &[3]);
    for _ in 0..5 {
        cx.simulate_input("z");
        assert!(buffer.read_with(cx, |b, _| b.wait_for_syntax(Duration::from_secs(30))));
    }
    run(cx, a::Undo);
    assert!(buffer.read_with(cx, |b, _| b.wait_for_syntax(Duration::from_secs(30))));
    let stats = buffer.read_with(cx, |b, _| b.syntax_stats().unwrap());
    assert!(stats.completed >= 2);
    assert_eq!(
        stats.started - stats.incremental,
        1,
        "only the first parse starts from nothing: {stats:?}"
    );
    let snapshot = buffer.read_with(cx, |b, _| b.syntax_snapshot().unwrap());
    assert_eq!(snapshot.text().len(), source.len());
    assert!(!snapshot.has_errors());
}

#[gpui::test]
fn another_view_keeps_its_caret_on_its_text(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", "one two three\n", false);
    let other = cx.new(|cx| CodeView::new(buffer.clone(), style(), cx));
    other.update(cx, |v, cx| v.set_selections(Selections::cursor(8), cx));
    place(&view, cx, &[0]);
    cx.simulate_input("zero ");
    assert_eq!(text_of(&buffer, cx), "zero one two three\n");
    assert_eq!(
        other.read_with(cx, |v, _| v.selections().primary().head),
        13,
        "still before `three`"
    );
}

#[gpui::test]
fn copy_and_paste_follow_the_carets(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", "a1\nb2\nc3\n", false);
    // Three selections copied, pasted at three carets: one piece each.
    let three = Selections::new(
        [
            Selection::new(0, 1),
            Selection::new(3, 4),
            Selection::new(6, 7),
        ],
        0,
    )
    .unwrap();
    view.update(cx, |v, cx| v.set_selections(three, cx));
    run(cx, a::Copy);
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("a\nb\nc")
    );
    place(&view, cx, &[2, 5, 8]);
    run(cx, a::Paste);
    assert_eq!(text_of(&buffer, cx), "a1a\nb2b\nc3c\n");

    // Without a selection, copy takes the line and paste puts it above the caret's line.
    place(&view, cx, &[1]);
    run(cx, a::Copy);
    place(&view, cx, &[9]);
    run(cx, a::Paste);
    assert_eq!(text_of(&buffer, cx), "a1a\nb2b\na1a\nc3c\n");
    // Text from elsewhere goes in at every caret.
    cx.write_to_clipboard(ClipboardItem::new_string("Q\r\n".to_owned()));
    place(&view, cx, &[0, 4]);
    run(cx, a::Paste);
    assert_eq!(text_of(&buffer, cx), "Q\na1a\nQ\nb2b\na1a\nc3c\n");
    // Cut without a selection takes the line.
    place(&view, cx, &[0]);
    run(cx, a::Cut);
    assert_eq!(text_of(&buffer, cx), "a1a\nQ\nb2b\na1a\nc3c\n");
}

#[gpui::test]
fn line_commands_comment_duplicate_move_and_indent(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.rs", "fn a() {\n    x();\n    y();\n}\n", false);
    view.update(cx, |v, cx| v.select(13..25, cx));
    run(cx, a::ToggleComment);
    assert_eq!(
        text_of(&buffer, cx),
        "fn a() {\n    // x();\n    // y();\n}\n"
    );
    run(cx, a::ToggleComment);
    assert_eq!(text_of(&buffer, cx), "fn a() {\n    x();\n    y();\n}\n");

    place(&view, cx, &[14]);
    run(cx, a::DuplicateLineDown);
    assert_eq!(
        text_of(&buffer, cx),
        "fn a() {\n    x();\n    x();\n    y();\n}\n"
    );
    assert_eq!(
        ranges(&view, cx),
        vec![23..23],
        "the caret goes with the copy"
    );
    run(cx, a::MoveLineDown);
    assert_eq!(
        text_of(&buffer, cx),
        "fn a() {\n    x();\n    y();\n    x();\n}\n"
    );
    run(cx, a::MoveLineUp);
    run(cx, a::MoveLineUp);
    assert_eq!(
        text_of(&buffer, cx),
        "fn a() {\n    x();\n    x();\n    y();\n}\n"
    );
    assert_eq!(ranges(&view, cx), vec![14..14]);
    run(cx, a::DeleteLine);
    assert_eq!(text_of(&buffer, cx), "fn a() {\n    x();\n    y();\n}\n");

    run(cx, a::Indent);
    assert_eq!(
        text_of(&buffer, cx),
        "fn a() {\n        x();\n    y();\n}\n"
    );
    run(cx, a::Outdent);
    run(cx, a::Outdent);
    assert_eq!(text_of(&buffer, cx), "fn a() {\nx();\n    y();\n}\n");
    run(cx, a::Undo);
    run(cx, a::Undo);
    run(cx, a::Undo);
    assert_eq!(text_of(&buffer, cx), "fn a() {\n    x();\n    y();\n}\n");
}

#[gpui::test]
fn a_large_file_is_read_only(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", "", false);
    buffer.update(cx, |b, cx| b.set_loaded("big\n", true, cx));
    draw(cx);
    cx.simulate_input("x");
    run(cx, a::DeleteForward);
    assert_eq!(text_of(&buffer, cx), "big\n");
    assert!(!view.read_with(cx, |v, cx| v.is_editable(cx)));
}

#[gpui::test]
fn crlf_and_bom_survive_an_edit(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", "\u{feff}one\r\ntwo\r\n", false);
    assert_eq!(text_of(&buffer, cx), "one\ntwo\n", "drawn with \\n");
    place(&view, cx, &[3]);
    run(cx, a::Newline);
    cx.simulate_input("new");
    let (version, bytes) = buffer.update(cx, |b, _| b.bytes_to_save());
    assert_eq!(bytes, "\u{feff}one\r\nnew\r\ntwo\r\n");
    buffer.update(cx, |b, cx| b.saved(version, bytes.len() as u64, cx));
    assert!(!buffer.read_with(cx, |b, _| b.is_dirty()));
}
