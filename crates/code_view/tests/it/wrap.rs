//! Soft wrap in a code view (EDITOR-WRAP), in a GPUI test window: the rows a wrapped line is
//! drawn in, the gutter, the caret, selections, find matches and diagnostics on them, the
//! mouse, scrolling, and that the map follows edits and the width of the view.

use gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px, size};
use hutan_buffer::Utf16Point;
use hutan_code_view::{
    CodeView, DiagnosticInput, DrawnRow, FindQuery, Severity, WrapMode, actions as a,
};

use crate::editing::{draw, open, place, ranges, run, text_of};

/// `n` words of four characters (`w000 w001 …`) on one line.
fn words(n: usize) -> String {
    (0..n)
        .map(|i| format!("w{i:03}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn wrap(view: &gpui::Entity<CodeView>, cx: &mut VisualTestContext, mode: WrapMode) {
    view.update(cx, |v, cx| v.set_soft_wrap(mode, cx));
    draw(cx);
}

fn rows_of(view: &gpui::Entity<CodeView>, cx: &VisualTestContext) -> Vec<DrawnRow> {
    view.read_with(cx, |v, _| v.stats().last_rows.clone())
}

fn cols(view: &gpui::Entity<CodeView>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |v, _| v.wrap_params().expect("wrapping").cols)
}

fn row_texts(view: &gpui::Entity<CodeView>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |v, cx| {
        (0..v.display_rows(cx))
            .filter_map(|row| v.display_row_text(row, cx))
            .collect()
    })
}

#[gpui::test]
fn a_long_line_is_drawn_as_several_rows_and_only_the_first_has_the_number(cx: &mut TestAppContext) {
    let text = format!("{}\nshort\n{}\n", words(60), words(12));
    let (view, _buffer, cx) = open(cx, "a.md", &text, false);
    // Off: one row per line.
    assert!(!view.read_with(cx, |v, _| v.is_wrapped()));
    assert_eq!(view.read_with(cx, |v, cx| v.display_rows(cx)), 4);
    wrap(&view, cx, WrapMode::Viewport);
    assert!(view.read_with(cx, |v, _| v.is_wrapped()));
    let cols = cols(&view, cx);
    assert!(
        (20..400).contains(&cols),
        "the test window has a sensible width: {cols} cells"
    );

    let rows = rows_of(&view, cx);
    let texts = row_texts(&view, cx);
    let first_line: Vec<&DrawnRow> = rows.iter().filter(|r| r.line == 0).collect();
    assert!(first_line.len() >= 2, "{}-cell rows: {rows:?}", cols);
    assert_eq!(
        first_line.iter().map(|r| r.sub).collect::<Vec<_>>(),
        (0..first_line.len()).collect::<Vec<_>>()
    );
    for row in &rows {
        assert_eq!(row.number, row.sub == 0, "{row:?}");
    }
    // The rows of a line, put together, are the line.
    let first_rows = &texts[..first_line.len()];
    assert_eq!(first_rows.concat(), words(60));
    for row in first_rows {
        assert!(
            row.trim_end().chars().count() <= cols,
            "{row:?} is wider than {cols} cells"
        );
    }
    // The short line follows as one row with its number.
    assert_eq!(texts[first_line.len()], "short");
    assert_eq!(
        view.read_with(cx, |v, cx| v.display_rows(cx)),
        texts.len(),
        "the last row of the text is the last row of the view"
    );
    // Off again: back to lines.
    wrap(&view, cx, WrapMode::Off);
    assert!(!view.read_with(cx, |v, _| v.is_wrapped()));
    assert_eq!(view.read_with(cx, |v, cx| v.display_rows(cx)), 4);
    assert!(rows_of(&view, cx).iter().all(|r| r.sub == 0 && r.number));
}

#[gpui::test]
fn a_wider_window_draws_fewer_rows_and_keeps_the_top_line(cx: &mut TestAppContext) {
    let text: String = (0..40).map(|i| format!("{} {i}\n", words(30))).collect();
    let (view, _buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let narrow = view.read_with(cx, |v, cx| v.display_rows(cx));
    // Scroll so that line 20 starts at the top, then widen the window.
    let row = view.read_with(cx, |v, cx| v.first_row_of_line(20, cx));
    let line_h = view.read_with(cx, |v, _| {
        f32::from(v.layout().expect("laid out").line_height)
    });
    view.update(cx, |v, cx| v.scroll_to(row as f32 * line_h, cx));
    draw(cx);
    assert_eq!(view.read_with(cx, |v, _| v.top_line()), 20);
    cx.simulate_resize(size(px(1800.0), px(600.0)));
    draw(cx);
    let wide = view.read_with(cx, |v, cx| v.display_rows(cx));
    assert!(wide < narrow, "{wide} rows wide, {narrow} narrow");
    assert_eq!(
        view.read_with(cx, |v, _| v.top_line()),
        20,
        "the line at the top stays there when the width changes"
    );
}

#[gpui::test]
fn the_caret_is_drawn_on_the_row_it_is_in(cx: &mut TestAppContext) {
    let text = format!("{}\nend\n", words(60));
    let (view, _buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let texts = row_texts(&view, cx);
    let (first, second) = (texts[0].len(), texts[1].len());
    // In the middle of the second row.
    place(&view, cx, &[first + second / 2]);
    let rows = rows_of(&view, cx);
    assert_eq!(
        rows.iter()
            .filter(|r| r.carets > 0)
            .map(|r| (r.line, r.sub))
            .collect::<Vec<_>>(),
        vec![(0, 1)]
    );
    // At the break: drawn at the start of the second row, not also at the end of the first.
    place(&view, cx, &[first]);
    let rows = rows_of(&view, cx);
    assert_eq!(rows.iter().map(|r| r.carets).sum::<usize>(), 1);
    assert_eq!(rows[1].carets, 1);
    // The end of the text: the last row.
    place(&view, cx, &[text.len()]);
    let rows = rows_of(&view, cx);
    assert_eq!(rows.iter().filter(|r| r.carets > 0).count(), 1);
}

#[gpui::test]
fn a_selection_across_a_wrap_fills_the_end_of_one_row_and_the_start_of_the_next(
    cx: &mut TestAppContext,
) {
    let text = format!("{}\nend\n", words(60));
    let (view, _buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let texts = row_texts(&view, cx);
    let first = texts[0].len();
    // Three characters before the break to three after it.
    view.update(cx, |v, cx| v.select(first - 3..first + 3, cx));
    draw(cx);
    let rows = rows_of(&view, cx);
    assert_eq!(rows[0].selected_cells, 3, "{rows:?}");
    assert_eq!(rows[1].selected_cells, 3, "{rows:?}");
    assert!(rows[2..].iter().all(|r| r.selected_cells == 0));
    assert_eq!(
        view.read_with(cx, |v, cx| v.selected_text(cx))
            .expect("selected"),
        format!("{}{}", &texts[0][texts[0].len() - 3..], &texts[1][..3])
    );
    // The whole first line: its rows are filled to their ends and the line break is one cell.
    view.update(cx, |v, cx| v.select(0..words(60).len() + 1, cx));
    draw(cx);
    let rows = rows_of(&view, cx);
    let line_rows: Vec<&DrawnRow> = rows.iter().filter(|r| r.line == 0).collect();
    assert!(line_rows.iter().all(|r| r.selected_cells > 0));
    assert_eq!(
        line_rows.iter().map(|r| r.selected_cells).sum::<usize>(),
        words(60).chars().count() + 1
    );
}

#[gpui::test]
fn a_click_on_a_continuation_row_puts_the_caret_there(cx: &mut TestAppContext) {
    let text = format!("{}\nend\n", words(60));
    let (view, _buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let layout = view.read_with(cx, |v, _| v.layout().expect("laid out"));
    let at = |col: f32, row: f32| {
        point(
            layout.text_left + layout.cell_width * col,
            layout.bounds.top() + layout.line_height * (row + 0.5),
        )
    };
    let texts = row_texts(&view, cx);
    let first = texts[0].len();
    // Column 7 of the second row: between `w` and the next digit of the second word.
    cx.simulate_mouse_down(at(7.2, 1.0), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(at(7.2, 1.0), MouseButton::Left, Modifiers::none());
    assert_eq!(ranges(&view, cx), vec![first + 7..first + 7]);
    // Past the end of the first row: the end of that row, drawn there.
    let edge =
        f32::from(layout.bounds.right() - layout.text_left) / f32::from(layout.cell_width) - 0.3;
    cx.simulate_mouse_down(at(edge, 0.0), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(at(1.0, 0.0), MouseButton::Left, Modifiers::none());
    draw(cx);
    assert_eq!(ranges(&view, cx), vec![first..first]);
    let rows = rows_of(&view, cx);
    assert_eq!((rows[0].carets, rows[1].carets), (1, 0), "{rows:?}");
    // Below the last row: the end of the text.
    cx.simulate_mouse_down(at(2.0, 12.0), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(at(2.0, 12.0), MouseButton::Left, Modifiers::none());
    assert_eq!(ranges(&view, cx), vec![text.len()..text.len()]);
    // A drag from the first row into the second selects across the wrap.
    cx.simulate_mouse_down(at(2.0, 0.0), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(at(3.0, 1.0), MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(at(3.0, 1.0), MouseButton::Left, Modifiers::none());
    assert_eq!(ranges(&view, cx), vec![2..first + 3]);
    // Where the caret is, the platform's text input puts the candidate window: on that row.
    let p = view
        .read_with(cx, |v, cx| v.point_of(first + 3, cx))
        .expect("point");
    assert_eq!(p.y, layout.bounds.top() + layout.line_height);
    assert_eq!(p.x, layout.text_left + layout.cell_width * 3.0);
}

#[gpui::test]
fn find_matches_and_diagnostics_are_drawn_on_their_rows(cx: &mut TestAppContext) {
    let text = format!("{}\nend\n", words(60));
    let (view, buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let texts = row_texts(&view, cx);
    let first = texts[0].len();
    // A word that starts the third row.
    let needle = texts[2].split(' ').next().expect("word").to_owned();
    view.update(cx, |v, cx| {
        v.set_find_query(FindQuery::literal(needle.clone()), cx)
    });
    cx.run_until_parked();
    draw(cx);
    let sel = ranges(&view, cx)[0].clone();
    assert_eq!(&text[sel.clone()], needle);
    assert_eq!(
        sel.start,
        texts[0].len() + texts[1].len(),
        "the match is the third row's first word"
    );
    let rows = rows_of(&view, cx);
    assert_eq!(rows[2].selected_cells, needle.chars().count(), "{rows:?}");

    // A diagnostic that crosses the break between the first two rows: an underline on each
    // and the gutter's glyph once, beside the first.
    buffer.update(cx, |b, cx| {
        b.set_diagnostics(
            vec![DiagnosticInput {
                start: Utf16Point {
                    line: 0,
                    character: first - 4,
                },
                end: Utf16Point {
                    line: 0,
                    character: first + 4,
                },
                severity: Severity::Error,
                message: "wrapped".into(),
                source: None,
                code: None,
            }],
            cx,
        )
    });
    draw(cx);
    let drawn = view.read_with(cx, |v, _| v.stats().last_diagnostics.clone());
    assert_eq!(drawn.len(), 2, "{drawn:?}");
    assert!(drawn[0].glyph.is_some(), "the first row has the glyph");
    assert!(drawn[1].glyph.is_none(), "the second has none: {drawn:?}");
}

#[gpui::test]
fn the_scroll_extent_is_in_rows(cx: &mut TestAppContext) {
    let text: String = (0..30).map(|i| format!("{} {i}\n", words(30))).collect();
    let (view, _buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let total = view.read_with(cx, |v, cx| v.display_rows(cx));
    assert!(total > 60, "{total} rows");
    let line_h = view.read_with(cx, |v, _| {
        f32::from(v.layout().expect("laid out").line_height)
    });
    view.update(cx, |v, cx| v.scroll_to(1.0e9, cx));
    draw(cx);
    let view_h = view.read_with(cx, |v, _| {
        f32::from(v.layout().expect("laid out").bounds.size.height)
    });
    assert_eq!(
        view.read_with(cx, |v, _| v.scroll_y()),
        total as f32 * line_h - view_h * 0.5,
        "the last row ends half a screen above the bottom of the view"
    );
    // The caret at the end of the text brings the end into view (the last rows are below).
    view.update(cx, |v, cx| v.scroll_to(0.0, cx));
    run(cx, a::MoveToEnd);
    let top = view.read_with(cx, |v, _| v.scroll_y());
    let view_h = view.read_with(cx, |v, _| {
        f32::from(v.layout().expect("laid out").bounds.size.height)
    });
    assert!(top + view_h >= total as f32 * line_h - line_h, "{top}");
    // No sideways scroll while wrapping.
    assert_eq!(view.read_with(cx, |v, _| v.scroll_x()), 0.0);
}

#[gpui::test]
fn edits_keep_the_map_in_step_with_the_text(cx: &mut TestAppContext) {
    let text = format!("{}\n{}\nend\n", words(40), words(40));
    let (view, buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let before = view.read_with(cx, |v, cx| v.display_rows(cx));
    place(&view, cx, &[10]);
    cx.simulate_input("insert some more words here and there, ");
    let typed = view.read_with(cx, |v, cx| v.display_rows(cx));
    assert!(typed >= before);
    let fresh: usize = {
        let current = text_of(&buffer, cx);
        // A second view of the same text, wrapped from scratch.
        let text = current.clone();
        let (other, _b, cx) = open(cx, "b.md", &text, false);
        wrap(&other, cx, WrapMode::Viewport);
        other.read_with(cx, |v, cx| v.display_rows(cx))
    };
    assert_eq!(typed, fresh);
    // Undo brings the rows back.
    run(cx, a::Undo);
    assert_eq!(view.read_with(cx, |v, cx| v.display_rows(cx)), before);
    // Replacing the whole text builds a new map.
    buffer.update(cx, |b, cx| b.set_loaded("one\ntwo\n", false, cx));
    draw(cx);
    assert_eq!(view.read_with(cx, |v, cx| v.display_rows(cx)), 3);
}

#[gpui::test]
fn column_mode_wraps_at_the_column_or_the_view(cx: &mut TestAppContext) {
    let text = format!("{}\n", words(60));
    let (view, _buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Column(30));
    let rows = row_texts(&view, cx);
    assert!(
        rows.iter().all(|r| r.trim_end().chars().count() <= 30),
        "{rows:?}"
    );
    assert!(rows.len() >= 8);
    // A column wider than the view: the view's width.
    wrap(&view, cx, WrapMode::Column(10_000));
    let viewport = {
        wrap(&view, cx, WrapMode::Viewport);
        cols(&view, cx)
    };
    wrap(&view, cx, WrapMode::Column(10_000));
    assert_eq!(cols(&view, cx), viewport);
}

#[gpui::test]
fn selecting_and_editing_work_on_wide_text_in_rows(cx: &mut TestAppContext) {
    // Cyrillic and CJK rows: the caret column is in cells.
    let text =
        "привет мир как дела у вас сегодня ".repeat(8) + "\n" + &"日本語のテキスト".repeat(10);
    let (view, _buffer, cx) = open(cx, "a.txt", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let rows = row_texts(&view, cx);
    assert!(rows.len() > 3);
    for row in &rows {
        let cells: usize = row
            .chars()
            .map(|c| {
                if c.is_ascii() || ('а'..='я').contains(&c) {
                    1
                } else {
                    2
                }
            })
            .sum();
        assert!(cells <= cols(&view, cx) + 1, "{row:?} is {cells} cells");
    }
    place(&view, cx, &[text.len()]);
    run(cx, a::SelectLeft);
    assert_eq!(ranges(&view, cx)[0].len(), "ト".len());
}

fn head(view: &gpui::Entity<CodeView>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |v, _| v.selections().primary().head)
}

#[gpui::test]
fn up_and_down_move_by_display_row_and_keep_the_goal_column(cx: &mut TestAppContext) {
    let text = format!("{}\nshort\n{}\n", words(60), words(60));
    let (view, _buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let texts = row_texts(&view, cx);
    let (r0, r1, r2) = (texts[0].len(), texts[1].len(), texts[2].len());
    place(&view, cx, &[12]);
    run(cx, a::MoveDown);
    assert_eq!(head(&view, cx), r0 + 12, "the same column on the next row");
    let rows = rows_of(&view, cx);
    assert_eq!((rows[1].line, rows[1].sub, rows[1].carets), (0, 1, 1));
    run(cx, a::MoveDown);
    assert_eq!(head(&view, cx), r0 + r1 + 12);
    run(cx, a::MoveUp);
    assert_eq!(head(&view, cx), r0 + 12);
    run(cx, a::MoveUp);
    assert_eq!(head(&view, cx), 12);
    // The first row: up goes to the start of the text.
    run(cx, a::MoveUp);
    assert_eq!(head(&view, cx), 0);

    // Down from the last row of the first line: the short line (clamped), and the column wanted
    // comes back on the line after it.
    place(&view, cx, &[12]);
    let last_row_of_line = texts.iter().take_while(|t| t.as_str() != "short").count() - 1;
    for _ in 0..last_row_of_line {
        run(cx, a::MoveDown);
    }
    run(cx, a::MoveDown);
    let short_start = words(60).len() + 1;
    assert_eq!(
        head(&view, cx),
        short_start + "short".len(),
        "clamped to the short line"
    );
    run(cx, a::MoveDown);
    let third = short_start + "short".len() + 1;
    assert_eq!(
        head(&view, cx),
        third + 12,
        "the goal column of the first row"
    );
    let _ = r2;
    // Shift extends the selection by rows.
    place(&view, cx, &[12]);
    run(cx, a::SelectDown);
    assert_eq!(ranges(&view, cx), vec![12..r0 + 12]);
}

#[gpui::test]
fn home_and_end_go_to_the_row_first_then_the_line(cx: &mut TestAppContext) {
    let text = format!("  {}\nend\n", words(60));
    let (view, buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let texts = row_texts(&view, cx);
    let (r0, r1) = (texts[0].len(), texts[1].len());
    let line_end = text.find('\n').expect("line break");

    // Home on the second row: its start, then the line's first character that is not a space,
    // then the line's start.
    place(&view, cx, &[r0 + 10]);
    run(cx, a::MoveLineStart);
    assert_eq!(head(&view, cx), r0);
    run(cx, a::MoveLineStart);
    assert_eq!(head(&view, cx), 2);
    run(cx, a::MoveLineStart);
    assert_eq!(head(&view, cx), 0);

    // End on the first row: the end of the row, drawn there; then the end of the line.
    place(&view, cx, &[5]);
    run(cx, a::MoveLineEnd);
    assert_eq!(head(&view, cx), r0);
    let rows = rows_of(&view, cx);
    assert_eq!((rows[0].carets, rows[1].carets), (1, 0), "{rows:?}");
    run(cx, a::MoveLineEnd);
    assert_eq!(head(&view, cx), line_end);
    // On the last row End goes straight to the end of the line.
    place(&view, cx, &[line_end - 3]);
    run(cx, a::MoveLineEnd);
    assert_eq!(head(&view, cx), line_end);
    // Shift+End selects to the end of the row, then to the end of the line.
    place(&view, cx, &[5]);
    run(cx, a::SelectLineEnd);
    assert_eq!(ranges(&view, cx), vec![5..r0]);
    run(cx, a::SelectLineEnd);
    assert_eq!(ranges(&view, cx), vec![5..line_end]);
    // Typing at the end of a row inserts there.
    place(&view, cx, &[5]);
    run(cx, a::MoveLineEnd);
    cx.simulate_input("X");
    assert_eq!(&text_of(&buffer, cx)[r0..r0 + 1], "X");
    let _ = r1;
}

#[gpui::test]
fn page_up_and_down_move_by_display_rows(cx: &mut TestAppContext) {
    let text: String = (0..40).map(|i| format!("{} {i}\n", words(30))).collect();
    let (view, _buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let page = view.read_with(cx, |v, _| v.layout().expect("laid out").rows - 2);
    place(&view, cx, &[0]);
    run(cx, a::PageDown);
    let row = view.read_with(cx, |v, cx| v.row_of_offset(head_of(v), cx));
    assert_eq!(row, page, "one page of rows down");
    let scrolled = view.read_with(cx, |v, _| v.scroll_y());
    assert!(scrolled > 0.0);
    run(cx, a::PageUp);
    assert_eq!(head(&view, cx), 0);
}

fn head_of(v: &CodeView) -> usize {
    v.selections().primary().head
}

#[gpui::test]
fn vim_j_k_stay_on_lines_and_gj_gk_g0_g_dollar_move_by_rows(cx: &mut TestAppContext) {
    use hutan_code_view::{VimKey, VimSettings};
    let text = format!("{}\nshort\n", words(60));
    let (view, _buffer, cx) = open(cx, "a.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    let texts = row_texts(&view, cx);
    let (r0, r1) = (texts[0].len(), texts[1].len());
    view.update(cx, |v, cx| v.set_vim(Some(VimSettings::default()), cx));
    let keys = |cx: &mut VisualTestContext, keys: &str| {
        for c in keys.chars() {
            view.update(cx, |v, cx| {
                v.vim_key(VimKey::Char(c), cx);
            });
        }
        draw(cx);
    };
    // `gj` / `gk`: a display row, the same column.
    place(&view, cx, &[12]);
    keys(cx, "gj");
    assert_eq!(head(&view, cx), r0 + 12);
    keys(cx, "gj");
    assert_eq!(head(&view, cx), r0 + r1 + 12);
    keys(cx, "gk");
    assert_eq!(head(&view, cx), r0 + 12);
    keys(cx, "gkgk");
    assert_eq!(head(&view, cx), 12, "nothing above the first row");
    // `j` goes to the next line, whatever the rows.
    keys(cx, "j");
    assert_eq!(
        head(&view, cx),
        words(60).len() + 1 + 5 - 1,
        "the short line's last character"
    );
    keys(cx, "k");
    assert_eq!(
        head(&view, cx),
        12,
        "the column Vim wanted before the short line, on the first row of the line above"
    );
    // `g0` `g$`: the edges of the row the caret is in.
    place(&view, cx, &[r0 + 12]);
    keys(cx, "g0");
    assert_eq!(head(&view, cx), r0);
    keys(cx, "g$");
    assert_eq!(head(&view, cx), r0 + r1 - 1);
    keys(cx, "0");
    assert_eq!(head(&view, cx), 0, "`0` is the line's start");
    keys(cx, "$");
    assert_eq!(head(&view, cx), words(60).len() - 1);
}

#[gpui::test]
fn a_keystroke_in_a_big_wrapped_file_wraps_and_shapes_only_the_line_it_changed(
    cx: &mut TestAppContext,
) {
    // 10 000 lines of Markdown: paragraphs that wrap to two or three rows.
    let mut text = String::new();
    for n in 0..2_000 {
        text.push_str(&format!(
            "## Section {n}\n\n{} {n}\n\n- item one of {n}\n",
            words(30)
        ));
    }
    let (view, _buffer, cx) = open(cx, "big.md", &text, false);
    wrap(&view, cx, WrapMode::Viewport);
    assert!(view.read_with(cx, |v, cx| v.display_rows(cx)) > 12_000);
    // The middle paragraph.
    let middle = text.find("## Section 1000").expect("section") + 20;
    place(&view, cx, &[middle]);
    let shaped = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.stats().lines_shaped);
    let rewrapped = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.wrap_rewrapped());
    let (shaped_before, rewrapped_before) = (shaped(cx), rewrapped(cx));
    let keys = 40;
    for _ in 0..keys {
        cx.simulate_input("x");
        draw(cx);
    }
    let shaped_per_key = (shaped(cx) - shaped_before) as f32 / keys as f32;
    let rewrapped_per_key = (rewrapped(cx) - rewrapped_before) as f32 / keys as f32;
    assert!(
        rewrapped_per_key <= 1.0,
        "a keystroke wrapped {rewrapped_per_key} lines"
    );
    assert!(
        shaped_per_key <= 3.0,
        "a keystroke shaped {shaped_per_key} rows (the rows of the line it changed)"
    );
}
