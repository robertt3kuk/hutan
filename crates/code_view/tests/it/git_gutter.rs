//! The git change marks of the gutter (EDITOR-POLISH), in a GPUI test window: the marks appear
//! after typing and clear after undo, they are drawn on every row of a wrapped line, a click on
//! one opens the peek, *Revert hunk* is one undo step, *Stage hunk* asks the owner for the
//! lines, the step keys (the actions and Vim's `]c` `[c`) walk the changes, and
//! `editor.git_gutter = false` computes nothing.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point};
use hutan_buffer::Selections;
use hutan_code_view::{
    CodeView, CodeViewEvent, DrawnRow, FileBuffer, GitBase, GitBaseKind, HunkKind, VimSettings,
    WrapMode, actions as a, agent,
};

use crate::editing::{draw, open, place, run, text_of};

fn numbered(lines: usize) -> String {
    (1..=lines).map(|n| format!("line {n}\n")).collect()
}

/// Lets the debounced diff after an edit run.
fn settle(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(agent::SLOW_DIFF_DELAY * 2);
    draw(cx);
}

fn base(buffer: &Entity<FileBuffer>, text: &str, cx: &mut VisualTestContext) {
    buffer.update(cx, |b, cx| {
        b.set_git_base(Some(GitBase::new(text, GitBaseKind::Index)), cx);
    });
    draw(cx);
}

/// What each git hunk covers now, as text.
fn hunk_texts(buffer: &Entity<FileBuffer>, cx: &VisualTestContext) -> Vec<String> {
    buffer.read_with(cx, |b, _| {
        assert!(b.git_hunks_current(), "hunks of the current text");
        let text = b.text().to_string();
        b.git_hunks()
            .iter()
            .map(|h| text[h.bytes.clone()].to_owned())
            .collect()
    })
}

fn kinds(buffer: &Entity<FileBuffer>, cx: &VisualTestContext) -> Vec<HunkKind> {
    buffer.read_with(cx, |b, _| b.git_hunks().iter().map(|h| h.kind()).collect())
}

fn rows(view: &Entity<CodeView>, cx: &VisualTestContext) -> Vec<DrawnRow> {
    view.read_with(cx, |v, _| v.stats().last_rows.clone())
}

/// The window position of the git mark strip at the middle of display row `row`.
fn mark_at(view: &Entity<CodeView>, row: f32, cx: &VisualTestContext) -> gpui::Point<gpui::Pixels> {
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    point(
        layout.text_left - layout.cell_width * 1.0,
        layout.bounds.top() + layout.line_height * (row + 0.5),
    )
}

fn events(view: &Entity<CodeView>, cx: &mut VisualTestContext) -> Rc<RefCell<Vec<CodeViewEvent>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let subscription = cx.update(|_, cx| {
        cx.subscribe(view, move |_, event: &CodeViewEvent, _| {
            sink.borrow_mut().push(*event);
        })
    });
    // Lives as long as the test.
    std::mem::forget(subscription);
    seen
}

#[gpui::test]
fn a_file_equal_to_its_base_has_no_marks(cx: &mut TestAppContext) {
    let text = numbered(10);
    let (_view, buffer, cx) = open(cx, "a.txt", &text, false);
    base(&buffer, &text, cx);
    assert!(hunk_texts(&buffer, cx).is_empty());
    // CRLF in the base is no change.
    base(&buffer, &text.replace('\n', "\r\n"), cx);
    assert!(hunk_texts(&buffer, cx).is_empty());
}

#[gpui::test]
fn marks_appear_after_typing_and_clear_after_undo(cx: &mut TestAppContext) {
    let text = numbered(10);
    let (view, buffer, cx) = open(cx, "a.txt", &text, false);
    base(&buffer, &text, cx);
    // Typing inside line 3 changes it; a new line after line 6 adds one.
    place(&view, cx, &[text.find("line 3").unwrap() + 5]);
    cx.simulate_input("XY");
    // Until the pause is over the hunks are only carried across the edit.
    assert!(!buffer.read_with(cx, |b, _| b.git_hunks_current()));
    settle(cx);
    assert_eq!(hunk_texts(&buffer, cx), ["line XY3\n"]);
    assert_eq!(kinds(&buffer, cx), [HunkKind::Modified]);
    let drawn = rows(&view, cx);
    assert!(drawn[2].git_bar, "line 3 has its bar: {drawn:?}");
    assert!(!drawn[1].git_bar && !drawn[3].git_bar);

    // Undo brings the base back: no marks.
    run(cx, a::Undo);
    settle(cx);
    assert!(hunk_texts(&buffer, cx).is_empty());
    assert!(rows(&view, cx).iter().all(|r| !r.git_bar));

    // Redo, and delete a line, and add one.
    run(cx, a::Redo);
    let now = text_of(&buffer, cx);
    place(&view, cx, &[now.find("line 7").unwrap()]);
    cx.simulate_input("new");
    run(cx, a::Newline);
    let now = text_of(&buffer, cx);
    let line_9 = now.find("line 9").unwrap();
    view.update(cx, |v, cx| {
        v.set_selections(
            Selections::single(hutan_buffer::Selection::new(line_9, line_9 + 7)),
            cx,
        );
    });
    run(cx, a::DeleteBackward);
    settle(cx);
    assert_eq!(
        kinds(&buffer, cx),
        [HunkKind::Modified, HunkKind::Added, HunkKind::Removed]
    );
    let drawn = rows(&view, cx);
    assert!(drawn[6].git_bar, "the added line");
    assert!(
        drawn.iter().any(|r| r.git_removed_above),
        "the wedge of the removed line"
    );
}

#[gpui::test]
fn removed_lines_at_the_edges_are_a_mark(cx: &mut TestAppContext) {
    let text = "a\nb\nc\n";
    let (view, buffer, cx) = open(cx, "a.txt", text, false);
    base(&buffer, "zero\na\nb\nc\nd\n", cx);
    assert_eq!(
        kinds(&buffer, cx),
        [HunkKind::Removed, HunkKind::Removed],
        "a line above the first and one below the last"
    );
    let drawn = rows(&view, cx);
    assert!(drawn[0].git_removed_above);
    // The mark of lines removed past the last line is drawn above the (empty) line after it.
    assert!(drawn.iter().any(|r| r.line == 3 && r.git_removed_above));
}

#[gpui::test]
fn every_row_of_a_wrapped_line_has_its_bar(cx: &mut TestAppContext) {
    let long = (0..60)
        .map(|i| format!("w{i:03}"))
        .collect::<Vec<_>>()
        .join(" ");
    let text = format!("short\n{long}\nlast\n");
    let (view, buffer, cx) = open(cx, "a.md", &text, false);
    view.update(cx, |v, cx| v.set_soft_wrap(WrapMode::Viewport, cx));
    draw(cx);
    base(&buffer, "short\nlast\n", cx);
    let drawn = rows(&view, cx);
    let long_rows: Vec<&DrawnRow> = drawn.iter().filter(|r| r.line == 1).collect();
    assert!(long_rows.len() >= 2, "the line wraps: {drawn:?}");
    assert!(long_rows.iter().all(|r| r.git_bar), "{long_rows:?}");
    assert!(drawn.iter().filter(|r| r.line != 1).all(|r| !r.git_bar));
    // A click on the mark of a continuation row opens that hunk.
    let second = drawn
        .iter()
        .position(|r| r.line == 1 && r.sub == 1)
        .unwrap();
    cx.simulate_click(mark_at(&view, second as f32, cx), Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.git_peek()), Some(0));
}

#[gpui::test]
fn a_click_on_a_mark_opens_the_peek_and_leaves_the_caret_alone(cx: &mut TestAppContext) {
    let text = numbered(10);
    let (view, buffer, cx) = open(cx, "a.txt", &text, false);
    base(&buffer, &text.replace("line 4\n", "four\n"), cx);
    assert_eq!(hunk_texts(&buffer, cx), ["line 4\n"]);
    let caret = view.read_with(cx, |v, _| v.selections().primary().head);
    cx.simulate_click(mark_at(&view, 3.0, cx), Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.git_peek()), Some(0));
    assert_eq!(
        view.read_with(cx, |v, _| v.selections().primary().head),
        caret
    );
    // The same mark again closes it; a line without a mark does nothing here.
    cx.simulate_click(mark_at(&view, 3.0, cx), Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.git_peek()), None);
    cx.simulate_click(mark_at(&view, 6.0, cx), Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.git_peek()), None);
    // Escape closes an open peek.
    run(cx, a::NextGitHunk);
    assert_eq!(view.read_with(cx, |v, _| v.git_peek()), Some(0));
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |v, _| v.git_peek()), None);
}

#[gpui::test]
fn revert_hunk_is_one_undo_step(cx: &mut TestAppContext) {
    let text = numbered(10);
    let (view, buffer, cx) = open(cx, "a.txt", &text, false);
    let base_text = text
        .replace("line 4\n", "four\nand more\n")
        .replace("line 8\n", "");
    base(&buffer, &base_text, cx);
    assert_eq!(kinds(&buffer, cx), [HunkKind::Modified, HunkKind::Added]);
    // Revert the first change: its lines are the base's again.
    assert!(view.update(cx, |v, cx| v.revert_git_hunk(0, cx)));
    draw(cx);
    assert_eq!(
        text_of(&buffer, cx),
        text.replace("line 4\n", "four\nand more\n")
    );
    assert_eq!(
        view.read_with(cx, |v, _| v.git_peek()),
        None,
        "the peek closed"
    );
    settle(cx);
    assert_eq!(
        kinds(&buffer, cx),
        [HunkKind::Added],
        "the other change stays"
    );
    // One undo brings the change back.
    run(cx, a::Undo);
    assert_eq!(text_of(&buffer, cx), text);
    settle(cx);
    assert_eq!(kinds(&buffer, cx), [HunkKind::Modified, HunkKind::Added]);
}

#[gpui::test]
fn reverting_removed_lines_puts_them_back(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", "a\nc\n", false);
    base(&buffer, "a\nb\nc\n", cx);
    assert_eq!(kinds(&buffer, cx), [HunkKind::Removed]);
    assert!(view.update(cx, |v, cx| v.revert_git_hunk(0, cx)));
    draw(cx);
    assert_eq!(text_of(&buffer, cx), "a\nb\nc\n");
    settle(cx);
    assert!(hunk_texts(&buffer, cx).is_empty());
}

#[gpui::test]
fn stage_hunk_asks_the_owner_for_the_lines(cx: &mut TestAppContext) {
    let text = numbered(10);
    let (view, buffer, cx) = open(cx, "a.txt", &text, false);
    base(
        &buffer,
        &text.replace("line 4\n", "four\n").replace("line 7\n", ""),
        cx,
    );
    assert_eq!(kinds(&buffer, cx), [HunkKind::Modified, HunkKind::Added]);
    let seen = events(&view, cx);
    view.update(cx, |v, cx| v.set_git_stage_available(true, cx));
    view.update(cx, |v, cx| v.stage_git_hunk(1, cx));
    let staged: Vec<_> = seen
        .borrow()
        .iter()
        .filter_map(|e| match e {
            CodeViewEvent::GitHunkStage {
                first_line,
                end_line,
                kind,
            } => Some((*first_line, *end_line, *kind)),
            _ => None,
        })
        .collect();
    assert_eq!(staged, [(6, 7, HunkKind::Added)]);
    // The text is not touched.
    assert_eq!(text_of(&buffer, cx), text);
}

#[gpui::test]
fn the_step_keys_walk_the_changes_and_wrap(cx: &mut TestAppContext) {
    let text = numbered(40);
    let (view, buffer, cx) = open(cx, "a.txt", &text, false);
    base(
        &buffer,
        &text
            .replace("line 5\n", "five\n")
            .replace("line 30\n", "thirty\n"),
        cx,
    );
    let at = |n: usize| text.find(&format!("line {n}\n")).unwrap();
    let head = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.selections().primary().head);
    place(&view, cx, &[0]);
    run(cx, a::NextGitHunk);
    assert_eq!(head(cx), at(5));
    assert_eq!(
        view.read_with(cx, |v, _| v.git_peek()),
        Some(0),
        "the peek opens"
    );
    run(cx, a::NextGitHunk);
    assert_eq!(head(cx), at(30));
    assert_eq!(view.read_with(cx, |v, _| v.git_peek()), Some(1));
    run(cx, a::NextGitHunk);
    assert_eq!(head(cx), at(5), "wraps around");
    run(cx, a::PreviousGitHunk);
    assert_eq!(head(cx), at(30), "wraps back");
    run(cx, a::PreviousGitHunk);
    assert_eq!(head(cx), at(5));
}

#[gpui::test]
fn vim_steps_with_bracket_c(cx: &mut TestAppContext) {
    let text = numbered(40);
    let (view, buffer, cx) = open(cx, "a.txt", &text, false);
    view.update(cx, |v, cx| v.set_vim(Some(VimSettings::default()), cx));
    base(
        &buffer,
        &text
            .replace("line 5\n", "five\n")
            .replace("line 30\n", "thirty\n"),
        cx,
    );
    let at = |n: usize| text.find(&format!("line {n}\n")).unwrap();
    let head = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.selections().primary().head);
    place(&view, cx, &[0]);
    cx.simulate_input("]c");
    assert_eq!(head(cx), at(5));
    cx.simulate_input("]c");
    assert_eq!(head(cx), at(30));
    cx.simulate_input("[c");
    assert_eq!(head(cx), at(5));
    // Vim is back in Normal mode, not waiting for a key.
    cx.simulate_input("j");
    assert_eq!(head(cx), at(6));
}

#[gpui::test]
fn the_setting_off_computes_and_draws_nothing(cx: &mut TestAppContext) {
    let text = numbered(10);
    let (view, buffer, cx) = open(cx, "a.txt", &text, false);
    buffer.update(cx, |b, cx| b.set_git_gutter(false, cx));
    base(&buffer, "different\n", cx);
    assert!(buffer.read_with(cx, |b, _| b.git_hunks().is_empty()));
    assert!(buffer.read_with(cx, |b, _| b.git_hunks_current()));
    assert!(rows(&view, cx).iter().all(|r| !r.git_bar));
    buffer.update(cx, |b, cx| b.set_git_gutter(true, cx));
    draw(cx);
    assert_eq!(buffer.read_with(cx, |b, _| b.git_hunks().len()), 1);
    assert!(
        rows(&view, cx)
            .iter()
            .filter(|r| r.line < 10)
            .all(|r| r.git_bar)
    );
}

#[gpui::test]
fn taking_the_base_away_clears_the_marks(cx: &mut TestAppContext) {
    let text = numbered(5);
    let (view, buffer, cx) = open(cx, "a.txt", &text, false);
    base(&buffer, "x\n", cx);
    assert!(!hunk_texts(&buffer, cx).is_empty());
    buffer.update(cx, |b, cx| b.set_git_base(None, cx));
    draw(cx);
    assert!(buffer.read_with(cx, |b, _| b.git_hunks().is_empty()));
    assert!(rows(&view, cx).iter().all(|r| !r.git_bar));
}
