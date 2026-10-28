//! Git blame in the gutter (GIT-HISTORY), in a GPUI test window: the buffer keeps the blame in
//! step with the text (a typed line is uncommitted, an undo brings the commit back), the gutter
//! column makes room and draws, hovering it shows the commit and clicking it asks the owner to
//! open it.

use std::sync::Arc;

use gpui::{Modifiers, TestAppContext, point};
use hutan_buffer::Selections;
use hutan_code_view::{BlameCommit, BlameData, BlameLine, CodeViewEvent};

use crate::editing::{draw, open};

fn commit(id: &str, author: &str, subject: &str) -> BlameCommit {
    BlameCommit {
        id: id.repeat(40).chars().take(40).collect(),
        author: author.to_owned(),
        email: format!("{}@example.com", author.to_lowercase()),
        time: 1_700_000_000,
        subject: subject.to_owned(),
        uncommitted: false,
    }
}

/// Lines 1 and 3 by Ada, line 2 by Grace, line 4 by Ada again.
fn data(text: &str) -> BlameData {
    BlameData {
        commits: vec![commit("a", "Ada", "first"), commit("b", "Grace", "second")],
        lines: vec![0, 1, 0, 0],
        text: Arc::from(text),
    }
}

const TEXT: &str = "one\ntwo\nthree\nfour\n";

fn who(
    buffer: &gpui::Entity<hutan_code_view::FileBuffer>,
    row: usize,
    cx: &gpui::VisualTestContext,
) -> String {
    buffer.read_with(cx, |b, _| match b.blame_line(row) {
        Some(BlameLine::Commit(c)) => c.author.clone(),
        Some(BlameLine::Uncommitted) => "uncommitted".to_owned(),
        None => "none".to_owned(),
    })
}

fn authors(
    buffer: &gpui::Entity<hutan_code_view::FileBuffer>,
    cx: &gpui::VisualTestContext,
) -> Vec<String> {
    (0..5).map(|row| who(buffer, row, cx)).collect()
}

#[gpui::test]
fn the_blame_follows_typing_and_undo(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", TEXT, false);
    assert_eq!(
        authors(&buffer, cx),
        ["none"; 5],
        "no blame until the owner gives one"
    );
    buffer.update(cx, |b, cx| b.set_blame(Some(data(TEXT)), cx));
    assert_eq!(authors(&buffer, cx), ["Ada", "Grace", "Ada", "Ada", "none"]);

    // A new line typed at the start of line 2: it is uncommitted, the others keep their commit.
    view.update(cx, |v, cx| {
        v.set_selections(Selections::cursor(4), cx);
    });
    cx.simulate_input("new line");
    cx.dispatch_action(hutan_code_view::Newline);
    assert_eq!(
        authors(&buffer, cx),
        ["Ada", "uncommitted", "Grace", "Ada", "Ada"]
    );

    // Typing inside a line makes that line uncommitted.
    view.update(cx, |v, cx| {
        v.set_selections(Selections::cursor(0), cx);
    });
    cx.simulate_input("X");
    assert_eq!(
        authors(&buffer, cx),
        ["uncommitted", "uncommitted", "Grace", "Ada", "Ada"]
    );

    // Undo to the start: the original text, and every line has its commit back.
    for _ in 0..6 {
        cx.dispatch_action(hutan_code_view::Undo);
    }
    assert_eq!(buffer.read_with(cx, |b, _| b.text().to_string()), TEXT);
    assert_eq!(authors(&buffer, cx), ["Ada", "Grace", "Ada", "Ada", "none"]);

    // Taking the blame away.
    buffer.update(cx, |b, cx| b.set_blame(None, cx));
    assert!(!buffer.read_with(cx, |b, _| b.has_blame()));
}

#[gpui::test]
fn the_gutter_column_makes_room_draws_and_answers_the_mouse(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", TEXT, false);
    let before = view.read_with(cx, |v, _| v.layout().expect("laid out").text_left);

    // The column is on, but there is no blame to draw yet: the gutter is as it was.
    view.update(cx, |v, cx| v.set_blame_gutter(true, cx));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |v, _| v.layout().unwrap().text_left),
        before
    );

    buffer.update(cx, |b, cx| b.set_blame(Some(data(TEXT)), cx));
    draw(cx);
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    // The column is as wide as `Ada · 3y · first` needs, not the 40 to 72 cells it was.
    let used = (layout.text_left - before) / layout.cell_width;
    assert!(
        (hutan_code_view::blame::COLUMN_MIN_CELLS as f32 - 0.5
            ..=hutan_code_view::blame::COLUMN_MAX_CELLS as f32 + 0.5)
            .contains(&used),
        "the text moved right by {used} cells for the column"
    );

    // Hovering the column of a line shows that line's commit; leaving it hides it.
    let row_y = |row: f32| layout.bounds.top() + layout.line_height * (row + 0.5);
    let column = layout.bounds.left() + layout.cell_width * 6.0;
    cx.simulate_mouse_move(point(column, row_y(1.0)), None, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.blame_hover()), Some(1));
    cx.simulate_mouse_move(
        point(layout.text_left + layout.cell_width, row_y(1.0)),
        None,
        Modifiers::none(),
    );
    assert_eq!(view.read_with(cx, |v, _| v.blame_hover()), None);

    // A click asks the owner to open the commit of that line, and leaves the caret alone.
    let clicked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = clicked.clone();
    let _subscription = cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &CodeViewEvent, _| {
            if let CodeViewEvent::BlameClicked { row } = event {
                seen.borrow_mut().push(*row);
            }
        })
    });
    let caret = view.read_with(cx, |v, _| v.selections().primary().head);
    cx.simulate_click(point(column, row_y(2.0)), Modifiers::none());
    assert_eq!(*clicked.borrow(), vec![2]);
    assert_eq!(
        view.read_with(cx, |v, _| v.selections().primary().head),
        caret
    );
    let id = buffer.read_with(cx, |b, _| {
        b.blame_line(2)
            .and_then(BlameLine::commit)
            .map(|c| c.id.clone())
    });
    assert_eq!(id, Some("a".repeat(40)));
}

#[gpui::test]
fn inline_blame_is_off_until_asked_and_needs_a_blame(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", TEXT, false);
    view.update(cx, |v, cx| v.set_inline_blame(true, cx));
    assert!(view.read_with(cx, |v, _| v.inline_blame()));
    draw(cx);
    buffer.update(cx, |b, cx| b.set_blame(Some(data(TEXT)), cx));
    draw(cx);
    // Drawn without a gutter column: the text did not move.
    let layout = view.read_with(cx, |v, _| v.layout().unwrap());
    assert!(!view.read_with(cx, |v, _| v.blame_gutter()));
    assert!(layout.text_left < layout.bounds.left() + layout.cell_width * 12.0);
}

fn long_data(text: &str) -> BlameData {
    BlameData {
        commits: vec![commit(
            "a",
            "Robert Tkachenko",
            "a subject that goes on and on and on and on and on and on and on",
        )],
        lines: vec![0, 0, 0, 0],
        text: Arc::from(text),
    }
}

/// How many cells the column took (the text's shift from a gutter without one).
fn column_cells(
    view: &gpui::Entity<hutan_code_view::CodeView>,
    before: gpui::Pixels,
    cx: &gpui::VisualTestContext,
) -> f32 {
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    (layout.text_left - before) / layout.cell_width
}

#[gpui::test]
fn the_column_is_capped_at_32_cells_and_30_percent_of_the_view(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "a.txt", TEXT, false);
    let before = view.read_with(cx, |v, _| v.layout().expect("laid out").text_left);
    view.update(cx, |v, cx| v.set_blame_gutter(true, cx));
    buffer.update(cx, |b, cx| b.set_blame(Some(long_data(TEXT)), cx));
    draw(cx);
    // A long author and a long subject: the cap, not the 72 cells the text would take.
    let wide = column_cells(&view, before, cx);
    assert!(
        (wide - hutan_code_view::blame::COLUMN_MAX_CELLS as f32).abs() < 0.5,
        "{wide}"
    );
    // The same commits in a narrower window: never more than 30 % of it.
    cx.simulate_resize(gpui::size(gpui::px(420.0), gpui::px(600.0)));
    draw(cx);
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    let before = {
        // The gutter without the column, in this window.
        view.update(cx, |v, cx| v.set_blame_gutter(false, cx));
        draw(cx);
        view.read_with(cx, |v, _| v.layout().unwrap().text_left)
    };
    view.update(cx, |v, cx| v.set_blame_gutter(true, cx));
    draw(cx);
    let narrow = column_cells(&view, before, cx);
    let view_cells = layout.bounds.size.width / layout.cell_width;
    assert!(
        narrow <= view_cells * 0.3 + 0.5 && narrow > 0.0,
        "{narrow} cells in a view {view_cells} cells wide"
    );
    // A window too narrow for a useful column draws none.
    cx.simulate_resize(gpui::size(gpui::px(200.0), gpui::px(600.0)));
    draw(cx);
    assert!(
        column_cells(&view, before, cx) <= 0.5,
        "no column in a tiny view"
    );
}

#[gpui::test]
fn a_run_that_began_above_the_view_still_says_who_in_the_first_row_in_view(
    cx: &mut TestAppContext,
) {
    // One commit made all 100 lines.
    let text: String = (1..=100).map(|n| format!("line {n}\n")).collect();
    let (view, buffer, cx) = open(cx, "a.txt", &text, false);
    view.update(cx, |v, cx| v.set_blame_gutter(true, cx));
    buffer.update(cx, |b, cx| {
        b.set_blame(
            Some(BlameData {
                commits: vec![commit("a", "Ada", "everything")],
                lines: vec![0; 100],
                text: Arc::from(text.as_str()),
            }),
            cx,
        );
    });
    draw(cx);
    let labelled = |cx: &gpui::VisualTestContext| -> Vec<usize> {
        view.read_with(cx, |v, _| {
            v.stats()
                .last_rows
                .iter()
                .filter(|r| r.blame_head)
                .map(|r| r.line)
                .collect()
        })
    };
    let blank = |cx: &gpui::VisualTestContext| -> usize {
        view.read_with(cx, |v, _| {
            v.stats()
                .last_rows
                .iter()
                .filter(|r| r.sub == 0 && !r.blame)
                .count()
        })
    };
    assert_eq!(labelled(cx), [0], "the run's first line");
    assert_eq!(blank(cx), 0, "no row inside a run is blank (F-09)");
    // Scrolled down: the run began above the view; its label is on the first line in view.
    let line_h = view.read_with(cx, |v, _| f32::from(v.layout().unwrap().line_height));
    view.update(cx, |v, cx| v.scroll_to(30.0 * line_h, cx));
    draw(cx);
    assert_eq!(labelled(cx), [30]);
    assert_eq!(blank(cx), 0);
}
